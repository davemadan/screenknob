//! The window, styled after macOS System Settings: grouped rounded boxes,
//! switches, a thin slider with a round knob, the system font and accent
//! colour, following light and dark mode.

use crate::engine::{Command, DeviceView, Mode, SharedSnapshot, Snapshot};
use crate::{keys, perm};
use eframe::egui::{
    self, Align, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, Layout, Pos2, Rect, RichText,
    Sense, Stroke, StrokeKind, Theme, ThemePreference, Vec2,
};
use std::sync::mpsc::Sender;
use std::time::Instant;

/// Space left for the window buttons in the transparent title bar.
pub const TITLEBAR: f32 = 30.0;

#[derive(Clone, Copy)]
struct Pal {
    bg: Color32,
    group: Color32,
    border: Color32,
    sep: Color32,
    text: Color32,
    dim: Color32,
    accent: Color32,
    track: Color32,
    switch_off: Color32,
    hover: Color32,
    warn: Color32,
}

impl Pal {
    fn of(ui: &egui::Ui) -> Pal {
        let accent = accent(ui.visuals().dark_mode);
        if ui.visuals().dark_mode {
            Pal {
                bg: Color32::from_rgb(0x1e, 0x1e, 0x1e),
                group: Color32::from_rgb(0x2a, 0x2a, 0x2a),
                border: Color32::from_rgb(0x38, 0x38, 0x38),
                sep: Color32::from_rgb(0x3a, 0x3a, 0x3a),
                text: Color32::from_rgb(0xe8, 0xe8, 0xe8),
                dim: Color32::from_rgb(0x98, 0x98, 0x9d),
                accent,
                track: Color32::from_rgb(0x4a, 0x4a, 0x4c),
                switch_off: Color32::from_rgb(0x48, 0x48, 0x4a),
                hover: Color32::from_rgb(0x33, 0x33, 0x35),
                warn: Color32::from_rgb(0xff, 0x9f, 0x0a),
            }
        } else {
            Pal {
                bg: Color32::from_rgb(0xf2, 0xf2, 0xf2),
                group: Color32::WHITE,
                border: Color32::from_rgb(0xdc, 0xdc, 0xdc),
                sep: Color32::from_rgb(0xe6, 0xe6, 0xe6),
                text: Color32::from_rgb(0x1d, 0x1d, 0x1f),
                dim: Color32::from_rgb(0x86, 0x86, 0x8b),
                accent,
                track: Color32::from_rgb(0xd9, 0xd9, 0xdc),
                switch_off: Color32::from_rgb(0xe3, 0xe3, 0xe6),
                hover: Color32::from_rgb(0xf3, 0xf3, 0xf5),
                warn: Color32::from_rgb(0xc9, 0x6a, 0x00),
            }
        }
    }
}

