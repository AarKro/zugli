//! Minimal SNTP client over an embassy-net UDP socket (PROJECT_BRIEF.md §7.4).
//!
//! We only need a one-shot Unix time to compute minutes-to-departure, so rather than pull
//! in a full NTP crate we send a single 48-byte NTP request and read the transmit
//! timestamp from the reply. Absolute Unix time means no timezone math is needed (§6.2).

use embassy_net::dns::DnsQueryType;
use embassy_net::{IpAddress, IpEndpoint, Stack};
use embassy_time::{Duration, with_timeout};
use log::{info, warn};

use crate::udp::UdpBuffers;

/// Seconds between the NTP epoch (1900) and the Unix epoch (1970).
const NTP_TO_UNIX: i64 = 2_208_988_800;
const NTP_SERVER: &str = "pool.ntp.org";

/// Query an NTP server once and return Unix time in seconds, or `None` on failure.
pub async fn sync(stack: Stack<'_>) -> Option<i64> {
    let addrs = stack.dns_query(NTP_SERVER, DnsQueryType::A).await.ok()?;
    let ip = addrs.into_iter().find_map(|a| match a {
        IpAddress::Ipv4(v) => Some(v),
        #[allow(unreachable_patterns)]
        _ => None,
    })?;

    let mut bufs = UdpBuffers::<128, 128, 4>::new();
    let mut sock = bufs.socket(stack);
    sock.bind(0).ok()?;

    // LI = 0, VN = 3, Mode = 3 (client).
    let mut req = [0u8; 48];
    req[0] = 0x1B;
    let server = IpEndpoint::new(IpAddress::Ipv4(ip), 123);
    if sock.send_to(&req, server).await.is_err() {
        warn!("sntp: send failed");
        return None;
    }

    let mut resp = [0u8; 48];
    let n = match with_timeout(Duration::from_secs(5), sock.recv_from(&mut resp)).await {
        Ok(Ok((n, _))) => n,
        _ => {
            warn!("sntp: no reply");
            return None;
        }
    };
    if n < 48 {
        return None;
    }
    // Transmit timestamp seconds live at bytes 40..44 (big-endian).
    let secs = u32::from_be_bytes([resp[40], resp[41], resp[42], resp[43]]) as i64;
    let unix = secs - NTP_TO_UNIX;
    info!("sntp: unix time = {unix}");
    Some(unix)
}
