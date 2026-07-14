//! FOTA tab: flash firmware to car ECUs over the XBee serial link
//!
//! Ported from the ms-bootloader host GUI (`client/src/gui.rs`) into a telemetry-client
//! tab. Serial (XBee) only — the SocketCAN path is Linux-only and was left in the
//! standalone tool. The serial port lives on a worker thread the tab spawns on Connect
//! and stops on Disconnect, so the port is only held while FOTA is in use (the telemetry
//! decoder may want the same COM port).
//!
//! - **Author:** Midnight Sun Team #24

mod client;
mod config;
mod image;
mod names;
mod protocol;
mod serial;
mod shared;
mod transport;
mod worker;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::time::Duration;

use egui::{Color32, RichText};

use config::FotaConfig;
use shared::{Command, Event, NodeInfo, SharedData};

/// Highest node id plus one; the datagram mask is 64-bit (one bit per node id)
const MAX_NODES: usize = 64;
/// Cap on retained log lines
const LOG_MAX: usize = 500;

/// Colour for an inline error label
const ERROR_RED: Color32 = Color32::from_rgb(200, 70, 70);
/// Accent used for the flash action and active highlights
const ACCENT: Color32 = Color32::from_rgb(45, 140, 80);

pub struct FotaTab {
    /// Present while the worker thread is running and holds the serial port
    worker: Option<SharedData>,

    // Connection form (string backed so partial edits are allowed)
    serial_port: String,
    serial_baud: String,
    timeout_ms: String,
    response_ms: String,
    retries: String,

    // Flash inputs
    image_path: String,
    manifest_path: String,
    node_spec: String,
    project: String,
    version: String,
    git: String,
    jump_after: bool,

    // Live job state
    busy: bool,
    file_hovering: bool,
    status: String,
    stage: String,
    progress: Option<(usize, usize)>,
    node_status: BTreeMap<u16, String>,
    boot_list: Vec<NodeInfo>,
    log: VecDeque<String>,
}

impl FotaTab {
    /// `default_port` seeds the port field — the XBee is usually the same COM port the
    /// telemetry decoder uses, so the telemetry settings value is a good starting point.
    pub fn new(default_port: &str) -> Self {
        let cfg = FotaConfig::default();
        Self {
            worker: None,
            serial_port: if default_port.trim().is_empty() {
                cfg.serial_port.clone()
            } else {
                default_port.to_string()
            },
            serial_baud: cfg.serial_baud.to_string(),
            timeout_ms: cfg.ack_timeout.as_millis().to_string(),
            response_ms: cfg.discovery_window.as_millis().to_string(),
            retries: cfg.retries.to_string(),
            image_path: String::new(),
            manifest_path: String::new(),
            node_spec: String::new(),
            project: String::new(),
            version: "0.0".to_string(),
            git: String::new(),
            jump_after: true,
            busy: false,
            file_hovering: false,
            status: "disconnected".to_string(),
            stage: String::new(),
            progress: None,
            node_status: BTreeMap::new(),
            boot_list: Vec::new(),
            log: VecDeque::new(),
        }
    }

    pub fn connected(&self) -> bool {
        self.worker.is_some()
    }

    /// Stop the worker and release the serial port. Safe to call when not connected.
    pub fn shutdown(&mut self) {
        if let Some(shared) = self.worker.take() {
            shared.request_cancel();
            shared.send(Command::Stop);
        }
        self.busy = false;
        self.stage.clear();
        self.progress = None;
        self.boot_list.clear();
        self.status = "disconnected".to_string();
    }

    // == Helpers =============================================================

    fn push_log(&mut self, line: impl Into<String>) {
        self.log.push_back(line.into());
        while self.log.len() > LOG_MAX {
            self.log.pop_front();
        }
    }

    /// Parse the node text field into a sorted unique list, an empty field yields no nodes
    fn parsed_nodes(&self) -> Result<Vec<u16>, String> {
        parse_node_spec(&self.node_spec)
    }

