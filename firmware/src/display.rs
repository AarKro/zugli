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

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer};
// Latin-1 (ISO-8859-1) font variants — same glyphs/metrics as the `ascii` ones, but with
// the Western-European accented range (ä ö ü Ä Ö Ü ß …) needed for Swiss station names.
use embedded_graphics::mono_font::iso_8859_1::{FONT_5X7, FONT_6X10, FONT_9X15};
use embedded_graphics::mono_font::{MonoTextStyle, MonoTextStyleBuilder};
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

use crate::model::DisplayState;
use crate::shared::DISPLAY;

pub const ROWS: usize = 64;
pub const COLS: usize = 64;
const NROWS: usize = compute_rows(ROWS);
const PLANES: usize = 7;

// BRIGHTNESS — the only real dimmer available: the HUB75 driver has no brightness register, so
// brightness is purely the RGB values we write (Binary Code Modulation). The palette below is
// defined at FULL strength and scaled down at draw time by [`scaled`], using a brightness that
// steps between a bright daytime and a dim night-time level from the wall clock.
const DAY_BRIGHTNESS: u32 = 60; // 08:00–20:00 local: bright enough for daylight
const NIGHT_BRIGHTNESS: u32 = 10; // 20:00–08:00 local: dim so it isn't glaring at night
const DAY_START_HOUR: u32 = 8;
const DAY_END_HOUR: u32 = 20;
// Local time = UTC + this offset. Switzerland is UTC+1 (CET); set to 2 for summer time (CEST).
// DST is not auto-handled, so in summer the day/night boundaries simply shift by an hour — fine
// for a coarse 12-hour window.
const LOCAL_UTC_OFFSET_HOURS: i64 = 1;
// How often a static screen is redrawn so its brightness still tracks the day/night switch.
const BRIGHTNESS_REFRESH_SECS: u64 = 60;

/// The brightness percent to use right now, from the synced wall clock. Falls back to daytime
/// brightness before SNTP has synced (e.g. during the boot animation) so the panel is visible.
fn current_brightness() -> u32 {
    match crate::shared::now_unix() {
        Some(unix) => {
            let hour = ((unix + LOCAL_UTC_OFFSET_HOURS * 3600).rem_euclid(86_400) / 3600) as u32;
            if (DAY_START_HOUR..DAY_END_HOUR).contains(&hour) {
                DAY_BRIGHTNESS
            } else {
                NIGHT_BRIGHTNESS
            }
        }
        None => DAY_BRIGHTNESS,
    }
}

/// Brightness percent applied to the frame currently being drawn. Set once per frame at the top
/// of [`draw_state`] and read by [`scaled`]. Only the single render task touches it, so
/// `Relaxed` ordering is sufficient.
static RENDER_BRIGHTNESS: AtomicU32 = AtomicU32::new(DAY_BRIGHTNESS);

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

/// Brand copper (#B87648) (brief §7.7 — not a generic "yellow").
pub const ACCENT: Color = brand(0xB8, 0x76, 0x48);
const DIM: Color = brand(0x5C, 0x55, 0x4C); // --muted, secondary text
const CREAM: Color = brand(0xF5, 0xEF, 0xE6); // --cream, primary text
const SURFACE: Color = Color::new(0x0E, 0x0C, 0x0A); // --surface, dark text on the copper badge

