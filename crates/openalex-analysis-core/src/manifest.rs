use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, path::Path};

use crate::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultManifest {
    pub format_version: String,
    pub vault_id: String,
    pub logical_schema_version: String,
    pub created_at: String,
    pub source: VaultSourceProvenance,
    pub tables: BTreeMap<String, VaultTableManifest>,
}

pub type DatasetManifest = VaultManifest;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultSourceProvenance {
    pub name: String,
    pub entity: String,
    pub snapshot_date: Option<String>,
    pub input_path: String,
}

pub type SourceProvenance = VaultSourceProvenance;
pub type DatasetSourceProvenance = VaultSourceProvenance;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultTableManifest {
    pub rows: usize,
    pub primary_key: Vec<String>,
    pub parquet: Option<VaultTableFile>,
    pub arrow: Option<VaultTableFile>,
}

pub type TableManifest = VaultTableManifest;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultTableFile {
    pub path: String,
    pub size_bytes: u64,
}

pub type TableFile = VaultTableFile;

impl VaultManifest {
    pub fn read(path: impl AsRef<Path>) -> Result<Self> {
        let text = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn write_pretty(&self, path: impl AsRef<Path>) -> Result<()> {
        let text = serde_json::to_string_pretty(self)?;
        fs::write(path, text)?;
        Ok(())
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

    fn sample_manifest() -> VaultManifest {
        VaultManifest {
            format_version: "0.1.0".to_string(),
            vault_id: "test-vault".to_string(),
            logical_schema_version: "0.1.0".to_string(),
            created_at: "2026-08-26T00:00:00Z".to_string(),
            source: VaultSourceProvenance {
                name: "Test source".to_string(),
                entity: "sources".to_string(),
                snapshot_date: Some("2026-08-26".to_string()),
                input_path: "input".to_string(),
            },
            tables: BTreeMap::from([(
                "sources".to_string(),
                VaultTableManifest {
                    rows: 3,
                    primary_key: vec!["id".to_string()],
                    parquet: Some(VaultTableFile {
                        path: "parquet/sources.parquet".to_string(),
                        size_bytes: 12,
                    }),
                    arrow: None,
                },
            )]),
        }
    }

    #[test]
    fn manifest_roundtrips_without_losing_vault_semantics() {
        let root = std::env::temp_dir().join(format!(
            "cistella-manifest-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("manifest.json");
        let expected = sample_manifest();
        expected.write_pretty(&path).unwrap();
        let actual = VaultManifest::read(&path).unwrap();
        assert_eq!(actual.vault_id, "test-vault");
        assert_eq!(actual.tables["sources"].rows, 3);
        assert_eq!(actual.source.entity, "sources");
        fs::remove_dir_all(root).unwrap();
    }
}
