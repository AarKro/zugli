//! Phase 3 runtime poll (PROJECT_BRIEF.md §6.2 / §7.3).
//!
//! Every 30 s (or immediately when the selection changes) we fetch the saved stop's
//! stationboard over HTTPS, filter to the tracked connections (every departure in
//! all-connections mode, or just the user's picks otherwise), keep the next three departures by
//! time, compute minutes, and push them to the display. TLS uses `embedded-tls` with
//! `TlsVerify::None` (decision §8-4 — documented in the README).

use core::fmt::Write as _;

use alloc::vec;
use embassy_futures::select::{Either, select};
use embassy_net::Stack;
use embassy_net::dns::DnsSocket;
use embassy_net::tcp::client::{TcpClient, TcpClientState};
use embassy_time::{Duration, Instant, Timer, with_timeout};
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
    // Capacity 20, but the request asks for only `limit=16` (below): the opendata.ch API does NOT
    // honour `limit` exactly — it returns `limit + 1` or `+ 2` entries when departures share a
    // timestamp near the cutoff. serde_json_core fails the WHOLE board deserialize (`CustomError`)
    // the instant a `heapless::Vec` overflows, so the request stays well under the Vec's capacity
    // to leave headroom for that overage. Don't grow this Vec to widen the gap instead: commit
    // 78c5126 shrank it 24 -> 20 to stop a core-0 stack overflow (this struct is built on the
    // stack during the synchronous JSON parse), so the slack has to come from a smaller request.
    stationboard: Vec<Entry, 20>,
}

// Field capacities are deliberately LARGER than what we display/store (`Departure` truncates via
// `str_to` to line 16 / category 12 / destination 48). `serde_json_core` fails the WHOLE board
// deserialize if any single field overflows its `String`, so one untracked oddball — a long
// destination, or a category like "Standseilbahn" (13 > 12) — would otherwise blank every
// departure until it rolls off. Tracked-connection matching is unaffected: a connection the user
// could save is itself ≤ the `Selection` caps, so its live `to`/line still fits and compares equal.
#[derive(Deserialize)]
struct Entry {
    category: Option<String<24>>,
    number: Option<String<24>>,
    name: Option<String<24>>,
    to: Option<String<64>>,
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

    // Start of the current unbroken run of failed polls (`None` once a poll succeeds). The
    // offline screen only appears once this run reaches `OFFLINE_AFTER_SECS`; until then the
    // last good board stays up, so a transient hiccup never flashes "offline / reconnecting".
    let mut failing_since: Option<Instant> = None;

    loop {
        let selection = SELECTION.lock().await.clone();
        // How long to wait before the next poll: the normal interval after a good poll or an
        // idle screen, but a short retry after a failure so a transient hiccup recovers fast.
        let delay = match selection {
            None => {
                // No connection chosen yet: show the address screen (brief §7.7). Not polling,
                // so any earlier failure streak no longer applies.
                info!("poll: no selection set — showing idle screen, not polling");
                failing_since = None;
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
                        failing_since = None;
                        DISPLAY.signal(DisplayState::Departures {
                            station: sel.stop_name.clone(),
                            deps,
                        });
                        crate::POLL_INTERVAL_SECS
                    }
                    Ok(Err(())) => {
                        warn!("poll: fetch failed");
                        offline_if_persistent(&mut failing_since);
                        crate::POLL_RETRY_SECS
                    }
                    Err(_) => {
                        warn!("poll: fetch timed out after {FETCH_TIMEOUT_SECS}s");
                        offline_if_persistent(&mut failing_since);
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

/// Handle a failed poll: record the start of the failure streak (if this is its first failure),
/// and only push the offline screen once the streak has lasted `OFFLINE_AFTER_SECS`. Below that
/// nothing is signalled, so the last good board stays on the panel through a brief outage.
fn offline_if_persistent(failing_since: &mut Option<Instant>) {
    let now = Instant::now();
    let start = *failing_since.get_or_insert(now);
    if now.duration_since(start) >= Duration::from_secs(crate::OFFLINE_AFTER_SECS) {
        DISPLAY.signal(DisplayState::Offline);
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
        "https://transport.opendata.ch/v1/stationboard?id={}&limit=16\
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

/// Parse the stationboard body and build the up-to-three soonest departures the panel should
/// show — every connection in all-connections mode, or only the user's picks otherwise.
fn parse_departures(body: &[u8], sel: &Selection) -> Option<Departures> {
    // Scratch space for decoding JSON `\uXXXX` escapes (e.g. `ü`); must fit the longest single
    // unescaped string, so it tracks the largest field capacity above (`to`, now 64).
    let mut unescape = [0u8; 96];
    let board = match serde_json_core::from_slice_escaped::<Board>(body, &mut unescape) {
        Ok((b, _)) => b,
        // Log the specific error (e.g. an over-capacity field) so a recurring failure is
        // diagnosable rather than a generic "parse failed".
        Err(e) => {
            warn!("poll: stationboard deserialize failed: {e:?}");
            return None;
        }
    };
    let now = shared::now_unix();

    // Collect (timestamp, departure) pairs. In all-connections mode we keep every entry on the
    // board; otherwise only the ones whose `(line, destination)` is one the user picked.
    let mut matches: Vec<(i64, Departure), 20> = Vec::new();
    for e in &board.stationboard {
        let line = e
            .number
            .as_deref()
            .filter(|s| !s.is_empty())
            .or(e.name.as_deref())
            .unwrap_or("");
        let to = e.to.as_deref().unwrap_or("");
        if !tracked(sel, line, to) {
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

    // Diagnostic: an empty board (the panel's "no service") means either the clock isn't synced
    // (so minutes can't be computed — though we still list entries) or nothing on the board
    // matched the tracked connections. This says which mode is active and how much matched.
    info!(
        "poll: parsed {} entries, {} kept (mode {}, {} tracked), clock {}",
        board.stationboard.len(),
        matches.len(),
        if sel.all_connections { "all" } else { "specific" },
        sel.connections.len(),
        if now.is_some() { "set" } else { "UNSET" },
    );

    // Up to three soonest departures. Empty when nothing matched — the panel renders the stop
    // header with a "no service" note.
    let mut out: Departures = Vec::new();
    for (_, dep) in matches.into_iter().take(3) {
        let _ = out.push(dep);
    }
    Some(out)
}

/// Whether a board entry's `(line, destination)` is one the panel should show: any departure in
/// all-connections mode, otherwise only the connections the user explicitly picked.
fn tracked(sel: &Selection, line: &str, to: &str) -> bool {
    sel.all_connections
        || sel
            .connections
            .iter()
            .any(|c| c.line.as_str() == line && c.destination.as_str() == to)
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
