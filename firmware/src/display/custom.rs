//! Custom layout renderer (FEATURE_UI_BUILDER §7.5). [`draw_custom_layout`] walks the user's saved
//! [`Layout`] and dispatches each element on its numeric type tag `t`, reusing the board's
//! primitives. Data-bound elements (station, departure fields) honour the same global config as the
//! built-in board (`line_badges_enabled` / `city`), so a custom board stays in lock-step with
//! Default/Focus. Font upscaling (`k ∈ 1..=3`) goes through [`blit_scaled_text`], which pixel-doubles
//! the font's own glyphs so the panel matches the JS simulator glyph-for-glyph (§8.2). Everything is
//! defensive: out-of-range fields are clamped and a hostile POST can never panic the render task.

use core::fmt::Write as _;
use core::ops::Range;

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
    blit_bitmap, blit_cell, city, draw_train_front_scaled, fill_rect, fmt_minutes,
    ARROW_GLYPH, Z_GLYPH,
};
use super::{marquee_offset, ACCENT, AMBER, COLS, DIM, FBType, MARQUEE_GAP, OFF};

/// Resolve an element's colour: an explicit 24-bit `col` (`0xRRGGBB`) overrides the preset index
/// `c` (0 = AMBER, 1 = ACCENT, 2 = DIM). The result still passes through `scaled()` at draw time
/// (via `pset`/`fill_rect`) like every other colour, so custom colours dim with the panel.
fn elem_color(el: &Element) -> Color {
    match (el.col, el.c) {
        (Some(rgb), _) => Color::new((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8),
        (None, 1) => ACCENT,
        (None, 2) => DIM,
        (None, _) => AMBER,
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
    /// Which canvas cells the rasterised glyph lights up, indexed `[y][x]` — a 1-bit image of
    /// the glyph, before scaling.
    lit_pixels: [[bool; 6]; 10],
}

impl GlyphCanvas {
    fn new() -> Self {
        Self { lit_pixels: [[false; 6]; 10] }
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
                self.lit_pixels[p.y as usize][p.x as usize] = true;
            }
        }
        Ok(())
    }
}

/// Font, integer upscale factor and colour of a piece of scaled custom text — grouped so
/// [`blit_scaled_text`]'s signature stays readable and one style can serve several blit calls
/// (the two wrapped copies of a marquee).
#[derive(Clone, Copy)]
struct GlyphStyle {
    font: &'static MonoFont<'static>,
    k: i32,
    color: Color,
}

