use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    error::{CoreError, Result},
    literature::atomic_replace,
};

const PORTABLE_DIR_NAME: &str = "cistella-portable";
const APP_DIR_NAME: &str = "cistella";
const CONFIG_DIR_NAME: &str = "config";
const CACHE_DIR_NAME: &str = "cache";
const RECENT_VAULTS_FILE_NAME: &str = "recent_vaults.json";
const APP_CONFIG_FILE_NAME: &str = "app.json";
const MIGRATION_VAULTS_DIR_NAME: &str = "vaults";
const MAX_MIGRATION_SUFFIX: u32 = 10_000;

/// Runtime application data directories. Detects and switches between portable
/// mode (a `cistella-portable/` directory next to the executable) and installed
/// mode (the system standard application data directory).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppDirectories {
    is_portable: bool,
    portable_root: Option<PathBuf>,
    installed_data_dir: Option<PathBuf>,
    config_dir: PathBuf,
    recent_vaults_path: PathBuf,
    cache_dir: PathBuf,
}

impl AppDirectories {
    /// Detects portable mode by looking for a `cistella-portable/` directory
    /// inside `exe_dir`. When found, all application data lives under that
    /// directory; otherwise the system standard local data directory is used.
    pub fn from_exe_dir(exe_dir: impl AsRef<Path>) -> Result<Self> {
        let exe_dir = exe_dir.as_ref();
        let portable_root = exe_dir.join(PORTABLE_DIR_NAME);
        let is_portable = portable_root.is_dir();

        if is_portable {
            let config_dir = portable_root.join(CONFIG_DIR_NAME);
            let cache_dir = portable_root.join(CACHE_DIR_NAME);
            let recent_vaults_path = portable_root.join(RECENT_VAULTS_FILE_NAME);
            create_app_dirs(&config_dir, &cache_dir, &portable_root)?;
            Ok(Self {
                is_portable: true,
                portable_root: Some(portable_root),
                installed_data_dir: None,
                config_dir,
                recent_vaults_path,
                cache_dir,
            })
        } else {
            let installed_data_dir = local_app_data_dir()?;
            let config_dir = installed_data_dir.join(CONFIG_DIR_NAME);
            let cache_dir = installed_data_dir.join(CACHE_DIR_NAME);
            let recent_vaults_path = installed_data_dir.join(RECENT_VAULTS_FILE_NAME);
            create_app_dirs(&config_dir, &cache_dir, &installed_data_dir)?;
            Ok(Self {
                is_portable: false,
                portable_root: None,
                installed_data_dir: Some(installed_data_dir),
                config_dir,
                recent_vaults_path,
                cache_dir,
            })
        }
    }

    /// Returns `true` when running in portable mode.
    pub fn is_portable_mode(&self) -> bool {
        self.is_portable
    }

    /// Returns the portable root directory when in portable mode, otherwise
    /// `None`.
    pub fn portable_root(&self) -> Option<&Path> {
        self.portable_root.as_deref()
    }

    /// Returns the directory that holds `app.json` and other configuration.
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    /// Returns the path to `recent_vaults.json`.
    pub fn recent_vaults_path(&self) -> PathBuf {
        self.recent_vaults_path.clone()
    }

    /// Returns the cache directory path.
    pub fn cache_dir(&self) -> PathBuf {
        self.cache_dir.clone()
    }

    /// Returns the path to `config/app.json`.
    pub fn app_config_path(&self) -> PathBuf {
        self.config_dir.join(APP_CONFIG_FILE_NAME)
    }

    /// Saves the recent vault list to the canonical location for this
    /// application directory layout. In portable mode, vault paths inside the
    /// portable root are automatically converted to relative paths before
    /// persistence; in installed mode paths are stored as-is.
    pub fn save_recent_vaults(&self, vaults: &[RecentVault]) -> Result<()> {
        let vaults = if self.is_portable_mode() {
            if let Some(ref portable_root) = self.portable_root {
                make_vault_paths_portable(portable_root, vaults)
            } else {
                vaults.to_vec()
            }
        } else {
            vaults.to_vec()
        };
        save_recent_vaults(&self.recent_vaults_path, &vaults)
    }

