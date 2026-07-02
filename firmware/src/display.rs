//! HUB75 panel driver and rendering (PROJECT_BRIEF.md §7.2 / §7.7).
//!
//! Two Embassy tasks run on the **second core** (brief §7.6): [`hub75_task`] continuously
//! drives the DMA refresh, and [`render_task`] redraws the framebuffer whenever the shared
//! [`DISPLAY`](crate::shared::DISPLAY) state changes. They swap two framebuffers back and
//! forth (the standard `esp-hub75` double-buffer handshake).
//!
//! The on-panel *layout* is intentionally a clear placeholder — all drawing is isolated in
//! [`draw_state`] so it can be reworked on real hardware without touching anything else
//! (brief §7.7).

use core::fmt::Write as _;
use core::sync::atomic::{AtomicU32, Ordering};

use embassy_futures::select::{Either3, select3};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
// Latin-1 (ISO-8859-1) font variants — same glyphs/metrics as the `ascii` ones, but with
// the Western-European accented range (ä ö ü Ä Ö Ü ß …) needed for Swiss station names.
use embedded_graphics::mono_font::iso_8859_1::{FONT_5X7, FONT_6X10};
use embedded_graphics::draw_target::{DrawTarget, DrawTargetExt};
use embedded_graphics::mono_font::{MonoFont, MonoTextStyle, MonoTextStyleBuilder};
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Line, PrimitiveStyle, Rectangle};
use embedded_graphics::text::{Baseline, Text};
use embedded_graphics::Pixel;
use esp_hal::gpio::AnyPin;
use esp_hal::peripherals::{DMA_CH0, LCD_CAM};
use esp_hal::time::Rate;
use esp_hub75::framebuffer::bitplane::plain::DmaFrameBuffer;
use esp_hub75::framebuffer::compute_rows;
use esp_hub75::{Color, Hub75, Hub75Pins16};
use heapless::String;
use static_cell::StaticCell;

use crate::model::{DisplayState, Element, Layout, UiMode};
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
// Local time = UTC + the Swiss civil offset, computed per-instant with EU daylight-saving rules
// (CET = UTC+1 in winter, CEST = UTC+2 in summer). See [`swiss_offset_seconds`].
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
    let local_min = ((unix + swiss_offset_seconds(unix)).rem_euclid(86_400) / 60) as u16;
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

/// Whether `now` (minutes since local midnight) is within the `[start, end)` window, which may
/// wrap past midnight (`start > end`, e.g. 20:00→08:00). An empty `start == end` window never matches.
fn in_window(now: u16, start: u16, end: u16) -> bool {
    if start == end {
        false
    } else if start < end {
        now >= start && now < end
    } else {
        now >= start || now < end
    }
}

/// Switzerland's UTC offset (seconds) at Unix time `unix`, honouring EU daylight saving: CEST
/// (UTC+2) from 01:00 UTC on the last Sunday of March to 01:00 UTC on the last Sunday of October,
/// and CET (UTC+1) the rest of the year. Keeps the auto-dim window on wall-clock time year-round
/// (a fixed offset made summer dimming an hour late).
fn swiss_offset_seconds(unix: i64) -> i64 {
    let (year, _, _) = civil_from_days(unix.div_euclid(86_400));
    let dst_start = last_sunday_0100_utc(year, 3); // CEST begins
    let dst_end = last_sunday_0100_utc(year, 10); // CET resumes
    if unix >= dst_start && unix < dst_end { 2 * 3600 } else { 3600 }
}

/// Unix seconds for 01:00 UTC on the last Sunday of `month` in `year` — the EU DST switch instant.
/// Both DST months (March, October) have 31 days, so start from the 31st and step back to Sunday.
fn last_sunday_0100_utc(year: i64, month: u32) -> i64 {
    let last = days_from_civil(year, month, 31);
    // 1970-01-01 (day 0) was a Thursday; with 0 = Sunday that is `(days + 4) mod 7`.
    let weekday = (last + 4).rem_euclid(7);
    (last - weekday) * 86_400 + 3600
}

/// Civil date `(year, month, day)` for a count of days since the Unix epoch (Howard Hinnant's
/// algorithm). Only the year is needed here, but the full date keeps the routine self-contained.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (y + if m <= 2 { 1 } else { 0 }, m, d)
}

/// Days since the Unix epoch for civil date `(year, month, day)` (inverse of [`civil_from_days`]).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400; // [0, 399]
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// Brightness percent applied to the frame currently being drawn. Set once per frame at the top
/// of [`draw_state`] and read by [`scaled`]. Only the single render task touches it, so
/// `Relaxed` ordering is sufficient.
static RENDER_BRIGHTNESS: AtomicU32 = AtomicU32::new(REDUCED_BRIGHTNESS);

