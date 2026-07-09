//! A small helper that bundles the four buffers an [`embassy_net::udp::UdpSocket`] borrows for
//! its lifetime: the `rx_meta`/`tx_meta` packet-metadata rings and the `rx_buf`/`tx_buf` byte
//! buffers. Every UDP-based task ([`crate::mdns`], [`crate::sntp`], and the captive portal's
//! `dhcp_task`/`dns_task`) repeated the same four `let mut` array declarations plus the
//! `UdpSocket::new` call.
//!
//! The socket only *borrows* these buffers — it can't own them, since they must outlive it in the
//! caller's own stack frame — so a helper can't hand back a ready-made socket on its own. What it
//! *can* do is collapse the four separate array declarations into one struct declaration, leaving
//! just one method call to build the socket from it.

use embassy_net::Stack;
use embassy_net::udp::{PacketMetadata, UdpSocket};

/// Backing storage for one UDP socket. `RX`/`TX` size the byte buffers; `META` sizes the
/// packet-metadata rings (embassy-net needs one metadata slot per in-flight datagram, independent
/// of the byte buffer size).
pub struct UdpBuffers<const RX: usize, const TX: usize, const META: usize> {
    rx_meta: [PacketMetadata; META],
    tx_meta: [PacketMetadata; META],
    rx_buf: [u8; RX],
    tx_buf: [u8; TX],
}

impl<const RX: usize, const TX: usize, const META: usize> UdpBuffers<RX, TX, META> {
    pub const fn new() -> Self {
        Self {
            rx_meta: [PacketMetadata::EMPTY; META],
            tx_meta: [PacketMetadata::EMPTY; META],
            rx_buf: [0u8; RX],
            tx_buf: [0u8; TX],
        }
    }

    /// Build the socket, borrowing all four buffers for as long as it lives — hence `&'a mut
    /// self`: the socket can't outlive the buffers it borrows, so this ties its lifetime to theirs.
    pub fn socket<'a>(&'a mut self, stack: Stack<'a>) -> UdpSocket<'a> {
        UdpSocket::new(
            stack,
            &mut self.rx_meta,
            &mut self.rx_buf,
            &mut self.tx_meta,
            &mut self.tx_buf,
        )
    }
}
