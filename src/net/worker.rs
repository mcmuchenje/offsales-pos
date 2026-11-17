//! Background sync on a till: send unsent sales and voids, then pull the
//! server's latest data. Runs every few seconds and whenever woken.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;

use super::client::{discover, Remote};
use super::protocol::DISCOVERY_PORT;
use crate::db::Db;

const INTERVAL: Duration = Duration::from_secs(5);
const REDISCOVER_EVERY: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, Default)]
pub struct SyncStatus {
    pub online: bool,
    /// Local time of the last successful sync, "14:02".
    pub last_sync: Option<String>,
    pub unsent_sales: i64,
    pub unsent_voids: i64,
    /// Why the last attempt failed, in plain words.
    pub problem: Option<String>,
}

pub struct SyncWorker {
    pub status: Arc<Mutex<SyncStatus>>,
    /// Set after each sync that may have changed data; the UI reloads and clears it.
    pub changed: Arc<AtomicBool>,
    wake: Sender<()>,
    stop: Arc<AtomicBool>,
}

impl SyncWorker {
    pub fn start(db_path: &Path, repaint: Arc<dyn Fn() + Send + Sync>) -> SyncWorker {
        let status = Arc::new(Mutex::new(SyncStatus::default()));
        let changed = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (wake, rx) = mpsc::channel();
        let ctx = Ctx { path: db_path.to_path_buf(), status: status.clone(), changed: changed.clone(), stop: stop.clone(), repaint };
        let _ = std::thread::Builder::new().name("pos-sync".into()).spawn(move || run(ctx, rx));
        SyncWorker { status, changed, wake, stop }
    }

    /// Sync now instead of waiting for the next round.
    pub fn sync_now(&self) {
        let _ = self.wake.send(());
    }

    pub fn status(&self) -> SyncStatus {
        self.status.lock().unwrap().clone()
    }
}

impl Drop for SyncWorker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.wake.send(());
    }
}

struct Ctx {
    path: PathBuf,
    status: Arc<Mutex<SyncStatus>>,
    changed: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    repaint: Arc<dyn Fn() + Send + Sync>,
}

/// The pairing stored on this till: (address, fingerprint, token).
pub fn pairing(db: &Db) -> Option<(String, String, String)> {
    Some((db.setting("server_addr").ok()??, db.setting("server_fingerprint").ok()??, db.setting("device_token").ok()??))
}

/// One round: push, then pull. Returns Ok even when nothing changed.
pub fn sync_once(db: &mut Db, remote: &Remote) -> Result<()> {
    for s in db.pending_sales()? {
        remote.send_sale(&s)?;
        db.mark_sale_sent(&s.uuid)?;
    }
    for v in db.pending_voids()? {
        remote.send_void(&v)?;
        db.mark_void_sent(&v.uuid)?;
    }
    let snap = remote.snapshot(db.sync_rev())?;
    db.apply_snapshot(&snap)?;
    Ok(())
}

fn run(ctx: Ctx, rx: Receiver<()>) {
    let mut db = match Db::open(&ctx.path) {
        Ok(db) => db,
        Err(e) => {
            ctx.status.lock().unwrap().problem = Some(format!("{e:#}"));
            return;
        }
    };
    let mut remote: Option<(String, Remote)> = None;
    let mut last_discovery: Option<Instant> = None;

    while !ctx.stop.load(Ordering::SeqCst) {
        if let Some((addr, fp, token)) = pairing(&db) {
            let key = format!("{addr}|{fp}|{token}");
            if remote.as_ref().is_none_or(|(k, _)| *k != key) {
                remote = Remote::connect(&addr, &fp, &token).ok().map(|r| (key, r));
            }
            let result = match &remote {
                Some((_, r)) => sync_once(&mut db, r),
                None => Err(anyhow::anyhow!("The pairing details on this till are damaged. Pair it again.")),
            };
            let (sales, voids) = db.unsent_counts().unwrap_or((0, 0));
            {
                let mut st = ctx.status.lock().unwrap();
                st.unsent_sales = sales;
                st.unsent_voids = voids;
                match &result {
                    Ok(()) => {
                        st.online = true;
                        st.problem = None;
                        st.last_sync = Some(chrono::Local::now().format("%H:%M").to_string());
                    }
                    Err(e) => {
                        st.online = false;
                        st.problem = Some(format!("{e:#}"));
                    }
                }
            }
            if result.is_ok() {
                ctx.changed.store(true, Ordering::SeqCst);
            } else if last_discovery.is_none_or(|t| t.elapsed() >= REDISCOVER_EVERY) {
                // The server may have a new address (DHCP). Find it by its pinned certificate.
                last_discovery = Some(Instant::now());
                for f in discover(DISCOVERY_PORT, Duration::from_millis(1500), &[]) {
                    if f.addr != addr && Remote::connect(&f.addr, &fp, &token).and_then(|r| r.hello()).is_ok() {
                        let _ = db.set_setting("server_addr", &f.addr);
                        break;
                    }
                }
            }
            (ctx.repaint)();
        }
        match rx.recv_timeout(INTERVAL) {
            Ok(()) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}
