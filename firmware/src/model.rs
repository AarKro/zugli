//! Shared data types: the saved connection selection, WiFi credentials, a single
//! departure, and what the LED panel is currently showing.

use heapless::{String, Vec};
use serde::{Deserialize, Serialize};

/// One tracked connection: a line and where it's headed. Matched against the stationboard's
/// `(number, to)` at runtime, and carries the category for the panel badge.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Conn {
    /// Line number/name (API `number`), e.g. `2`, `S12`.
    pub line: String<16>,
    /// Raw API `category` (kept for future badge styling).
    pub category: String<12>,
    /// Final destination (API `to`), e.g. `Schlieren`.
    pub destination: String<48>,
}

/// Largest number of specific connections the user can track at one stop. Bounded so the whole
/// [`Selection`] still fits the flash record (`storage::MAX_PAYLOAD`).
pub const MAX_CONNS: usize = 6;

/// The user's panel selection, persisted to flash and matched against the stationboard at
/// runtime. Mirrors the `POST /save` body (PROJECT_BRIEF.md §4.4). The panel always renders the
/// stop's next departures as a board; `all_connections` chooses which departures count:
/// - `false` (default): only the connections the user explicitly picked ([`connections`]).
/// - `true`: every connection departing the stop.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Selection {
    /// API location `id` of the chosen stop.
    #[serde(rename = "stopId")]
    pub stop_id: String<24>,
    /// Human-readable stop name (display/echo only).
    #[serde(rename = "stopName")]
    pub stop_name: String<64>,
    /// When `true`, show every departure at the stop and ignore [`connections`]. Defaults via
    /// `#[serde(default)]` so older flash records (and one-mode saves) still load.
    #[serde(rename = "allConnections", default)]
    pub all_connections: bool,
    /// The specific connections to track (specific-connections mode); empty in all-connections
    /// mode. `#[serde(default)]` keeps older flash records loading (they had no such field).
    #[serde(default)]
    pub connections: Vec<Conn, MAX_CONNS>,
}

/// Home WiFi credentials entered during provisioning (brief §5).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WifiCreds {
    pub ssid: String<32>,
    pub password: String<64>,
}

/// User-tweakable board settings, edited from the config page's settings sheet and persisted
/// to flash. The container-level `#[serde(default)]` means any field missing from the JSON
/// (older flash records, or a partial update) falls back to [`Config::default`].
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// When `true`, drop the leading "City, " prefix from stop/destination names on the panel
    /// (e.g. "Zürich, Schlieren" → "Schlieren").
    #[serde(rename = "stripCity")]
    pub strip_city: bool,
    /// When `true` (default), each departure row shows its line as a filled badge; when `false`,
    /// the line is rendered as plain text, freeing the badge's padding for the destination.
    #[serde(rename = "showLineBadges")]
    pub show_line_badges: bool,
    /// Manual brightness level, 1–10, mapping linearly to 10–100 % panel brightness.
    #[serde(rename = "brightness")]
    pub brightness: u8,
    /// When `true`, the panel auto-dims to 10 % during the reduced window (local time);
    /// otherwise it stays at the manual [`brightness`](Config::brightness) level all day.
    #[serde(rename = "autoBrightness")]
    pub auto_brightness: bool,
    /// When `true`, the panel turns fully off (all LEDs dark) during the reduced window instead
    /// of dimming to 10 %. Only meaningful while [`auto_brightness`](Config::auto_brightness) is on.
    #[serde(rename = "offWhenDimmed")]
    pub off_when_dimmed: bool,
    /// Start/end of the reduced-brightness window, as minutes since local midnight (e.g.
    /// 20:00 = 1200). The window wraps past midnight when `start > end`.
    #[serde(rename = "reducedStart")]
    pub reduced_start: u16,
    #[serde(rename = "reducedEnd")]
    pub reduced_end: u16,
    /// Which view the panel draws for the departures screen: `0` = the default three-departure
    /// board, `1` = the single-departure focus view, `2` = the user's custom layout (rendered only
    /// when a non-empty layout is saved; otherwise it falls back to the default board). Held as a
    /// `u8` rather than an enum so `serde-json-core` round-trips it as a plain integer.
    #[serde(rename = "uiMode", default)]
    pub ui_mode: u8,
    /// Migration shim for the pre-`uiMode` config (commit `3bc2fab`), which stored the focus view
    /// as a `focusView` boolean. Read from old flash records only and **never serialized**, so it
    /// disappears the first time the record is rewritten. [`Config::migrate`] folds a legacy
    /// `focusView:true` into `ui_mode = 1`.
    #[serde(rename = "focusView", default, skip_serializing)]
    pub focus_view_legacy: bool,
}