// Animation cadence for the scrolling title. The render task redraws at ~20 fps while a
// title needs scrolling; `HOLD_FRAMES` is the pause (~5 s) before and after each round.
const FRAME_MS: u64 = 50;
const HOLD_FRAMES: u32 = 100;

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
                // A static screen blocks for the next state, but also wakes periodically so it
                // is redrawn at the current brightness when the day/night threshold is crossed.
                match select(
                    Timer::after(Duration::from_secs(BRIGHTNESS_REFRESH_SECS)),
                    DISPLAY.wait(),
                )
                .await
                {
                    Either::First(_) => {}
                    Either::Second(next) => state = next,
                }
                break;
            }
            match select(Timer::after(Duration::from_millis(FRAME_MS)), DISPLAY.wait()).await {
                Either::First(_) => frame = frame.wrapping_add(1),
                Either::Second(next) => {
                    // Hold the switch until the tram has finished a full pass; cut over
                    // immediately for every other state.
                    if matches!(state, DisplayState::Connecting) && !connect_cycle_done(frame) {
                        pending = Some(next);
                    } else {
                        state = next;
                        break;
                    }
                }
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

/// Dispatch on the current state and draw it. Returns `true` if the screen is animating
/// (a scrolling title) and should be redrawn on the next frame tick.
pub fn draw_state(fb: &mut FBType, state: &DisplayState, frame: u32) -> bool {
    // Pick the brightness for this frame once; every colour is scaled to it via `scaled`.
    RENDER_BRIGHTNESS.store(current_brightness(), Ordering::Relaxed);
    match state {
        DisplayState::Provisioning => {
            draw_provisioning(fb);
            false
        }
        DisplayState::Connecting => draw_connecting(fb, frame),
        DisplayState::IdleAddress { octets } => {
            draw_idle(fb, *octets);
            false
        }
        DisplayState::Departures { station, deps } => draw_departures(fb, station, deps, frame),
        DisplayState::Offline => {
            draw_offline(fb);
            false
        }
    }
}

fn draw_provisioning(fb: &mut FBType) {
    let big = style(&FONT_6X10, ACCENT);
    let dim = style(&FONT_5X7, DIM);
    let accent = style(&FONT_5X7, ACCENT);
    // 1) Join the SoftAP, 2) open the portal address. iOS often doesn't auto-pop the
    // captive portal, so we show the address to type in manually on the bottom row.
    left(fb, "Zugli", 2, 1, big);
    left(fb, "Join WiFi:", 2, 15, dim);
    left(fb, "Zugli-Setup", 2, 24, accent);
    left(fb, "then open:", 2, 40, dim);
    // Bottom row — the SoftAP address (fixed at 192.168.4.1, PROJECT_BRIEF.md §5.2).
    left(fb, "192.168.4.1", 2, 55, accent);
}

// ---------------------------------------------------------------------------------------
// Startup "connecting" animation — a single Swiss tram rolls left→right across the panel
// while the board joins WiFi, ported from the product-site LED-board scene (one train, one
// track). The lit route blind reads "Z" (for Zügli). The render loop guarantees at least one
// full pass before it cuts over to the board, and keeps looping if the join takes longer.
// ---------------------------------------------------------------------------------------

// Tram palette (full-strength RGB, scaled by the global brightness), matching the site's
// warm copper / cream scene.
const T_BODY: Color = brand(0xDB, 0x8C, 0x52); // copper body
const T_HI: Color = brand(0xF7, 0xA8, 0x66); // roof / highlight
const T_DK: Color = brand(0x6B, 0x40, 0x21); // skirt shadow / pantograph
const T_WHITE: Color = brand(0xFF, 0xFA, 0xEB); // headlight
const T_GLOW: Color = brand(0xF2, 0xD9, 0x99); // headlight spill
const T_GLASS: Color = brand(0x12, 0x0E, 0x0A); // dark windows
const T_LITWIN: Color = brand(0xD9, 0xC7, 0x8C); // one lit window
const T_BLIND: Color = brand(0xFF, 0xD1, 0x6B); // lit route-blind glyph
const T_BLIND_BG: Color = brand(0x0D, 0x0A, 0x08); // route-blind background
const T_WHEEL: Color = brand(0x0A, 0x08, 0x05); // bogie wheels
const WIRE: Color = brand(0x4D, 0x38, 0x29); // catenary
const RAIL: Color = brand(0x8C, 0x66, 0x42); // running rail
const SLEEP: Color = brand(0x33, 0x21, 0x14); // sleepers

const TW: i32 = 28; // tram length in LEDs (matches the site)
const CONNECT_SPAN: i32 = COLS as i32 + TW; // travel: fully off-left → fully off-right
/// Frames for one full pass of the tram (~2.4 s at [`FRAME_MS`]).
pub const CONNECT_CYCLE_FRAMES: u32 = 48;
const TRAIN_TOP: i32 = 26; // body-top row; wire sits above, rail below
const WIRE_Y: i32 = 20;
const RAIL_Y: i32 = 42;

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
    tp(fb, ox, 0, top + 8, T_WHITE);
    tp(fb, ox, 0, top + 9, T_WHITE);
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

    true
}

