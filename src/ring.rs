//! Reads the sound the ScreenKnob driver shares (layout: driver/ScreenKnobRing.h)
//! and plays it on the screen with a plain output callback. No capture API is
//! used, so macOS shows no recording indicator.
//!
//! Timing: the driver runs on the Mac's host clock and publishes where its
//! sample 0 is (`anchor_host`) and how long a frame lasts. Each screen callback
//! knows, in host time, when its sound will be heard, so the reader computes
//! exactly which driver frames belong there, a fixed delay behind. Whatever
//! block sizes apps make the driver use, the reader never runs ahead of what
//! was written. The small remaining difference between the two clocks is
//! followed slowly (a fraction of a frame per callback), so it can't click.

use crate::ca::{self, AudioBufferList, AudioObjectID, IOProcID};
use crate::devices::Device;
use crate::tap::Shared;
use std::cell::UnsafeCell;
use std::ffi::{c_char, c_int, c_void};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

pub const SHM_NAME: &std::ffi::CStr = c"/screenknob.audio.1";
/// The test copy of the driver shares under this name (scripts/make-driver.sh).
pub const TEST_SHM_NAME: &std::ffi::CStr = c"/screenknob.test.1";
const MAGIC: u32 = 0x534B_4155;
const VERSION: u32 = 1;
const FRAMES: u64 = 65536;
const CHANNELS: usize = 2;
const BYTES: usize = 64 + FRAMES as usize * CHANNELS * 4;
/// Extra frames kept between the reader and the newest sound.
const MARGIN: f64 = 256.0;
/// Most the delay may grow to after running dry (about 0.17 s).
const MAX_DELAY: f64 = 8192.0;
/// For a driver that doesn't publish its clock (version 1): how far behind to read.
const OLD_TARGET: f64 = 6144.0;

#[repr(C)]
struct Header {
    magic: u32,
    version: u32,
    capacity: u32,
    channels: u32,
    sample_rate: f64,
    write_end: AtomicU64,
    running: AtomicU32,
    generation: AtomicU32,
    volume_bits: AtomicU32,
    muted: AtomicU32,
    anchor_host: AtomicU64,
    ticks_per_frame: AtomicU64, // f64 bits
}

#[repr(C)]
struct AudioTimeStamp {
    sample_time: f64,
    host_time: u64,
    rate_scalar: f64,
    word_clock_time: u64,
    smpte: [u8; 24],
    flags: u32,
    reserved: u32,
}
const HOST_TIME_VALID: u32 = 2;
const RATE_SCALAR_VALID: u32 = 4;

unsafe extern "C" {
    fn shm_open(name: *const c_char, oflag: c_int, ...) -> c_int;
    fn mmap(addr: *mut c_void, len: usize, prot: c_int, flags: c_int, fd: c_int, off: i64) -> *mut c_void;
    fn munmap(addr: *mut c_void, len: usize) -> c_int;
    fn close(fd: c_int) -> c_int;
    fn mach_absolute_time() -> u64;
}
const O_RDONLY: c_int = 0;
const PROT_READ: c_int = 1;
const MAP_SHARED: c_int = 1;
const MAP_FAILED: *mut c_void = !0usize as *mut c_void;

/// A read-only view of the driver's shared memory.
pub struct Ring {
    base: *mut c_void,
}

unsafe impl Send for Ring {}
unsafe impl Sync for Ring {}

impl Ring {
    pub fn open(name: &std::ffi::CStr) -> Result<Ring, String> {
        let fd = unsafe { shm_open(name.as_ptr(), O_RDONLY) };
        if fd < 0 {
            return Err("The ScreenKnob driver isn't sharing sound yet (restart the Mac's sound or reinstall the driver).".into());
        }
        let base = unsafe { mmap(std::ptr::null_mut(), BYTES, PROT_READ, MAP_SHARED, fd, 0) };
        unsafe { close(fd) };
        if base == MAP_FAILED || base.is_null() {
            return Err("Couldn't read the ScreenKnob driver's sound.".into());
        }
        let r = Ring { base };
        let h = r.header();
        if h.magic != MAGIC || h.version != VERSION || h.capacity as u64 != FRAMES || h.channels as usize != CHANNELS {
            return Err("The ScreenKnob driver is a different version; reinstall it.".into());
        }
        Ok(r)
    }

    fn header(&self) -> &Header {
        unsafe { &*(self.base as *const Header) }
    }