/// The three-way departures view selected by [`Config::ui_mode`]. `#[repr(u8)]` with explicit
/// discriminants so `mode as u8` matches the persisted `uiMode` integer exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum UiMode {
    /// The built-in three-departure board.
    Default = 0,
    /// The single-departure focus view.
    Focus = 1,
    /// The user's custom layout (falls back to [`Default`](Self::Default) when none is saved).
    Custom = 2,
}

impl UiMode {
    /// Map the persisted `u8` to a mode; any unknown value falls back to [`Default`](Self::Default).
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Focus,
            2 => Self::Custom,
            _ => Self::Default,
        }
    }
}

impl Config {
    /// Fold a legacy `focusView:true` record forward into `ui_mode = 1` (Focus). A no-op for
    /// records already carrying `uiMode`. Call once after loading from flash.
    pub fn migrate(&mut self) {
        if self.ui_mode == 0 && self.focus_view_legacy {
            self.ui_mode = 1;
        }
        self.focus_view_legacy = false;
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            strip_city: false,
            show_line_badges: true, // badges on by default
            brightness: 6,          // 60 %
            auto_brightness: true,  // preserve the board's existing night-dimming behaviour
            off_when_dimmed: false, // dim to 10 % by default, not fully off
            reduced_start: 20 * 60, // 20:00
            reduced_end: 8 * 60,    // 08:00
            ui_mode: 0,             // default to the three-departure board
            focus_view_legacy: false,
        }
    }
}

/// Largest number of elements a custom layout may hold. A **secondary** sanity bound on the
/// heapless `Vec`; the authoritative flash bound is [`LAYOUT_MAX_BYTES`] (accented text escapes to
/// 6-byte `\uXXXX`, so byte count — not element count — is what guarantees the record fits).
pub const MAX_ELEMENTS: usize = 16;

/// Authoritative flash bound for a serialized custom layout, in bytes (FEATURE_UI_BUILDER §5.5/§6).
/// A layout is valid only if its serialized JSON is `<=` this. Enforced by the editor (live) and by
/// the firmware (`POST /layout` / `POST /preview` reject over-budget bodies before any flash write).
pub const LAYOUT_MAX_BYTES: usize = 1536;

/// Maximum length of a Text element's literal string. Bounds a single field and keeps the storage
/// unescape buffer sane; the [`LAYOUT_MAX_BYTES`] cap still governs the whole layout.
pub const MAX_TEXT_LEN: usize = 24;

fn is_zero_u8(v: &u8) -> bool {
    *v == 0
}
fn is_one_u8(v: &u8) -> bool {
    *v == 1
}
fn is_false(v: &bool) -> bool {
    !*v
}
fn str_is_empty(v: &String<MAX_TEXT_LEN>) -> bool {
    v.is_empty()
}
fn one_u8() -> u8 {
    1
}
fn version_default() -> u8 {
    1
}

/// The custom board layout (FEATURE_UI_BUILDER §5.3): a schema `v`ersion plus an ordered list of
/// `e`lements (draw order = array order, later = on top). An empty `e` means "no custom layout";
/// in Custom [`UiMode`] the renderer then falls back to the built-in board.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Layout {
    /// Schema version, currently `1`. A newer `v` than the firmware understands is treated as "no
    /// custom layout" by the renderer rather than mis-rendered.
    #[serde(default = "version_default")]
    pub v: u8,
    /// The elements, in draw order.
    #[serde(default)]
    pub e: Vec<Element, MAX_ELEMENTS>,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            v: 1,
            e: Vec::new(),
        }
    }
}

