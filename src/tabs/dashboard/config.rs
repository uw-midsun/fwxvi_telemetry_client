use serde::{Deserialize, Serialize};
use std::path::Path;

/// Total horizontal units in one flow row. Each panel spans some of these units
/// according to its `width`; panels wrap to the next row when the running total
/// would exceed this. See `PanelWidth::span`.
pub const ROW_UNITS: usize = 6;

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
    /// Generic label / value / bar table with an arbitrary number of rows.
    Meters,
    /// Combined throttle + brake pedal bars (brake tinted by regen state).
    Pedals,
    /// 2-D value grid: `rows` (each a labeled list of cells) × `columns` headers.
    /// Used for compact recaps like voltage/current per power rail.
    Matrix,
}

/// One cell of a `Matrix` panel: the signal to read plus an optional unit shown
/// after the value. Cells map positionally to the panel's `columns`.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MatrixCell {
    pub signal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit:   Option<String>,
}

/// One row of a `Matrix` panel: a row label plus one cell per column.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MatrixRow {
    pub label: String,
    pub cells: Vec<MatrixCell>,
}

/// Relative width of a panel in the flowing layout. Panels no longer carry
/// absolute grid coordinates — they flow in JSON order and wrap by width.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum PanelWidth {
    Small,
    #[default]
    Medium,
    Wide,
    Full,
}

impl PanelWidth {
    /// How many of `ROW_UNITS` this panel occupies.
    pub fn span(self) -> usize {
        match self {
            PanelWidth::Small  => 2,
            PanelWidth::Medium => 3,
            PanelWidth::Wide   => 4,
            PanelWidth::Full   => ROW_UNITS,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct GaugeConfig {
    pub min:  f64,
    pub max:  f64,
    pub unit: String,
}

/// One row of a `Meters` panel: a signal plus its own display range and unit.
/// `label` overrides the derived signal name; range fields fall back to the
/// panel-level `gauge` config (then 0..100) when omitted.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FieldConfig {
    pub signal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label:  Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min:    Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max:    Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit:   Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PanelConfig {
    pub id:         String,
    pub title:      String,
    pub chart_type: ChartType,
    #[serde(default)]
    pub signals:    Vec<String>,  // "message_name.signal_name"
    /// Per-signal line colors for `Line` panels, matched positionally to
    /// `signals`. Each is a name (red, orange, blue, green, …) or "#RRGGBB".
    /// Omit — or leave an entry out — to keep the default auto-assigned color.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub colors:     Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gauge:      Option<GaugeConfig>,
    /// Predefined y-axis range `[min, max]` for `Line` panels. The axis always
    /// shows at least this span; it grows (never shrinks) to fit data that goes
    /// out of range. Omit for fully auto-scaled bounds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y_range:    Option<[f64; 2]>,
    /// Per-row config for `Meters` panels (ignored by other chart types).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields:     Vec<FieldConfig>,
    /// Column headers for a `Matrix` panel (ignored by other chart types).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub columns:    Vec<String>,
    /// Row definitions for a `Matrix` panel (ignored by other chart types).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rows:       Vec<MatrixRow>,
    /// Relative width; panels flow in order and wrap by this. Defaults to medium.
    #[serde(default)]
    pub width:      PanelWidth,
}

impl PanelConfig {
    /// Split "message_name.signal_name" on the first dot.
    pub fn signal_parts(s: &str) -> Option<(&str, &str)> {
        s.find('.').map(|i| (&s[..i], &s[i + 1..]))
    }

    /// Signal keys referenced by this panel, whether declared via `signals`
    /// or the richer `fields` list (Meters panels use the latter).
    pub fn all_signals(&self) -> impl Iterator<Item = &str> {
        self.signals.iter().map(String::as_str)
            .chain(self.fields.iter().map(|f| f.signal.as_str()))
            .chain(self.rows.iter().flat_map(|r| r.cells.iter().map(|c| c.signal.as_str())))
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

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
}
