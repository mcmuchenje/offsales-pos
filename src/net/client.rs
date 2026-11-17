//! The till's side of the link: discovery, pairing and API calls.

use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use serde::de::DeserializeOwned;
use serde::Serialize;

use super::protocol::*;
use super::tls::{self, PinnedVerifier};

/// "192.168.1.20" -> "192.168.1.20:47800"
pub fn normalize_addr(addr: &str) -> String {
    let a = addr.trim().trim_start_matches("https://").trim_end_matches('/');
    if a.parse::<SocketAddr>().is_ok() || (a.contains(':') && !a.contains("::")) {
        a.to_string()
    } else {
        format!("{a}:{DEFAULT_PORT}")
    }
}

pub struct Remote {
    addr: String,
    base: String,
    http: reqwest::blocking::Client,
    token: Option<String>,
}

fn http_client(verifier: std::sync::Arc<PinnedVerifier>, timeout: Duration) -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .use_preconfigured_tls(tls::client_config(verifier)?)
        .connect_timeout(Duration::from_secs(3))
        .timeout(timeout)
        .build()?)
}

impl Remote {
    /// A connection to a paired server; only its pinned certificate is accepted.
    pub fn connect(addr: &str, fingerprint: &str, token: &str) -> Result<Self> {
        let addr = normalize_addr(addr);
        Ok(Remote {
            base: format!("https://{addr}"),
            http: http_client(PinnedVerifier::new(Some(fingerprint.to_string())), Duration::from_secs(10))?,
            addr,
            token: Some(token.to_string()),
        })
    }

    fn unreachable(&self, e: reqwest::Error) -> anyhow::Error {
        // Surface a pinning failure as itself; everything else is "can't reach".
        let mut src: Option<&dyn std::error::Error> = Some(&e);
        while let Some(s) = src {
            if s.to_string().contains("isn't the server this till was paired with") {
                return anyhow!("{s}");
            }
            src = s.source();
        }
        anyhow!("Can't reach the server at {}.", self.addr)
    }

    fn send<T: DeserializeOwned>(&self, req: reqwest::blocking::RequestBuilder, session: Option<&str>) -> Result<T> {
        let mut req = req;
        if let Some(t) = &self.token {
            req = req.bearer_auth(t);
        }
        if let Some(s) = session {
            req = req.header("x-session", s);
        }
        let resp = req.send().map_err(|e| self.unreachable(e))?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp.json()?);
        }
        let msg = resp.json::<ApiError>().map(|e| e.error).unwrap_or_else(|_| format!("The server said {status}."));
        bail!(msg)
    }

    fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        self.send(self.http.get(format!("{}{path}", self.base)), None)
    }

    fn post<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B, session: Option<&str>) -> Result<T> {
        self.send(self.http.post(format!("{}{path}", self.base)).json(body), session)
    }

    pub fn hello(&self) -> Result<Hello> {
        let h: Hello = self.get("/api/hello")?;
        if h.api != API_VERSION {
            bail!(
                "The server runs version {} of the till software and this till runs {}. Update both to the same version.",
                h.app_version,
                crate::updater::CURRENT_VERSION
            );
        }
        Ok(h)
    }

    pub fn snapshot(&self, since: i64) -> Result<Snapshot> {
        self.get(&format!("/api/snapshot?since={since}"))
    }

    pub fn send_sale(&self, s: &SaleRecord) -> Result<()> {
        self.post::<_, serde_json::Value>("/api/sales", s, None).map(|_| ())
    }

    pub fn send_void(&self, v: &VoidRecord) -> Result<()> {
        self.post::<_, serde_json::Value>("/api/voids", v, None).map(|_| ())
    }

    /// Manager session for making changes; the server checks the PIN itself.
    pub fn session(&self, user_id: i64, pin: &str) -> Result<String> {
        let r: SessionResponse = self.post("/api/session", &SessionRequest { user_id, pin: pin.into() }, None)?;
        Ok(r.session)
    }

    pub fn change_product(&self, session: &str, c: &ProductChange) -> Result<()> {
        self.post::<_, serde_json::Value>("/api/products", c, Some(session)).map(|_| ())
    }
    pub fn change_stock(&self, session: &str, c: &StockChange) -> Result<()> {
        self.post::<_, serde_json::Value>("/api/stock", c, Some(session)).map(|_| ())
    }
    pub fn change_user(&self, session: &str, c: &UserChange) -> Result<()> {
        self.post::<_, serde_json::Value>("/api/users", c, Some(session)).map(|_| ())
    }
    pub fn change_settings(&self, session: &str, c: &SettingsChange) -> Result<()> {
        self.post::<_, serde_json::Value>("/api/settings", c, Some(session)).map(|_| ())
    }
}

