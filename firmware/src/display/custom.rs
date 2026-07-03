//! Custom layout renderer (FEATURE_UI_BUILDER §7.5). [`draw_custom_layout`] walks the user's saved
//! [`Layout`] and dispatches each element on its numeric type tag `t`, reusing the board's
//! primitives. Data-bound elements (station, departure fields) honour the same global config as the
//! built-in board (`line_badges_enabled` / `city`), so a custom board stays in lock-step with
//! Default/Focus. Font upscaling (`k ∈ 1..=3`) goes through [`blit_scaled_text`], which pixel-doubles
//! the font's own glyphs so the panel matches the JS simulator glyph-for-glyph (§8.2). Everything is
//! defensive: out-of-range fields are clamped and a hostile POST can never panic the render task.

use core::fmt::Write as _;

use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::mono_font::iso_8859_1::{FONT_5X7, FONT_6X10};
use embedded_graphics::mono_font::{MonoFont, MonoTextStyleBuilder};
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::Rectangle;
use embedded_graphics::text::{Baseline, Text};
use embedded_graphics::Pixel;
use esp_hub75::Color;
use heapless::String;

use crate::localtime::local_parts;
use crate::model::{Element, Layout};

use super::draw::{
    blit_bitmap, city, draw_badge, draw_train_front_scaled, fill_rect, fmt_minutes, pset,
    ARROW_GLYPH, Z_GLYPH,
};
use super::{marquee_offset, ACCENT, AMBER, COLS, DIM, FBType, MARQUEE_GAP, OFF};

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
        let offset = marquee_offset(period, frame);
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

/// Clock element (`t=3`): the local time, `HH:MM` (`f=0`, zero-padded) or `H:MM` (`f=1`). Static —
/// never forces animation; it refreshes on the `BRIGHTNESS_REFRESH_SECS` static-screen wake.
fn draw_clock(fb: &mut FBType, el: &Element) -> bool {
    // Before SNTP has synced there is no local time, so the element draws nothing.
    let Some(unix) = crate::shared::now_unix() else {
        return false;
    };
    let (_, _, _, hh, mm) = local_parts(unix);
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
    let Some(unix) = crate::shared::now_unix() else {
        return false;
    };
    let (y, m, d, _, _) = local_parts(unix);
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
pub(super) fn draw_custom_layout(
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
