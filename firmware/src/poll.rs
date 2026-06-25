//! Phase 3 runtime poll (PROJECT_BRIEF.md §6.2 / §7.3).
//!
//! Every 30 s (or immediately when the selection changes) we fetch the saved stop's
//! stationboard over HTTPS, filter to the saved `(line, destination)`, keep the next three
//! departures by time, compute minutes, and push them to the display. TLS uses
//! `embedded-tls` with `TlsVerify::None` (decision §8-4 — documented in the README).

use core::fmt::Write as _;

use alloc::vec;
use embassy_futures::select::{Either, select};
use embassy_net::Stack;
use embassy_net::dns::DnsSocket;
use embassy_net::tcp::client::{TcpClient, TcpClientState};
use embassy_time::{Duration, Timer};
use heapless::{String, Vec};
use log::{info, warn};
use reqwless::client::{HttpClient, TlsConfig, TlsVerify};
use reqwless::request::Method;
use serde::Deserialize;

use crate::model::{Departure, Departures, DisplayState, Selection};
use crate::shared::{self, DISPLAY, SELECTION, SELECTION_CHANGED};

// TLS record buffers and the HTTP response buffer, allocated once on the heap to keep them
// off the task stack. The read record must hold the server's certificate (we receive but
// don't verify it, §8-4), so it stays large; the write side and body buffer are smaller.
const TLS_READ_BUF: usize = 16 * 1024;
const TLS_WRITE_BUF: usize = 4 * 1024;
// The stationboard JSON for limit=20 with real-time prognosis can exceed 20 KiB; give the
// body buffer plenty of room (it lives in PSRAM).
const HTTP_BUF: usize = 48 * 1024;

#[derive(Deserialize)]
struct Board<'a> {
    #[serde(borrow)]
    stationboard: Vec<Entry<'a>, 24>,
}

#[derive(Deserialize)]
struct Entry<'a> {
    category: Option<&'a str>,
    number: Option<&'a str>,
    name: Option<&'a str>,
    to: Option<&'a str>,
    stop: EntryStop,
}

#[derive(Deserialize)]
struct EntryStop {
    #[serde(rename = "departureTimestamp")]
    departure_timestamp: Option<i64>,
}

/// The poll loop. Runs forever; reads the live [`SELECTION`] each cycle.
#[embassy_executor::task]
pub async fn poll_task(stack: Stack<'static>, seed: u64) {
    let tcp_state = TcpClientState::<1, 4096, 4096>::new();
    let tcp_client = TcpClient::new(stack, &tcp_state);
    let dns = DnsSocket::new(stack);

    let mut read_record = vec![0u8; TLS_READ_BUF];
    let mut write_record = vec![0u8; TLS_WRITE_BUF];
    let mut http_buf = vec![0u8; HTTP_BUF];

    loop {
        let selection = SELECTION.lock().await.clone();
        match selection {
            None => {
                // No connection chosen yet: show the address screen (brief §7.7).
                if let Some(octets) = shared::device_ip() {
                    DISPLAY.signal(DisplayState::IdleAddress { octets });
                }
            }
            Some(sel) => {
                match fetch(
                    &tcp_client,
                    &dns,
                    seed,
                    &mut read_record,
                    &mut write_record,
                    &mut http_buf,
                    &sel,
                )
                .await
                {
                    Ok(deps) => {
                        info!("poll: {} departures", deps.len());
                        DISPLAY.signal(DisplayState::Departures(deps));
                    }
                    Err(_) => {
                        warn!("poll: fetch failed");
                        DISPLAY.signal(DisplayState::Offline);
                    }
                }
            }
        }

        // Sleep until the next interval, or wake early when the selection changes.
        match select(
            Timer::after(Duration::from_secs(crate::POLL_INTERVAL_SECS)),
            SELECTION_CHANGED.wait(),
        )
        .await
        {
            Either::First(_) => {}
            Either::Second(_) => info!("poll: selection changed, re-polling"),
        }
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
    sel: &Selection,
) -> Result<Departures, ()> {
    let tls = TlsConfig::new(seed, read_record, write_record, TlsVerify::None);
    let mut client = HttpClient::new_with_tls(tcp_client, dns, tls);

    let mut url: String<160> = String::new();
    write!(
        url,
        "https://transport.opendata.ch/v1/stationboard?id={}&limit=20",
        sel.stop_id.as_str()
    )
    .map_err(|_| ())?;

    // Each step is logged separately so a failure points at connect/TLS vs send vs body.
    let mut req = match client.request(Method::GET, url.as_str()).await {
        Ok(r) => r,
        Err(e) => {
            warn!("poll: connect/TLS failed: {e:?}");
            return Err(());
        }
    };
    let resp = match req.send(http_buf).await {
        Ok(r) => r,
        Err(e) => {
            warn!("poll: send/headers failed: {e:?}");
            return Err(());
        }
    };
    let status = resp.status;
    let body = match resp.body().read_to_end().await {
        Ok(b) => b,
        Err(e) => {
            warn!("poll: body read failed (status {status:?}): {e:?}");
            return Err(());
        }
    };

    match parse_departures(body, sel) {
        Some(d) => Ok(d),
        None => {
            warn!("poll: JSON parse failed ({} bytes)", body.len());
            Err(())
        }
    }
}

/// Parse the stationboard body and build up to three departures for the saved connection.
fn parse_departures(body: &[u8], sel: &Selection) -> Option<Departures> {
    let (board, _) = serde_json_core::from_slice::<Board>(body).ok()?;
    let now = shared::now_unix();

    // Collect matching (timestamp, entry) pairs.
    let mut matches: Vec<(i64, Departure), 24> = Vec::new();
    for e in &board.stationboard {
        let line = e.number.or(e.name).unwrap_or("");
        let to = e.to.unwrap_or("");
        if line != sel.line.as_str() || to != sel.destination.as_str() {
            continue;
        }
        let ts = match e.stop.departure_timestamp {
            Some(ts) => ts,
            None => continue,
        };
        let dep = Departure {
            line: str_to(line),
            category: str_to(e.category.unwrap_or("")),
            destination: str_to(to),
            minutes: now.map(|n| minutes_until(ts, n)),
        };
        let _ = matches.push((ts, dep));
    }

    matches.sort_unstable_by_key(|(ts, _)| *ts);

    let mut out: Departures = Vec::new();
    if matches.is_empty() {
        // No matching departure on the board → show "<line> <dest> --" (brief §7.7).
        let _ = out.push(Departure {
            line: sel.line.clone(),
            category: sel.category.clone(),
            destination: sel.destination.clone(),
            minutes: None,
        });
        return Some(out);
    }
    for (_, dep) in matches.into_iter().take(3) {
        let _ = out.push(dep);
    }
    Some(out)
}

fn minutes_until(departure: i64, now: i64) -> u16 {
    let diff = departure - now;
    if diff <= 0 {
        0
    } else {
        ((diff + 30) / 60).clamp(0, u16::MAX as i64) as u16
    }
}

fn str_to<const N: usize>(s: &str) -> String<N> {
    let mut out = String::new();
    // Truncate to capacity; departures are short labels.
    for c in s.chars() {
        if out.push(c).is_err() {
            break;
        }
    }
    out
}
