//! Open-Meteo current-weather fetch for the custom layout's Weather element (t=7).
//!
//! Piggybacks on the runtime poll loop rather than running as its own task: [`poll::poll_task`]
//! (crate::poll) calls [`WeatherPoll::refresh_if_due`] once per cycle with its own TCP client and
//! PSRAM scratch buffers, so weather costs no extra socket state or internal RAM. The fetch only
//! runs while a layout that is actually on the panel contains a Weather element
//! ([`shared::weather_element_active`]), the saved stop carries coordinates, and the last sample
//! has aged past [`crate::WEATHER_REFRESH_SECS`]. Results land in the [`shared`] weather mirror
//! for the render task; no display state is pushed from here.

use core::fmt::Write as _;

use embassy_net::dns::DnsSocket;
use embassy_net::tcp::client::TcpClient;
use embassy_time::{Duration, Instant, with_timeout};
use heapless::String;
use log::{info, warn};
use reqwless::client::{HttpClient, TlsConfig, TlsVerify};
use reqwless::request::Method;
use serde::Deserialize;

use crate::model::Weather;
use crate::shared;

/// Hard ceiling on a single weather fetch, mirroring the stationboard fetch's bound so a stalled
/// connection can never wedge the shared poll loop.
const FETCH_TIMEOUT_SECS: u64 = 15;

// The response envelope: we only parse the `current` block the request asks for; the metadata
// around it (units, coordinates, generation time) is skipped by serde.
#[derive(Deserialize)]
struct Forecast {
    current: Current,
}

#[derive(Deserialize)]
struct Current {
    #[serde(rename = "temperature_2m")]
    temperature: f32,
    /// WMO weather interpretation code, 0–99.
    #[serde(rename = "weather_code")]
    code: u8,
}

/// Cadence state for the weather refresh — just the last *attempt* time, so a failing fetch backs
/// off to [`crate::WEATHER_RETRY_SECS`] instead of re-running every 30 s poll cycle. Success
/// freshness lives in the [`shared`] mirror ([`shared::weather_is_fresh`]).
pub struct WeatherPoll {
    last_attempt: Option<Instant>,
}

impl WeatherPoll {
    pub fn new() -> Self {
        Self { last_attempt: None }
    }

    /// Fetch the current weather for `(lat, lon)` if a refresh is due, publishing a success via
    /// [`shared::set_weather`]. A no-op when no Weather element is live, coordinates are missing
    /// (older saves — the element then draws nothing), the sample is still fresh, or the last
    /// attempt was under [`crate::WEATHER_RETRY_SECS`] ago. Failures only log: the mirror keeps
    /// the previous sample until it ages out ([`crate::WEATHER_STALE_SECS`]).
    #[allow(clippy::too_many_arguments)]
    pub async fn refresh_if_due(
        &mut self,
        tcp_client: &TcpClient<'_, 1, 4096, 4096>,
        dns: &DnsSocket<'_>,
        seed: u64,
        read_record: &mut [u8],
        write_record: &mut [u8],
        http_buf: &mut [u8],
        lat: Option<f32>,
        lon: Option<f32>,
    ) {
        let (Some(lat), Some(lon)) = (lat, lon) else {
            return;
        };
        if !shared::weather_element_active() || shared::weather_is_fresh() {
            return;
        }
        let now = Instant::now();
        if self
            .last_attempt
            .is_some_and(|t| now.duration_since(t) < Duration::from_secs(crate::WEATHER_RETRY_SECS))
        {
            return;
        }
        self.last_attempt = Some(now);

        let attempt = with_timeout(
            Duration::from_secs(FETCH_TIMEOUT_SECS),
            fetch(tcp_client, dns, seed, read_record, write_record, http_buf, lat, lon),
        )
        .await;
        match attempt {
            Ok(Ok(w)) => {
                let (whole, frac) = (w.deci_celsius / 10, (w.deci_celsius % 10).abs());
                info!("weather: {whole}.{frac}°C, code {}", w.code);
                shared::set_weather(w);
            }
            Ok(Err(())) => warn!("weather: fetch failed"),
            Err(_) => warn!("weather: fetch timed out after {FETCH_TIMEOUT_SECS}s"),
        }
    }
}

impl Default for WeatherPoll {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(clippy::too_many_arguments)]
async fn fetch(
    tcp_client: &TcpClient<'_, 1, 4096, 4096>,
    dns: &DnsSocket<'_>,
    seed: u64,
    read_record: &mut [u8],
    write_record: &mut [u8],
    http_buf: &mut [u8],
    lat: f32,
    lon: f32,
) -> Result<Weather, ()> {
    let tls = TlsConfig::new(seed, read_record, write_record, TlsVerify::None);
    let mut client = HttpClient::new_with_tls(tcp_client, dns, tls);

    // Ask for exactly the two current fields the element renders; the response stays a few
    // hundred bytes (vs. the multi-KB default forecast payload).
    let mut url: String<160> = String::new();
    write!(
        url,
        "https://api.open-meteo.com/v1/forecast?latitude={lat}&longitude={lon}\
         &current=temperature_2m,weather_code"
    )
    .map_err(|_| ())?;

    // Each step logged separately, like the stationboard fetch, so failures are attributable.
    let mut req = match client.request(Method::GET, url.as_str()).await {
        Ok(r) => r,
        Err(e) => {
            warn!("weather: connect/TLS failed: {e:?}");
            return Err(());
        }
    };
    let resp = match req.send(http_buf).await {
        Ok(r) => r,
        Err(e) => {
            warn!("weather: send/headers failed: {e:?}");
            return Err(());
        }
    };
    let status = resp.status;
    let body = match resp.body().read_to_end().await {
        Ok(b) => b,
        Err(e) => {
            warn!("weather: body read failed (status {status:?}): {e:?}");
            return Err(());
        }
    };

    let forecast = match serde_json_core::from_slice::<Forecast>(body) {
        Ok((f, _)) => f,
        Err(e) => {
            warn!("weather: deserialize failed ({} bytes): {e:?}", body.len());
            return Err(());
        }
    };
    let t = forecast.current.temperature;
    // Round to deci-°C half away from zero; `as i16` then truncates the pre-offset value.
    let deci_celsius = (t * 10.0 + if t >= 0.0 { 0.5 } else { -0.5 }) as i16;
    Ok(Weather {
        deci_celsius,
        code: forecast.current.code,
    })
}
