pub mod can_config;

use crate::db::signal_store::{increment_stat, insert_signal, SignalSample};
use std::io::Read;
use can_config::{extract_signal, CanMessage};
use crossbeam_channel::Receiver;
use rusqlite::Connection;
use std::path::PathBuf;

// ── Datagram framing ──────────────────────────────────────────────────────────
const SOF: u8 = 0xAA;
const EOF: u8 = 0xBB;

#[derive(Debug, PartialEq)]
enum State {
    Sof,
    Id,
    Dlc,
    Data,
    Eof,
}

struct Datagram {
    id:   u16,
    dlc:  u8,
    data: Vec<u8>,
}

struct Parser {
    state:  State,
    buf:    Vec<u8>,
    dgram:  Option<Datagram>,
}

impl Parser {
    fn new() -> Self {
        Self { state: State::Sof, buf: vec![], dgram: None }
    }

    /// Feed one byte; returns a complete datagram when one is ready.
    fn push(&mut self, byte: u8) -> Option<Datagram> {
        match self.state {
            State::Sof => {
                if byte == SOF {
                    self.buf.clear();
                    self.dgram = None;
                    self.state = State::Id;
                }
            }
            State::Id => {
                self.buf.push(byte);
                if self.buf.len() == 2 {
                    let id = u16::from_be_bytes([self.buf[0], self.buf[1]]);
                    self.dgram = Some(Datagram { id, dlc: 0, data: vec![] });
                    self.buf.clear();
                    self.state = State::Dlc;
                }
            }
            State::Dlc => {
                if byte > 8 {
                    self.reset();
                    return None;
                }
                if let Some(ref mut d) = self.dgram {
                    d.dlc = byte;
                }
                self.buf.clear();
                self.state = State::Data;
            }
            State::Data => {
                if byte == SOF || byte == EOF {
                    self.reset();
                    return None;
                }
                self.buf.push(byte);
                if let Some(ref d) = self.dgram {
                    if self.buf.len() == d.dlc as usize {
                        if let Some(ref mut dg) = self.dgram {
                            dg.data = self.buf.clone();
                        }
                        self.buf.clear();
                        self.state = State::Eof;
                    }
                }
            }
            State::Eof => {
                if byte == EOF || byte == 0x00 {
                    self.state = State::Sof;
                    return self.dgram.take();
                }
                self.reset();
            }
        }
        None
    }

    fn reset(&mut self) {
        self.state = State::Sof;
        self.buf.clear();
        self.dgram = None;
    }
}

// ── Thermistor demux ──────────────────────────────────────────────────────────
// AFE_temperature is a multiplexed message: the same CAN id is sent three times
// with its `id` field set to 0, 1, 2. Each frame carries up to seven readings
// (temperature_0..6); a reading's global thermistor index is bank*7 + n, and only
// indices 0..=17 exist (bank 2 stops at thermistor 17). Demuxing at decode time
// spreads the 18 readings across distinct signal names (temperature_0..17) rather
// than letting each successive frame overwrite the last under the same seven keys.
const TEMP_MSG:       &str = "AFE_temperature";
const TEMPS_PER_BANK: i64  = 7;
const MAX_THERMISTOR: i64  = 17;

/// Map a signal on the multiplexed AFE_temperature frame to the global signal
/// name it should be stored under. `bank` is the frame's `id` field (0, 1, 2);
/// temperature_n becomes temperature_{bank*7 + n}. Returns `None` for the bank
/// selector itself (and anything else that isn't a temperature) and for indices
/// past thermistor 17 (bank 2 only carries thermistors 14..17).
fn thermistor_signal(bank: i64, sig_name: &str) -> Option<String> {
    let local  = sig_name.strip_prefix("temperature_")?.parse::<i64>().ok()?;
    let global = bank * TEMPS_PER_BANK + local;
    (0..=MAX_THERMISTOR)
        .contains(&global)
        .then(|| format!("temperature_{global}"))
}