/// The accent colour picked in System Settings (Appearance), blue by default.
fn accent(dark: bool) -> Color32 {
    static CHOICE: std::sync::OnceLock<Option<i32>> = std::sync::OnceLock::new();
    let choice = *CHOICE.get_or_init(|| {
        std::process::Command::new("/usr/bin/defaults")
            .args(["read", "-g", "AppleAccentColor"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
    });
    let (l, d) = match choice {
        Some(-1) => ((0x8c, 0x8c, 0x8c), (0x98, 0x98, 0x98)),
        Some(0) => ((0xff, 0x3b, 0x30), (0xff, 0x45, 0x3a)),
        Some(1) => ((0xff, 0x95, 0x00), (0xff, 0x9f, 0x0a)),
        Some(2) => ((0xff, 0xcc, 0x00), (0xff, 0xd6, 0x0a)),
        Some(3) => ((0x28, 0xcd, 0x41), (0x32, 0xd7, 0x4b)),
        Some(5) => ((0xaf, 0x52, 0xde), (0xbf, 0x5a, 0xf2)),
        Some(6) => ((0xff, 0x2d, 0x55), (0xff, 0x37, 0x5f)),
        _ => ((0x00, 0x7a, 0xff), (0x0a, 0x84, 0xff)),
    };
    let (r, g, b) = if dark { d } else { l };
    Color32::from_rgb(r, g, b)
}

pub struct Ui {
    snapshot: SharedSnapshot,
    tx: Sender<Command>,
    /// The slider being dragged, so incoming updates don't fight your hand.
    dragging: Option<(String, f32)>,
}

impl Ui {
    pub fn new(snapshot: SharedSnapshot, tx: Sender<Command>) -> Ui {
        Ui { snapshot, tx, dragging: None }
    }

    /// The system font (San Francisco) and macOS-like widgets, light and dark.
    pub fn style(ctx: &egui::Context) {
        let mut fonts = FontDefinitions::default();
        if let Ok(bytes) = std::fs::read("/System/Library/Fonts/SFNS.ttf") {
            fonts.font_data.insert("sf".into(), FontData::from_owned(bytes).into());
            if let Some(f) = fonts.families.get_mut(&FontFamily::Proportional) {
                f.insert(0, "sf".into());
            }
        }
        ctx.set_fonts(fonts);
        ctx.set_theme(ThemePreference::System);
        for theme in [Theme::Dark, Theme::Light] {
            let dark = theme == Theme::Dark;
            let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
            let (btn, btn_hover, stroke, text) = if dark {
                (Color32::from_rgb(0x58, 0x58, 0x5a), Color32::from_rgb(0x63, 0x63, 0x66), Color32::TRANSPARENT, Color32::from_rgb(0xec, 0xec, 0xec))
            } else {
                (Color32::WHITE, Color32::from_rgb(0xf6, 0xf6, 0xf6), Color32::from_rgb(0xcc, 0xcc, 0xcc), Color32::from_rgb(0x1d, 0x1d, 0x1f))
            };
            v.override_text_color = Some(text);
            v.panel_fill = if dark { Color32::from_rgb(0x1e, 0x1e, 0x1e) } else { Color32::from_rgb(0xf2, 0xf2, 0xf2) };
            v.window_fill = v.panel_fill;
            for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active] {
                w.corner_radius = CornerRadius::same(6);
                w.bg_stroke = Stroke::new(0.5, stroke);
                w.fg_stroke.color = text;
            }
            v.widgets.inactive.weak_bg_fill = btn;
            v.widgets.inactive.bg_fill = btn;
            v.widgets.hovered.weak_bg_fill = btn_hover;
            v.widgets.hovered.bg_fill = btn_hover;
            v.widgets.active.weak_bg_fill = btn_hover;
            v.selection.bg_fill = accent(dark);
            ctx.set_visuals_of(theme, v);
        }
        ctx.all_styles_mut(|s| {
            s.spacing.item_spacing = Vec2::new(8.0, 6.0);
            s.spacing.button_padding = Vec2::new(10.0, 3.0);
            s.spacing.interact_size.y = 22.0;
            s.text_styles.insert(egui::TextStyle::Body, egui::FontId::proportional(13.0));
            s.text_styles.insert(egui::TextStyle::Button, egui::FontId::proportional(13.0));
            s.text_styles.insert(egui::TextStyle::Small, egui::FontId::proportional(11.0));
        });
    }

    fn send(&self, c: Command) {
        let _ = self.tx.send(c);
    }

    pub fn draw(&mut self, ui: &mut egui::Ui) {
        let snap: Snapshot = self.snapshot.lock().map(|s| s.clone()).unwrap_or_default();
        let pal = Pal::of(ui);
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(pal.bg).inner_margin(egui::Margin {
                left: 20,
                right: 20,
                top: TITLEBAR as i8,
                bottom: 16,
            }))
            .show(ui, |ui| {
                ui.label(RichText::new("ScreenKnob").size(15.0).strong().color(pal.text));
                ui.add_space(8.0);
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    self.volume_group(ui, &snap, pal);
                    ui.add_space(14.0);
                    self.outputs(ui, &snap, pal);
                    ui.add_space(14.0);
                    self.options(ui, &snap, pal);
                    if !snap.log.is_empty() {
                        ui.add_space(14.0);
                        recent(ui, &snap, pal);
                    }
                });
            });
    }

    fn volume_group(&mut self, ui: &mut egui::Ui, snap: &Snapshot, pal: Pal) {
        let cur = snap.devices.iter().find(|d| d.is_current).cloned();
        heading(ui, "Output volume", pal);
        group(ui, pal, |ui| {
            let Some(d) = cur else {
                row(ui, |ui| {
                    ui.label(RichText::new("No sound output found").color(pal.dim));
                });
                return;
            };
            if d.mode == Mode::Unavailable {
                row(ui, |ui| {
                    ui.label(RichText::new(format!("{} has a fixed volume. Turn on Software volume below.", d.name)).color(pal.dim));
                });
                return;
            }
            let mut v = match &self.dragging {
                Some((uid, v)) if *uid == d.uid => *v,
                _ => d.volume,
            };
            row(ui, |ui| {
                speaker(ui, 0.0, pal.dim);
                let w = ui.available_width() - 30.0;
                let r = mac_slider(ui, &mut v, w, pal, !d.muted);
                if r.dragged() || r.clicked() {
                    self.dragging = Some((d.uid.clone(), v));
                    self.send(Command::SetVolume(d.uid.clone(), v));
                }
                if !r.dragged() && !r.clicked() {
                    self.dragging = None;
                }
                speaker(ui, 1.0, pal.dim);
            });
            separator(ui, pal);
            row(ui, |ui| {
                ui.label(RichText::new("Mute").color(pal.text));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let mut m = d.muted;
                    if mac_switch(ui, &mut m, pal).changed() {
                        self.send(Command::SetMute(d.uid.clone(), m));
                    }
                    ui.label(
                        RichText::new(if d.muted { "Muted".to_string() } else { format!("{:.0}%", v * 100.0) })
                            .color(pal.dim),
                    );
                });
            });
            if d.mode == Mode::Software && snap.level > 0.0005 {
                meter(ui, snap.level, pal);
            }
        });
        footnote(ui, &snap.status, if snap.status_ok { pal.dim } else { pal.warn });
        if snap.needs_audio_permission && ui.button("Open Privacy & Security…").clicked() {
            perm::open_privacy(snap.permission_pane);
        }
        if snap.keys && !snap.keys_trusted && cur_is_software(snap) && !snap.native_active {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Volume keys need Accessibility permission.").size(11.5).color(pal.warn));
                if ui.button("Allow…").clicked() {
                    keys::ask_permission();
                }
                if ui
                    .button("Reset & Ask Again")
                    .on_hover_text("Use this if you already allowed it and the keys still don't work")
                    .clicked()
                {
                    keys::reset_and_ask();
                }
            });
        }
    }

    fn outputs(&self, ui: &mut egui::Ui, snap: &Snapshot, pal: Pal) {
        heading(ui, "Output device", pal);
        group(ui, pal, |ui| {
            table_header(ui, pal);
            separator(ui, pal);
            if snap.devices.is_empty() {
                row(ui, |ui| {
                    ui.label(RichText::new("Looking for outputs…").color(pal.dim));
                });
            }
            ui.add_space(2.0);
            for d in &snap.devices {
                if device_row(ui, d, pal).clicked() && !d.is_current {
                    self.send(Command::SelectDevice(d.uid.clone()));
                }
            }
            ui.add_space(4.0);
        });
        if snap.preferred_missing {
            if let Some(n) = &snap.preferred_name {
                footnote(ui, &format!("Waiting for {n} to come back…"), pal.dim);
            }
        }
    }

    fn options(&self, ui: &mut egui::Ui, snap: &Snapshot, pal: Pal) {
        heading(ui, "Options", pal);
        group(ui, pal, |ui| {
            let mut sw = snap.software;
            if option_row(ui, pal, "Software volume", "For screens without their own volume control", &mut sw) {
                self.send(Command::SetSoftware(sw));
            }
            separator(ui, pal);
            let mut mv = snap.monitor_volume;
            if option_row(ui, pal, "Monitor's own volume", "Over the cable when the screen allows it; no recording dot", &mut mv) {
                self.send(Command::SetMonitorVolume(mv));
            }
            separator(ui, pal);
            let mut n = snap.native;
            let native_note = if !snap.native_available {
                "Needs ScreenKnob's sound driver (install below)"
            } else {
                "Native popup, keys and click for screens"
            };
            if option_row(ui, pal, "macOS volume popup", native_note, &mut n) {
                self.send(Command::SetNative(n));
            }
            if snap.native {
                row(ui, |ui| {
                    if snap.driver_installed {
                        ui.label(RichText::new("ScreenKnob driver installed: nothing is recorded").size(11.5).color(pal.dim));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.add_enabled(!snap.driver_busy, egui::Button::new("Remove…")).clicked() {
                                self.send(Command::Driver(false));
                            }
                        });
                    } else if snap.driver_available {
                        ui.label(RichText::new("No recording dot: install ScreenKnob's own sound driver").size(11.5).color(pal.dim));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let label = if snap.driver_busy { "Installing…" } else { "Install Driver…" };
                            if ui.add_enabled(!snap.driver_busy, egui::Button::new(label)).clicked() {
                                self.send(Command::Driver(true));
                            }
                        });
                    }
                });
            }
            separator(ui, pal);
            let mut k = snap.keys;
            let keys_note = if snap.native_active {
                "Handled by macOS while the volume popup is on"
            } else if !snap.keys {
                "Use the keyboard volume keys for those screens"
            } else if !snap.keys_trusted {
                "Needs Accessibility permission"
            } else if snap.keys_active {
                "The keyboard volume keys control those screens"
            } else {
                "Starting…"
            };
            if option_row(ui, pal, "Volume keys", keys_note, &mut k) {
                self.send(Command::SetKeys(k));
            }
            separator(ui, pal);
            let mut ar = snap.auto_return;
            if option_row(ui, pal, "Return to chosen output", "When a screen sleeps, wakes or changes input", &mut ar) {
                self.send(Command::SetAutoReturn(ar));
            }
            separator(ui, pal);
            use crate::login::Login;
            match snap.login {
                Login::Unavailable => {
                    row(ui, |ui| {
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 1.0;
                            ui.label(RichText::new("Open at login").color(pal.dim));
                            ui.label(
                                RichText::new("Available once ScreenKnob is installed in Applications")
                                    .size(11.0)
                                    .color(pal.dim),
                            );
                        });
                    });
                }
                state => {
                    let mut on = state != Login::Off;
                    let note = if state == Login::NeedsApproval {
                        "Switched off in System Settings → Login Items"
                    } else {
                        "Start ScreenKnob when you log in"
                    };
                    if option_row(ui, pal, "Open at login", note, &mut on) {
                        self.send(Command::SetLogin(on));
                    }
                    if state == Login::NeedsApproval {
                        row(ui, |ui| {
                            if ui.button("Open Login Items…").clicked() {
                                crate::login::open_settings();
                            }
                        });
                    }
                }
            }
        });
        footnote(ui, "Closing this window keeps ScreenKnob running in the menu bar.", pal.dim);
    }
}

