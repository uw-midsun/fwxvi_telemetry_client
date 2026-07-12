//! Serial transport for the XBee gateway link, the host side of `bl_transport_uart.c`
//!
//! Speaks the firmware UART framing over a transparent serial port (the XBee COM port):
//! datagrams fragment onto `0x7E` frames, acks arrive as `0x7D` frames, and the `0x7C` ENTER
//! frame drops the running telemetry app into fota. fota then relays everything onto CAN, so
//! from here a flash looks identical to the CAN path, just framed for a byte stream.
//!
//! Ported from ms-bootloader `client/src/serial.rs`.
//!
//! - **Author:** Midnight Sun Team #24

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::io::ErrorKind;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use serialport::SerialPort;

use super::config::FotaConfig;
use super::protocol::{crc32, Datagram, NackCode, BOOT_ENTER_MAGIC, HEADER_SIZE};
use super::transport::{Ack, DiscoveryAnswers, Transport};

// == Framing constants, mirror bl_transport_uart.h and bl_entry_shim.h =========

/// Start byte of a datagram fragment frame
const SOF_DATAGRAM: u8 = 0x7E;
/// Start byte of an ack / nack frame
const SOF_ACK: u8 = 0x7D;
/// Start byte of the app facing ENTER frame
const SOF_ENTER: u8 = 0x7C;
/// Payload bytes carried per fragment frame
const FRAGMENT_STRIDE: usize = 128;
/// Ack body length before its crc, status (1), datagram id (4), source (1)
const ACK_BODY_LEN: usize = 6;
/// ENTER body length before its crc, magic (4), target_mask_lo (4)
const ENTER_BODY_LEN: usize = 8;
/// How long a single read waits before looping back to check the deadline
const READ_SLICE: Duration = Duration::from_millis(50);

// == Frame builders ===========================================================

/// Split a serialized datagram into framed fragments, [0x7E][index:2 LE][len:1][payload][crc32:4 LE]
fn fragment_frames(blob: &[u8]) -> Vec<Vec<u8>> {
    let nfrags = blob.len().div_ceil(FRAGMENT_STRIDE);
    let mut frames = Vec::with_capacity(nfrags);
    for i in 0..nfrags {
        let offset = i * FRAGMENT_STRIDE;
        let end = (offset + FRAGMENT_STRIDE).min(blob.len());
        let n = end - offset;
        let mut frame = Vec::with_capacity(1 + 3 + n + 4);
        frame.push(SOF_DATAGRAM);
        frame.push((i & 0xFF) as u8);
        frame.push(((i >> 8) & 0xFF) as u8);
        frame.push(n as u8);
        frame.extend_from_slice(&blob[offset..end]);
        // crc covers the 3 header bytes (index, len) and the payload
        let crc = crc32(&frame[1..4 + n]);
        frame.extend_from_slice(&crc.to_le_bytes());
        frames.push(frame);
    }
    frames
}

/// Build the ENTER frame, [0x7C][magic:4 LE][mask:4 LE][crc32:4 LE]
fn enter_frame(mask: u32) -> Vec<u8> {
    let mut frame = Vec::with_capacity(1 + ENTER_BODY_LEN + 4);
    frame.push(SOF_ENTER);
    frame.extend_from_slice(&BOOT_ENTER_MAGIC.to_le_bytes());
    frame.extend_from_slice(&mask.to_le_bytes());
    let crc = crc32(&frame[1..1 + ENTER_BODY_LEN]);
    frame.extend_from_slice(&crc.to_le_bytes());
    frame
}

fn le32(p: &[u8]) -> u32 {
    u32::from_le_bytes([p[0], p[1], p[2], p[3]])
}

// == Receive state machine ====================================================

/// Receive parser phase, the start byte selects fragment or ack
enum RxPhase {
    WaitSof,
    FragHdr,
    FragPayload,
    FragCrc,
    AckBody,
}

/// Deframes the inbound byte stream into completed acks and datagrams
struct RxState {
    phase: RxPhase,
    fill: usize,
    frag_hdr: [u8; 3],
    frag_index: u16,
    frag_len: usize,
    frag_payload: [u8; FRAGMENT_STRIDE],
    frag_crc: [u8; 4],
    ack_buf: [u8; ACK_BODY_LEN + 4],
    reasm: HashMap<u16, Vec<u8>>,
    acks: VecDeque<Ack>,
    datagrams: VecDeque<Datagram>,
}

