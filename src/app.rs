use crate::config::{db_path, AppConfig};
use crate::decoder::can_config::{self, EnumLookup};
use crate::decoder::DecoderCmd;
use crate::replay::{ReplayController, SPEEDS};
use crate::tabs::{
    cells::CellsTab, dashboard::DashboardTab, fota::FotaTab, settings::SettingsTab,
    signal_table::SignalTableTab,
};
use crossbeam_channel::Sender;
use std::path::{Path, PathBuf};
use std::thread;

#[derive(PartialEq)]
enum Tab {
    SignalTable,
    Dashboard,
    Cells,
    Fota,
    Settings,
}

pub struct TelemetryApp {
    cfg:           AppConfig,
    settings_path: PathBuf,
    db_conn:       Option<rusqlite::Connection>,

    signal_table:  SignalTableTab,
    dashboard:     DashboardTab,
    cells_tab:     CellsTab,
    fota:          FotaTab,
    settings_tab:  SettingsTab,

    active_tab:    Tab,

    // Live decoder
    connected:     bool,
    decoder_tx:    Option<Sender<DecoderCmd>>,

    // Replay
    replay:        Option<ReplayController>,

    enum_lookup:   EnumLookup,

    dark_mode:     bool,
}

impl TelemetryApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let (cfg, settings_path) = AppConfig::load_or_default();
        let db_conn = crate::db::open(&db_path()).ok();
        let settings_tab = SettingsTab::new(&cfg);

        let messages = can_config::load(&resolve_yaml(&cfg.can_yaml_path)).unwrap_or_default();
        let enum_lookup = can_config::build_enum_lookup(&messages);

        // Start in dark mode; the tab bar has a toggle to switch to light.
        cc.egui_ctx.set_visuals(egui::Visuals::dark());

        Self {
            cfg: cfg.clone(),
            settings_path,
            db_conn,
            signal_table:  SignalTableTab::new(),
            dashboard:     DashboardTab::new(),
            cells_tab:     CellsTab::new(),
            fota:          FotaTab::new(&cfg.com_port),
            settings_tab,
            active_tab:    Tab::SignalTable,
            connected:     false,
            decoder_tx:    None,
            replay:        None,
            enum_lookup,
            dark_mode:     true,
        }
    }

    // ── Live decoder ──────────────────────────────────────────────────────────

    fn connect(&mut self) {
        let baud      = self.cfg.baud_rate;
        let port_name = self.cfg.com_port.clone();

        let port = match serialport::new(&port_name, baud)
            .timeout(std::time::Duration::from_millis(100))
            .open()
        {
            Ok(p)  => p,
            Err(e) => {
                self.settings_tab.status_msg = format!("Cannot open {port_name}: {e}");
                return;
            }
        };

        let (tx, rx) = crossbeam_channel::bounded(1);
        let db   = db_path();
        let yaml = resolve_yaml(&self.cfg.can_yaml_path);

        // Refresh enum lookup in case the YAML path changed since startup
        if let Ok(messages) = can_config::load(&yaml) {
            self.enum_lookup = can_config::build_enum_lookup(&messages);
        }

        thread::spawn(move || crate::decoder::run(port, rx, db, yaml));

        // Skip past any rows a previous session left in the DB, so the live table
        // shows only data from this capture instead of replaying old samples.
        if let Some(conn) = self.db_conn.as_ref() {
            self.signal_table.reset_to_latest(conn);
        }

        self.decoder_tx = Some(tx);
        self.connected  = true;
        self.settings_tab.status_msg = format!("Connected to {port_name} @ {baud} baud");
    }

    fn disconnect(&mut self) {
        if let Some(tx) = self.decoder_tx.take() {
            let _ = tx.send(DecoderCmd::Stop);
        }
        self.connected = false;
        self.settings_tab.status_msg = "Disconnected.".to_string();
    }

    // ── Replay ────────────────────────────────────────────────────────────────

    fn load_replay(&mut self, path: &Path) {
        match ReplayController::open(path) {
            Ok(ctrl) => {
                self.signal_table.clear();
                self.replay = Some(ctrl);
                self.settings_tab.status_msg =
                    format!("Replay loaded: {}", path.display());
            }
            Err(e) => {
                self.settings_tab.status_msg = format!("Failed to load replay: {e}");
            }
        }
    }

    fn stop_replay(&mut self) {
        self.replay = None;
        self.signal_table.clear();
        self.settings_tab.status_msg = "Replay stopped.".to_string();
    }
}

