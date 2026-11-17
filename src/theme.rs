//! Visual identity for the till.
//!
//! Concept: a bottle-store counter. Cool shelf-grey room, white price labels,
//! deep bottle-glass ink for text, and one bold element — the dark till display
//! with gold figures, like the customer-facing display on a cash register.
//! Prices and totals use Bahnschrift (Windows' DIN signage face); everything
//! else uses Segoe UI. Both ship with Windows 10/11; egui defaults are the fallback.

use std::sync::Arc;

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Stroke, TextStyle,
};

// ---------------------------------------------------------------- palette
pub const INK: Color32 = Color32::from_rgb(0x12, 0x30, 0x3A); // bottle glass: text, top bar, till display
pub const INK_SOFT: Color32 = Color32::from_rgb(0x4A, 0x62, 0x6A); // secondary text
pub const INK_FAINT: Color32 = Color32::from_rgb(0x84, 0x96, 0x9B); // hints, captions
pub const SHELF: Color32 = Color32::from_rgb(0xEC, 0xF0, 0xEF); // app background
pub const LABEL: Color32 = Color32::WHITE; // surfaces
pub const RULE: Color32 = Color32::from_rgb(0xD4, 0xDD, 0xDB); // borders and dividers
pub const HOVER: Color32 = Color32::from_rgb(0xF4, 0xF7, 0xF6);
pub const TILL_GREEN: Color32 = Color32::from_rgb(0x1E, 0x85, 0x58); // take-the-money actions, positive
pub const TILL_GREEN_DARK: Color32 = Color32::from_rgb(0x16, 0x6C, 0x47);
pub const GOLD: Color32 = Color32::from_rgb(0xE2, 0xA7, 0x2E); // only on the till display and the active tab
pub const AMBER: Color32 = Color32::from_rgb(0x9A, 0x62, 0x00); // low stock, readable on white
pub const RED: Color32 = Color32::from_rgb(0xB3, 0x32, 0x2A); // out of stock, errors
pub const ON_INK: Color32 = Color32::from_rgb(0xE6, 0xEE, 0xEC); // text on the ink surfaces
pub const ON_INK_SOFT: Color32 = Color32::from_rgb(0x8F, 0xA9, 0xAE);

pub const LOW_STOCK: i64 = 5;

// ---------------------------------------------------------------- type
pub const DISPLAY: &str = "display";
pub const SEMIBOLD: &str = "semibold";

/// Prices, totals, headings.
pub fn display(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(DISPLAY.into()))
}
/// Emphasis in body text (names in the cart, table headers).
pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
}
pub fn body(size: f32) -> FontId {
    FontId::proportional(size)
}

// ---------------------------------------------------------------- layout
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Width {
    /// < 820 logical px: one column, cart docks to the bottom.
    Narrow,
    /// 820–1180: side cart, compact.
    Medium,
    /// > 1180
    Wide,
}

pub fn width_class(w: f32) -> Width {
    if w < 820.0 {
        Width::Narrow
    } else if w < 1180.0 {
        Width::Medium
    } else {
        Width::Wide
    }
}

// ---------------------------------------------------------------- install
fn load_windows_font(file: &str) -> Option<Vec<u8>> {
    let dir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
    std::fs::read(std::path::Path::new(&dir).join("Fonts").join(file)).ok()
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let defaults = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();

    let mut add = |key: &str, file: &str| -> Option<String> {
        let bytes = load_windows_font(file)?;
        fonts.font_data.insert(key.to_owned(), Arc::new(FontData::from_owned(bytes)));
        Some(key.to_owned())
    };
    let segoe = add("segoe", "segoeui.ttf");
    let segoe_sb = add("segoe-semibold", "seguisb.ttf");
    let bahn = add("bahnschrift", "bahnschrift.ttf");

    let chain = |first: &[&Option<String>]| -> Vec<String> {
        let mut v: Vec<String> = first.iter().filter_map(|f| (*f).clone()).collect();
        v.extend(defaults.iter().cloned());
        v
    };
    fonts.families.insert(FontFamily::Proportional, chain(&[&segoe]));
    fonts.families.insert(FontFamily::Name(SEMIBOLD.into()), chain(&[&segoe_sb, &segoe]));
    fonts.families.insert(FontFamily::Name(DISPLAY.into()), chain(&[&bahn, &segoe_sb, &segoe]));
    ctx.set_fonts(fonts);
}