    /// The parsed node list, empty when the field is empty or fails to parse (the UI shows the error)
    fn selected_nodes(&self) -> Vec<u16> {
        self.parsed_nodes().unwrap_or_default()
    }

    /// 32-bit ENTER mask plus whether any node >= 32 was selected (those can't fit it)
    fn selected_mask32(&self) -> (u32, bool) {
        let mut mask = 0u32;
        let mut has_high = false;
        for n in self.selected_nodes() {
            if n < 32 {
                mask |= 1u32 << n;
            } else {
                has_high = true;
            }
        }
        (mask, has_high)
    }

    fn has_image(&self) -> bool {
        !self.image_path.trim().is_empty()
    }

    /// Set the selected image and default the manifest to its conventional sibling, applying it
    /// when present so the project, version and git of the actual artifact carry over for free.
    fn set_image(&mut self, path: String) {
        self.image_path = path;
        let sibling = image::sibling_manifest_path(std::path::Path::new(self.image_path.trim()));
        self.manifest_path = sibling.display().to_string();
        self.apply_manifest();
    }

    /// Point at an explicit manifest file (overriding the sibling default) and apply it
    fn set_manifest(&mut self, path: String) {
        self.manifest_path = path;
        self.apply_manifest();
    }

    /// Load the manifest at the current manifest_path and pre-fill the flash metadata fields
    fn apply_manifest(&mut self) {
        let path = self.manifest_path.trim().to_string();
        if path.is_empty() {
            return;
        }
        let Some(m) = image::read_manifest(std::path::Path::new(&path)) else {
            self.push_log(format!("no usable manifest at {path}"));
            return;
        };
        if !m.project.is_empty() {
            self.project = m.project.clone();
        }
        if !m.version.is_empty() {
            self.version = m.version.clone();
        }
        if !m.git_hash.is_empty() {
            self.git = m.git_hash.clone();
        }
        self.push_log(format!(
            "loaded manifest: project={} board={} ver={} git={}",
            m.project, m.board, m.version, m.git_hash
        ));
    }

    /// Parse the form into a FotaConfig. Returns the parse error.
    fn parse_config(&self) -> Result<FotaConfig, String> {
        let timeout = self
            .timeout_ms
            .trim()
            .parse::<u64>()
            .map_err(|_| format!("bad timeout '{}'", self.timeout_ms))?;
        let response = self
            .response_ms
            .trim()
            .parse::<u64>()
            .map_err(|_| format!("bad response window '{}'", self.response_ms))?;
        let retries = self
            .retries
            .trim()
            .parse::<u32>()
            .map_err(|_| format!("bad retries '{}'", self.retries))?;
        let serial_baud = self
            .serial_baud
            .trim()
            .parse::<u32>()
            .map_err(|_| format!("bad baud '{}'", self.serial_baud))?;

        Ok(FotaConfig {
            serial_port: self.serial_port.trim().to_string(),
            serial_baud,
            ack_timeout: Duration::from_millis(timeout),
            discovery_window: Duration::from_millis(response),
            retries,
        })
    }

    /// Push the current form config to the running worker, logging a parse error instead
    fn apply_config(&mut self) -> Result<(), String> {
        let cfg = self.parse_config()?;
        if let Some(ref shared) = self.worker {
            shared.set_config(cfg);
        }
        Ok(())
    }

    /// Apply the config then run `f` to send a command, logging a config error instead
    fn with_config<F: FnOnce(&mut Self)>(&mut self, f: F) {
        match self.apply_config() {
            Ok(()) => f(self),
            Err(e) => {
                self.push_log(format!("config error: {e}"));
                self.status = format!("config error: {e}");
            }
        }
    }

    fn send(&self, cmd: Command) {
        if let Some(ref shared) = self.worker {
            shared.send(cmd);
        }
    }