/// Draw `text` at baseline-top `(x0, y)` in `style` (each source glyph pixel becomes a `k×k`
/// block), clipped to the horizontal band of columns `clip`. Advance per glyph is `char_w × k`,
/// matching the frozen metrics. Glyphs fully outside the clip are skipped.
fn blit_scaled_text(fb: &mut FBType, text: &str, x0: i32, y: i32, style: &GlyphStyle, clip: Range<i32>) {
    let GlyphStyle { font, k, color } = *style;
    let cw = font.character_size.width as i32;
    let ch = font.character_size.height as i32;
    let on = MonoTextStyleBuilder::new()
        .font(font)
        .text_color(Color::new(0xFF, 0xFF, 0xFF))
        .build();
    let mut x = x0;
    for c in text.chars() {
        // Only rasterise glyphs that overlap the clip band.
        if x + cw * k > clip.start && x < clip.end {
            let mut canvas = GlyphCanvas::new();
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            let _ = Text::with_baseline(s, Point::zero(), on, Baseline::Top).draw(&mut canvas);
            for gy in 0..ch {
                for gx in 0..cw {
                    if canvas.lit_pixels[gy as usize][gx as usize] {
                        blit_cell(fb, x + gx * k, y + gy * k, k, color, &clip);
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
    let style = GlyphStyle {
        font: font_for(el.s),
        k: (el.k as i32).clamp(1, 3),
        color: elem_color(el),
    };
    let x = el.x as i32;
    let y = el.y as i32;
    let text_w = text.chars().count() as i32 * style.font.character_size.width as i32 * style.k;
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
        let clip = if el.w > 0 { x..x + avail } else { i32::MIN..i32::MAX };
        blit_scaled_text(fb, text, x + off, y, &style, clip);
        false
    } else {
        let period = text_w + MARQUEE_GAP;
        let offset = marquee_offset(period, frame);
        blit_scaled_text(fb, text, x - offset, y, &style, x..x + avail);
        blit_scaled_text(fb, text, x - offset + period, y, &style, x..x + avail);
        true
    }
}

/// Draw the fk=0 line label as a filled badge with the digits cut out (unlit), upscaled by the
/// element's `k` — base metrics match `draw::draw_badge` (FONT_6X10 label, 11 px tall) at `k=1`,
/// and the JS simulator's `drawBadge` at every scale.
fn draw_badge_scaled(fb: &mut FBType, line: &str, el: &Element, fill: Color) {
    let k = (el.k as i32).clamp(1, 3);
    let (x, y) = (el.x as i32, el.y as i32);
    let w = (line.chars().count() as i32 * 6 + 5) * k;
    fill_rect(fb, x, y, w, 11 * k, fill);
    let style = GlyphStyle { font: &FONT_6X10, k, color: OFF };
    blit_scaled_text(fb, line, x + 3 * k, y + k, &style, i32::MIN..i32::MAX);
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
        // Badge, per its style `f`: 0 = a filled badge (scaled by `k`) when line badges are on,
        // else plain text; 1 = minimal — always the line as plain (scalable) text, no box.
        0 => {
            if el.f != 1 && crate::shared::line_badges_enabled() {
                draw_badge_scaled(fb, dep.line.as_str(), el, color);
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

/// Shared body of the Clock and Date elements: format the synced local time into a small string
/// and place it as **static** text (no marquee — both refresh on the `BRIGHTNESS_REFRESH_SECS`
/// static-screen wake). Draws nothing before SNTP has synced, since there is no local time yet.
fn draw_local_time_text(
    fb: &mut FBType,
    el: &Element,
    format: impl FnOnce(&mut String<16>, (i64, u32, u32, u32, u32)),
) -> bool {
    let Some(unix) = crate::shared::now_unix() else {
        return false;
    };
    let mut s: String<16> = String::new();
    format(&mut s, local_parts(unix));
    place_text(fb, s.as_str(), el, false, 0)
}

/// Clock element (`t=3`): the local time, `HH:MM` (`f=0`, zero-padded) or `H:MM` (`f=1`).
fn draw_clock(fb: &mut FBType, el: &Element) -> bool {
    draw_local_time_text(fb, el, |s, (_, _, _, hh, mm)| {
        let _ = match el.f {
            1 => write!(s, "{}:{:02}", hh, mm),
            _ => write!(s, "{:02}:{:02}", hh, mm),
        };
    })
}

/// Date element (`t=4`): the local date, `DD.MM.` (`f=0`) or `DD.MM.YYYY` (`f=1`).
fn draw_date(fb: &mut FBType, el: &Element) -> bool {
    draw_local_time_text(fb, el, |s, (y, m, d, _, _)| {
        let _ = match el.f {
            1 => write!(s, "{:02}.{:02}.{}", d, m, y),
            _ => write!(s, "{:02}.{:02}.", d, m),
        };
    })
}

/// Divider element (`t=5`): a horizontal bar at `y`, length `w` (or to the panel edge when `w=0`),
/// thickness `th` (1..=2). Uses `fill_rect`, which clips to the panel and scales the colour.
fn draw_divider(fb: &mut FBType, el: &Element) {
    let len = if el.w > 0 { el.w as i32 } else { COLS as i32 - el.x as i32 };
    let th = (el.th as i32).clamp(1, 2);
    fill_rect(fb, el.x as i32, el.y as i32, len, th, elem_color(el));
}

// Weather condition glyphs (8×7, row-major, two-tone: 0 = unlit, 1 = primary, 2 = accent),
// one per bucket of WMO weather codes — picked by [`weather_condition_glyph`]. The accent cells
// mark the detail part of a condition (the sun behind the partly-cloudy cloud, rain drops, snow
// flakes, the thunder bolt); in the single-colour icon mode both tones draw in the element
// colour, in the colourful mode they take the fixed [`colorful_weather_palette`] pair. The JS
// simulator carries identical bitmaps (`WEATHER_GLYPHS` in index.html) so the preview matches
// the panel pixel-for-pixel.
const SUN_GLYPH: [[u8; 8]; 7] = [
    [0, 0, 0, 1, 0, 0, 0, 0],
    [0, 1, 0, 1, 0, 1, 0, 0],
    [0, 0, 1, 1, 1, 0, 0, 0],
    [1, 1, 1, 1, 1, 1, 1, 0],
    [0, 0, 1, 1, 1, 0, 0, 0],
    [0, 1, 0, 1, 0, 1, 0, 0],
    [0, 0, 0, 1, 0, 0, 0, 0],
];
const PARTLY_CLOUDY_GLYPH: [[u8; 8]; 7] = [
    [0, 0, 0, 0, 0, 2, 2, 0],
    [0, 0, 0, 0, 0, 2, 2, 2],
    [0, 0, 1, 1, 1, 0, 0, 0],
    [0, 1, 1, 1, 1, 1, 1, 0],
    [1, 1, 1, 1, 1, 1, 1, 1],
    [1, 1, 1, 1, 1, 1, 1, 1],
    [0, 0, 0, 0, 0, 0, 0, 0],
];
const CLOUD_GLYPH: [[u8; 8]; 7] = [
    [0, 0, 0, 0, 0, 0, 0, 0],
    [0, 0, 1, 1, 1, 0, 0, 0],
    [0, 1, 1, 1, 1, 1, 1, 0],
    [1, 1, 1, 1, 1, 1, 1, 1],
    [1, 1, 1, 1, 1, 1, 1, 1],
    [0, 0, 0, 0, 0, 0, 0, 0],
    [0, 0, 0, 0, 0, 0, 0, 0],
];
const FOG_GLYPH: [[u8; 8]; 7] = [
    [0, 0, 0, 0, 0, 0, 0, 0],
    [1, 1, 1, 1, 1, 1, 1, 0],
    [0, 0, 0, 0, 0, 0, 0, 0],
    [0, 1, 1, 1, 1, 1, 1, 1],
    [0, 0, 0, 0, 0, 0, 0, 0],
    [1, 1, 1, 1, 1, 1, 0, 0],
    [0, 0, 0, 0, 0, 0, 0, 0],
];
const RAIN_GLYPH: [[u8; 8]; 7] = [
    [0, 0, 1, 1, 1, 0, 0, 0],
    [0, 1, 1, 1, 1, 1, 1, 0],
    [1, 1, 1, 1, 1, 1, 1, 1],
    [1, 1, 1, 1, 1, 1, 1, 1],
    [0, 0, 0, 0, 0, 0, 0, 0],
    [0, 2, 0, 0, 2, 0, 0, 2],
    [2, 0, 0, 2, 0, 0, 2, 0],
];
const SNOW_GLYPH: [[u8; 8]; 7] = [
    [0, 0, 1, 1, 1, 0, 0, 0],
    [0, 1, 1, 1, 1, 1, 1, 0],
    [1, 1, 1, 1, 1, 1, 1, 1],
    [1, 1, 1, 1, 1, 1, 1, 1],
    [0, 0, 0, 0, 0, 0, 0, 0],
    [2, 0, 0, 2, 0, 0, 2, 0],
    [0, 0, 2, 0, 0, 2, 0, 0],
];
const THUNDER_GLYPH: [[u8; 8]; 7] = [
    [0, 0, 1, 1, 1, 0, 0, 0],
    [0, 1, 1, 1, 1, 1, 1, 0],
    [1, 1, 1, 1, 1, 1, 1, 1],
    [1, 1, 1, 1, 1, 1, 1, 1],
    [0, 0, 0, 2, 2, 0, 0, 0],
    [0, 0, 2, 2, 0, 0, 0, 0],
    [0, 2, 2, 0, 0, 0, 0, 0],
];

/// Horizontal advance from a weather icon to the temperature next to it, in unscaled LEDs
/// (the 8-column glyph plus a 1-LED gap). The editor's `measureEl` mirrors this.
const WEATHER_ICON_ADVANCE: u8 = 9;

// Fixed palette for the colourful icon mode (`g=1`), mirrored exactly by the JS simulator
// (`WEATHER_COLORFUL` in index.html). Whites/blues wash toward white on the HUB75 panel (the
// gamut caveat on the brand palette above), which is fine here — clouds and snow *should* read
// white; the sun and the thunder bolt stay red-weighted enough to hold their yellow.
const WEATHER_SUN_YELLOW: Color = Color::new(0xFF, 0xD4, 0x00);
const WEATHER_CLOUD_WHITE: Color = Color::new(0xE8, 0xE8, 0xE8);
const WEATHER_CLOUD_GRAY: Color = Color::new(0x8A, 0x8A, 0x8A);
const WEATHER_FOG_GRAY: Color = Color::new(0xB4, 0xB4, 0xB4);
const WEATHER_RAIN_BLUE: Color = Color::new(0x4A, 0x90, 0xE2);

/// The colourful mode's `(primary, accent)` pair for a WMO weather code — the two tones of the
/// condition glyph: yellow sun; white cloud (yellow sun accent when partly cloudy); gray cloud
/// with blue rain, white snow, or a yellow bolt; gray fog. Buckets match
/// [`weather_condition_glyph`].
fn colorful_weather_palette(code: u8) -> (Color, Color) {
    match code {
        0 | 1 => (WEATHER_SUN_YELLOW, WEATHER_SUN_YELLOW),
        2 => (WEATHER_CLOUD_WHITE, WEATHER_SUN_YELLOW),
        45 | 48 => (WEATHER_FOG_GRAY, WEATHER_FOG_GRAY),
        51..=67 | 80..=82 => (WEATHER_CLOUD_GRAY, WEATHER_RAIN_BLUE),
        71..=77 | 85 | 86 => (WEATHER_CLOUD_GRAY, WEATHER_CLOUD_WHITE),
        95..=99 => (WEATHER_CLOUD_GRAY, WEATHER_SUN_YELLOW),
        _ => (WEATHER_CLOUD_WHITE, WEATHER_CLOUD_WHITE), // overcast (3) and unknown codes
    }
}

/// Blit a two-tone weather glyph with its top-left at `(x, y)`, each cell drawn as a `k×k` block:
/// `1` cells in `primary`, `2` cells in `accent` (see the glyph comment above). The single-colour
/// icon mode simply passes the same colour twice.
fn blit_weather_glyph(
    fb: &mut FBType,
    glyph: &[[u8; 8]; 7],
    x: i32,
    y: i32,
    k: i32,
    primary: Color,
    accent: Color,
) {
    let no_clip = i32::MIN..i32::MAX; // pset already clips to the panel
    for (gy, row) in glyph.iter().enumerate() {
        for (gx, &tone) in row.iter().enumerate() {
            if tone != 0 {
                let color = if tone == 2 { accent } else { primary };
                blit_cell(fb, x + gx as i32 * k, y + gy as i32 * k, k, color, &no_clip);
            }
        }
    }
}

/// The condition glyph for a WMO weather interpretation code (Open-Meteo `weather_code`):
/// clear (0–1), partly cloudy (2), overcast (3), fog (45/48), rain incl. drizzle and showers
/// (51–67, 80–82), snow (71–77, 85–86), thunderstorm (95–99). Unknown codes read as cloud.
fn weather_condition_glyph(code: u8) -> &'static [[u8; 8]; 7] {
    match code {
        0 | 1 => &SUN_GLYPH,
        2 => &PARTLY_CLOUDY_GLYPH,
        3 => &CLOUD_GLYPH,
        45 | 48 => &FOG_GLYPH,
        51..=67 | 80..=82 => &RAIN_GLYPH,
        71..=77 | 85 | 86 => &SNOW_GLYPH,
        95..=99 => &THUNDER_GLYPH,
        _ => &CLOUD_GLYPH,
    }
}

/// Weather element (`t=7`): the current conditions at the tracked stop, per its format `f`
/// (0 = icon + temperature, 1 = temperature only, 2 = icon only). The icon draws per its
/// palette mode `g`: 0 = the element colour (single tone, custom-colourable like any element),
/// 1 = colourful ([`colorful_weather_palette`]'s fixed per-condition pair); the temperature
/// always uses the element colour. Draws nothing until the Open-Meteo fetch has a fresh-enough
/// sample ([`crate::shared::weather`]) — the same missing-live-data contract as an absent
/// departure slot. Static text, never a marquee.
fn draw_weather(fb: &mut FBType, el: &Element) -> bool {
    let Some(w) = crate::shared::weather() else {
        return false;
    };
    let k = (el.k as i32).clamp(1, 3);
    let show_icon = el.f != 1;
    let show_temp = el.f != 2;
    if show_icon {
        let (primary, accent) = if el.g == 1 {
            colorful_weather_palette(w.code)
        } else {
            let c = elem_color(el);
            (c, c)
        };
        let glyph = weather_condition_glyph(w.code);
        blit_weather_glyph(fb, glyph, el.x as i32, el.y as i32, k, primary, accent);
    }
    if show_temp {
        let mut s: String<8> = String::new();
        let _ = write!(s, "{}°", w.whole_celsius());
        // The temperature reuses `place_text` via a shifted copy of the element: natural width,
        // pinned left (alignment has nothing to align against without a box).
        let mut text_el = el.clone();
        if show_icon {
            text_el.x = el.x.saturating_add(WEATHER_ICON_ADVANCE.saturating_mul(k as u8));
        }
        text_el.w = 0;
        text_el.a = 0;
        place_text(fb, s.as_str(), &text_el, false, 0);
    }
    false
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
            7 => draw_weather(fb, el),                           // live Weather
            _ => false, // unknown type: ignore
        };
    }
    animating
}
