//! The built-in screens (brief §7.7), one drawer per [`DisplayState`](crate::model::DisplayState)
//! variant plus the two departure views: provisioning, the rolling-tram connecting/offline
//! animation, the idle address screen, the three-row departures board, and the single-departure
//! focus view. Each drawer returns `true` while anything on it is animating (a scrolling
//! marquee, the tram) so the render loop keeps ticking frames.

use core::fmt::Write as _;

use embedded_graphics::mono_font::iso_8859_1::{FONT_5X7, FONT_6X10};
use esp_hub75::Color;
use heapless::String;

use super::draw::{
    blit_bitmap, centered, city, draw_line_label, draw_marquee, draw_marquee_clipped,
    draw_segments_row, draw_train_front, draw_train_front_scaled, fill_rect, fmt_minutes, left,
    pset, rule, style, ARROW_GLYPH, TRAIN_H, TRAIN_W, Z_GLYPH,
};
use super::{brand, ACCENT, AMBER, COLS, DIM, FBType};

pub(super) fn draw_provisioning(fb: &mut FBType, frame: u32) -> bool {
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
/// Frames for one full pass of the tram (~2.4 s at `FRAME_MS`).
pub const CONNECT_CYCLE_FRAMES: u32 = 48;
// The scene sits in the lower part of the panel, leaving room for the "Connecting" label up top.
const TRAIN_TOP: i32 = 34; // body-top row; wire sits above, rail below
const WIRE_Y: i32 = 28;
const RAIL_Y: i32 = 50;

/// `true` once the connecting animation has completed at least one full pass (frame numbers
/// `0..CONNECT_CYCLE_FRAMES` make up one pass, so the last frame of it is `… - 1`).
pub(super) fn connect_cycle_done(frame: u32) -> bool {
    frame >= CONNECT_CYCLE_FRAMES - 1
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
pub(super) fn draw_connecting(fb: &mut FBType, frame: u32) -> bool {
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

    // Front route blind: a lit "Z" (for Zügli) on a dark sign — the same [`Z_GLYPH`] the icon
    // element draws, but with the unlit cells filled as the sign's dark background.
    for (gy, row) in Z_GLYPH.iter().enumerate() {
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

pub(super) fn draw_idle(fb: &mut FBType, octets: [u8; 4], frame: u32) -> bool {
    let accent = style(&FONT_5X7, ACCENT);
    let amber = style(&FONT_5X7, AMBER);
    let dim = style(&FONT_5X7, DIM);
    // The board is on WiFi but no connection is picked yet. Lead with the call to action (too
    // wide for one line at this font, so split over two with a right arrow), then how to reach
    // the config page: mDNS name first, IP as a fallback, on one line.
    left(fb, "Choose a", 2, 2, amber);
    left(fb, "connection", 2, 12, amber);
    blit_bitmap(fb, &ARROW_GLYPH, 54, 13, 1, ACCENT);
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
pub(super) fn draw_offline(fb: &mut FBType, frame: u32) -> bool {
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
pub(super) fn draw_departures(
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
        let badge_end = draw_line_label(fb, dep.line.as_str(), 1, badge_y);

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
pub(super) fn draw_focus(
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
    let badge_end = draw_line_label(fb, next.line.as_str(), 1, 1);
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