    /// Spawn the worker thread, which opens the serial port for the current config
    fn connect(&mut self) {
        let cfg = match self.parse_config() {
            Ok(c) => c,
            Err(e) => {
                self.push_log(format!("config error: {e}"));
                self.status = format!("config error: {e}");
                return;
            }
        };
        let shared = SharedData::new(cfg);
        let (cmd_rx, event_tx, config, cancel) = shared.worker_endpoints();
        std::thread::spawn(move || worker::run(cmd_rx, event_tx, config, cancel));
        self.worker = Some(shared);
        self.status = "connecting...".to_string();
        self.push_log("FOTA link starting");
    }

    // == Event pump ==========================================================

    fn drain_events(&mut self) {
        let Some(shared) = self.worker.clone() else {
            return;
        };
        while let Ok(ev) = shared.try_recv_event() {
            match ev {
                Event::JobStarted(s) => {
                    self.busy = true;
                    self.push_log(format!("> {s}"));
                }
                Event::Stage(s) => {
                    self.stage = s.clone();
                    self.push_log(s);
                }
                Event::Chunk { done, total, bytes } => {
                    self.progress = Some((done, total));
                    self.push_log(format!("chunk {done}/{total} ({bytes} B) acked"));
                }
                Event::NodeAck { node } => {
                    self.node_status.insert(node, "ok".to_string());
                }
                Event::NodeNack {
                    node,
                    code,
                    transient,
                } => {
                    self.node_status.insert(node, code.clone());
                    let tail = if transient { " (retrying)" } else { "" };
                    self.push_log(format!("node {node} NACK {code}{tail}"));
                }
                Event::Retry {
                    attempt,
                    of,
                    pending,
                } => {
                    self.push_log(format!("retry {attempt}/{of}, pending {pending:?}"));
                }
                Event::BootList(list) => {
                    self.boot_list = list;
                }
                Event::JobDone(s) => {
                    self.busy = false;
                    self.stage.clear();
                    self.push_log(format!("[ok] {s}"));
                }
                Event::JobFailed(s) => {
                    self.busy = false;
                    self.stage.clear();
                    self.status = format!("failed: {s}");
                    self.push_log(format!("[fail] {s}"));
                }
                Event::Status(s) => {
                    self.status = s.clone();
                    self.push_log(s);
                }
            }
        }
    }

    // == Panels ==============================================================

