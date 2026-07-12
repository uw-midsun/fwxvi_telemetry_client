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
                    if decode_datagram(&conn, &dgram, &messages, debug_bytes) {
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
fn decode_datagram(conn: &Connection, dgram: &Datagram, messages: &[CanMessage], debug: bool) -> bool {
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
    for sig in &msg.signals {
        let raw = extract_signal(payload, sig.start_bit, sig.length, sig.signed);
        let value = raw as f64 * sig.scale.unwrap_or(1.0);
        let sample = SignalSample {
            ts_ms,
            can_id:       dgram.id as u32,
            parent_name:  msg.name.clone(),
            message_name: msg.name.clone(),
            signal_name:  sig.name.clone(),
            value,
        };
        let _ = insert_signal(&tx, &sample);
    }
    let _ = tx.commit();
    true
}
