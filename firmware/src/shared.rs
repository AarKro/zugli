//! Cross-task shared state: the display channel, the live selection, the wall clock,
//! and the device IP. All are process-global statics guarded for concurrent access.

use core::cell::RefCell;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};

use alloc::boxed::Box;
use esp_alloc::ExternalMemory;
use portable_atomic::{AtomicI64, AtomicU64};

use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant};

use crate::model::{Config, DisplayState, Layout, Selection, UiMode};

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
static OFF_WHEN_DIMMED: AtomicBool = AtomicBool::new(false);
static REDUCED_WINDOW: AtomicU32 = AtomicU32::new(((20 * 60) << 16) | (8 * 60));
static UI_MODE: AtomicU8 = AtomicU8::new(0); // 0=Default, 1=Focus, 2=Custom

/// Live mirror of the persisted custom [`Layout`], read by the render task in Custom mode. A
/// `Layout` is far larger than an atomic, so it sits behind a critical-section mutex (like
/// [`SELECTION`]); the render task clones it out once per redraw, which is event-driven — not
/// per-DMA-frame — so the brief lock is acceptable. `None` = no custom layout saved.
///
/// The layout is boxed **into PSRAM** ([`ExternalMemory`]) so it touches neither of the two scarce
/// internal-RAM budgets: an inline `~900-byte` `Layout` in `.bss` would push down the core-0 stack
/// floor and overflow the poll task's TLS handshake, while a plain `Box` (internal heap) would eat
/// the DMA-capable RAM WiFi needs and null-fault it. PSRAM is abundant (8 MB) and is where the poll
/// task's big TLS/HTTP buffers already live. This static then costs only a pointer in `.bss`.
static CUSTOM_LAYOUT: BlockingMutex<
    CriticalSectionRawMutex,
    RefCell<Option<Box<Layout, ExternalMemory>>>,
> = BlockingMutex::new(RefCell::new(None));

/// Push a whole [`Config`] into the live mirror (boot load or a config-page change).
pub fn apply_config(cfg: &Config) {
    STRIP_CITY.store(cfg.strip_city, Ordering::Relaxed);
    SHOW_LINE_BADGES.store(cfg.show_line_badges, Ordering::Relaxed);
    BRIGHTNESS_LEVEL.store(cfg.brightness.clamp(1, 10) as u32, Ordering::Relaxed);
    AUTO_BRIGHTNESS.store(cfg.auto_brightness, Ordering::Relaxed);
    OFF_WHEN_DIMMED.store(cfg.off_when_dimmed, Ordering::Relaxed);
    REDUCED_WINDOW.store(
        ((cfg.reduced_start as u32) << 16) | cfg.reduced_end as u32,
        Ordering::Relaxed,
    );
    UI_MODE.store(cfg.ui_mode, Ordering::Relaxed);
}

/// Replace the live custom-layout mirror (boot load from flash, or a `POST /layout`). Pass `None`
/// to clear it. The new layout is boxed and the old one dropped **outside** the critical section, so
/// no heap allocation/free runs while interrupts are disabled.
pub fn apply_layout(layout: Option<Layout>) {
    let boxed = layout.map(|l| Box::new_in(l, ExternalMemory));
    let old = CUSTOM_LAYOUT.lock(|cell| cell.replace(boxed));
    drop(old);
}

/// Clone the current custom layout out of the mirror for the render task. Returns `None` when no
/// custom layout is saved. The clone keeps the critical section short (a stack memcpy, no drawing).
pub fn custom_layout() -> Option<Layout> {
    CUSTOM_LAYOUT.lock(|cell| cell.borrow().as_deref().cloned())
}

// --- Live on-panel preview (FEATURE_UI_BUILDER §4.3 / §7.4) --------------------------------------

