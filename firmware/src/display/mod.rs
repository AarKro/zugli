//! HUB75 panel driver and rendering (PROJECT_BRIEF.md §7.2 / §7.7).
//!
//! Two Embassy tasks run on the **second core** (brief §7.6): [`hub75_task`] continuously
//! drives the DMA refresh, and [`render_task`] redraws the framebuffer whenever the shared
//! [`DISPLAY`](crate::shared::DISPLAY) state changes. They swap two framebuffers back and
//! forth (the standard `esp-hub75` double-buffer handshake).
//!
//! Drawing is split over three submodules, all dispatched from [`draw_state`]:
//! * [`draw`] — the shared primitives (text, marquees, badges, pixel-art blits),
//! * [`screens`] — the built-in screens (provisioning, connecting/offline tram, idle,
//!   departures board, focus view),
//! * [`custom`] — the user's custom-layout renderer (FEATURE_UI_BUILDER §7.5).
//!
//! This file owns the hardware side (tasks, framebuffers) plus the frame-global concerns
//! every drawer shares: the brightness pipeline ([`scaled`]) and the marquee cadence.

mod custom;
mod draw;
mod screens;

pub use screens::CONNECT_CYCLE_FRAMES;

use core::sync::atomic::{AtomicU32, Ordering};

use embassy_futures::select::{Either3, select3};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use embedded_graphics::prelude::*;
use esp_hal::gpio::AnyPin;
use esp_hal::peripherals::{DMA_CH0, LCD_CAM};
use esp_hal::time::Rate;
use esp_hub75::framebuffer::bitplane::plain::DmaFrameBuffer;
use esp_hub75::framebuffer::compute_rows;
use esp_hub75::{Color, Hub75, Hub75Pins16};
use static_cell::StaticCell;

use crate::localtime::{in_window, local_minutes};
use crate::model::{DisplayState, UiMode};
use crate::shared::DISPLAY;

pub const ROWS: usize = 64;
pub const COLS: usize = 64;
const NROWS: usize = compute_rows(ROWS);
const PLANES: usize = 7;

// BRIGHTNESS — the only real dimmer available: the HUB75 driver has no brightness register, so
// brightness is purely the RGB values we write (Binary Code Modulation). The palette below is
// defined at FULL strength and scaled down at draw time by [`scaled`]. The active percent comes
// from the user's settings: a manual 1–10 level (× 10 %), optionally auto-dimmed to
// `REDUCED_BRIGHTNESS` during a configurable local-time window.
const REDUCED_BRIGHTNESS: u32 = 10; // the dimmed level used inside the auto-dim window
// How often a static screen is redrawn so its brightness still tracks the auto-dim window.
const BRIGHTNESS_REFRESH_SECS: u64 = 60;

/// The brightness percent to use right now. The manual level always applies; when auto-dim is
/// on and the synced local time is inside the reduced window, drop to [`REDUCED_BRIGHTNESS`].
/// Before SNTP has synced (e.g. the boot animation) we can't know the time, so we stay manual.
fn current_brightness() -> u32 {
    let base = (crate::shared::brightness_level().clamp(1, 10) as u32) * 10; // 10..=100
    if !crate::shared::auto_brightness_enabled() {
        return base;
    }
    let Some(unix) = crate::shared::now_unix() else {
        return base;
    };
    let local_min = local_minutes(unix);
    if in_window(local_min, crate::shared::reduced_start_min(), crate::shared::reduced_end_min()) {
        // Inside the window: either dim to the low level, or turn the panel fully off (0 %, every
        // colour scales to black) when the user opted for that.
        if crate::shared::off_when_dimmed_enabled() {
            0
        } else {
            REDUCED_BRIGHTNESS
        }
    } else {
        base
    }
}

/// Brightness percent applied to the frame currently being drawn. Set once per frame at the top
/// of [`draw_state`] and read by [`scaled`]. Only the single render task touches it, so
/// `Relaxed` ordering is sufficient.
static RENDER_BRIGHTNESS: AtomicU32 = AtomicU32::new(REDUCED_BRIGHTNESS);