fn cur_is_software(snap: &Snapshot) -> bool {
    snap.devices.iter().any(|d| d.is_current && d.mode == Mode::Software)
}

fn heading(ui: &mut egui::Ui, text: &str, pal: Pal) {
    ui.label(RichText::new(text).size(13.0).strong().color(pal.text));
    ui.add_space(2.0);
}

fn footnote(ui: &mut egui::Ui, text: &str, color: Color32) {
    ui.add_space(2.0);
    ui.horizontal_wrapped(|ui| {
        ui.add_space(4.0);
        ui.label(RichText::new(text).size(11.5).color(color));
    });
}

fn group<R>(ui: &mut egui::Ui, pal: Pal, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(pal.group)
        .stroke(Stroke::new(0.5, pal.border))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(egui::Margin::symmetric(0, 2))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            add(ui)
        })
        .inner
}

fn row<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| add(ui)).inner
        })
        .inner
}

fn separator(ui: &mut egui::Ui, pal: Pal) {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 1.0), Sense::hover());
    ui.painter().hline(rect.left() + 12.0..=rect.right() - 12.0, rect.center().y, Stroke::new(0.5, pal.sep));
}

fn option_row(ui: &mut egui::Ui, pal: Pal, title: &str, note: &str, value: &mut bool) -> bool {
    row(ui, |ui| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            ui.label(RichText::new(title).color(pal.text));
            ui.label(RichText::new(note).size(11.0).color(pal.dim));
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| mac_switch(ui, value, pal).changed()).inner
    })
}

