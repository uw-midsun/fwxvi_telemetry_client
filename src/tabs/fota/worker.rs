//! FOTA worker thread: owns the XBee serial port, listens between jobs
//!
//! Ported from ms-bootloader `client/src/worker.rs`, serial-only. Always-on while spawned:
//! it auto-opens the port for the current config and, when no command is pending, spends a
//! short slice pumping the link (discovery over serial is pending gateway support, so the
//! node table stays empty for now). A Flash/Jump/Enter command preempts the listen, runs to
//! completion, then the loop resumes. `Command::Stop` exits the thread and releases the
//! port — important here because the telemetry decoder may want the same COM port.
//!
//! - **Author:** Midnight Sun Team #24

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TryRecvError};

use super::client::{self, Discovered, NodeMode, Progress};
use super::config::FotaConfig;
use super::image;
use super::serial::SerialTransport;
use super::shared::{Command, Event, NodeInfo};
use super::transport::Transport;

/// How long each idle listen slice waits before the loop checks for a command again
const LISTEN_SLICE: Duration = Duration::from_millis(300);
/// Backoff between reconnect attempts when the port will not open
const REOPEN_BACKOFF: Duration = Duration::from_millis(1000);

/// Map an engine `Progress` to a GUI `Event`
fn to_event(p: Progress) -> Event {
    match p {
        Progress::Stage(s) => Event::Stage(s),
        Progress::Chunk { done, total, bytes } => Event::Chunk { done, total, bytes },
        Progress::NodeAck { node } => Event::NodeAck { node },
        Progress::NodeNack {
            node,
            code,
            transient,
        } => Event::NodeNack {
            node,
            code: format!("{code:?}"),
            transient,
        },
        Progress::Retry {
            attempt,
            of,
            pending,
        } => Event::Retry {
            attempt,
            of,
            pending,
        },
    }
}

/// Reopen the port only when the addressing config actually changes. Per call
/// timeouts are read fresh each job, so they never force a reopen.
fn needs_reopen(open: &Option<FotaConfig>, cfg: &FotaConfig) -> bool {
    match open {
        None => true,
        Some(o) => o.serial_port != cfg.serial_port || o.serial_baud != cfg.serial_baud,
    }
}

/// A short label for the active link, shown on a successful open
fn link_label(cfg: &FotaConfig) -> String {
    format!("{} @ {}", cfg.serial_port, cfg.serial_baud)
}

/// Flatten an engine `Discovered` into the GUI `NodeInfo` table row, stamped with the arrival time.
fn to_node_info(d: Discovered, seen: Instant) -> NodeInfo {
    let mode = match d.mode {
        NodeMode::Bootloader => "bootloader",
        NodeMode::App => "app",
    }
    .to_string();
    NodeInfo {
        node: d.node,
        mode,
        board: super::names::board_name(d.node).to_string(),
        app_present: d.app_present,
        app_size: d.app_size,
        version: d.version,
        last_seen: seen,
    }
}

/// Fold one listen slice of announces into the live node map, age out silent nodes, and push the
/// current sorted view to the GUI
fn merge_and_emit(
    found: super::transport::DiscoveryAnswers,
    cfg: &FotaConfig,
    live: &mut HashMap<u16, NodeInfo>,
    tx: &Sender<Event>,
) {
    let now = Instant::now();
    for d in client::parse_discovered(found) {
        live.insert(d.node, to_node_info(d, now));
    }
    live.retain(|_, info| now.duration_since(info.last_seen) <= cfg.discovery_window);

    let mut list: Vec<NodeInfo> = live.values().cloned().collect();
    list.sort_by_key(|n| n.node);
    let _ = tx.try_send(Event::BootList(list));
}