/// Scale a full-strength palette colour down to the active brightness. Applied at every draw
/// choke point (`style`, `rule`, `pset`, and the badge fill) so the whole palette dims
/// uniformly with the time of day.
fn scaled(c: Color) -> Color {
    let pct = RENDER_BRIGHTNESS.load(Ordering::Relaxed);
    let s = |ch: u8| ((ch as u32 * pct) / 100) as u8;
    Color::new(s(c.r()), s(c.g()), s(c.b()))
}

/// A Zügli brand colour at full strength (dimmed to the active brightness when drawn).
const fn brand(r: u8, g: u8, b_: u8) -> Color {
    Color::new(r, g, b_)
}

// Deep, saturated copper — kept dark and low-blue on purpose: on the HUB75 panel any colour
// with the blue/green channels riding high washes out to white, so the palette stays heavily
// red-weighted to read as real copper rather than a pale tan.
pub const ACCENT: Color = brand(0xAA, 0x4A, 0x10); // deep copper — primary accent / structure
const AMBER: Color = brand(0xFF, 0xA8, 0x00); // departure-board amber — primary readable text
const DIM: Color = brand(0x74, 0x4A, 0x1E); // muted copper — secondary text / labels
const OFF: Color = Color::new(0, 0, 0); // fully unlit — LEDs stay dark (e.g. badge digit cut-outs)

// Animation cadence for the scrolling title. The render task redraws at ~20 fps while a
// title needs scrolling; `HOLD_FRAMES` is the pause (~5 s) before and after each round.
const FRAME_MS: u64 = 50;
const HOLD_FRAMES: u32 = 100;
/// Blank space (px) between a marquee's text and its wrapped copy — the same value baked into
/// every built-in marquee helper (`draw_marquee` etc.), shared by the custom renderer so scrolling
/// custom text matches the board and the JS simulator (§8.2).
const MARQUEE_GAP: i32 = 14;

/// Scroll offset (px) of a marquee at `frame`: held at 0 for the initial [`HOLD_FRAMES`] pause,
/// then 1 px per frame through one full `period` (content width + [`MARQUEE_GAP`]), then repeats.
/// The single cadence shared by every scrolling drawer, so all marquees move in lock-step.
fn marquee_offset(period: i32, frame: u32) -> i32 {
    let phase = frame % (HOLD_FRAMES + period as u32);
    phase.saturating_sub(HOLD_FRAMES) as i32
}

pub type FBType = DmaFrameBuffer<NROWS, COLS, PLANES>;
type Hub75Type = Hub75<'static, esp_hal::Async>;
/// One-slot channel used to hand a framebuffer between the two display tasks.
pub type FrameBufferExchange = Signal<CriticalSectionRawMutex, &'static mut FBType>;

/// All the GPIOs + peripherals the HUB75 driver needs. Pins match brief §3.1.
pub struct Hub75Peripherals {
    pub lcd_cam: LCD_CAM<'static>,
    pub dma_channel: DMA_CH0<'static>,
    pub red1: AnyPin<'static>,
    pub grn1: AnyPin<'static>,
    pub blu1: AnyPin<'static>,
    pub red2: AnyPin<'static>,
    pub grn2: AnyPin<'static>,
    pub blu2: AnyPin<'static>,
    pub addr0: AnyPin<'static>,
    pub addr1: AnyPin<'static>,
    pub addr2: AnyPin<'static>,
    pub addr3: AnyPin<'static>,
    pub addr4: AnyPin<'static>,
    pub blank: AnyPin<'static>,
    pub clock: AnyPin<'static>,
    pub latch: AnyPin<'static>,
}

