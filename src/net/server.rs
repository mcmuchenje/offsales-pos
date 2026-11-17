//! The back-office server: HTTPS API for tills plus a UDP discovery responder.
//!
//! Runs inside the normal app when this computer is set to server mode. It
//! opens its own connection to the same database file as the app's screens.

use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use super::protocol::*;
use super::tls;
use crate::db::{Db, Role};

const PAIRING_VALID: Duration = Duration::from_secs(10 * 60);
const PAIRING_MAX_TRIES: u32 = 5;
const SESSION_VALID: Duration = Duration::from_secs(12 * 60 * 60);

struct Pairing {
    code: String,
    expires: Instant,
    fails: u32,
}

pub struct ServerState {
    db: Mutex<Db>,
    fingerprint: String,
    pairing: Mutex<Option<Pairing>>,
    /// session token -> (manager user id, issued)
    sessions: Mutex<HashMap<String, (i64, Instant)>>,
    on_change: Arc<dyn Fn() + Send + Sync>,
}

pub struct ServerHandle {
    pub addr: SocketAddr,
    /// UDP port answering discovery probes, if enabled.
    #[cfg_attr(not(test), allow(dead_code))]
    pub discovery_port: Option<u16>,
    pub state: Arc<ServerState>,
    http: axum_server::Handle,
    stop: Arc<AtomicBool>,
}

impl ServerHandle {
    pub fn fingerprint(&self) -> &str {
        &self.state.fingerprint
    }

    /// Starts a new single-use pairing code, valid for 10 minutes.
    pub fn new_pairing_code(&self) -> String {
        let code = tls::pairing_code();
        *self.state.pairing.lock().unwrap() = Some(Pairing { code: code.clone(), expires: Instant::now() + PAIRING_VALID, fails: 0 });
        code
    }

    /// The active pairing code and seconds left, if any.
    pub fn pairing_code(&self) -> Option<(String, u64)> {
        let p = self.state.pairing.lock().unwrap();
        let p = p.as_ref()?;
        let left = p.expires.checked_duration_since(Instant::now())?;
        Some((p.code.clone(), left.as_secs()))
    }

