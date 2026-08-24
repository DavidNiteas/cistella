use std::{collections::BTreeMap, path::{Path, PathBuf}};

use polars::prelude::*;

use crate::{
    error::{CoreError, Result},
    layout::StorageLayout,
    manifest::{DatasetManifest, SourceProvenance, TableFile, TableManifest},
    schema::TableName,
};

#[derive(Debug, Clone)]
pub struct DatasetOpenOptions {
    pub layout: StorageLayout,
    pub use_mmap: bool,
}

impl Default for DatasetOpenOptions {
    fn default() -> Self { Self { layout: StorageLayout::Auto, use_mmap: true } }
}

#[derive(Debug, Clone)]
pub struct Dataset {
    root: PathBuf,
    manifest: DatasetManifest,
    options: DatasetOpenOptions,
}

impl Dataset {
    pub fn open(root: impl AsRef<Path>, options: DatasetOpenOptions) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let manifest = DatasetManifest::read(root.join("manifest.json"))?;
        Ok(Self { root, manifest, options })
    }

    /// Open either a full analysis library directory, or a single `sources.arrow`/`sources.parquet` file.
    /// Single Arrow IPC files are treated as serving-cache sources; single Parquet files are treated as
    /// canonical compressed sources and are collected into memory by query calls.
    pub fn open_any(path: impl AsRef<Path>, options: DatasetOpenOptions) -> Result<Self> {
        let path = path.as_ref();
        if path.is_dir() { return Self::open(path, options); }
        Self::open_sources_file(path, options)
    }

    pub fn open_sources_file(path: impl AsRef<Path>, options: DatasetOpenOptions) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let root = path.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
        let file_name = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string_lossy().to_string());
        let size = std::fs::metadata(&path)?.len();
        let ext = path.extension().and_then(|s| s.to_str()).unwrap_or_default().to_ascii_lowercase();
        let table_file = TableFile { path: file_name, size_bytes: size };
        let (parquet, arrow) = if ext == "arrow" || ext == "ipc" { (None, Some(table_file)) } else { (Some(table_file), None) };
        let mut tables = BTreeMap::new();
        tables.insert(TableName::Sources.as_str().to_string(), TableManifest { rows: 0, primary_key: vec!["openalex_id".to_string()], parquet, arrow });
        let manifest = DatasetManifest {
            format_version: "0.1.0".to_string(),
            dataset_id: format!("external:{}", path.display()),
            logical_schema_version: "0.1.0".to_string(),
            created_at: String::new(),
            source: SourceProvenance { name: "External".to_string(), entity: "sources".to_string(), snapshot_date: None, input_path: path.to_string_lossy().to_string() },
            tables,
        };
        Ok(Self { root, manifest, options })
    }

    pub fn manifest(&self) -> &DatasetManifest { &self.manifest }

    pub fn table_path(&self, table: TableName) -> Result<PathBuf> {
        let tm = self.manifest.tables.get(table.as_str()).ok_or_else(|| CoreError::MissingTable(table.as_str().to_string()))?;
        let rel = match self.options.layout {
            StorageLayout::ArrowIpc => tm.arrow.as_ref().or(tm.parquet.as_ref()),
            StorageLayout::Parquet => tm.parquet.as_ref().or(tm.arrow.as_ref()),
            StorageLayout::Auto => tm.arrow.as_ref().or(tm.parquet.as_ref()),
        }.ok_or_else(|| CoreError::MissingTable(table.as_str().to_string()))?;
        Ok(self.root.join(&rel.path))
    }

    pub fn scan_table(&self, table: TableName) -> Result<LazyFrame> {
        let path = self.table_path(table)?;
        let path_str = path.to_string_lossy().replace('\\', "/");
        if path.extension().and_then(|s| s.to_str()).is_some_and(|s| s.eq_ignore_ascii_case("arrow") || s.eq_ignore_ascii_case("ipc")) {
            Ok(LazyFrame::scan_ipc(path_str.as_str().into(), Default::default(), Default::default())?)
        } else {
            Ok(LazyFrame::scan_parquet(path_str.as_str().into(), Default::default())?)
        }
    }

    pub fn load_table(&self, table: TableName) -> Result<DataFrame> { Ok(self.scan_table(table)?.collect()?) }
}