    /// Test-only constructor that builds `AppDirectories` from explicit
    /// directories without touching the executable path or system folders.
    #[doc(hidden)]
    pub fn from_test_dirs(
        portable_root: Option<PathBuf>,
        installed_data_dir: Option<PathBuf>,
    ) -> Result<Self> {
        match (portable_root, installed_data_dir) {
            (Some(portable_root), None) => {
                let config_dir = portable_root.join(CONFIG_DIR_NAME);
                let cache_dir = portable_root.join(CACHE_DIR_NAME);
                let recent_vaults_path = portable_root.join(RECENT_VAULTS_FILE_NAME);
                create_app_dirs(&config_dir, &cache_dir, &portable_root)?;
                Ok(Self {
                    is_portable: true,
                    portable_root: Some(portable_root),
                    installed_data_dir: None,
                    config_dir,
                    recent_vaults_path,
                    cache_dir,
                })
            }
            (None, Some(installed_data_dir)) => {
                let config_dir = installed_data_dir.join(CONFIG_DIR_NAME);
                let cache_dir = installed_data_dir.join(CACHE_DIR_NAME);
                let recent_vaults_path = installed_data_dir.join(RECENT_VAULTS_FILE_NAME);
                create_app_dirs(&config_dir, &cache_dir, &installed_data_dir)?;
                Ok(Self {
                    is_portable: false,
                    portable_root: None,
                    installed_data_dir: Some(installed_data_dir),
                    recent_vaults_path,
                    config_dir,
                    cache_dir,
                })
            }
            _ => Err(CoreError::PortableModeDetectionFailed(
                "invalid test directories".to_string(),
            )),
        }
    }
}

fn create_app_dirs(config_dir: &Path, cache_dir: &Path, data_root: &Path) -> Result<()> {
    fs::create_dir_all(config_dir)?;
    fs::create_dir_all(cache_dir)?;
    fs::create_dir_all(data_root)?;
    Ok(())
}

fn local_app_data_dir() -> Result<PathBuf> {
    dirs::data_local_dir()
        .map(|dir| dir.join(APP_DIR_NAME))
        .ok_or_else(|| {
            CoreError::PortableModeDetectionFailed("local app data dir unavailable".to_string())
        })
}

/// A recently opened Vault entry persisted in `recent_vaults.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecentVault {
    pub path: String,
    pub name: String,
    pub opened_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct AppConfig {
    pub theme: Option<String>,
    pub window_size: Option<WindowSize>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct WindowSize {
    pub width: u32,
    pub height: u32,
}

/// Loads the recent vault list from `path`. Missing files are treated as an
/// empty list.
pub fn load_recent_vaults(path: impl AsRef<Path>) -> Result<Vec<RecentVault>> {
    let path = path.as_ref();
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes = fs::read(path)?;
    let wrapper: RecentVaultsFile = serde_json::from_slice(&bytes)
        .map_err(|e| CoreError::RecentVaultsParseFailed(e.to_string()))?;
    Ok(wrapper.vaults)
}

/// Saves the recent vault list to `path` atomically.
pub fn save_recent_vaults(path: impl AsRef<Path>, vaults: &[RecentVault]) -> Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let wrapper = RecentVaultsFile {
        vaults: vaults.to_vec(),
    };
    let payload = serde_json::to_vec_pretty(&wrapper)?;
    atomic_replace(path, &payload)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RecentVaultsFile {
    vaults: Vec<RecentVault>,
}

/// Loads application configuration from `path`. Missing files return the
/// default config.
pub fn load_app_config(path: impl AsRef<Path>) -> Result<AppConfig> {
    let path = path.as_ref();
    if !path.exists() {
        return Ok(AppConfig::default());
    }
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(|e| CoreError::AppConfigParseFailed(e.to_string()))
}

/// Saves application configuration to `path` atomically.
pub fn save_app_config(path: impl AsRef<Path>, config: &AppConfig) -> Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let payload = serde_json::to_vec_pretty(config)?;
    atomic_replace(path, &payload)
}

/// Stores `vaults` using paths relative to `portable_root` when applicable.
/// Vaults located inside `portable_root` are converted to relative paths;
/// outside paths are kept absolute.
pub fn make_vault_paths_portable(portable_root: &Path, vaults: &[RecentVault]) -> Vec<RecentVault> {
    vaults
        .iter()
        .map(|vault| {
            let path = Path::new(&vault.path);
            let stored_path = if path.is_absolute() {
                if let Ok(relative) = path.strip_prefix(portable_root) {
                    portable_relative_path(relative)
                } else {
                    vault.path.clone()
                }
            } else {
                vault.path.clone()
            };
            RecentVault {
                path: stored_path,
                ..vault.clone()
            }
        })
        .collect()
}

/// Resolves stored recent-vault paths against `portable_root`. Relative paths
/// become absolute paths rooted at `portable_root`; absolute paths pass
/// through unchanged.
pub fn resolve_recent_vault_paths(
    portable_root: &Path,
    vaults: &[RecentVault],
) -> Vec<RecentVault> {
    vaults
        .iter()
        .map(|vault| {
            let path = Path::new(&vault.path);
            let resolved = if path.is_absolute() {
                path.to_path_buf()
            } else {
                portable_root.join(path)
            };
            RecentVault {
                path: resolved.to_string_lossy().to_string(),
                ..vault.clone()
            }
        })
        .collect()
}

