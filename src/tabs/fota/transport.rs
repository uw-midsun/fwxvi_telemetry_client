//! Transport seam the DFU engine drives
//!
//! The engine in `client.rs` only ever needs four operations from a transport, so they
//! live here as a trait. This port supports the XBee serial link only (`SerialTransport`);
//! the upstream ms-bootloader client also has a SocketCAN implementation, which is
//! Linux-only and deliberately left out so the telemetry client builds on Windows.
//!
//! - **Author:** Midnight Sun Team #24

use std::time::Duration;

use anyhow::Result;

use super::protocol::{Datagram, Heartbeat, NackCode};

/// A decoded ACK / NACK frame
#[derive(Clone, Copy, Debug)]
pub struct Ack {
    pub code: NackCode,
    pub datagram_id: u32,
    pub src_node: u16,
}

/// Discovery answers: every node's heartbeat frame tagged by source node
pub type DiscoveryAnswers = Vec<(u16, Heartbeat)>;

/// The operations the engine drives, blocking request/response stop-and-wait
pub trait Transport {
    /// Serialize and fragment a datagram onto the wire
    fn send_datagram(&self, dg: &Datagram) -> Result<()>;

    /// Send the app facing ENTER control frame that drops a running app into the bootloader
    fn send_enter(&self, mask: u32) -> Result<()>;

    /// Wait up to `timeout` for an ACK frame, ignoring everything else
    fn recv_ack(&self, timeout: Duration) -> Result<Option<Ack>>;

    /// Collect discovery answers for `window`, reassembled and tagged by source node
    fn collect_discovery(&self, window: Duration) -> Result<DiscoveryAnswers>;
}
