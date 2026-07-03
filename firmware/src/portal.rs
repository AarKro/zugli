//! Phase 1 captive portal (PROJECT_BRIEF.md §5).
//!
//! Four services run on the open `Zügli-Setup` SoftAP at 192.168.4.1:
//! * a DHCP server ([`dhcp_task`]) that hands the phone an address,
//! * a DNS catch-all ([`dns_task`]) that points every name at the device so the OS captive
//!   check pops the portal,
//! * an HTTP server ([`setup_server_task`]) serving the setup page + `/scan` + `/connect`,
//! * a WiFi manager ([`portal_wifi_task`]) that scans and tries to join the chosen network.
//!
//! On a successful join we persist the credentials and reboot into STA mode (Phase 2).

use core::fmt::Write as _;

use embassy_futures::select::{Either, select};
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpAddress, IpEndpoint, Stack};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use heapless::{String, Vec};
use log::{info, warn};
use picoserve::extract::Json;
use picoserve::response::{IntoResponse, Response};
use picoserve::routing::{get, post};
use picoserve::Router;

use crate::httpd::{html, json};
use crate::model::WifiCreds;
use crate::storage;
use crate::wifi::{self, WifiDevice};
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{AuthenticationMethod, Config as WifiConfig, WifiController};

const SETUP_HTML: &str = include_str!("../../web/setup.html");

/// AP gateway / portal address.
const AP_IP: [u8; 4] = [192, 168, 4, 1];

/// One scanned network for the setup page list.
#[derive(Clone)]
struct NetInfo {
    ssid: String<32>,
    rssi: i8,
    secure: bool,
    /// Channel the AP is on — used to co-locate the SoftAP so the join works (see below).
    channel: u8,
    /// Advertised auth method, used as the connect threshold.
    auth: Option<AuthenticationMethod>,
}

/// Latest scan results, served by `GET /scan`.
static SCAN_CACHE: Mutex<CriticalSectionRawMutex, Vec<NetInfo, 16>> = Mutex::new(Vec::new());
/// Credentials submitted by `POST /connect`, handed to the WiFi manager.
static CONNECT_REQ: Signal<CriticalSectionRawMutex, WifiCreds> = Signal::new();
/// Pulsed by the "Scan again" button.
static RESCAN_REQ: Signal<CriticalSectionRawMutex, ()> = Signal::new();

// ---------------------------------------------------------------------------------------
// WiFi manager: scan + join (brief §5.1)
// ---------------------------------------------------------------------------------------

async fn refresh_scan(controller: &mut WifiController<'static>) {
    let found = wifi::scan(controller).await;
    let mut cache = SCAN_CACHE.lock().await;
    // Merge results by SSID rather than clearing: the boot scan (STA-only) sees every
    // channel, but a later rescan with the AP up is pinned to the AP's channel and would
    // otherwise drop networks it couldn't reach. Merging means a rescan only ever adds.
    for ap in found.iter() {
        let mut ssid: String<32> = String::new();
        let _ = ssid.push_str(ap.ssid.as_str());
        if ssid.is_empty() {
            continue;
        }
        let secure = !matches!(ap.auth_method, None | Some(AuthenticationMethod::None));
        if let Some(existing) = cache.iter_mut().find(|n| n.ssid == ssid) {
            existing.rssi = ap.signal_strength;
            existing.secure = secure;
            existing.channel = ap.channel;
            existing.auth = ap.auth_method;
        } else if !cache.is_full() {
            let _ = cache.push(NetInfo {
                ssid,
                rssi: ap.signal_strength,
                secure,
                channel: ap.channel,
                auth: ap.auth_method,
            });
        }
    }
    info!("portal: {} networks cached", cache.len());
}

