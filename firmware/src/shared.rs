//! Cross-task shared state: the display channel, the live selection, the wall clock,
//! and the device IP. All are process-global statics guarded for concurrent access.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use portable_atomic::AtomicI64;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use embassy_time::Instant;

use crate::model::{Config, DisplayState, Selection};

/// Latest thing to show on the panel. The render task waits on this and redraws on change.
pub static DISPLAY: Signal<CriticalSectionRawMutex, DisplayState> = Signal::new();

/// The currently active selection (the connection being tracked), or `None`.
pub static SELECTION: Mutex<CriticalSectionRawMutex, Option<Selection>> = Mutex::new(None);

/// Pulsed whenever the selection changes (via `POST /save`) so the poll task re-polls now.
pub static SELECTION_CHANGED: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Unix time corresponding to `Instant` zero, set once SNTP succeeds. `0` = not yet synced.
static BOOT_UNIX: AtomicI64 = AtomicI64::new(0);

/// Device IPv4 once DHCP has assigned one (packed big-endian), `0` = none yet.
static DEVICE_IP: AtomicU32 = AtomicU32::new(0);

// Live mirror of the persisted [`Config`], read by the render task each frame. Set at boot from
// flash and whenever the config page changes a setting. Plain atomics (no mutex) so the render
// task never blocks; the reduced-brightness window is packed `start << 16 | end` into one u32.
static STRIP_CITY: AtomicBool = AtomicBool::new(false);
static SHOW_LINE_BADGES: AtomicBool = AtomicBool::new(true);
static BRIGHTNESS_LEVEL: AtomicU32 = AtomicU32::new(6);
static AUTO_BRIGHTNESS: AtomicBool = AtomicBool::new(true);
static REDUCED_WINDOW: AtomicU32 = AtomicU32::new(((20 * 60) << 16) | (8 * 60));

/// Push a whole [`Config`] into the live mirror (boot load or a config-page change).
pub fn apply_config(cfg: &Config) {
    STRIP_CITY.store(cfg.strip_city, Ordering::Relaxed);
    SHOW_LINE_BADGES.store(cfg.show_line_badges, Ordering::Relaxed);
    BRIGHTNESS_LEVEL.store(cfg.brightness.clamp(1, 10) as u32, Ordering::Relaxed);
    AUTO_BRIGHTNESS.store(cfg.auto_brightness, Ordering::Relaxed);
    REDUCED_WINDOW.store(
        ((cfg.reduced_start as u32) << 16) | cfg.reduced_end as u32,
        Ordering::Relaxed,
    );
}

/// Whether the panel should drop the "City, " prefix from stop/destination names.
pub fn strip_city_enabled() -> bool {
    STRIP_CITY.load(Ordering::Relaxed)
}

/// Whether departure rows show the line as a filled badge (vs. plain text).
pub fn line_badges_enabled() -> bool {
    SHOW_LINE_BADGES.load(Ordering::Relaxed)
}

/// Manual brightness level, 1–10 (× 10 % = panel brightness).
pub fn brightness_level() -> u8 {
    BRIGHTNESS_LEVEL.load(Ordering::Relaxed) as u8
}

/// Whether time-of-day auto-dimming is enabled.
pub fn auto_brightness_enabled() -> bool {
    AUTO_BRIGHTNESS.load(Ordering::Relaxed)
}

/// Start of the reduced-brightness window, minutes since local midnight.
pub fn reduced_start_min() -> u16 {
    (REDUCED_WINDOW.load(Ordering::Relaxed) >> 16) as u16
}

/// End of the reduced-brightness window, minutes since local midnight.
pub fn reduced_end_min() -> u16 {
    (REDUCED_WINDOW.load(Ordering::Relaxed) & 0xFFFF) as u16
}

/// Record the wall clock from an SNTP sample.
pub fn set_clock(now_unix: i64) {
    BOOT_UNIX.store(now_unix - Instant::now().as_secs() as i64, Ordering::Relaxed);
}

/// Current Unix time, or `None` if SNTP has not synced yet.
pub fn now_unix() -> Option<i64> {
    let base = BOOT_UNIX.load(Ordering::Relaxed);
    if base == 0 {
        None
    } else {
        Some(base + Instant::now().as_secs() as i64)
    }
}

/// Store the device IPv4 address.
pub fn set_device_ip(octets: [u8; 4]) {
    DEVICE_IP.store(u32::from_be_bytes(octets), Ordering::Relaxed);
}

/// Read the device IPv4 address, or `None` if not assigned.
pub fn device_ip() -> Option<[u8; 4]> {
    let v = DEVICE_IP.load(Ordering::Relaxed);
    if v == 0 {
        None
    } else {
        Some(v.to_be_bytes())
    }
}
