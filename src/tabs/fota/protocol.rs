//! Bootloader wire protocol: datagram codec, CRC and payload structs
//!
//! Ported from ms-bootloader `client/src/protocol.rs`. Mirrors the firmware
//! `bl_datagram.h` / `bl_dfu.h` byte for byte so the host and target can never
//! disagree on the wire format.
//!
//! - **Author:** Midnight Sun Team #24

use anyhow::{anyhow, ensure, Result};

// == Constants ===============================================================

/// Largest payload a datagram can carry, matches the firmware chunk size
pub const MAX_DATAGRAM_SIZE: usize = 2048;
/// Serialized header size in bytes
pub const HEADER_SIZE: usize = 21;
/// App entry magic the shim writes to the boot flag to request DFU
pub const BOOT_ENTER_MAGIC: u32 = 0xB007_10AD;

/// Packed firmware metadata payload size
pub const METADATA_SIZE: usize = 58;
/// Discovery heartbeat size, one classic CAN frame so it never fragments
#[allow(dead_code)] // heartbeat decode goes live once the fota gateway relays discovery
pub const HEARTBEAT_SIZE: usize = 8;

/// Heartbeat `flags` bit0: the application announced, the node is running its app
#[allow(dead_code)]
pub const HEARTBEAT_FLAG_APP: u8 = 0x01;
/// Heartbeat `flags` bit1: a valid app is flashed
#[allow(dead_code)]
pub const HEARTBEAT_FLAG_APP_PRESENT: u8 = 0x02;

/// Heartbeat mode: the bootloader answered, the node is ready to be flashed
#[allow(dead_code)]
pub const IDENTITY_MODE_BOOTLOADER: u8 = 0;
/// Heartbeat mode: the application answered, the node is running its app
pub const IDENTITY_MODE_APPLICATION: u8 = 1;

/// Fixed char field lengths, must match BOOTCONFIG_*_LEN in the firmware
pub const PROJECT_NAME_LEN: usize = 32;
pub const GIT_HASH_LEN: usize = 16;

// == Datagram type and status enums ==========================================

/// Datagram kind, travels in the header type byte. Mirrors `BlDatagramType` in the firmware. Only
/// METADATA, CHUNK and JUMP travel host to node as datagrams, discovery is the raw heartbeat frame
/// (see `Heartbeat`), there is no query or ping request/response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum DatagramType {
    FirmwareMetadata = 0,
    FirmwareChunk = 1,
    JumpToApp = 2,
    Ack = 3,
    Nack = 4,
}

impl DatagramType {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::FirmwareMetadata),
            1 => Some(Self::FirmwareChunk),
            2 => Some(Self::JumpToApp),
            3 => Some(Self::Ack),
            4 => Some(Self::Nack),
            _ => None,
        }
    }
}

/// One byte status reported by a node in an ACK or NACK frame
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum NackCode {
    Ok = 0x00,
    Crc = 0x01,
    Sequence = 0x02,
    Incomplete = 0x03,
    Flash = 0x04,
    Oversized = 0x05,
    BadState = 0x06,
    NoApp = 0x07,
    Internal = 0x08,
    BadVector = 0x09,
}

impl NackCode {
    pub fn from_u8(v: u8) -> NackCode {
        match v {
            0x00 => NackCode::Ok,
            0x01 => NackCode::Crc,
            0x02 => NackCode::Sequence,
            0x03 => NackCode::Incomplete,
            0x04 => NackCode::Flash,
            0x05 => NackCode::Oversized,
            0x06 => NackCode::BadState,
            0x07 => NackCode::NoApp,
            0x09 => NackCode::BadVector,
            _ => NackCode::Internal,
        }
    }

    /// True when resending the same datagram can recover. A node only verifies a
    /// datagram CRC in the transport, so a corrupt datagram times out rather than
    /// NACKing, which makes a received Crc a fatal whole image failure, not transient.
    pub fn is_transient(self) -> bool {
        matches!(self, NackCode::Sequence | NackCode::Incomplete)
    }

    /// A human readable reason for surfacing a NACK in the log
    pub fn describe(self) -> &'static str {
        match self {
            NackCode::Ok => "accepted",
            NackCode::Crc => "datagram CRC mismatch",
            NackCode::Sequence => "out of order datagram",
            NackCode::Incomplete => "missing fragments",
            NackCode::Flash => "flash erase/write/verify failed",
            NackCode::Oversized => "image larger than the app region",
            NackCode::BadState => "datagram illegal for the node state",
            NackCode::NoApp => "no valid app (not flashed or app CRC mismatch)",
            NackCode::Internal => "internal node failure",
            NackCode::BadVector => "app vector table invalid (bad stack pointer or reset handler)",
        }
    }
}

// == CRC =====================================================================

/// CRC32 over a byte slice, the zlib reflected algorithm the firmware uses everywhere
pub fn crc32(data: &[u8]) -> u32 {
    crc32fast::hash(data)
}