#[embassy_executor::task]
pub async fn portal_wifi_task(mut controller: WifiController<'static>) {
    // Full, all-channel scan in STA-only mode FIRST, before the SoftAP is up. With the AP
    // running the radio is pinned to its channel, so scans miss networks on other channels
    // (the classic "only sees the neighbour" symptom). No client is connected yet, so
    // dropping straight into a clean STA scan here is safe.
    let _ = controller.set_config(&WifiConfig::Station(StationConfig::default()));
    refresh_scan(&mut controller).await;

    // Now bring up the SoftAP (with an idle STA so we can still test-join later).
    let ap = wifi::ap_config();
    let _ = controller.set_config(&WifiConfig::AccessPointStation(StationConfig::default(), ap));

    loop {
        match select(CONNECT_REQ.wait(), RESCAN_REQ.wait()).await {
            Either::First(creds) => {
                info!("portal: trying to join {}", creds.ssid.as_str());

                // The handler already 200'd the phone; give that response a moment to flush
                // before we tear the SoftAP down (the join below disconnects the phone).
                Timer::after(Duration::from_millis(800)).await;

                // Use the network's advertised auth (from the scan) as the connect threshold.
                let auth = {
                    let cache = SCAN_CACHE.lock().await;
                    cache
                        .iter()
                        .find(|n| n.ssid.as_str() == creds.ssid.as_str())
                        .and_then(|n| n.auth)
                        .unwrap_or(AuthenticationMethod::Wpa2Personal)
                };

                // Join in STA-only mode: with the SoftAP down the radio is free to use the
                // router's channel, which is far more reliable than AP+STA channel juggling.
                let _ = controller.set_config(&WifiConfig::Station(wifi::sta_config(&creds, auth)));
                let mut ok = false;
                for attempt in 0..4 {
                    if controller.connect_async().await.is_ok() {
                        ok = true;
                        break;
                    }
                    warn!("portal: join attempt {} failed", attempt + 1);
                    Timer::after(Duration::from_secs(1)).await;
                }

                if ok {
                    info!("portal: joined; saving creds + rebooting");
                    // A failed save is non-fatal: the reboot then lands back in the portal.
                    storage::persist("portal: wifi creds", |s| s.save_wifi(&creds)).await;
                    Timer::after(Duration::from_secs(1)).await;
                    esp_hal::system::software_reset();
                } else {
                    // Bad password / unreachable: bring the portal back so the user retries.
                    warn!("portal: all join attempts failed; re-advertising Zügli-Setup");
                    let ap = wifi::ap_config();
                    let _ = controller.set_config(&WifiConfig::AccessPointStation(
                        StationConfig::default(),
                        ap,
                    ));
                }
            }
            Either::Second(_) => refresh_scan(&mut controller).await,
        }
    }
}

// ---------------------------------------------------------------------------------------
// DHCP server (brief §5.2) — edge-dhcp packet codec driven over an embassy-net UDP socket.
// ---------------------------------------------------------------------------------------

