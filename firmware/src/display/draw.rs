//! Shared drawing primitives: text (flush, centred, marquee), badges, rules, and the small
//! pixel-art glyphs (tram front, Z-blind, arrow). Everything routes through the module's
//! brightness choke points ([`super::scaled`] via `style`/`pset`/`fill_rect`), so the whole
//! palette dims uniformly. Used by both the built-in [`super::screens`] and the
//! [`super::custom`] layout renderer.

use core::fmt::Write as _;

use embedded_graphics::draw_target::DrawTargetExt;
use embedded_graphics::mono_font::iso_8859_1::FONT_6X10;
use embedded_graphics::mono_font::{MonoFont, MonoTextStyle, MonoTextStyleBuilder};
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Line, PrimitiveStyle, Rectangle};
use embedded_graphics::text::{Baseline, Text};
use embedded_graphics::Pixel;
use esp_hub75::Color;
use heapless::String;

use super::{marquee_offset, scaled, AMBER, COLS, FBType, MARQUEE_GAP, OFF, ROWS};

pub(super) fn style(font: &'static MonoFont<'static>, color: Color) -> MonoTextStyle<'static, Color> {
    MonoTextStyleBuilder::new().font(font).text_color(scaled(color)).build()
}

pub(super) fn left(fb: &mut FBType, s: &str, x: i32, y: i32, st: MonoTextStyle<'static, Color>) {
    let _ = Text::with_baseline(s, Point::new(x, y), st, Baseline::Top).draw(fb);
}

/// Draw `s` horizontally centred at baseline-top `y`. `char_w` is the font's per-character
/// advance (e.g. 5 for `FONT_5X7`, 6 for `FONT_6X10`).
pub(super) fn centered(fb: &mut FBType, s: &str, y: i32, st: MonoTextStyle<'static, Color>, char_w: i32) {
    let x = (COLS as i32 - s.chars().count() as i32 * char_w) / 2;
    left(fb, s, x, y, st);
}

/// Set a single pixel, clipped to the panel.
pub(super) fn pset(fb: &mut FBType, x: i32, y: i32, c: Color) {
    if x >= 0 && y >= 0 && x < COLS as i32 && y < ROWS as i32 {
        let _ = Pixel(Point::new(x, y), scaled(c)).draw(fb);
    }
}

/// Fill an axis-aligned rectangle in `c` (scaled to the active brightness like every other draw).
pub(super) fn fill_rect(fb: &mut FBType, x: i32, y: i32, w: i32, h: i32, c: Color) {
    let _ = Rectangle::new(Point::new(x, y), Size::new(w.max(0) as u32, h.max(0) as u32))
        .into_styled(PrimitiveStyle::with_fill(scaled(c)))
        .draw(fb);
}

/// A full-width rule at row `y` in `color`.
pub(super) fn rule(fb: &mut FBType, y: i32, color: Color) {
    let _ = Line::new(Point::new(0, y), Point::new(COLS as i32 - 1, y))
        .into_styled(PrimitiveStyle::with_stroke(scaled(color), 1))
        .draw(fb);
}

