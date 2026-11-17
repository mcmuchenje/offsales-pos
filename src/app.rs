use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::{Duration, Local, NaiveDate};
use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, CornerRadius, CursorIcon, Key, Layout, Margin, Rect, Response, RichText,
    Sense, Stroke, StrokeKind, Ui,
};
use egui_extras::{Column, TableBuilder};

use crate::auth::Throttle;
use crate::db::{
    money, parse_money, CartLine, CashierSummary, DaySummary, Db, Product, ProductSummary, SaleRow, User,
};
use crate::theme::{self as t, Width};
use crate::updater::{self, UpdateState};

mod login;
mod network;
mod staff;

use login::{LoginState, SetupForm};
use network::Net;
use staff::StaffForm;
use crate::net::protocol::{ProductChange, SettingsChange, StockChange};

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Sell,
    Products,
    Sales,
    Reports,
    Staff,
    Settings,
}

const TABS: [(Tab, &str); 6] = [
    (Tab::Sell, "Sell"),
    (Tab::Products, "Products"),
    (Tab::Sales, "Sales"),
    (Tab::Reports, "Reports"),
    (Tab::Staff, "Staff"),
    (Tab::Settings, "Settings"),
];

impl Tab {
    /// Cashiers sell and see their own sales; everything else is for managers.
    fn allowed_for(self, user: &User) -> bool {
        user.is_manager() || matches!(self, Tab::Sell | Tab::Sales)
    }
}

/// Manager approval being collected inline on a sale row (cashier voids).
#[derive(Default)]
struct VoidApproval {
    sale_id: i64,
    pin: String,
}

#[derive(Default)]
struct ProductForm {
    editing: Option<i64>,
    name: String,
    price: String,
    stock: String,
    active: bool,
    every_till: bool,
    /// Ticked tills when not sold at every till.
    tills: Vec<i64>,
}

impl ProductForm {
    fn new() -> Self {
        ProductForm { active: true, every_till: true, ..Default::default() }
    }
}

#[derive(Default)]
struct StockForm {
    product_id: Option<i64>,
    qty: String,
    note: String,
}

pub struct PosApp {
    db: Option<Db>,
    fatal: Option<String>,
    tab: Tab,
    status: Option<(String, bool)>, // (message, is_error)

    products: Vec<Product>,
    all_products: Vec<Product>,
    search: String,
    cart: Vec<CartLine>,
    tendered: String,
    cart_open: bool, // narrow layout: whether the docked cart shows its lines
    scroll_to_forms: bool,

    product_form: ProductForm,
    stock_form: StockForm,

    from: String,
    to: String,
    sales: Vec<SaleRow>,
    day_summary: Vec<DaySummary>,
    product_summary: Vec<ProductSummary>,
    confirm_void: Option<i64>,
    approval: Option<VoidApproval>,
    cashier_summary: Vec<CashierSummary>,

    // People and sign-in
    user: Option<User>,
    cart_owner: Option<i64>, // who the cart belongs to when the till is locked mid-sale
    login: LoginState,
    setup: SetupForm,
    throttle: Throttle,
    staff: Vec<User>,
    /// Paired tills (id, name) products can be assigned to; empty when standalone.
    tills: Vec<(i64, String)>,
    staff_form: StaffForm,
    auto_lock_minutes: String,
    last_activity: std::time::Instant,

    shop_name: String,
    update_repo: String,
    update: updater::Shared,
    dirty: bool,
    db_path: PathBuf,
    net: Net,
}

fn today() -> String {
    Local::now().format("%Y-%m-%d").to_string()
}