/// Fired to make the render task redraw the **current** [`DisplayState`] immediately, without a
/// poll round-trip. The live preview uses it so the panel tracks editor edits in real time; unlike
/// [`SELECTION_CHANGED`] (which forces a network re-poll), this just re-runs the draw of whatever is
/// already on screen — cheap enough for the editor's debounced, high-frequency preview pushes.
pub static REDRAW: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// How long a preview push keeps the panel on the draft before auto-reverting (§4.3). Re-armed on
/// every `POST /preview`, so it only fires when pushes stop (phone locked, WiFi dropped, tab closed).
pub const PREVIEW_TTL: Duration = Duration::from_secs(15);

/// Expiry of the active preview as raw [`Instant`] ticks, or `0` when no preview is active. Kept as
/// a plain atomic so the render loop can read it without blocking.
static PREVIEW_DEADLINE: AtomicU64 = AtomicU64::new(0);

/// Transient live-preview layout, kept **separate** from [`CUSTOM_LAYOUT`] so ending a preview
/// restores the persisted layout without a re-fetch. PSRAM-boxed for the same reasons as
/// [`CUSTOM_LAYOUT`]: it must touch neither `.bss` (cpu0 stack floor) nor the DMA-capable internal
/// heap WiFi needs, so only a pointer lives in `.bss`.
static PREVIEW_LAYOUT: BlockingMutex<
    CriticalSectionRawMutex,
    RefCell<Option<Box<Layout, ExternalMemory>>>,
> = BlockingMutex::new(RefCell::new(None));

/// Push a transient preview layout to the panel and (re)arm the [`PREVIEW_TTL`] auto-revert
/// deadline. Writes no flash. The old layout is boxed/dropped outside the critical section, then the
/// render task is woken to draw the new one. Called from `POST /preview`.
pub fn set_preview(layout: Layout) {
    let boxed = Box::new_in(layout, ExternalMemory);
    let old = PREVIEW_LAYOUT.lock(|cell| cell.replace(Some(boxed)));
    PREVIEW_DEADLINE.store((Instant::now() + PREVIEW_TTL).as_ticks(), Ordering::Relaxed);
    drop(old);
    REDRAW.signal(());
}

/// End the live preview: clear the deadline and drop the transient layout so the render task reverts
/// to the persisted UI mode + layout. Idempotent — called by `POST /preview/end` **and** by the
/// render loop when the deadline lapses. Wakes the render task to redraw.
pub fn end_preview() {
    PREVIEW_DEADLINE.store(0, Ordering::Relaxed);
    let old = PREVIEW_LAYOUT.lock(|cell| cell.take());
    drop(old);
    REDRAW.signal(());
}

/// Clone the active preview layout out for the render task, or `None` when no preview is active or
/// it has expired. Cloned like [`custom_layout`] to keep the critical section short.
pub fn preview_layout() -> Option<Layout> {
    if !preview_active() {
        return None;
    }
    PREVIEW_LAYOUT.lock(|cell| cell.borrow().as_deref().cloned())
}

/// Whether a live preview is currently active and unexpired.
pub fn preview_active() -> bool {
    let dl = PREVIEW_DEADLINE.load(Ordering::Relaxed);
    dl != 0 && Instant::now().as_ticks() < dl
}

/// The active preview's expiry, or `None` if none is armed. The render loop reads it both to bound
/// its idle sleep (so it wakes in time) and to auto-revert once the deadline has passed.
pub fn preview_deadline() -> Option<Instant> {
    match PREVIEW_DEADLINE.load(Ordering::Relaxed) {
        0 => None,
        dl => Some(Instant::from_ticks(dl)),
    }
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

/// Whether the panel should turn fully off (rather than dim to 10 %) inside the reduced window.
pub fn off_when_dimmed_enabled() -> bool {
    OFF_WHEN_DIMMED.load(Ordering::Relaxed)
}

/// Which view the panel draws for the departures screen (Default / Focus / Custom).
pub fn ui_mode() -> UiMode {
    UiMode::from_u8(UI_MODE.load(Ordering::Relaxed))
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
