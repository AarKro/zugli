//! Cross-task shared state: the display channel, the live selection, the wall clock,
//! and the device IP. All are process-global statics guarded for concurrent access.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use portable_atomic::AtomicI64;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use embassy_time::Instant;

use crate::model::{DisplayState, Selection};

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

/// Live mirror of [`Config::strip_city`](crate::model::Config), read by the render task each
/// frame. Set at boot from flash and whenever the config page toggles it.
static STRIP_CITY: AtomicBool = AtomicBool::new(false);

/// Update the "hide city names" setting (from boot load or a config change).
pub fn set_strip_city(on: bool) {
    STRIP_CITY.store(on, Ordering::Relaxed);
}

/// Whether the panel should drop the "City, " prefix from stop/destination names.
pub fn strip_city_enabled() -> bool {
    STRIP_CITY.load(Ordering::Relaxed)
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
