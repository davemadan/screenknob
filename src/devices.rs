//! Output devices: listing them and reading / writing their own (hardware) volume.

use crate::ca::{self, AudioObjectID, AudioObjectPropertyAddress as Addr};

/// UID prefix of the private aggregate devices this app makes; they're never listed.
pub const OWN_UID_PREFIX: &str = "craft.monitor-volume.";

#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub id: AudioObjectID,
    pub uid: String,
    pub name: String,
    pub transport: u32,
    pub channels: u32,
    pub sample_rate: f64,
    pub alive: bool,
    /// The device has a volume control macOS can set.
    pub hw_volume: bool,
    pub hw_mute: bool,
}

impl Device {
    pub fn kind_label(&self) -> &'static str {
        match self.transport {
            ca::TRANSPORT_BUILTIN => "Built-in",
            ca::TRANSPORT_HDMI => "HDMI",
            ca::TRANSPORT_DISPLAYPORT => "DisplayPort",
            ca::TRANSPORT_USB => "USB",
            ca::TRANSPORT_BLUETOOTH | ca::TRANSPORT_BLUETOOTH_LE => "Bluetooth",
            ca::TRANSPORT_AIRPLAY => "AirPlay",
            ca::TRANSPORT_THUNDERBOLT => "Thunderbolt",
            ca::TRANSPORT_AGGREGATE => "Aggregate",
            ca::TRANSPORT_VIRTUAL => "Virtual",
            _ => "Other",
        }
    }
}

fn vol_addr(elem: u32) -> Addr {
    Addr::new(ca::PROP_VOLUME_SCALAR, ca::SCOPE_OUTPUT, elem)
}

fn mute_addr(elem: u32) -> Addr {
    Addr::new(ca::PROP_MUTE, ca::SCOPE_OUTPUT, elem)
}

/// Elements whose volume can be set: the main element, or else each channel.
fn volume_elements(id: AudioObjectID, channels: u32) -> Vec<u32> {
    if ca::settable(id, vol_addr(ca::ELEMENT_MAIN)) {
        return vec![ca::ELEMENT_MAIN];
    }
    (1..=channels.max(2)).filter(|&e| ca::settable(id, vol_addr(e))).collect()
}

pub fn read_device(id: AudioObjectID) -> Option<Device> {
    let uid = ca::get_string(id, Addr::new(ca::PROP_UID, ca::SCOPE_GLOBAL, ca::ELEMENT_MAIN))?;
    if uid.starts_with(OWN_UID_PREFIX) {
        return None;
    }
    let channels = ca::output_channels(id);
    if channels == 0 {
        return None;
    }
    let hidden: u32 = ca::get(id, Addr::new(ca::PROP_HIDDEN, ca::SCOPE_GLOBAL, ca::ELEMENT_MAIN)).unwrap_or(0);
    if hidden != 0 {
        return None;
    }
    let name = ca::get_string(id, Addr::new(ca::PROP_NAME, ca::SCOPE_GLOBAL, ca::ELEMENT_MAIN))
        .unwrap_or_else(|| "Unnamed output".into());
    let transport: u32 = ca::get(id, Addr::new(ca::PROP_TRANSPORT, ca::SCOPE_GLOBAL, ca::ELEMENT_MAIN)).unwrap_or(0);
    let sample_rate: f64 =
        ca::get(id, Addr::new(ca::PROP_NOMINAL_RATE, ca::SCOPE_GLOBAL, ca::ELEMENT_MAIN)).unwrap_or(0.0);
    let alive: u32 = ca::get(id, Addr::new(ca::PROP_IS_ALIVE, ca::SCOPE_GLOBAL, ca::ELEMENT_MAIN)).unwrap_or(1);
    let hw_volume = !volume_elements(id, channels).is_empty();
    let hw_mute = ca::settable(id, mute_addr(ca::ELEMENT_MAIN));
    Some(Device { id, uid, name, transport, channels, sample_rate, alive: alive != 0, hw_volume, hw_mute })
}

pub fn list_outputs() -> Vec<Device> {
    let ids = ca::get_u32_list(
        ca::kAudioObjectSystemObject,
        Addr::new(ca::PROP_DEVICES, ca::SCOPE_GLOBAL, ca::ELEMENT_MAIN),
    );
    ids.into_iter().filter_map(read_device).collect()
}

pub fn default_output() -> Option<AudioObjectID> {
    let id: u32 = ca::get(
        ca::kAudioObjectSystemObject,
        Addr::new(ca::PROP_DEFAULT_OUTPUT, ca::SCOPE_GLOBAL, ca::ELEMENT_MAIN),
    )?;
    (id != ca::kAudioObjectUnknown).then_some(id)
}

pub fn set_default_output(id: AudioObjectID) -> bool {
    ca::set(
        ca::kAudioObjectSystemObject,
        Addr::new(ca::PROP_DEFAULT_OUTPUT, ca::SCOPE_GLOBAL, ca::ELEMENT_MAIN),
        id,
    ) == 0
}

/// The device's own volume (average of its controllable elements), 0..1.
pub fn hw_volume(dev: &Device) -> Option<f32> {
    let elems = volume_elements(dev.id, dev.channels);
    let vals: Vec<f32> = elems.iter().filter_map(|&e| ca::get::<f32>(dev.id, vol_addr(e))).collect();
    if vals.is_empty() { None } else { Some(vals.iter().sum::<f32>() / vals.len() as f32) }
}

/// Sets the device's own volume (clamped to 0..1, as the original does).
pub fn set_hw_volume(dev: &Device, v: f32) -> bool {
    let v = v.clamp(0.0, 1.0);
    let mut ok = false;
    for e in volume_elements(dev.id, dev.channels) {
        ok |= ca::set(dev.id, vol_addr(e), v) == 0;
    }
    ok
}

pub fn hw_muted(dev: &Device) -> Option<bool> {
    ca::get::<u32>(dev.id, mute_addr(ca::ELEMENT_MAIN)).map(|m| m != 0)
}

pub fn set_hw_mute(dev: &Device, muted: bool) -> bool {
    ca::set(dev.id, mute_addr(ca::ELEMENT_MAIN), muted as u32) == 0
}

/// UID of ScreenKnob's own virtual speaker (driver/ScreenKnobAudio.c).
pub const DRIVER_UID: &str = "ScreenKnob_Device";

/// A virtual speaker standing in for a screen: ScreenKnob's own driver, or the
/// free BlackHole driver. macOS gives either a real volume control.
pub fn is_proxy(d: &Device) -> bool {
    is_driver(d) || d.uid.starts_with("BlackHole2ch") || (d.transport == ca::TRANSPORT_VIRTUAL && d.name == "BlackHole 2ch")
}

/// ScreenKnob's own driver: its sound is shared directly, so nothing is recorded.
pub fn is_driver(d: &Device) -> bool {
    d.uid == DRIVER_UID
}
