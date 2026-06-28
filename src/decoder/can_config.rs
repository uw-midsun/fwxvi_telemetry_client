use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct SignalDef {
    pub name:      String,
    pub start_bit: u32,
    pub length:    u32,
    #[serde(default)]
    pub scale:     Option<f64>,
    #[serde(default)]
    pub signed:    bool,
    #[serde(default, rename = "enum")]
    pub enum_map:  Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CanMessage {
    pub id:      u32,
    pub name:    String,
    #[allow(dead_code)]
    pub dlc:     u8,
    pub signals: Vec<SignalDef>,
}

#[derive(Debug, Deserialize)]
struct GlobalCanYaml {
    messages: Vec<CanMessage>,
}

pub fn load(path: &Path) -> Result<Vec<CanMessage>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("Cannot read CAN config: {}", path.display()))?;
    let parsed: GlobalCanYaml = serde_yaml::from_str(&text)
        .with_context(|| format!("Invalid YAML in {}", path.display()))?;
    Ok(parsed.messages)
}

/// Build a lookup from (message_name, signal_name) to enum label map.
/// Used by the UI to display string labels for enum-typed signals.
pub type EnumLookup = HashMap<(String, String), HashMap<String, String>>;

pub fn build_enum_lookup(messages: &[CanMessage]) -> EnumLookup {
    let mut lookup = EnumLookup::new();
    for msg in messages {
        for sig in &msg.signals {
            if let Some(map) = &sig.enum_map {
                lookup.insert((msg.name.clone(), sig.name.clone()), map.clone());
            }
        }
    }
    lookup
}

/// Extract a signal value from a little-endian payload.
/// If `signed` is true, applies two's-complement sign extension to `length` bits.
pub fn extract_signal(payload: u64, start_bit: u32, length: u32, signed: bool) -> i64 {
    let mask = if length >= 64 { u64::MAX } else { (1u64 << length) - 1 };
    let raw = ((payload >> start_bit) & mask) as i64;
    if signed && length > 0 && length < 64 {
        let shift = 64 - length;
        (raw << shift) >> shift
    } else {
        raw
    }
}