fn portable_relative_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Migrates the listed Vaults into the portable directory tree.
///
/// Each Vault is copied to `<portable-root>/vaults/<sanitized-name>/`. The
/// source Vault is never modified or removed. On success the returned
/// `RecentVault` entries use relative paths so the portable bundle remains
/// valid after being moved.
pub fn migrate_vaults_to_portable(
    app_dirs: &AppDirectories,
    vaults: &[RecentVault],
) -> Result<Vec<RecentVault>> {
    let portable_root = app_dirs
        .portable_root()
        .ok_or_else(|| CoreError::MigrationFailed("not running in portable mode".to_string()))?;
    let vaults_dir = portable_root.join(MIGRATION_VAULTS_DIR_NAME);
    fs::create_dir_all(&vaults_dir)?;

    let mut migrated = Vec::with_capacity(vaults.len());
    for vault in vaults {
        let source = Path::new(&vault.path);
        if !source.exists() {
            return Err(CoreError::MigrationFailed(format!(
                "source vault does not exist: {}",
                source.display()
            )));
        }
        let target_name = sanitize_dir_name(&vault.name).unwrap_or_else(|| {
            source
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "vault".to_string())
        });
        let target = unique_migration_target(&vaults_dir, &target_name, source)?;
        copy_dir_recursively(source, &target)?;
        let relative = target
            .strip_prefix(portable_root)
            .map(|p| portable_relative_path(p))
            .map_err(|_| {
                CoreError::MigrationFailed(format!(
                    "migrated vault is not under portable root: {}",
                    target.display()
                ))
            })?;
        migrated.push(RecentVault {
            path: relative,
            name: vault.name.clone(),
            opened_at: vault.opened_at.clone(),
        });
    }
    Ok(migrated)
}

/// Migrates the listed Vaults into the installed data directory tree.
///
/// Each Vault is copied to `<installed-data-dir>/vaults/<sanitized-name>/`.
/// The source Vault is preserved. The returned entries use absolute paths.
pub fn migrate_vaults_to_installed(
    app_dirs: &AppDirectories,
    vaults: &[RecentVault],
) -> Result<Vec<RecentVault>> {
    let installed_data_dir = app_dirs
        .installed_data_dir
        .as_deref()
        .ok_or_else(|| CoreError::MigrationFailed("not running in installed mode".to_string()))?;
    let vaults_dir = installed_data_dir.join(MIGRATION_VAULTS_DIR_NAME);
    fs::create_dir_all(&vaults_dir)?;

    let mut migrated = Vec::with_capacity(vaults.len());
    for vault in vaults {
        let source = Path::new(&vault.path);
        let source = if source.is_absolute() {
            source.to_path_buf()
        } else if let Some(portable_root) = app_dirs.portable_root() {
            portable_root.join(source)
        } else {
            return Err(CoreError::MigrationFailed(format!(
                "cannot resolve relative vault path without portable root: {}",
                vault.path
            )));
        };
        if !source.exists() {
            return Err(CoreError::MigrationFailed(format!(
                "source vault does not exist: {}",
                source.display()
            )));
        }
        let target_name = sanitize_dir_name(&vault.name).unwrap_or_else(|| {
            source
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "vault".to_string())
        });
        let target = unique_migration_target(&vaults_dir, &target_name, &source)?;
        copy_dir_recursively(&source, &target)?;
        migrated.push(RecentVault {
            path: target.to_string_lossy().to_string(),
            name: vault.name.clone(),
            opened_at: vault.opened_at.clone(),
        });
    }
    Ok(migrated)
}

fn sanitize_dir_name(name: &str) -> Option<String> {
    let sanitized: String = name
        .chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | ' ' => c,
            _ => '_',
        })
        .collect();
    let trimmed = sanitized.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn unique_migration_target(vaults_dir: &Path, base_name: &str, source: &Path) -> Result<PathBuf> {
    let mut target = vaults_dir.join(base_name);
    if !target.exists() {
        return Ok(target);
    }
    // If the target already exists and points at the same source, reuse it.
    let canonical_target = fs::canonicalize(&target).ok();
    let canonical_source = fs::canonicalize(source).ok();
    if let (Some(t), Some(s)) = (canonical_target, canonical_source) {
        if t == s {
            return Ok(target);
        }
    }
    for i in 2..=MAX_MIGRATION_SUFFIX {
        target = vaults_dir.join(format!("{}-{}", base_name, i));
        if !target.exists() {
            return Ok(target);
        }
    }
    Err(CoreError::MigrationFailed(format!(
        "could not find a unique migration target after {} attempts",
        MAX_MIGRATION_SUFFIX
    )))
}