    fn config_panel(&mut self, ui: &mut egui::Ui, telemetry_connected: bool) {
        ui.add_space(4.0);
        ui.heading("XBee link");
        egui::Grid::new("fota_cfg")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label("Port");
                ui.text_edit_singleline(&mut self.serial_port);
                ui.end_row();
                ui.label("Baud");
                ui.text_edit_singleline(&mut self.serial_baud);
                ui.end_row();
                ui.label("ACK timeout (ms)");
                ui.text_edit_singleline(&mut self.timeout_ms);
                ui.end_row();
                ui.label("Node timeout (ms)");
                ui.text_edit_singleline(&mut self.response_ms)
                    .on_hover_text(
                        "How long a node stays in the live table after its last announce",
                    );
                ui.end_row();
                ui.label("Retries");
                ui.text_edit_singleline(&mut self.retries);
                ui.end_row();
            });
        ui.add_space(6.0);

        ui.horizontal(|ui| {
            if self.connected() {
                if ui.button("Disconnect").clicked() {
                    self.shutdown();
                    self.push_log("FOTA link stopped");
                }
                if ui.button("Apply").clicked() {
                    match self.apply_config() {
                        Ok(()) => self.push_log("config applied"),
                        Err(e) => self.push_log(format!("config error: {e}")),
                    }
                }
            } else if ui
                .add(egui::Button::new(
                    RichText::new("Connect").strong().color(Color32::WHITE),
                ).fill(ACCENT))
                .clicked()
            {
                self.connect();
            }
        });

        if telemetry_connected {
            ui.add_space(4.0);
            ui.label(
                RichText::new(
                    "Telemetry decoder is connected — disconnect it in Settings first if it \
                     holds the same COM port.",
                )
                .small()
                .color(Color32::from_rgb(200, 160, 60)),
            );
        }

        ui.add_space(10.0);
        ui.separator();
        ui.heading("Recovery");
        ui.label(RichText::new("Targets come from the node field in the flash panel").weak());
        ui.add_enabled_ui(self.connected() && !self.busy, |ui| {
            if ui.button("Jump to Bootloader (all)").clicked() {
                self.with_config(|s| s.send(Command::Enter { mask: 0 }));
            }
            if ui.button("Jump to Bootloader (selected)").clicked() {
                let (mask, has_high) = self.selected_mask32();
                if mask == 0 {
                    self.push_log(
                        "no nodes selected: use Jump to Bootloader (all) for a broadcast",
                    );
                } else {
                    if has_high {
                        self.push_log(
                            "enter mask is 32-bit; nodes >=32 ignored, use the (all) button",
                        );
                    }
                    self.with_config(|s| s.send(Command::Enter { mask }));
                }
            }
            if ui.button("Jump to App (selected)").clicked() {
                let nodes = self.selected_nodes();
                if nodes.is_empty() {
                    self.push_log("no nodes selected for jump to app");
                } else {
                    self.with_config(|s| s.send(Command::Jump { nodes }));
                }
            }
        });
    }

    fn drop_zone(&mut self, ui: &mut egui::Ui) {
        let (fill, stroke) = if self.file_hovering {
            (ACCENT.linear_multiply(0.18), egui::Stroke::new(1.5, ACCENT))
        } else {
            (
                ui.visuals().faint_bg_color,
                ui.visuals().widgets.noninteractive.bg_stroke,
            )
        };
        egui::Frame::none()
            .fill(fill)
            .stroke(stroke)
            .rounding(8.0)
            .inner_margin(egui::Margin::same(12.0))
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new("Drop a .elf or .bin anywhere on the window").strong(),
                    );
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label("Path");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.image_path)
                                .hint_text("path/to/app.bin")
                                .desired_width(340.0),
                        );
                        if ui.button("Browse").clicked() {
                            if let Some(p) = rfd::FileDialog::new()
                                .add_filter("firmware image", &["bin", "elf"])
                                .pick_file()
                            {
                                self.set_image(p.display().to_string());
                            }
                        }
                        if self.has_image() && ui.button("Clear").clicked() {
                            self.image_path.clear();
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label("Manifest");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.manifest_path)
                                .hint_text("auto: <image>.manifest.json")
                                .desired_width(340.0),
                        )
                        .on_hover_text("Optional. Defaults to the sibling .manifest.json; override to point elsewhere.");
                        if ui.button("Load").clicked() {
                            self.apply_manifest();
                        }
                    });
                });
            });
    }

    fn node_selector(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.strong("Target nodes");
            match self.parsed_nodes() {
                Ok(n) => {
                    ui.label(RichText::new(format!("({} selected)", n.len())).weak());
                }
                Err(e) => {
                    ui.colored_label(ERROR_RED, e);
                }
            }
        });
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.node_spec)
                    .hint_text("e.g. 0, 3, 5  or  0-7")
                    .desired_width(260.0),
            );
            if ui.small_button("All").clicked() {
                self.node_spec = format!("0-{}", MAX_NODES - 1);
            }
            if ui.small_button("None").clicked() {
                self.node_spec.clear();
            }
        });
    }

    fn flash_panel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.heading("Flash application");
        ui.add_space(4.0);
        self.drop_zone(ui);

        ui.add_space(10.0);
        self.node_selector(ui);

        ui.add_space(10.0);
        egui::CollapsingHeader::new("Metadata (optional)")
            .default_open(false)
            .show(ui, |ui| {
                egui::Grid::new("fota_meta").num_columns(2).show(ui, |ui| {
                    ui.label("Project");
                    ui.text_edit_singleline(&mut self.project);
                    ui.end_row();
                    ui.label("Version (M.m)");
                    ui.text_edit_singleline(&mut self.version);
                    ui.end_row();
                    ui.label("Git hash");
                    ui.text_edit_singleline(&mut self.git);
                    ui.end_row();
                });
            });

        ui.add_space(8.0);
        ui.checkbox(&mut self.jump_after, "Jump to application after flashing");

        ui.add_space(10.0);
        let can_flash = self.connected()
            && !self.busy
            && self.has_image()
            && !self.selected_nodes().is_empty();
        ui.horizontal(|ui| {
            let flash = egui::Button::new(RichText::new("Flash").strong().color(Color32::WHITE))
                .fill(ACCENT)
                .min_size(egui::vec2(120.0, 28.0));
            if ui.add_enabled(can_flash, flash).clicked() {
                self.start_flash();
            }
            if self.busy && ui.button("Cancel").clicked() {
                if let Some(ref shared) = self.worker {
                    shared.request_cancel();
                }
                self.push_log("cancel requested");
            }
            if !self.connected() {
                ui.label(RichText::new("connect the XBee link first").weak());
            }
        });

        if !self.stage.is_empty() {
            ui.add_space(8.0);
            ui.label(RichText::new(&self.stage).italics());
        }
        if let Some((done, total)) = self.progress {
            let frac = if total == 0 {
                0.0
            } else {
                done as f32 / total as f32
            };
            ui.add(
                egui::ProgressBar::new(frac)
                    .text(format!("{done}/{total} chunks, {:.0}%", frac * 100.0))
                    .animate(self.busy),
            );
        }
        if !self.node_status.is_empty() {
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                for (node, st) in &self.node_status {
                    let ok = st == "ok";
                    let mark = if ok { "[ok]" } else { "[x]" };
                    let color = if ok { ACCENT } else { ERROR_RED };
                    ui.colored_label(color, format!("{mark} node {node} ({st})"));
                }
            });
        }

        // Live bus table, auto-populated from the announce heartbeat once the fota gateway
        // relays downstream CAN heartbeats upstream (pending firmware change).
        ui.add_space(10.0);
        ui.separator();
        ui.horizontal(|ui| {
            ui.heading("Nodes on the bus");
            ui.label(RichText::new(format!("({} live)", self.boot_list.len())).weak());
        });
        if self.boot_list.is_empty() {
            ui.label(
                RichText::new(
                    "Node discovery over the XBee link is pending gateway support — \
                     target nodes manually above.",
                )
                .weak(),
            );
        } else {
            egui::Grid::new("fota_bootls")
                .striped(true)
                .num_columns(6)
                .spacing([14.0, 4.0])
                .show(ui, |ui| {
                    for h in ["node", "mode", "board", "app", "ver", "last rx"] {
                        ui.label(RichText::new(h).strong());
                    }
                    ui.end_row();
                    for n in &self.boot_list {
                        ui.label(n.node.to_string());
                        ui.label(&n.mode);
                        ui.label(&n.board);
                        ui.label(if n.app_present {
                            format!("{} B", n.app_size)
                        } else {
                            "none".to_string()
                        });
                        ui.label(format!("{}.{}", n.version.0, n.version.1));
                        ui.label(format!("{:.1}s ago", n.last_seen.elapsed().as_secs_f32()));
                        ui.end_row();
                    }
                });
        }
    }

    fn start_flash(&mut self) {
        if !self.has_image() {
            return;
        }
        let path = PathBuf::from(self.image_path.trim());
        let nodes = self.selected_nodes();
        let version = match parse_version(&self.version) {
            Ok(v) => v,
            Err(e) => {
                self.push_log(format!("config error: {e}"));
                return;
            }
        };
        self.progress = None;
        self.node_status.clear();
        let project = self.project.clone();
        let git = self.git.clone();
        let jump_after = self.jump_after;
        self.with_config(move |s| {
            s.send(Command::Flash {
                image: path,
                nodes,
                project,
                version,
                git,
                jump_after,
            });
        });
    }

    fn log_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Log");
            if ui.button("Clear").clicked() {
                self.log.clear();
            }
        });
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for line in &self.log {
                    ui.label(RichText::new(line).monospace());
                }
            });
    }

    // == Tab entry point =====================================================

    pub fn show(&mut self, ui: &mut egui::Ui, telemetry_connected: bool) {
        // Accept dropped files and track hover state for the drop zone highlight.
        // Only runs while this tab is visible, so drops elsewhere are unaffected.
        let (dropped, hovering) = ui
            .ctx()
            .input(|i| (i.raw.dropped_files.clone(), !i.raw.hovered_files.is_empty()));
        self.file_hovering = hovering;
        for f in dropped {
            if let Some(p) = f.path {
                // A dropped .json is treated as the manifest, anything else as the image
                let is_json = p
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("json"));
                if is_json {
                    self.set_manifest(p.display().to_string());
                } else {
                    self.set_image(p.display().to_string());
                }
            }
        }

        self.drain_events();

        egui::TopBottomPanel::top("fota_status").show_inside(ui, |ui| {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.strong("FOTA");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.busy {
                        ui.spinner();
                    }
                    ui.label(RichText::new(&self.status).weak());
                });
            });
            ui.add_space(2.0);
        });

        egui::SidePanel::left("fota_side")
            .resizable(true)
            .default_width(270.0)
            .show_inside(ui, |ui| {
                egui::ScrollArea::vertical()
                    .show(ui, |ui| self.config_panel(ui, telemetry_connected));
            });

        egui::TopBottomPanel::bottom("fota_log")
            .resizable(true)
            .default_height(150.0)
            .show_inside(ui, |ui| self.log_panel(ui));

        egui::CentralPanel::default().show_inside(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.flash_panel(ui));
        });

        // Events arrive from the worker asynchronously: poll briskly while a job is running
        // (or a file drag is in flight), at a relaxed cadence while merely connected.
        if self.busy || self.file_hovering {
            ui.ctx().request_repaint_after(Duration::from_millis(120));
        } else if self.connected() {
            ui.ctx().request_repaint_after(Duration::from_millis(500));
        }
    }
}

