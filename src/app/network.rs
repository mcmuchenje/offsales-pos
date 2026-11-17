//! Network modes in the app: standalone, server (back-office PC) or till.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use eframe::egui::{self, vec2, Align, Color32, CornerRadius, Layout, Margin, RichText, Sense, Ui};

use super::{field, hairline, note_chip, PosApp};
use crate::db::Db;
use crate::net::client::{self, discover, Found, Remote};
use crate::net::protocol::{DEFAULT_PORT, DISCOVERY_PORT};
use crate::net::server::{self, ServerHandle};
use crate::net::tls;
use crate::net::worker::{pairing, sync_once, SyncWorker};
use crate::theme::{self as t, Width};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Mode {
    Standalone,
    Server,
    Till,
}

impl Mode {
    fn from_setting(s: &str) -> Mode {
        match s {
            "server" => Mode::Server,
            "till" => Mode::Till,
            _ => Mode::Standalone,
        }
    }
    fn as_setting(self) -> &'static str {
        match self {
            Mode::Standalone => "standalone",
            Mode::Server => "server",
            Mode::Till => "till",
        }
    }
}

#[derive(Default)]
pub(super) struct PairForm {
    addr: String,
    code: String,
    till_name: String,
    found: Option<Vec<Found>>,
    confirm_wipe: bool,
    error: Option<String>,
    open: bool,
}

pub(super) struct Net {
    pub mode: Mode,
    server: Option<ServerHandle>,
    server_error: Option<String>,
    sync: Option<SyncWorker>,
    /// Set by the server when a till changes data; the screens reload.
    changed: Arc<AtomicBool>,
    /// Manager's server session (till mode), made on first change after sign-in.
    session: Option<String>,
    /// The signed-in manager's PIN, kept in memory only, to open a server session.
    pin: Option<String>,
    pair: PairForm,
    confirm: Option<&'static str>,
    revoke: Option<i64>,
}

impl Net {
    pub(super) fn new(mode_setting: &str) -> Net {
        Net {
            mode: Mode::from_setting(mode_setting),
            server: None,
            server_error: None,
            sync: None,
            changed: Arc::new(AtomicBool::new(false)),
            session: None,
            pin: None,
            pair: PairForm::default(),
            confirm: None,
            revoke: None,
        }
    }

    pub(super) fn is_till(&self) -> bool {
        self.mode == Mode::Till
    }

    /// Remembers (manager) or forgets the PIN used to open a server session.
    pub(super) fn signed_in(&mut self, manager_pin: Option<String>) {
        self.pin = manager_pin;
        self.session = None;
    }

    pub(super) fn sync_now(&self) {
        if let Some(w) = &self.sync {
            w.sync_now();
        }
    }

    /// True once after the server or sync thread changed data.
    pub(super) fn take_changed(&self) -> bool {
        let a = self.changed.swap(false, Ordering::SeqCst);
        let b = self.sync.as_ref().is_some_and(|w| w.changed.swap(false, Ordering::SeqCst));
        a || b
    }
}

impl PosApp {
    /// Starts whatever this computer's mode needs: the server, or the till's sync.
    pub(super) fn start_network(&mut self, ctx: &egui::Context) {
        let repaint = {
            let ctx = ctx.clone();
            Arc::new(move || ctx.request_repaint()) as Arc<dyn Fn() + Send + Sync>
        };
        match self.net.mode {
            Mode::Standalone => {}
            Mode::Server if self.net.server.is_none() => {
                let changed = self.net.changed.clone();
                let r = repaint.clone();
                let on_change = Arc::new(move || {
                    changed.store(true, Ordering::SeqCst);
                    r();
                });
                // Development builds can keep the server on this computer only.
                let loopback = cfg!(feature = "dev-tools") && std::env::var_os("POS_SERVER_LOOPBACK").is_some();
                match server::start(&self.db_path, loopback, DEFAULT_PORT, Some(DISCOVERY_PORT), on_change) {
                    Ok(h) => {
                        self.net.server = Some(h);
                        self.net.server_error = None;
                    }
                    Err(e) => self.net.server_error = Some(format!("{e:#}")),
                }
            }
            Mode::Till if self.net.sync.is_none() => {
                self.net.sync = Some(SyncWorker::start(&self.db_path, repaint));
            }
            _ => {}
        }
    }

