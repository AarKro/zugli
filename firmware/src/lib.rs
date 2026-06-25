#![no_std]
//! Zügli firmware library crate.
//!
//! The firmware is `no_std` embedded Rust on the ESP32-S3, async via Embassy. It drives a
//! 64×64 HUB75 LED panel and runs three phases (PROJECT_BRIEF.md §2):
//!
//! * **Phase 1 — provisioning** ([`portal`]): SoftAP captive portal to enter home WiFi.
//! * **Phase 2 — configuration** ([`httpd`]): serves the config page + `POST /save`.
//! * **Phase 3 — runtime** ([`poll`] + [`display`]): polls the transport API and renders
//!   the countdown to the next departures.
//!
//! Orchestration lives in `src/bin/main.rs`; the modules here hold the building blocks.

extern crate alloc;

pub mod display;
pub mod httpd;
pub mod model;
pub mod poll;
pub mod portal;
pub mod shared;
pub mod sntp;
pub mod storage;
pub mod wifi;

/// Open SoftAP SSID shown during provisioning (PROJECT_BRIEF.md §0/§5.1).
pub const SETUP_SSID: &str = "Zügli-Setup";
/// mDNS hostname (ASCII only — the `ü` would need punycode; brief §3.3).
pub const HOSTNAME: &str = "zugli";
/// Runtime poll cadence (brief §7.3 / §2 Phase 3).
pub const POLL_INTERVAL_SECS: u64 = 30;
/// Shorter retry cadence after a failed poll, so a transient network hiccup recovers fast.
pub const POLL_RETRY_SECS: u64 = 5;

/// Leak a value into a `'static` via a `StaticCell`. Panics if called twice for one cell.
#[macro_export]
macro_rules! mk_static {
    ($t:ty, $val:expr) => {{
        static STATIC_CELL: ::static_cell::StaticCell<$t> = ::static_cell::StaticCell::new();
        STATIC_CELL.uninit().write($val)
    }};
}