fn fmt_day(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

/// "2026-10-25 08:01:00" -> "25 Oct, 08:01"
fn pretty_time(ts: &str) -> String {
    chrono::NaiveDateTime::parse_from_str(ts, "%Y-%m-%d %H:%M:%S")
        .map(|d| d.format("%-d %b, %H:%M").to_string())
        .unwrap_or_else(|_| ts.to_string())
}

fn pretty_day(day: &str) -> String {
    NaiveDate::parse_from_str(day, "%Y-%m-%d").map(|d| d.format("%a %-d %b").to_string()).unwrap_or_else(|_| day.into())
}

impl PosApp {
    pub fn new(cc: &eframe::CreationContext<'_>, db_path: PathBuf, start_tab: Option<&str>) -> Self {
        t::install(&cc.egui_ctx);

        let (db, fatal) = match Db::open(&db_path) {
            Ok(db) => (Some(db), None),
            Err(e) => (None, Some(format!("The database at {} could not be opened.\n\n{e:#}", db_path.display()))),
        };
        let shop_name = db
            .as_ref()
            .and_then(|d| d.setting("shop_name").ok().flatten())
            .unwrap_or_else(|| "Off Sales".into());
        let update_repo = db
            .as_ref()
            .and_then(|d| d.setting("update_repo").ok().flatten())
            .unwrap_or_else(|| updater::default_repo().to_string());

        let mode = db.as_ref().map(|d| d.mode()).unwrap_or_default();
        let mut app = PosApp {
            net: Net::new(&mode),
            db_path: db_path.clone(),
            db,
            fatal,
            tab: TABS
                .iter()
                .find(|(_, name)| start_tab.is_some_and(|s| s.eq_ignore_ascii_case(name)))
                .map(|(tab, _)| *tab)
                .unwrap_or(Tab::Sell),
            status: None,
            products: vec![],
            all_products: vec![],
            search: String::new(),
            cart: vec![],
            tendered: String::new(),
            cart_open: false,
            scroll_to_forms: false,
            product_form: ProductForm::new(),
            tills: vec![],
            stock_form: StockForm::default(),
            from: today(),
            to: today(),
            sales: vec![],
            day_summary: vec![],
            product_summary: vec![],
            confirm_void: None,
            approval: None,
            cashier_summary: vec![],
            user: None,
            cart_owner: None,
            login: LoginState::default(),
            setup: SetupForm::default(),
            throttle: Throttle::default(),
            staff: vec![],
            staff_form: StaffForm::new(),
            auto_lock_minutes: String::new(),
            last_activity: std::time::Instant::now(),
            shop_name,
            update_repo,
            update: Arc::new(Mutex::new(UpdateState::Idle)),
            dirty: true,
        };
        // Quietly check for updates at startup; failure (e.g. offline) is fine.
        if !app.update_repo.trim().is_empty() {
            updater::check(app.update.clone(), app.update_repo.clone(), cc.egui_ctx.clone());
        }
        app.auto_lock_minutes = app
            .db
            .as_ref()
            .and_then(|d| d.setting("auto_lock_minutes").ok().flatten())
            .unwrap_or_else(|| "10".into());
        app.refresh();
        app.start_network(&cc.egui_ctx);
        #[cfg(feature = "dev-tools")]
        if let Ok(name) = std::env::var("POS_DEV_USER") {
            if let Some(u) = app.staff.iter().find(|u| u.name == name && u.active).cloned() {
                app.user = Some(u);
            }
        }
        app
    }

    fn manager(&self) -> bool {
        self.user.as_ref().is_some_and(User::is_manager)
    }
    fn user_id(&self) -> i64 {
        self.user.as_ref().map(|u| u.id).unwrap_or_default()
    }

    fn ok(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), false));
    }
    fn err(&mut self, msg: impl std::fmt::Display) {
        self.status = Some((format!("{msg:#}"), true));
    }

    fn refresh(&mut self) {
        let only_user = match &self.user {
            Some(u) if !u.is_manager() => Some(u.id),
            _ => None,
        };
        let Some(db) = &self.db else { return };
        if self.tab != Tab::Settings {
            if let Some(name) = db.setting("shop_name").ok().flatten() {
                self.shop_name = name;
            }
            if let Some(m) = db.setting("auto_lock_minutes").ok().flatten() {
                self.auto_lock_minutes = m;
            }
            // On a till this comes from the server, so it can change after a sync.
            if let Some(r) = db.setting("update_repo").ok().flatten() {
                self.update_repo = r;
            }
        }
        let res = (|| -> anyhow::Result<_> {
            Ok((
                db.products_for_sale()?,
                db.products(true)?,
                db.sales_between(&self.from, &self.to, only_user)?,
                db.daily_summary(&self.from, &self.to)?,
                db.product_summary(&self.from, &self.to)?,
                db.cashier_summary(&self.from, &self.to)?,
                db.users(true)?,
                db.tills()?,
            ))
        })();
        match res {
            Ok((p, ap, s, ds, ps, cs, staff, tills)) => {
                self.tills = tills;
                self.products = p;
                self.all_products = ap;
                self.sales = s;
                self.day_summary = ds;
                self.product_summary = ps;
                self.cashier_summary = cs;
                self.staff = staff;
            }
            Err(e) => self.err(e),
        }
        self.dirty = false;
    }

    // ------------------------------------------------------------ sale logic

    fn cart_total(&self) -> i64 {
        self.cart.iter().map(|l| l.qty * l.unit_price_cents).sum()
    }
    fn cart_count(&self) -> i64 {
        self.cart.iter().map(|l| l.qty).sum()
    }
    fn tendered_cents(&self) -> Option<i64> {
        if self.tendered.trim().is_empty() {
            Some(self.cart_total())
        } else {
            parse_money(&self.tendered)
        }
    }
    fn can_complete(&self) -> bool {
        !self.cart.is_empty() && self.tendered_cents().is_some_and(|t| t >= self.cart_total())
    }

    fn add_to_cart(&mut self, p: &Product) {
        if p.price_cents == 0 {
            return self.err(format!("{} has no price yet. Set one in Products, then add it to the sale.", p.name));
        }
        if let Some(line) = self.cart.iter_mut().find(|l| l.product_id == p.id) {
            line.qty += 1;
        } else {
            self.cart.push(CartLine { product_id: p.id, name: p.name.clone(), qty: 1, unit_price_cents: p.price_cents });
        }
    }

    fn complete_sale(&mut self) {
        let total = self.cart_total();
        let Some(tendered) = self.tendered_cents() else {
            return self.err("Cash received isn't a valid amount. Enter it like 10 or 7.50.");
        };
        let user_id = self.user_id();
        let Some(db) = self.db.as_mut() else { return };
        match db.record_sale_as(&self.cart, tendered, "cash", user_id, self.net.is_till()) {
            Ok(_) => {
                self.net.sync_now();
                self.cart.clear();
                self.tendered.clear();
                self.cart_open = false;
                let change = tendered - total;
                if change > 0 {
                    self.ok(format!("Sale completed for {}. Give {} change.", money(total), money(change)));
                } else {
                    self.ok(format!("Sale completed for {}.", money(total)));
                }
                self.dirty = true;
            }
            Err(e) => self.err(e),
        }
    }

    // ------------------------------------------------------------ chrome

    fn top_bar(&mut self, ctx: &egui::Context, width: Width) {
        let frame = egui::Frame::new().fill(t::INK).inner_margin(Margin::symmetric(if width == Width::Narrow { 8 } else { 20 }, 0));
        egui::TopBottomPanel::top("top").frame(frame).exact_height(56.0).show_separator_line(false).show(ctx, |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                if width != Width::Narrow {
                    ui.label(RichText::new(&self.shop_name).font(t::display(22.0)).color(Color32::WHITE));
                    ui.add_space(24.0);
                }
                let Some(user) = self.user.clone() else { return };
                for (tab, label) in TABS {
                    if !tab.allowed_for(&user) {
                        continue;
                    }
                    let label = if tab == Tab::Sales && !user.is_manager() { "My sales" } else { label };
                    if nav_tab(ui, label, self.tab == tab, width == Width::Narrow).clicked() && self.tab != tab {
                        self.tab = tab;
                        self.status = None;
                        self.confirm_void = None;
                        self.approval = None;
                        self.dirty = true;
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let lock = egui::Button::new(RichText::new("Lock").color(t::ON_INK))
                        .fill(Color32::TRANSPARENT)
                        .stroke(Stroke::new(1.0_f32, t::ON_INK_SOFT));
                    if ui.add(lock).on_hover_text("Lock the till so the next person signs in").clicked() {
                        self.lock();
                    }
                    ui.add_space(8.0);
                    let who = if width == Width::Narrow {
                        user.name.clone()
                    } else {
                        format!("{} ({})", user.name, user.role.label().to_lowercase())
                    };
                    ui.label(RichText::new(who).color(t::ON_INK));
                    ui.add_space(16.0);
                    self.net_indicator(ui, width == Width::Narrow);
                    ui.add_space(12.0);
                    if width == Width::Wide {
                        ui.label(RichText::new(Local::now().format("%A %-d %B").to_string()).color(t::ON_INK_SOFT));
                        ui.add_space(12.0);
                    }
                    let available = matches!(&*self.update.lock().unwrap(), UpdateState::Available { .. });
                    if available {
                        let b = egui::Button::new(RichText::new("Update ready").color(t::GOLD))
                            .fill(Color32::TRANSPARENT)
                            .stroke(Stroke::new(1.0_f32, t::GOLD));
                        if ui.add(b).on_hover_text("A new version is ready to install").clicked() {
                            self.tab = Tab::Settings;
                        }
                    }
                });
            });
        });
    }

    fn status_bar(&mut self, ctx: &egui::Context) {
        let frame = egui::Frame::new().fill(t::LABEL).inner_margin(Margin::symmetric(20, 8)).stroke(Stroke::new(1.0_f32, t::RULE));
        egui::TopBottomPanel::bottom("status").frame(frame).show_separator_line(false).show(ctx, |ui| {
            ui.horizontal(|ui| {
                match &self.status {
                    Some((m, err)) => {
                        let c = if *err { t::RED } else { t::TILL_GREEN };
                        let (r, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                        ui.painter().rect_filled(r, CornerRadius::ZERO, c);
                        ui.label(RichText::new(m).color(if *err { t::RED } else { t::INK }));
                    }
                    None => {
                        ui.label(RichText::new("Ready").color(t::INK_FAINT));
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if self.status.is_some() && ui.small_button("Dismiss").clicked() {
                        self.status = None;
                    }
                    ui.label(RichText::new(format!("Version {}", updater::CURRENT_VERSION)).color(t::INK_FAINT).size(12.5));
                });
            });
        });
    }

    fn page_frame(width: Width) -> egui::Frame {
        let (x, y) = if width == Width::Narrow { (12, 12) } else { (24, 20) };
        egui::Frame::new().fill(t::SHELF).inner_margin(Margin::symmetric(x, y))
    }

    // ------------------------------------------------------------ sell

    fn ui_sell(&mut self, ctx: &egui::Context, width: Width) {
        let cart_frame = egui::Frame::new().fill(t::LABEL).inner_margin(Margin::same(20));
        match width {
            Width::Narrow => {
                egui::TopBottomPanel::bottom("cart-dock")
                    .frame(cart_frame.inner_margin(Margin::same(12)).stroke(Stroke::new(1.0_f32, t::RULE)))
                    .show_separator_line(false)
                    .show(ctx, |ui| self.cart_docked(ui));
            }
            _ => {
                let w = if width == Width::Wide { 410.0 } else { 340.0 };
                egui::SidePanel::right("cart")
                    .frame(cart_frame.stroke(Stroke::new(1.0_f32, t::RULE)))
                    .exact_width(w)
                    .resizable(false)
                    .show_separator_line(false)
                    .show(ctx, |ui| self.cart_panel(ui));
            }
        }

        egui::CentralPanel::default().frame(Self::page_frame(width)).show(ctx, |ui| self.product_grid(ui, width));

        // Enter completes the sale (unless the search box is using it).
        let enter = ctx.input(|i| i.key_pressed(Key::Enter));
        let search_focused = ctx.memory(|m| m.has_focus(egui::Id::new("search")));
        if enter && !search_focused && self.can_complete() {
            self.complete_sale();
        }
    }

    fn product_grid(&mut self, ui: &mut Ui, width: Width) {
        // Search
        ui.horizontal(|ui| {
            let w = (ui.available_width() - 140.0).clamp(160.0, 420.0);
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .id(egui::Id::new("search"))
                    .hint_text("Search products")
                    .font(t::body(16.0))
                    .margin(Margin::symmetric(12, 9))
                    .desired_width(w),
            );
            // Enter in the search box adds the first match; Esc clears it.
            if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                let q = self.search.to_lowercase();
                if let Some(p) = self.products.iter().find(|p| p.name.to_lowercase().contains(&q)).cloned() {
                    if !q.is_empty() {
                        self.add_to_cart(&p);
                        self.search.clear();
                        resp.request_focus();
                    }
                }
            }
            if resp.has_focus() && ui.input(|i| i.key_pressed(Key::Escape)) {
                self.search.clear();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                t::caption(ui, format!("{} products", self.products.len()));
            });
        });
        ui.add_space(6.0);

        let q = self.search.to_lowercase();
        let items: Vec<Product> =
            self.products.iter().filter(|p| q.is_empty() || p.name.to_lowercase().contains(&q)).cloned().collect();

        if items.is_empty() {
            ui.add_space(24.0);
            if self.products.is_empty() {
                t::caption(ui, "No products yet. Add your first product in the Products tab.");
            } else {
                t::caption(ui, format!("No products match \u{201c}{}\u{201d}. Check the spelling or clear the search.", self.search));
            }
            return;
        }

        let gap = if width == Width::Narrow { 8.0 } else { 12.0 };
        let min_tile = if width == Width::Narrow { 150.0 } else { 184.0 };
        let tile_h = if width == Width::Narrow { 104.0 } else { 120.0 };

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let avail = ui.available_width();
            let cols = (((avail + gap) / (min_tile + gap)).floor() as usize).max(1);
            let tile_w = (avail - gap * (cols as f32 - 1.0)) / cols as f32;
            ui.spacing_mut().item_spacing = vec2(gap, gap);
            for row in items.chunks(cols) {
                ui.horizontal(|ui| {
                    for p in row {
                        let in_cart: i64 = self.cart.iter().filter(|l| l.product_id == p.id).map(|l| l.qty).sum();
                        if product_tile(ui, p, in_cart, vec2(tile_w, tile_h)).clicked() {
                            self.add_to_cart(p);
                        }
                    }
                });
            }
        });
    }

    fn cart_header(&mut self, ui: &mut Ui, title_size: f32) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Current sale").font(t::display(title_size)).color(t::INK));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let n = self.cart_count();
                if n > 0 {
                    let clear = egui::Button::new(RichText::new("Clear sale").color(t::INK_SOFT).size(13.0)).frame(false);
                    if ui.add(clear).on_hover_text("Remove everything from this sale").clicked() {
                        self.cart.clear();
                        self.tendered.clear();
                        self.cart_open = false;
                    }
                    t::caption(ui, if n == 1 { "1 item".into() } else { format!("{n} items") });
                }
            });
        });
    }

    /// Side-panel cart. On tall screens the cash input stays pinned with the
    /// totals; on short screens (e.g. 1366x768 laptops) only the till display
    /// and the Complete button are pinned, and cash input scrolls with the lines.
    fn cart_panel(&mut self, ui: &mut Ui) {
        self.cart_header(ui, 22.0);
        ui.add_space(2.0);
        let tall = ui.available_height() > 640.0;

        egui::TopBottomPanel::bottom("pay")
            .frame(egui::Frame::new().inner_margin(Margin { top: 14, ..Default::default() }))
            .show_separator_line(false)
            .show_inside(ui, |ui| {
                // Divider marks where the scrolling part ends and payment begins.
                let y = ui.max_rect().top() - 7.0;
                ui.painter().hline(ui.max_rect().x_range(), y, Stroke::new(1.0_f32, t::RULE));
                if tall {
                    self.cash_inputs(ui);
                }
                self.settle(ui, !tall);
                if tall {
                    ui.label(RichText::new("Press Enter to complete the sale").color(t::INK_FAINT).size(12.5));
                }
            });

        egui::CentralPanel::default().frame(egui::Frame::new()).show_inside(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                self.cart_lines(ui);
                if !tall && !self.cart.is_empty() {
                    ui.add_space(10.0);
                    self.cash_inputs(ui);
                }
            });
        });
    }

    /// Bottom dock used on narrow windows: totals always visible, lines on demand.
    fn cart_docked(&mut self, ui: &mut Ui) {
        let n = self.cart_count();
        self.cart_header(ui, 18.0);
        if n > 0 {
            let label = if self.cart_open { "Hide items and cash".to_string() } else { format!("Show items and cash ({n})") };
            if ui.button(label).clicked() {
                self.cart_open = !self.cart_open;
            }
            if self.cart_open {
                egui::ScrollArea::vertical().max_height(ui.ctx().screen_rect().height() * 0.4).show(ui, |ui| {
                    self.cart_lines(ui);
                    ui.add_space(8.0);
                    self.cash_inputs(ui);
                });
            }
        }
        self.settle(ui, true);
    }

    fn cart_lines(&mut self, ui: &mut Ui) {
        if self.cart.is_empty() {
            ui.add_space(20.0);
            ui.label(RichText::new("Tap a product to start a sale.").color(t::INK_SOFT));
            return;
        }
        let mut remove = None;
        let count = self.cart.len();
        for (i, line) in self.cart.iter_mut().enumerate() {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(&line.name).font(t::semibold(15.5)).color(t::INK));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new(money(line.qty * line.unit_price_cents)).font(t::display(19.0)).color(t::INK));
                });
            });
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                if stepper(ui, "\u{2212}").on_hover_text("One less").clicked() {
                    line.qty -= 1;
                    if line.qty == 0 {
                        remove = Some(i);
                    }
                }
                ui.add_sized(vec2(30.0, 30.0), egui::Label::new(RichText::new(line.qty.to_string()).font(t::display(18.0))));
                if stepper(ui, "+").on_hover_text("One more").clicked() {
                    line.qty += 1;
                }
                ui.label(RichText::new(format!("at {}", money(line.unit_price_cents))).color(t::INK_FAINT).size(13.0));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let rm = egui::Button::new(RichText::new("Remove").color(t::INK_SOFT).size(13.0)).frame(false);
                    if ui.add(rm).clicked() {
                        remove = Some(i);
                    }
                });
            });
            if i + 1 < count {
                ui.add_space(4.0);
                hairline(ui);
            }
        }
        if let Some(i) = remove {
            self.cart.remove(i);
        }
    }

    /// Cash received field and quick note buttons.
    fn cash_inputs(&mut self, ui: &mut Ui) {
        let total = self.cart_total();
        let has_items = !self.cart.is_empty();
        ui.label(RichText::new("Cash received").font(t::semibold(13.5)).color(t::INK_SOFT));
        ui.add(
            egui::TextEdit::singleline(&mut self.tendered)
                .hint_text(if has_items { format!("Exact: {}", money(total)) } else { "Exact amount".into() })
                .font(t::display(20.0))
                .margin(Margin::symmetric(12, 8))
                .desired_width(f32::INFINITY),
        );
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            for v in [1, 2, 5, 10, 20, 50] {
                let selected = !self.tendered.trim().is_empty() && parse_money(&self.tendered) == Some(v * 100);
                if note_chip(ui, &format!("${v}"), selected).clicked() {
                    self.tendered = v.to_string();
                }
            }
            if note_chip(ui, "Exact", self.tendered.trim().is_empty()).clicked() {
                self.tendered.clear();
            }
        });
        ui.add_space(6.0);
    }

    /// The till display and the Complete sale button.
    fn settle(&mut self, ui: &mut Ui, compact: bool) {
        let total = self.cart_total();
        let has_items = !self.cart.is_empty();
        till_display(ui, total, self.tendered_cents(), has_items, compact);
        ui.add_space(if compact { 8.0 } else { 10.0 });
        let label = if has_items { format!("Complete sale  {}", money(total)) } else { "Complete sale".into() };
        let h = if compact { 48.0 } else { 56.0 };
        let resp = ui
            .add_enabled(self.can_complete(), t::primary(label, 18.0).min_size(vec2(ui.available_width(), h)))
            .on_hover_text("Enter");
        if resp.clicked() {
            self.complete_sale();
        }
    }

    // ------------------------------------------------------------ products

    fn ui_products(&mut self, ui: &mut Ui, width: Width) {
        let side_by_side = ui.available_width() >= 880.0;
        if side_by_side {
            let left_w = ui.available_width() * 0.58;
            let h = ui.available_height();
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(vec2(left_w, h), Layout::top_down(Align::Min), |ui| {
                    t::surface().show(ui, |ui| {
                        ui.set_min_height(h - 40.0);
                        self.product_table(ui, true);
                    });
                });
                ui.add_space(8.0);
                ui.allocate_ui_with_layout(vec2(ui.available_width(), h), Layout::top_down(Align::Min), |ui| {
                    egui::ScrollArea::vertical().id_salt("forms").show(ui, |ui| self.product_forms(ui));
                });
            });
        } else {
            // Stacked: the list first; Edit/Stock scroll down to the forms.
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                t::surface().inner_margin(Margin::same(if width == Width::Narrow { 12 } else { 18 })).show(ui, |ui| {
                    self.product_table(ui, false);
                });
                ui.add_space(4.0);
                self.product_forms(ui);
            });
        }
    }

    fn product_table(&mut self, ui: &mut Ui, scroll: bool) {
        ui.horizontal(|ui| {
            t::heading(ui, "Products");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let hidden = self.all_products.iter().filter(|p| !p.active).count();
                if hidden > 0 {
                    t::caption(ui, format!("{hidden} hidden from the till"));
                }
            });
        });
        ui.add_space(4.0);
        let mut edit = None;
        let mut stock = None;
        let editing = self.product_form.editing;
        TableBuilder::new(ui)
            .striped(true)
            .vscroll(scroll)
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::remainder().at_least(140.0).clip(true))
            .column(Column::exact(80.0))
            .column(Column::exact(110.0))
            .column(Column::exact(150.0))
            .header(30.0, |mut h| {
                for title in ["Name", "Price", "Stock", ""] {
                    h.col(|ui| {
                        ui.label(RichText::new(title).font(t::semibold(13.0)).color(t::INK_SOFT));
                    });
                }
            })
            .body(|mut body| {
                for p in &self.all_products {
                    body.row(44.0, |mut r| {
                        r.set_selected(editing == Some(p.id));
                        r.col(|ui| {
                            let mut name = RichText::new(&p.name).font(t::semibold(15.0));
                            name = if p.active { name.color(t::INK) } else { name.color(t::INK_FAINT) };
                            if p.active && !p.tills.is_empty() {
                                // Which tills sell it, under the name so it isn't cut off.
                                ui.vertical(|ui| {
                                    ui.spacing_mut().item_spacing.y = 0.0;
                                    ui.label(name);
                                    let names = till_names(&self.tills, &p.tills);
                                    ui.label(RichText::new(names).color(t::INK_SOFT).size(12.5)).on_hover_text("Sold only at these tills");
                                });
                            } else {
                                ui.label(name);
                                if !p.active {
                                    ui.label(RichText::new("hidden").color(t::INK_FAINT).size(12.5));
                                }
                            }
                        });
                        r.col(|ui| {
                            if p.price_cents == 0 {
                                ui.label(RichText::new("Not set").color(t::RED));
                            } else {
                                ui.label(RichText::new(money(p.price_cents)).font(t::display(17.0)).color(t::INK));
                            }
                        });
                        r.col(|ui| {
                            ui.label(RichText::new(t::stock_text(p.stock_qty)).color(t::stock_color(p.stock_qty)));
                        });
                        r.col(|ui| {
                            if ui.button("Edit").clicked() {
                                edit = Some(p.clone());
                            }
                            if ui.button("Stock").on_hover_text("Receive or adjust stock").clicked() {
                                stock = Some(p.id);
                            }
                        });
                    });
                }
            });
        if edit.is_some() || stock.is_some() {
            self.scroll_to_forms = true;
        }
        if let Some(p) = edit {
            self.product_form = ProductForm {
                editing: Some(p.id),
                name: p.name,
                price: format!("{:.2}", p.price_cents as f64 / 100.0),
                stock: String::new(),
                active: p.active,
                every_till: p.tills.is_empty(),
                tills: p.tills,
            };
        }
        if let Some(id) = stock {
            self.stock_form = StockForm { product_id: Some(id), ..Default::default() };
        }
    }

    fn product_forms(&mut self, ui: &mut Ui) {
        if std::mem::take(&mut self.scroll_to_forms) {
            ui.scroll_to_cursor(Some(Align::Min));
        }
        // ---- add / edit product
        t::surface().show(ui, |ui| {
            ui.set_width(ui.available_width());
            let f = &mut self.product_form;
            let editing = f.editing.is_some();
            t::heading(ui, if editing { "Edit product" } else { "Add product" });
            ui.add_space(4.0);
            field(ui, "Name", &mut f.name, "e.g. Castle 750ml");
            field(ui, "Price ($)", &mut f.price, "e.g. 2.50");
            if editing {
                ui.checkbox(&mut f.active, "Show on the till");
            } else {
                field(ui, "Opening stock", &mut f.stock, "How many you have now");
            }
            if !self.tills.is_empty() {
                sold_at(ui, &self.tills, &mut f.every_till, &mut f.tills);
            }
            ui.add_space(6.0);
            let mut save = false;
            let mut cancel = false;
            ui.horizontal(|ui| {
                save = ui.add(t::primary(if editing { "Save changes" } else { "Add product" }, 15.0)).clicked();
                if editing {
                    cancel = ui.button("Cancel").clicked();
                }
            });
            if cancel {
                self.product_form = ProductForm::new();
            }
            if save {
                let f = &self.product_form;
                let price = parse_money(&f.price);
                let opening = if f.stock.trim().is_empty() { Some(0) } else { f.stock.trim().parse::<i64>().ok() };
                let res = match (price, opening) {
                    (None, _) => Err(anyhow::anyhow!("Price isn't a valid amount. Enter it like 2 or 2.50.")),
                    (_, None) => Err(anyhow::anyhow!("Opening stock must be a whole number, like 24.")),
                    (Some(price), Some(stock)) => {
                        // Ignore tills that have since been removed.
                        let tills: Vec<i64> = if f.every_till {
                            vec![]
                        } else {
                            f.tills.iter().copied().filter(|t| self.tills.iter().any(|(id, _)| id == t)).collect()
                        };
                        if !f.every_till && tills.is_empty() {
                            self.err("Tick the tills that sell this product, or choose Every till.");
                            return;
                        }
                        let c = ProductChange {
                            id: f.editing,
                            name: f.name.clone(),
                            price_cents: price,
                            opening_stock: stock,
                            active: f.active,
                            tills,
                        };
                        self.admin(|db, uid| db.save_product(&c, uid), |r, s| r.change_product(s, &c))
                    }
                };
                match res {
                    Ok(()) => {
                        let name = self.product_form.name.trim().to_string();
                        self.ok(if editing { format!("Saved changes to {name}.") } else { format!("Added {name}.") });
                        self.product_form = ProductForm::new();
                        self.dirty = true;
                    }
                    Err(e) => self.err(e),
                }
            }
        });

        ui.add_space(4.0);

        // ---- receive / adjust stock
        t::surface().show(ui, |ui| {
            ui.set_width(ui.available_width());
            t::heading(ui, "Receive stock");
            t::caption(ui, "Record a delivery. Use a negative number to write off breakages or correct a count.");
            ui.add_space(4.0);
            let sf = &mut self.stock_form;
            ui.label(RichText::new("Product").font(t::semibold(13.5)).color(t::INK_SOFT));
            let selected = sf
                .product_id
                .and_then(|id| self.all_products.iter().find(|p| p.id == id))
                .map(|p| format!("{}  ({})", p.name, t::stock_text(p.stock_qty).to_lowercase()))
                .unwrap_or_else(|| "Choose a product".into());
            egui::ComboBox::from_id_salt("stockprod")
                .selected_text(selected)
                .width(ui.available_width())
                .height(360.0)
                .show_ui(ui, |ui| {
                    for p in &self.all_products {
                        ui.selectable_value(&mut sf.product_id, Some(p.id), &p.name);
                    }
                });
            ui.add_space(4.0);
            field(ui, "Quantity", &mut sf.qty, "e.g. 24, or -2");
            field(ui, "Note (optional)", &mut sf.note, "Supplier, breakage, recount\u{2026}");
            ui.add_space(6.0);
            if ui.add(t::primary("Update stock", 15.0)).clicked() {
                let sf = &self.stock_form;
                let res = match (sf.product_id, sf.qty.trim().parse::<i64>()) {
                    (None, _) => Err(anyhow::anyhow!("Choose which product the stock is for.")),
                    (_, Err(_)) => Err(anyhow::anyhow!("Quantity must be a whole number, like 24 or -2.")),
                    (Some(id), Ok(q)) => {
                        let c = StockChange { product_id: id, qty: q, note: sf.note.trim().to_string() };
                        self.admin(
                            |db, uid| db.adjust_stock(c.product_id, c.qty, if c.qty > 0 { "restock" } else { "adjust" }, &c.note, uid),
                            |r, s| r.change_stock(s, &c),
                        )
                    }
                };
                match res {
                    Ok(()) => {
                        let name = self
                            .stock_form
                            .product_id
                            .and_then(|id| self.all_products.iter().find(|p| p.id == id))
                            .map(|p| p.name.clone())
                            .unwrap_or_default();
                        self.ok(format!("Stock updated for {name}."));
                        self.stock_form.qty.clear();
                        self.stock_form.note.clear();
                        self.dirty = true;
                    }
                    Err(e) => self.err(e),
                }
            }
        });
    }

    // ------------------------------------------------------------ sales

    fn date_range_bar(&mut self, ui: &mut Ui) {
        let now = Local::now().date_naive();
        let ranges: [(&str, String, String); 5] = [
            ("Today", fmt_day(now), fmt_day(now)),
            ("Yesterday", fmt_day(now - Duration::days(1)), fmt_day(now - Duration::days(1))),
            ("Last 7 days", fmt_day(now - Duration::days(6)), fmt_day(now)),
            ("This month", now.format("%Y-%m-01").to_string(), fmt_day(now)),
            ("All time", "2000-01-01".into(), "2999-12-31".into()),
        ];
        let mut changed = false;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 8.0);
            for (label, from, to) in &ranges {
                let selected = &self.from == from && &self.to == to;
                if note_chip(ui, label, selected).clicked() {
                    self.from = from.clone();
                    self.to = to.clone();
                    changed = true;
                }
            }
            ui.add_space(12.0);
            ui.label(RichText::new("From").color(t::INK_SOFT));
            changed |= ui.add(egui::TextEdit::singleline(&mut self.from).desired_width(104.0).margin(Margin::symmetric(8, 6))).lost_focus();
            ui.label(RichText::new("to").color(t::INK_SOFT));
            changed |= ui.add(egui::TextEdit::singleline(&mut self.to).desired_width(104.0).margin(Margin::symmetric(8, 6))).lost_focus();
        });
        if changed {
            self.dirty = true;
            self.confirm_void = None;
        }
    }

    fn ui_sales(&mut self, ui: &mut Ui, width: Width) {
        self.date_range_bar(ui);
        ui.add_space(6.0);
        let live: Vec<&SaleRow> = self.sales.iter().filter(|s| !s.voided).collect();
        let taken: i64 = live.iter().map(|s| s.total_cents).sum();
        ui.label(
            RichText::new(match live.len() {
                0 => "No sales in this period".to_string(),
                1 => format!("1 sale, {} taken", money(taken)),
                n => format!("{n} sales, {} taken", money(taken)),
            })
            .font(t::display(24.0))
            .color(t::INK),
        );
        ui.add_space(4.0);

        if self.sales.is_empty() {
            t::caption(ui, "Completed sales appear here. Pick another period above to see earlier sales.");
            return;
        }

        let mut void = None;
        let mut ask: Option<Option<i64>> = None;
        let mut approve: Option<Option<i64>> = None; // Some(None) = cancel
        let mut start_approval = None;
        let is_manager = self.manager();
        let pad = if width == Width::Narrow { 12 } else { 18 };
        t::surface().inner_margin(Margin::symmetric(pad, 6)).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                let n = self.sales.len();
                for (i, s) in self.sales.iter().enumerate() {
                    ui.add_space(8.0);
                    let right_w = if width == Width::Narrow { 170.0 } else { 260.0 };
                    let left_w = (ui.available_width() - right_w).max(120.0);
                    ui.horizontal_top(|ui| {
                        ui.allocate_ui_with_layout(vec2(left_w, 0.0), Layout::top_down(Align::Min), |ui| {
                            let mut items = RichText::new(&s.items).font(t::semibold(15.0));
                            items = if s.voided { items.strikethrough().color(t::INK_FAINT) } else { items.color(t::INK) };
                            ui.add(egui::Label::new(items).wrap());
                            let mut meta = if s.id < 0 {
                                format!("New sale at {}", pretty_time(&s.created_at))
                            } else {
                                format!("Sale {} at {}", s.id, pretty_time(&s.created_at))
                            };
                            if is_manager {
                                meta += &match &s.cashier {
                                    Some(c) => format!(" by {c}"),
                                    None => " (imported)".into(),
                                };
                            }
                            if s.voided {
                                meta += &match &s.voided_by {
                                    Some(v) => format!(", voided by {v}"),
                                    None => ", voided".into(),
                                };
                            }
                            if s.unsent {
                                meta += ". Not on the server yet";
                            }
                            ui.label(RichText::new(meta).color(t::INK_FAINT).size(13.0));
                        });
                        ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                            let approving = self.approval.as_ref().is_some_and(|a| a.sale_id == s.id);
                            if !s.voided && approving {
                                // Cashier void: a manager types their PIN here.
                                if ui.button("Keep").clicked() {
                                    approve = Some(None);
                                }
                                if ui.add(t::danger("Approve void")).clicked() {
                                    approve = Some(Some(s.id));
                                }
                                if let Some(a) = self.approval.as_mut() {
                                    let r = ui.add(
                                        egui::TextEdit::singleline(&mut a.pin)
                                            .password(true)
                                            .hint_text("Manager PIN")
                                            .desired_width(120.0)
                                            .margin(Margin::symmetric(8, 6)),
                                    );
                                    if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                                        approve = Some(Some(s.id));
                                    }
                                }
                            } else if !s.voided && !is_manager {
                                if ui.button("Void").on_hover_text("Needs a manager's PIN").clicked() {
                                    start_approval = Some(s.id);
                                }
                            } else if !s.voided {
                                if self.confirm_void == Some(s.id) {
                                    if ui.button("Keep").clicked() {
                                        ask = Some(None);
                                    }
                                    if ui.add(t::danger("Void sale")).clicked() {
                                        void = Some(s.id);
                                    }
                                } else if ui.button("Void").on_hover_text("Cancel this sale and return the stock").clicked() {
                                    ask = Some(Some(s.id));
                                }
                            }
                            let mut total = RichText::new(money(s.total_cents)).font(t::display(20.0));
                            total = if s.voided { total.strikethrough().color(t::INK_FAINT) } else { total.color(t::INK) };
                            ui.label(total);
                        });
                    });
                    ui.add_space(8.0);
                    if i + 1 < n {
                        hairline(ui);
                    }
                }
            });
        });

        if let Some(a) = ask {
            self.confirm_void = a;
        }
        if let Some(id) = start_approval {
            self.approval = Some(VoidApproval { sale_id: id, pin: String::new() });
        }
        match approve {
            Some(None) => self.approval = None,
            Some(Some(id)) => self.approve_void(id),
            None => {}
        }
        if let Some(id) = void {
            self.confirm_void = None;
            let me = self.user_id();
            if let Some(db) = self.db.as_mut() {
                match db.void_sale_as(id, me, self.net.is_till()) {
                    Ok(()) => {
                        self.net.sync_now();
                        self.ok("Sale voided. Its stock is back on the shelf.")
                    }
                    Err(e) => self.err(e),
                }
            }
            self.dirty = true;
        }
    }

    /// Cashier void: checks the typed PIN belongs to an active manager.
    fn approve_void(&mut self, sale_id: i64) {
        const APPROVALS: i64 = -1; // throttle bucket for manager-approval attempts
        if let Some(secs) = self.throttle.locked_for(APPROVALS) {
            return self.err(format!("Too many wrong PINs. Try again in {secs} seconds."));
        }
        let pin = self.approval.as_ref().map(|a| a.pin.clone()).unwrap_or_default();
        let Some(db) = self.db.as_mut() else { return };
        match db.manager_with_pin(&pin) {
            Ok(Some(manager)) => {
                self.throttle.success(APPROVALS);
                match db.void_sale_as(sale_id, manager.id, self.net.is_till()) {
                    Ok(()) => {
                        self.net.sync_now();
                        self.ok(format!("Sale voided with {}'s approval. Its stock is back on the shelf.", manager.name))
                    }
                    Err(e) => self.err(e),
                }
                self.approval = None;
                self.dirty = true;
            }
            Ok(None) => {
                let left = self.throttle.fail(APPROVALS);
                if let Some(a) = self.approval.as_mut() {
                    a.pin.clear();
                }
                self.err(if left == 0 {
                    "Too many wrong PINs. Try again in a minute.".to_string()
                } else {
                    "That isn't a manager's PIN. Ask a manager to type theirs.".to_string()
                });
            }
            Err(e) => self.err(e),
        }
    }

    // ------------------------------------------------------------ reports

    fn ui_reports(&mut self, ui: &mut Ui, width: Width) {
        self.date_range_bar(ui);
        ui.add_space(6.0);
        let total: i64 = self.day_summary.iter().map(|d| d.total_cents).sum();
        let items: i64 = self.day_summary.iter().map(|d| d.items).sum();
        let sales: i64 = self.day_summary.iter().map(|d| d.sales).sum();

        // The headline is a sentence, set in the display face.
        if sales == 0 {
            ui.label(RichText::new("No sales in this period").font(t::display(28.0)).color(t::INK));
            t::caption(ui, "Pick another period above, or check Stock on hand below.");
        } else {
            ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.label(RichText::new(money(total)).font(t::display(40.0)).color(t::INK));
            ui.label(
                RichText::new(format!(
                    "taken from {sales} sale{} and {items} item{}",
                    if sales == 1 { "" } else { "s" },
                    if items == 1 { "" } else { "s" }
                ))
                .font(t::display(20.0))
                .color(t::INK_SOFT),
            );
            });
        }
        ui.add_space(8.0);

        let cols = match width {
            Width::Wide if ui.available_width() > 1050.0 => 3,
            Width::Narrow => 1,
            _ => 2,
        };
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.columns(cols, |c| {
                let (day_col, prod_col, stock_col, cashier_col) = match cols {
                    3 => (0, 1, 2, 0),
                    2 => (0, 1, 0, 1),
                    _ => (0, 0, 0, 0),
                };
                t::surface().show(&mut c[day_col], |ui| {
                    ui.set_width(ui.available_width());
                    t::heading(ui, "By day");
                    ui.spacing_mut().item_spacing.y = 6.0;
                    if self.day_summary.is_empty() {
                        t::caption(ui, "Nothing sold in this period.");
                    }
                    for d in &self.day_summary {
                        report_row(ui, &pretty_day(&d.day), &format!("{} sales", d.sales), &money(d.total_cents), None);
                    }
                });
                c[day_col].add_space(4.0);

                t::surface().show(&mut c[cashier_col], |ui| {
                    ui.set_width(ui.available_width());
                    t::heading(ui, "By cashier");
                    ui.spacing_mut().item_spacing.y = 6.0;
                    if self.cashier_summary.is_empty() {
                        t::caption(ui, "Nothing sold in this period.");
                    }
                    for c in &self.cashier_summary {
                        let n = if c.sales == 1 { "1 sale".to_string() } else { format!("{} sales", c.sales) };
                        report_row(ui, &c.name, &n, &money(c.total_cents), None);
                    }
                });
                c[cashier_col].add_space(4.0);

                let max = self.product_summary.iter().map(|p| p.total_cents).max().unwrap_or(1).max(1);
                t::surface().show(&mut c[prod_col], |ui| {
                    ui.set_width(ui.available_width());
                    t::heading(ui, "By product");
                    ui.spacing_mut().item_spacing.y = 6.0;
                    if self.product_summary.is_empty() {
                        t::caption(ui, "Nothing sold in this period.");
                    }
                    for p in &self.product_summary {
                        report_row(ui, &p.name, &format!("{} sold", p.qty), &money(p.total_cents), Some(p.total_cents as f32 / max as f32));
                    }
                });
                c[prod_col].add_space(4.0);

                t::surface().show(&mut c[stock_col], |ui| {
                    ui.set_width(ui.available_width());
                    t::heading(ui, "Stock on hand");
                    ui.spacing_mut().item_spacing.y = 6.0;
                    let mut stock: Vec<&Product> = self.products.iter().collect();
                    stock.sort_by_key(|p| p.stock_qty);
                    for p in stock {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&p.name).color(t::INK));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.label(RichText::new(t::stock_text(p.stock_qty)).color(t::stock_color(p.stock_qty)));
                            });
                        });
                        hairline(ui);
                    }
                });
            });
        });
    }

    // ------------------------------------------------------------ settings

    fn ui_settings(&mut self, ui: &mut Ui, width: Width) {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.set_max_width(ui.available_width().min(720.0));

            t::surface().show(ui, |ui| {
                ui.set_width(ui.available_width());
                t::heading(ui, "Shop");
                field(ui, "Shop name", &mut self.shop_name, "Shown at the top of the till");
                field(ui, "Lock the till after this many idle minutes", &mut self.auto_lock_minutes, "0 = never");
                t::caption(ui, "The till never locks while a sale is in progress.");
                if ui.add(t::primary("Save", 15.0)).clicked() {
                    let minutes = self.auto_lock_minutes.trim().parse::<u32>();
                    match minutes {
                        Err(_) => self.err("Idle minutes must be a whole number, like 10. Use 0 to never lock."),
                        Ok(m) => {
                            let c = SettingsChange {
                                values: vec![
                                    ("shop_name".into(), self.shop_name.trim().to_string()),
                                    ("auto_lock_minutes".into(), m.to_string()),
                                ],
                            };
                            let res = self.admin(
                                |db, _| c.values.iter().try_for_each(|(k, v)| db.set_setting(k, v)),
                                |r, s| r.change_settings(s, &c),
                            );
                            match res {
                                Ok(()) => self.ok("Saved shop settings."),
                                Err(e) => self.err(e),
                            }
                        }
                    }
                }
            });
            ui.add_space(4.0);

            self.ui_network_settings(ui, width);
            ui.add_space(4.0);

            t::surface().show(ui, |ui| {
                ui.set_width(ui.available_width());
                t::heading(ui, "Updates");
                t::caption(
                    ui,
                    format!(
                        "You're on version {}. The till works offline; updates are checked only when there's an internet connection.",
                        updater::CURRENT_VERSION
                    ),
                );
                ui.add_space(4.0);
                field(ui, "Update source (GitHub owner/repo)", &mut self.update_repo, "e.g. myname/offsales-pos");
                t::caption(ui, "On a shop network, set this once. The server passes it on to every till when they sync.");

                let state = self.update.lock().unwrap().clone();
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    let busy = matches!(state, UpdateState::Checking | UpdateState::Installing);
                    if ui.add_enabled(!busy, egui::Button::new("Check for updates")).clicked() {
                        let repo = self.update_repo.trim().to_string();
                        let saved = self
                            .db
                            .as_ref()
                            .and_then(|d| d.setting("update_repo").ok().flatten())
                            .unwrap_or_default();
                        if repo != saved {
                            // Saved on the server (directly, or from a till via the server)
                            // so every till picks it up on its next sync.
                            let c = SettingsChange { values: vec![("update_repo".into(), repo.clone())] };
                            match self.admin(|db, _| db.set_setting("update_repo", &repo), |r, s| r.change_settings(s, &c)) {
                                Ok(()) => self.ok("Saved the update source."),
                                Err(e) => self.err(e),
                            }
                        }
                        updater::check(self.update.clone(), self.update_repo.clone(), ui.ctx().clone());
                    }
                    match &state {
                        UpdateState::Idle => {}
                        UpdateState::Checking => {
                            ui.spinner();
                            ui.label("Checking for updates\u{2026}");
                        }
                        UpdateState::UpToDate => {
                            ui.label(RichText::new("You have the latest version.").color(t::TILL_GREEN));
                        }
                        UpdateState::Available { version, .. } => {
                            if ui.add(t::primary(format!("Install version {version}"), 15.0)).clicked() {
                                self.backup_now(true);
                                updater::install(self.update.clone(), self.update_repo.clone(), ui.ctx().clone());
                            }
                        }
                        UpdateState::Installing => {
                            ui.spinner();
                            ui.label("Installing the update\u{2026}");
                        }
                        UpdateState::Installed { version } => {
                            ui.label(RichText::new(format!("Version {version} installed.")).color(t::TILL_GREEN));
                            if ui.add(t::primary("Restart now", 15.0)).clicked() {
                                updater::restart();
                            }
                        }
                        UpdateState::Error(e) => {
                            ui.add(egui::Label::new(RichText::new(e).color(t::RED)).wrap());
                        }
                    }
                });
                if let UpdateState::Available { notes, .. } = &state {
                    if !notes.trim().is_empty() {
                        ui.add_space(6.0);
                        ui.label(RichText::new("What's new").font(t::semibold(14.0)).color(t::INK));
                        ui.label(RichText::new(notes).color(t::INK_SOFT));
                    }
                }
            });
            ui.add_space(4.0);

            t::surface().show(ui, |ui| {
                ui.set_width(ui.available_width());
                t::heading(ui, "Backups");
                t::caption(ui, "Copies go to Documents\\OffSales POS Backups. One is also made before every update.");
                if let Some(db) = &self.db {
                    ui.label(RichText::new(format!("Database: {}", db.path().display())).color(t::INK_FAINT).size(12.5));
                }
                ui.add_space(4.0);
                if ui.button("Back up now").clicked() {
                    self.backup_now(false);
                }
            });
        });
    }

    /// Saves a timestamped copy of the database into Documents\OffSales POS Backups.
    fn backup_now(&mut self, quiet: bool) {
        let Some(db) = &self.db else { return };
        let dir = directories::UserDirs::new()
            .and_then(|u| u.document_dir().map(|d| d.to_path_buf()))
            .unwrap_or_else(|| db.path().parent().unwrap().to_path_buf())
            .join("OffSales POS Backups");
        let file = dir.join(format!("pos-{}.db", Local::now().format("%Y%m%d-%H%M%S")));
        let res = std::fs::create_dir_all(&dir).map_err(anyhow::Error::from).and_then(|_| db.backup_to(&file));
        match res {
            Ok(()) if !quiet => self.ok(format!("Backed up to {}", file.display())),
            Ok(()) => {}
            Err(e) => self.err(format!("Backup failed: {e:#}")),
        }
    }
}

