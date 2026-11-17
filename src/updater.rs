//! Update checks against GitHub Releases.
//!
//! A release is expected to have a tag like `v0.2.0` and an asset named
//! `offsales-pos-<version>-x86_64-pc-windows-msvc.zip` containing `offsales-pos.exe`
//! (the release workflow in `.github/workflows/release.yml` produces exactly this).
//! The POS works fully offline; update checks simply report "offline" if there is
//! no internet connection.

use std::sync::{Arc, Mutex};

use self_update::backends::github::Update;
use self_update::update::ReleaseUpdate;

pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const BIN_NAME: &str = "offsales-pos";

/// The GitHub "owner/repo" every installation updates from.
pub const REPO: &str = "mcmuchenje/offsales-pos";

#[derive(Clone, Debug, Default)]
pub enum UpdateState {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available { version: String, notes: String },
    Installing,
    /// Installed; the app must be restarted to use the new version.
    Installed { version: String },
    Error(String),
}

pub type Shared = Arc<Mutex<UpdateState>>;

fn builder(repo: &str) -> Result<Box<dyn ReleaseUpdate>, String> {
    let (owner, name) = repo
        .trim()
        .split_once('/')
        .filter(|(o, n)| !o.is_empty() && !n.is_empty())
        .ok_or_else(|| "The update source isn't a valid GitHub owner/repo".to_string())?;
    Update::configure()
        .repo_owner(owner)
        .repo_name(name)
        .bin_name(BIN_NAME)
        .current_version(CURRENT_VERSION)
        .show_download_progress(false)
        .show_output(false)
        .no_confirm(true)
        .build()
        .map_err(|e| e.to_string())
}

fn friendly(e: impl ToString) -> String {
    let s = e.to_string();
    let l = s.to_lowercase();
    if l.contains("dns") || l.contains("connect") || l.contains("resolve") || l.contains("timed out") {
        "Offline — could not reach the update server. The POS keeps working normally.".into()
    } else {
        s
    }
}

/// Checks for a newer release on a background thread.
pub fn check(state: Shared, repo: String, ctx: eframe::egui::Context) {
    *state.lock().unwrap() = UpdateState::Checking;
    std::thread::spawn(move || {
        let result = (|| -> Result<UpdateState, String> {
            let rel = builder(&repo)?.get_latest_release().map_err(friendly)?;
            let newer = self_update::version::bump_is_greater(CURRENT_VERSION, &rel.version)
                .map_err(|e| e.to_string())?;
            Ok(if newer {
                UpdateState::Available { version: rel.version, notes: rel.body.unwrap_or_default() }
            } else {
                UpdateState::UpToDate
            })
        })();
        *state.lock().unwrap() = result.unwrap_or_else(UpdateState::Error);
        ctx.request_repaint();
    });
}

/// Downloads and swaps in the latest release executable on a background thread.
pub fn install(state: Shared, repo: String, ctx: eframe::egui::Context) {
    *state.lock().unwrap() = UpdateState::Installing;
    std::thread::spawn(move || {
        let result = builder(&repo).and_then(|u| u.update().map_err(friendly));
        *state.lock().unwrap() = match result {
            Ok(status) if status.updated() => {
                record_installed_version(status.version());
                UpdateState::Installed { version: status.version().to_string() }
            }
            Ok(_) => UpdateState::UpToDate,
            Err(e) => UpdateState::Error(e),
        };
        ctx.request_repaint();
    });
}

/// Keeps the version shown in Windows "Apps & features" in step after an in-app
/// update. Only touches the entry the installer created; does nothing otherwise.
fn record_installed_version(version: &str) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\OffSalesPOS";
        let exists = std::process::Command::new("reg")
            .args(["query", key])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .is_ok_and(|o| o.status.success());
        if exists {
            let _ = std::process::Command::new("reg")
                .args(["add", key, "/v", "DisplayVersion", "/t", "REG_SZ", "/d", version, "/f"])
                .creation_flags(CREATE_NO_WINDOW)
                .output();
        }
    }
    #[cfg(not(windows))]
    let _ = version;
}

/// Relaunches the (now updated) executable and exits this process.
pub fn restart() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).spawn();
    }
    std::process::exit(0);
}
