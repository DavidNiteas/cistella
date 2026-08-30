use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Result, import::source_record::SourceRecord};

use super::identifier::Identifier;

/// On-disk cache entry for a remote metadata response.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedEntry {
    cached_at: DateTime<Utc>,
    source_record: SourceRecord,
    raw_response: serde_json::Value,
}

/// Returns the cache file path for a resolver + identifier pair.
pub fn cache_file_path(vault_path: &Path, resolver_name: &str, identifier: &Identifier) -> PathBuf {
    let hash = sha256_hex(identifier.canonical());
    vault_path
        .join("cache")
        .join("remote_metadata")
        .join(resolver_name)
        .join(format!("{hash}.json"))
}

/// Loads a cached `SourceRecord` if it exists and has not expired.
pub fn load_cached(
    vault_path: &Path,
    resolver_name: &str,
    identifier: &Identifier,
    max_age: Duration,
) -> Result<Option<SourceRecord>> {
    let path = cache_file_path(vault_path, resolver_name, identifier);
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = fs::read(&path)?;
    let entry: CachedEntry = serde_json::from_slice(&bytes)?;
    let age = Utc::now().signed_duration_since(entry.cached_at);
    if age.num_seconds() < 0 || age.to_std().unwrap_or(Duration::MAX) > max_age {
        return Ok(None);
    }
    Ok(Some(entry.source_record))
}

/// Saves a cache entry atomically, overwriting any existing file.
pub fn save_cached(
    vault_path: &Path,
    resolver_name: &str,
    identifier: &Identifier,
    source_record: &SourceRecord,
    raw_response: &serde_json::Value,
) -> Result<()> {
    let path = cache_file_path(vault_path, resolver_name, identifier);
    let parent = path
        .parent()
        .ok_or_else(|| crate::CoreError::InvalidUserDataPath(path.display().to_string()))?;
    fs::create_dir_all(parent)?;
    let entry = CachedEntry {
        cached_at: Utc::now(),
        source_record: source_record.clone(),
        raw_response: raw_response.clone(),
    };
    let payload = serde_json::to_vec_pretty(&entry)?;
    crate::literature::atomic_replace_named(&path, &payload, ".remote_metadata_cache")
}

fn sha256_hex(input: impl AsRef<[u8]>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input);
    format!("{:x}", hasher.finalize())
}
