//! FOTA client configuration, serial (XBee) link only
//!
//! Trimmed from ms-bootloader `client/src/config.rs`: the SocketCAN transport and its
//! addressing knobs (iface, xfer id base, enter id) are host-CAN concepts that don't
//! apply to the XBee gateway path, and socketcan is Linux-only anyway.
//!
//! - **Author:** Midnight Sun Team #24

use std::time::Duration;

/// All host side knobs for the XBee serial link. Baud default matches the 230400 the
/// telemetry firmware brings up on the gateway UART.
#[derive(Clone, Debug, PartialEq)]
pub struct FotaConfig {
    pub serial_port: String,
    pub serial_baud: u32,
    pub ack_timeout: Duration,
    /// How long a node stays listed after its last announce, before it ages out of the live table
    pub discovery_window: Duration,
    pub retries: u32,
}

impl Default for FotaConfig {
    fn default() -> Self {
        Self {
            serial_port: "COM9".to_string(),
            serial_baud: 230_400,
            ack_timeout: Duration::from_millis(1000),
            // Nodes announce ~1 Hz, so keep one for ~3 missed heartbeats before dropping it
            discovery_window: Duration::from_millis(3000),
            retries: 5,
        }
    }
}