/// Apply the user's "hide city names" setting: when enabled, drop a leading "City, " prefix so
/// only the place name shows (e.g. "Zürich, Klusplatz" → "Klusplatz"); otherwise pass through.
pub(super) fn city(name: &str) -> &str {
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
pub(super) fn fmt_minutes(minutes: Option<u16>) -> String<8> {
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

/// Draw `text` at baseline-top `(x0, y)`. If it fits within `avail` pixels it sits flush at
/// `x0`; otherwise it scrolls as a seamless marquee — paused ~5 s at the start, then one full
/// round, repeat. Returns `true` when it is scrolling (so the caller keeps ticking frames).
#[allow(clippy::too_many_arguments)] // a layout helper: position, width, style and frame all matter
pub(super) fn draw_marquee(
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
    let period = text_w + MARQUEE_GAP;
    let offset = marquee_offset(period, frame);
    left(fb, text, x0 - offset, y, st);
    left(fb, text, x0 - offset + period, y, st);
    true
}

/// Like [`draw_marquee`], but the text is clipped to the band `[x0, x0+avail) × [clip_top,
/// clip_top+clip_h)` so a scrolling label can't spill into neighbouring content (the badge to
/// its left or the time to its right). Returns `true` when it is scrolling.
#[allow(clippy::too_many_arguments)] // a layout helper: position, clip band, style and frame all matter
pub(super) fn draw_marquee_clipped(
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
    let period = text_w + MARQUEE_GAP;
    let offset = marquee_offset(period, frame);
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
pub(super) fn draw_segments_row(
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
    let period = total + MARQUEE_GAP;
    let offset = marquee_offset(period, frame);
    draw_at(fb, x0 - offset);
    draw_at(fb, x0 - offset + period);
    true
}

/// Draw a filled badge holding the line label, top-left at `(x, y)`: `fill` background with
/// `text` colour. Sized to the label so any length fits. Returns the x just past the badge.
pub(super) fn draw_badge(fb: &mut FBType, line: &str, x: i32, y: i32, fill: Color, text: Color) -> i32 {
    let w = line.chars().count() as i32 * 6 + 5;
    let _ = Rectangle::new(Point::new(x, y), Size::new(w as u32, 11))
        .into_styled(PrimitiveStyle::with_fill(scaled(fill)))
        .draw(fb);
    left(fb, line, x + 3, y + 1, style(&FONT_6X10, text));
    x + w
}

/// Draw a departure's line label at badge origin `(x, y)`, honouring the "line badges" setting:
/// a filled amber badge when badges are on, else plain FONT_6X10 amber text on the badge's own
/// label baseline (`y + 1`). Returns the x just past what was drawn.
pub(super) fn draw_line_label(fb: &mut FBType, line: &str, x: i32, y: i32) -> i32 {
    if crate::shared::line_badges_enabled() {
        draw_badge(fb, line, x, y, AMBER, OFF)
    } else {
        left(fb, line, x, y + 1, style(&FONT_6X10, AMBER));
        x + line.chars().count() as i32 * 6
    }
}

// Small pixel-art glyphs. The Z-blind doubles as the connecting-tram's route blind; the arrow
// points at the idle screen's call to action; the tram front is the "departing now" pictogram.
pub(super) const Z_GLYPH: [[u8; 3]; 5] = [[1, 1, 1], [0, 0, 1], [0, 1, 0], [1, 0, 0], [1, 1, 1]];
pub(super) const ARROW_GLYPH: [[u8; 7]; 5] = [
    [0, 0, 0, 0, 1, 0, 0],
    [0, 0, 0, 0, 0, 1, 0],
    [1, 1, 1, 1, 1, 1, 1],
    [0, 0, 0, 0, 0, 1, 0],
    [0, 0, 0, 0, 1, 0, 0],
];

/// Width and height of the [`draw_train_front`] pictogram, in pixels.
pub(super) const TRAIN_W: i32 = 9;
pub(super) const TRAIN_H: i32 = 10;

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

/// Blit a small bitmap glyph with its top-left at `(x, y)`, each lit cell drawn as a `k×k` block.
pub(super) fn blit_bitmap<const W: usize, const H: usize>(
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

/// Draw the front-of-tram pictogram with its top-left at `(x, y)` in `c`.
pub(super) fn draw_train_front(fb: &mut FBType, x: i32, y: i32, c: Color) {
    draw_train_front_scaled(fb, x, y, 1, c);
}

/// Draw the front-of-tram pictogram blown up by an integer `scale` (each lit cell becomes a
/// `scale`×`scale` block), top-left at `(x, y)`. `scale == 1` is the board's pictogram; the focus
/// view uses `2` for the large "departing now" state.
pub(super) fn draw_train_front_scaled(fb: &mut FBType, x: i32, y: i32, scale: i32, c: Color) {
    blit_bitmap(fb, &TRAIN_GLYPH, x, y, scale, c);
}