fn copy_dir_recursively(source: &Path, target: &Path) -> Result<()> {
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            copy_dir_recursively(&source_path, &target_path)?;
        } else if metadata.is_file() {
            if let Some(parent) = target_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&source_path, &target_path)?;
        } else if metadata.file_type().is_symlink() {
            // Skip symlinks during migration to avoid escaping the vault.
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "{}-{}",
            prefix,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn detects_portable_mode_when_marker_directory_exists() {
        let exe_dir = temp_dir("cistella-portable-detect");
        fs::create_dir_all(&exe_dir).unwrap();
        fs::create_dir_all(exe_dir.join(PORTABLE_DIR_NAME)).unwrap();

        let dirs = AppDirectories::from_exe_dir(&exe_dir).unwrap();
        assert!(dirs.is_portable_mode());
        assert_eq!(
            dirs.portable_root(),
            Some(exe_dir.join(PORTABLE_DIR_NAME).as_path())
        );

        fs::remove_dir_all(&exe_dir).unwrap();
    }

    #[test]
    fn detects_installed_mode_when_marker_directory_missing() {
        let exe_dir = temp_dir("cistella-installed-detect");
        fs::create_dir_all(&exe_dir).unwrap();

        let dirs = AppDirectories::from_exe_dir(&exe_dir).unwrap();
        assert!(!dirs.is_portable_mode());
        assert!(dirs.portable_root().is_none());

        fs::remove_dir_all(&exe_dir).unwrap();
    }

    #[test]
    fn saves_and_loads_recent_vaults_roundtrip() {
        let path = temp_dir("cistella-recent-vaults").join("recent_vaults.json");
        let vaults = vec![RecentVault {
            path: "/tmp/vault".to_string(),
            name: "Test".to_string(),
            opened_at: Some("2026-08-30T00:00:00Z".to_string()),
        }];
        save_recent_vaults(&path, &vaults).unwrap();
        let loaded = load_recent_vaults(&path).unwrap();
        assert_eq!(loaded, vaults);
    }

    #[test]
    fn saves_and_loads_app_config_roundtrip() {
        let path = temp_dir("cistella-app-config").join("app.json");
        let config = AppConfig {
            theme: Some("dark".to_string()),
            window_size: Some(WindowSize {
                width: 1280,
                height: 720,
            }),
        };
        save_app_config(&path, &config).unwrap();
        let loaded = load_app_config(&path).unwrap();
        assert_eq!(loaded, config);
    }

    #[test]
    fn resolves_relative_paths_against_portable_root() {
        let portable_root = PathBuf::from("D:/app/cistella-portable");
        let vaults = vec![RecentVault {
            path: "vaults/my-library".to_string(),
            name: "My Library".to_string(),
            opened_at: None,
        }];
        let resolved = resolve_recent_vault_paths(&portable_root, &vaults);
        assert_eq!(
            resolved[0].path,
            portable_root.join("vaults/my-library").to_string_lossy()
        );
    }

    #[test]
    fn keeps_absolute_paths_untouched() {
        let portable_root = PathBuf::from("D:/app/cistella-portable");
        let vaults = vec![RecentVault {
            path: "C:/external/vault".to_string(),
            name: "External".to_string(),
            opened_at: None,
        }];
        let resolved = resolve_recent_vault_paths(&portable_root, &vaults);
        assert_eq!(resolved[0].path, "C:/external/vault");
    }

    #[test]
    fn makes_paths_inside_portable_root_relative() {
        let portable_root = PathBuf::from("D:/app/cistella-portable");
        let vaults = vec![RecentVault {
            path: portable_root
                .join("vaults/my-library")
                .to_string_lossy()
                .to_string(),
            name: "My Library".to_string(),
            opened_at: None,
        }];
        let portable = make_vault_paths_portable(&portable_root, &vaults);
        assert_eq!(portable[0].path, "vaults/my-library");
    }

    #[test]
    fn keeps_paths_outside_portable_root_absolute() {
        let portable_root = PathBuf::from("D:/app/cistella-portable");
        let vaults = vec![RecentVault {
            path: "C:/external/vault".to_string(),
            name: "External".to_string(),
            opened_at: None,
        }];
        let portable = make_vault_paths_portable(&portable_root, &vaults);
        assert_eq!(portable[0].path, "C:/external/vault");
    }
}
