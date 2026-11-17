//! Staff tab (managers only): add people, set roles and PINs, stop someone signing in.

use eframe::egui::{self, vec2, Align, Layout, Margin, RichText, Ui};

use super::login::pin_field;
use super::{field, hairline, note_chip, PosApp};
use crate::db::Role;
use crate::net::protocol::UserChange;
use crate::theme::{self as t, Width};

pub(super) struct StaffForm {
    editing: Option<i64>,
    name: String,
    role: Role,
    pin: String,
    pin2: String,
    active: bool,
}

impl StaffForm {
    pub(super) fn new() -> Self {
        StaffForm { editing: None, name: String::new(), role: Role::Cashier, pin: String::new(), pin2: String::new(), active: true }
    }
}

impl PosApp {
    pub(super) fn ui_staff(&mut self, ui: &mut Ui, width: Width) {
        if ui.available_width() >= 880.0 {
            let left_w = ui.available_width() * 0.55;
            let h = ui.available_height();
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(vec2(left_w, h), Layout::top_down(Align::Min), |ui| {
                    egui::ScrollArea::vertical().id_salt("staff-list").show(ui, |ui| self.staff_list(ui, width));
                });
                ui.add_space(8.0);
                ui.allocate_ui_with_layout(vec2(ui.available_width(), h), Layout::top_down(Align::Min), |ui| {
                    egui::ScrollArea::vertical().id_salt("staff-form").show(ui, |ui| self.staff_form(ui));
                });
            });
        } else {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                self.staff_list(ui, width);
                ui.add_space(4.0);
                if std::mem::take(&mut self.scroll_to_forms) {
                    ui.scroll_to_cursor(Some(Align::Min));
                }
                self.staff_form(ui);
            });
        }
    }

    fn staff_list(&mut self, ui: &mut Ui, width: Width) {
        let me = self.user_id();
        let mut edit = None;
        t::surface().inner_margin(Margin::same(if width == Width::Narrow { 12 } else { 18 })).show(ui, |ui| {
            ui.set_width(ui.available_width());
            t::heading(ui, "Staff");
            t::caption(ui, "Everyone who can sign in to the till. Each person has their own PIN.");
            ui.add_space(4.0);
            let n = self.staff.len();
            for (i, u) in self.staff.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        let mut name = RichText::new(if u.id == me { format!("{} (you)", u.name) } else { u.name.clone() })
                            .font(t::semibold(16.0));
                        name = if u.active { name.color(t::INK) } else { name.color(t::INK_FAINT) };
                        ui.label(name);
                        let detail = if u.active { u.role.label().to_string() } else { format!("{}, can't sign in", u.role.label()) };
                        ui.label(RichText::new(detail).color(t::INK_FAINT).size(13.0));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Edit").clicked() {
                            edit = Some(u.clone());
                        }
                    });
                });
                if i + 1 < n {
                    hairline(ui);
                }
            }
        });
        if let Some(u) = edit {
            self.staff_form = StaffForm {
                editing: Some(u.id),
                name: u.name,
                role: u.role,
                pin: String::new(),
                pin2: String::new(),
                active: u.active,
            };
            self.scroll_to_forms = true;
        }
    }

    fn staff_form(&mut self, ui: &mut Ui) {
        let mut save = false;
        let mut cancel = false;
        t::surface().show(ui, |ui| {
            ui.set_width(ui.available_width());
            let f = &mut self.staff_form;
            let editing = f.editing.is_some();
            t::heading(ui, if editing { "Edit person" } else { "Add person" });
            ui.add_space(4.0);
            field(ui, "Name", &mut f.name, "e.g. Rudo");

            ui.label(RichText::new("Role").font(t::semibold(13.5)).color(t::INK_SOFT));
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                if note_chip(ui, "Cashier", f.role == Role::Cashier).clicked() {
                    f.role = Role::Cashier;
                }
                if note_chip(ui, "Manager", f.role == Role::Manager).clicked() {
                    f.role = Role::Manager;
                }
            });
            t::caption(
                ui,
                match f.role {
                    Role::Cashier => "Sells and sees their own sales. Voiding a sale needs a manager's PIN.",
                    Role::Manager => "Can do everything: prices, stock, reports, staff, settings and voids.",
                },
            );
            ui.add_space(4.0);

            if editing {
                pin_field(ui, "New PIN (leave blank to keep the current one)", &mut f.pin);
            } else {
                pin_field(ui, "PIN (4 to 6 digits)", &mut f.pin);
            }
            pin_field(ui, "Type the PIN again", &mut f.pin2);
            if editing {
                ui.checkbox(&mut f.active, "Can sign in");
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                save = ui.add(t::primary(if editing { "Save changes" } else { "Add person" }, 15.0)).clicked();
                if editing {
                    cancel = ui.button("Cancel").clicked();
                }
            });
        });

        if cancel {
            self.staff_form = StaffForm::new();
        }
        if save {
            self.save_staff();
        }
    }

    fn save_staff(&mut self) {
        let me = self.user_id();
        let f = &self.staff_form;
        if f.pin != f.pin2 {
            self.staff_form.pin.clear();
            self.staff_form.pin2.clear();
            return self.err("The two PINs don't match. Type them again.");
        }
        if f.editing == Some(me) && !f.active {
            return self.err("You can't stop yourself signing in. Ask another manager to do it.");
        }
        let name = f.name.trim().to_string();
        let c = UserChange {
            id: f.editing,
            name: name.clone(),
            role: f.role.as_str().to_string(),
            pin: (!f.pin.is_empty()).then(|| f.pin.clone()),
            active: f.active,
        };
        let (role, editing) = (f.role, f.editing);
        let res = self.admin(
            |db, _| match c.id {
                None => db.create_user(&c.name, role, c.pin.as_deref().unwrap_or_default()).map(|_| ()),
                Some(id) => db.update_user(id, &c.name, role, c.active).and_then(|_| match &c.pin {
                    Some(pin) => db.set_pin(id, pin),
                    None => Ok(()),
                }),
            },
            |r, s| r.change_user(s, &c),
        );
        let res = res.map(|_| match editing {
            None => format!("Added {name}. They can sign in with their PIN now."),
            Some(_) => format!("Saved changes to {name}."),
        });
        let f = &self.staff_form;
        match res {
            Ok(msg) => {
                // Keep the signed-in user's own name and role current.
                if f.editing == Some(me) {
                    if let Some(u) = self.user.as_mut() {
                        u.name = name.clone();
                        u.role = f.role;
                    }
                }
                self.ok(msg);
                self.staff_form = StaffForm::new();
                self.dirty = true;
            }
            Err(e) => self.err(e),
        }
    }
}