    fn set_mode(&mut self, mode: Mode) {
        if let Some(db) = &self.db {
            let _ = db.set_setting("mode", mode.as_setting());
        }
        if mode != Mode::Server {
            if let Some(s) = self.net.server.take() {
                s.stop();
            }
        }
        if mode != Mode::Till {
            self.net.sync = None;
        }
        self.net.mode = mode;
    }

    /// A change only managers make. Standalone/server: straight into the
    /// database. Till: sent to the server (which checks the manager's PIN
    /// itself), then the till pulls the result.
    pub(super) fn admin(
        &mut self,
        local: impl FnOnce(&mut Db, i64) -> anyhow::Result<()>,
        remote: impl Fn(&Remote, &str) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let uid = self.user_id();
        if !self.net.is_till() {
            let db = self.db.as_mut().ok_or_else(|| anyhow::anyhow!("No database"))?;
            return local(db, uid);
        }
        let db = self.db.as_mut().ok_or_else(|| anyhow::anyhow!("No database"))?;
        let (addr, fp, token) = pairing(db).ok_or_else(|| anyhow::anyhow!("This till isn't connected to a server."))?;
        let r = Remote::connect(&addr, &fp, &token)?;
        let offline = |e: anyhow::Error| {
            if e.to_string().starts_with("Can't reach the server") {
                anyhow::anyhow!("Can't reach the server, so this change wasn't saved. Selling still works; try again when the server is back.")
            } else {
                e
            }
        };
        for attempt in 0..2 {
            let session = match &self.net.session {
                Some(s) if attempt == 0 => s.clone(),
                _ => {
                    let pin = self.net.pin.clone().ok_or_else(|| anyhow::anyhow!("Sign in again as a manager to make changes."))?;
                    let s = r.session(uid, &pin).map_err(offline)?;
                    self.net.session = Some(s.clone());
                    s
                }
            };
            match remote(&r, &session) {
                Ok(()) => {
                    let db = self.db.as_mut().expect("checked above");
                    let _ = sync_once(db, &r);
                    self.net.sync_now();
                    return Ok(());
                }
                // Session expired (server restarted): get a new one and retry once.
                Err(e) if attempt == 0 && e.to_string().starts_with("Sign in again") => continue,
                Err(e) => return Err(offline(e)),
            }
        }
        unreachable!()
    }

    // ------------------------------------------------------------ status

