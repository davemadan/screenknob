//! The one worker thread that owns every audio object.
//!
//! CoreAudio callbacks, the volume keys and the window only send `Command`s
//! here. Device ids are re-read after every change and never kept stale, so a
//! screen going to sleep or switching inputs mid-change can't crash anything:
//! the worst case is one failed call that is retried.

use crate::ca::{self, AudioObjectPropertyAddress as Addr};
use crate::devices::{self, Device};
use crate::keys;
use crate::perm;
use crate::settings::Settings;
use crate::tap::Pipeline;
use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

/// From the original (AudioCoordinator, 0x1000203c8): 10 tries, 500 ms apart.
const RESTORE_ATTEMPTS: u32 = 10;
const RESTORE_INTERVAL: Duration = Duration::from_millis(500);
/// Estimated: one burst of device events is handled once.
const DEBOUNCE: Duration = Duration::from_millis(250);
/// Estimated: wait this long before deciding whether a default-output change was you or macOS.
const CLASSIFY_AFTER: Duration = Duration::from_millis(1200);
/// Estimated: rebuild the software path if its audio stops for this long.
const WATCHDOG: Duration = Duration::from_secs(2);

pub enum Command {
    /// A CoreAudio property changed (its selector).
    Hal(u32),
    SelectDevice(String),
    SetVolume(String, f32),
    SetMute(String, bool),
    StepVolume(f32),
    ToggleMuteCurrent,
    SetAutoReturn(bool),
    SetSoftware(bool),
    SetKeys(bool),
    SetNative(bool),
    SetLogin(bool),
    SetMonitorVolume(bool),
    /// Install (true) or remove (false) ScreenKnob's sound driver.
    Driver(bool),
    DriverDone(bool, Result<(), String>),
    /// Screens found over their cable, with their speaker volume (None: no answer).
    DdcReport(Vec<(String, Option<f32>)>),
    /// A volume key: step (0 for mute), and whether Shift was held.
    Key(f32, bool),
    /// Look again (e.g. after a permission answer).
    Recheck,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Hardware,
    /// The screen's own speaker volume, set over its cable (no sound is captured).
    Monitor,
    Software,
    Unavailable,
}

#[derive(Clone, Debug)]
pub struct DeviceView {
    pub uid: String,
    pub name: String,
    pub kind: &'static str,
    pub is_current: bool,
    pub is_preferred: bool,
    pub mode: Mode,
    pub volume: f32,
    pub muted: bool,
}

#[derive(Clone, Default)]
pub struct Snapshot {
    pub devices: Vec<DeviceView>,
    pub status: String,
    pub status_ok: bool,
    pub preferred_name: Option<String>,
    pub preferred_missing: bool,
    pub auto_return: bool,
    pub software: bool,
    pub keys: bool,
    pub keys_trusted: bool,
    pub keys_active: bool,
    pub needs_audio_permission: bool,
    /// Which Privacy & Security page that permission is on.
    pub permission_pane: &'static str,
    pub native: bool,
    pub native_available: bool,
    pub native_active: bool,
    pub login: crate::login::Login,
    pub monitor_volume: bool,
    pub driver_installed: bool,
    pub driver_available: bool,
    pub driver_busy: bool,
    /// The volume popup to show: when, level, muted, output name.
    pub hud: Option<(Instant, f32, bool, String)>,
    pub level: f32,
    pub log: Vec<(Instant, String)>,
}

pub type SharedSnapshot = Arc<Mutex<Snapshot>>;

static TX: OnceLock<Sender<Command>> = OnceLock::new();
/// While sound goes through the virtual speaker: its id, and the screen to give
/// the sound back to if the app stops.
static PROXY_ID: AtomicU32 = AtomicU32::new(0);
static EXIT_TO: AtomicU32 = AtomicU32::new(0);

/// If the virtual speaker is still the Mac's output, switch back to the screen
/// so sound keeps playing after the app is gone.
pub fn restore_output_on_exit() {
    let proxy = PROXY_ID.load(Ordering::Relaxed);
    let to = EXIT_TO.load(Ordering::Relaxed);
    if proxy != 0 && to != 0 && devices::default_output() == Some(proxy) {
        devices::set_default_output(to);
    }
}
pub static REPAINT: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

extern "C" fn hal_listener(obj: u32, n: u32, addrs: *const Addr, client: *mut std::ffi::c_void) -> i32 {
    std::panic::catch_unwind(|| on_hal(obj, n, addrs, client)).unwrap_or(0)
}

fn on_hal(_obj: u32, n: u32, addrs: *const Addr, _client: *mut std::ffi::c_void) -> i32 {
    if addrs.is_null() {
        return 0;
    }
    let Some(tx) = TX.get() else { return 0 };
    for i in 0..n as usize {
        let sel = unsafe { (*addrs.add(i)).mSelector };
        let _ = tx.send(Command::Hal(sel));
    }
    0
}

fn device_listen_addrs() -> [Addr; 5] {
    [
        Addr::new(ca::PROP_IS_ALIVE, ca::SCOPE_GLOBAL, ca::ELEMENT_MAIN),
        Addr::new(ca::PROP_NOMINAL_RATE, ca::SCOPE_GLOBAL, ca::ELEMENT_MAIN),
        Addr::new(ca::PROP_STREAM_CONFIG, ca::SCOPE_OUTPUT, ca::ELEMENT_MAIN),
        Addr::new(ca::PROP_VOLUME_SCALAR, ca::SCOPE_OUTPUT, ca::ELEMENT_WILDCARD),
        Addr::new(ca::PROP_MUTE, ca::SCOPE_OUTPUT, ca::ELEMENT_WILDCARD),
    ]
}