pub fn run(
    cmd_rx: Receiver<Command>,
    event_tx: Sender<Event>,
    config: Arc<Mutex<FotaConfig>>,
    cancel: Arc<AtomicBool>,
) {
    let mut transport: Option<SerialTransport> = None;
    let mut open_cfg: Option<FotaConfig> = None;
    let mut live: HashMap<u16, NodeInfo> = HashMap::new();

    loop {
        let cfg = config.lock().map(|g| g.clone()).unwrap_or_default();

        // Auto-connect on startup and reopen whenever the addressing config changes
        if transport.is_none() || needs_reopen(&open_cfg, &cfg) {
            match SerialTransport::open(&cfg) {
                Ok(t) => {
                    transport = Some(t);
                    open_cfg = Some(cfg.clone());
                    live.clear();
                    let _ = event_tx.try_send(Event::BootList(Vec::new()));
                    let _ = event_tx
                        .try_send(Event::Status(format!("listening on {}", link_label(&cfg))));
                }
                Err(e) => {
                    transport = None;
                    open_cfg = None;
                    let _ = event_tx.try_send(Event::Status(format!(
                        "cannot open {}: {e}",
                        link_label(&cfg)
                    )));
                    // Wait for a config change (a fresh command) or retry after a backoff, no spin
                    match cmd_rx.recv_timeout(REOPEN_BACKOFF) {
                        Ok(Command::Stop) => return,
                        Ok(_) => {
                            let _ = event_tx.try_send(Event::JobFailed(format!(
                                "not connected to {}",
                                link_label(&cfg)
                            )));
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                    continue;
                }
            }
        }

        let link = transport.as_ref().expect("transport open above");
        let mut lost_link = false;
        match cmd_rx.try_recv() {
            Ok(Command::Stop) => return,
            Ok(cmd) => {
                cancel.store(false, Ordering::Relaxed);
                dispatch(cmd, link, &cfg, &event_tx, &cancel);
            }
            Err(TryRecvError::Empty) => match link.collect_discovery(LISTEN_SLICE) {
                Ok(found) => merge_and_emit(found, &cfg, &mut live, &event_tx),
                Err(e) => {
                    let _ = event_tx.try_send(Event::Status(format!("link read error: {e}")));
                    lost_link = true;
                }
            },
            Err(TryRecvError::Disconnected) => return,
        }

        if lost_link {
            transport = None;
            open_cfg = None;
        }
    }
}

fn dispatch(
    cmd: Command,
    link: &dyn Transport,
    cfg: &FotaConfig,
    tx: &Sender<Event>,
    cancel: &AtomicBool,
) {
    match cmd {
        Command::Flash {
            image: path,
            nodes,
            project,
            version,
            git,
            jump_after,
        } => run_flash(
            link, cfg, tx, cancel, path, nodes, project, version, git, jump_after,
        ),

        Command::Jump { nodes } => {
            let _ = tx.try_send(Event::JobStarted(format!("jump to app: nodes {nodes:?}")));
            match client::jump(link, cfg, &nodes) {
                Ok(()) => done(tx, format!("jump to app requested for nodes {nodes:?}")),
                Err(e) => fail(tx, e.to_string()),
            }
        }

        Command::Enter { mask } => {
            let label = if mask == 0 {
                "jump to bootloader: all nodes".to_string()
            } else {
                format!("jump to bootloader: mask {mask:#010X}")
            };
            let _ = tx.try_send(Event::JobStarted(label.clone()));
            match client::enter(link, mask) {
                Ok(()) => done(tx, label),
                Err(e) => fail(tx, e.to_string()),
            }
        }

        Command::Stop => unreachable!("Stop is handled in the main loop"),
    }
}

#[allow(clippy::too_many_arguments)]
fn run_flash(
    link: &dyn Transport,
    cfg: &FotaConfig,
    tx: &Sender<Event>,
    cancel: &AtomicBool,
    path: PathBuf,
    nodes: Vec<u16>,
    project: String,
    version: (u8, u8),
    git: String,
    jump_after: bool,
) {
    let _ = tx.try_send(Event::JobStarted(format!(
        "flash {} to nodes {nodes:?}",
        path.display()
    )));

    let bytes = match image::load_image(&path) {
        Ok(b) => b,
        Err(e) => return fail(tx, e.to_string()),
    };
    let project = if project.trim().is_empty() {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("app")
            .to_string()
    } else {
        project
    };
    let meta = image::build_metadata(&bytes, &project, version, &git);

    let mut report = |p: Progress| {
        let _ = tx.try_send(to_event(p));
    };
    let cancel_fn = || cancel.load(Ordering::Relaxed);

    match client::flash(
        link,
        cfg,
        &nodes,
        &bytes,
        meta,
        jump_after,
        &mut report,
        &cancel_fn,
    ) {
        Ok(()) => done(tx, format!("flash complete on nodes {nodes:?}")),
        Err(e) => fail(tx, e.to_string()),
    }
}

fn done(tx: &Sender<Event>, msg: String) {
    let _ = tx.try_send(Event::JobDone(msg));
}

fn fail(tx: &Sender<Event>, msg: String) {
    let _ = tx.try_send(Event::JobFailed(msg));
}