/// Column titles, at the same places as the columns in `device_row`.
fn table_header(ui: &mut egui::Ui, pal: Pal) {
    let w = ui.available_width() - 12.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w + 12.0, 24.0), Sense::hover());
    let left = rect.left() + 6.0;
    let small = egui::FontId::proportional(11.0);
    let p = ui.painter();
    p.text(Pos2::new(left + 8.0, rect.center().y), egui::Align2::LEFT_CENTER, "Name", small.clone(), pal.dim);
    p.text(Pos2::new(left + (w + 12.0) * 0.62 - 6.0, rect.center().y), egui::Align2::LEFT_CENTER, "Type", small, pal.dim);
}

fn device_row(ui: &mut egui::Ui, d: &DeviceView, pal: Pal) -> egui::Response {
    let w = ui.available_width() - 12.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w + 12.0, 26.0), Sense::click());
    let inner = Rect::from_min_size(Pos2::new(rect.left() + 6.0, rect.top()), Vec2::new(w, rect.height()));
    let p = ui.painter();
    let (fg, sub) = if d.is_current {
        p.rect_filled(inner, 6.0, pal.accent);
        (Color32::WHITE, Color32::from_white_alpha(210))
    } else {
        if resp.hovered() {
            p.rect_filled(inner, 6.0, pal.hover);
        }
        (pal.text, pal.dim)
    };
    let font = egui::FontId::proportional(13.0);
    let small = egui::FontId::proportional(11.5);
    let name_x = inner.left() + 8.0;
    let type_x = inner.left() + (w + 12.0) * 0.62 - 6.0;
    let clip = Rect::from_min_max(inner.min, Pos2::new(type_x - 8.0, inner.max.y));
    p.with_clip_rect(clip).text(Pos2::new(name_x, inner.center().y), egui::Align2::LEFT_CENTER, &d.name, font.clone(), fg);
    let kind = match d.mode {
        Mode::Software => format!("{} · software", d.kind),
        Mode::Monitor => format!("{} · monitor", d.kind),
        _ => d.kind.to_string(),
    };
    p.text(Pos2::new(type_x, inner.center().y), egui::Align2::LEFT_CENTER, kind, small.clone(), sub);
    if d.is_preferred && !d.is_current {
        p.text(Pos2::new(inner.right() - 8.0, inner.center().y), egui::Align2::RIGHT_CENTER, "chosen", small, sub);
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A macOS switch: grey when off, accent colour when on, white knob.
fn mac_switch(ui: &mut egui::Ui, on: &mut bool, pal: Pal) -> egui::Response {
    let size = Vec2::new(32.0, 19.0);
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool_with_time(resp.id, *on, 0.12);
    let p = ui.painter();
    let track = lerp_color(pal.switch_off, pal.accent, t);
    p.rect_filled(rect, rect.height() / 2.0, track);
    let r = rect.height() / 2.0 - 1.5;
    let x = egui::lerp(rect.left() + r + 1.5..=rect.right() - r - 1.5, t);
    let c = Pos2::new(x, rect.center().y);
    p.circle_filled(c + Vec2::new(0.0, 0.6), r, Color32::from_black_alpha(40));
    p.circle_filled(c, r, Color32::WHITE);
    resp
}

/// A macOS slider: thin track, accent fill up to a round white knob.
fn mac_slider(ui: &mut egui::Ui, v: &mut f32, width: f32, pal: Pal, active: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width.max(80.0), 22.0), Sense::click_and_drag());
    let knob = 9.5;
    let left = rect.left() + knob;
    let right = rect.right() - knob;
    if let Some(pos) = resp.interact_pointer_pos() {
        if resp.dragged() || resp.clicked() || resp.is_pointer_button_down_on() {
            *v = ((pos.x - left) / (right - left)).clamp(0.0, 1.0);
        }
    }
    let p = ui.painter();
    let y = rect.center().y;
    let track = Rect::from_min_max(Pos2::new(left, y - 2.0), Pos2::new(right, y + 2.0));
    p.rect_filled(track, 2.0, pal.track);
    let x = egui::lerp(left..=right, v.clamp(0.0, 1.0));
    let fill = Rect::from_min_max(track.min, Pos2::new(x, track.max.y));
    p.rect_filled(fill, 2.0, if active { pal.accent } else { pal.dim });
    let c = Pos2::new(x, y);
    p.circle_filled(c + Vec2::new(0.0, 0.8), knob, Color32::from_black_alpha(45));
    p.circle_filled(c, knob, Color32::WHITE);
    p.circle_stroke(c, knob, Stroke::new(0.5, Color32::from_black_alpha(30)));
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A small drawn speaker; `loud` adds sound waves.
fn speaker(ui: &mut egui::Ui, loud: f32, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(18.0, 18.0), Sense::hover());
    let p = ui.painter();
    let c = rect.center() + Vec2::new(-3.0, 0.0);
    let body = vec![
        c + Vec2::new(-4.5, -2.2),
        c + Vec2::new(-1.5, -2.2),
        c + Vec2::new(2.0, -5.5),
        c + Vec2::new(2.0, 5.5),
        c + Vec2::new(-1.5, 2.2),
        c + Vec2::new(-4.5, 2.2),
    ];
    p.add(egui::Shape::convex_polygon(body, color, Stroke::NONE));
    let waves = if loud > 0.5 { 2 } else { 0 };
    for i in 0..waves {
        let r = 3.5 + 3.0 * i as f32;
        let pts: Vec<Pos2> = (0..=8)
            .map(|k| {
                let a = -0.8 + 1.6 * k as f32 / 8.0;
                c + Vec2::new(2.5 + r * a.cos(), r * a.sin())
            })
            .collect();
        p.add(egui::Shape::line(pts, Stroke::new(1.3, color)));
    }
}