    fn sample(&self, frame: u64, ch: usize) -> f32 {
        let i = (frame % FRAMES) as usize * CHANNELS + ch;
        unsafe { *(self.base as *const u8).add(64).cast::<f32>().add(i) }
    }

    pub fn sample_rate(&self) -> f64 {
        self.header().sample_rate
    }
}

impl Drop for Ring {
    fn drop(&mut self) {
        unsafe { munmap(self.base, BYTES) };
    }
}

/// State only the audio callback touches.
struct Reader {
    /// Driver frame where the next callback starts (continuing the last one).
    next: f64,
    /// How far behind the driver's clock to read, in frames.
    delay: f64,
    generation: u32,
    synced: bool,
    /// Fallback for drivers that don't publish their clock.
    old_err: f64,
}

struct Ctx {
    shared: Arc<Shared>,
    ring: Ring,
    reader: UnsafeCell<Reader>,
    /// Driver frames per screen frame (nominal).
    base_ratio: f64,
}

unsafe impl Sync for Ctx {}

/// Plays the driver's sound on one screen.
pub struct RingPlayer {
    dev: AudioObjectID,
    proc_id: IOProcID,
    ctx: Arc<Ctx>,
    running: bool,
}

unsafe impl Send for RingPlayer {}

impl RingPlayer {
    pub fn start(dev: &Device, shared: Arc<Shared>) -> Result<RingPlayer, String> {
        RingPlayer::start_named(dev, shared, SHM_NAME)
    }

    pub fn start_named(dev: &Device, shared: Arc<Shared>, name: &std::ffi::CStr) -> Result<RingPlayer, String> {
        let ring = Ring::open(name)?;
        let screen_rate = if dev.sample_rate > 0.0 { dev.sample_rate } else { 48000.0 };
        let ctx = Arc::new(Ctx {
            shared,
            base_ratio: ring.sample_rate() / screen_rate,
            ring,
            reader: UnsafeCell::new(Reader { next: 0.0, delay: 0.0, generation: 0, synced: false, old_err: 0.0 }),
        });
        let mut p = RingPlayer { dev: dev.id, proc_id: std::ptr::null_mut(), ctx, running: false };
        let client = Arc::as_ptr(&p.ctx) as *mut c_void;
        let st = unsafe { ca::AudioDeviceCreateIOProcID(dev.id, ring_proc, client, &mut p.proc_id) };
        if st != 0 || p.proc_id.is_null() {
            return Err(format!("Couldn't open {} (error {st}).", dev.name));
        }
        let st = unsafe { ca::AudioDeviceStart(dev.id, p.proc_id) };
        if st != 0 {
            return Err(format!("{} isn't taking sound right now (error {st}).", dev.name));
        }
        p.running = true;
        Ok(p)
    }
}

impl Drop for RingPlayer {
    fn drop(&mut self) {
        unsafe {
            if self.running {
                let _ = ca::AudioDeviceStop(self.dev, self.proc_id);
            }
            if !self.proc_id.is_null() {
                let _ = ca::AudioDeviceDestroyIOProcID(self.dev, self.proc_id);
            }
        }
    }
}

extern "C" fn ring_proc(
    _dev: AudioObjectID,
    _now: *const c_void,
    _input: *const AudioBufferList,
    _in_time: *const c_void,
    output: *mut AudioBufferList,
    out_time: *const c_void,
    client: *mut c_void,
) -> i32 {
    std::panic::catch_unwind(|| render(output, out_time as *const AudioTimeStamp, client)).unwrap_or(0)
}