impl eframe::App for TelemetryApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Schedule passive repaints at the cadence each surface actually needs, rather
        // than a blanket 60fps. User input always triggers an immediate repaint on top of
        // this, so interactivity is unaffected.
        //   - Replay playback needs smooth ~60fps frames for the slider/table motion.
        //   - The live signal table polls the DB every 250ms (see SignalTableTab).
        //   - The dashboard schedules its own repaint at its data-refresh cadence.
        if self.replay.as_ref().map_or(false, |r| r.playing) {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        } else if self.connected && self.active_tab == Tab::SignalTable {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }

        // Tick replay and refresh signal table if position changed
        if let Some(ref mut replay) = self.replay {
            let changed = replay.tick() || replay.needs_refresh();
            if changed {
                let snapshot = replay.snapshot();
                // load_snapshot() clears rows internally — do NOT call clear() here,
                // which would also wipe read_counts and cause a scroll-position reset.
                self.signal_table.load_snapshot(&snapshot);
            }
        }

        // ── Tab bar ───────────────────────────────────────────────────────────
        egui::TopBottomPanel::top("tab_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.active_tab, Tab::SignalTable, "Signal Table");
                ui.selectable_value(&mut self.active_tab, Tab::Dashboard,   "Dashboard");
                ui.selectable_value(&mut self.active_tab, Tab::Cells,       "Cells");
                ui.selectable_value(&mut self.active_tab, Tab::Fota,        "FOTA");
                ui.selectable_value(&mut self.active_tab, Tab::Settings,    "Settings");

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Light / dark toggle. Icon shows the mode you'd switch TO.
                    let icon = if self.dark_mode { "☀ Light" } else { "🌙 Dark" };
                    if ui.button(icon).on_hover_text("Toggle light / dark theme").clicked() {
                        self.dark_mode = !self.dark_mode;
                        let visuals = if self.dark_mode {
                            egui::Visuals::dark()
                        } else {
                            egui::Visuals::light()
                        };
                        ctx.set_visuals(visuals);
                    }
                    ui.separator();

                    // Replay indicator
                    if let Some(ref r) = self.replay {
                        let icon = if r.playing { "> Replay" } else { "|| Replay" };
                        ui.colored_label(egui::Color32::from_rgb(200, 160, 60), icon);
                        ui.separator();
                    }
                    // Live indicator
                    let (label, color) = if self.connected {
                        ("● Live", egui::Color32::from_rgb(80, 200, 80))
                    } else {
                        ("○ Idle", egui::Color32::GRAY)
                    };
                    ui.colored_label(color, label);
                });
            });
        });

        // ── Replay controls (bottom bar) ──────────────────────────────────────
        if self.replay.is_some() {
            egui::TopBottomPanel::bottom("replay_bar")
                .min_height(52.0)
                .show(ctx, |ui| {
                    replay_bar(ui, self.replay.as_mut().unwrap());
                });
        }

        // ── Central panel ─────────────────────────────────────────────────────
        egui::CentralPanel::default().show(ctx, |ui| {
            match self.active_tab {
                Tab::SignalTable => {
                    if self.replay.is_some() {
                        self.signal_table.show_replay(ui, &self.enum_lookup);
                    } else if let Some(ref conn) = self.db_conn {
                        self.signal_table.show(ui, conn, &self.enum_lookup);
                    } else {
                        ui.label("Database unavailable.");
                    }
                }
                Tab::Dashboard => {
                    self.dashboard.show(ui, self.db_conn.as_ref(), &self.enum_lookup);
                }
                Tab::Cells => {
                    self.cells_tab.show(ui, self.db_conn.as_ref());
                }
                Tab::Fota => {
                    self.fota.show(ui, self.connected);
                }
                Tab::Settings => {
                    let mut cfg            = self.cfg.clone();
                    let settings_path      = self.settings_path.clone();
                    let connected          = self.connected;
                    let replay_loaded      = self.replay.is_some();

                    let mut do_connect     = false;
                    let mut do_disconnect  = false;
                    let mut replay_path: Option<PathBuf> = None;
                    let mut do_stop_replay = false;

                    self.settings_tab.show(
                        ui,
                        &mut cfg,
                        &settings_path,
                        connected,
                        &mut |_| do_connect = true,
                        &mut || do_disconnect = true,
                        replay_loaded,
                        &mut |p| replay_path = Some(p.to_path_buf()),
                        &mut || do_stop_replay = true,
                    );

                    self.cfg = cfg;
                    if do_connect       { self.connect(); }
                    if do_disconnect    { self.disconnect(); }
                    if let Some(p) = replay_path { self.load_replay(&p); }
                    if do_stop_replay   { self.stop_replay(); }
                }
            }
        });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.disconnect();
        self.fota.shutdown();
    }
}

// ── Replay bar UI ─────────────────────────────────────────────────────────────

fn replay_bar(ui: &mut egui::Ui, replay: &mut ReplayController) {
    ui.add_space(4.0);

    // Transport buttons + speed
    ui.horizontal(|ui| {
        if ui.button("|<").on_hover_text("Go to start").clicked() {
            replay.reset();
        }
        if ui.button("<<").on_hover_text("Back 10 s").clicked() {
            replay.seek_relative(-10.0);
        }
        let pp_label = if replay.playing { "||" } else { ">" };
        if ui.button(pp_label).on_hover_text(if replay.playing { "Pause" } else { "Play" }).clicked() {
            replay.playing = !replay.playing;
        }
        if ui.button(">>").on_hover_text("Forward 10 s").clicked() {
            replay.seek_relative(10.0);
        }
        if ui.button(">|").on_hover_text("Go to end").clicked() {
            replay.seek(replay.end_ts);
        }

        ui.separator();

        // Speed selector — format without x first so trim works, then append
        let speed_label = format_speed(replay.speed);
        egui::ComboBox::from_id_salt("replay_speed")
            .selected_text(&speed_label)
            .width(60.0)
            .show_ui(ui, |ui| {
                for &s in SPEEDS {
                    ui.selectable_value(&mut replay.speed, s, format_speed(s));
                }
            });
    });

    // Time slider
    ui.horizontal(|ui| {
        ui.label(replay.format_start());

        let total = replay.duration_secs();
        let mut pos = replay.current_ts;
        let slider = egui::Slider::new(&mut pos, replay.start_ts..=replay.end_ts)
            .show_value(false)
            .trailing_fill(true);

        if ui.add(slider).changed() {
            replay.seek(pos);
        }

        ui.label(replay.format_end());
        ui.separator();
        ui.label(format!("  {}  /  {:.0}s", replay.format_current(), total));
    });
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn format_speed(s: f32) -> String {
    let raw = format!("{s:.2}");
    let trimmed = raw.trim_end_matches('0').trim_end_matches('.');
    format!("{trimmed}x")
}

fn resolve_yaml(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        crate::config::exe_dir().join(path)
    }
}
