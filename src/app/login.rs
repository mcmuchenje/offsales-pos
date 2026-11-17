//! Sign-in (tap your name, enter your PIN), first-run manager setup, locking.

use std::time::{Duration as StdDuration, Instant};

use eframe::egui::{self, vec2, Align, Color32, CornerRadius, CursorIcon, Key, Layout, Margin, RichText, Sense, Stroke, StrokeKind, Ui};

use super::{PosApp, Tab};
use crate::auth;
use crate::db::{Role, User};
use crate::theme::{self as t, Width};

/// Slightly lifted ink for tiles and keys on the sign-in screen.
const RAISED: Color32 = Color32::from_rgb(0x1C, 0x42, 0x4F);
const RAISED_HOVER: Color32 = Color32::from_rgb(0x24, 0x51, 0x60);
const SOFT_RED: Color32 = Color32::from_rgb(0xF2, 0x9B, 0x90);

#[derive(Default)]
pub(super) struct LoginState {
    selected: Option<i64>,
    pin: String,
    /// (message, is_error)
    message: Option<(String, bool)>,
}

impl LoginState {
    pub(super) fn reset_with_message(&mut self, msg: String) {
        *self = LoginState { message: Some((msg, false)), ..Default::default() };
    }
}

#[derive(Default)]
pub(super) struct SetupForm {
    /// First run: connect to an existing server instead of creating a manager.
    extra_till: bool,
    name: String,
    pin: String,
    pin2: String,
    error: Option<String>,
}

impl PosApp {
    /// Locks the till. An unfinished sale is kept for the same person.
    pub(super) fn lock(&mut self) {
        self.cart_owner = if self.cart.is_empty() { None } else { self.user.as_ref().map(|u| u.id) };
        self.user = None;
        self.net.signed_in(None);
        self.login = LoginState::default();
        self.status = None;
        self.approval = None;
        self.confirm_void = None;
    }

    /// Locks after the configured idle time, but never with a sale in progress.
    pub(super) fn auto_lock(&mut self, ctx: &egui::Context) {
        let active = ctx.input(|i| !i.events.is_empty() || i.pointer.is_moving() || i.pointer.any_down());
        if active {
            self.last_activity = Instant::now();
        }
        let minutes: u64 = self.auto_lock_minutes.trim().parse().unwrap_or(0);
        if minutes == 0 || !self.cart.is_empty() {
            return;
        }
        if self.last_activity.elapsed() >= StdDuration::from_secs(minutes * 60) {
            self.lock();
            self.login.message = Some(("The till locked after being idle. Sign in to carry on.".into(), false));
        } else {
            ctx.request_repaint_after(StdDuration::from_secs(5));
        }
    }

    fn try_sign_in(&mut self) {
        let Some(id) = self.login.selected else { return };
        let name = self.staff.iter().find(|u| u.id == id).map(|u| u.name.clone()).unwrap_or_default();
        if let Some(secs) = self.throttle.locked_for(id) {
            self.login.message = Some((format!("Too many wrong PINs. {name} can try again in {secs} seconds."), true));
            return;
        }
        let pin = std::mem::take(&mut self.login.pin);
        if pin.len() < auth::PIN_MIN {
            self.login.message = Some((format!("A PIN is {} to {} digits.", auth::PIN_MIN, auth::PIN_MAX), true));
            return;
        }
        let Some(db) = &self.db else { return };
        match db.verify_pin(id, &pin) {
            Ok(Some(user)) => {
                self.throttle.success(id);
                self.sign_in(user, pin);
            }
            Ok(None) => {
                let left = self.throttle.fail(id);
                let msg = match left {
                    0 => format!("Too many wrong PINs. {name} can try again in a minute."),
                    1 => "Wrong PIN. 1 try left before a short lock-out.".to_string(),
                    n => format!("Wrong PIN. {n} tries left."),
                };
                self.login.message = Some((msg, true));
            }
            Err(e) => self.login.message = Some((format!("{e:#}"), true)),
        }
    }

    fn sign_in(&mut self, user: User, pin: String) {
        // Managers on a till need their PIN again to open a server session for changes.
        self.net.signed_in(user.is_manager().then_some(pin));
        let mut note = String::new();
        if self.cart_owner.is_some_and(|owner| owner != user.id) {
            note = " The unfinished sale from the last person was cleared.".into();
            self.cart.clear();
            self.tendered.clear();
        }
        self.cart_owner = None;
        if !self.tab.allowed_for(&user) {
            self.tab = Tab::Sell;
        }
        self.ok(format!("Signed in as {}.{note}", user.name));
        self.user = Some(user);
        self.login = LoginState::default();
        self.last_activity = Instant::now();
        self.dirty = true;
    }

    // ------------------------------------------------------------ sign-in screen