// ── BPS fault-detail union ─────────────────────────────────────────────────────
// `bps_fault_info.extra_info` is a 64-bit union (firmware `BpsFaultData`, see
// global_enums.h). The union is untagged: which view is valid is decided by the
// active bit in `rear_controller_status.bps_fault` — a *different* message. We
// track the last-seen fault code and, when a bps_fault_info frame arrives, decode
// the payload into that view's named detail signals instead of storing the
// meaningless 64-bit raw as a single f64.
const BPS_STATUS_MSG: &str = "rear_controller_status";
const BPS_FAULT_SIG:  &str = "bps_fault";
const BPS_INFO_MSG:   &str = "bps_fault_info";

// Bit indices in the BpsFault code (mirror global_enums.h `BpsFault`).
const BPS_OVERVOLTAGE:   u32 = 0;
const BPS_UNBALANCE:     u32 = 1;
const BPS_OVERTEMP_AMB:  u32 = 2;
const BPS_OVERTEMP_CELL: u32 = 5;
const BPS_OVERCURRENT:   u32 = 6;
const BPS_UNDERVOLTAGE:  u32 = 7;

/// Cell-voltage fields are in 100 µV units; scale to volts for display.
const CELL_UV_TO_V: f64 = 0.0001;

/// Decode the `bps_fault_info.extra_info` union into named detail signals, picking
/// the view from `fault_code` (the latest `rear_controller_status.bps_fault`).
/// The firmware latches the detail of the first (root) fault, which we can't
/// recover from the bitmask alone, so we choose the lowest-indexed active
/// data-bearing bit. Fault bits with no payload (comms loss, killswitch, relay,
/// disconnected) — and the no-fault case — yield no rows.
fn decode_bps_fault_detail(payload: u64, fault_code: u32) -> Vec<(&'static str, f64)> {
    let bit    = |b: u32| fault_code & (1u32 << b) != 0;
    let u8_at  = |shift: u32| ((payload >> shift) & 0xFF) as f64;
    let u16_at = |shift: u32| ((payload >> shift) & 0xFFFF) as f64;
    let i16_at = |shift: u32| (((payload >> shift) & 0xFFFF) as u16 as i16) as f64;

    if bit(BPS_OVERVOLTAGE) || bit(BPS_UNDERVOLTAGE) {
        // BpsCellFaultData: cell_index @byte0, cell_voltage[100µV] @byte2..3
        vec![
            ("fault_cell_index",   u8_at(0)),
            ("fault_cell_voltage", u16_at(16) * CELL_UV_TO_V),
        ]
    } else if bit(BPS_UNBALANCE) {
        // BpsUnbalanceFaultData: max_idx @0, min_idx @1, max_v @2..3, min_v @4..5
        vec![
            ("fault_max_cell_index",   u8_at(0)),
            ("fault_min_cell_index",   u8_at(8)),
            ("fault_max_cell_voltage", u16_at(16) * CELL_UV_TO_V),
            ("fault_min_cell_voltage", u16_at(32) * CELL_UV_TO_V),
        ]
    } else if bit(BPS_OVERTEMP_AMB) || bit(BPS_OVERTEMP_CELL) {
        // BpsTempFaultData: cell_index @byte0, temperature_c(i16) @byte2..3
        vec![
            ("fault_cell_index",    u8_at(0)),
            ("fault_temperature_c", i16_at(16)),
        ]
    } else if bit(BPS_OVERCURRENT) {
        // BpsCurrentFaultData: current_a(f32) @byte0..3
        vec![("fault_current_a", f32::from_bits((payload & 0xFFFF_FFFF) as u32) as f64)]
    } else {
        vec![]
    }
}

// ── Decoder thread ────────────────────────────────────────────────────────────

pub enum DecoderCmd {
    Stop,
}