impl eframe::App for PosApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(msg) = &self.fatal {
            egui::CentralPanel::default().frame(Self::page_frame(Width::Wide)).show(ctx, |ui| {
                t::surface().show(ui, |ui| {
                    t::heading(ui, "The till can't start");
                    ui.label(RichText::new(msg).color(t::INK));
                    t::caption(ui, "Close other copies of the app and try again. If it keeps happening, restore a backup from Documents\\OffSales POS Backups.");
                });
            });
            return;
        }
        if self.net.take_changed() {
            self.dirty = true;
        }
        if self.dirty {
            let repo_before = self.update_repo.clone();
            self.refresh();
            // A till that just received an update source from the server checks it straight away.
            let idle = matches!(
                *self.update.lock().unwrap(),
                UpdateState::Idle | UpdateState::UpToDate | UpdateState::Error(_)
            );
            if self.update_repo != repo_before && !self.update_repo.trim().is_empty() && idle {
                updater::check(self.update.clone(), self.update_repo.clone(), ctx.clone());
            }
        }
        let width = t::width_class(ctx.screen_rect().width());

        // First run: no one can sign in yet, so create the manager account.
        let has_users = self.db.as_ref().is_some_and(|d| d.has_users().unwrap_or(false));
        if !has_users {
            if self.net.is_till() {
                return self.ui_waiting(ctx);
            }
            return self.ui_setup(ctx, width);
        }
        let Some(user) = self.user.clone() else {
            return self.ui_login(ctx, width);
        };

        self.auto_lock(ctx);
        if !self.tab.allowed_for(&user) {
            self.tab = Tab::Sell;
        }

        self.top_bar(ctx, width);
        self.status_bar(ctx);

        match self.tab {
            Tab::Sell => self.ui_sell(ctx, width),
            tab => {
                egui::CentralPanel::default().frame(Self::page_frame(width)).show(ctx, |ui| match tab {
                    Tab::Products => self.ui_products(ui, width),
                    Tab::Sales => self.ui_sales(ui, width),
                    Tab::Reports => self.ui_reports(ui, width),
                    Tab::Staff => self.ui_staff(ui, width),
                    Tab::Settings => self.ui_settings(ui, width),
                    Tab::Sell => unreachable!(),
                });
            }
        }
    }
}