    pub fn cancel_pairing(&self) {
        *self.state.pairing.lock().unwrap() = None;
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        self.http.shutdown();
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Loads (or creates on first start) the server's certificate.
pub fn identity(db: &Db) -> Result<(String, String, String)> {
    let (cert, key) = match (db.setting("server_cert")?, db.setting("server_key")?) {
        (Some(c), Some(k)) => (c, k),
        _ => {
            let (c, k) = tls::generate_identity()?;
            db.set_setting("server_cert", &c)?;
            db.set_setting("server_key", &k)?;
            (c, k)
        }
    };
    let fp = tls::fingerprint_of_pem(&cert)?;
    Ok((cert, key, fp))
}

/// Starts the server. `port` 0 picks a free port (tests); `discovery_port`
/// None disables the UDP responder. `loopback_only` listens on this computer
/// only (tests and development), which also avoids firewall prompts.
pub fn start(
    db_path: &Path,
    loopback_only: bool,
    port: u16,
    discovery_port: Option<u16>,
    on_change: Arc<dyn Fn() + Send + Sync>,
) -> Result<ServerHandle> {
    let ip: [u8; 4] = if loopback_only { [127, 0, 0, 1] } else { [0, 0, 0, 0] };
    let db = Db::open(db_path)?;
    let (cert, key, fingerprint) = identity(&db)?;
    let tls_cfg = tls::server_config(&cert, &key)?;
    let state = Arc::new(ServerState {
        db: Mutex::new(db),
        fingerprint,
        pairing: Mutex::new(None),
        sessions: Mutex::new(HashMap::new()),
        on_change,
    });

    let app = Router::new()
        .route("/api/hello", get(hello))
        .route("/api/pair", post(pair))
        .route("/api/snapshot", get(snapshot))
        .route("/api/sales", post(sale))
        .route("/api/voids", post(void))
        .route("/api/session", post(session))
        .route("/api/products", post(product))
        .route("/api/stock", post(stock))
        .route("/api/users", post(user))
        .route("/api/settings", post(settings))
        .with_state(state.clone());

    let http = axum_server::Handle::new();
    let (tx, rx) = std::sync::mpsc::channel();
    let h = http.clone();
    std::thread::Builder::new().name("pos-server".into()).spawn(move || {
        let rt = match tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() {
            Ok(rt) => rt,
            Err(e) => {
                let _ = tx.send(Err(anyhow!(e)));
                return;
            }
        };
        rt.block_on(async move {
            let config = axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(tls_cfg));
            let h2 = h.clone();
            let tx2 = tx.clone();
            tokio::spawn(async move {
                let _ = tx2.send(h2.listening().await.ok_or_else(|| anyhow!("listen failed")));
            });
            let addr = SocketAddr::from((ip, port));
            if let Err(e) = axum_server::bind_rustls(addr, config).handle(h).serve(app.into_make_service()).await {
                let _ = tx.send(Err(anyhow!(e)));
            }
        });
    })?;
    let addr = rx
        .recv_timeout(Duration::from_secs(10))
        .map_err(|_| anyhow!("The server didn't start in time."))?
        .with_context(|| format!("Couldn't open port {port}. Is another copy of the server already running?"))?;

    let stop = Arc::new(AtomicBool::new(false));
    let discovery_port = match discovery_port {
        Some(dp) => Some(start_discovery(ip, dp, addr.port(), state.clone(), stop.clone())?),
        None => None,
    };
    Ok(ServerHandle { addr, discovery_port, state, http, stop })
}

fn start_discovery(ip: [u8; 4], discovery_port: u16, api_port: u16, state: Arc<ServerState>, stop: Arc<AtomicBool>) -> Result<u16> {
    let sock = UdpSocket::bind(SocketAddr::from((ip, discovery_port)))
        .with_context(|| format!("Couldn't open discovery port {discovery_port}"))?;
    sock.set_read_timeout(Some(Duration::from_millis(500)))?;
    let bound = sock.local_addr()?.port();
    std::thread::Builder::new().name("pos-discovery".into()).spawn(move || {
        let mut buf = [0u8; 64];
        while !stop.load(Ordering::SeqCst) {
            let Ok((n, from)) = sock.recv_from(&mut buf) else { continue };
            if &buf[..n] != DISCOVERY_PROBE {
                continue;
            }
            let shop_name = state.db.lock().unwrap().setting("shop_name").ok().flatten().unwrap_or_default();
            let reply = Announce { api: API_VERSION, shop_name, port: api_port };
            if let Ok(bytes) = serde_json::to_vec(&reply) {
                let _ = sock.send_to(&bytes, from);
            }
        }
    })?;
    Ok(bound)
}

// ---------------------------------------------------------------- handlers

type ApiResult<T> = std::result::Result<Json<T>, (StatusCode, Json<ApiError>)>;

fn fail(code: StatusCode, msg: impl Into<String>) -> (StatusCode, Json<ApiError>) {
    (code, Json(ApiError { error: msg.into() }))
}

fn bad(e: anyhow::Error) -> (StatusCode, Json<ApiError>) {
    fail(StatusCode::BAD_REQUEST, format!("{e:#}"))
}

/// The paired till making this request: (device id, till name).
fn device(s: &ServerState, headers: &HeaderMap) -> std::result::Result<(i64, String), (StatusCode, Json<ApiError>)> {
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(|| fail(StatusCode::UNAUTHORIZED, "This till isn't paired with the server."))?;
    s.db
        .lock()
        .unwrap()
        .device_for_token(&tls::sha256_hex(token.as_bytes()))
        .map_err(bad)?
        .ok_or_else(|| fail(StatusCode::UNAUTHORIZED, "This till was removed from the server. Pair it again in Settings."))
}

/// The manager behind a management request.
fn manager(s: &ServerState, headers: &HeaderMap) -> std::result::Result<i64, (StatusCode, Json<ApiError>)> {
    device(s, headers)?;
    let token = headers.get("x-session").and_then(|v| v.to_str().ok()).unwrap_or_default();
    let mut sessions = s.sessions.lock().unwrap();
    sessions.retain(|_, (_, at)| at.elapsed() < SESSION_VALID);
    let (uid, _) = sessions.get(token).copied().ok_or_else(|| fail(StatusCode::FORBIDDEN, "Sign in again as a manager to make changes."))?;
    // Still an active manager?
    let ok = s.db.lock().unwrap().users(false).map_err(bad)?.iter().any(|u| u.id == uid && u.is_manager());
    if !ok {
        return Err(fail(StatusCode::FORBIDDEN, "Only managers can make this change."));
    }
    Ok(uid)
}

async fn hello(State(s): State<Arc<ServerState>>) -> Json<Hello> {
    let shop_name = s.db.lock().unwrap().setting("shop_name").ok().flatten().unwrap_or_default();
    Json(Hello { api: API_VERSION, app_version: crate::updater::CURRENT_VERSION.into(), shop_name })
}

async fn pair(State(s): State<Arc<ServerState>>, Json(req): Json<PairRequest>) -> ApiResult<PairResponse> {
    let code = {
        let mut guard = s.pairing.lock().unwrap();
        let Some(p) = guard.as_mut().filter(|p| p.expires > Instant::now()) else {
            return Err(fail(StatusCode::FORBIDDEN, "No pairing code is active. On the server, open Settings and choose Add a till."));
        };
        let expected = tls::proof(&p.code, &["till", &s.fingerprint, &req.nonce]);
        if !tls::same(&expected, &req.proof) {
            p.fails += 1;
            if p.fails >= PAIRING_MAX_TRIES {
                *guard = None;
            }
            return Err(fail(
                StatusCode::FORBIDDEN,
                "That pairing code didn't work. Check the code shown on the server and try again.",
            ));
        }
        let code = p.code.clone();
        *guard = None; // single use
        code
    };
    let name = if req.till_name.trim().is_empty() { "Till".to_string() } else { req.till_name.trim().to_string() };
    let token = tls::random_hex(32);
    s.db.lock().unwrap().add_device(&name, &tls::sha256_hex(token.as_bytes())).map_err(bad)?;
    let shop_name = s.db.lock().unwrap().setting("shop_name").ok().flatten().unwrap_or_default();
    let proof = tls::proof(&code, &["server", &s.fingerprint, &req.nonce, &token]);
    (s.on_change)();
    Ok(Json(PairResponse { token, proof, shop_name }))
}

#[derive(Deserialize)]
struct Since {
    since: Option<i64>,
}

async fn snapshot(State(s): State<Arc<ServerState>>, headers: HeaderMap, Query(q): Query<Since>) -> ApiResult<Snapshot> {
    let (me, _) = device(&s, &headers)?;
    let snap = s.db.lock().unwrap().snapshot(q.since.unwrap_or(0), Some(me)).map_err(bad)?;
    Ok(Json(snap))
}

async fn sale(State(s): State<Arc<ServerState>>, headers: HeaderMap, Json(rec): Json<SaleRecord>) -> ApiResult<serde_json::Value> {
    let (_, till) = device(&s, &headers)?;
    let id = s.db.lock().unwrap().ingest_sale(&rec, &till).map_err(bad)?;
    (s.on_change)();
    Ok(Json(serde_json::json!({ "id": id })))
}

async fn void(State(s): State<Arc<ServerState>>, headers: HeaderMap, Json(rec): Json<VoidRecord>) -> ApiResult<serde_json::Value> {
    device(&s, &headers)?;
    s.db.lock().unwrap().ingest_void(&rec).map_err(bad)?;
    (s.on_change)();
    Ok(Json(serde_json::json!({})))
}

async fn session(State(s): State<Arc<ServerState>>, headers: HeaderMap, Json(req): Json<SessionRequest>) -> ApiResult<SessionResponse> {
    device(&s, &headers)?;
    let user = s.db.lock().unwrap().verify_pin(req.user_id, &req.pin).map_err(bad)?;
    match user {
        Some(u) if u.is_manager() => {
            let token = tls::random_hex(24);
            s.sessions.lock().unwrap().insert(token.clone(), (u.id, Instant::now()));
            Ok(Json(SessionResponse { session: token }))
        }
        Some(_) => Err(fail(StatusCode::FORBIDDEN, "Only managers can make this change.")),
        None => Err(fail(StatusCode::FORBIDDEN, "The server didn't accept that PIN. Sign in again.")),
    }
}

async fn product(State(s): State<Arc<ServerState>>, headers: HeaderMap, Json(c): Json<ProductChange>) -> ApiResult<serde_json::Value> {
    let uid = manager(&s, &headers)?;
    s.db.lock().unwrap().save_product(&c, uid).map_err(bad)?;
    (s.on_change)();
    Ok(Json(serde_json::json!({})))
}

async fn stock(State(s): State<Arc<ServerState>>, headers: HeaderMap, Json(c): Json<StockChange>) -> ApiResult<serde_json::Value> {
    let uid = manager(&s, &headers)?;
    let reason = if c.qty > 0 { "restock" } else { "adjust" };
    s.db.lock().unwrap().adjust_stock(c.product_id, c.qty, reason, &c.note, uid).map_err(bad)?;
    (s.on_change)();
    Ok(Json(serde_json::json!({})))
}

async fn user(State(s): State<Arc<ServerState>>, headers: HeaderMap, Json(c): Json<UserChange>) -> ApiResult<serde_json::Value> {
    manager(&s, &headers)?;
    let role = if c.role == "manager" { Role::Manager } else { Role::Cashier };
    let mut db = s.db.lock().unwrap();
    let res = match c.id {
        None => db.create_user(&c.name, role, c.pin.as_deref().unwrap_or_default()).map(|_| ()),
        Some(id) => db.update_user(id, &c.name, role, c.active).and_then(|_| match &c.pin {
            Some(pin) if !pin.is_empty() => db.set_pin(id, pin),
            _ => Ok(()),
        }),
    };
    res.map_err(bad)?;
    drop(db);
    (s.on_change)();
    Ok(Json(serde_json::json!({})))
}

async fn settings(State(s): State<Arc<ServerState>>, headers: HeaderMap, Json(c): Json<SettingsChange>) -> ApiResult<serde_json::Value> {
    manager(&s, &headers)?;
    let db = s.db.lock().unwrap();
    for (k, v) in &c.values {
        if !SHARED_SETTINGS.contains(&k.as_str()) {
            return Err(fail(StatusCode::BAD_REQUEST, format!("Tills can't change the setting {k}.")));
        }
        db.set_setting(k, v).map_err(bad)?;
    }
    drop(db);
    (s.on_change)();
    Ok(Json(serde_json::json!({})))
}
