//! Static node id to board name map
//!
//! The discovery heartbeat is a single CAN frame, so the human friendly board name is not on the
//! wire, the host resolves it from the node id here. Mirrors `SystemCanDevice` in
//! `can/inc/system_can.h`, extend it as the fleet grows.
//!
//! - **Author:** Midnight Sun Team #24

/// Board name for a node id, empty when the id is outside the known fleet
pub fn board_name(node: u16) -> &'static str {
    match node {
        0 => "can_communication",
        1 => "front_controller",
        2 => "imu",
        3 => "rear_controller",
        4 => "steering",
        5 => "telemetry",
        _ => "",
    }
}
