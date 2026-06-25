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
use embassy_time::{Duration, Timer, with_timeout};
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
// We request only the five fields we actually parse (see `fetch`), which shrinks the
// stationboard response from ~100 KiB (the full payload with real-time prognosis) to ~2 KiB.
// 16 KiB leaves generous room for headers + body (the buffer lives in PSRAM).
const HTTP_BUF: usize = 16 * 1024;

/// Hard ceiling on a single fetch (DNS + connect + TLS + request + body). Anything slower is
/// treated as a failure so the poll loop can never wedge on a stalled connection.
const FETCH_TIMEOUT_SECS: u64 = 15;

// Owned (not borrowed) strings: the API JSON-escapes non-ASCII (e.g. `Zürich`), and
// serde-json-core can only *decode* those escapes into an owned target via the
// `from_slice_escaped` path below. Borrowing `&str` would keep `ü` literal, so a saved
// destination like "Zürich, Klusplatz" would never match. Capacities mirror `Selection`.
#[derive(Deserialize)]
struct Board {
    stationboard: Vec<Entry, 24>,
}

#[derive(Deserialize)]
struct Entry {
    category: Option<String<12>>,
    number: Option<String<16>>,
    name: Option<String<16>>,
    to: Option<String<48>>,
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

    // Don't poll until DHCP has actually configured the interface — fetching before the
    // network is up just burns a failed attempt and flashes the offline screen on boot.
    stack.wait_config_up().await;

    loop {
        let selection = SELECTION.lock().await.clone();
        // How long to wait before the next poll: the normal interval after a good poll or an
        // idle screen, but a short retry after a failure so a transient hiccup recovers fast.
        let delay = match selection {
            None => {
                // No connection chosen yet: show the address screen (brief §7.7).
                info!("poll: no selection set — showing idle screen, not polling");
                if let Some(octets) = shared::device_ip() {
                    DISPLAY.signal(DisplayState::IdleAddress { octets });
                }
                crate::POLL_INTERVAL_SECS
            }
            Some(sel) => {
                // Bound the whole fetch: reqwless/embassy-net put no timeout on DNS, the TCP
                // connect, or the TLS handshake, so any stall would hang the poll loop
                // forever (and, with the single-socket pool, wedge every later attempt too).
                // On timeout the future is dropped, which frees the socket, and we retry.
                let attempt = with_timeout(
                    Duration::from_secs(FETCH_TIMEOUT_SECS),
                    fetch(
                        &tcp_client,
                        &dns,
                        seed,
                        &mut read_record,
                        &mut write_record,
                        &mut http_buf,
                        &sel,
                    ),
                )
                .await;
                match attempt {
                    Ok(Ok(deps)) => {
                        info!("poll: {} departures", deps.len());
                        DISPLAY.signal(DisplayState::Departures {
                            station: sel.stop_name.clone(),
                            deps,
                        });
                        crate::POLL_INTERVAL_SECS
                    }
                    Ok(Err(())) => {
                        warn!("poll: fetch failed");
                        DISPLAY.signal(DisplayState::Offline);
                        crate::POLL_RETRY_SECS
                    }
                    Err(_) => {
                        warn!("poll: fetch timed out after {FETCH_TIMEOUT_SECS}s");
                        DISPLAY.signal(DisplayState::Offline);
                        crate::POLL_RETRY_SECS
                    }
                }
            }
        };

        // Sleep until the next interval, or wake early when the selection changes.
        match select(
            Timer::after(Duration::from_secs(delay)),
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

    // Request only the fields we parse below. Without this the full payload (real-time
    // prognosis for every entry) is ~100 KiB; the `fields[]` filter trims it to ~2 KiB.
    // `[]` is percent-encoded (`%5B%5D`) so reqwless's URL parser accepts it.
    let mut url: String<320> = String::new();
    write!(
        url,
        "https://transport.opendata.ch/v1/stationboard?id={}&limit=20\
         &fields%5B%5D=stationboard/number\
         &fields%5B%5D=stationboard/name\
         &fields%5B%5D=stationboard/category\
         &fields%5B%5D=stationboard/to\
         &fields%5B%5D=stationboard/stop/departureTimestamp",
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
    // Scratch space for decoding JSON `\uXXXX` escapes (e.g. `ü`); must fit the longest
    // single unescaped string, so it tracks the largest field capacity above.
    let mut unescape = [0u8; 64];
    let (board, _) = serde_json_core::from_slice_escaped::<Board>(body, &mut unescape).ok()?;
    let now = shared::now_unix();

    // Collect matching (timestamp, entry) pairs.
    let mut matches: Vec<(i64, Departure), 24> = Vec::new();
    for e in &board.stationboard {
        let line = e
            .number
            .as_deref()
            .filter(|s| !s.is_empty())
            .or(e.name.as_deref())
            .unwrap_or("");
        let to = e.to.as_deref().unwrap_or("");
        if line != sel.line.as_str() || to != sel.destination.as_str() {
            continue;
        }
        let ts = match e.stop.departure_timestamp {
            Some(ts) => ts,
            None => continue,
        };
        let dep = Departure {
            line: str_to(line),
            category: str_to(e.category.as_deref().unwrap_or("")),
            destination: str_to(to),
            minutes: now.map(|n| minutes_until(ts, n)),
        };
        let _ = matches.push((ts, dep));
    }

    matches.sort_unstable_by_key(|(ts, _)| *ts);

    // Diagnostic: a `--` on the panel means either the clock isn't synced (so minutes can't
    // be computed) or nothing on the board matched the saved line/destination. This says which.
    info!(
        "poll: parsed {} entries, {} matched '{} → {}', clock {}",
        board.stationboard.len(),
        matches.len(),
        sel.line.as_str(),
        sel.destination.as_str(),
        if now.is_some() { "set" } else { "UNSET" },
    );

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
