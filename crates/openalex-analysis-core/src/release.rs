use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{CoreError, Result};

/// Result of a local-only update check.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpdateCheck {
    pub current_version: String,
    pub latest_version: String,
    pub has_update: bool,
}

/// Reads the `version` field from a Tauri `tauri.conf.json` file.
pub fn read_version_from_tauri_conf(path: &Path) -> Result<String> {
    let raw = std::fs::read_to_string(path)?;
    let value: Value = serde_json::from_str(&raw)?;
    value
        .get("version")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| CoreError::UpdateCheckFailed(format!("missing version in {path:?}")))
}

/// Reads the `latest_version` field from a local update descriptor.
pub fn read_latest_version_from_json(path: &Path) -> Result<String> {
    let raw = std::fs::read_to_string(path)?;
    let value: Value = serde_json::from_str(&raw)?;
    value
        .get("latest_version")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| CoreError::UpdateCheckFailed(format!("missing latest_version in {path:?}")))
}

/// Compares two `major.minor.patch[-prerelease]` style version strings.
///
/// Pre-releases are always considered older than their release counterpart.
/// Build metadata after `+` is ignored. Returns `Ok(true)` when `latest` is
/// newer than `current`, `Ok(false)` when equal or older, and `Err` when a
/// version string cannot be parsed.
pub fn is_newer_version(current: &str, latest: &str) -> Result<bool> {
    let current = parse_semver(current)?;
    let latest = parse_semver(latest)?;
    Ok(latest > current)
}

/// Checks for an update using the local Tauri config and update descriptor.
pub fn check_update(tauri_conf_path: &Path, update_json_path: &Path) -> Result<UpdateCheck> {
    let current_version = read_version_from_tauri_conf(tauri_conf_path)?;
    let latest_version = read_latest_version_from_json(update_json_path)?;
    let has_update = is_newer_version(&current_version, &latest_version)?;
    Ok(UpdateCheck {
        current_version,
        latest_version,
        has_update,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SemVer {
    major: u64,
    minor: u64,
    patch: u64,
    pre: Vec<PrereleasePart>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum PrereleasePart {
    Numeric(u64),
    Alphanumeric(String),
}

fn parse_semver(version: &str) -> Result<SemVer> {
    // Strip optional build metadata.
    let version = version.split('+').next().unwrap_or(version);
    let (core, pre) = version
        .split_once('-')
        .map(|(c, p)| (c, Some(p)))
        .unwrap_or((version, None));

    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() != 3 {
        return Err(CoreError::UpdateCheckFailed(format!(
            "expected major.minor.patch, got {version}"
        )));
    }

    let major = parts[0]
        .parse::<u64>()
        .map_err(|_| CoreError::UpdateCheckFailed(format!("invalid major version in {version}")))?;
    let minor = parts[1]
        .parse::<u64>()
        .map_err(|_| CoreError::UpdateCheckFailed(format!("invalid minor version in {version}")))?;
    let patch = parts[2]
        .parse::<u64>()
        .map_err(|_| CoreError::UpdateCheckFailed(format!("invalid patch version in {version}")))?;

    let pre = pre.map(parse_prerelease).transpose()?.unwrap_or_default();

    Ok(SemVer {
        major,
        minor,
        patch,
        pre,
    })
}

fn parse_prerelease(pre: &str) -> Result<Vec<PrereleasePart>> {
    if pre.is_empty() {
        return Ok(Vec::new());
    }
    pre.split('.')
        .map(|part| {
            if part.is_empty() {
                return Err(CoreError::UpdateCheckFailed(
                    "empty prerelease segment".to_string(),
                ));
            }
            if part.chars().all(|c| c.is_ascii_digit()) {
                Ok(PrereleasePart::Numeric(part.parse::<u64>().map_err(
                    |_| CoreError::UpdateCheckFailed(format!("invalid prerelease number {part}")),
                )?))
            } else {
                Ok(PrereleasePart::Alphanumeric(part.to_string()))
            }
        })
        .collect()
}
