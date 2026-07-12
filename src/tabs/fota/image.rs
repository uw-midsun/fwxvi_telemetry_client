//! App image preparation: .elf to raw .bin via objcopy, plus image metadata
//!
//! Ported from ms-bootloader `client/src/image.rs`.
//!
//! - **Author:** Midnight Sun Team #24

use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use super::protocol::{crc32, FirmwareMetadata};

/// Build manifest emitted next to a .bin by the ms-bootloader `bl_manifest` tool. It records the
/// artifact identity (the build that produced the binary) so the GUI can pre-fill the flash
/// metadata instead of the user retyping it, keeping the identity bound to the actual image.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Manifest {
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub board: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub git_hash: String,
}

/// Read a manifest from an explicit path when it exists and parses.
pub fn read_manifest(manifest: &Path) -> Option<Manifest> {
    let text = std::fs::read_to_string(manifest).ok()?;
    serde_json::from_str(&text).ok()
}

/// The conventional sibling manifest path for an image (`<stem>.manifest.json`).
pub fn sibling_manifest_path(image: &Path) -> std::path::PathBuf {
    image.with_extension("manifest.json")
}

/// Load a flashable image. A .bin is read as is, anything else is treated as an ELF
/// and converted with arm-none-eabi-objcopy (the same tool SCons links with).
pub fn load_image(path: &Path) -> Result<Vec<u8>> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if ext.eq_ignore_ascii_case("bin") {
        return std::fs::read(path).with_context(|| format!("reading {}", path.display()));
    }

    let tmp = std::env::temp_dir().join(format!("bl_image_{}.bin", std::process::id()));
    let status = Command::new("arm-none-eabi-objcopy")
        .args(["-O", "binary", "-R", ".eh_frame"])
        .arg(path)
        .arg(&tmp)
        .status()
        .context("running arm-none-eabi-objcopy (is the ARM toolchain on PATH?)")?;
    if !status.success() {
        bail!("objcopy failed to convert {}", path.display());
    }
    let bytes = std::fs::read(&tmp).with_context(|| format!("reading {}", tmp.display()))?;
    let _ = std::fs::remove_file(&tmp);
    Ok(bytes)
}

/// Build the metadata datagram payload for an image. The CRC is taken over the exact
/// image bytes, matching the firmware which CRCs over image_size (its 8 byte tail pad
/// is excluded).
pub fn build_metadata(
    image: &[u8],
    project: &str,
    version: (u8, u8),
    git: &str,
) -> FirmwareMetadata {
    FirmwareMetadata {
        image_size: image.len() as u32,
        image_crc32: crc32(image),
        version_major: version.0,
        version_minor: version.1,
        project_name: project.to_string(),
        git_hash: git.to_string(),
    }
}
