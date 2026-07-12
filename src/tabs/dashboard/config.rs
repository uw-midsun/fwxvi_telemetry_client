use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum ChartType {
    Line,
    Gauge,
    Histogram,
    Scatter,
    Numeric,
    Bar,
    Status,
    Leds,
    Bars,
    Stat,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct GaugeConfig {
    pub min:  f64,
    pub max:  f64,
    pub unit: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct GridPos {
    pub col:      usize,
    pub row:      usize,
    pub col_span: usize,
    pub row_span: usize,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PanelConfig {
    pub id:         String,
    pub title:      String,
    pub chart_type: ChartType,
    pub signals:    Vec<String>,  // "message_name.signal_name"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gauge:      Option<GaugeConfig>,
    pub grid:       GridPos,
}

impl PanelConfig {
    /// Split "message_name.signal_name" on the first dot.
    pub fn signal_parts(s: &str) -> Option<(&str, &str)> {
        s.find('.').map(|i| (&s[..i], &s[i + 1..]))
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DashboardSetup {
    pub name:           String,
    #[serde(default = "default_true")]
    pub live:           bool,
    #[serde(default = "default_window")]
    pub window_seconds: f64,
    #[serde(default)]
    pub panels:         Vec<PanelConfig>,
}

fn default_true()   -> bool { true }
fn default_window() -> f64  { 30.0 }

impl Default for DashboardSetup {
    fn default() -> Self {
        Self { name: "default".into(), live: true, window_seconds: 30.0, panels: vec![] }
    }
}

impl DashboardSetup {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let ext  = path.extension().and_then(|e| e.to_str()).unwrap_or("json");
        if matches!(ext, "yaml" | "yml") {
            Ok(serde_yaml::from_str(&text)?)
        } else {
            Ok(serde_json::from_str(&text)?)
        }
    }

    #[allow(dead_code)]
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
}