#[embassy_executor::task]
pub async fn dhcp_task(stack: Stack<'static>) {
    use edge_dhcp::server::{Server, ServerOptions};
    use edge_dhcp::{Ipv4Addr as DhcpIpv4, Options, Packet};

    let mut rx_meta = [PacketMetadata::EMPTY; 8];
    let mut tx_meta = [PacketMetadata::EMPTY; 8];
    let mut rx_buf = [0u8; 1024];
    let mut tx_buf = [0u8; 1024];
    let mut sock = UdpSocket::new(stack, &mut rx_meta, &mut rx_buf, &mut tx_meta, &mut tx_buf);
    if sock.bind(67).is_err() {
        warn!("dhcp: bind failed");
        return;
    }

    let ip = DhcpIpv4::new(AP_IP[0], AP_IP[1], AP_IP[2], AP_IP[3]);
    let mut server = Server::<_, 16>::new(|| Instant::now().as_secs(), ip);
    let mut gw = [ip];
    let opts = ServerOptions::new(ip, Some(&mut gw));

    let mut packet = [0u8; 1024];
    let mut out = [0u8; 1024];
    loop {
        let (n, _meta) = match sock.recv_from(&mut packet).await {
            Ok(v) => v,
            Err(_) => continue,
        };
        let request = match Packet::decode(&packet[..n]) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let mut opt_buf = Options::buf();
        if let Some(reply) = server.handle_request(&mut opt_buf, &opts, &request) {
            if let Ok(bytes) = reply.encode(&mut out) {
                // DHCP clients have no address yet → broadcast the reply to :68.
                let dest = IpEndpoint::new(IpAddress::v4(255, 255, 255, 255), 68);
                let _ = sock.send_to(bytes, dest).await;
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// DNS catch-all (brief §5.2) — answer every A query with the portal IP.
// ---------------------------------------------------------------------------------------

#[embassy_executor::task]
pub async fn dns_task(stack: Stack<'static>) {
    let mut rx_meta = [PacketMetadata::EMPTY; 8];
    let mut tx_meta = [PacketMetadata::EMPTY; 8];
    let mut rx_buf = [0u8; 512];
    let mut tx_buf = [0u8; 512];
    let mut sock = UdpSocket::new(stack, &mut rx_meta, &mut rx_buf, &mut tx_meta, &mut tx_buf);
    if sock.bind(53).is_err() {
        warn!("dns: bind failed");
        return;
    }

    let mut query = [0u8; 512];
    let mut resp = [0u8; 512];
    loop {
        let (n, meta) = match sock.recv_from(&mut query).await {
            Ok(v) => v,
            Err(_) => continue,
        };
        if n < 12 {
            continue;
        }
        // Find the end of the (single) question: the QNAME terminator + QTYPE/QCLASS.
        let mut i = 12;
        while i < n && query[i] != 0 {
            i += 1;
        }
        let qend = i + 1 + 4; // 0x00 + qtype(2) + qclass(2)
        if qend > n || qend + 16 > resp.len() {
            continue;
        }

        // Header: copy id, set "response + recursion available", 1 question, 1 answer.
        resp[0] = query[0];
        resp[1] = query[1];
        resp[2] = 0x81;
        resp[3] = 0x80;
        resp[4..6].copy_from_slice(&query[4..6]); // qdcount
        resp[6..8].copy_from_slice(&[0, 1]); // ancount
        resp[8..12].copy_from_slice(&[0, 0, 0, 0]);
        // Question, copied verbatim.
        resp[12..qend].copy_from_slice(&query[12..qend]);
        // Answer: name pointer → 0x0C, type A, class IN, TTL 60, rdlength 4, AP IP.
        let mut p = qend;
        resp[p..p + 2].copy_from_slice(&[0xC0, 0x0C]);
        resp[p + 2..p + 4].copy_from_slice(&[0, 1]);
        resp[p + 4..p + 6].copy_from_slice(&[0, 1]);
        resp[p + 6..p + 10].copy_from_slice(&[0, 0, 0, 60]);
        resp[p + 10..p + 12].copy_from_slice(&[0, 4]);
        resp[p + 12..p + 16].copy_from_slice(&AP_IP);
        p += 16;

        let _ = sock.send_to(&resp[..p], meta.endpoint).await;
    }
}

// ---------------------------------------------------------------------------------------
// HTTP setup server (brief §5.3)
// ---------------------------------------------------------------------------------------

async fn setup_index() -> impl IntoResponse {
    Response::ok(html(SETUP_HTML))
}

async fn scan_handler() -> impl IntoResponse {
    // Kick off a fresh background rescan (merged into the cache, never destructive), then
    // return whatever we have right now. "Scan again" picks up the new results next tap.
    RESCAN_REQ.signal(());
    let cache = SCAN_CACHE.lock().await;
    let mut out: String<768> = String::new();
    let _ = out.push('[');
    for (i, n) in cache.iter().enumerate() {
        if i > 0 {
            let _ = out.push(',');
        }
        let _ = out.push_str("{\"ssid\":\"");
        for c in n.ssid.chars() {
            if c == '"' || c == '\\' {
                let _ = out.push('\\');
            }
            let _ = out.push(c);
        }
        let _ = write!(out, "\",\"rssi\":{},\"secure\":{}}}", n.rssi, n.secure);
    }
    let _ = out.push(']');
    Response::ok(out)
}

async fn connect_handler(Json(creds): Json<WifiCreds>) -> impl IntoResponse {
    // Acknowledge receipt now, then hand off to the WiFi manager. We can't return the real
    // join result: attempting it drops the SoftAP, so this page loses contact either way.
    // The page tells the user how to check both outcomes (see web/setup.html).
    CONNECT_REQ.signal(creds);
    Response::ok(json("{\"ok\":true}"))
}

#[embassy_executor::task]
pub async fn setup_server_task(stack: Stack<'static>) {
    // Route the page on "/" and on the common OS captive-portal probe paths so the portal
    // pops automatically (brief §5.2).
    let app = Router::new()
        .route("/", get(setup_index))
        .route("/generate_204", get(setup_index))
        .route("/gen_204", get(setup_index))
        .route("/hotspot-detect.html", get(setup_index))
        .route("/ncsi.txt", get(setup_index))
        .route("/connecttest.txt", get(setup_index))
        .route("/scan", get(scan_handler))
        .route("/connect", post(connect_handler));
    crate::httpd::serve(&app, "portal", stack).await
}

/// Create the static IPv4 config for the SoftAP (192.168.4.1/24).
pub fn ap_net_config() -> embassy_net::Config {
    use embassy_net::{Ipv4Address, Ipv4Cidr, StaticConfigV4};
    embassy_net::Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(Ipv4Address::new(AP_IP[0], AP_IP[1], AP_IP[2], AP_IP[3]), 24),
        gateway: Some(Ipv4Address::new(AP_IP[0], AP_IP[1], AP_IP[2], AP_IP[3])),
        dns_servers: Default::default(),
    })
}

/// Drives the AP-side embassy-net stack.
pub type ApDevice = WifiDevice;
