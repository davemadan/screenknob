//! Which screens are asleep. When a screen wakes, its sound link starts over
//! (HDMI and DisplayPort renegotiate), and a stream that was playing through
//! the sleep keeps running but is no longer heard. So the engine restarts the
//! sound whenever a screen wakes.

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGGetOnlineDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
    fn CGDisplayIsAsleep(display: u32) -> u32;
}

/// The screens that are connected and asleep, by display id.
pub fn asleep() -> Vec<u32> {
    let mut ids = [0u32; 16];
    let mut n = 0u32;
    if unsafe { CGGetOnlineDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut n) } != 0 {
        return Vec::new();
    }
    ids[..n as usize].iter().copied().filter(|&d| unsafe { CGDisplayIsAsleep(d) } != 0).collect()
}