#[derive(Debug)]
pub struct Paired {
    pub addr: String,
    pub fingerprint: String,
    pub token: String,
    pub shop_name: String,
}

/// Pairs with a server using the code shown on its screen. Both sides prove
/// they know the code *and* saw the same certificate, so a device in the
/// middle can't pose as the server.
pub fn pair(addr: &str, code: &str, till_name: &str) -> Result<Paired> {
    let addr = normalize_addr(addr);
    let code = code.trim().replace([' ', '-'], "");
    if code.len() != 8 || !code.bytes().all(|b| b.is_ascii_digit()) {
        bail!("The pairing code is 8 digits. It's shown on the server under Settings, Add a till.");
    }
    let verifier = PinnedVerifier::new(None);
    let remote = Remote {
        base: format!("https://{addr}"),
        http: http_client(verifier.clone(), Duration::from_secs(10))?,
        addr: addr.clone(),
        token: None,
    };
    remote.hello()?;
    let fingerprint = verifier.seen().ok_or_else(|| anyhow!("Couldn't read the server's certificate."))?;
    let nonce = tls::random_hex(16);
    let req = PairRequest { till_name: till_name.trim().into(), nonce: nonce.clone(), proof: tls::proof(&code, &["till", &fingerprint, &nonce]) };
    let resp: PairResponse = remote.post("/api/pair", &req, None)?;
    let expected = tls::proof(&code, &["server", &fingerprint, &nonce, &resp.token]);
    if !tls::same(&expected, &resp.proof) {
        bail!("The server couldn't prove it's the shop's server. Don't pair this till; check it's on the shop network.");
    }
    Ok(Paired { addr, fingerprint, token: resp.token, shop_name: resp.shop_name })
}

#[derive(Clone, Debug)]
pub struct Found {
    pub addr: String,
    pub shop_name: String,
}

/// Asks the LAN who's a server (UDP broadcast), waiting up to `wait`.
/// `also` adds direct targets (e.g. 127.0.0.1 in tests).
pub fn discover(discovery_port: u16, wait: Duration, also: &[std::net::IpAddr]) -> Vec<Found> {
    let mut found: Vec<Found> = vec![];
    let Ok(sock) = UdpSocket::bind(("0.0.0.0", 0)) else { return found };
    let _ = sock.set_broadcast(true);
    let _ = sock.send_to(DISCOVERY_PROBE, ("255.255.255.255", discovery_port));
    for ip in also {
        let _ = sock.send_to(DISCOVERY_PROBE, (*ip, discovery_port));
    }
    let deadline = Instant::now() + wait;
    let mut buf = [0u8; 1024];
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let _ = sock.set_read_timeout(Some(left.max(Duration::from_millis(10))));
        let Ok((n, from)) = sock.recv_from(&mut buf) else { break };
        let Ok(a) = serde_json::from_slice::<Announce>(&buf[..n]) else { continue };
        let addr = format!("{}:{}", from.ip(), a.port);
        if a.api == API_VERSION && !found.iter().any(|f| f.addr == addr) {
            found.push(Found { addr, shop_name: a.shop_name });
        }
    }
    found
}
