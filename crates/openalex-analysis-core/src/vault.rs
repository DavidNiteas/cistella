use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
};

use polars::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{
    error::{CoreError, Result},
    layout::StorageLayout,
    manifest::{VaultManifest, VaultSourceProvenance, VaultTableFile, VaultTableManifest},
    schema::TableName,
    secure_user_records::SecureVaultRoot,
};

#[derive(Debug, Clone)]
pub struct VaultOpenOptions {
    pub layout: StorageLayout,
    pub use_mmap: bool,
}

impl Default for VaultOpenOptions {
    fn default() -> Self {
        Self {
            layout: StorageLayout::Auto,
            use_mmap: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultContext {
    pub root: String,
    pub manifest: VaultManifest,
    pub table_count: usize,
}

#[derive(Debug, Clone)]
pub struct Vault {
    root: PathBuf,
    manifest: VaultManifest,
    options: VaultOpenOptions,
    secure_user_root: SecureVaultRoot,
}

impl Vault {
    pub fn open(root: impl AsRef<Path>, options: VaultOpenOptions) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let manifest = VaultManifest::read(root.join("manifest.json"))?;
        Self::validate_manifest_table_paths(&manifest)?;
        let vault_id = manifest.vault_id.clone();
        Ok(Self {
            root: root.clone(),
            manifest,
            options,
            secure_user_root: SecureVaultRoot::open(&root, &vault_id)?,
        })
    }

    fn validate_table_file_path(path: &str) -> Result<()> {
        let trimmed = path.trim();
        let is_invalid = trimmed.is_empty()
            || Path::new(path).is_absolute()
            || Path::new(path).components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::Prefix(_) | Component::RootDir
                )
            });
        if is_invalid {
            return Err(CoreError::InvalidTablePath(path.to_string()));
        }
        Ok(())
    }

    fn validate_manifest_table_paths(manifest: &VaultManifest) -> Result<()> {
        for table in manifest.tables.values() {
            if let Some(file) = table.parquet.as_ref() {
                Self::validate_table_file_path(&file.path)?;
            }
            if let Some(file) = table.arrow.as_ref() {
                Self::validate_table_file_path(&file.path)?;
            }
        }
        Ok(())
    }

    /// Open either a full analysis library directory, or a single `sources.arrow`/`sources.parquet` file.
    /// Single Arrow IPC files are treated as serving-cache sources; single Parquet files are treated as
    /// canonical compressed sources and are collected into memory by query calls.
    pub fn open_any(path: impl AsRef<Path>, options: VaultOpenOptions) -> Result<Self> {
        let path = path.as_ref();
        if path.is_dir() {
            return Self::open(path, options);
        }
        Self::open_sources_file(path, options)
    }

    pub fn open_sources_file(path: impl AsRef<Path>, options: VaultOpenOptions) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let root = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let file_name = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string_lossy().to_string());
        let size = std::fs::metadata(&path)?.len();
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let table_file = VaultTableFile {
            path: file_name,
            size_bytes: size,
        };
        let (parquet, arrow) = if ext == "arrow" || ext == "ipc" {
            (None, Some(table_file))
        } else {
            (Some(table_file), None)
        };
        let mut tables = BTreeMap::new();
        tables.insert(
            TableName::Sources.as_str().to_string(),
            VaultTableManifest {
                rows: 0,
                primary_key: vec!["openalex_id".to_string()],
                parquet,
                arrow,
            },
        );
        let manifest = VaultManifest {
            format_version: "0.1.0".to_string(),
            vault_id: format!("external:{}", path.display()),
            logical_schema_version: "0.1.0".to_string(),
            created_at: String::new(),
            source: VaultSourceProvenance {
                name: "External".to_string(),
                entity: "sources".to_string(),
                snapshot_date: None,
                input_path: path.to_string_lossy().to_string(),
            },
            tables,
        };
        let vault_id = manifest.vault_id.clone();
        Ok(Self {
            root: root.clone(),
            manifest,
            options,
            secure_user_root: SecureVaultRoot::open(&root, &vault_id)?,
        })
    }

    pub(crate) fn secure_user_root(&self) -> &SecureVaultRoot {
        &self.secure_user_root
    }

    pub(crate) fn root_path(&self) -> &Path {
        &self.root
    }

    pub fn manifest(&self) -> &VaultManifest {
        &self.manifest
    }

    pub fn context(&self) -> VaultContext {
        VaultContext {
            root: self.root.to_string_lossy().to_string(),
            manifest: self.manifest.clone(),
            table_count: self.manifest.tables.len(),
        }
    }

    pub fn table_path(&self, table: TableName) -> Result<PathBuf> {
        let tm = self
            .manifest
            .tables
            .get(table.as_str())
            .ok_or_else(|| CoreError::MissingTable(table.as_str().to_string()))?;
        let rel = match self.options.layout {
            StorageLayout::ArrowIpc => tm.arrow.as_ref().or(tm.parquet.as_ref()),
            StorageLayout::Parquet => tm.parquet.as_ref().or(tm.arrow.as_ref()),
            StorageLayout::Auto => tm.arrow.as_ref().or(tm.parquet.as_ref()),
        }
        .ok_or_else(|| CoreError::MissingTable(table.as_str().to_string()))?;
        Self::validate_table_file_path(&rel.path)?;
        Ok(self.root.join(&rel.path))
    }

    pub fn scan_table(&self, table: TableName) -> Result<LazyFrame> {
        let path = self.table_path(table)?;
        let path_str = path.to_string_lossy().replace('\\', "/");
        if path
            .extension()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.eq_ignore_ascii_case("arrow") || s.eq_ignore_ascii_case("ipc"))
        {
            Ok(LazyFrame::scan_ipc(
                path_str.as_str().into(),
                Default::default(),
                Default::default(),
            )?)
        } else {
            Ok(LazyFrame::scan_parquet(
                path_str.as_str().into(),
                Default::default(),
            )?)
        }
    }

    pub fn load_table(&self, table: TableName) -> Result<DataFrame> {
        Ok(self.scan_table(table)?.collect()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::BTreeMap,
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn temp_root(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "{}-{}",
            prefix,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn write_manifest(root: &Path, table_path: &str) {
        let manifest = format!(
            r#"{{
          "format_version":"0.1.0",
          "vault_id":"context-test",
          "logical_schema_version":"0.1.0",
          "created_at":"2026-08-26T00:00:00Z",
          "source":{{"name":"Test","entity":"sources","snapshot_date":null,"input_path":"input"}},
          "tables":{{"sources":{{"rows":0,"primary_key":["id"],"parquet":{{"path":"{}","size_bytes":0}},"arrow":null}}}}
        }}"#,
            table_path
        );
        fs::write(root.join("manifest.json"), manifest).unwrap();
    }

    fn valid_vault(root: &Path, table_path: &str) -> Vault {
        let manifest = VaultManifest {
            format_version: "0.1.0".to_string(),
            vault_id: "context-test".to_string(),
            logical_schema_version: "0.1.0".to_string(),
            created_at: "2026-08-26T00:00:00Z".to_string(),
            source: VaultSourceProvenance {
                name: "Test".to_string(),
                entity: "sources".to_string(),
                snapshot_date: None,
                input_path: "input".to_string(),
            },
            tables: BTreeMap::from([(
                "sources".to_string(),
                VaultTableManifest {
                    rows: 0,
                    primary_key: vec!["id".to_string()],
                    parquet: Some(VaultTableFile {
                        path: table_path.to_string(),
                        size_bytes: 0,
                    }),
                    arrow: None,
                },
            )]),
        };
        let vault_id = manifest.vault_id.clone();
        Vault {
            root: root.to_path_buf(),
            manifest,
            options: VaultOpenOptions::default(),
            secure_user_root: SecureVaultRoot::open(root, &vault_id).unwrap(),
        }
    }

    #[test]
    fn context_exposes_portable_root_and_table_count() {
        let root = temp_root("cistella-vault-test");
        fs::create_dir_all(&root).unwrap();
        write_manifest(&root, "parquet/sources.parquet");
        let vault = Vault::open(&root, VaultOpenOptions::default()).unwrap();
        let context = vault.context();
        assert_eq!(context.manifest.vault_id, "context-test");
        assert_eq!(context.table_count, 1);
        assert_eq!(context.root, root.to_string_lossy());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn vault_open_rejects_absolute_table_file_paths() {
        let root = temp_root("cistella-vault-abs-test");
        fs::create_dir_all(&root).unwrap();
        let absolute_path = root
            .join("tables")
            .join("sources.parquet")
            .to_string_lossy()
            .replace('\\', "/");
        write_manifest(&root, &absolute_path);
        let err = Vault::open(&root, VaultOpenOptions::default()).unwrap_err();
        assert!(matches!(err, CoreError::InvalidTablePath(_)));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn vault_open_rejects_parent_dir_table_file_paths() {
        let root = temp_root("cistella-vault-escape-test");
        fs::create_dir_all(&root).unwrap();
        write_manifest(&root, "../escape/sources.parquet");
        let err = Vault::open(&root, VaultOpenOptions::default()).unwrap_err();
        assert!(matches!(err, CoreError::InvalidTablePath(_)));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn table_path_joins_relative_table_file_paths() {
        let root = temp_root("cistella-vault-table-path-test");
        fs::create_dir_all(&root).unwrap();
        let vault = valid_vault(&root, "tables/sources.parquet");
        let path = vault.table_path(TableName::Sources).unwrap();
        assert_eq!(path, root.join("tables/sources.parquet"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn table_path_rejects_escape_sequences_even_after_open() {
        let root = temp_root("cistella-vault-table-path-escape-test");
        fs::create_dir_all(&root).unwrap();
        let vault = valid_vault(&root, "../escape/sources.parquet");
        let err = vault.table_path(TableName::Sources).unwrap_err();
        assert!(matches!(err, CoreError::InvalidTablePath(_)));
        fs::remove_dir_all(root).unwrap();
    }
}
// User-owned literature items intentionally live outside manifest tables.