fn draw_idle(fb: &mut FBType, octets: [u8; 4]) {
    let accent = style(&FONT_5X7, ACCENT);
    let dim = style(&FONT_5X7, DIM);
    left(fb, "Open", 2, 4, dim);
    left(fb, "zugli", 2, 16, accent);
    left(fb, ".local", 2, 26, accent);
    left(fb, "or IP:", 2, 40, dim);
    let mut ip: String<16> = String::new();
    let _ = write!(ip, "{}.{}", octets[0], octets[1]);
    left(fb, ip.as_str(), 2, 50, accent);
    let mut ip2: String<16> = String::new();
    let _ = write!(ip2, "{}.{}", octets[2], octets[3]);
    left(fb, ip2.as_str(), 2, 58, accent);
}

fn draw_offline(fb: &mut FBType) {
    let dim = style(&FONT_6X10, DIM);
    left(fb, "offline", 2, 28, dim);
}

/// Runtime departures screen. A header band — copper rule, the station name, copper rule —
/// sits at the top; below it the connection (destination + a copper line badge) and the next
/// two departure times. Returns `true` while a heading is mid-scroll so the loop keeps
/// animating. Colours are the Zügli palette: cream for primary text, copper for accents.
fn draw_departures(
    fb: &mut FBType,
    station: &str,
    deps: &[crate::model::Departure],
    frame: u32,
) -> bool {
    if deps.is_empty() {
        draw_offline(fb);
        return false;
    }

    // Header band: the station name (title) framed by a rule above and below it, in copper.
    rule(fb, 0, ACCENT);
    let station_name = strip_city(station);
    let scroll_station = draw_marquee(fb, station_name, 2, style(&FONT_6X10, ACCENT), 6, frame);
    rule(fb, 13, ACCENT);

    // Subtitle: where the saved line is heading (city prefix dropped), smaller, in cream.
    let dest = strip_city(deps[0].destination.as_str());
    let scroll_dest = draw_marquee(fb, dest, 15, style(&FONT_5X7, CREAM), 5, frame);

    // Line label on its OWN full-width row, so it has room regardless of length — a tram
    // number ("2"), a train ("S12", "IC"), or a bus ("N13") all fit here, where they didn't
    // when squeezed next to the time. Copper badge, dark text.
    draw_badge(fb, deps[0].line.as_str(), 1, 22, ACCENT, SURFACE);

    // The next two departures, large, in copper.
    left(fb, &fmt_minutes(deps[0].minutes), 2, 35, style(&FONT_9X15, ACCENT));
    if let Some(next) = deps.get(1) {
        left(fb, &fmt_minutes(next.minutes), 2, 50, style(&FONT_9X15, ACCENT));
    }

    scroll_station || scroll_dest
}

/// Format minutes-to-departure as the panel shows it: `--` (no service), `now`, or `N'`.
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

/// Draw `text` at baseline-top `y`. If it fits, it sits flush left; otherwise it scrolls as
/// a seamless marquee — paused ~5 s at the default position, then one full round, repeat.
/// Returns `true` when it is scrolling (so the caller keeps ticking frames).
fn draw_marquee(
    fb: &mut FBType,
    text: &str,
    y: i32,
    st: MonoTextStyle<'static, Color>,
    char_w: i32,
    frame: u32,
) -> bool {
    let text_w = text.chars().count() as i32 * char_w;
    if text_w <= COLS as i32 - 1 {
        left(fb, text, 1, y, st);
        return false;
    }
    const GAP: i32 = 14; // blank space between the end of the text and its wrapped copy
    let period = text_w + GAP;
    let phase = frame % (HOLD_FRAMES + period as u32);
    // 1 px per frame once the initial hold has elapsed.
    let offset = phase.saturating_sub(HOLD_FRAMES) as i32;
    left(fb, text, 1 - offset, y, st);
    left(fb, text, 1 - offset + period, y, st);
    true
}

/// Drop a leading "City, " prefix from a destination so only the place name is shown
/// (e.g. "Zürich, Klusplatz" → "Klusplatz"). Names without that prefix are left untouched.
fn strip_city(dest: &str) -> &str {
    match dest.split_once(", ") {
        Some((_, rest)) if !rest.is_empty() => rest,
        _ => dest,
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