// == Datagram ================================================================

/// A complete datagram, the transport neutral unit above CAN or UART
#[derive(Clone, Debug)]
pub struct Datagram {
    pub target_node_mask: u64,
    pub dtype: DatagramType,
    pub datagram_id: u32,
    pub payload: Vec<u8>,
}

impl Datagram {
    pub fn new(
        target_node_mask: u64,
        dtype: DatagramType,
        datagram_id: u32,
        payload: Vec<u8>,
    ) -> Self {
        Self {
            target_node_mask,
            dtype,
            datagram_id,
            payload,
        }
    }

    /// Serialize into the flat little endian blob a transport fragments onto the wire
    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(HEADER_SIZE + self.payload.len());
        buf.extend_from_slice(&self.target_node_mask.to_le_bytes());
        buf.push(self.dtype as u8);
        buf.extend_from_slice(&self.datagram_id.to_le_bytes());
        buf.extend_from_slice(&(self.payload.len() as u32).to_le_bytes());
        buf.extend_from_slice(&crc32(&self.payload).to_le_bytes());
        buf.extend_from_slice(&self.payload);
        buf
    }

    /// Parse a flat blob (header followed by payload) and verify the payload CRC
    pub fn deserialize(buf: &[u8]) -> Result<Self> {
        ensure!(
            buf.len() >= HEADER_SIZE,
            "datagram blob shorter than header ({} < {})",
            buf.len(),
            HEADER_SIZE
        );
        let target_node_mask = u64::from_le_bytes(buf[0..8].try_into().unwrap());
        let type_byte = buf[8];
        let datagram_id = u32::from_le_bytes(buf[9..13].try_into().unwrap());
        let total_length = u32::from_le_bytes(buf[13..17].try_into().unwrap()) as usize;
        let crc = u32::from_le_bytes(buf[17..21].try_into().unwrap());

        let dtype = DatagramType::from_u8(type_byte)
            .ok_or_else(|| anyhow!("unknown datagram type {type_byte}"))?;
        ensure!(
            total_length <= MAX_DATAGRAM_SIZE,
            "payload too large ({total_length})"
        );
        ensure!(
            buf.len() >= HEADER_SIZE + total_length,
            "datagram blob truncated ({} < {})",
            buf.len(),
            HEADER_SIZE + total_length
        );

        let payload = buf[HEADER_SIZE..HEADER_SIZE + total_length].to_vec();
        ensure!(crc32(&payload) == crc, "datagram CRC mismatch");
        Ok(Self {
            target_node_mask,
            dtype,
            datagram_id,
            payload,
        })
    }
}

// == Payloads ================================================================

/// Firmware metadata, the payload of a METADATA datagram
#[derive(Clone, Debug)]
pub struct FirmwareMetadata {
    pub image_size: u32,
    pub image_crc32: u32,
    pub version_major: u8,
    pub version_minor: u8,
    pub project_name: String,
    pub git_hash: String,
}

impl FirmwareMetadata {
    pub fn pack(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(METADATA_SIZE);
        buf.extend_from_slice(&self.image_size.to_le_bytes());
        buf.extend_from_slice(&self.image_crc32.to_le_bytes());
        buf.push(self.version_major);
        buf.push(self.version_minor);
        buf.extend_from_slice(&fixed_bytes(&self.project_name, PROJECT_NAME_LEN));
        buf.extend_from_slice(&fixed_bytes(&self.git_hash, GIT_HASH_LEN));
        debug_assert_eq!(buf.len(), METADATA_SIZE);
        buf
    }
}

/// Discovery heartbeat, the 8 byte frame every node broadcasts on its own announce id. The node id
/// rides the arbitration id, not the payload, and the human friendly board and project names live in
/// a host side node id map, so only the small fixed fields a host needs to decide flashable travel
/// here. Mirrors the layout in `bl_identity.h`.
#[derive(Clone, Copy, Debug)]
pub struct Heartbeat {
    /// Which firmware answered, IDENTITY_MODE_BOOTLOADER or IDENTITY_MODE_APPLICATION
    pub mode: u8,
    pub app_present: bool,
    pub version_major: u8,
    pub version_minor: u8,
    pub app_size: u32,
}

impl Heartbeat {
    #[allow(dead_code)] // heartbeat decode goes live once the fota gateway relays discovery
    pub fn parse(buf: &[u8]) -> Result<Self> {
        ensure!(
            buf.len() >= HEARTBEAT_SIZE,
            "heartbeat frame too short ({} < {})",
            buf.len(),
            HEARTBEAT_SIZE
        );
        let flags = buf[0];
        let mode = if flags & HEARTBEAT_FLAG_APP != 0 {
            IDENTITY_MODE_APPLICATION
        } else {
            IDENTITY_MODE_BOOTLOADER
        };
        Ok(Self {
            mode,
            app_present: flags & HEARTBEAT_FLAG_APP_PRESENT != 0,
            version_major: buf[1],
            version_minor: buf[2],
            app_size: u32::from_le_bytes(buf[3..7].try_into().unwrap()),
        })
    }
}

