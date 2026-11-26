//! Reads the sound the ScreenKnob driver shares (layout: driver/ScreenKnobRing.h)
//! and plays it on the screen with a plain output callback. No capture API is
//! used, so macOS shows no recording indicator.
//!
//! The driver's clock and the screen's clock drift slightly apart, so the
//! reader resamples by a ratio that's nudged to keep a steady distance behind
//! the writer (linear interpolation; changes of a few parts per million).

use crate::ca::{self, AudioBufferList, AudioObjectID, IOProcID};
use crate::devices::Device;
use crate::tap::Shared;
use std::cell::UnsafeCell;
use std::ffi::{c_char, c_int, c_void};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

pub const SHM_NAME: &std::ffi::CStr = c"/screenknob.audio.1";
const MAGIC: u32 = 0x534B_4155;
const VERSION: u32 = 1;
const FRAMES: u64 = 65536;
const CHANNELS: usize = 2;
const BYTES: usize = 64 + FRAMES as usize * CHANNELS * 4;
/// How far behind the driver to read (about 40 ms at 48 kHz).
const TARGET: f64 = 1920.0;

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
    _reserved: [u8; 16],
}

unsafe extern "C" {
    fn shm_open(name: *const c_char, oflag: c_int, ...) -> c_int;
    fn mmap(addr: *mut c_void, len: usize, prot: c_int, flags: c_int, fd: c_int, off: i64) -> *mut c_void;
    fn munmap(addr: *mut c_void, len: usize) -> c_int;
    fn close(fd: c_int) -> c_int;
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
    pub fn open() -> Result<Ring, String> {
        let fd = unsafe { shm_open(SHM_NAME.as_ptr(), O_RDONLY) };
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
    pos: f64,
    generation: u32,
    synced: bool,
    err: f64,
}

struct Ctx {
    shared: Arc<Shared>,
    ring: Ring,
    reader: UnsafeCell<Reader>,
    /// Driver frames per screen frame, before the drift correction.
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
        let ring = Ring::open()?;
        let screen_rate = if dev.sample_rate > 0.0 { dev.sample_rate } else { 48000.0 };
        let ctx = Arc::new(Ctx {
            shared: shared.clone(),
            base_ratio: ring.sample_rate() / screen_rate,
            ring,
            reader: UnsafeCell::new(Reader { pos: 0.0, generation: 0, synced: false, err: 0.0 }),
        });
        drop(shared);
        let mut p = RingPlayer {
            dev: dev.id,
            proc_id: std::ptr::null_mut(),
            ctx,
            running: false,
        };
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
    _out_time: *const c_void,
    client: *mut c_void,
) -> i32 {
    std::panic::catch_unwind(|| render(output, client)).unwrap_or(0)
}

fn render(output: *mut AudioBufferList, client: *mut c_void) -> i32 {
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
    let end = h.write_end.load(Ordering::Acquire) as f64;
    let generation = h.generation.load(Ordering::Relaxed);
    let playing = h.running.load(Ordering::Relaxed) != 0;

    // Catch up after a start, a pause or a big jump.
    let lag = end - r.pos;
    if !r.synced || generation != r.generation || lag < 0.0 || lag > (FRAMES as f64) * 0.5 {
        r.pos = (end - TARGET).max(0.0);
        r.generation = generation;
        r.synced = true;
        r.err = 0.0;
    }
    // Nudge the speed to hold the target distance (at most ±0.5 %).
    let lag = end - r.pos;
    r.err = r.err * 0.995 + (lag - TARGET) * 0.005;
    let adjust = (r.err / TARGET * 0.01).clamp(-0.005, 0.005);
    let step = ctx.base_ratio * (1.0 + adjust);

    let gain_start = f32::from_bits(shared_applied(shared));
    let gain_end = shared.target_gain();
    let mut peak = 0.0f32;
    let start_pos = r.pos;
    for b in outs {
        let och = b.mNumberChannels as usize;
        if b.mData.is_null() || och == 0 {
            continue;
        }
        let out = unsafe { std::slice::from_raw_parts_mut(b.mData as *mut f32, b.mDataByteSize as usize / 4) };
        let n = out.len() / och;
        let mut pos = start_pos;
        for f in 0..n {
            let g = gain_start + (gain_end - gain_start) * (f as f32 / n.max(1) as f32);
            let ok = playing && pos + 1.0 < end;
            let i0 = pos.floor();
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
            if ok {
                pos += step;
            }
        }
    }
    if playing {
        r.pos = start_pos + step * frames as f64;
    }
    // For the self-test: how far behind the driver the reader is.
    shared.layout.store((end - r.pos).max(0.0) as u64, Ordering::Relaxed);
    shared.store_applied(gain_end);
    let prev = f32::from_bits(shared.level.load(Ordering::Relaxed));
    shared.level.store(peak.max(prev * 0.9).to_bits(), Ordering::Relaxed);
    0
}

fn shared_applied(s: &Shared) -> u32 {
    s.applied_bits()
}
