use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct SignalDef {
    pub name:      String,
    pub start_bit: u32,
    pub length:    u32,
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

/// Extract a single signal value from a little-endian payload integer.
pub fn extract_signal(payload: u64, start_bit: u32, length: u32) -> i64 {
    let mask = if length >= 64 { u64::MAX } else { (1u64 << length) - 1 };
    ((payload >> start_bit) & mask) as i64
}
