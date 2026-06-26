//! Minimal multicast-DNS responder so `zugli.local` resolves on the home network
//! (PROJECT_BRIEF.md §3.3).
//!
//! mDNS is a tiny corner of DNS: a host listens on UDP `224.0.0.251:5353` and, when it
//! sees a query for its own name, multicasts an `A` answer. We only own a single name
//! (`zugli.local`, brief §0/§3.3), so rather than pull in a full responder crate we
//! hand-roll one in the same spirit as the captive-portal [`crate::portal::dns_task`]:
//! parse the question, match our labels, emit one answer record. The address we hand out
//! is read live from the network stack, so it always reflects the current DHCP lease.

use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpAddress, IpEndpoint, Stack};
use embassy_time::{Duration, Timer};
use log::{info, warn};

/// Link-local mDNS multicast group + port (RFC 6762).
const MDNS_GROUP: IpAddress = IpAddress::v4(224, 0, 0, 251);
const MDNS_PORT: u16 = 5353;

/// The two labels of `zugli.local` (the trailing root label is the terminating `0x00`).
const LABELS: [&[u8]; 2] = [b"zugli", b"local"];

/// Serve `zugli.local` over mDNS for as long as the device is on the network.
///
/// Joins the multicast group, sends a couple of unsolicited announcements so resolvers
/// cache the name promptly, then answers matching queries for the rest of the run.
#[embassy_executor::task]
pub async fn mdns_task(stack: Stack<'static>) {
    // We can only answer with an address once we have one.
    stack.wait_config_up().await;

    // Join the group so smoltcp stops dropping packets addressed to 224.0.0.251. The stack
    // can briefly reject this right after config-up, so retry until it takes.
    loop {
        match stack.join_multicast_group(MDNS_GROUP) {
            Ok(()) => break,
            Err(_) => Timer::after(Duration::from_millis(500)).await,
        }
    }
    info!("mdns: serving zugli.local");

    let mut rx_meta = [PacketMetadata::EMPTY; 8];
    let mut tx_meta = [PacketMetadata::EMPTY; 8];
    let mut rx_buf = [0u8; 512];
    let mut tx_buf = [0u8; 512];
    let mut sock = UdpSocket::new(stack, &mut rx_meta, &mut rx_buf, &mut tx_meta, &mut tx_buf);
    if sock.bind(MDNS_PORT).is_err() {
        warn!("mdns: bind failed");
        return;
    }

    let dest = IpEndpoint::new(MDNS_GROUP, MDNS_PORT);
    let mut resp = [0u8; 64];

    // Unsolicited announcements (RFC 6762 §8.3): multicast our record twice ~1 s apart so
    // caches populate before the user even types the name.
    if let Some(ip) = device_ip(stack) {
        let len = build_response(&mut resp, ip);
        for _ in 0..2 {
            let _ = sock.send_to(&resp[..len], dest).await;
            Timer::after(Duration::from_secs(1)).await;
        }
    }

    let mut query = [0u8; 512];
    loop {
        let n = match sock.recv_from(&mut query).await {
            Ok((n, _meta)) => n,
            Err(_) => continue,
        };
        // Only react to queries (QR bit clear) that ask for our name.
        if n < 12 || query[2] & 0x80 != 0 || !question_matches(&query[..n]) {
            continue;
        }
        let ip = match device_ip(stack) {
            Some(ip) => ip,
            None => continue,
        };
        let len = build_response(&mut resp, ip);
        let _ = sock.send_to(&resp[..len], dest).await;
    }
}

/// Current IPv4 address from the live DHCP config, as octets.
fn device_ip(stack: Stack<'_>) -> Option<[u8; 4]> {
    stack.config_v4().map(|c| c.address.address().octets())
}

/// Does the first question name our host (`zugli.local`) and ask for an address record?
///
/// Question names in mDNS queries are uncompressed, so a linear label walk is enough.
fn question_matches(pkt: &[u8]) -> bool {
    let mut i = 12; // skip the 12-byte header to the first QNAME
    for label in LABELS {
        let len = match pkt.get(i) {
            Some(&l) => l as usize,
            None => return false,
        };
        // Reject compression pointers / the root label here — neither belongs mid-name.
        if len == 0 || len & 0xC0 != 0 || len != label.len() {
            return false;
        }
        i += 1;
        let end = i + len;
        if pkt.get(i..end).is_none_or(|s| !s.eq_ignore_ascii_case(label)) {
            return false;
        }
        i = end;
    }
    // Terminating root label, then QTYPE = A (1) or ANY (255). (QCLASS is ignored: its top
    // bit is the unicast-response request, the rest is IN; either way we answer.)
    if pkt.get(i) != Some(&0) {
        return false;
    }
    match pkt.get(i + 1..i + 3) {
        Some([hi, lo]) => {
            let qtype = u16::from_be_bytes([*hi, *lo]);
            qtype == 1 || qtype == 255
        }
        _ => false,
    }
}

/// Build the mDNS response packet (one `A` record for `zugli.local`); returns its length.
fn build_response(out: &mut [u8; 64], ip: [u8; 4]) -> usize {
    // Header: id 0, flags 0x8400 (response + authoritative), 0 questions, 1 answer.
    out[0..4].copy_from_slice(&[0x00, 0x00, 0x84, 0x00]);
    out[4..6].copy_from_slice(&[0x00, 0x00]); // qdcount
    out[6..8].copy_from_slice(&[0x00, 0x01]); // ancount
    out[8..12].copy_from_slice(&[0x00, 0x00, 0x00, 0x00]); // ns/ar count

    // Answer NAME: the full `zugli.local` (no question to compress against).
    let mut p = 12;
    for label in LABELS {
        out[p] = label.len() as u8;
        p += 1;
        out[p..p + label.len()].copy_from_slice(label);
        p += label.len();
    }
    out[p] = 0; // root label
    p += 1;

    // TYPE A, CLASS IN with the cache-flush bit (0x8001), TTL 120 s, RDLENGTH 4, the IP.
    out[p..p + 2].copy_from_slice(&[0x00, 0x01]);
    out[p + 2..p + 4].copy_from_slice(&[0x80, 0x01]);
    out[p + 4..p + 8].copy_from_slice(&[0x00, 0x00, 0x00, 0x78]);
    out[p + 8..p + 10].copy_from_slice(&[0x00, 0x04]);
    out[p + 10..p + 14].copy_from_slice(&ip);
    p + 14
}