// ================================================================ widgets

/// Top-bar tab: light text on ink, gold underline when active.
fn nav_tab(ui: &mut Ui, text: &str, selected: bool, compact: bool) -> Response {
    let font = t::display(if compact { 15.5 } else { 17.0 });
    let pad = if compact { 10.0 } else { 16.0 };
    let probe = ui.painter().layout_no_wrap(text.to_owned(), font.clone(), Color32::WHITE);
    let (rect, resp) = ui.allocate_exact_size(vec2(probe.size().x + pad * 2.0, 56.0), Sense::click());
    let color = if selected || resp.hovered() { t::ON_INK } else { t::ON_INK_SOFT };
    let painter = ui.painter();
    if resp.hovered() && !selected {
        painter.rect_filled(rect.shrink2(vec2(2.0, 10.0)), CornerRadius::ZERO, Color32::from_white_alpha(12));
    }
    let galley = painter.layout_no_wrap(text.to_owned(), font, color);
    painter.galley(rect.center() - galley.size() / 2.0, galley, color);
    if selected {
        let bar = Rect::from_min_max(pos2(rect.left() + pad - 4.0, rect.bottom() - 4.0), pos2(rect.right() - pad + 4.0, rect.bottom()));
        painter.rect_filled(bar, CornerRadius::ZERO, t::GOLD);
    }
    if resp.has_focus() {
        painter.rect_stroke(rect.shrink(6.0), CornerRadius::ZERO, Stroke::new(1.5_f32, t::GOLD), StrokeKind::Inside);
    }
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// A shelf label: name, price and stock. Low/empty stock gets a coloured edge;
/// a dark badge shows how many are already in the current sale.
fn product_tile(ui: &mut Ui, p: &Product, in_cart: i64, size: egui::Vec2) -> Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let left = p.stock_qty - in_cart;
    let painter = ui.painter();
    let radius = CornerRadius::ZERO;

    let fill = if resp.is_pointer_button_down_on() { t::HOVER } else { t::LABEL };
    painter.rect_filled(rect, radius, fill);
    let border = if resp.hovered() { t::INK_FAINT } else { t::RULE };
    painter.rect_stroke(rect, radius, Stroke::new(1.0_f32, border), StrokeKind::Inside);

    if left <= t::LOW_STOCK {
        let edge = Rect::from_min_max(rect.min, pos2(rect.left() + 5.0, rect.bottom()));
        painter.rect_filled(edge, CornerRadius::ZERO, t::stock_color(left));
    }

    let inset = 16.0;
    let badge_room = if in_cart > 0 { 34.0 } else { 0.0 };
    let name_color = if left <= 0 { t::INK_SOFT } else { t::INK };
    let name = painter.layout(p.name.clone(), t::semibold(16.0), name_color, rect.width() - inset * 2.0 - badge_room);
    painter.galley(rect.min + vec2(inset, 12.0), name, name_color);

    if in_cart > 0 {
        let c = pos2(rect.right() - 22.0, rect.top() + 22.0);
        painter.rect_filled(Rect::from_center_size(c, vec2(26.0, 26.0)), CornerRadius::ZERO, t::INK);
        painter.text(c, Align2::CENTER_CENTER, in_cart.to_string(), t::semibold(14.0), t::GOLD);
    }

    let base = rect.bottom() - 14.0;
    if p.price_cents == 0 {
        painter.text(pos2(rect.left() + inset, base), Align2::LEFT_BOTTOM, "No price set", t::semibold(14.0), t::RED);
    } else {
        painter.text(pos2(rect.left() + inset, base + 3.0), Align2::LEFT_BOTTOM, money(p.price_cents), t::display(28.0), t::INK);
    }
    painter.text(pos2(rect.right() - inset, base), Align2::RIGHT_BOTTOM, t::stock_text(left), t::body(13.0), t::stock_color(left));

    if resp.has_focus() {
        painter.rect_stroke(rect.expand(2.0), CornerRadius::ZERO, Stroke::new(2.0_f32, t::INK), StrokeKind::Outside);
    }

    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    if p.price_cents == 0 {
        resp.on_hover_text("Set a price in Products before selling this")
    } else {
        resp
    }
}