/// Software volume curve (estimated, MECHANICS.md): amplitude = slider².
pub fn gain_for(volume: f32, muted: bool) -> f32 {
    if muted { 0.0 } else { volume.clamp(0.0, 1.0).powi(2) }
}

/// The sound is captured before the virtual speaker, so its volume and mute are
/// applied here, on the same v² curve as software volume (MECHANICS.md).
fn proxy_gain(proxy: &Device) -> f32 {
    let v = devices::hw_volume(proxy).unwrap_or(1.0);
    gain_for(v, devices::hw_muted(proxy).unwrap_or(false))
}

/// macOS's own volume click, when "Play feedback when volume is changed" is on
/// (Shift flips it, as with a real speaker).
fn play_feedback(shift: bool) {
    let on = std::process::Command::new("/usr/bin/defaults")
        .args(["read", "-g", "com.apple.sound.beep.feedback"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "1")
        .unwrap_or(false);
    if on != shift {
        let _ = std::process::Command::new("/usr/bin/afplay")
            .arg("/System/Library/LoginPlugins/BezelServices.loginPlugin/Contents/Resources/volume.aiff")
            .spawn();
    }
}

struct Restore {
    uid: String,
    attempts_left: u32,
    next_at: Instant,
}

struct Engine {
    passive: bool,
    snapshot: SharedSnapshot,
    settings: Settings,
    devices: Vec<Device>,
    current_id: Option<u32>,
    listened: HashSet<u32>,
    pipeline: Option<Pipeline>,
    pipe_error: Option<String>,
    perm_message: Option<String>,
    build_attempts: u32,
    next_build: Instant,
    dirty_at: Option<Instant>,
    last_devices_change: Option<Instant>,
    pending_default: Option<Instant>,
    restore: Option<Restore>,
    wall: SystemTime,
    mono: Instant,
    cb_count: u64,
    cb_checked: Instant,
    save_at: Option<Instant>,
    log: VecDeque<(Instant, String)>,
    last_publish: Instant,
    show_checked: Instant,
    /// A default-output change this app made itself (not to be read as your choice).
    expect_default: Option<(u32, Instant)>,
    /// Screens that answered over their cable: name → speaker volume (0..1).
    ddc_vol: std::collections::HashMap<String, f32>,
    ddc: Option<Sender<crate::ddc::Req>>,
    ddc_probed: Instant,
    hud: Option<(Instant, f32, bool, String)>,
    driver_busy: bool,
}

pub fn start(snapshot: SharedSnapshot, passive: bool, tx: Sender<Command>, rx: Receiver<Command>) {
    let _ = TX.set(tx.clone());
    if !passive {
        keys::start(tx);
    }
    std::thread::Builder::new()
        .name("audio-engine".into())
        .spawn(move || {
            // If anything inside ever fails, its audio objects are released (Drop)
            // and the engine starts over a moment later instead of stopping.
            let rx = std::sync::Mutex::new(rx);
            loop {
                let snap = snapshot.clone();
                let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let rx = rx.lock().unwrap_or_else(|e| e.into_inner());
                    let mut e = Engine::new(snap, passive);
                    e.run(&rx);
                }));
                if ok.is_ok() {
                    break; // the window closed
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        })
        .ok();
}

impl Engine {
    fn new(snapshot: SharedSnapshot, passive: bool) -> Engine {
        let now = Instant::now();
        Engine {
            passive,
            snapshot,
            settings: Settings::load(),
            devices: Vec::new(),
            current_id: None,
            listened: HashSet::new(),
            pipeline: None,
            pipe_error: None,
            perm_message: None,
            build_attempts: 0,
            next_build: now,
            dirty_at: Some(now),
            last_devices_change: None,
            pending_default: None,
            restore: None,
            wall: SystemTime::now(),
            mono: now,
            cb_count: 0,
            cb_checked: now,
            save_at: None,
            log: VecDeque::new(),
            last_publish: now,
            show_checked: now,
            expect_default: None,
            ddc_vol: std::collections::HashMap::new(),
            ddc: None,
            ddc_probed: now,
            hud: None,
            driver_busy: false,
        }
    }

