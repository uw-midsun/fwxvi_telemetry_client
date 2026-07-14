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
    /// "float" marks firmware float scaling: a float in [min, max] quantized into
    /// the signal's bits (autogen `type: float` or a postprocess `float:` rule).
    #[serde(default, rename = "type")]
    pub sig_type:  Option<String>,
    #[serde(default)]
    pub min:       Option<f64>,
    #[serde(default)]
    pub max:       Option<f64>,
}

impl SignalDef {
    /// The [min, max] range when this signal uses firmware float scaling.
    pub fn float_range(&self) -> Option<(f64, f64)> {
        if self.sig_type.as_deref() == Some("float") {
            Some((self.min?, self.max?))
        } else {
            None
        }
    }
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

/// Invert the firmware float quantization, mirroring the autogen getter exactly:
///
///   raw == 0            -> min - 1.0   (underflow sentinel: tx value was below min)
///   raw == 2^length - 1 -> max + 1.0   (overflow sentinel:  tx value was above max)
///   otherwise           -> (raw - 1) / (2^length - 3) * (max - min) + min
///
/// The raw code is always unsigned — the encoder maps [min, max] onto [1, 2^length - 2].
pub fn decode_float(raw: u64, length: u32, min: f64, max: f64) -> f64 {
    let max_code = if length >= 64 { u64::MAX } else { (1u64 << length) - 1 };
    if raw == 0 {
        min - 1.0
    } else if raw >= max_code {
        max + 1.0
    } else {
        (raw - 1) as f64 / (max_code - 2) as f64 * (max - min) + min
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Mirror of the firmware setter: quantize a float into [1, 2^len - 2] with
    // 0 / 2^len - 1 as under/overflow sentinels.
    fn encode_float(val: f64, length: u32, min: f64, max: f64) -> u64 {
        let max_code = (1u64 << length) - 1;
        if val < min {
            0
        } else if val > max {
            max_code
        } else {
            ((val - min) / (max - min) * (max_code - 2) as f64 + 1.0) as u64
        }
    }

    #[test]
    fn float_sentinels_decode_like_firmware_getter() {
        // battery_stats_A.pack_voltage_v: float, min 0, max 200, 32 bits
        assert_eq!(decode_float(0, 32, 0.0, 200.0), -1.0);           // underflow -> min - 1
        assert_eq!(decode_float(u32::MAX as u64, 32, 0.0, 200.0), 201.0); // overflow -> max + 1
    }

    #[test]
    fn float_endpoints_round_trip() {
        // min encodes to code 1, max to code 2^len - 2
        for &(len, min, max) in &[(8u32, 20000.0, 50000.0), (16, 0.0, 100.0), (32, -200.0, 200.0)] {
            let max_code = (1u64 << len) - 1;
            assert_eq!(encode_float(min, len, min, max), 1);
            assert_eq!(encode_float(max, len, min, max), max_code - 1);
            assert_eq!(decode_float(1, len, min, max), min);
            assert_eq!(decode_float(max_code - 1, len, min, max), max);
        }
    }

    #[test]
    fn float_midpoint_round_trips_within_quantization_step() {
        // battery_stats_B.pack_current_a: float, min -200, max 200, 32 bits
        let (len, min, max) = (32u32, -200.0, 200.0);
        let val = 73.2;
        let raw = encode_float(val, len, min, max);
        let back = decode_float(raw, len, min, max);
        let step = (max - min) / ((1u64 << len) - 3) as f64;
        assert!((back - val).abs() <= step, "err {} > step {step}", (back - val).abs());
    }

    #[test]
    fn float_8bit_code_range_is_coarse_but_exact_at_ends() {
        // battery_stats_B.max_cell_voltage: float, min 20000, max 50000, 8 bits
        let (len, min, max) = (8u32, 20000.0, 50000.0);
        assert_eq!(decode_float(0, len, min, max), 19999.0);  // underflow
        assert_eq!(decode_float(255, len, min, max), 50001.0); // overflow
        assert_eq!(decode_float(1, len, min, max), 20000.0);
        assert_eq!(decode_float(254, len, min, max), 50000.0);
    }

    #[test]
    fn signal_def_float_range_requires_type_and_bounds() {
        let yaml = "
name: pack_voltage_v
start_bit: 0
length: 32
type: float
min: 0
max: 200
";
        let sig: SignalDef = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(sig.float_range(), Some((0.0, 200.0)));

        let plain: SignalDef =
            serde_yaml::from_str("{name: x, start_bit: 0, length: 16}").unwrap();
        assert_eq!(plain.float_range(), None);
    }
}