fn meter(ui: &mut egui::Ui, level: f32, pal: Pal) {
    let w = ui.available_width() - 24.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w + 24.0, 6.0), Sense::hover());
    let bar = Rect::from_min_size(Pos2::new(rect.left() + 12.0, rect.top()), Vec2::new(w, 3.0));
    let p = ui.painter();
    p.rect_filled(bar, 1.5, pal.track);
    let mut fill = bar;
    fill.set_width(w * level.clamp(0.0, 1.0).sqrt());
    p.rect_filled(fill, 1.5, Color32::from_rgb(0x34, 0xc7, 0x59));
    p.rect_stroke(bar, 1.5, Stroke::NONE, StrokeKind::Inside);
}

fn recent(ui: &mut egui::Ui, snap: &Snapshot, pal: Pal) {
    heading(ui, "Recent activity", pal);
    group(ui, pal, |ui| {
        for (i, (t, msg)) in snap.log.iter().enumerate() {
            if i > 0 {
                separator(ui, pal);
            }
            row(ui, |ui| {
                ui.label(RichText::new(msg).size(12.0).color(pal.text));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new(ago(*t)).size(11.0).color(pal.dim));
                });
            });
        }
    });
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

fn ago(t: Instant) -> String {
    let s = t.elapsed().as_secs();
    match s {
        0..=9 => "now".into(),
        10..=59 => format!("{s}s ago"),
        60..=3599 => format!("{}m ago", s / 60),
        _ => format!("{}h ago", s / 3600),
    }
}