pub fn install(ctx: &egui::Context) {
    install_fonts(ctx);
    ctx.set_theme(egui::Theme::Light);

    ctx.style_mut_of(egui::Theme::Light, |style| {
        style.text_styles = [
            (TextStyle::Small, body(12.5)),
            (TextStyle::Body, body(15.0)),
            (TextStyle::Button, body(15.0)),
            (TextStyle::Heading, display(24.0)),
            (TextStyle::Monospace, FontId::monospace(14.0)),
        ]
        .into();

        let s = &mut style.spacing;
        s.item_spacing = egui::vec2(10.0, 10.0);
        s.button_padding = egui::vec2(14.0, 8.0);
        s.interact_size = egui::vec2(40.0, 36.0); // touch-friendly
        s.combo_width = 260.0;
        s.text_edit_width = 260.0;
        s.window_margin = Margin::same(18);
        s.menu_margin = Margin::same(8);
        s.scroll = egui::style::ScrollStyle::thin();

        let v = &mut style.visuals;
        v.dark_mode = false;
        v.panel_fill = SHELF;
        v.window_fill = LABEL;
        v.window_stroke = Stroke::new(1.0_f32, RULE);
        v.window_corner_radius = CornerRadius::ZERO;
        v.menu_corner_radius = CornerRadius::ZERO;
        v.window_shadow = egui::epaint::Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_black_alpha(28) };
        v.popup_shadow = egui::epaint::Shadow { offset: [0, 4], blur: 12, spread: 0, color: Color32::from_black_alpha(24) };
        v.extreme_bg_color = LABEL; // text inputs
        v.faint_bg_color = HOVER;
        v.code_bg_color = HOVER;
        v.hyperlink_color = TILL_GREEN;
        v.warn_fg_color = AMBER;
        v.error_fg_color = RED;
        v.override_text_color = None;
        v.selection.bg_fill = Color32::from_rgb(0xCF, 0xE6, 0xDB);
        v.selection.stroke = Stroke::new(1.0_f32, INK);
        v.text_cursor.stroke = Stroke::new(2.0_f32, INK);

        let r = CornerRadius::ZERO;
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = LABEL;
        w.noninteractive.weak_bg_fill = LABEL;
        w.noninteractive.bg_stroke = Stroke::new(1.0_f32, RULE);
        w.noninteractive.fg_stroke = Stroke::new(1.0_f32, INK);
        w.noninteractive.corner_radius = r;

        w.inactive.bg_fill = LABEL;
        w.inactive.weak_bg_fill = LABEL;
        w.inactive.bg_stroke = Stroke::new(1.0_f32, RULE);
        w.inactive.fg_stroke = Stroke::new(1.0_f32, INK);
        w.inactive.corner_radius = r;
        w.inactive.expansion = 0.0;

        w.hovered.bg_fill = HOVER;
        w.hovered.weak_bg_fill = HOVER;
        w.hovered.bg_stroke = Stroke::new(1.0_f32, INK_FAINT);
        w.hovered.fg_stroke = Stroke::new(1.0_f32, INK);
        w.hovered.corner_radius = r;
        w.hovered.expansion = 0.0;

        w.active.bg_fill = Color32::from_rgb(0xE2, 0xE9, 0xE7);
        w.active.weak_bg_fill = Color32::from_rgb(0xE2, 0xE9, 0xE7);
        w.active.bg_stroke = Stroke::new(1.5_f32, INK);
        w.active.fg_stroke = Stroke::new(1.5_f32, INK);
        w.active.corner_radius = r;
        w.active.expansion = 0.0;

        w.open = w.active;
    });
}

// ---------------------------------------------------------------- building blocks

/// White surface with a hairline border: forms, tables, the cart.
pub fn surface() -> egui::Frame {
    egui::Frame::new()
        .fill(LABEL)
        .stroke(Stroke::new(1.0_f32, RULE))
        .corner_radius(CornerRadius::ZERO)
        .inner_margin(Margin::same(18))
}

/// Section heading inside a page.
pub fn heading(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).font(display(22.0)).color(INK));
}

/// Short explanatory line under a heading.
pub fn caption(ui: &mut egui::Ui, text: impl Into<String>) {
    ui.label(egui::RichText::new(text.into()).color(INK_SOFT).size(13.5));
}

/// Filled green button for the action that moves money or saves.
pub fn primary(text: impl Into<String>, size: f32) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(text.into()).font(semibold(size)).color(Color32::WHITE))
        .fill(TILL_GREEN)
        .stroke(Stroke::new(1.0_f32, TILL_GREEN_DARK))
        .corner_radius(CornerRadius::ZERO)
}

/// Outlined button with red text for destructive actions.
pub fn danger(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(text.into()).color(RED)).stroke(Stroke::new(1.0_f32, RED))
}

pub fn stock_color(left: i64) -> Color32 {
    if left <= 0 {
        RED
    } else if left <= LOW_STOCK {
        AMBER
    } else {
        INK_SOFT
    }
}

pub fn stock_text(left: i64) -> String {
    match left {
        i64::MIN..=0 => "Out of stock".into(),
        1 => "1 left".into(),
        n => format!("{n} left"),
    }
}