/// Continuously refresh the panel over DMA, swapping in a new framebuffer whenever the
/// render task offers one. Runs as a high-priority task on the second core.
#[embassy_executor::task]
pub async fn hub75_task(
    peripherals: Hub75Peripherals,
    rx: &'static FrameBufferExchange,
    tx: &'static FrameBufferExchange,
    fb: &'static mut FBType,
) {
    let channel = peripherals.dma_channel;
    let tx_descriptors = esp_hub75::hub75_dma_descriptors!(FBType);

    let pins = Hub75Pins16 {
        red1: peripherals.red1,
        grn1: peripherals.grn1,
        blu1: peripherals.blu1,
        red2: peripherals.red2,
        grn2: peripherals.grn2,
        blu2: peripherals.blu2,
        addr0: peripherals.addr0,
        addr1: peripherals.addr1,
        addr2: peripherals.addr2,
        addr3: peripherals.addr3,
        addr4: peripherals.addr4,
        blank: peripherals.blank,
        clock: peripherals.clock,
        latch: peripherals.latch,
    };

    let mut hub75 = Hub75Type::new_async(
        peripherals.lcd_cam,
        pins,
        channel,
        tx_descriptors,
        Rate::from_mhz(20),
    )
    .expect("failed to create Hub75");

    // Initial buffer handshake with the render task.
    let mut fb = fb;
    let new_fb = rx.wait().await;
    tx.signal(fb);
    fb = new_fb;

    loop {
        if rx.signaled() {
            let new_fb = rx.wait().await;
            tx.signal(fb);
            fb = new_fb;
        }

        // Never panic on a transient DMA hiccup — that would reset the whole device. A
        // failed start or transfer just means we skip this frame and try again; the panel
        // keeps running. (This path is exercised heavily once a title/subtitle scrolls.)
        let mut xfer = match hub75.render(fb) {
            Ok(xfer) => xfer,
            Err((_, recovered)) => {
                hub75 = recovered;
                continue;
            }
        };
        let _ = xfer.wait_for_done().await;
        let (result, new_hub75) = xfer.wait();
        hub75 = new_hub75;
        if result.is_err() {
            continue;
        }
    }
}

/// Redraw the framebuffer whenever [`DISPLAY`] changes; runs as a low-priority task on the
/// second core. Blocks between updates, so the panel is otherwise idle (brief §7.6 pt. 3).
#[embassy_executor::task]
pub async fn render_task(
    rx: &'static FrameBufferExchange,
    tx: &'static FrameBufferExchange,
    fb: &'static mut FBType,
) {
    let mut fb = fb;
    let mut state = DISPLAY.wait().await;
    loop {
        // Render frames of the current state. A static screen is drawn once and then we
        // block for the next state; an animated one (a scrolling title) advances a frame
        // every `FRAME_MS`, but cuts over immediately if a new state arrives.
        //
        // The `Connecting` startup animation is the exception: it must play at least one
        // full pass before switching to the board (or back to the portal). An incoming
        // state is parked in `pending` and only applied once the tram has cleared the panel.
        let mut frame: u32 = 0;
        let mut pending: Option<DisplayState> = None;
        loop {
            // Auto-revert an expired live preview before drawing, so the panel returns to the
            // persisted UI mode + layout even without an explicit `/preview/end` (phone locked,
            // WiFi dropped, tab closed). `preview_layout()` then reads back `None` for this frame.
            if let Some(dl) = crate::shared::preview_deadline()
                && Instant::now() >= dl
            {
                crate::shared::end_preview();
            }
            fb.erase();
            let animating = draw_state(fb, &state, frame);
            tx.signal(fb);
            fb = rx.wait().await;
            // A state arrived earlier while the connecting animation was mid-pass — apply it
            // as soon as that pass completes (and keep looping the tram until then).
            if let Some(next) = pending.take() {
                if screens::connect_cycle_done(frame) {
                    state = next;
                    break;
                }
                pending = Some(next);
            }
            if !animating {
                // A static screen blocks for the next state, but also wakes periodically so it is
                // redrawn at the current brightness when the day/night threshold is crossed — and
                // sooner if a live preview is about to expire (so the deadline check above fires on
                // time) or a `REDRAW` (a preview push) arrives.
                let refresh = Instant::now() + Duration::from_secs(BRIGHTNESS_REFRESH_SECS);
                let wake_at = crate::shared::preview_deadline().map_or(refresh, |d| d.min(refresh));
                match select3(Timer::at(wake_at), DISPLAY.wait(), crate::shared::REDRAW.wait()).await
                {
                    Either3::First(_) => {}           // refresh / preview deadline → redraw state
                    Either3::Second(next) => state = next,
                    Either3::Third(_) => {}           // preview push → redraw the current state
                }
                break;
            }
            match select3(
                Timer::after(Duration::from_millis(FRAME_MS)),
                DISPLAY.wait(),
                crate::shared::REDRAW.wait(),
            )
            .await
            {
                Either3::First(_) => frame = frame.wrapping_add(1),
                Either3::Second(next) => {
                    // Hold the switch until the tram has finished a full pass; cut over
                    // immediately for every other state.
                    if matches!(state, DisplayState::Connecting) && !screens::connect_cycle_done(frame) {
                        pending = Some(next);
                    } else {
                        state = next;
                        break;
                    }
                }
                Either3::Third(_) => {} // preview push → redraw the current frame with the new mirror
            }
        }
    }
}