/// The customer-facing figures: total in gold on bottle ink.
fn till_display(ui: &mut Ui, total: i64, tendered: Option<i64>, has_items: bool, compact: bool) {
    egui::Frame::new()
        .fill(t::INK)
        .corner_radius(CornerRadius::ZERO)
        .inner_margin(Margin::symmetric(18, if compact { 10 } else { 14 }))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new("Total").font(t::display(18.0)).color(t::ON_INK_SOFT));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let c = if has_items { t::GOLD } else { t::ON_INK_SOFT };
                    ui.label(RichText::new(money(total)).font(t::display(if compact { 32.0 } else { 44.0 })).color(c));
                });
            });
            if has_items {
                ui.horizontal(|ui| {
                    let (label, value, color) = match tendered {
                        Some(t) if t >= total => ("Change", money(t - total), Color32::WHITE),
                        Some(t) => ("Short by", money(total - t), Color32::from_rgb(0xF2, 0x9B, 0x90)),
                        None => ("Change", "Check the amount".into(), Color32::from_rgb(0xF2, 0x9B, 0x90)),
                    };
                    ui.label(RichText::new(label).font(t::display(16.0)).color(t::ON_INK_SOFT));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(value).font(t::display(24.0)).color(color));
                    });
                });
            }
        });
}