    /// Small connection indicator for the top bar.
    pub(super) fn net_indicator(&self, ui: &mut Ui, compact: bool) {
        let (text, color) = match self.net.mode {
            Mode::Standalone => return,
            Mode::Server => match &self.net.server_error {
                None => ("Server on".to_string(), t::TILL_GREEN),
                Some(_) => ("Server stopped".to_string(), t::RED),
            },
            Mode::Till => {
                let st = self.net.sync.as_ref().map(|w| w.status()).unwrap_or_default();
                let waiting = st.unsent_sales + st.unsent_voids;
                if st.online && waiting == 0 {
                    ("Online".to_string(), t::TILL_GREEN)
                } else if st.online {
                    (format!("Sending {waiting}"), t::GOLD)
                } else if waiting > 0 && !compact {
                    (format!("Offline, {waiting} to send"), t::GOLD)
                } else {
                    ("Offline".to_string(), t::GOLD)
                }
            }
        };
        // On narrow windows only the coloured square shows; the words are in its tooltip.
        let resp = ui
            .horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                if !compact {
                    ui.label(RichText::new(&text).color(t::ON_INK));
                }
                let (r, _) = ui.allocate_exact_size(vec2(9.0, 9.0), Sense::hover());
                ui.painter().rect_filled(r, CornerRadius::ZERO, color);
            })
            .response;
        let resp = if compact { resp.on_hover_text(&text) } else { resp };
        if self.net.mode == Mode::Till {
            let st = self.net.sync.as_ref().map(|w| w.status()).unwrap_or_default();
            let tip = match (&st.problem, &st.last_sync) {
                (Some(p), _) => format!("{p} Sales are kept on this till and sent when the server is back."),
                (None, Some(at)) => format!("Up to date with the server at {at}."),
                _ => "Connecting to the server\u{2026}".into(),
            };
            resp.on_hover_text(tip);
        }
    }

    // ------------------------------------------------------------ first run on a till

    /// A paired till that hasn't received the server's data yet.
    pub(super) fn ui_waiting(&mut self, ctx: &egui::Context) {
        if self.net.take_changed() {
            self.dirty = true;
        }
        let st = self.net.sync.as_ref().map(|w| w.status()).unwrap_or_default();
        egui::CentralPanel::default().frame(egui::Frame::new().fill(t::INK).inner_margin(Margin::same(32))).show(ctx, |ui| {
            ui.label(RichText::new("Connecting to the server").font(t::display(30.0)).color(Color32::WHITE));
            ui.add_space(8.0);
            let msg = st.problem.unwrap_or_else(|| "Downloading products and staff\u{2026}".into());
            ui.label(RichText::new(msg).color(t::ON_INK_SOFT).size(16.0));
            ui.add_space(12.0);
            if ui.add(t::primary("Try again", 16.0)).clicked() {
                self.net.sync_now();
            }
        });
        ctx.request_repaint_after(Duration::from_secs(1));
    }

    // ------------------------------------------------------------ pairing form

    /// Connects this computer to a server as a till. Returns true once paired.
    pub(super) fn pair_form(&mut self, ui: &mut Ui, first_run: bool) -> bool {
        let mut connect = false;
        let mut search = false;
        {
            let f = &mut self.net.pair;
            t::caption(ui, "On the server, open Settings and choose Add a till. It shows an 8-digit code to type here.");
            ui.add_space(4.0);
            if ui.button("Search the shop network").clicked() {
                search = true;
            }
            match &f.found {
                Some(list) if list.is_empty() => {
                    t::caption(ui, "No server answered. Check the server is on and the app is open there, or type its address below.");
                }
                Some(list) => {
                    ui.horizontal_wrapped(|ui| {
                        for s in list.clone() {
                            let label = format!("{} ({})", s.shop_name, s.addr);
                            if note_chip(ui, &label, f.addr == s.addr).clicked() {
                                f.addr = s.addr.clone();
                            }
                        }
                    });
                }
                None => {}
            }
            ui.add_space(4.0);
            field(ui, "Server address", &mut f.addr, "e.g. 192.168.1.20");
            field(ui, "This till's name", &mut f.till_name, "e.g. Front counter");
            field(ui, "Pairing code", &mut f.code, "8 digits");
            if !first_run {
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Connecting replaces this computer's products, staff and sales with the server's.")
                        .color(t::AMBER),
                );
                ui.checkbox(&mut f.confirm_wipe, "I understand");
            }
            ui.add_space(6.0);
            let ready = !f.addr.trim().is_empty() && !f.code.trim().is_empty() && (first_run || f.confirm_wipe);
            if ui.add_enabled(ready, t::primary("Connect to server", 16.0)).clicked() {
                connect = true;
            }
            if let Some(e) = &f.error {
                ui.add(egui::Label::new(RichText::new(e).color(t::RED)).wrap());
            }
        }
        if search {
            self.net.pair.found = Some(discover(DISCOVERY_PORT, Duration::from_millis(1500), &[]));
            if let Some(first) = self.net.pair.found.as_ref().and_then(|l| l.first()) {
                if self.net.pair.addr.trim().is_empty() {
                    self.net.pair.addr = first.addr.clone();
                }
            }
        }
        if connect {
            return self.connect_to_server(ui.ctx());
        }
        false
    }

    fn connect_to_server(&mut self, ctx: &egui::Context) -> bool {
        let f = &self.net.pair;
        let name = if f.till_name.trim().is_empty() { "Till".to_string() } else { f.till_name.trim().to_string() };
        let paired = match client::pair(&f.addr, &f.code, &name) {
            Ok(p) => p,
            Err(e) => {
                self.net.pair.error = Some(format!("{e:#}"));
                return false;
            }
        };
        let Some(db) = self.db.as_mut() else { return false };
        let res = (|| -> anyhow::Result<()> {
            db.wipe_for_till()?;
            db.set_setting("server_addr", &paired.addr)?;
            db.set_setting("server_fingerprint", &paired.fingerprint)?;
            db.set_setting("device_token", &paired.token)?;
            db.set_setting("till_name", &name)?;
            Ok(())
        })();
        if let Err(e) = res {
            self.net.pair.error = Some(format!("{e:#}"));
            return false;
        }
        // Get staff and products right away so people can sign in.
        if let Ok(r) = Remote::connect(&paired.addr, &paired.fingerprint, &paired.token) {
            let _ = sync_once(db, &r);
        }
        self.set_mode(Mode::Till);
        self.net.pair = PairForm::default();
        self.user = None;
        self.cart.clear();
        self.dirty = true;
        self.start_network(ctx);
        self.login.reset_with_message(format!("Connected to {}. Sign in to start selling.", paired.shop_name));
        true
    }

    // ------------------------------------------------------------ settings

    pub(super) fn ui_network_settings(&mut self, ui: &mut Ui, width: Width) {
        t::surface().show(ui, |ui| {
            ui.set_width(ui.available_width());
            t::heading(ui, "Network");
            match self.net.mode {
                Mode::Standalone => self.network_standalone(ui),
                Mode::Server => self.network_server(ui, width),
                Mode::Till => self.network_till(ui),
            }
        });
    }

    fn network_standalone(&mut self, ui: &mut Ui) {
        t::caption(
            ui,
            "This till works on its own. To run several tills, make the back-office computer the server, then connect each till to it.",
        );
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            if ui.add(t::primary("Make this computer the server", 15.0)).clicked() {
                self.set_mode(Mode::Server);
                self.start_network(ui.ctx());
                match &self.net.server_error {
                    None => self.ok("This computer is now the server. Add tills from Settings, Network."),
                    Some(e) => {
                        let e = e.clone();
                        self.err(e);
                    }
                }
            }
            if ui.button("Connect to a server").clicked() {
                self.net.pair.open = !self.net.pair.open;
            }
        });
        if self.net.pair.open {
            ui.add_space(8.0);
            hairline(ui);
            ui.add_space(4.0);
            self.pair_form(ui, false);
        }
    }

    fn network_server(&mut self, ui: &mut Ui, width: Width) {
        t::caption(ui, "This computer is the server. Keep the app open here; tills keep selling if it's closed and catch up later.");
        if let Some(e) = self.net.server_error.clone() {
            ui.add(egui::Label::new(RichText::new(e).color(t::RED)).wrap());
            if ui.button("Start the server again").clicked() {
                self.start_network(ui.ctx());
            }
            return;
        }
        let Some(srv) = &self.net.server else { return };
        ui.add_space(4.0);
        let ip = local_ip().map(|ip| ip.to_string()).unwrap_or_else(|| "this computer's IP address".into());
        info_row(ui, "Address for tills", &format!("{ip}:{}", srv.addr.port()));
        info_row(ui, "Security code", &tls::short_fingerprint(srv.fingerprint()));
        ui.add_space(8.0);

        // Pairing code
        match srv.pairing_code() {
            Some((code, secs)) => {
                egui::Frame::new().fill(t::INK).inner_margin(Margin::same(16)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new("Pairing code").color(t::ON_INK_SOFT));
                    let spaced = format!("{} {}", &code[..4], &code[4..]);
                    ui.label(RichText::new(spaced).font(t::display(if width == Width::Narrow { 36.0 } else { 48.0 })).color(t::GOLD));
                    ui.label(
                        RichText::new(format!("Type it on the new till within {}:{:02}. It works once.", secs / 60, secs % 60))
                            .color(t::ON_INK),
                    );
                });
                if ui.button("Cancel code").clicked() {
                    srv.cancel_pairing();
                }
                ui.ctx().request_repaint_after(Duration::from_secs(1));
            }
            None => {
                if ui.add(t::primary("Add a till", 15.0)).clicked() {
                    srv.new_pairing_code();
                }
            }
        }

        // Paired tills
        ui.add_space(10.0);
        ui.label(RichText::new("Tills").font(t::semibold(15.0)).color(t::INK));
        let devices = self.db.as_ref().and_then(|d| d.devices().ok()).unwrap_or_default();
        if devices.is_empty() {
            t::caption(ui, "No tills yet. Choose Add a till, then type the code on the till.");
        }
        let mut remove = None;
        for (id, name, seen) in &devices {
            ui.horizontal(|ui| {
                ui.label(RichText::new(name).color(t::INK));
                let seen = match seen {
                    Some(s) => format!("last seen {}", super::pretty_time(s)),
                    None => "not seen yet".into(),
                };
                ui.label(RichText::new(seen).color(t::INK_FAINT).size(13.0));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if self.net.revoke == Some(*id) {
                        if ui.button("Keep").clicked() {
                            self.net.revoke = None;
                        }
                        if ui.add(t::danger("Remove till")).clicked() {
                            remove = Some(*id);
                        }
                    } else if ui.button("Remove").on_hover_text("The till stops syncing until it's paired again").clicked() {
                        self.net.revoke = Some(*id);
                    }
                });
            });
            hairline(ui);
        }
        if let Some(id) = remove {
            if let Some(db) = &self.db {
                match db.revoke_device(id) {
                    Ok(()) => self.ok("Till removed. It keeps its data but can't sync until it's paired again."),
                    Err(e) => self.err(e),
                }
            }
            self.net.revoke = None;
        }

        // Windows integration
        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            if ui.button("Allow through Windows Firewall").clicked() {
                match allow_firewall() {
                    Ok(()) => self.ok("Windows will ask for permission. Choose Yes so tills can reach this computer."),
                    Err(e) => self.err(e),
                }
            }
            let mut auto = autostart_enabled();
            if ui.checkbox(&mut auto, "Start with Windows").changed() {
                match set_autostart(auto) {
                    Ok(()) => self.ok(if auto { "The server will start when Windows starts." } else { "The server won't start with Windows." }),
                    Err(e) => self.err(e),
                }
            }
        });
        t::caption(ui, "Tills can't connect until the firewall allows it. You only need to do this once.");

        ui.add_space(10.0);
        if self.net.confirm == Some("stop-server") {
            ui.label(RichText::new("Tills will keep selling but can't sync until this computer is the server again.").color(t::AMBER));
            ui.horizontal(|ui| {
                if ui.add(t::danger("Stop being the server")).clicked() {
                    self.set_mode(Mode::Standalone);
                    self.net.confirm = None;
                    self.ok("This computer is a single till again.");
                }
                if ui.button("Keep").clicked() {
                    self.net.confirm = None;
                }
            });
        } else if ui.button("Stop being the server").clicked() {
            self.net.confirm = Some("stop-server");
        }
    }

    fn network_till(&mut self, ui: &mut Ui) {
        let st = self.net.sync.as_ref().map(|w| w.status()).unwrap_or_default();
        let get = |k: &str| self.db.as_ref().and_then(|d| d.setting(k).ok().flatten()).unwrap_or_default();
        let (addr, name, fp) = (get("server_addr"), get("till_name"), get("server_fingerprint"));
        t::caption(ui, "This till works from the server's data and keeps selling when the server can't be reached.");
        ui.add_space(4.0);
        info_row(ui, "This till", &name);
        info_row(ui, "Server", &addr);
        info_row(ui, "Security code", &tls::short_fingerprint(&fp));
        let state = if st.online { "Connected".to_string() } else { "Can't reach the server".to_string() };
        info_row(ui, "Status", &state);
        if let Some(at) = &st.last_sync {
            info_row(ui, "Last synced", at);
        }
        let waiting = st.unsent_sales + st.unsent_voids;
        info_row(ui, "Waiting to send", &if waiting == 0 { "Nothing".to_string() } else { waiting.to_string() });
        if let Some(p) = &st.problem {
            ui.add(egui::Label::new(RichText::new(p).color(t::AMBER)).wrap());
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("Sync now").clicked() {
                self.net.sync_now();
            }
        });
        ui.add_space(10.0);
        if waiting > 0 {
            t::caption(ui, "To disconnect this till from the server, first let it send its waiting sales.");
        } else if self.net.confirm == Some("leave") {
            ui.label(RichText::new("This till will stop syncing and keep a copy of the data as it is now.").color(t::AMBER));
            ui.horizontal(|ui| {
                if ui.add(t::danger("Disconnect")).clicked() {
                    if let Some(db) = &self.db {
                        for k in ["server_addr", "server_fingerprint", "device_token"] {
                            let _ = db.set_setting(k, "");
                        }
                    }
                    self.set_mode(Mode::Standalone);
                    self.net.confirm = None;
                    self.ok("Disconnected. This computer is a single till now.");
                }
                if ui.button("Keep").clicked() {
                    self.net.confirm = None;
                }
            });
        } else if ui.button("Disconnect from server").clicked() {
            self.net.confirm = Some("leave");
        }
    }
}