/// Runs in a dedicated thread. Reads bytes from `port`, decodes datagrams,
/// writes signals to SQLite. Stops when `cmd_rx` receives `DecoderCmd::Stop`
/// or the port errors out.
pub fn run(
    mut port: Box<dyn serialport::SerialPort>,
    cmd_rx: Receiver<DecoderCmd>,
    db_path: PathBuf,
    yaml_path: PathBuf,
) {
    let conn = match crate::db::open(&db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Decoder: cannot open DB: {e}");
            return;
        }
    };

    let messages: Vec<CanMessage> = match can_config::load(&yaml_path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("Decoder: cannot load CAN config: {e}");
            return;
        }
    };

    let debug_bytes = std::env::args().any(|a| a == "--debug-bytes");

    let mut parser = Parser::new();
    let mut byte_buf = [0u8; 1];
    // Latest rear_controller_status.bps_fault code; selects the bps_fault_info union view.
    let mut last_bps_fault: u32 = 0;
    let mut batch_bytes: i64 = 0;
    let mut parsed: i64 = 0;
    let mut matched: i64 = 0;
    let mut unmatched: i64 = 0;
    let mut last_flush = std::time::Instant::now();

    loop {
        // Check for stop command (non-blocking)
        if cmd_rx.try_recv().is_ok() {
            break;
        }

        match port.read(&mut byte_buf) {
            Ok(1) => {
                batch_bytes += 1;
                if debug_bytes {
                    eprint!("{:02X} ", byte_buf[0]);
                }
                if let Some(dgram) = parser.push(byte_buf[0]) {
                    if debug_bytes {
                        eprintln!("\n[dgram] id={:#06X} dlc={} data={:02X?}", dgram.id, dgram.dlc, dgram.data);
                    }
                    parsed += 1;
                    if decode_datagram(&conn, &dgram, &messages, &mut last_bps_fault, debug_bytes) {
                        matched += 1;
                    } else {
                        unmatched += 1;
                    }
                }
            }
            Ok(_) => {}
            Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => {
                eprintln!("Decoder: serial read error: {e}");
                break;
            }
        }

        // Flush accumulated counters every second, off the per-datagram hot path.
        if last_flush.elapsed().as_secs() >= 1 {
            flush_stats(&conn, &mut batch_bytes, &mut parsed, &mut matched, &mut unmatched);
            last_flush = std::time::Instant::now();
        }
    }

    // Flush the tail so counts since the last flush aren't lost on stop/error exit.
    flush_stats(&conn, &mut batch_bytes, &mut parsed, &mut matched, &mut unmatched);
}

/// Writes accumulated decoder counters to `decoder_stats` and resets them to zero.
fn flush_stats(
    conn: &Connection,
    batch_bytes: &mut i64,
    parsed: &mut i64,
    matched: &mut i64,
    unmatched: &mut i64,
) {
    let _ = increment_stat(conn, "parse_byte_calls", *batch_bytes);
    let _ = increment_stat(conn, "parsed_messages", *parsed);
    let _ = increment_stat(conn, "matched_messages", *matched);
    let _ = increment_stat(conn, "unmatched_messages", *unmatched);
    *batch_bytes = 0;
    *parsed = 0;
    *matched = 0;
    *unmatched = 0;
}