/// Square +/- button used in the cart.
fn stepper(ui: &mut Ui, text: &str) -> Response {
    ui.add(
        egui::Button::new(RichText::new(text).font(t::display(18.0)).color(t::INK))
            .min_size(vec2(34.0, 30.0))
            .corner_radius(CornerRadius::ZERO),
    )
}

/// Pill used for cash notes and date ranges. Selected = ink fill.
fn note_chip(ui: &mut Ui, text: &str, selected: bool) -> Response {
    let (fill, fg, stroke) = if selected { (t::INK, Color32::WHITE, t::INK) } else { (t::LABEL, t::INK, t::RULE) };
    ui.add(
        egui::Button::new(RichText::new(text).font(t::semibold(14.0)).color(fg))
            .fill(fill)
            .stroke(Stroke::new(1.0_f32, stroke))
            .corner_radius(CornerRadius::ZERO)
            .min_size(vec2(48.0, 34.0)),
    )
}

/// Label-above-input form field.
fn field(ui: &mut Ui, label: &str, value: &mut String, hint: &str) -> Response {
    ui.label(RichText::new(label).font(t::semibold(13.5)).color(t::INK_SOFT));
    let r = ui.add(
        egui::TextEdit::singleline(value)
            .hint_text(hint)
            .font(t::body(16.0))
            .margin(Margin::symmetric(10, 8))
            .desired_width(f32::INFINITY),
    );
    ui.add_space(2.0);
    r
}