impl RxState {
    fn new() -> Self {
        Self {
            phase: RxPhase::WaitSof,
            fill: 0,
            frag_hdr: [0; 3],
            frag_index: 0,
            frag_len: 0,
            frag_payload: [0; FRAGMENT_STRIDE],
            frag_crc: [0; 4],
            ack_buf: [0; ACK_BODY_LEN + 4],
            reasm: HashMap::new(),
            acks: VecDeque::new(),
            datagrams: VecDeque::new(),
        }
    }

    fn reset_frame(&mut self) {
        self.phase = RxPhase::WaitSof;
        self.fill = 0;
    }

    /// A fragment passed its crc, place it and emit the datagram once it reassembles
    fn place_fragment(&mut self) {
        self.reasm
            .insert(self.frag_index, self.frag_payload[..self.frag_len].to_vec());
        if let Some(dg) = try_assemble(&self.reasm) {
            self.datagrams.push_back(dg);
            self.reasm.clear();
        }
    }

    /// Feed one received byte through the framing state machine
    fn feed_byte(&mut self, byte: u8) {
        match self.phase {
            RxPhase::WaitSof => {
                if byte == SOF_DATAGRAM {
                    self.phase = RxPhase::FragHdr;
                    self.fill = 0;
                } else if byte == SOF_ACK {
                    self.phase = RxPhase::AckBody;
                    self.fill = 0;
                }
            }
            RxPhase::FragHdr => {
                self.frag_hdr[self.fill] = byte;
                self.fill += 1;
                if self.fill == 3 {
                    self.frag_index = u16::from_le_bytes([self.frag_hdr[0], self.frag_hdr[1]]);
                    self.frag_len = self.frag_hdr[2] as usize;
                    if self.frag_len == 0 || self.frag_len > FRAGMENT_STRIDE {
                        self.reset_frame();
                    } else {
                        self.phase = RxPhase::FragPayload;
                        self.fill = 0;
                    }
                }
            }
            RxPhase::FragPayload => {
                self.frag_payload[self.fill] = byte;
                self.fill += 1;
                if self.fill == self.frag_len {
                    self.phase = RxPhase::FragCrc;
                    self.fill = 0;
                }
            }
            RxPhase::FragCrc => {
                self.frag_crc[self.fill] = byte;
                self.fill += 1;
                if self.fill == 4 {
                    let mut hashed = self.frag_hdr.to_vec();
                    hashed.extend_from_slice(&self.frag_payload[..self.frag_len]);
                    if le32(&self.frag_crc) == crc32(&hashed) {
                        self.place_fragment();
                    }
                    self.reset_frame();
                }
            }
            RxPhase::AckBody => {
                self.ack_buf[self.fill] = byte;
                self.fill += 1;
                if self.fill == self.ack_buf.len() {
                    let want = crc32(&self.ack_buf[..ACK_BODY_LEN]);
                    if le32(&self.ack_buf[ACK_BODY_LEN..]) == want {
                        self.acks.push_back(Ack {
                            code: NackCode::from_u8(self.ack_buf[0]),
                            datagram_id: le32(&self.ack_buf[1..5]),
                            src_node: self.ack_buf[5] as u16,
                        });
                    }
                    self.reset_frame();
                }
            }
        }
    }
}

/// Concatenate contiguous fragments from index 0 and return a datagram once the
/// header's total_length is satisfied and the blob verifies
fn try_assemble(frags: &HashMap<u16, Vec<u8>>) -> Option<Datagram> {
    let mut blob = Vec::new();
    let mut i = 0u16;
    while let Some(part) = frags.get(&i) {
        blob.extend_from_slice(part);
        if blob.len() >= HEADER_SIZE {
            let total = le32(&blob[13..17]) as usize;
            if blob.len() >= HEADER_SIZE + total {
                return Datagram::deserialize(&blob[..HEADER_SIZE + total]).ok();
            }
        }
        i += 1;
    }
    None
}

// == Transport ================================================================

pub struct SerialTransport {
    port: RefCell<Box<dyn SerialPort>>,
    rx: RefCell<RxState>,
}

impl SerialTransport {
    pub fn open(cfg: &FotaConfig) -> Result<Self> {
        let port = serialport::new(&cfg.serial_port, cfg.serial_baud)
            .timeout(READ_SLICE)
            .open()
            .with_context(|| format!("opening serial port '{}'", cfg.serial_port))?;
        Ok(Self {
            port: RefCell::new(port),
            rx: RefCell::new(RxState::new()),
        })
    }