/// How long the volume popup stays up after a key press.
pub const HUD_SECS: f32 = 1.6;

/// A volume popup in the style of macOS's own (top right of the screen), for
/// outputs macOS can't show one for: icon, level bar and the output's name.
pub fn hud(ctx: &egui::Context, snap: &Snapshot) {
    let Some((at, level, muted, name)) = snap.hud.clone() else { return };
    let age = at.elapsed().as_secs_f32();
    if age > HUD_SECS {
        return;
    }
    let size = Vec2::new(300.0, 64.0);
    let screen = ctx.input(|i| i.viewport().monitor_size).unwrap_or(Vec2::new(1440.0, 900.0));
    let builder = egui::ViewportBuilder::default()
        .with_title("ScreenKnob volume")
        .with_inner_size(size)
        .with_position(Pos2::new(screen.x - size.x - 14.0, 38.0))
        .with_decorations(false)
        .with_transparent(true)
        .with_has_shadow(false)
        .with_resizable(false)
        .with_always_on_top()
        .with_mouse_passthrough(true)
        .with_active(false)
        .with_taskbar(false);
    ctx.show_viewport_immediate(egui::ViewportId::from_hash_of("screenknob-hud"), builder, |ui, _| {
        let dark = ui.visuals().dark_mode;
        let pal = Pal::of(ui);
        // Fade out over the last quarter second.
        let fade = ((HUD_SECS - age) / 0.25).clamp(0.0, 1.0);
        let rect = ui.max_rect().shrink(2.0);
        let p = ui.painter();
        let bg = if dark { Color32::from_rgba_unmultiplied(38, 38, 40, (235.0 * fade) as u8) } else {
            Color32::from_rgba_unmultiplied(246, 246, 248, (240.0 * fade) as u8)
        };
        p.rect_filled(rect, 18.0, bg);
        p.rect_stroke(rect, 18.0, Stroke::new(0.5, Color32::from_white_alpha((40.0 * fade) as u8)), StrokeKind::Inside);
        let fg = pal.text.gamma_multiply(fade);
        let dim = pal.dim.gamma_multiply(fade);
        p.text(Pos2::new(rect.left() + 16.0, rect.top() + 17.0), egui::Align2::LEFT_CENTER, &name,
            egui::FontId::proportional(12.0), dim);
        // Speaker icon, then the level bar.
        let icon_c = Pos2::new(rect.left() + 24.0, rect.top() + 42.0);
        let body = vec![
            icon_c + Vec2::new(-6.0, -3.0),
            icon_c + Vec2::new(-2.0, -3.0),
            icon_c + Vec2::new(3.0, -7.5),
            icon_c + Vec2::new(3.0, 7.5),
            icon_c + Vec2::new(-2.0, 3.0),
            icon_c + Vec2::new(-6.0, 3.0),
        ];
        p.add(egui::Shape::convex_polygon(body, fg, Stroke::NONE));
        if muted || level <= 0.001 {
            let x = icon_c + Vec2::new(9.0, 0.0);
            p.line_segment([x + Vec2::new(-3.0, -3.0), x + Vec2::new(3.0, 3.0)], Stroke::new(1.6, fg));
            p.line_segment([x + Vec2::new(-3.0, 3.0), x + Vec2::new(3.0, -3.0)], Stroke::new(1.6, fg));
        }
        let bar = Rect::from_min_max(Pos2::new(rect.left() + 46.0, icon_c.y - 3.5), Pos2::new(rect.right() - 18.0, icon_c.y + 3.5));
        p.rect_filled(bar, 3.5, if dark { Color32::from_white_alpha((50.0 * fade) as u8) } else { Color32::from_black_alpha((30.0 * fade) as u8) });
        let shown = if muted { 0.0 } else { level.clamp(0.0, 1.0) };
        if shown > 0.0 {
            let mut fill = bar;
            fill.set_width((bar.width() * shown).max(7.0));
            p.rect_filled(fill, 3.5, fg);
        }
    });
}