/// "Front counter, Bar" for the given till ids.
fn till_names(all: &[(i64, String)], ids: &[i64]) -> String {
    let names: Vec<&str> = all.iter().filter(|(id, _)| ids.contains(id)).map(|(_, n)| n.as_str()).collect();
    if names.is_empty() {
        "No till".into()
    } else {
        format!("{} only", names.join(", "))
    }
}

/// "Sold at": every till, or only the ticked ones.
fn sold_at(ui: &mut Ui, all: &[(i64, String)], every: &mut bool, chosen: &mut Vec<i64>) {
    ui.add_space(6.0);
    ui.label(RichText::new("Sold at").font(t::semibold(13.5)).color(t::INK_SOFT));
    ui.radio_value(every, true, "Every till");
    ui.radio_value(every, false, "Only these tills");
    if !*every {
        ui.indent("sold_at", |ui| {
            for (id, name) in all {
                let mut on = chosen.contains(id);
                if ui.checkbox(&mut on, name).changed() {
                    if on {
                        chosen.push(*id);
                    } else {
                        chosen.retain(|c| c != id);
                    }
                }
            }
        });
    }
    ui.add_space(2.0);
}

fn hairline(ui: &mut Ui) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(r, CornerRadius::ZERO, t::RULE);
}

/// Report line: label and detail on the left, figure on the right, optional share bar.
fn report_row(ui: &mut Ui, label: &str, detail: &str, figure: &str, share: Option<f32>) {
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).font(t::semibold(14.5)).color(t::INK));
        ui.label(RichText::new(detail).color(t::INK_FAINT).size(13.0));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(figure).font(t::display(18.0)).color(t::INK));
        });
    });
    if let Some(s) = share {
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 4.0), Sense::hover());
        ui.painter().rect_filled(r, CornerRadius::ZERO, t::HOVER);
        let fill = Rect::from_min_size(r.min, vec2((r.width() * s).max(2.0), r.height()));
        ui.painter().rect_filled(fill, CornerRadius::ZERO, t::TILL_GREEN);
    } else {
        hairline(ui);
    }
    ui.add_space(2.0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Role;

    #[test]
    fn cashiers_only_see_sell_and_their_sales() {
        let cashier = User { id: 2, name: "Rudo".into(), role: Role::Cashier, active: true };
        let manager = User { id: 1, name: "Tendai".into(), role: Role::Manager, active: true };
        let allowed: Vec<Tab> = TABS.iter().map(|(t, _)| *t).filter(|t| t.allowed_for(&cashier)).collect();
        assert!(allowed == vec![Tab::Sell, Tab::Sales]);
        assert!(TABS.iter().all(|(t, _)| t.allowed_for(&manager)));
    }
}