/// Scale a full-strength palette colour down to the active brightness. Applied at every draw
/// choke point ([`style`], [`rule`], [`pset`], and the badge fill) so the whole palette dims
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
                if connect_cycle_done(frame) {
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
                    if matches!(state, DisplayState::Connecting) && !connect_cycle_done(frame) {
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

// ---------------------------------------------------------------------------------------
// Drawing — PLACEHOLDER layout (brief §7.7). Keep all of it behind these helpers.
// ---------------------------------------------------------------------------------------

fn style(font: &'static embedded_graphics::mono_font::MonoFont<'static>, color: Color) -> MonoTextStyle<'static, Color> {
    MonoTextStyleBuilder::new().font(font).text_color(scaled(color)).build()
}

fn left(fb: &mut FBType, s: &str, x: i32, y: i32, st: MonoTextStyle<'static, Color>) {
    let _ = Text::with_baseline(s, Point::new(x, y), st, Baseline::Top).draw(fb);
}

/// Draw `s` horizontally centred at baseline-top `y`. `char_w` is the font's per-character
/// advance (e.g. 5 for `FONT_5X7`, 6 for `FONT_6X10`).
fn centered(fb: &mut FBType, s: &str, y: i32, st: MonoTextStyle<'static, Color>, char_w: i32) {
    let x = (COLS as i32 - s.chars().count() as i32 * char_w) / 2;
    left(fb, s, x, y, st);
}

/// Dispatch on the current state and draw it. Returns `true` if the screen is animating
/// (a scrolling title) and should be redrawn on the next frame tick.
pub fn draw_state(fb: &mut FBType, state: &DisplayState, frame: u32) -> bool {
    // Pick the brightness for this frame once; every colour is scaled to it via `scaled`.
    RENDER_BRIGHTNESS.store(current_brightness(), Ordering::Relaxed);
    match state {
        DisplayState::Provisioning => draw_provisioning(fb, frame),
        DisplayState::Connecting => draw_connecting(fb, frame),
        DisplayState::IdleAddress { octets } => draw_idle(fb, *octets, frame),
        DisplayState::Departures { station, deps } => {
            // A live preview (§4.3) forces the custom path regardless of the persisted UI mode, so
            // the user can design a Custom layout while the device is still in Default/Focus. An
            // empty preview falls back to the built-in board, same as an empty saved layout.
            if let Some(preview) = crate::shared::preview_layout() {
                if preview.e.is_empty() {
                    draw_departures(fb, station, deps, frame)
                } else {
                    draw_custom_layout(fb, &preview, station, deps, frame)
                }
            } else {
                match crate::shared::ui_mode() {
                    UiMode::Focus => draw_focus(fb, station, deps, frame),
                    UiMode::Default => draw_departures(fb, station, deps, frame),
                    // Custom draws the user's saved layout; with no (or an empty) layout it falls
                    // back to the built-in board so the panel is never blank (§7.5).
                    UiMode::Custom => match crate::shared::custom_layout() {
                        Some(layout) if !layout.e.is_empty() => {
                            draw_custom_layout(fb, &layout, station, deps, frame)
                        }
                        _ => draw_departures(fb, station, deps, frame),
                    },
                }
            }
        }
        DisplayState::Offline => draw_offline(fb, frame),
    }
}

fn draw_provisioning(fb: &mut FBType, frame: u32) -> bool {
    let big = style(&FONT_6X10, ACCENT);
    let dim = style(&FONT_5X7, DIM);
    let accent = style(&FONT_5X7, ACCENT);
    // Title, then two label/value sections: 1) join the SoftAP, 2) open the portal. The portal
    // is reachable by the mDNS name (served on the SoftAP too) with the bare IP as a fallback,
    // shown on one scrolling line. (iOS often doesn't auto-pop the captive portal, so an address
    // is shown to type in.) Each label sits tight above its value.
    left(fb, "Zugli", 2, 2, big);
    left(fb, "Join WiFi:", 2, 18, dim);
    left(fb, "Zugli-Setup", 2, 26, accent);
    left(fb, "then open:", 2, 42, dim);
    // Fixed SoftAP address, always wider than the panel, so it scrolls as a marquee. Returns
    // whether it's scrolling so the render loop keeps ticking frames.
    draw_marquee(fb, "zugli.local or 192.168.4.1", 2, 50, COLS as i32 - 2, accent, 5, frame)
}

// ---------------------------------------------------------------------------------------
// Startup "connecting" animation — a single Swiss tram rolls left→right across the panel
// while the board joins WiFi, ported from the product-site LED-board scene (one train, one
// track). The lit route blind reads "Z" (for Zügli). The render loop guarantees at least one
// full pass before it cuts over to the board, and keeps looping if the join takes longer.
// ---------------------------------------------------------------------------------------

// Tram palette — the same deep-copper + amber scheme as the rest of the panel: copper for the
// bodywork/structure (low blue so it stays saturated, not white), amber for the lit elements
// (headlight, windows, the route-blind "Z").
const T_BODY: Color = brand(0xBE, 0x56, 0x14); // deep copper body
const T_HI: Color = brand(0xDE, 0x74, 0x1C); // roof / highlight (lighter copper)
const T_DK: Color = brand(0x58, 0x26, 0x08); // skirt shadow / pantograph
const T_HEAD: Color = brand(0xFF, 0xBE, 0x3C); // headlight (warm amber)
const T_GLOW: Color = brand(0xE0, 0x86, 0x10); // headlight spill (amber)
const T_GLASS: Color = brand(0x10, 0x0A, 0x05); // dark windows
const T_LITWIN: Color = brand(0xF0, 0x9C, 0x00); // one lit window (amber)
const T_BLIND: Color = brand(0xFF, 0xA8, 0x00); // lit route-blind glyph (amber)
const T_BLIND_BG: Color = brand(0x0D, 0x09, 0x04); // route-blind background
const T_WHEEL: Color = brand(0x0A, 0x06, 0x03); // bogie wheels
const WIRE: Color = brand(0x3E, 0x24, 0x0A); // catenary (deep copper)
const RAIL: Color = brand(0x90, 0x4E, 0x16); // running rail (copper)
const SLEEP: Color = brand(0x2C, 0x16, 0x06); // sleepers

const TW: i32 = 28; // tram length in LEDs (matches the site)
const CONNECT_SPAN: i32 = COLS as i32 + TW; // travel: fully off-left → fully off-right
/// Frames for one full pass of the tram (~2.4 s at [`FRAME_MS`]).
pub const CONNECT_CYCLE_FRAMES: u32 = 48;
// The scene sits in the lower part of the panel, leaving room for the "Connecting" label up top.
const TRAIN_TOP: i32 = 34; // body-top row; wire sits above, rail below
const WIRE_Y: i32 = 28;
const RAIL_Y: i32 = 50;

/// `true` once the connecting animation has completed at least one full pass (frame numbers
/// `0..CONNECT_CYCLE_FRAMES` make up one pass, so the last frame of it is `… - 1`).
fn connect_cycle_done(frame: u32) -> bool {
    frame >= CONNECT_CYCLE_FRAMES - 1
}

/// Set a single pixel, clipped to the panel.
fn pset(fb: &mut FBType, x: i32, y: i32, c: Color) {
    if x >= 0 && y >= 0 && x < COLS as i32 && y < ROWS as i32 {
        let _ = Pixel(Point::new(x, y), scaled(c)).draw(fb);
    }
}

/// Plot a tram-local pixel for a right-running tram whose left edge is at screen x `ox`. The
/// tram is modelled nose-first (local x=0 is the nose) and mirrored so the nose leads right.
fn tp(fb: &mut FBType, ox: i32, lx: i32, ly: i32, c: Color) {
    pset(fb, ox + (TW - 1 - lx), ly, c);
}

/// One bogie (truck) with two wheelsets, at tram-local `(lx, ly)`.
fn bogie(fb: &mut FBType, ox: i32, lx: i32, ly: i32) {
    for i in 0..5 {
        tp(fb, ox, lx + i, ly, T_DK);
    }
    for &wx in &[lx + 1, lx + 3] {
        tp(fb, ox, wx, ly + 1, T_WHEEL);
        tp(fb, ox, wx, ly + 2, T_WHEEL);
    }
}

/// Draw one frame of the connecting animation. Always returns `true` (always animating).
fn draw_connecting(fb: &mut FBType, frame: u32) -> bool {
    // Label at the top so the user knows what's happening while WiFi comes up.
    centered(fb, "Connecting", 6, style(&FONT_5X7, AMBER), 5);
    draw_tram_scene(fb, frame);
    true
}

/// Draw one frame of the rolling-tram scene (catenary, rail, and the tram itself), shared by the
/// startup "connecting" screen and the offline/reconnecting fallback. The caller draws whatever
/// label sits above it (the scene fills the lower part of the panel, below `WIRE_Y`).
fn draw_tram_scene(fb: &mut FBType, frame: u32) {
    // Catenary wire above, running rail with sleepers below.
    rule(fb, WIRE_Y, WIRE);
    rule(fb, RAIL_Y, RAIL);
    let mut sx = 2;
    while sx < COLS as i32 {
        pset(fb, sx, RAIL_Y + 1, SLEEP);
        sx += 5;
    }

    // Tram x: nose enters from the left, exits on the right; one pass per cycle.
    let phase = (frame % CONNECT_CYCLE_FRAMES) as i32;
    let ox = -TW + phase * CONNECT_SPAN / CONNECT_CYCLE_FRAMES as i32;
    let top = TRAIN_TOP;

    // Body, with a sloped nose at the leading (local x=0) edge.
    for lx in 0..TW {
        let bt = match lx {
            0 => top + 4,
            1 => top + 2,
            _ => top + 1,
        };
        for ly in bt..=top + 11 {
            tp(fb, ox, lx, ly, T_BODY);
        }
    }
    for lx in 2..=TW - 2 {
        tp(fb, ox, lx, top + 1, T_HI); // roof highlight
    }
    for lx in 1..=TW - 2 {
        tp(fb, ox, lx, top + 11, T_DK); // skirt shadow
    }

    // Dark glass windows with thin mullions, plus one lit window.
    for lx in 8..TW - 2 {
        if (lx - 8) % 3 != 2 {
            for ly in top + 3..=top + 6 {
                tp(fb, ox, lx, ly, T_GLASS);
            }
        }
    }
    for ly in top + 3..=top + 5 {
        tp(fb, ox, 10, ly, T_LITWIN);
        tp(fb, ox, 11, ly, T_LITWIN);
    }

    // Front route blind: a lit "Z" (for Zügli) on a dark sign.
    const Z: [[u8; 3]; 5] = [[1, 1, 1], [0, 0, 1], [0, 1, 0], [1, 0, 0], [1, 1, 1]];
    for (gy, row) in Z.iter().enumerate() {
        for (gx, &on) in row.iter().enumerate() {
            let c = if on == 1 { T_BLIND } else { T_BLIND_BG };
            tp(fb, ox, 3 + gx as i32, top + 3 + gy as i32, c);
        }
    }

    // Headlight on the nose.
    tp(fb, ox, 0, top + 8, T_HEAD);
    tp(fb, ox, 0, top + 9, T_HEAD);
    tp(fb, ox, 0, top + 10, T_HI);
    tp(fb, ox, 1, top + 9, T_GLOW);

    // Pantograph reaching up toward the wire.
    tp(fb, ox, TW - 9, top, T_HI);
    tp(fb, ox, TW - 9, top - 1, T_DK);
    for lx in TW - 11..=TW - 7 {
        tp(fb, ox, lx, top - 2, T_DK);
    }

    // Bogies.
    bogie(fb, ox, 3, top + 12);
    bogie(fb, ox, 18, top + 12);
}

fn draw_idle(fb: &mut FBType, octets: [u8; 4], frame: u32) -> bool {
    let accent = style(&FONT_5X7, ACCENT);
    let amber = style(&FONT_5X7, AMBER);
    let dim = style(&FONT_5X7, DIM);
    // The board is on WiFi but no connection is picked yet. Lead with the call to action (too
    // wide for one line at this font, so split over two with a right arrow), then how to reach
    // the config page: mDNS name first, IP as a fallback, on one line.
    left(fb, "Choose a", 2, 2, amber);
    left(fb, "connection", 2, 12, amber);
    arrow(fb, 54, 13, ACCENT);
    left(fb, "Open:", 2, 32, dim);
    let mut addr: String<32> = String::new();
    let _ = write!(
        addr,
        "zugli.local or {}.{}.{}.{}",
        octets[0], octets[1], octets[2], octets[3]
    );
    // The combined line is always wider than the panel, so it scrolls as a marquee (clip-safe;
    // never crashes). Returns whether it's scrolling so the render loop keeps ticking frames.
    draw_marquee(fb, addr.as_str(), 2, 42, COLS as i32 - 2, accent, 5, frame)
}

/// Offline fallback: the same rolling-tram scene as [`draw_connecting`], but labelled "offline /
/// reconnecting" on two lines while the poll task keeps retrying. Always animating, so the render
/// loop keeps ticking frames (and cuts straight over once a poll succeeds).
fn draw_offline(fb: &mut FBType, frame: u32) -> bool {
    let label = style(&FONT_5X7, AMBER);
    centered(fb, "offline", 4, label, 5);
    centered(fb, "reconnecting", 13, label, 5);
    draw_tram_scene(fb, frame);
    true
}

/// Runtime departures board: the watched stop on top, then up to three upcoming departures —
/// soonest first — one per row. Each row pins the line badge on the left, scrolls the
/// destination in the space up to the time, and right-aligns the minutes-to-departure. Used for
/// both tracking modes (specific connections vs. the whole stop); only the poll-side filter
/// differs. The "hide city names" setting applies to both the stop and each destination, and the
/// "line badges" setting switches each line between a filled badge and plain text. A departure
/// leaving now shows a front-of-tram pictogram in place of the minutes. Returns `true` while
/// anything (the stop heading or a destination) is mid-scroll so the render loop keeps ticking.
fn draw_departures(
    fb: &mut FBType,
    station: &str,
    deps: &[crate::model::Departure],
    frame: u32,
) -> bool {
    // Top: the stop we're watching, scrolling if its full name is too wide.
    let scroll_station =
        draw_marquee(fb, city(station), 1, 0, COLS as i32 - 2, style(&FONT_6X10, AMBER), 6, frame);
    rule(fb, 11, ACCENT);

    if deps.is_empty() {
        // Online, but nothing tracked is departing right now (poll yields empty `deps`).
        centered(fb, "no service", 32, style(&FONT_5X7, DIM), 5);
        return scroll_station;
    }

    // Up to three departures fill the 52 px below the rule, one per row.
    const ROW_H: i32 = 17;
    const TOP: i32 = 12;
    let mut scrolling = scroll_station;
    for (i, dep) in deps.iter().take(3).enumerate() {
        let ry = TOP + i as i32 * ROW_H;
        let badge_y = ry + 3; // 11 px badge, vertically centred in the row
        let text_y = ry + 6; // FONT_5X7 baseline-top, nudged down 1 px to sit on the badge number

        // Right: time-to-departure, right-aligned to the panel edge, in copper. A departure
        // leaving now (`Some(0)`) shows a front-of-tram pictogram (as SBB does) instead of text;
        // otherwise the minutes (`--`/`N'`) are drawn as figures. The pictogram is nudged left of
        // the edge so it centres over the first-digit column of the other rows' times rather than
        // sitting flush right. `time_x` is the region's left edge, so the destination clips short
        // of it either way.
        let now = matches!(dep.minutes, Some(0));
        let mins = fmt_minutes(dep.minutes);
        let time_x = if now {
            // Centre the icon on the first glyph of a right-aligned single-digit `N'` time: that
            // 5-px cell starts at `COLS-11`, so shift by half the width difference to the icon.
            const FIRST_DIGIT_X: i32 = COLS as i32 - 1 - 2 * 5;
            FIRST_DIGIT_X + (5 - TRAIN_W) / 2
        } else {
            COLS as i32 - 1 - mins.chars().count() as i32 * 5
        };
        if now {
            draw_train_front(fb, time_x, ry + 4, ACCENT);
        } else {
            left(fb, &mins, time_x, text_y, style(&FONT_5X7, ACCENT));
        }

        // Left: the line. With badges on (default), an amber block with the digits left unlit so
        // they read as clean cut-outs; with badges off, plain amber text in the same slot.
        let badge_end = if crate::shared::line_badges_enabled() {
            draw_badge(fb, dep.line.as_str(), 1, badge_y, AMBER, OFF)
        } else {
            left(fb, dep.line.as_str(), 1, badge_y + 1, style(&FONT_6X10, AMBER));
            1 + dep.line.chars().count() as i32 * 6
        };

        // Middle: destination, clipped to the gap between the badge and the time so a long name
        // scrolls behind the minutes rather than over them.
        let dest_x = badge_end + 2;
        let dest_avail = time_x - 2 - dest_x;
        if dest_avail > 0 {
            scrolling |= draw_marquee_clipped(
                fb,
                city(dep.destination.as_str()),
                dest_x,
                text_y,
                dest_avail,
                ry,
                ROW_H,
                style(&FONT_5X7, AMBER),
                5,
                frame,
            );
        }
    }
    scrolling
}

/// Single-departure **focus view** (config `focusView`): instead of the three-row board, give the
/// whole panel to the next departure. This view intentionally omits the stop heading, so the top of
/// the panel goes straight to the next connection's line badge + destination, a large 7-segment
/// countdown of its minutes as the focal element (grown to use the reclaimed height), and a small
/// footer for the departure after it. Returns `true` while anything (the destination or the footer)
/// is mid-scroll.
fn draw_focus(
    fb: &mut FBType,
    _station: &str,
    deps: &[crate::model::Departure],
    frame: u32,
) -> bool {
    let mut scrolling = false;

    let Some(next) = deps.first() else {
        // Online, but nothing tracked is departing — same message as the board, vertically centred
        // now that there's no heading above it.
        centered(fb, "no service", 28, style(&FONT_5X7, DIM), 5);
        return scrolling;
    };

    // Identity row at the very top (no stop heading): the next departure's line (badge or plain
    // text) with its destination beside it.
    let badge_end = if crate::shared::line_badges_enabled() {
        draw_badge(fb, next.line.as_str(), 1, 1, AMBER, OFF)
    } else {
        left(fb, next.line.as_str(), 1, 2, style(&FONT_6X10, AMBER));
        1 + next.line.chars().count() as i32 * 6
    };
    let dest_x = badge_end + 2;
    let dest_avail = COLS as i32 - 1 - dest_x;
    if dest_avail > 0 {
        scrolling |= draw_marquee_clipped(
            fb,
            city(next.destination.as_str()),
            dest_x,
            4,
            dest_avail,
            1,
            11,
            style(&FONT_5X7, AMBER),
            5,
            frame,
        );
    }

    // Centre: the big countdown for the next departure — the whole point of this view, taller now
    // that it no longer shares the panel with a stop heading.
    draw_big_minutes(fb, next.minutes, 33);

    // Footer: the departure after the next one, small — "next <line> in <minutes>". Omitted when
    // only one departure is upcoming. The four coloured parts pack left-to-right with exactly one
    // space between them (never a fixed grid, so line/minute widths don't leave gaps), and the whole
    // row scrolls as one if it can't fit.
    if let Some(after) = deps.get(1) {
        let mins = fmt_minutes(after.minutes);
        let segs = [
            ("next", style(&FONT_5X7, DIM)),
            (after.line.as_str(), style(&FONT_5X7, AMBER)),
            ("in", style(&FONT_5X7, DIM)),
            (mins.as_str(), style(&FONT_5X7, ACCENT)),
        ];
        scrolling |= draw_segments_row(fb, &segs, 1, 57, COLS as i32 - 2, 5, 4, frame);
    }

    scrolling
}

// Big-number geometry for the focus view's countdown.
const BIG_DW: i32 = 15; // 7-segment digit cell width
const BIG_DH: i32 = 34; // digit cell height
const BIG_GAP: i32 = 4; // gap between digits

/// Fill an axis-aligned rectangle in `c` (scaled to the active brightness like every other draw).
fn fill_rect(fb: &mut FBType, x: i32, y: i32, w: i32, h: i32, c: Color) {
    let _ = Rectangle::new(Point::new(x, y), Size::new(w.max(0) as u32, h.max(0) as u32))
        .into_styled(PrimitiveStyle::with_fill(scaled(c)))
        .draw(fb);
}

/// Draw the next departure's minutes as a large, centred 7-segment figure whose vertical centre is
/// panel row `cy` — the focal element of the focus view. `Some(0)` (leaving now) shows the
/// front-of-tram pictogram blown up; `None` (no service) shows two large dashes.
fn draw_big_minutes(fb: &mut FBType, minutes: Option<u16>, cy: i32) {
    match minutes {
        Some(0) => {
            // Departing now: the board's front-of-tram pictogram, doubled in size and centred.
            draw_train_front_scaled(fb, (COLS as i32 - TRAIN_W * 2) / 2, cy - TRAIN_H, 2, ACCENT);
        }
        None => {
            // The board's `--`, drawn big as two centred bars.
            const T: i32 = 4;
            let total = 2 * BIG_DW + BIG_GAP;
            let mut x = (COLS as i32 - total) / 2;
            let y = cy - T / 2;
            fill_rect(fb, x, y, BIG_DW, T, DIM);
            x += BIG_DW + BIG_GAP;
            fill_rect(fb, x, y, BIG_DW, T, DIM);
        }
        Some(m) => {
            let mut buf: String<8> = String::new();
            let _ = write!(buf, "{}", m);
            let n = buf.chars().count() as i32;
            // Centre the digits themselves dead-centre on the panel; the apostrophe is appended
            // after (hanging to the right), not folded into the centred width, so the number reads
            // as centred with the marker tacked on.
            let digits_w = n * BIG_DW + (n - 1) * BIG_GAP;
            let mut x = (COLS as i32 - digits_w) / 2;
            let y = cy - BIG_DH / 2;
            for ch in buf.chars() {
                draw_seg_digit(fb, x, y, ch as u8 - b'0', AMBER);
                x += BIG_DW + BIG_GAP;
            }
            // Trailing apostrophe high on the right, echoing the board's `N'`.
            fill_rect(fb, x - BIG_GAP + 3, y, 2, 6, AMBER);
        }
    }
}

/// Draw one 7-segment digit `d` (0–9) in cell `(x, y)` sized [`BIG_DW`]×[`BIG_DH`]. Segments are
/// labelled a–g (a=top, b=top-right, c=bottom-right, d=bottom, e=bottom-left, f=top-left, g=middle).
fn draw_seg_digit(fb: &mut FBType, x: i32, y: i32, d: u8, c: Color) {
    const T: i32 = 3; // segment thickness
    // Bit per segment: a=0, b=1, c=2, d=3, e=4, f=5, g=6.
    const MASK: [u8; 10] = [
        0b0111111, // 0
        0b0000110, // 1
        0b1011011, // 2
        0b1001111, // 3
        0b1100110, // 4
        0b1101101, // 5
        0b1111101, // 6
        0b0000111, // 7
        0b1111111, // 8
        0b1101111, // 9
    ];
    let m = MASK[(d % 10) as usize];
    let on = |seg: u8| m & (1 << seg) != 0;
    let (w, h) = (BIG_DW, BIG_DH);
    let mid = y + (h - T) / 2;
    if on(0) {
        fill_rect(fb, x, y, w, T, c); // a — top
    }
    if on(6) {
        fill_rect(fb, x, mid, w, T, c); // g — middle
    }
    if on(3) {
        fill_rect(fb, x, y + h - T, w, T, c); // d — bottom
    }
    if on(5) {
        fill_rect(fb, x, y, T, h / 2, c); // f — top-left
    }
    if on(1) {
        fill_rect(fb, x + w - T, y, T, h / 2, c); // b — top-right
    }
    if on(4) {
        fill_rect(fb, x, y + h / 2, T, h / 2, c); // e — bottom-left
    }
    if on(2) {
        fill_rect(fb, x + w - T, y + h / 2, T, h / 2, c); // c — bottom-right
    }
}

// ---------------------------------------------------------------------------------------
// Custom layout renderer (FEATURE_UI_BUILDER §7.5). `draw_custom_layout` walks the user's saved
// [`Layout`] and dispatches each element on its numeric type tag `t`, reusing the board's
// primitives. Data-bound elements (station, departure fields) honour the same global config as the
// built-in board (`line_badges_enabled` / `city`), so a custom board stays in lock-step with
// Default/Focus. Font upscaling (`k ∈ 1..=3`) goes through [`blit_scaled_text`], which pixel-doubles
// the font's own glyphs so the panel matches the JS simulator glyph-for-glyph (§8.2). Everything is
// defensive: out-of-range fields are clamped and a hostile POST can never panic the render task.
// ---------------------------------------------------------------------------------------

/// Resolve an element's colour: an explicit 24-bit `col` (`0xRRGGBB`) overrides the preset index
/// `c` (0 = AMBER, 1 = ACCENT, 2 = DIM). The result still passes through `scaled()` at draw time
/// (via `pset`/`fill_rect`) like every other colour, so custom colours dim with the panel.
fn elem_color(el: &Element) -> Color {
    match el.col {
        Some(rgb) => Color::new((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8),
        None => match el.c {
            1 => ACCENT,
            2 => DIM,
            _ => AMBER,
        },
    }
}

/// The mono font for an element's `s` selector: `1` = FONT_6X10 (M), anything else = FONT_5X7 (S).
fn font_for(s: u8) -> &'static MonoFont<'static> {
    if s == 1 { &FONT_6X10 } else { &FONT_5X7 }
}

/// A tiny one-bit canvas sized to the largest glyph cell (FONT_6X10 = 6×10). A single glyph is
/// rendered into it by embedded-graphics' own rasteriser, then [`blit_scaled_text`] pixel-doubles
/// the lit cells into the framebuffer — reading the real font keeps custom text identical to the
/// built-in board (and to the simulator). Reading the font atlas via `GetPixel` would be O(atlas)
/// per pixel; this draws each glyph once instead.
struct GlyphCanvas {
    lit: [[bool; 6]; 10],
}

impl GlyphCanvas {
    fn new() -> Self {
        Self { lit: [[false; 6]; 10] }
    }
}

impl Dimensions for GlyphCanvas {
    fn bounding_box(&self) -> Rectangle {
        Rectangle::new(Point::zero(), Size::new(6, 10))
    }
}

impl DrawTarget for GlyphCanvas {
    type Color = Color;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        // The mono-font style draws only lit ("on") pixels in foreground mode, so any pixel that
        // reaches here belongs to the glyph — the colour value itself is irrelevant.
        for Pixel(p, _) in pixels {
            if (0..6).contains(&p.x) && (0..10).contains(&p.y) {
                self.lit[p.y as usize][p.x as usize] = true;
            }
        }
        Ok(())
    }
}

/// Draw `text` at baseline-top `(x0, y)` in `font`, upscaled by integer `k` (each source glyph
/// pixel becomes a `k×k` block), clipped to the horizontal band `[clip_x0, clip_x1)`. Advance per
/// glyph is `char_w × k`, matching the frozen metrics. Glyphs fully outside the clip are skipped.
#[allow(clippy::too_many_arguments)] // a layout helper: font, scale, colour and clip band all matter
fn blit_scaled_text(
    fb: &mut FBType,
    text: &str,
    x0: i32,
    y: i32,
    font: &'static MonoFont<'static>,
    k: i32,
    color: Color,
    clip_x0: i32,
    clip_x1: i32,
) {
    let cw = font.character_size.width as i32;
    let ch = font.character_size.height as i32;
    let on = MonoTextStyleBuilder::new()
        .font(font)
        .text_color(Color::new(0xFF, 0xFF, 0xFF))
        .build();
    let mut x = x0;
    for c in text.chars() {
        // Only rasterise glyphs that overlap the clip band.
        if x + cw * k > clip_x0 && x < clip_x1 {
            let mut canvas = GlyphCanvas::new();
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            let _ = Text::with_baseline(s, Point::zero(), on, Baseline::Top).draw(&mut canvas);
            for gy in 0..ch {
                for gx in 0..cw {
                    if canvas.lit[gy as usize][gx as usize] {
                        for sy in 0..k {
                            for sx in 0..k {
                                let px = x + gx * k + sx;
                                if px >= clip_x0 && px < clip_x1 {
                                    pset(fb, px, y + gy * k + sy, color);
                                }
                            }
                        }
                    }
                }
            }
        }
        x += cw * k;
    }
}

/// Draw a text-bearing element's `text` at its own `x,y`, respecting its font `s`, scale `k`,
/// alignment `a`, colour and box width `w`. If it fits within `w` (or `w == 0`, natural width) it
/// sits flush, aligned within the box; otherwise, when `allow_marquee`, it scrolls as a marquee
/// (same cadence as the board) clipped to `[x, x+w)`. Returns `true` while it is scrolling.
fn place_text(fb: &mut FBType, text: &str, el: &Element, allow_marquee: bool, frame: u32) -> bool {
    let font = font_for(el.s);
    let cw = font.character_size.width as i32;
    let k = (el.k as i32).clamp(1, 3);
    let x = el.x as i32;
    let y = el.y as i32;
    let color = elem_color(el);
    let text_w = text.chars().count() as i32 * cw * k;
    let avail = if el.w > 0 { el.w as i32 } else { text_w };
    let fits = text_w <= avail;
    if fits || !allow_marquee {
        // Flush: align within the box when it fits, else pin left and clip (non-marquee overflow).
        let off = if fits {
            match el.a {
                1 => (avail - text_w) / 2,
                2 => avail - text_w,
                _ => 0,
            }
        } else {
            0
        };
        // Clip to the box only when one is given (`w > 0`); a natural-width element is unbounded.
        let (c0, c1) = if el.w > 0 { (x, x + avail) } else { (i32::MIN, i32::MAX) };
        blit_scaled_text(fb, text, x + off, y, font, k, color, c0, c1);
        false
    } else {
        let period = text_w + MARQUEE_GAP;
        let phase = frame % (HOLD_FRAMES + period as u32);
        let offset = phase.saturating_sub(HOLD_FRAMES) as i32; // 1 px/frame after the initial hold
        blit_scaled_text(fb, text, x - offset, y, font, k, color, x, x + avail);
        blit_scaled_text(fb, text, x - offset + period, y, font, k, color, x, x + avail);
        true
    }
}

/// Render one live **Departure field** (`t=1`): look up the departure at slot `di` (soonest-first)
/// and draw its `fk` field (badge / direction / time) at the element's own `x,y`. A missing slot
/// (fewer live departures than `di+1`) draws nothing. Mirrors the built-in board's per-field logic
/// (badge honours `line_badges_enabled`; direction is city-stripped; time shows the "now" tram
/// pictogram). Returns `true` while the direction field is mid-scroll.
fn draw_dep_field(
    fb: &mut FBType,
    el: &Element,
    deps: &[crate::model::Departure],
    frame: u32,
) -> bool {
    let Some(dep) = deps.get((el.di as usize).min(2)) else {
        return false; // slot absent → draw nothing
    };
    let color = elem_color(el);
    match el.fk {
        // Badge: a filled badge when line badges are on, else the line as plain (scalable) text.
        0 => {
            if crate::shared::line_badges_enabled() {
                draw_badge(fb, dep.line.as_str(), el.x as i32, el.y as i32, color, OFF);
                false
            } else {
                place_text(fb, dep.line.as_str(), el, true, frame)
            }
        }
        // Direction: the destination, city-stripped, as a marquee clipped to `w`.
        1 => place_text(fb, city(dep.destination.as_str()), el, true, frame),
        // Time: the "now" tram pictogram (scaled by `k`) when leaving now, else the `N'`/`--` text.
        _ => match dep.minutes {
            Some(0) => {
                let k = (el.k as i32).clamp(1, 3);
                draw_train_front_scaled(fb, el.x as i32, el.y as i32, k, color);
                false
            }
            other => {
                let mins = fmt_minutes(other);
                place_text(fb, mins.as_str(), el, true, frame)
            }
        },
    }
}

/// Local civil parts `(year, month, day, hour, minute)` for the clock/date elements, or `None`
/// before SNTP has synced (the element then draws nothing). Uses the same Swiss-offset + civil-date
/// helpers as the auto-dim window, so it tracks CET/CEST year-round.
fn local_parts() -> Option<(i64, u32, u32, u32, u32)> {
    let unix = crate::shared::now_unix()?;
    let local = unix + swiss_offset_seconds(unix);
    let (y, m, d) = civil_from_days(local.div_euclid(86_400));
    let sod = local.rem_euclid(86_400);
    Some((y, m, d, (sod / 3600) as u32, (sod % 3600 / 60) as u32))
}

/// Clock element (`t=3`): the local time, `HH:MM` (`f=0`, zero-padded) or `H:MM` (`f=1`). Static —
/// never forces animation; it refreshes on the `BRIGHTNESS_REFRESH_SECS` static-screen wake.
fn draw_clock(fb: &mut FBType, el: &Element) -> bool {
    let Some((_, _, _, hh, mm)) = local_parts() else {
        return false;
    };
    let mut s: String<16> = String::new();
    match el.f {
        1 => {
            let _ = write!(s, "{}:{:02}", hh, mm);
        }
        _ => {
            let _ = write!(s, "{:02}:{:02}", hh, mm);
        }
    }
    place_text(fb, s.as_str(), el, false, 0)
}

/// Date element (`t=4`): the local date, `DD.MM.` (`f=0`) or `DD.MM.YYYY` (`f=1`). Static.
fn draw_date(fb: &mut FBType, el: &Element) -> bool {
    let Some((y, m, d, _, _)) = local_parts() else {
        return false;
    };
    let mut s: String<16> = String::new();
    match el.f {
        1 => {
            let _ = write!(s, "{:02}.{:02}.{}", d, m, y);
        }
        _ => {
            let _ = write!(s, "{:02}.{:02}.", d, m);
        }
    }
    place_text(fb, s.as_str(), el, false, 0)
}

/// Divider element (`t=5`): a horizontal bar at `y`, length `w` (or to the panel edge when `w=0`),
/// thickness `th` (1..=2). Uses `fill_rect`, which clips to the panel and scales the colour.
fn draw_divider(fb: &mut FBType, el: &Element) {
    let len = if el.w > 0 { el.w as i32 } else { COLS as i32 - el.x as i32 };
    let th = (el.th as i32).clamp(1, 2);
    fill_rect(fb, el.x as i32, el.y as i32, len, th, elem_color(el));
}

// Icon glyphs for `t=6`. The tram-front reuses [`draw_train_front_scaled`]; the Z-blind and arrow
// are small bitmaps pixel-doubled by `k` (the Z matches the connecting-animation route blind).
const Z_GLYPH: [[u8; 3]; 5] = [[1, 1, 1], [0, 0, 1], [0, 1, 0], [1, 0, 0], [1, 1, 1]];
const ARROW_GLYPH: [[u8; 7]; 5] = [
    [0, 0, 0, 0, 1, 0, 0],
    [0, 0, 0, 0, 0, 1, 0],
    [1, 1, 1, 1, 1, 1, 1],
    [0, 0, 0, 0, 0, 1, 0],
    [0, 0, 0, 0, 1, 0, 0],
];

/// Blit a small bitmap glyph with its top-left at `(x, y)`, each lit cell drawn as a `k×k` block.
fn blit_bitmap<const W: usize, const H: usize>(
    fb: &mut FBType,
    glyph: &[[u8; W]; H],
    x: i32,
    y: i32,
    k: i32,
    color: Color,
) {
    for (gy, row) in glyph.iter().enumerate() {
        for (gx, &on) in row.iter().enumerate() {
            if on == 1 {
                for sy in 0..k {
                    for sx in 0..k {
                        pset(fb, x + gx as i32 * k + sx, y + gy as i32 * k + sy, color);
                    }
                }
            }
        }
    }
}

/// Icon element (`t=6`): glyph `g` (0 = tram-front, 1 = Z-blind, 2 = arrow), scaled by `k`.
fn draw_icon(fb: &mut FBType, el: &Element) {
    let k = (el.k as i32).clamp(1, 3);
    let (x, y, color) = (el.x as i32, el.y as i32, elem_color(el));
    match el.g {
        1 => blit_bitmap(fb, &Z_GLYPH, x, y, k, color),
        2 => blit_bitmap(fb, &ARROW_GLYPH, x, y, k, color),
        _ => draw_train_front_scaled(fb, x, y, k, color),
    }
}

/// Render the user's custom layout: iterate elements in draw order (later = on top) and dispatch on
/// the numeric type tag `t`. Returns `true` while any element is mid-marquee so the render loop
/// keeps ticking frames. An unknown `t` is ignored (forward-compat, §5.5).
fn draw_custom_layout(
    fb: &mut FBType,
    layout: &Layout,
    station: &str,
    deps: &[crate::model::Departure],
    frame: u32,
) -> bool {
    let mut animating = false;
    for el in layout.e.iter() {
        animating |= match el.t {
            0 => place_text(fb, el.v.as_str(), el, true, frame), // static Text
            1 => draw_dep_field(fb, el, deps, frame),            // live Departure field
            2 => place_text(fb, city(station), el, true, frame), // Station name
            3 => draw_clock(fb, el),                             // Clock
            4 => draw_date(fb, el),                              // Date
            5 => {
                draw_divider(fb, el); // Divider
                false
            }
            6 => {
                draw_icon(fb, el); // Icon
                false
            }
            _ => false, // unknown type: ignore
        };
    }
    animating
}

/// Like [`draw_marquee`], but the text is clipped to the band `[x0, x0+avail) × [clip_top,
/// clip_top+clip_h)` so a scrolling label can't spill into neighbouring content (the badge to
/// its left or the time to its right). Returns `true` when it is scrolling.
#[allow(clippy::too_many_arguments)] // a layout helper: position, clip band, style and frame all matter
fn draw_marquee_clipped(
    fb: &mut FBType,
    text: &str,
    x0: i32,
    y: i32,
    avail: i32,
    clip_top: i32,
    clip_h: i32,
    st: MonoTextStyle<'static, Color>,
    char_w: i32,
    frame: u32,
) -> bool {
    let clip = Rectangle::new(
        Point::new(x0, clip_top),
        Size::new(avail.max(0) as u32, clip_h.max(0) as u32),
    );
    let mut target = fb.clipped(&clip);
    let text_w = text.chars().count() as i32 * char_w;
    if text_w <= avail {
        let _ = Text::with_baseline(text, Point::new(x0, y), st, Baseline::Top).draw(&mut target);
        return false;
    }
    const GAP: i32 = 14; // blank space between the end of the text and its wrapped copy
    let period = text_w + GAP;
    let phase = frame % (HOLD_FRAMES + period as u32);
    let offset = phase.saturating_sub(HOLD_FRAMES) as i32; // 1 px/frame after the initial hold
    let _ = Text::with_baseline(text, Point::new(x0 - offset, y), st, Baseline::Top).draw(&mut target);
    let _ = Text::with_baseline(text, Point::new(x0 - offset + period, y), st, Baseline::Top)
        .draw(&mut target);
    true
}

/// Draw a row of coloured text segments on one baseline at `(x0, y)`, separated by `space` px
/// between adjacent segments. If the assembled row fits within `avail` it sits flush at `x0`;
/// otherwise the whole row scrolls together as a single marquee (same cadence as [`draw_marquee`]).
/// Returns `true` while it is scrolling.
#[allow(clippy::too_many_arguments)] // a layout helper: position, spacing, width and frame all matter
fn draw_segments_row(
    fb: &mut FBType,
    segs: &[(&str, MonoTextStyle<'static, Color>)],
    x0: i32,
    y: i32,
    avail: i32,
    char_w: i32,
    space: i32,
    frame: u32,
) -> bool {
    let seg_w = |s: &str| s.chars().count() as i32 * char_w;
    let total: i32 =
        segs.iter().map(|(s, _)| seg_w(s)).sum::<i32>() + space * (segs.len() as i32 - 1).max(0);

    // Lay every segment out from `start`, advancing by its width plus one space.
    let draw_at = |fb: &mut FBType, start: i32| {
        let mut x = start;
        for &(s, st) in segs {
            left(fb, s, x, y, st);
            x += seg_w(s) + space;
        }
    };

    if total <= avail {
        draw_at(fb, x0);
        return false;
    }
    const GAP: i32 = 14; // blank space between the row and its wrapped copy
    let period = total + GAP;
    let phase = frame % (HOLD_FRAMES + period as u32);
    let offset = phase.saturating_sub(HOLD_FRAMES) as i32; // 1 px/frame after the initial hold
    draw_at(fb, x0 - offset);
    draw_at(fb, x0 - offset + period);
    true
}

/// A small right-pointing arrow (a 7×5 glyph) with its top-left at `(x, y)`.
fn arrow(fb: &mut FBType, x: i32, y: i32, c: Color) {
    for i in 0..6 {
        pset(fb, x + i, y + 2, c); // shaft
    }
    // chevron head
    pset(fb, x + 4, y, c);
    pset(fb, x + 5, y + 1, c);
    pset(fb, x + 6, y + 2, c);
    pset(fb, x + 5, y + 3, c);
    pset(fb, x + 4, y + 4, c);
}

/// Apply the user's "hide city names" setting: when enabled, drop a leading "City, " prefix so
/// only the place name shows (e.g. "Zürich, Klusplatz" → "Klusplatz"); otherwise pass through.
fn city(name: &str) -> &str {
    if crate::shared::strip_city_enabled() {
        match name.split_once(", ") {
            Some((_, rest)) if !rest.is_empty() => rest,
            _ => name,
        }
    } else {
        name
    }
}

/// Format minutes-to-departure as text: `--` (no service) or `N'`. The `Some(0)` "now" case is
/// drawn by the caller as a [`draw_train_front`] pictogram, not text, so it never reaches here in
/// practice; it still maps to `now` as a harmless fallback.
fn fmt_minutes(minutes: Option<u16>) -> String<8> {
    let mut t: String<8> = String::new();
    match minutes {
        None => {
            let _ = write!(t, "--");
        }
        Some(0) => {
            let _ = write!(t, "now");
        }
        Some(m) => {
            let _ = write!(t, "{}'", m);
        }
    }
    t
}

/// A full-width rule at row `y` in `color`.
fn rule(fb: &mut FBType, y: i32, color: Color) {
    let _ = Line::new(Point::new(0, y), Point::new(COLS as i32 - 1, y))
        .into_styled(PrimitiveStyle::with_stroke(scaled(color), 1))
        .draw(fb);
}

/// Width and height of the [`draw_train_front`] pictogram, in pixels.
const TRAIN_W: i32 = 9;
const TRAIN_H: i32 = 10;

/// The "departing now" front-of-tram pictogram (9×10): a rounded roof, a dark **destination-blind
/// slot** framed by a lit rim — the horizontal line above the windscreen that keeps it reading as a
/// tram front rather than a ghost — then two cab windows, the body, and two wheels. Same idea as
/// SBB's imminent-departure icon.
const TRAIN_GLYPH: [[u8; 9]; 10] = [
    [0, 0, 1, 1, 1, 1, 1, 0, 0], // rounded roof top
    [0, 1, 1, 1, 1, 1, 1, 1, 0], // roof
    [1, 1, 1, 1, 1, 1, 1, 1, 1], // roof front
    [1, 0, 0, 0, 0, 0, 0, 0, 1], // destination-blind slot (the line above the windows)
    [1, 1, 1, 1, 1, 1, 1, 1, 1], // lit rim under the blind
    [1, 0, 0, 0, 1, 0, 0, 0, 1], // windows, split by the centre pillar
    [1, 0, 0, 0, 1, 0, 0, 0, 1],
    [1, 1, 1, 1, 1, 1, 1, 1, 1], // body
    [1, 1, 1, 1, 1, 1, 1, 1, 1],
    [0, 1, 1, 0, 0, 0, 1, 1, 0], // wheels
];

/// Draw the front-of-tram pictogram with its top-left at `(x, y)` in `c`.
fn draw_train_front(fb: &mut FBType, x: i32, y: i32, c: Color) {
    draw_train_front_scaled(fb, x, y, 1, c);
}

/// Draw the front-of-tram pictogram blown up by an integer `scale` (each lit cell becomes a
/// `scale`×`scale` block), top-left at `(x, y)`. `scale == 1` is the board's pictogram; the focus
/// view uses `2` for the large "departing now" state.
fn draw_train_front_scaled(fb: &mut FBType, x: i32, y: i32, scale: i32, c: Color) {
    for (gy, row) in TRAIN_GLYPH.iter().enumerate() {
        for (gx, &on) in row.iter().enumerate() {
            if on == 1 {
                for sy in 0..scale {
                    for sx in 0..scale {
                        pset(fb, x + gx as i32 * scale + sx, y + gy as i32 * scale + sy, c);
                    }
                }
            }
        }
    }
}

/// Draw a filled badge holding the line label, top-left at `(x, y)`: `fill` background with
/// `text` colour. Sized to the label so any length fits. Returns the x just past the badge.
fn draw_badge(fb: &mut FBType, line: &str, x: i32, y: i32, fill: Color, text: Color) -> i32 {
    let w = line.chars().count() as i32 * 6 + 5;
    let _ = Rectangle::new(Point::new(x, y), Size::new(w as u32, 11))
        .into_styled(PrimitiveStyle::with_fill(scaled(fill)))
        .draw(fb);
    left(fb, line, x + 3, y + 1, style(&FONT_6X10, text));
    x + w
}

/// Draw `text` at baseline-top `(x0, y)`. If it fits within `avail` pixels it sits flush at
/// `x0`; otherwise it scrolls as a seamless marquee — paused ~5 s at the start, then one full
/// round, repeat. Returns `true` when it is scrolling (so the caller keeps ticking frames).
#[allow(clippy::too_many_arguments)] // a layout helper: position, width, style and frame all matter
fn draw_marquee(
    fb: &mut FBType,
    text: &str,
    x0: i32,
    y: i32,
    avail: i32,
    st: MonoTextStyle<'static, Color>,
    char_w: i32,
    frame: u32,
) -> bool {
    let text_w = text.chars().count() as i32 * char_w;
    if text_w <= avail {
        left(fb, text, x0, y, st);
        return false;
    }
    const GAP: i32 = 14; // blank space between the end of the text and its wrapped copy
    let period = text_w + GAP;
    let phase = frame % (HOLD_FRAMES + period as u32);
    // 1 px per frame once the initial hold has elapsed.
    let offset = phase.saturating_sub(HOLD_FRAMES) as i32;
    left(fb, text, x0 - offset, y, st);
    left(fb, text, x0 - offset + period, y, st);
    true
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