/// Decodes one datagram, inserting all of its signals in a single transaction.
/// Returns `true` if the datagram matched a known CAN message. Stat counters are
/// accumulated by the caller and flushed periodically, off the hot path.
fn decode_datagram(conn: &Connection, dgram: &Datagram, messages: &[CanMessage], last_bps_fault: &mut u32, debug: bool) -> bool {
    let Some(msg) = messages.iter().find(|m| m.id == dgram.id as u32) else {
        eprintln!("[decoder] no match for CAN ID {:#06X}", dgram.id);
        return false;
    };

    if debug {
        eprintln!("[decoder] matched id={:#06X} name={}", dgram.id, msg.name);
    }

    // Build little-endian u64 from payload bytes
    let mut payload: u64 = 0;
    for (i, &b) in dgram.data.iter().enumerate().take(8) {
        payload |= (b as u64) << (i * 8);
    }

    let ts_ms = chrono::Utc::now().timestamp_millis();

    // Remember the active BPS fault code so a following bps_fault_info frame can
    // pick the right union view (the selector lives in this separate message).
    if msg.name == BPS_STATUS_MSG {
        if let Some(sig) = msg.signals.iter().find(|s| s.name == BPS_FAULT_SIG) {
            *last_bps_fault = extract_signal(payload, sig.start_bit, sig.length, false) as u32;
        }
    }

    // For the multiplexed thermistor message the `id` field selects which bank of
    // readings this frame carries; `None` for every other message (no demux).
    let temp_bank: Option<i64> = (msg.name == TEMP_MSG)
        .then(|| {
            msg.signals
                .iter()
                .find(|s| s.name == "id")
                .map(|s| extract_signal(payload, s.start_bit, s.length, false))
        })
        .flatten();

    // One transaction per datagram: collapses N loose autocommits into a single
    // WAL commit. `unchecked_transaction` borrows &Connection; &tx deref-coerces
    // to &Connection for insert_signal.
    let tx = match conn.unchecked_transaction() {
        Ok(tx) => tx,
        Err(e) => {
            eprintln!("[decoder] cannot begin transaction: {e}");
            return true;
        }
    };

    // bps_fault_info carries a single 64-bit union (extra_info). Store the decoded
    // detail fields of whichever fault is active, not the meaningless raw as f64.
    if msg.name == BPS_INFO_MSG {
        for (name, value) in decode_bps_fault_detail(payload, *last_bps_fault) {
            let sample = SignalSample {
                ts_ms,
                can_id:       dgram.id as u32,
                parent_name:  msg.name.clone(),
                message_name: msg.name.clone(),
                signal_name:  name.to_string(),
                value,
            };
            let _ = insert_signal(&tx, &sample);
        }
        let _ = tx.commit();
        return true;
    }

    for sig in &msg.signals {
        // Demux thermistors: remap temperature_n -> temperature_{bank*7+n}, dropping
        // the bank selector (`id`) and any index past thermistor 17. Non-temperature
        // messages keep their signal names verbatim.
        let signal_name = match temp_bank {
            Some(bank) => match thermistor_signal(bank, &sig.name) {
                Some(name) => name,
                None => continue,
            },
            None => sig.name.clone(),
        };

        // Firmware float scaling wins over scale/signed: the raw code is an unsigned
        // quantization of [min, max], inverted exactly like the autogen getters.
        let value = if let Some((min, max)) = sig.float_range() {
            let raw = extract_signal(payload, sig.start_bit, sig.length, false) as u64;
            can_config::decode_float(raw, sig.length, min, max)
        } else {
            let raw = extract_signal(payload, sig.start_bit, sig.length, sig.signed);
            raw as f64 * sig.scale.unwrap_or(1.0)
        };
        let sample = SignalSample {
            ts_ms,
            can_id:       dgram.id as u32,
            parent_name:  msg.name.clone(),
            message_name: msg.name.clone(),
            signal_name,
            value,
        };
        let _ = insert_signal(&tx, &sample);
    }
    let _ = tx.commit();
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thermistor_demux_maps_three_banks_onto_0_through_17() {
        // Bank 0 -> thermistors 0..6, bank 1 -> 7..13, bank 2 -> 14..17.
        for local in 0..7 {
            assert_eq!(
                thermistor_signal(0, &format!("temperature_{local}")),
                Some(format!("temperature_{local}"))
            );
            assert_eq!(
                thermistor_signal(1, &format!("temperature_{local}")),
                Some(format!("temperature_{}", 7 + local))
            );
        }
        // Bank 2 only carries thermistors 14..17 (locals 0..3); 4..6 spill past 17.
        assert_eq!(thermistor_signal(2, "temperature_0"), Some("temperature_14".into()));
        assert_eq!(thermistor_signal(2, "temperature_3"), Some("temperature_17".into()));
        for local in 4..7 {
            assert_eq!(thermistor_signal(2, &format!("temperature_{local}")), None);
        }
    }

    #[test]
    fn thermistor_demux_ignores_the_bank_selector_and_non_temps() {
        assert_eq!(thermistor_signal(0, "id"), None);
        assert_eq!(thermistor_signal(1, "voltage_3"), None);
    }

    // Build a little-endian u64 payload from bytes, matching the decoder's packing.
    fn payload_le(bytes: [u8; 8]) -> u64 {
        let mut p = 0u64;
        for (i, b) in bytes.iter().enumerate() {
            p |= (*b as u64) << (i * 8);
        }
        p
    }

    #[test]
    fn bps_detail_overvoltage_decodes_cell_index_and_voltage() {
        // cell_index @byte0 = 3, cell_voltage[100µV] @byte2..3 = 41000 -> 4.1 V
        let payload = payload_le([3, 0, (41000u16 & 0xFF) as u8, (41000u16 >> 8) as u8, 0, 0, 0, 0]);
        let rows = decode_bps_fault_detail(payload, 1 << BPS_OVERVOLTAGE);
        assert_eq!(rows[0], ("fault_cell_index", 3.0));
        assert_eq!(rows[1].0, "fault_cell_voltage");
        assert!((rows[1].1 - 4.1).abs() < 1e-9);
        // UNDERVOLTAGE uses the same view.
        assert_eq!(decode_bps_fault_detail(payload, 1 << BPS_UNDERVOLTAGE), rows);
    }

    #[test]
    fn bps_detail_unbalance_decodes_max_and_min() {
        // max_idx @0=5, min_idx @1=12, max_v @2..3=42000(4.2V), min_v @4..5=38000(3.8V)
        let payload = payload_le([
            5, 12,
            (42000u16 & 0xFF) as u8, (42000u16 >> 8) as u8,
            (38000u16 & 0xFF) as u8, (38000u16 >> 8) as u8,
            0, 0,
        ]);
        let rows = decode_bps_fault_detail(payload, 1 << BPS_UNBALANCE);
        assert_eq!(rows[0], ("fault_max_cell_index", 5.0));
        assert_eq!(rows[1], ("fault_min_cell_index", 12.0));
        assert_eq!(rows[2].0, "fault_max_cell_voltage");
        assert!((rows[2].1 - 4.2).abs() < 1e-9);
        assert_eq!(rows[3].0, "fault_min_cell_voltage");
        assert!((rows[3].1 - 3.8).abs() < 1e-9);
    }

    #[test]
    fn bps_detail_overtemp_decodes_signed_temperature() {
        // cell_index @0=7, temperature_c(i16) @2..3 = -10
        let t = (-10i16) as u16;
        let payload = payload_le([7, 0, (t & 0xFF) as u8, (t >> 8) as u8, 0, 0, 0, 0]);
        for code in [1 << BPS_OVERTEMP_AMB, 1 << BPS_OVERTEMP_CELL] {
            let rows = decode_bps_fault_detail(payload, code);
            assert_eq!(rows, vec![("fault_cell_index", 7.0), ("fault_temperature_c", -10.0)]);
        }
    }

    #[test]
    fn bps_detail_overcurrent_decodes_f32_current() {
        // current_a(f32) @byte0..3 = -73.5 A
        let bits = (-73.5f32).to_bits();
        let payload = payload_le([
            (bits & 0xFF) as u8, ((bits >> 8) & 0xFF) as u8,
            ((bits >> 16) & 0xFF) as u8, ((bits >> 24) & 0xFF) as u8,
            0, 0, 0, 0,
        ]);
        let rows = decode_bps_fault_detail(payload, 1 << BPS_OVERCURRENT);
        assert_eq!(rows, vec![("fault_current_a", -73.5)]);
    }

    #[test]
    fn bps_detail_no_payload_faults_and_no_fault_yield_no_rows() {
        assert!(decode_bps_fault_detail(u64::MAX, 0).is_empty());              // no fault
        assert!(decode_bps_fault_detail(u64::MAX, 1 << 8).is_empty());         // KILLSWITCH
        assert!(decode_bps_fault_detail(u64::MAX, 1 << 10).is_empty());        // DISCONNECTED
    }

    #[test]
    fn bps_detail_lowest_data_bearing_bit_wins() {
        // OVERVOLTAGE (0) + OVERCURRENT (6) both set -> cell view (lower bit).
        let payload = payload_le([2, 0, (40000u16 & 0xFF) as u8, (40000u16 >> 8) as u8, 0, 0, 0, 0]);
        let rows = decode_bps_fault_detail(payload, (1 << BPS_OVERVOLTAGE) | (1 << BPS_OVERCURRENT));
        assert_eq!(rows[0].0, "fault_cell_index");
    }
}