fn render(output: *mut AudioBufferList, out_time: *const AudioTimeStamp, client: *mut c_void) -> i32 {
    if client.is_null() {
        return 0;
    }
    let ctx = unsafe { &*(client as *const Ctx) };
    let r = unsafe { &mut *ctx.reader.get() };
    let shared = &ctx.shared;
    shared.callbacks.fetch_add(1, Ordering::Relaxed);
    let outs = unsafe { AudioBufferList::buffers(output) };
    let frames = outs
        .iter()
        .find(|b| b.mNumberChannels > 0)
        .map(|b| b.mDataByteSize as usize / 4 / b.mNumberChannels as usize)
        .unwrap_or(0);

    let h = ctx.ring.header();
    let anchor = h.anchor_host.load(Ordering::Acquire);
    let tpf = f64::from_bits(h.ticks_per_frame.load(Ordering::Relaxed));
    let end = h.write_end.load(Ordering::Acquire) as f64;
    let generation = h.generation.load(Ordering::Relaxed);
    let playing = h.running.load(Ordering::Relaxed) != 0;
    let ts = unsafe { out_time.as_ref() };

    let (start, step) = match ts {
        Some(ts) if anchor != 0 && tpf > 0.0 && ts.flags & HOST_TIME_VALID != 0 && ts.host_time > anchor => {
            let rate = if ts.flags & RATE_SCALAR_VALID != 0 && ts.rate_scalar > 0.5 && ts.rate_scalar < 2.0 {
                ts.rate_scalar
            } else {
                1.0
            };
            let step = ctx.base_ratio * rate;
            // The driver frame that will be heard when this buffer is heard.
            let at = (ts.host_time - anchor) as f64 / tpf;
            if !r.synced || generation != r.generation || r.delay <= 0.0 {
                // How far ahead of now the screen plays, plus this buffer, plus a margin.
                let now = unsafe { mach_absolute_time() };
                let lead = if ts.host_time > now { (ts.host_time - now) as f64 / tpf } else { 0.0 };
                r.delay = r.delay.max(lead + frames as f64 * step + MARGIN).min(MAX_DELAY);
                r.next = at - r.delay;
                r.generation = generation;
                r.synced = true;
            }
            let want = at - r.delay;
            let diff = want - r.next;
            // Follow the clock difference a little at a time; only a big jump re-aligns.
            let start = if diff.abs() > 256.0 { want } else { r.next + diff * 0.02 };
            (start, step)
        }
        _ => {
            // Older driver: keep a steady distance behind what's been written.
            let lag = end - r.next;
            if !r.synced || generation != r.generation || lag < 0.0 || lag > (FRAMES as f64) * 0.5 {
                r.next = (end - OLD_TARGET).max(0.0);
                r.generation = generation;
                r.synced = true;
                r.old_err = 0.0;
            }
            r.old_err = r.old_err * 0.999 + (end - r.next - OLD_TARGET) * 0.001;
            let adjust = (r.old_err / OLD_TARGET * 0.005).clamp(-0.002, 0.002);
            (r.next, ctx.base_ratio * (1.0 + adjust))
        }
    };

    let gain_start = f32::from_bits(shared.applied_bits());
    let gain_end = shared.target_gain();
    let mut peak = 0.0f32;
    let mut dry = false;
    for b in outs {
        let och = b.mNumberChannels as usize;
        if b.mData.is_null() || och == 0 {
            continue;
        }
        let out = unsafe { std::slice::from_raw_parts_mut(b.mData as *mut f32, b.mDataByteSize as usize / 4) };
        let n = out.len() / och;
        for f in 0..n {
            let pos = start + step * f as f64;
            let g = gain_start + (gain_end - gain_start) * (f as f32 / n.max(1) as f32);
            let ok = playing && pos >= 0.0 && pos + 1.0 < end && end - pos < FRAMES as f64 - 1024.0;
            dry |= playing && !ok;
            let i0 = pos.max(0.0).floor();
            let t = (pos - i0) as f32;
            for c in 0..och {
                let v = if ok && c < CHANNELS {
                    let a = ctx.ring.sample(i0 as u64, c);
                    let bb = ctx.ring.sample(i0 as u64 + 1, c);
                    ((a + (bb - a) * t) * g).clamp(-1.0, 1.0)
                } else {
                    0.0
                };
                peak = peak.max(v.abs());
                if let Some(o) = out.get_mut(f * och + c) {
                    *o = v;
                }
            }
        }
    }
    r.next = start + step * frames as f64;
    if dry {
        // Ran out of the driver's sound: count it and stay further behind from now on.
        shared.underruns.fetch_add(1, Ordering::Relaxed);
        if anchor != 0 {
            r.delay = (r.delay + 512.0).min(MAX_DELAY);
            r.synced = false;
        }
    }
    shared.store_applied(gain_end);
    let prev = f32::from_bits(shared.level.load(Ordering::Relaxed));
    shared.level.store(peak.max(prev * 0.9).to_bits(), Ordering::Relaxed);
    // For the self-test: how far behind the newest sound the reader is.
    shared.layout.store((end - r.next).max(0.0) as u64, Ordering::Relaxed);
    0
}