// == Fixed char field helpers ================================================

/// Copy a string into an n byte field, truncating or zero padding to fit
fn fixed_bytes(s: &str, n: usize) -> Vec<u8> {
    let mut v = vec![0u8; n];
    let b = s.as_bytes();
    let take = b.len().min(n);
    v[..take].copy_from_slice(&b[..take]);
    v
}

// == Tests ===================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_matches_zlib_known_answers() {
        assert_eq!(crc32(b""), 0x0000_0000);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(
            crc32(b"The quick brown fox jumps over the lazy dog"),
            0x414F_A339
        );
    }

    #[test]
    fn datagram_header_is_21_le_bytes() {
        let dg = Datagram::new(0x1, DatagramType::FirmwareChunk, 7, vec![0xAA, 0xBB]);
        let blob = dg.serialize();
        assert_eq!(blob.len(), HEADER_SIZE + 2);
        assert_eq!(&blob[0..8], &1u64.to_le_bytes());
        assert_eq!(blob[8], DatagramType::FirmwareChunk as u8);
        assert_eq!(&blob[9..13], &7u32.to_le_bytes());
        assert_eq!(&blob[13..17], &2u32.to_le_bytes());
        assert_eq!(&blob[17..21], &crc32(&[0xAA, 0xBB]).to_le_bytes());
        assert_eq!(&blob[21..], &[0xAA, 0xBB]);
    }

    #[test]
    fn datagram_roundtrips() {
        let dg = Datagram::new(
            0xDEAD_BEEF_0000_0001,
            DatagramType::FirmwareChunk,
            42,
            (0..200u32).map(|i| i as u8).collect(),
        );
        let back = Datagram::deserialize(&dg.serialize()).unwrap();
        assert_eq!(back.target_node_mask, dg.target_node_mask);
        assert_eq!(back.dtype, DatagramType::FirmwareChunk);
        assert_eq!(back.datagram_id, 42);
        assert_eq!(back.payload, dg.payload);
    }

    #[test]
    fn datagram_detects_payload_corruption() {
        let dg = Datagram::new(0, DatagramType::FirmwareChunk, 1, vec![1, 2, 3]);
        let mut blob = dg.serialize();
        blob[HEADER_SIZE] ^= 0xFF;
        assert!(Datagram::deserialize(&blob).is_err());
    }

    #[test]
    fn datagram_rejects_truncated_blob() {
        let dg = Datagram::new(0, DatagramType::FirmwareChunk, 1, vec![1, 2, 3, 4, 5]);
        let blob = dg.serialize();
        assert!(Datagram::deserialize(&blob[..HEADER_SIZE + 2]).is_err());
    }

    #[test]
    fn metadata_packs_to_58_bytes() {
        let m = FirmwareMetadata {
            image_size: 1234,
            image_crc32: 0xAABB_CCDD,
            version_major: 1,
            version_minor: 2,
            project_name: "front_controller".into(),
            git_hash: "abcdef0123456789".into(),
        };
        let p = m.pack();
        assert_eq!(p.len(), METADATA_SIZE);
        assert_eq!(&p[0..4], &1234u32.to_le_bytes());
        assert_eq!(&p[4..8], &0xAABB_CCDDu32.to_le_bytes());
        assert_eq!(p[8], 1);
        assert_eq!(p[9], 2);
        assert_eq!(&p[10..26], b"front_controller");
        assert_eq!(p[26], 0); // zero padded
    }

    #[test]
    fn heartbeat_parses_known_layout() {
        let mut buf = [0u8; HEARTBEAT_SIZE];
        buf[0] = HEARTBEAT_FLAG_APP | HEARTBEAT_FLAG_APP_PRESENT;
        buf[1] = 3;
        buf[2] = 4;
        buf[3..7].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        let hb = Heartbeat::parse(&buf).unwrap();
        assert_eq!(hb.mode, IDENTITY_MODE_APPLICATION);
        assert!(hb.app_present);
        assert_eq!(hb.version_major, 3);
        assert_eq!(hb.version_minor, 4);
        assert_eq!(hb.app_size, 0x0001_0000);
    }

    #[test]
    fn heartbeat_reads_the_bootloader_mode() {
        let buf = [0u8; HEARTBEAT_SIZE];
        let hb = Heartbeat::parse(&buf).unwrap();
        assert_eq!(hb.mode, IDENTITY_MODE_BOOTLOADER);
        assert!(!hb.app_present);
    }

    #[test]
    fn heartbeat_rejects_short_frame() {
        assert!(Heartbeat::parse(&[0u8; 4]).is_err());
    }
}
