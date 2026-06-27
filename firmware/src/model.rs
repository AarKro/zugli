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
/// to flash. Defaults are chosen so a fresh board behaves exactly as before any config exists.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Config {
    /// When `true`, drop the leading "City, " prefix from stop/destination names on the panel
    /// (e.g. "Zürich, Schlieren" → "Schlieren").
    #[serde(rename = "stripCity", default)]
    pub strip_city: bool,
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
