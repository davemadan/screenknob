//! ScreenKnob: volume control for every Mac output, including HDMI and
//! DisplayPort screens that have no volume control of their own.

mod ca;
mod ddc;
mod devices;
mod diag;
mod driver;
mod engine;
mod keys;
mod login;
mod perm;
mod ring;
mod settings;
mod tap;
mod tray;
mod ui;

use eframe::egui;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};

struct App {
    ui: ui::Ui,
    shot: Option<PathBuf>,
    frames: u32,
    shot_requested: bool,
    shot_done: Arc<std::sync::atomic::AtomicBool>,
    snapshot: engine::SharedSnapshot,
    tray: Option<tray::Tray>,
    quitting: bool,
    shown_once: bool,
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        use std::sync::atomic::Ordering;
        if self.shot.is_some() {
            return;
        }
        if !self.shown_once {
            // A menu bar app isn't brought forward by itself; do it once at start.
            self.shown_once = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        if tray::QUIT.swap(false, Ordering::Relaxed) {
            self.quitting = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if tray::OPEN.swap(false, Ordering::Relaxed) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        // Closing the window only hides it while the menu bar icon is there.
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting && self.tray.is_some() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
        if let Ok(snap) = self.snapshot.lock().map(|s| s.clone()) {
            if let Some(t) = self.tray.as_mut() {
                t.update(&snap);
            }
            // Keep drawing while the popup is up, even with the window closed.
            if snap.hud.as_ref().is_some_and(|h| h.0.elapsed().as_secs_f32() <= ui::HUD_SECS + 0.1) {
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaintWhileHidden);
                ctx.request_repaint();
            }
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0] // the volume popup's corners stay see-through
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.ui.draw(ui);
        if self.shot.is_none() {
            if let Ok(snap) = self.snapshot.lock().map(|s| s.clone()) {
                ui::hud(ui.ctx(), &snap);
            }
        }
        let Some(path) = self.shot.clone() else { return };
        let ctx = ui.ctx().clone();
        self.frames += 1;
        ctx.request_repaint();
        if self.shot_done.load(std::sync::atomic::Ordering::Relaxed) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if self.frames >= 120 && !self.shot_requested {
            self.shot_requested = true;
            let done = self.shot_done.clone();
            let repaint = ctx.clone();
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(
                move |img| {
                    let [w, h] = img.size;
                    let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_srgba_unmultiplied()).collect();
                    if let Some(dir) = path.parent() {
                        let _ = std::fs::create_dir_all(dir);
                    }
                    if let Err(e) = image::save_buffer(&path, &bytes, w as u32, h as u32, image::ColorType::Rgba8) {
                        eprintln!("could not save screenshot: {e}");
                    }
                    done.store(true, std::sync::atomic::Ordering::Relaxed);
                    repaint.request_repaint();
                },
            )));
        }
    }
}

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let shot = args
        .iter()
        .position(|a| a == "--screenshot")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from);
    if args.iter().any(|a| a == "--selftest") {
        selftest();
        return Ok(());
    }
    let passive = shot.is_some();
    if let Some(i) = args.iter().position(|a| a == "--ring-selftest") {
        // Plays the TEST driver's shared sound (scripts/make-driver.sh) on the named
        // output for 3 s and reports. Never touches the real driver's sound.
        let name = args.get(i + 1).cloned().unwrap_or_default();
        let Some(d) = devices::list_outputs().into_iter().find(|d| d.name == name) else {
            println!("no output named {name}");
            std::process::exit(1);
        };
        match tap::Pipeline::start_ring_test(&d, 1.0) {
            Ok(p) => {
                let mut lags = Vec::new();
                let mut peak = 0.0f32;
                for _ in 0..30 {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    lags.push(p.shared.layout.load(std::sync::atomic::Ordering::Relaxed));
                    peak = peak.max(f32::from_bits(p.shared.level.load(std::sync::atomic::Ordering::Relaxed)));
                }
                println!(
                    "callbacks {}, callbacks that ran dry {}, loudest {:.3}, distance behind the driver (frames): first {:?} last {:?}",
                    p.shared.callbacks.load(std::sync::atomic::Ordering::Relaxed),
                    p.shared.underruns.load(std::sync::atomic::Ordering::Relaxed),
                    peak,
                    &lags[..3],
                    &lags[lags.len() - 3..]
                );
            }
            Err(e) => println!("failed: {e}"),
        }
        std::process::exit(0);
    }
    if args.iter().any(|a| a == "--ddc-status") {
        for m in ddc::monitors() {
            println!("{}: speaker volume {:?}", m.name, m.volume());
        }
        std::process::exit(0);
    }
    if args.iter().any(|a| a == "--login-status") {
        println!("installed {}: open at login {:?}", login::installed(), login::status());
        std::process::exit(0);
    }
    if !passive {
        // Ask macOS for permissions as this app, not as whoever started it.
        if let Some(code) = perm::run_as_own_app() {
            std::process::exit(code);
        }
    }
    let status_only = args.iter().any(|a| a == "--permission-status");
    if !passive && !status_only && !perm::single_instance() {
        diag::log("already running; asked it to show its window");
        return Ok(());
    }
    if !passive {
        diag::log(format!(
            "started {:?}: system audio recording {:?}, accessibility {}",
            std::env::current_exe().unwrap_or_default(),
            perm::audio_capture(),
            keys::trusted()
        ));
    }
    if args.iter().any(|a| a == "--permission-status") {
        println!("pid {} parent {}: system audio recording {:?}, accessibility (volume keys) {}", std::process::id(),
            std::os::unix::process::parent_id(), perm::audio_capture(), keys::trusted());
        std::process::exit(3);
    }

    if !passive {
        // However the app is stopped, give the sound back to the screen first.
        if let Ok(mut signals) = signal_hook::iterator::Signals::new([
            signal_hook::consts::SIGTERM,
            signal_hook::consts::SIGINT,
            signal_hook::consts::SIGHUP,
        ]) {
            std::thread::spawn(move || {
                if let Some(sig) = signals.forever().next() {
                    diag::log(format!("stopping (signal {sig}); handing the sound back"));
                    engine::restore_output_on_exit();
                    std::process::exit(0);
                }
            });
        }
    }

    let snapshot = Arc::new(Mutex::new(engine::Snapshot::default()));
    let (tx, rx) = mpsc::channel();
    engine::start(snapshot.clone(), passive, tx.clone(), rx);

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("ScreenKnob")
        .with_inner_size([440.0, 800.0])
        .with_min_inner_size([380.0, 420.0])
        .with_fullsize_content_view(true)
        .with_titlebar_shown(false)
        .with_title_shown(false);
    if passive {
        viewport = viewport.with_active(false);
        #[cfg(target_os = "windows")]
        {
            viewport = viewport.with_window_level(egui::WindowLevel::AlwaysOnBottom);
        }
    }
    let mut options = eframe::NativeOptions { viewport, ..Default::default() };
    options.run_and_return = true;
    options.event_loop_builder = Some(Box::new(move |_builder| {
        #[cfg(target_os = "macos")]
        {
            use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
            if passive {
                _builder.with_activate_ignoring_other_apps(false);
            } else {
                // A menu bar app: no Dock icon, keeps running with its window closed.
                _builder.with_activation_policy(ActivationPolicy::Accessory);
            }
        }
    }));

    let result = eframe::run_native(
        "ScreenKnob",
        options,
        Box::new(move |cc| {
            ui::Ui::style(&cc.egui_ctx);
            let ctx = cc.egui_ctx.clone();
            let _ = engine::REPAINT.set(Box::new(move || ctx.request_repaint()));
            let tray = if passive { None } else { tray::Tray::create(tx.clone(), cc.egui_ctx.clone()) };
            if !passive && tray.is_none() {
                diag::log("menu bar icon couldn't be created; closing the window will quit");
            }
            Ok(Box::new(App {
                ui: ui::Ui::new(snapshot.clone(), tx),
                shot,
                frames: 0,
                shot_requested: false,
                shot_done: Default::default(),
                snapshot,
                tray,
                quitting: false,
                shown_once: false,
            }))
        }),
    );
    // Quit from the menu bar: give the sound back to the screen.
    engine::restore_output_on_exit();
    result
}

