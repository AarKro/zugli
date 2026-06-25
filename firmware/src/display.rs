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

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embedded_graphics::mono_font::ascii::{FONT_5X7, FONT_6X10};
use embedded_graphics::mono_font::{MonoTextStyle, MonoTextStyleBuilder};
use embedded_graphics::prelude::*;
use embedded_graphics::text::{Baseline, Text};
use esp_hal::gpio::AnyPin;
use esp_hal::peripherals::{DMA_CH0, LCD_CAM};
use esp_hal::time::Rate;
use esp_hub75::framebuffer::bitplane::plain::DmaFrameBuffer;
use esp_hub75::framebuffer::compute_rows;
use esp_hub75::{Color, Hub75, Hub75Pins16};
use heapless::String;

use crate::model::DisplayState;
use crate::shared::DISPLAY;

pub const ROWS: usize = 64;
pub const COLS: usize = 64;
const NROWS: usize = compute_rows(ROWS);
const PLANES: usize = 7;

/// Brand copper (#B87648) as an explicit RGB value (brief §7.7 — not a generic "yellow").
pub const ACCENT: Color = Color::new(0xB8, 0x76, 0x48);
const DIM: Color = Color::new(0x5C, 0x55, 0x4C); // --muted, for secondary text

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

        let mut xfer = hub75
            .render(fb)
            .map_err(|(e, _)| e)
            .expect("failed to start render");
        xfer.wait_for_done().await.expect("render wait failed");
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
        fb.erase();
        draw_state(fb, &state);
        tx.signal(fb);
        fb = rx.wait().await;
        state = DISPLAY.wait().await;
    }
}

// ---------------------------------------------------------------------------------------
// Drawing — PLACEHOLDER layout (brief §7.7). Keep all of it behind these helpers.
// ---------------------------------------------------------------------------------------

fn style(font: &'static embedded_graphics::mono_font::MonoFont<'static>, color: Color) -> MonoTextStyle<'static, Color> {
    MonoTextStyleBuilder::new().font(font).text_color(color).build()
}

fn left(fb: &mut FBType, s: &str, x: i32, y: i32, st: MonoTextStyle<'static, Color>) {
    let _ = Text::with_baseline(s, Point::new(x, y), st, Baseline::Top).draw(fb);
}

fn right(fb: &mut FBType, s: &str, y: i32, st: MonoTextStyle<'static, Color>, char_w: i32) {
    let x = COLS as i32 - (s.len() as i32 * char_w) - 1;
    left(fb, s, x.max(0), y, st);
}

/// Dispatch on the current state and draw it.
pub fn draw_state(fb: &mut FBType, state: &DisplayState) {
    match state {
        DisplayState::Provisioning => draw_provisioning(fb),
        DisplayState::IdleAddress { octets } => draw_idle(fb, *octets),
        DisplayState::Departures(deps) => draw_departures(fb, deps),
        DisplayState::Offline => draw_offline(fb),
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

fn draw_departures(fb: &mut FBType, deps: &[crate::model::Departure]) {
    let accent = style(&FONT_5X7, ACCENT);
    if deps.is_empty() {
        draw_offline(fb);
        return;
    }
    for (i, d) in deps.iter().take(3).enumerate() {
        let y = 4 + (i as i32) * 20;
        // Left: "<line> <dest>", truncated to fit.
        let mut lbl: String<24> = String::new();
        let _ = write!(lbl, "{} {}", d.line.as_str(), d.destination.as_str());
        let mut shown = lbl.as_str();
        if shown.len() > 9 {
            shown = &shown[..9];
        }
        left(fb, shown, 1, y, accent);
        // Right: minutes — "--", "now", or "N'".
        let mut mins: String<8> = String::new();
        match d.minutes {
            None => {
                let _ = write!(mins, "--");
            }
            Some(0) => {
                let _ = write!(mins, "now");
            }
            Some(m) => {
                let _ = write!(mins, "{}'", m);
            }
        }
        right(fb, mins.as_str(), y + 9, accent, 5);
    }
}

/// Allocate the two framebuffers as `'static` and return them. Call once.
pub fn framebuffers() -> (&'static mut FBType, &'static mut FBType) {
    let fb0 = crate::mk_static!(FBType, FBType::new());
    fb0.erase();
    let fb1 = crate::mk_static!(FBType, FBType::new());
    fb1.erase();
    (fb0, fb1)
}

/// Allocate the two framebuffer-exchange signals as `'static`. Call once.
pub fn exchanges() -> (&'static FrameBufferExchange, &'static FrameBufferExchange) {
    let tx = crate::mk_static!(FrameBufferExchange, FrameBufferExchange::new());
    let rx = crate::mk_static!(FrameBufferExchange, FrameBufferExchange::new());
    (tx, rx)
}
