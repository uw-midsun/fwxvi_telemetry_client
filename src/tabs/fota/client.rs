//! Bootloader DFU and discovery engine, stop-and-wait over the transport
//!
//! UI agnostic: long running jobs report structured `Progress` through a callback and
//! honour a cancel predicate, so the same engine drives the GUI worker thread.
//!
//! Ported from ms-bootloader `client/src/client.rs` (tracing swapped for stderr).
//!
//! - **Author:** Midnight Sun Team #24

use std::collections::BTreeSet;

use anyhow::{bail, Result};

use super::config::FotaConfig;
use super::protocol::{
    Datagram, DatagramType, FirmwareMetadata, Heartbeat, NackCode, IDENTITY_MODE_APPLICATION,
    MAX_DATAGRAM_SIZE,
};
use super::transport::{DiscoveryAnswers, Transport};

/// Datagram id the metadata uses, chunks then run consecutively from id + 1
const SESSION_BASE_ID: u32 = 0;

fn mask_of(nodes: &BTreeSet<u16>) -> u64 {
    nodes.iter().fold(0u64, |m, &n| m | (1u64 << n))
}

// == Progress reporting ======================================================

/// Structured progress emitted by long running jobs. A front end maps these to its
/// own events, the worker turns them into channel messages for the GUI.
#[derive(Clone, Debug)]
pub enum Progress {
    /// A human readable phase change, e.g. "metadata accepted"
    Stage(String),
    /// One image chunk acknowledged by every target
    Chunk {
        done: usize,
        total: usize,
        bytes: usize,
    },
    /// A node acknowledged the current datagram
    NodeAck { node: u16 },
    /// A node reported a NACK, transient ones are retried
    NodeNack {
        node: u16,
        code: NackCode,
        transient: bool,
    },
    /// The datagram is being resent to the laggards
    Retry {
        attempt: u32,
        of: u32,
        pending: Vec<u16>,
    },
}

// == Reliable delivery =======================================================

/// Send a datagram and collect an OK ACK from every expected node, narrowing the mask
/// to the laggards and resending on timeout. A fatal NACK aborts the whole operation.
fn deliver(
    link: &dyn Transport,
    cfg: &FotaConfig,
    dg: &mut Datagram,
    expected: &[u16],
    report: &mut dyn FnMut(Progress),
    cancel: &dyn Fn() -> bool,
) -> Result<()> {
    let mut pending: BTreeSet<u16> = expected.iter().copied().collect();
    if pending.is_empty() {
        bail!("no target nodes given");
    }

    for attempt in 0..=cfg.retries {
        if cancel() {
            bail!("cancelled");
        }
        dg.target_node_mask = mask_of(&pending);
        link.send_datagram(dg)?;

        loop {
            let Some(ack) = link.recv_ack(cfg.ack_timeout)? else {
                break;
            };
            if ack.datagram_id != dg.datagram_id {
                continue;
            }
            match ack.code {
                NackCode::Ok => {
                    if pending.remove(&ack.src_node) {
                        report(Progress::NodeAck { node: ack.src_node });
                    }
                    if pending.is_empty() {
                        return Ok(());
                    }
                }
                c if c.is_transient() => {
                    report(Progress::NodeNack {
                        node: ack.src_node,
                        code: c,
                        transient: true,
                    });
                    eprintln!(
                        "[fota] node {} transient NACK {:?} on datagram {}, will resend",
                        ack.src_node, c, dg.datagram_id
                    );
                    break;
                }
                c => {
                    report(Progress::NodeNack {
                        node: ack.src_node,
                        code: c,
                        transient: false,
                    });
                    bail!(
                        "node {} rejected datagram {} with {:?}",
                        ack.src_node,
                        dg.datagram_id,
                        c
                    );
                }
            }
        }

        if attempt < cfg.retries {
            let laggards: Vec<u16> = pending.iter().copied().collect();
            report(Progress::Retry {
                attempt: attempt + 1,
                of: cfg.retries,
                pending: laggards.clone(),
            });
            eprintln!(
                "[fota] retry {}/{}: nodes {:?} have not acked datagram {}",
                attempt + 1,
                cfg.retries,
                laggards,
                dg.datagram_id
            );
        }
    }
    bail!(
        "nodes {:?} never acknowledged datagram {}",
        pending,
        dg.datagram_id
    )
}

// == Commands ================================================================