/// Checks the software-volume path without touching what you hear: it opens it
/// briefly on a screen that is *not* the current output, then closes it.
fn selftest() {
    keys::CATCH_KEYS.store(true, std::sync::atomic::Ordering::Relaxed);
    println!("volume key read back as {:?} (expect Some((0, true)))", keys::selftest_key());
    keys::CATCH_KEYS.store(false, std::sync::atomic::Ordering::Relaxed);
    let current = devices::default_output();
    let outs = devices::list_outputs();
    for d in &outs {
        println!("{:<40} {:<12} hw_volume={} channels={} rate={}", d.name, d.kind_label(), d.hw_volume, d.channels, d.sample_rate);
    }
    let wanted = std::env::args().skip_while(|a| a != "--selftest").nth(1);
    let Some(d) = outs.iter().find(|d| match &wanted {
        Some(n) => d.name == *n && Some(d.id) != current,
        None => !d.hw_volume && Some(d.id) != current,
    }) else {
        println!("no idle output without volume control to test on");
        return;
    };
    println!("testing software volume on {}", d.name);

    match tap::Pipeline::start(d, engine::gain_for(0.5, false)) {
        Ok(p) => {
            p.shared.set_gain(engine::gain_for(0.25, false));
            std::thread::sleep(std::time::Duration::from_millis(4000));
            let calls = p.shared.callbacks.load(std::sync::atomic::Ordering::Relaxed);
            println!("audio callbacks in 4 s: {calls}");
            let l = p.shared.layout.load(std::sync::atomic::Ordering::Relaxed);
            println!(
                "input: {} buffers / {} channels, output: {} buffers / {} channels, loudest captured sample {:.4}",
                l >> 48, (l >> 32) & 0xffff, (l >> 16) & 0xffff, l & 0xffff,
                f32::from_bits(p.shared.in_peak.load(std::sync::atomic::Ordering::Relaxed))
            );
            drop(p);
            println!("closed cleanly");
        }
        Err(e) => println!("failed: {e}"),
    }
}