    fn run(&mut self, rx: &Receiver<Command>) {
        if !self.passive {
            let ddc = crate::ddc::start(|found| {
                if let Some(tx) = TX.get() {
                    let _ = tx.send(Command::DdcReport(found));
                }
            });
            let _ = ddc.send(crate::ddc::Req::Probe);
            self.ddc = Some(ddc);
        }
        let sys = ca::kAudioObjectSystemObject;
        ca::add_listener(sys, Addr::new(ca::PROP_DEVICES, ca::SCOPE_GLOBAL, ca::ELEMENT_MAIN), hal_listener);
        ca::add_listener(sys, Addr::new(ca::PROP_DEFAULT_OUTPUT, ca::SCOPE_GLOBAL, ca::ELEMENT_MAIN), hal_listener);
        self.refresh();
        // Installed copies open at login unless you switch it off.
        if !self.passive && !self.settings.login_set_up && crate::login::installed() {
            self.settings.login_set_up = true;
            if crate::login::set(true).is_ok() {
                self.note("ScreenKnob will open at login");
            }
            self.save_soon();
        }
        if self.settings.preferred_uid.is_none() {
            self.settings.preferred_uid = self.target().map(|d| d.uid.clone());
            self.save_soon();
        }
        loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(cmd) => self.handle(cmd),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            while let Ok(cmd) = rx.try_recv() {
                self.handle(cmd);
            }
            self.tick();
        }
        self.pipeline = None;
        restore_output_on_exit();
        self.settings.save();
    }

    fn note(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        crate::diag::log(&msg);
        self.log.push_front((Instant::now(), msg));
        self.log.truncate(6);
    }

    fn save_soon(&mut self) {
        self.save_at.get_or_insert(Instant::now() + Duration::from_secs(1));
    }

    fn mark_dirty(&mut self) {
        self.dirty_at.get_or_insert(Instant::now() + DEBOUNCE);
    }

    fn current(&self) -> Option<&Device> {
        let id = self.current_id?;
        self.devices.iter().find(|d| d.id == id)
    }

    /// The virtual speaker, when the native popup is on and it's installed.
    fn proxy(&self) -> Option<&Device> {
        if !self.settings.native_popup || !self.settings.software_volume {
            return None;
        }
        // ScreenKnob's own driver (nothing recorded).
        self.devices.iter().find(|d| devices::is_driver(d) && d.alive)
    }

    /// The screen that should play through the virtual speaker: your chosen
    /// output, when it's there and has no volume control of its own.
    fn native_target(&self) -> Option<&Device> {
        self.proxy()?;
        let pref = self.settings.preferred_uid.as_deref()?;
        self.device_by_uid(pref)
            .filter(|d| d.alive && !d.hw_volume && !devices::is_proxy(d) && !self.ddc_ok(d))
    }

    fn default_is_proxy(&self) -> bool {
        self.current().is_some_and(devices::is_proxy) && self.proxy().is_some()
    }

    /// The output whose volume you're controlling: the screen behind the virtual
    /// speaker while it's in use, otherwise the Mac's output.
    fn target(&self) -> Option<&Device> {
        if self.default_is_proxy() {
            if let Some(t) = self.native_target() {
                return Some(t);
            }
        }
        self.current()
    }

    /// While a screen plays through the virtual speaker, its volume lives on the speaker.
    fn volume_device(&self, uid: &str) -> Option<Device> {
        if self.default_is_proxy() && self.native_target().is_some_and(|t| t.uid == uid) {
            return self.proxy().cloned();
        }
        self.device_by_uid(uid).cloned()
    }

    fn set_default(&mut self, d: &Device) -> bool {
        if self.passive {
            return false;
        }
        self.expect_default = Some((d.id, Instant::now()));
        let ok = devices::set_default_output(d.id);
        if ok {
            self.current_id = Some(d.id);
        }
        ok
    }

    /// A real output to use while your screen is away (built-in speakers first).
    fn fallback(&self) -> Option<Device> {
        let real = |d: &&Device| d.alive && !devices::is_proxy(d) && d.transport != ca::TRANSPORT_VIRTUAL;
        self.devices
            .iter()
            .filter(real)
            .find(|d| d.transport == ca::TRANSPORT_BUILTIN)
            .or_else(|| self.devices.iter().filter(real).find(|d| d.hw_volume))
            .or_else(|| self.devices.iter().find(real))
            .cloned()
    }

    fn device_by_uid(&self, uid: &str) -> Option<&Device> {
        self.devices.iter().find(|d| d.uid == uid)
    }

    /// The screen answers over its cable and you want its own volume used.
    fn ddc_ok(&self, d: &Device) -> bool {
        self.settings.monitor_volume && !d.hw_volume && self.ddc_vol.contains_key(&d.name)
    }

    fn mode_of(&self, d: &Device) -> Mode {
        if d.hw_volume {
            Mode::Hardware
        } else if self.ddc_ok(d) {
            Mode::Monitor
        } else if self.settings.software_volume {
            Mode::Software
        } else {
            Mode::Unavailable
        }
    }

    fn handle(&mut self, cmd: Command) {
        match cmd {
            Command::Hal(sel) => match sel {
                ca::PROP_DEVICES => {
                    self.last_devices_change = Some(Instant::now());
                    if let Some(tx) = &self.ddc {
                        let _ = tx.send(crate::ddc::Req::Probe);
                    }
                    self.mark_dirty();
                }
                ca::PROP_DEFAULT_OUTPUT => {
                    self.pending_default = Some(Instant::now());
                    self.mark_dirty();
                }
                ca::PROP_IS_ALIVE | ca::PROP_NOMINAL_RATE | ca::PROP_STREAM_CONFIG => self.mark_dirty(),
                _ => self.publish(),
            },
            Command::SelectDevice(uid) => {
                self.settings.preferred_uid = Some(uid.clone());
                self.restore = None;
                self.pending_default = None;
                if let Some(d) = self.device_by_uid(&uid).cloned() {
                    // A screen without volume control plays through the virtual speaker.
                    let via = match (self.native_target().cloned(), self.proxy().cloned()) {
                        (Some(t), Some(p)) if t.uid == d.uid => p,
                        _ => d.clone(),
                    };
                    if self.set_default(&via) {
                        self.note(format!("Switched to {}", d.name));
                    }
                }
                self.save_soon();
                self.mark_dirty();
            }
            Command::SetVolume(uid, v) => self.set_volume(&uid, v),
            Command::SetMute(uid, m) => self.set_mute(&uid, m),
            Command::StepVolume(step) => {
                if let Some(d) = self.target() {
                    let uid = d.uid.clone();
                    let v = self.volume_of(&uid) + step;
                    self.set_volume(&uid, v);
                    if self.muted_of(&uid) && step > 0.0 {
                        self.set_mute(&uid, false);
                    }
                }
            }
            Command::ToggleMuteCurrent => {
                if let Some(d) = self.target() {
                    let uid = d.uid.clone();
                    let m = !self.muted_of(&uid);
                    self.set_mute(&uid, m);
                }
            }
            Command::SetAutoReturn(b) => {
                self.settings.auto_return = b;
                self.save_soon();
                self.mark_dirty();
            }
            Command::SetSoftware(b) => {
                self.settings.software_volume = b;
                self.build_attempts = 0;
                self.next_build = Instant::now();
                self.save_soon();
                self.mark_dirty();
            }
            Command::Recheck => {
                self.next_build = Instant::now();
                self.mark_dirty();
            }
            Command::SetNative(b) => {
                self.settings.native_popup = b;
                if !b {
                    // Give the sound straight back to the screen.
                    if let (Some(t), true) = (self.native_target_ignoring_setting(), self.current().is_some_and(devices::is_proxy)) {
                        self.set_default(&t);
                    }
                }
                self.pipeline = None;
                self.next_build = Instant::now();
                self.save_soon();
                self.mark_dirty();
            }
            Command::SetLogin(b) => {
                match crate::login::set(b) {
                    Ok(()) => self.note(if b { "ScreenKnob will open at login" } else { "ScreenKnob won't open at login" }),
                    Err(e) => self.note(format!("Couldn't change opening at login: {e}")),
                }
                self.publish();
            }
            Command::Driver(install) => {
                if self.driver_busy || self.passive {
                    return;
                }
                self.driver_busy = true;
                // macOS's password prompt waits for you; don't hold up the sound meanwhile.
                std::thread::spawn(move || {
                    let r = if install { crate::driver::install() } else { crate::driver::remove() };
                    if let Some(tx) = TX.get() {
                        let _ = tx.send(Command::DriverDone(install, r));
                    }
                });
                self.publish();
            }
            Command::DriverDone(install, r) => {
                self.driver_busy = false;
                match r {
                    Ok(()) if install => self.note("ScreenKnob driver installed; the Mac's sound restarted"),
                    Ok(()) => self.note("ScreenKnob driver removed"),
                    Err(e) => self.note(format!("Driver not changed: {e}")),
                }
                self.pipeline = None;
                self.next_build = Instant::now() + RESTORE_INTERVAL;
                self.mark_dirty();
            }
            Command::SetMonitorVolume(b) => {
                self.settings.monitor_volume = b;
                self.save_soon();
                self.mark_dirty();
            }
            Command::DdcReport(found) => {
                let before: HashSet<String> = self.ddc_vol.keys().cloned().collect();
                self.ddc_vol = found.iter().filter_map(|(n, v)| v.map(|v| (n.clone(), v))).collect();
                for n in self.ddc_vol.keys().filter(|n| !before.contains(*n)).cloned().collect::<Vec<_>>() {
                    self.note(format!("{n}: using the monitor's own volume"));
                }
                self.mark_dirty();
            }
            Command::Key(step, shift) => {
                let Some(d) = self.target().cloned() else { return };
                if step == 0.0 {
                    let m = !self.muted_of(&d.uid);
                    self.set_mute(&d.uid, m);
                } else {
                    let v = self.volume_of(&d.uid) + step;
                    self.set_volume(&d.uid, v);
                    if self.muted_of(&d.uid) && step > 0.0 {
                        self.set_mute(&d.uid, false);
                    }
                    play_feedback(shift);
                }
                let (v, m) = (self.volume_of(&d.uid), self.muted_of(&d.uid));
                self.hud = Some((Instant::now(), v, m, d.name.clone()));
                self.publish();
            }
            Command::SetKeys(b) => {
                self.settings.volume_keys = b;
                self.save_soon();
                self.publish();
            }
        }
    }

    fn native_target_ignoring_setting(&self) -> Option<Device> {
        let pref = self.settings.preferred_uid.as_deref()?;
        self.device_by_uid(pref).filter(|d| d.alive && !d.hw_volume && !devices::is_proxy(d)).cloned()
    }

    fn volume_of(&self, uid: &str) -> f32 {
        if let Some(d) = self.device_by_uid(uid).filter(|d| self.ddc_ok(d)) {
            let saved = self.settings.volume_for(uid);
            return if saved.muted { saved.volume } else { self.ddc_vol.get(&d.name).copied().unwrap_or(0.0) };
        }
        match self.volume_device(uid).as_ref() {
            Some(d) if d.hw_volume => devices::hw_volume(d).unwrap_or(0.0),
            _ => self.settings.volume_for(uid).volume,
        }
    }

    fn muted_of(&self, uid: &str) -> bool {
        if self.device_by_uid(uid).is_some_and(|d| self.ddc_ok(d)) {
            return self.settings.volume_for(uid).muted;
        }
        match self.volume_device(uid).as_ref() {
            Some(d) if d.hw_mute => devices::hw_muted(d).unwrap_or(false),
            _ => self.settings.volume_for(uid).muted,
        }
    }

    fn set_volume(&mut self, uid: &str, v: f32) {
        let v = v.clamp(0.0, 1.0);
        if let Some(d) = self.device_by_uid(uid).filter(|d| self.ddc_ok(d)).cloned() {
            // Set on the monitor itself (rounded to its own steps by the worker).
            self.ddc_vol.insert(d.name.clone(), v);
            let e = self.settings.volumes.entry(uid.to_string()).or_default();
            e.volume = v;
            let muted = e.muted;
            if !muted && !self.passive {
                if let Some(tx) = &self.ddc {
                    let _ = tx.send(crate::ddc::Req::Set(d.name.clone(), v));
                }
            }
            self.save_soon();
            self.publish();
            return;
        }
        let dev = self.volume_device(uid);
        match dev {
            Some(d) if d.hw_volume => {
                if !self.passive {
                    devices::set_hw_volume(&d, v);
                }
            }
            _ => {
                let entry = self.settings.volumes.entry(uid.to_string()).or_default();
                entry.volume = v;
                let g = gain_for(entry.volume, entry.muted);
                if let Some(p) = self.pipeline.as_ref().filter(|p| p.device_uid == uid) {
                    p.shared.set_gain(g);
                }
                self.save_soon();
            }
        }
        self.publish();
    }

    fn set_mute(&mut self, uid: &str, m: bool) {
        if let Some(d) = self.device_by_uid(uid).filter(|d| self.ddc_ok(d)).cloned() {
            // Monitors rarely support mute over the cable: go to 0 and back.
            let cur = self.ddc_vol.get(&d.name).copied().unwrap_or(0.0);
            let e = self.settings.volumes.entry(uid.to_string()).or_default();
            if m && !e.muted {
                e.volume = cur;
            }
            e.muted = m;
            let level = if m { 0.0 } else { e.volume };
            if !m {
                self.ddc_vol.insert(d.name.clone(), level);
            }
            if !self.passive {
                if let Some(tx) = &self.ddc {
                    let _ = tx.send(crate::ddc::Req::Set(d.name.clone(), level));
                }
            }
            self.save_soon();
            self.publish();
            return;
        }
        let dev = self.volume_device(uid);
        match dev {
            Some(d) if d.hw_mute => {
                if !self.passive {
                    devices::set_hw_mute(&d, m);
                }
            }
            Some(d) if d.hw_volume => {
                // No mute control of its own: go to 0 and back to the remembered level.
                let entry = self.settings.volumes.entry(uid.to_string()).or_default();
                if m {
                    entry.volume = devices::hw_volume(&d).unwrap_or(entry.volume);
                }
                entry.muted = m;
                let back = entry.volume;
                if !self.passive {
                    devices::set_hw_volume(&d, if m { 0.0 } else { back });
                }
                self.save_soon();
            }
            _ => {
                let entry = self.settings.volumes.entry(uid.to_string()).or_default();
                entry.muted = m;
                let g = gain_for(entry.volume, entry.muted);
                if let Some(p) = self.pipeline.as_ref().filter(|p| p.device_uid == uid) {
                    p.shared.set_gain(g);
                }
                self.save_soon();
            }
        }
        self.publish();
    }

    fn sync_listeners(&mut self) {
        let now_ids: HashSet<u32> = self.devices.iter().map(|d| d.id).collect();
        for id in now_ids.difference(&self.listened.clone()) {
            for a in device_listen_addrs() {
                ca::add_listener(*id, a, hal_listener);
            }
        }
        for id in self.listened.difference(&now_ids) {
            for a in device_listen_addrs() {
                ca::remove_listener(*id, a, hal_listener);
            }
        }
        self.listened = now_ids;
    }

    /// Re-reads every device and decides what to do. Called after changes settle.
    fn refresh(&mut self) {
        let before: HashSet<String> = self.devices.iter().map(|d| d.uid.clone()).collect();
        let names_before: Vec<(String, String)> =
            self.devices.iter().map(|d| (d.uid.clone(), d.name.clone())).collect();
        self.devices = devices::list_outputs();
        self.current_id = devices::default_output();
        self.sync_listeners();

        let after: HashSet<String> = self.devices.iter().map(|d| d.uid.clone()).collect();
        if !before.is_empty() {
            for (uid, name) in names_before.iter().filter(|(u, _)| !after.contains(u)) {
                let _ = uid;
                self.note(format!("{name} went away"));
            }
            let new: Vec<String> =
                self.devices.iter().filter(|d| !before.contains(&d.uid)).map(|d| d.name.clone()).collect();
            for name in new {
                self.note(format!("{name} is back"));
            }
        }
        self.reconcile();
        self.publish();
    }

    fn classify_default_change(&mut self) {
        let Some(at) = self.pending_default else { return };
        if at.elapsed() < CLASSIFY_AFTER {
            return;
        }
        self.pending_default = None;
        let Some(cur) = self.current().cloned() else { return };
        if let Some((id, t)) = self.expect_default {
            if id == cur.id && t.elapsed() < Duration::from_secs(5) {
                return; // this app switched it
            }
        }
        if devices::is_proxy(&cur) && self.native_target().is_some() {
            return; // the virtual speaker standing in for your screen
        }
        let preferred = self.settings.preferred_uid.clone();
        if preferred.as_deref() == Some(cur.uid.as_str()) {
            return;
        }
        let pref_ok = preferred
            .as_deref()
            .and_then(|u| self.device_by_uid(u))
            .is_some_and(|d| d.alive);
        let recent_device_change = self
            .last_devices_change
            .is_some_and(|t| t + Duration::from_secs(2) > at);
        if preferred.is_none() || (pref_ok && !recent_device_change) {
            // You switched outputs yourself (here or in System Settings).
            self.settings.preferred_uid = Some(cur.uid.clone());
            self.save_soon();
        } else {
            let pname = preferred
                .as_deref()
                .and_then(|u| self.device_by_uid(u))
                .map(|d| d.name.clone())
                .unwrap_or_else(|| "Your chosen output".into());
            self.note(format!("{pname} isn't available; using {} for now", cur.name));
        }
    }

    fn maybe_start_restore(&mut self) {
        if self.passive || !self.settings.auto_return || self.pending_default.is_some() || self.restore.is_some() {
            return;
        }
        let Some(pref) = self.settings.preferred_uid.clone() else { return };
        let Some(d) = self.device_by_uid(&pref) else { return };
        if !d.alive {
            return;
        }
        // A screen without volume control comes back through the virtual speaker.
        let goal = if self.native_target().is_some() { self.proxy().cloned() } else { Some(d.clone()) };
        let Some(goal) = goal else { return };
        if Some(goal.id) == self.current_id {
            return;
        }
        self.restore = Some(Restore { uid: goal.uid, attempts_left: RESTORE_ATTEMPTS, next_at: Instant::now() });
    }

    fn step_restore(&mut self) {
        let Some(r) = self.restore.as_mut() else { return };
        if Instant::now() < r.next_at {
            return;
        }
        let uid = r.uid.clone();
        let fresh = devices::list_outputs();
        let target = fresh.iter().find(|d| d.uid == uid && d.alive).cloned();
        let current = devices::default_output();
        if let Some(t) = &target {
            if current == Some(t.id) {
                self.restore = None;
                let name = if devices::is_proxy(t) {
                    self.native_target().map(|d| d.name.clone()).unwrap_or_else(|| t.name.clone())
                } else {
                    t.name.clone()
                };
                self.note(format!("Switched back to {name}"));
                self.mark_dirty();
                return;
            }
            let t = t.clone();
            self.set_default(&t);
        }
        let gave_up = match self.restore.as_mut() {
            Some(r) => {
                r.attempts_left = r.attempts_left.saturating_sub(1);
                r.next_at = Instant::now() + RESTORE_INTERVAL;
                r.attempts_left == 0
            }
            None => false,
        };
        if gave_up {
            self.restore = None;
            self.note("Couldn't switch back to your chosen output yet");
        }
        self.mark_dirty();
    }

    /// Keeps the virtual speaker in front of your screen: switches to it when your
    /// screen is the output, and away from it to a real speaker when the screen is gone.
    fn route_through_proxy(&mut self) {
        if self.passive {
            return;
        }
        if self.pending_default.is_some() || self.restore.is_some() {
            return;
        }
        let Some(cur) = self.current().cloned() else { return };
        // Earlier versions left BlackHole as the Mac's output: go back to your chosen screen.
        if devices::is_blackhole(&cur) && self.screen_was_proxied() {
            let to = self.native_target().cloned().and(self.proxy().cloned()).or_else(|| {
                let pref = self.settings.preferred_uid.clone()?;
                self.device_by_uid(&pref).filter(|d| d.alive).cloned()
            });
            if let Some(to) = to.or_else(|| self.fallback()) {
                self.set_default(&to);
                self.note("No longer using BlackHole");
            }
            return;
        }
        match (self.native_target().cloned(), self.proxy().cloned()) {
            // Your screen, or the other virtual speaker, is the output: use the preferred stand-in.
            (Some(t), Some(p)) if cur.uid == t.uid || (devices::is_proxy(&cur) && cur.uid != p.uid) => {
                // Carry the screen's own level over, so nothing gets suddenly louder.
                let saved = self.settings.volume_for(&t.uid);
                devices::set_hw_volume(&p, saved.volume);
                if p.hw_mute {
                    devices::set_hw_mute(&p, saved.muted);
                }
                if self.set_default(&p) {
                    self.note(format!("Using macOS volume controls for {}", t.name));
                }
            }
            (None, _) if devices::is_proxy(&cur) && self.screen_was_proxied() => {
                if let Some(f) = self.fallback() {
                    self.set_default(&f);
                }
            }
            _ => {}
        }
    }

    /// True if the virtual speaker is the output because of this app (your chosen
    /// output is a screen without volume control), not because you picked it.
    fn screen_was_proxied(&self) -> bool {
        let Some(pref) = self.settings.preferred_uid.as_deref() else { return false };
        match self.device_by_uid(pref) {
            Some(d) => !d.hw_volume && !devices::is_proxy(d),
            None => true, // your screen is away (asleep or unplugged): never leave sound in the virtual speaker
        }
    }

    fn reconcile(&mut self) {
        self.classify_default_change();
        self.route_through_proxy();
        self.maybe_start_restore();
        // What should play where: (screen, virtual speaker it's fed from, if any).
        let want: Option<(Device, Option<Device>)> = if self.default_is_proxy() {
            match (self.native_target().cloned(), self.proxy().cloned(), self.current().cloned()) {
                // Only the stand-in that is actually the output carries the sound.
                (Some(t), Some(p), Some(c)) if c.uid == p.uid => Some((t, Some(p))),
                _ => None,
            }
        } else {
            self.current().filter(|d| d.alive && self.mode_of(d) == Mode::Software).cloned().map(|d| (d, None))
        };
        let keep = match (&self.pipeline, &want) {
            (Some(p), Some((d, via))) => {
                p.device_uid == d.uid
                    && (p.sample_rate - d.sample_rate).abs() < 1.0
                    && p.proxy_uid.as_deref() == via.as_ref().map(|v| v.uid.as_str())
            }
            _ => false,
        };
        if !keep && self.pipeline.is_some() {
            self.pipeline = None; // stops and removes the tap and its output
        }
        if want.is_none() {
            self.pipe_error = None;
            self.build_attempts = 0;
        }
        let native = want.as_ref().is_some_and(|(_, via)| via.is_some());
        if let Some((t, Some(p))) = &want {
            PROXY_ID.store(p.id, Ordering::Relaxed);
            EXIT_TO.store(t.id, Ordering::Relaxed);
        } else {
            PROXY_ID.store(0, Ordering::Relaxed);
        }
        // Through the virtual speaker macOS handles the keys itself.
        let monitor_mode = self.target().is_some_and(|d| self.mode_of(d) == Mode::Monitor);
        let catch = !self.passive && self.settings.volume_keys && ((want.is_some() && !native) || monitor_mode);
        keys::CATCH_KEYS.store(catch, Ordering::Relaxed);
        if catch && !keys::trusted() {
            keys::prompt_once();
        }
        if self.passive || self.pipeline.is_some() {
            return;
        }
        let Some((d, via)) = want else {
            self.perm_message = None;
            return;
        };
        if Instant::now() < self.next_build {
            return;
        }
        // Never start before macOS allows capturing the sound: it would mute
        // everything and hand us silence.
        // The tap routes need System Audio Recording; ScreenKnob's own driver doesn't.
        let needs_capture = via.as_ref().is_none_or(|p| !devices::is_driver(p));
        let access = if needs_capture { perm::audio_capture() } else { perm::Access::Granted };
        match access {
            perm::Access::Granted => self.perm_message = None,
            access => {
                if access == perm::Access::NotAsked {
                    let recheck = || {
                        if let Some(tx) = TX.get() {
                            let _ = tx.send(Command::Recheck);
                        }
                    };
                    perm::request_audio_capture(recheck);
                }
                let what = "System Audio Recording";
                self.perm_message = Some(match access {
                    perm::Access::Denied => format!(
                        "To set {}'s volume, allow ScreenKnob under {what} in Privacy & Security.",
                        d.name
                    ),
                    _ => format!("Waiting for you to allow ScreenKnob ({what})…"),
                });
                self.next_build = Instant::now() + Duration::from_secs(2);
                return;
            }
        }
        let vol = self.settings.volume_for(&d.uid);
        let started = match &via {
            // ScreenKnob's driver shares its sound directly: nothing is recorded.
            Some(p) => Pipeline::start_ring(&d, p, proxy_gain(p)),
            None => Pipeline::start(&d, gain_for(vol.volume, vol.muted)),
        };
        match started {
            Ok(p) => {
                // Counts as working only once sound actually flows (see the watchdog).
                self.pipeline = Some(p);
                self.cb_count = 0;
                self.cb_checked = Instant::now();
            }
            Err(e) => {
                self.build_attempts += 1;
                let wait = (RESTORE_INTERVAL * self.build_attempts).min(Duration::from_secs(5));
                self.next_build = Instant::now() + wait;
                self.pipe_error = Some(e);
            }
        }
    }

    fn tick(&mut self) {
        let now = Instant::now();
        // Waking from sleep shows up as wall-clock time jumping ahead of the monotonic clock.
        let wall_gap = SystemTime::now().duration_since(self.wall).unwrap_or_default();
        let mono_gap = now.duration_since(self.mono);
        if wall_gap > mono_gap + Duration::from_secs(3) {
            self.note("Mac woke up; checking outputs");
            self.pipeline = None;
            self.next_build = now + RESTORE_INTERVAL;
            self.restore = None;
            self.mark_dirty();
        }
        self.wall = SystemTime::now();
        self.mono = now;

        if self.dirty_at.is_some_and(|t| now >= t) {
            self.dirty_at = None;
            self.refresh();
        }
        if self.pending_default.is_some_and(|t| t.elapsed() >= CLASSIFY_AFTER) {
            self.reconcile();
            self.publish();
        }
        self.step_restore();
        if self.pipeline.is_none() && (self.pipe_error.is_some() || self.perm_message.is_some()) && now >= self.next_build {
            self.reconcile();
            self.publish();
        }

        // Watchdog: the software path must keep being called by the system.
        let watched = self
            .pipeline
            .as_ref()
            .map(|p| (p.shared.callbacks.load(Ordering::Relaxed), p.started.elapsed(), p.device_name.clone()));
        if let Some((c, age, name)) = watched {
            if now.duration_since(self.cb_checked) >= WATCHDOG {
                let stalled = c == self.cb_count && age >= WATCHDOG;
                self.cb_count = c;
                self.cb_checked = now;
                if c > 0 && !stalled && (self.build_attempts > 0 || self.pipe_error.is_some()) {
                    self.note(format!("Software volume running on {name}"));
                    self.build_attempts = 0;
                    self.pipe_error = None;
                    self.publish();
                }
                if stalled {
                    let never_started = c == 0;
                    self.pipeline = None;
                    self.build_attempts += 1;
                    let wait = (RESTORE_INTERVAL * self.build_attempts).min(Duration::from_secs(5));
                    self.next_build = now + wait;
                    if never_started {
                        // macOS reports such a screen as "not running": asleep, or showing another input.
                        if self.build_attempts == 1 {
                            self.note(format!("{name} isn't taking sound right now"));
                        }
                        self.pipe_error = Some(format!(
                            "{name} isn't taking sound right now (asleep, or on another input)."
                        ));
                    } else {
                        self.note(format!("Sound to {name} stalled; restarting"));
                        self.pipe_error = Some("Restarting…".into());
                    }
                }
            }
        }

        if !self.passive && now.duration_since(self.show_checked) >= Duration::from_millis(500) {
            self.show_checked = now;
            if perm::take_show_request() {
                crate::tray::OPEN.store(true, Ordering::Relaxed);
                if let Some(f) = REPAINT.get() {
                    f();
                }
            }
        }
        if let Some(tx) = &self.ddc {
            let silent = self
                .devices
                .iter()
                .any(|d| !d.hw_volume && !devices::is_proxy(d) && d.transport != ca::TRANSPORT_VIRTUAL && !self.ddc_vol.contains_key(&d.name));
            let every = if silent { Duration::from_secs(20) } else { Duration::from_secs(120) };
            if self.settings.monitor_volume && now.duration_since(self.ddc_probed) >= every {
                self.ddc_probed = now;
                let _ = tx.send(crate::ddc::Req::Probe);
            }
        }
        if self.save_at.is_some_and(|t| now >= t) {
            self.save_at = None;
            self.settings.save();
        }

        if let Some(p) = self.pipeline.as_ref().filter(|p| p.proxy_uid.is_some()) {
            if let Some(proxy) = self.proxy().cloned() {
                p.shared.set_gain(proxy_gain(&proxy));
                // Remember the level for the screen, for when it plays without the virtual speaker.
                let uid = p.device_uid.clone();
                let v = devices::hw_volume(&proxy).unwrap_or(1.0);
                let m = devices::hw_muted(&proxy).unwrap_or(false);
                let e = self.settings.volumes.entry(uid).or_default();
                if (e.volume - v).abs() > 0.001 || e.muted != m {
                    e.volume = v;
                    e.muted = m;
                    self.save_soon();
                }
            }
        }
        let level = self
            .pipeline
            .as_ref()
            .map(|p| f32::from_bits(p.shared.level.swap(0, Ordering::Relaxed)))
            .unwrap_or(0.0);
        if let Ok(mut s) = self.snapshot.lock() {
            s.level = level;
            s.keys_trusted = self.passive || keys::trusted();
            s.keys_active = keys::TAP_RUNNING.load(Ordering::Relaxed);
        }
        if now.duration_since(self.last_publish) > Duration::from_secs(2) {
            self.publish();
        } else if let Some(f) = REPAINT.get() {
            f();
        }
    }

    fn publish(&mut self) {
        self.last_publish = Instant::now();
        let pref = self.settings.preferred_uid.clone();
        let target_id = self.target().map(|d| d.id);
        let hide_proxy = self.proxy().is_some();
        let views: Vec<DeviceView> = self
            .devices
            .iter()
            .filter(|d| !(hide_proxy && devices::is_proxy(d)))
            .map(|d| {
                let mode = self.mode_of(d);
                let volume = if d.hw_volume {
                    devices::hw_volume(d).unwrap_or(0.0)
                } else {
                    self.settings.volume_for(&d.uid).volume
                };
                let muted = if d.hw_mute {
                    devices::hw_muted(d).unwrap_or(false)
                } else {
                    self.settings.volume_for(&d.uid).muted
                };
                let (volume, muted) = match self.volume_device(&d.uid) {
                    Some(v) if devices::is_proxy(&v) && !devices::is_proxy(d) => (
                        devices::hw_volume(&v).unwrap_or(volume),
                        devices::hw_muted(&v).unwrap_or(muted),
                    ),
                    _ => (volume, muted),
                };
                DeviceView {
                    uid: d.uid.clone(),
                    name: d.name.clone(),
                    kind: d.kind_label(),
                    is_current: Some(d.id) == target_id,
                    is_preferred: pref.as_deref() == Some(d.uid.as_str()),
                    mode,
                    volume,
                    muted,
                }
            })
            .collect();
        let cur = self.target().cloned();
        let native_active = self.pipeline.as_ref().is_some_and(|p| p.proxy_uid.is_some());
        let (status, ok) = match &cur {
            None => ("No sound output found".to_string(), false),
            Some(d) => match self.mode_of(d) {
                Mode::Hardware => (format!("{} uses its own volume control", d.name), true),
                Mode::Monitor => (format!("{}: volume set on the monitor itself (no recording)", d.name), true),
                Mode::Unavailable => (format!("{} has no volume control. Turn on software volume below.", d.name), false),
                Mode::Software if self.perm_message.is_some() && !self.passive => {
                    (self.perm_message.clone().unwrap_or_default(), false)
                }
                Mode::Software => match (&self.pipeline, &self.pipe_error) {
                    (_, Some(e)) => (format!("{e} Trying again…"), false),
                    (Some(p), None) if native_active && p.uses_driver() => {
                        (format!("{} uses the macOS volume controls (ScreenKnob driver, nothing recorded)", d.name), true)
                    }
                    (Some(_), None) => (format!("Software volume is on for {}", d.name), true),
                    (None, None) if self.passive => (format!("Software volume for {}", d.name), true),
                    (None, None) => (format!("Starting software volume for {}…", d.name), true),
                },
            },
        };
        let preferred_name = pref.as_deref().map(|u| {
            self.device_by_uid(u).map(|d| d.name.clone()).unwrap_or_else(|| "Your chosen output".into())
        });
        let preferred_missing = pref.as_deref().is_some_and(|u| self.device_by_uid(u).is_none());
        if let Ok(mut s) = self.snapshot.lock() {
            s.devices = views;
            s.status = status;
            s.status_ok = ok;
            s.preferred_name = preferred_name;
            s.preferred_missing = preferred_missing;
            s.auto_return = self.settings.auto_return;
            s.software = self.settings.software_volume;
            s.keys = self.settings.volume_keys;
            s.needs_audio_permission = self.perm_message.is_some();
            s.permission_pane = "Privacy_AudioCapture";
            s.native = self.settings.native_popup;
            s.native_available = self.devices.iter().any(devices::is_proxy);
            s.native_active = native_active;
            s.monitor_volume = self.settings.monitor_volume;
            s.driver_installed = self.devices.iter().any(devices::is_driver) || crate::driver::installed();
            s.driver_available = crate::driver::source().is_some();
            s.driver_busy = self.driver_busy;
            s.hud = self.hud.clone();
            s.login = if self.passive { crate::login::Login::Unavailable } else { crate::login::status() };

            s.log = self.log.iter().cloned().collect();
        }
        if let Some(f) = REPAINT.get() {
            f();
        }
    }
}
