use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub com_port:         String,
    pub baud_rate:        u32,
    pub can_yaml_path:    PathBuf,
    #[serde(default)]
    pub replay_file_path: Option<PathBuf>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            com_port:         "COM9".to_string(),
            baud_rate:        230_400,
            can_yaml_path:    PathBuf::from("global_can.yaml"),
            replay_file_path: None,
        }
    }
}

impl AppConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&text)?)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self)?;
        std::fs::write(path, text)?;
        Ok(())
    }

    /// Load from `settings.toml` next to the exe, or return defaults if missing.
    pub fn load_or_default() -> (Self, PathBuf) {
        let settings_path = exe_dir().join("settings.toml");
        let cfg = Self::load(&settings_path).unwrap_or_default();
        (cfg, settings_path)
    }
}

pub fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn db_path() -> PathBuf {
    exe_dir().join("data").join("decoded_data.sqlite")
}