/// Dispatch on the current state and draw it. Returns `true` if the screen is animating
/// (a scrolling title) and should be redrawn on the next frame tick.
pub fn draw_state(fb: &mut FBType, state: &DisplayState, frame: u32) -> bool {
    // Pick the brightness for this frame once; every colour is scaled to it via `scaled`.
    RENDER_BRIGHTNESS.store(current_brightness(), Ordering::Relaxed);
    match state {
        DisplayState::Provisioning => screens::draw_provisioning(fb, frame),
        DisplayState::Connecting => screens::draw_connecting(fb, frame),
        DisplayState::IdleAddress { octets } => screens::draw_idle(fb, *octets, frame),
        DisplayState::Departures { station, deps } => {
            // A live preview (§4.3) forces the custom path regardless of the persisted UI mode, so
            // the user can design a Custom layout while the device is still in Default/Focus. An
            // empty preview falls back to the built-in board, same as an empty saved layout.
            if let Some(preview) = crate::shared::preview_layout() {
                if preview.e.is_empty() {
                    screens::draw_departures(fb, station, deps, frame)
                } else {
                    custom::draw_custom_layout(fb, &preview, station, deps, frame)
                }
            } else {
                match crate::shared::ui_mode() {
                    UiMode::Focus => screens::draw_focus(fb, station, deps, frame),
                    UiMode::Default => screens::draw_departures(fb, station, deps, frame),
                    // Custom draws the user's saved layout; with no (or an empty) layout it falls
                    // back to the built-in board so the panel is never blank (§7.5).
                    UiMode::Custom => match crate::shared::custom_layout() {
                        Some(layout) if !layout.e.is_empty() => {
                            custom::draw_custom_layout(fb, &layout, station, deps, frame)
                        }
                        _ => screens::draw_departures(fb, station, deps, frame),
                    },
                }
            }
        }
        DisplayState::Offline => screens::draw_offline(fb, frame),
    }
}

/// Allocate the two framebuffers as `'static` and return them. Call once.
///
/// Each `FBType` is 28 KB (`PLANES·NROWS·COLS·2`). Building it via `mk_static!(FBType,
/// FBType::new())` materialises that 28 KB as a temporary on main's stack before the
/// memcpy into the static cell — which overflows main's (ProCpu) stack at boot (the panic
/// shows a 28 672-byte memcpy past the stack guard). `FBType::new()` isn't `const`, so we
/// can't `ConstStaticCell` it. Instead we construct each buffer **in place** in `.bss`:
/// an all-zero `FBType` is exactly what `new()` builds before it calls `format()` (every
/// `Entry` is `Entry(0)`), so we zero the slot in place — no stack temporary — then run
/// `format()`/`erase()` through the `&mut` reference.
pub fn framebuffers() -> (&'static mut FBType, &'static mut FBType) {
    fn init(cell: &'static StaticCell<FBType>) -> &'static mut FBType {
        let slot = cell.uninit();
        // SAFETY: `FBType` is `[[Row { [Entry(0); COLS] }; NROWS]; PLANES]` — all-zero bytes
        // are a valid, fully-initialised value (the pre-`format()` state of `new()`).
        let fb = unsafe {
            slot.as_mut_ptr().write_bytes(0, 1);
            slot.assume_init_mut()
        };
        fb.format(); // populate the per-row address/latch/OE control bits
        fb.erase(); // clear pixel colours (already zero; kept for parity with `new()`)
        fb
    }
    static FB0: StaticCell<FBType> = StaticCell::new();
    static FB1: StaticCell<FBType> = StaticCell::new();
    (init(&FB0), init(&FB1))
}

/// Allocate the two framebuffer-exchange signals as `'static`. Call once.
pub fn exchanges() -> (&'static FrameBufferExchange, &'static FrameBufferExchange) {
    let tx = crate::mk_static!(FrameBufferExchange, FrameBufferExchange::new());
    let rx = crate::mk_static!(FrameBufferExchange, FrameBufferExchange::new());
    (tx, rx)
}
