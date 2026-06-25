//! WiFi helpers: the embassy-net runner task, STA connection management, and the SoftAP /
//! station configuration builders (PROJECT_BRIEF.md §5 / §7.3).

extern crate alloc;

use alloc::vec::Vec;

use esp_radio::wifi::ap::{AccessPointConfig, AccessPointInfo};
use esp_radio::wifi::scan::ScanConfig;
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{AuthenticationMethod, Config as WifiConfig, Interface, WifiController};
use log::{info, warn};

use crate::model::WifiCreds;

/// The embassy-net driver — the same concrete type for both STA and AP interfaces.
pub type WifiDevice = Interface<'static>;

/// Drives the embassy-net stack. One instance per active interface.
#[embassy_executor::task]
pub async fn net_task(mut runner: embassy_net::Runner<'static, WifiDevice>) -> ! {
    runner.run().await
}

/// Build a station config for the home network. `auth` is the connect threshold (usually
/// the scanned AP's advertised auth method); an empty password forces an open network.
pub fn sta_config(creds: &WifiCreds, auth: AuthenticationMethod) -> StationConfig {
    let auth = if creds.password.is_empty() {
        AuthenticationMethod::None
    } else {
        auth
    };
    StationConfig::default()
        .with_ssid(creds.ssid.as_str())
        .with_password(alloc::string::String::from(creds.password.as_str()))
        .with_auth_method(auth)
}

/// Build the open SoftAP config used by the captive portal (brief §5.1, §8-2).
pub fn ap_config() -> AccessPointConfig {
    AccessPointConfig::default()
        .with_ssid(crate::SETUP_SSID)
        .with_auth_method(AuthenticationMethod::None)
}

/// Keep the station connected: (re)connect whenever the link drops (brief §7.7 "WiFi lost").
/// The controller must already have a station config applied.
#[embassy_executor::task]
pub async fn sta_connection_task(mut controller: WifiController<'static>) {
    use embassy_time::{Duration, Timer};
    loop {
        if controller.is_connected() {
            Timer::after(Duration::from_secs(5)).await;
            continue;
        }
        match controller.connect_async().await {
            Ok(_) => info!("wifi: connected"),
            Err(e) => {
                warn!("wifi: connect failed: {e:?}");
                Timer::after(Duration::from_secs(5)).await;
            }
        }
    }
}

/// Scan for nearby access points (used by the captive portal's network list).
pub async fn scan(controller: &mut WifiController<'static>) -> Vec<AccessPointInfo> {
    match controller.scan_async(&ScanConfig::default()).await {
        Ok(found) => found,
        Err(e) => {
            warn!("wifi: scan failed: {e:?}");
            Vec::new()
        }
    }
}

/// Apply a station-only config and start the radio. Phase 2 runs STA-only (no SoftAP), so
/// there is no channel conflict; a WPA2 threshold also admits WPA2/WPA3 (transitional) APs.
pub fn apply_sta(controller: &mut WifiController<'static>, creds: &WifiCreds) -> Result<(), ()> {
    controller
        .set_config(&WifiConfig::Station(sta_config(
            creds,
            AuthenticationMethod::Wpa2Personal,
        )))
        .map_err(|_| ())
}
