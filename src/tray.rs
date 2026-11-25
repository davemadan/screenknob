//! The menu bar icon, so ScreenKnob keeps running with its window closed:
//! current output and volume, louder / quieter / mute, open the window, quit.

use crate::engine::{Command, Snapshot};
use crate::keys;
use eframe::egui;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

pub static OPEN: AtomicBool = AtomicBool::new(false);
pub static QUIT: AtomicBool = AtomicBool::new(false);

pub struct Tray {
    _icon: TrayIcon,
    status: MenuItem,
    up: MenuItem,
    down: MenuItem,
    mute: MenuItem,
    last: (String, String),
}

/// A speaker drawn in black on transparent: macOS tints it for the menu bar
/// (a "template" image), so it fits light and dark menu bars.
fn icon() -> Option<Icon> {
    const S: usize = 36;
    let mut px = vec![0u8; S * S * 4];
    let mut put = |x: usize, y: usize, a: f32| {
        if x < S && y < S {
            let i = (y * S + x) * 4 + 3;
            px[i] = px[i].max((a.clamp(0.0, 1.0) * 255.0) as u8);
        }
    };
    let cy = 18.0f32;
    for y in 0..S {
        for x in 0..S {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let dy = (fy - cy).abs();
            // Box of the speaker, then its cone widening to the right.
            let in_box = (5.0..=11.0).contains(&fx) && dy <= 4.5;
            let in_cone = (11.0..=18.0).contains(&fx) && dy <= 4.5 + (fx - 11.0) * (6.5 / 7.0);
            if in_box || in_cone {
                put(x, y, 1.0);
            }
            // Two sound waves.
            let r = ((fx - 17.0).powi(2) + (fy - cy).powi(2)).sqrt();
            let ang = (fy - cy).atan2(fx - 17.0).abs();
            if fx > 19.0 && ang < 0.85 {
                for ring in [6.5f32, 11.5] {
                    let d = (r - ring).abs();
                    if d < 1.6 {
                        put(x, y, 1.6 - d);
                    }
                }
            }
        }
    }
    Icon::from_rgba(px, S as u32, S as u32).ok()
}

impl Tray {
    pub fn create(tx: Sender<Command>, ctx: egui::Context) -> Option<Tray> {
        let status = MenuItem::new("ScreenKnob", false, None);
        let up = MenuItem::new("Louder", true, None);
        let down = MenuItem::new("Quieter", true, None);
        let mute = MenuItem::new("Mute", true, None);
        let open = MenuItem::new("Open ScreenKnob…", true, None);
        let quit = MenuItem::new("Quit ScreenKnob", true, None);
        let menu = Menu::new();
        let sep1 = PredefinedMenuItem::separator();
        let sep2 = PredefinedMenuItem::separator();
        for item in [&status as &dyn tray_icon::menu::IsMenuItem, &sep1, &up, &down, &mute, &sep2, &open, &quit] {
            menu.append(item).ok()?;
        }
        let ids = (up.id().clone(), down.id().clone(), mute.id().clone(), open.id().clone(), quit.id().clone());
        MenuEvent::set_event_handler(Some(Arc::new(move |e: MenuEvent| {
            let (up, down, mute, open, quit) = &ids;
            let cmd = if e.id == *up {
                Some(Command::StepVolume(keys::STEP))
            } else if e.id == *down {
                Some(Command::StepVolume(-keys::STEP))
            } else if e.id == *mute {
                Some(Command::ToggleMuteCurrent)
            } else {
                if e.id == *open {
                    OPEN.store(true, Ordering::Relaxed);
                } else if e.id == *quit {
                    QUIT.store(true, Ordering::Relaxed);
                }
                None
            };
            if let Some(c) = cmd {
                let _ = tx.send(c);
            }
            ctx.request_repaint();
        })));
        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(true)
            .with_tooltip("ScreenKnob")
            .with_icon_templated(icon()?)
            .build()
            .ok()?;
        Some(Tray { _icon: icon, status, up, down, mute, last: (String::new(), String::new()) })
    }

    /// Keeps the menu's first line and the mute item in step with the app.
    pub fn update(&mut self, snap: &Snapshot) {
        let cur = snap.devices.iter().find(|d| d.is_current);
        let status = match cur {
            None => "No sound output".to_string(),
            Some(d) if d.muted => format!("{} — muted", d.name),
            Some(d) => format!("{} — {:.0}%", d.name, d.volume * 100.0),
        };
        let mute = if cur.is_some_and(|d| d.muted) { "Unmute" } else { "Mute" }.to_string();
        if self.last.0 != status {
            self.status.set_text(&status);
            let on = cur.is_some();
            self.up.set_enabled(on);
            self.down.set_enabled(on);
            self.mute.set_enabled(on);
        }
        if self.last.1 != mute {
            self.mute.set_text(&mute);
        }
        self.last = (status, mute);
    }
}