// == Field parsers ============================================================

/// Parse a node target field into a sorted unique list. Accepts single ids, comma separated
/// lists and inclusive ranges, e.g. "0", "0,3,5", "0-7" or "0-3,5,8". Empty yields no nodes.
fn parse_node_spec(s: &str) -> Result<Vec<u16>, String> {
    let mut nodes: BTreeSet<u16> = BTreeSet::new();
    for tok in s.split(',') {
        let tok = tok.trim();
        if tok.is_empty() {
            continue;
        }
        if let Some((a, b)) = tok.split_once('-') {
            let lo = parse_node(a)?;
            let hi = parse_node(b)?;
            if lo > hi {
                return Err(format!("range '{tok}' is backwards"));
            }
            nodes.extend(lo..=hi);
        } else {
            nodes.insert(parse_node(tok)?);
        }
    }
    Ok(nodes.into_iter().collect())
}

/// Parse one node id and bound it to the valid 0..=63 range
fn parse_node(s: &str) -> Result<u16, String> {
    let n: u16 = s
        .trim()
        .parse()
        .map_err(|_| format!("bad node '{}'", s.trim()))?;
    if (n as usize) >= MAX_NODES {
        return Err(format!("node {n} out of range 0..={}", MAX_NODES - 1));
    }
    Ok(n)
}

/// Parse a MAJOR.MINOR version string, defaulting the minor to 0
fn parse_version(s: &str) -> Result<(u8, u8), String> {
    let (a, b) = s.split_once('.').unwrap_or((s, "0"));
    let major = a
        .trim()
        .parse()
        .map_err(|_| format!("bad version major in '{s}'"))?;
    let minor = b
        .trim()
        .parse()
        .map_err(|_| format!("bad version minor in '{s}'"))?;
    Ok((major, minor))
}