/// Flash an image to one or more nodes, then optionally jump them to it
pub fn flash(
    link: &dyn Transport,
    cfg: &FotaConfig,
    nodes: &[u16],
    image: &[u8],
    meta: FirmwareMetadata,
    jump_after: bool,
    report: &mut dyn FnMut(Progress),
    cancel: &dyn Fn() -> bool,
) -> Result<()> {
    if nodes.is_empty() {
        bail!("specify at least one node");
    }
    report(Progress::Stage(format!(
        "flashing {} bytes (crc {:#010X}) to nodes {:?}",
        image.len(),
        meta.image_crc32,
        nodes
    )));

    let mut metadata = Datagram::new(
        0,
        DatagramType::FirmwareMetadata,
        SESSION_BASE_ID,
        meta.pack(),
    );
    deliver(link, cfg, &mut metadata, nodes, &mut *report, cancel)?;
    report(Progress::Stage(
        "metadata accepted, erasing and writing".into(),
    ));

    let nchunks = image.len().div_ceil(MAX_DATAGRAM_SIZE);
    for (i, chunk) in image.chunks(MAX_DATAGRAM_SIZE).enumerate() {
        if cancel() {
            bail!("cancelled");
        }
        let id = SESSION_BASE_ID + 1 + i as u32;
        let mut dg = Datagram::new(0, DatagramType::FirmwareChunk, id, chunk.to_vec());
        deliver(link, cfg, &mut dg, nodes, &mut *report, cancel)?;
        report(Progress::Chunk {
            done: i + 1,
            total: nchunks,
            bytes: chunk.len(),
        });
    }
    report(Progress::Stage(
        "image written and CRC verified on all nodes".into(),
    ));

    if jump_after {
        let id = SESSION_BASE_ID + 1 + nchunks as u32;
        jump_with_id(link, cfg, nodes, id)?;
    }
    Ok(())
}

/// Request nodes to jump to their application. A node with a valid app jumps and goes silent,
/// a node that refuses NACKs the reason, so we listen for the timeout window and report refusals.
pub fn jump(link: &dyn Transport, cfg: &FotaConfig, nodes: &[u16]) -> Result<()> {
    jump_with_id(link, cfg, nodes, SESSION_BASE_ID)
}

fn jump_with_id(link: &dyn Transport, cfg: &FotaConfig, nodes: &[u16], id: u32) -> Result<()> {
    let mask = mask_of(&nodes.iter().copied().collect());
    let dg = Datagram::new(mask, DatagramType::JumpToApp, id, Vec::new());
    link.send_datagram(&dg)?;

    // Drain acks for the timeout window. Silence (or an OK ack) means a node jumped away, any
    // NACK means it stayed in the bootloader and is telling us why the jump was refused.
    let mut refused: Vec<(u16, NackCode)> = Vec::new();
    while let Some(ack) = link.recv_ack(cfg.ack_timeout)? {
        if ack.datagram_id == id && ack.code != NackCode::Ok {
            refused.push((ack.src_node, ack.code));
        }
    }

    if !refused.is_empty() {
        let list = refused
            .iter()
            .map(|(node, code)| format!("node {node}: {}", code.describe()))
            .collect::<Vec<_>>()
            .join("; ");
        bail!("jump refused: {list}");
    }
    Ok(())
}

/// Drop running applications back into the bootloader via the ENTER control frame
pub fn enter(link: &dyn Transport, mask: u32) -> Result<()> {
    link.send_enter(mask)?;
    Ok(())
}

/// Whether a discovered node answered from the bootloader or from its running application
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeMode {
    Bootloader,
    App,
}

/// One node found by discovery. Both a bootloader node and a running app broadcast the same
/// heartbeat, the `mode` field is what distinguishes them (only a bootloader node is flashable).
/// Board and project names are not on the wire, the host resolves them from the node id.
#[derive(Clone, Copy, Debug)]
pub struct Discovered {
    pub node: u16,
    pub mode: NodeMode,
    pub app_present: bool,
    pub version: (u8, u8),
    pub app_size: u32,
}

impl Discovered {
    fn from_heartbeat(node: u16, hb: Heartbeat) -> Self {
        let mode = if hb.mode == IDENTITY_MODE_APPLICATION {
            NodeMode::App
        } else {
            NodeMode::Bootloader
        };
        Self {
            node,
            mode,
            app_present: hb.app_present,
            version: (hb.version_major, hb.version_minor),
            app_size: hb.app_size,
        }
    }
}

/// Turn a window of collected heartbeats into discovered nodes. Discovery is passive: every node,
/// bootloader or running app, periodically broadcasts a single heartbeat frame on its own id, so the
/// caller only listens (see `Transport::collect_discovery`) and feeds the result here.
pub fn parse_discovered(responses: DiscoveryAnswers) -> Vec<Discovered> {
    responses
        .into_iter()
        .map(|(src, hb)| Discovered::from_heartbeat(src, hb))
        .collect()
}

// == Tests ===================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_of_sets_one_bit_per_node() {
        let s: BTreeSet<u16> = [0u16, 3, 5].into_iter().collect();
        assert_eq!(mask_of(&s), 0b101001);
    }
}
