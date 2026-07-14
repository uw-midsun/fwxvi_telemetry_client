//! Shared command / event channels and config between the FOTA tab and its worker
//!
//! Ported from ms-bootloader `client/src/shared.rs` (flume swapped for crossbeam-channel,
//! which the telemetry client already depends on).
//!
//! - **Author:** Midnight Sun Team #24

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crossbeam_channel::{bounded, Receiver, Sender, TryRecvError};

use super::config::FotaConfig;

// == Commands (GUI to worker) ================================================

/// A job the worker should run. Each one reuses the open serial link, executes,
/// and reports its outcome through `Event`.
#[derive(Clone, Debug)]
pub enum Command {
    Flash {
        image: PathBuf,
        nodes: Vec<u16>,
        project: String,
        version: (u8, u8),
        git: String,
        jump_after: bool,
    },
    Jump {
        nodes: Vec<u16>,
    },
    Enter {
        mask: u32,
    },
    /// Shut the worker down and release the serial port
    Stop,
}

// == Events (worker to GUI) ==================================================

/// One node heard on the bus, the flattened view the live table renders. Every node, bootloader or
/// running app, broadcasts a heartbeat, `mode` marks which firmware is running and `board` is
/// resolved from the node id (names are not on the wire). `last_seen` is when its most recent
/// heartbeat arrived.
#[derive(Clone, Debug)]
pub struct NodeInfo {
    pub node: u16,
    pub mode: String,
    pub board: String,
    pub app_present: bool,
    pub app_size: u32,
    pub version: (u8, u8),
    pub last_seen: Instant,
}

/// Everything the worker reports back. The GUI also derives a log line from each.
#[derive(Clone, Debug)]
pub enum Event {
    /// A job has begun, carries a short label
    JobStarted(String),
    /// A human readable phase change inside a job
    Stage(String),
    /// One flash chunk acknowledged by every target
    Chunk {
        done: usize,
        total: usize,
        bytes: usize,
    },
    /// A node acknowledged the current datagram
    NodeAck { node: u16 },
    /// A node reported a NACK
    NodeNack {
        node: u16,
        code: String,
        transient: bool,
    },
    /// The current datagram is being resent to the laggards
    Retry {
        attempt: u32,
        of: u32,
        pending: Vec<u16>,
    },
    /// The current live view of the bus, every node heard within the timeout, sorted by id
    BootList(Vec<NodeInfo>),
    /// The current job finished successfully
    JobDone(String),
    /// The current job failed
    JobFailed(String),
    /// Connection / interface status changed
    Status(String),
}

// == Shared handle ===========================================================

#[derive(Clone)]
pub struct SharedData {
    pub cmd_tx: Sender<Command>,
    cmd_rx: Receiver<Command>,
    pub event_tx: Sender<Event>,
    pub event_rx: Receiver<Event>,
    config: Arc<Mutex<FotaConfig>>,
    cancel: Arc<AtomicBool>,
}

impl SharedData {
    pub fn new(cfg: FotaConfig) -> Self {
        let (cmd_tx, cmd_rx) = bounded(16);
        let (event_tx, event_rx) = bounded(1024);
        Self {
            cmd_tx,
            cmd_rx,
            event_tx,
            event_rx,
            config: Arc::new(Mutex::new(cfg)),
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Endpoints the worker thread owns
    pub fn worker_endpoints(
        &self,
    ) -> (
        Receiver<Command>,
        Sender<Event>,
        Arc<Mutex<FotaConfig>>,
        Arc<AtomicBool>,
    ) {
        (
            self.cmd_rx.clone(),
            self.event_tx.clone(),
            Arc::clone(&self.config),
            Arc::clone(&self.cancel),
        )
    }

    pub fn set_config(&self, cfg: FotaConfig) {
        if let Ok(mut g) = self.config.lock() {
            *g = cfg;
        }
    }

    pub fn request_cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn send(&self, cmd: Command) {
        let _ = self.cmd_tx.try_send(cmd);
    }

    pub fn try_recv_event(&self) -> Result<Event, TryRecvError> {
        self.event_rx.try_recv()
    }
}