fn info_row(ui: &mut Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(vec2(150.0, 22.0), Layout::left_to_right(Align::Center), |ui| {
            ui.set_min_width(150.0);
            ui.label(RichText::new(label).color(t::INK_SOFT));
        });
        ui.label(RichText::new(value).font(t::semibold(15.0)).color(t::INK));
    });
}

/// The address other computers on the LAN use to reach this one.
fn local_ip() -> Option<std::net::IpAddr> {
    // No packet is sent: connecting a UDP socket only picks the outgoing interface.
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.168.255.255:9").or_else(|_| s.connect("10.255.255.255:9")).ok()?;
    s.local_addr().ok().map(|a| a.ip()).filter(|ip| !ip.is_unspecified())
}

#[cfg(windows)]
fn hidden(cmd: &mut std::process::Command) -> &mut std::process::Command {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x0800_0000)
}

/// Adds an inbound allow rule for this program (private and domain networks).
/// Windows asks for administrator permission.
fn allow_firewall() -> anyhow::Result<()> {
    let exe = std::env::current_exe()?.display().to_string();
    let rule = format!(
        "/c netsh advfirewall firewall delete rule name=\"Off Sales POS\" & netsh advfirewall firewall add rule name=\"Off Sales POS\" dir=in action=allow program=\"{exe}\" enable=yes profile=private,domain"
    );
    let ps = format!("Start-Process cmd -Verb RunAs -WindowStyle Hidden -ArgumentList '{}'", rule.replace('\'', "''"));
    #[cfg(windows)]
    {
        hidden(std::process::Command::new("powershell").args(["-NoProfile", "-Command", &ps])).spawn()?;
    }
    #[cfg(not(windows))]
    let _ = ps;
    Ok(())
}

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

fn autostart_enabled() -> bool {
    #[cfg(windows)]
    {
        return hidden(std::process::Command::new("reg").args(["query", RUN_KEY, "/v", "Off Sales POS"]))
            .output()
            .is_ok_and(|o| o.status.success());
    }
    #[allow(unreachable_code)]
    false
}

fn set_autostart(on: bool) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        let exe = format!("\"{}\"", std::env::current_exe()?.display());
        let mut cmd = std::process::Command::new("reg");
        if on {
            cmd.args(["add", RUN_KEY, "/v", "Off Sales POS", "/t", "REG_SZ", "/d", &exe, "/f"]);
        } else {
            cmd.args(["delete", RUN_KEY, "/v", "Off Sales POS", "/f"]);
        }
        let out = hidden(&mut cmd).output()?;
        if !out.status.success() {
            anyhow::bail!("Windows didn't accept the change.");
        }
    }
    #[cfg(not(windows))]
    let _ = on;
    Ok(())
}