    /// Read whatever bytes are available and feed them through the deframer
    fn pump(&self) -> Result<()> {
        let mut buf = [0u8; 256];
        let n = {
            let mut port = self.port.borrow_mut();
            match port.as_mut().read(&mut buf) {
                Ok(n) => n,
                Err(e) if matches!(e.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) => 0,
                Err(e) => return Err(anyhow!("serial read error: {e}")),
            }
        };
        if n > 0 {
            let mut rx = self.rx.borrow_mut();
            for &b in &buf[..n] {
                rx.feed_byte(b);
            }
        }
        Ok(())
    }

    fn write_frame(&self, frame: &[u8]) -> Result<()> {
        let mut port = self.port.borrow_mut();
        port.as_mut()
            .write_all(frame)
            .context("writing serial frame")?;
        port.as_mut().flush().ok();
        Ok(())
    }
}

impl Transport for SerialTransport {
    fn send_datagram(&self, dg: &Datagram) -> Result<()> {
        for frame in fragment_frames(&dg.serialize()) {
            self.write_frame(&frame)?;
        }
        Ok(())
    }

    fn send_enter(&self, mask: u32) -> Result<()> {
        self.write_frame(&enter_frame(mask))
    }

    fn recv_ack(&self, timeout: Duration) -> Result<Option<Ack>> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(ack) = self.rx.borrow_mut().acks.pop_front() {
                return Ok(Some(ack));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            self.pump()?;
        }
    }

    fn collect_discovery(&self, window: Duration) -> Result<DiscoveryAnswers> {
        // Discovery over the serial link needs the gateway to relay downstream CAN heartbeats
        // upstream, a pending fota change (doc 11). Until then, drain and discard any inbound
        // datagrams so the queue never backs up and report nothing.
        let deadline = Instant::now() + window;
        while Instant::now() < deadline {
            self.pump()?;
            while self.rx.borrow_mut().datagrams.pop_front().is_some() {}
        }
        Ok(Vec::new())
    }
}

// == Tests ====================================================================

#[cfg(test)]
mod tests {
    use super::super::protocol::DatagramType;
    use super::*;

    #[test]
    fn enter_frame_matches_firmware_layout() {
        let f = enter_frame(0x0000_0005);
        assert_eq!(f.len(), 13);
        assert_eq!(f[0], SOF_ENTER);
        assert_eq!(&f[1..5], &BOOT_ENTER_MAGIC.to_le_bytes());
        assert_eq!(&f[5..9], &0x0000_0005u32.to_le_bytes());
        assert_eq!(&f[9..13], &crc32(&f[1..9]).to_le_bytes());
    }

    #[test]
    fn datagram_fragments_reassemble() {
        let dg = Datagram::new(
            0x1,
            DatagramType::FirmwareChunk,
            7,
            (0..300u32).map(|i| i as u8).collect(),
        );
        let mut rx = RxState::new();
        for frame in fragment_frames(&dg.serialize()) {
            for b in frame {
                rx.feed_byte(b);
            }
        }
        let out = rx.datagrams.pop_front().expect("a reassembled datagram");
        assert_eq!(out.datagram_id, 7);
        assert_eq!(out.payload, dg.payload);
    }

    #[test]
    fn ack_frame_decodes() {
        let mut body = vec![NackCode::Ok as u8];
        body.extend_from_slice(&42u32.to_le_bytes());
        body.push(9u8);
        let mut frame = vec![SOF_ACK];
        frame.extend_from_slice(&body);
        frame.extend_from_slice(&crc32(&body).to_le_bytes());

        let mut rx = RxState::new();
        for b in frame {
            rx.feed_byte(b);
        }
        let ack = rx.acks.pop_front().expect("an ack");
        assert_eq!(ack.code, NackCode::Ok);
        assert_eq!(ack.datagram_id, 42);
        assert_eq!(ack.src_node, 9);
    }

    #[test]
    fn corrupt_fragment_crc_is_dropped() {
        let dg = Datagram::new(0, DatagramType::FirmwareChunk, 1, vec![1, 2, 3]);
        let mut frames = fragment_frames(&dg.serialize());
        frames[0][5] ^= 0xFF;
        let mut rx = RxState::new();
        for frame in frames {
            for b in frame {
                rx.feed_byte(b);
            }
        }
        assert!(rx.datagrams.pop_front().is_none());
    }
}
