use crate::config::AppConfig;
use std::path::PathBuf;

pub struct SettingsTab {
    // Live section
    pub port_list:     Vec<String>,
    pub selected_port: String,
    pub baud_str:      String,
    last_port_scan:    std::time::Instant,

    // Replay section
    pub replay_path_str: String,

    pub status_msg: String,
}

impl SettingsTab {
    pub fn new(cfg: &AppConfig) -> Self {
        let mut s = Self {
            port_list:       vec![],
            selected_port:   cfg.com_port.clone(),
            baud_str:        cfg.baud_rate.to_string(),
            last_port_scan:  std::time::Instant::now() - std::time::Duration::from_secs(10),
            replay_path_str: cfg.replay_file_path
                .as_deref()
                .and_then(|p| p.to_str())
                .unwrap_or("")
                .to_string(),
            status_msg: String::new(),
        };
        s.scan_ports();
        s
    }

    pub fn scan_ports(&mut self) {
        self.port_list = serialport::available_ports()
            .unwrap_or_default()
            .into_iter()
            .map(|p| p.port_name)
            .collect();
        self.last_port_scan = std::time::Instant::now();
    }

    pub fn apply_to_config(&self, cfg: &mut AppConfig) {
        cfg.com_port  = self.selected_port.clone();
        if let Ok(b) = self.baud_str.trim().parse::<u32>() {
            cfg.baud_rate = b;
        }
        let rp = PathBuf::from(self.replay_path_str.trim());
        cfg.replay_file_path = if rp.as_os_str().is_empty() { None } else { Some(rp) };
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        cfg: &mut AppConfig,
        settings_path: &std::path::Path,
        // Live callbacks
        connected: bool,
        on_connect: &mut dyn FnMut(&AppConfig),
        on_disconnect: &mut dyn FnMut(),
        // Replay callbacks
        replay_loaded: bool,
        on_load_replay: &mut dyn FnMut(&std::path::Path),
        on_stop_replay: &mut dyn FnMut(),
    ) {
        if self.last_port_scan.elapsed().as_secs() >= 3 {
            self.scan_ports();
        }

        // ── Live Recording ────────────────────────────────────────────────────
        ui.group(|ui| {
            ui.strong("Live Recording");
            ui.add_space(4.0);

            egui::Grid::new("live_grid")
                .num_columns(2)
                .spacing([16.0, 6.0])
                .show(ui, |ui| {
                    ui.label("COM Port:");
                    egui::ComboBox::from_id_salt("com_port")
                        .selected_text(&self.selected_port)
                        .show_ui(ui, |ui| {
                            for port in &self.port_list {
                                ui.selectable_value(&mut self.selected_port, port.clone(), port);
                            }
                            ui.text_edit_singleline(&mut self.selected_port);
                        });
                    ui.end_row();

                    ui.label("Baud Rate:");
                    ui.text_edit_singleline(&mut self.baud_str);
                    ui.end_row();
                });

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if connected {
                    if ui.button("Disconnect").clicked() {
                        on_disconnect();
                    }
                    ui.colored_label(egui::Color32::from_rgb(80, 200, 80), "● Recording");
                } else if ui.button("Connect").clicked() {
                    self.apply_to_config(cfg);
                    let _ = cfg.save(settings_path);
                    on_connect(cfg);
                }
            });
        });

        ui.add_space(8.0);

        // ── Replay ────────────────────────────────────────────────────────────
        ui.group(|ui| {
            ui.strong("Replay");
            ui.add_space(4.0);

            egui::Grid::new("replay_grid")
                .num_columns(2)
                .spacing([16.0, 6.0])
                .show(ui, |ui| {
                    ui.label("File:");
                    ui.horizontal(|ui| {
                        ui.text_edit_singleline(&mut self.replay_path_str);
                        if ui.button("Browse…").clicked() {
                            let mut dialog = rfd::FileDialog::new()
                                .add_filter("SQLite", &["sqlite", "db"])
                                .set_title("Select decoded_data.sqlite");
                            if let Ok(cwd) = std::env::current_dir() {
                                dialog = dialog.set_directory(cwd);
                            }
                            if let Some(path) = dialog.pick_file() {
                                self.replay_path_str = path.to_string_lossy().into_owned();
                            }
                        }
                    });
                    ui.end_row();
                });

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let path = PathBuf::from(self.replay_path_str.trim());
                let has_path = !self.replay_path_str.trim().is_empty();

                if replay_loaded {
                    if ui.button("Stop Replay").clicked() {
                        on_stop_replay();
                    }
                    ui.colored_label(egui::Color32::from_rgb(200, 160, 60), "● Replay active");
                } else {
                    let btn = ui.add_enabled(has_path, egui::Button::new("Load Replay"));
                    if btn.clicked() {
                        self.apply_to_config(cfg);
                        let _ = cfg.save(settings_path);
                        on_load_replay(&path);
                    }
                }
            });
        });

        ui.add_space(8.0);

        // ── YAML path ─────────────────────────────────────────────────────────
        ui.group(|ui| {
            ui.strong("CAN Definitions");
            ui.add_space(4.0);
            egui::Grid::new("yaml_grid")
                .num_columns(2)
                .spacing([16.0, 6.0])
                .show(ui, |ui| {
                    ui.label("global_can.yaml:");
                    ui.horizontal(|ui| {
                        let mut yaml_str = cfg.can_yaml_path.to_string_lossy().into_owned();
                        if ui.text_edit_singleline(&mut yaml_str).changed() {
                            cfg.can_yaml_path = PathBuf::from(&yaml_str);
                        }
                        if ui.button("Browse…").clicked() {
                            let mut dialog = rfd::FileDialog::new()
                                .add_filter("YAML", &["yaml", "yml"]);
                            if let Ok(cwd) = std::env::current_dir() {
                                dialog = dialog.set_directory(cwd);
                            }
                            if let Some(p) = dialog.pick_file() {
                                cfg.can_yaml_path = p;
                            }
                        }
                    });
                    ui.end_row();
                });

            ui.add_space(4.0);
            if ui.button("Save Settings").clicked() {
                match cfg.save(settings_path) {
                    Ok(_)  => self.status_msg = "Settings saved.".to_string(),
                    Err(e) => self.status_msg = format!("Save failed: {e}"),
                }
            }
        });

        if !self.status_msg.is_empty() {
            ui.add_space(6.0);
            ui.label(&self.status_msg);
        }
    }
}