    pub(super) fn ui_login(&mut self, ctx: &egui::Context, width: Width) {
        let people: Vec<User> = self.staff.iter().filter(|u| u.active).cloned().collect();
        if people.len() == 1 {
            self.login.selected = Some(people[0].id);
        }

        // Keyboard: digits, Backspace, Enter, Esc.
        if self.login.selected.is_some() {
            let (typed, back, enter, esc) = ctx.input(|i| {
                let typed: String = i
                    .events
                    .iter()
                    .filter_map(|e| if let egui::Event::Text(t) = e { Some(t.clone()) } else { None })
                    .collect();
                (typed, i.key_pressed(Key::Backspace), i.key_pressed(Key::Enter), i.key_pressed(Key::Escape))
            });
            for c in typed.chars().filter(char::is_ascii_digit) {
                if self.login.pin.len() < auth::PIN_MAX {
                    self.login.pin.push(c);
                }
            }
            if back {
                self.login.pin.pop();
            }
            if esc {
                self.login.pin.clear();
            }
            if enter {
                self.try_sign_in();
            }
        }

        let margin = if width == Width::Narrow { 16 } else { 32 };
        egui::CentralPanel::default().frame(egui::Frame::new().fill(t::INK).inner_margin(Margin::same(margin))).show(ctx, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                let total_w = ui.available_width();
                let col_w = total_w.min(940.0);
                let side_by_side = col_w >= 760.0;
                ui.add_space((ui.available_height() * 0.08).min(60.0));
                ui.horizontal(|ui| {
                    ui.add_space((total_w - col_w) / 2.0);
                    ui.vertical(|ui| {
                        ui.set_width(col_w);
                        ui.label(RichText::new(&self.shop_name).font(t::display(34.0)).color(Color32::WHITE));
                        ui.label(RichText::new("Tap your name, then enter your PIN.").color(t::ON_INK_SOFT).size(16.0));
                        ui.add_space(20.0);
                        if side_by_side {
                            ui.horizontal_top(|ui| {
                                let names_w = col_w - 360.0;
                                ui.allocate_ui_with_layout(vec2(names_w, 0.0), Layout::top_down(Align::Min), |ui| {
                                    self.name_tiles(ui, &people);
                                });
                                ui.add_space(40.0);
                                ui.vertical(|ui| self.pin_pad(ui, &people));
                            });
                        } else {
                            self.name_tiles(ui, &people);
                            ui.add_space(20.0);
                            self.pin_pad(ui, &people);
                        }
                    });
                });
            });
        });
    }

    fn name_tiles(&mut self, ui: &mut Ui, people: &[User]) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(10.0, 10.0);
            for u in people {
                let selected = self.login.selected == Some(u.id);
                let (rect, resp) = ui.allocate_exact_size(vec2(176.0, 72.0), Sense::click());
                let fill = if selected {
                    t::LABEL
                } else if resp.hovered() {
                    RAISED_HOVER
                } else {
                    RAISED
                };
                let p = ui.painter();
                p.rect_filled(rect, CornerRadius::ZERO, fill);
                if selected {
                    let bar = egui::Rect::from_min_max(egui::pos2(rect.left(), rect.bottom() - 5.0), rect.max);
                    p.rect_filled(bar, CornerRadius::ZERO, t::GOLD);
                }
                let (fg, sub) = if selected { (t::INK, t::INK_SOFT) } else { (Color32::WHITE, t::ON_INK_SOFT) };
                let name = p.layout(u.name.clone(), t::semibold(17.0), fg, rect.width() - 28.0);
                p.galley(rect.min + vec2(14.0, 12.0), name, fg);
                p.text(rect.min + vec2(14.0, 44.0), egui::Align2::LEFT_TOP, u.role.label(), t::body(13.0), sub);
                if resp.has_focus() {
                    p.rect_stroke(rect.expand(2.0), CornerRadius::ZERO, Stroke::new(2.0_f32, t::GOLD), StrokeKind::Outside);
                }
                if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() && !selected {
                    self.login.selected = Some(u.id);
                    self.login.pin.clear();
                    self.login.message = None;
                }
            }
        });
    }

    fn pin_pad(&mut self, ui: &mut Ui, people: &[User]) {
        let pad_w = 320.0;
        ui.set_width(pad_w);
        let selected = self.login.selected.and_then(|id| people.iter().find(|u| u.id == id));
        let title = match selected {
            Some(u) => format!("PIN for {}", u.name),
            None => "Tap your name first".to_string(),
        };
        ui.label(RichText::new(title).font(t::display(20.0)).color(Color32::WHITE));
        ui.add_space(6.0);

        // PIN slots: filled squares for digits typed so far.
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            for i in 0..auth::PIN_MAX {
                let (r, _) = ui.allocate_exact_size(vec2(46.0, 52.0), Sense::hover());
                let p = ui.painter();
                p.rect_stroke(r, CornerRadius::ZERO, Stroke::new(1.0_f32, t::ON_INK_SOFT), StrokeKind::Inside);
                if i < self.login.pin.len() {
                    p.rect_filled(egui::Rect::from_center_size(r.center(), vec2(14.0, 14.0)), CornerRadius::ZERO, t::GOLD);
                }
            }
        });
        ui.add_space(12.0);

        let enabled = selected.is_some();
        let key_size = vec2((pad_w - 16.0) / 3.0, 58.0);
        let keys = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "Clear", "0", "Back"];
        for row in keys.chunks(3) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                for k in row {
                    let digit = k.len() == 1;
                    // Keys keep their colour before a name is chosen; only the digits dim.
                    let text = RichText::new(*k)
                        .font(if digit { t::display(26.0) } else { t::semibold(15.0) })
                        .color(if enabled { Color32::WHITE } else { t::ON_INK_SOFT });
                    let b = egui::Button::new(text).fill(RAISED).stroke(Stroke::NONE).min_size(key_size);
                    if ui.add(b).clicked() && enabled {
                        match *k {
                            "Clear" => self.login.pin.clear(),
                            "Back" => {
                                self.login.pin.pop();
                            }
                            d if self.login.pin.len() < auth::PIN_MAX => self.login.pin.push_str(d),
                            _ => {}
                        }
                    }
                }
            });
            ui.add_space(0.0);
        }
        ui.add_space(8.0);
        let can = enabled && self.login.pin.len() >= auth::PIN_MIN;
        if ui.add_enabled(can, t::primary("Sign in", 18.0).min_size(vec2(pad_w, 54.0))).clicked() {
            self.try_sign_in();
        }
        if let Some((m, err)) = &self.login.message {
            ui.add_space(6.0);
            ui.add(egui::Label::new(RichText::new(m).color(if *err { SOFT_RED } else { t::ON_INK })).wrap());
        }
    }

    // ------------------------------------------------------------ first run

    pub(super) fn ui_setup(&mut self, ctx: &egui::Context, width: Width) {
        let margin = if width == Width::Narrow { 16 } else { 32 };
        let mut create = false;
        egui::CentralPanel::default().frame(egui::Frame::new().fill(t::INK).inner_margin(Margin::same(margin))).show(ctx, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                let total_w = ui.available_width();
                let col_w = total_w.min(520.0);
                ui.add_space((ui.available_height() * 0.08).min(60.0));
                ui.horizontal(|ui| {
                    ui.add_space((total_w - col_w) / 2.0);
                    ui.vertical(|ui| {
                        ui.set_width(col_w);
                        ui.label(RichText::new(&self.shop_name).font(t::display(34.0)).color(Color32::WHITE));
                        ui.add_space(12.0);
                        t::surface().show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            t::heading(ui, "Set up the till");
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 6.0;
                                if super::note_chip(ui, "Main till or server", !self.setup.extra_till).clicked() {
                                    self.setup.extra_till = false;
                                }
                                if super::note_chip(ui, "Extra till", self.setup.extra_till).clicked() {
                                    self.setup.extra_till = true;
                                }
                            });
                            ui.add_space(4.0);
                            if self.setup.extra_till {
                                self.pair_form(ui, true);
                                return;
                            }
                            t::caption(
                                ui,
                                "Create the manager account first. Managers add cashiers, change prices, void sales and see reports.",
                            );
                            ui.add_space(6.0);
                            let f = &mut self.setup;
                            super::field(ui, "Your name", &mut f.name, "e.g. Tendai");
                            pin_field(ui, "Choose a PIN (4 to 6 digits)", &mut f.pin);
                            let r = pin_field(ui, "Type the PIN again", &mut f.pin2);
                            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                                create = true;
                            }
                            ui.add_space(6.0);
                            if ui.add(t::primary("Create manager account", 16.0).min_size(vec2(ui.available_width(), 48.0))).clicked() {
                                create = true;
                            }
                            if let Some(e) = &f.error {
                                ui.add_space(4.0);
                                ui.add(egui::Label::new(RichText::new(e).color(t::RED)).wrap());
                            }
                        });
                    });
                });
            });
        });

        if create {
            let f = &mut self.setup;
            if f.pin != f.pin2 {
                f.error = Some("The two PINs don't match. Type them again.".into());
                f.pin.clear();
                f.pin2.clear();
                return;
            }
            let Some(db) = &self.db else { return };
            match db.create_user(&f.name, Role::Manager, &f.pin) {
                Ok(id) => {
                    let user = User { id, name: f.name.trim().to_string(), role: Role::Manager, active: true };
                    self.setup = SetupForm::default();
                    self.dirty = true;
                    let pin = std::mem::take(&mut self.setup.pin);
                    self.sign_in(user, pin);
                    self.ok("Manager account created. Add cashiers in the Staff tab.");
                }
                Err(e) => f.error = Some(format!("{e:#}")),
            }
        }
    }
}

/// Masked, digits-only input for PINs.
pub(super) fn pin_field(ui: &mut Ui, label: &str, value: &mut String) -> egui::Response {
    ui.label(RichText::new(label).font(t::semibold(13.5)).color(t::INK_SOFT));
    let r = ui.add(
        egui::TextEdit::singleline(value)
            .password(true)
            .font(t::display(20.0))
            .margin(Margin::symmetric(10, 8))
            .desired_width(f32::INFINITY),
    );
    value.retain(|c| c.is_ascii_digit());
    value.truncate(auth::PIN_MAX);
    ui.add_space(2.0);
    r
}
