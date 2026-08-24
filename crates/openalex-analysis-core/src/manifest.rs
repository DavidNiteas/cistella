use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, path::Path};

use crate::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetManifest {
    pub format_version: String,
    pub dataset_id: String,
    pub logical_schema_version: String,
    pub created_at: String,
    pub source: SourceProvenance,
    pub tables: BTreeMap<String, TableManifest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceProvenance {
    pub name: String,
    pub entity: String,
    pub snapshot_date: Option<String>,
    pub input_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableManifest {
    pub rows: usize,
    pub primary_key: Vec<String>,
    pub parquet: Option<TableFile>,
    pub arrow: Option<TableFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableFile {
    pub path: String,
    pub size_bytes: u64,
}

impl DatasetManifest {
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