/// One layout element (FEATURE_UI_BUILDER §5.4). A **flat** struct with a numeric type tag `t` and
/// `#[serde(default)]` optional fields — **not** a data-carrying Rust enum, which `serde-json-core`
/// deserializes poorly (§5.1). Fields serialize only when they differ from their default, keeping
/// the common case compact for the flash budget. Type-specific fields are ignored by other types.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Element {
    /// Type tag: 0=Text, 1=Departure field, 2=Station, 3=Clock, 4=Date, 5=Divider, 6=Icon.
    pub t: u8,
    /// Top-left x (0..=63; baseline-top origin for text).
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub x: u8,
    /// Top-left y (0..=63; the text baseline-top).
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub y: u8,
    /// Clip/marquee width (text-bearing types) or length (divider). `0` = natural width.
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub w: u8,
    /// Font: 0 = FONT_5X7 (advance 5), 1 = FONT_6X10 (advance 6).
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub s: u8,
    /// Integer upscale factor, 1..=3 (each source glyph pixel becomes a `k`×`k` block).
    #[serde(default = "one_u8", skip_serializing_if = "is_one_u8")]
    pub k: u8,
    /// Preset colour index: 0 = AMBER, 1 = ACCENT (copper), 2 = DIM. Overridden by `col`.
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub c: u8,
    /// Optional custom colour `0xRRGGBB` (masked to 24 bits). When present, overrides `c`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub col: Option<u32>,
    /// Alignment: 0 = left, 1 = centre, 2 = right.
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub a: u8,
    /// Text literal (type 0 only).
    #[serde(default, skip_serializing_if = "str_is_empty")]
    pub v: String<MAX_TEXT_LEN>,
    /// Departure slot 0..=2 (type 1 only): the permanent data binding, soonest-first.
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub di: u8,
    /// Departure field 0=badge/1=direction/2=time (type 1 only).
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub fk: u8,
    /// Split flag (type 1 only): editor grouping state. The **firmware ignores it** — each field is
    /// drawn at its own `x,y` regardless — so connected/split is purely an editor concern.
    #[serde(default, skip_serializing_if = "is_false")]
    pub sp: bool,
    /// Divider thickness, 1..=2 (type 5 only).
    #[serde(default = "one_u8", skip_serializing_if = "is_one_u8")]
    pub th: u8,
    /// Format selector for Clock/Date (types 3/4); badge style for a Departure badge field
    /// (type 1, fk=0): 0 = filled badge box, 1 = minimal (line label only, no box).
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub f: u8,
    /// Icon glyph id (type 6 only): 0 = tram-front, 1 = Z-blind, 2 = arrow.
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub g: u8,
}

impl Layout {
    /// Defensively clamp every numeric field into its valid range (§5.5) so a hand-crafted POST can
    /// never feed an out-of-range value to the renderer. Element **count** and text length are
    /// already bounded by the heapless `Vec`/`String` at deserialize time; this bounds the scalars.
    /// Off-panel elements are left in place — the renderer clips them (`pset`) and skips fully
    /// off-panel ones — so this only normalizes, it does not drop elements.
    pub fn sanitize(&mut self) {
        for el in self.e.iter_mut() {
            el.x = el.x.min(64);
            el.y = el.y.min(64);
            el.w = el.w.min(64);
            el.s = el.s.min(1);
            el.k = el.k.clamp(1, 3);
            el.c = el.c.min(2);
            el.a = el.a.min(2);
            el.di = el.di.min(2);
            el.fk = el.fk.min(2);
            el.th = el.th.clamp(1, 2);
            el.f = el.f.min(1);
            if let Some(rgb) = el.col {
                el.col = Some(rgb & 0x00FF_FFFF);
            }
        }
    }
}

/// One upcoming departure of the saved connection, ready to render (brief §7.7).
#[derive(Clone, Debug)]
pub struct Departure {
    pub line: String<16>,
    pub category: String<12>,
    pub destination: String<48>,
    /// Minutes to departure. `None` renders as `--` (no service); `Some(0)` as `now`.
    pub minutes: Option<u16>,
}

/// Up to three departures, soonest first.
pub type Departures = Vec<Departure, 3>;

/// What the panel should be showing right now. The render task draws whichever variant
/// it last received over the [`crate::shared::DISPLAY`] signal.
#[derive(Clone, Debug)]
pub enum DisplayState {
    /// Phase 1: no WiFi saved, captive portal is up.
    Provisioning,
    /// Startup/loading animation: a tram rolls across the panel while the board joins WiFi
    /// in the background. The render loop plays at least one full pass before cutting over
    /// to whatever comes next (the board, or back to [`Provisioning`] if the join failed).
    Connecting,
    /// Joined WiFi but no connection selected yet: show the address so the user can
    /// reach the config page (brief §3.3 / §7.7). `octets` is the device IPv4.
    IdleAddress { octets: [u8; 4] },
    /// Normal runtime: the saved stop's name plus its next departures, rendered as a board —
    /// one row per departure (badge, destination, time-to-departure), soonest first. `deps` is
    /// empty when nothing matching is currently departing (the panel shows "no service").
    Departures {
        station: String<64>,
        deps: Departures,
    },
    /// Poll failed / network lost — the rolling-tram scene (as [`Connecting`](Self::Connecting))
    /// labelled "offline / reconnecting" while the poll task retries (brief §7.7).
    Offline,
}
