//! Shared data types: the saved connection selection, WiFi credentials, a single
//! departure, and what the LED panel is currently showing.

use heapless::{String, Vec};
use serde::{Deserialize, Serialize};

/// The connection the user picked, persisted to flash and matched against the
/// stationboard at runtime. Mirrors the `POST /save` body (PROJECT_BRIEF.md §4.4).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Selection {
    /// API location `id` of the chosen stop.
    #[serde(rename = "stopId")]
    pub stop_id: String<24>,
    /// Human-readable stop name (display/echo only).
    #[serde(rename = "stopName")]
    pub stop_name: String<64>,
    /// Line number/name (API `number`), e.g. `2`, `S12`.
    pub line: String<16>,
    /// Raw API `category` (drives the badge; kept for future styling).
    pub category: String<12>,
    /// Final destination (API `to`), e.g. `Schlieren`.
    pub destination: String<48>,
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
    /// Manual brightness level, 1–10, mapping linearly to 10–100 % panel brightness.
    #[serde(rename = "brightness")]
    pub brightness: u8,
    /// When `true`, the panel auto-dims to 10 % during the reduced window (local time);
    /// otherwise it stays at the manual [`brightness`](Config::brightness) level all day.
    #[serde(rename = "autoBrightness")]
    pub auto_brightness: bool,
    /// Start/end of the reduced-brightness window, as minutes since local midnight (e.g.
    /// 20:00 = 1200). The window wraps past midnight when `start > end`.
    #[serde(rename = "reducedStart")]
    pub reduced_start: u16,
    #[serde(rename = "reducedEnd")]
    pub reduced_end: u16,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            strip_city: false,
            brightness: 6,          // 60 %
            auto_brightness: true,  // preserve the board's existing night-dimming behaviour
            reduced_start: 20 * 60, // 20:00
            reduced_end: 8 * 60,    // 08:00
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
    /// Normal runtime: the saved stop's name plus its next departures (the `deps` vec may
    /// hold a single `--` entry when there's no upcoming service).
    Departures {
        station: String<64>,
        deps: Departures,
    },
    /// Poll failed / network lost — subtle offline indicator (brief §7.7).
    Offline,
}
