use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};

use serde::Serialize;

use chrono::Utc;
use polars::prelude::*;
use uuid::Uuid;

use crate::{
    error::Result,
    manifest::{VaultManifest, VaultSourceProvenance, VaultTableFile, VaultTableManifest},
    schema::TableName,
};

pub mod bibtex_importer;
pub mod bibtex_source;
pub mod conflict;
pub mod doi_resolver;
pub mod library_importer;
pub mod openalex_works_source;
pub mod record_source;
pub mod ris_source;
pub mod source_record;

pub use record_source::ImportFormat;

#[derive(Debug, Clone)]
pub struct ImportOptions {
    pub raw_sources_dir: PathBuf,
    pub output_dir: PathBuf,
    pub build_arrow_cache: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenAlexSourcesPreview {
    pub raw_sources_dir: String,
    pub partition_count: usize,
    pub has_manifest: bool,
    pub snapshot_date: Option<String>,
}

pub fn inspect_openalex_sources(
    raw_sources_dir: impl AsRef<Path>,
) -> Result<OpenAlexSourcesPreview> {
    let raw_sources_dir = raw_sources_dir.as_ref();
    let pattern = raw_sources_dir
        .join("updated_date=*/part_0000.parquet")
        .to_string_lossy()
        .replace('\\', "/");
    let partition_count = glob::glob(&pattern)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()))?
        .filter_map(std::result::Result::ok)
        .count();
    if partition_count == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no OpenAlex Sources partitions found (expected updated_date=*/part_0000.parquet)",
        )
        .into());
    }
    Ok(OpenAlexSourcesPreview {
        raw_sources_dir: raw_sources_dir.to_string_lossy().to_string(),
        partition_count,
        has_manifest: raw_sources_dir.join("manifest.json").is_file(),
        snapshot_date: read_raw_snapshot_date(raw_sources_dir),
    })
}

/// Builds a Vault in a sibling staging directory and publishes it with one directory rename.
///
/// The destination is deliberately required to be absent. This prevents a failed retry from
/// mixing newly generated table files with an older manifest, and keeps an existing Vault
/// untouched. A staging directory has no `manifest.json` at its beginning; the manifest is
/// written and synced only after all table files have been written. If the process is
/// interrupted, the abandoned staging directory is recognizable by its name and the requested
/// output path remains absent, so it cannot be opened as a Vault.
pub fn import_openalex_sources(options: ImportOptions) -> Result<VaultManifest> {
    ensure_output_is_available(&options.output_dir)?;
    inspect_openalex_sources(&options.raw_sources_dir)?;

    with_staging_directory(&options.output_dir, |staging_dir| {
        build_openalex_sources(&options, staging_dir)
    })
}

fn build_openalex_sources(options: &ImportOptions, output_dir: &Path) -> Result<VaultManifest> {
    let parquet_dir = output_dir.join("parquet");
    let arrow_dir = output_dir.join("arrow");
    fs::create_dir_all(&parquet_dir)?;
    if options.build_arrow_cache {
        fs::create_dir_all(&arrow_dir)?;
    }

    let glob_path = options
        .raw_sources_dir
        .join("updated_date=*/part_0000.parquet")
        .to_string_lossy()
        .replace('\\', "/");

    let mut sources = LazyFrame::scan_parquet(glob_path.as_str().into(), Default::default())?
        .select([
            col("id").alias("openalex_id"),
            col("display_name"),
            col("type").alias("source_type"),
            col("issn_l"),
            col("works_count"),
            col("oa_works_count"),
            col("cited_by_count"),
            col("summary_stats")
                .struct_()
                .field_by_name("2yr_mean_citedness")
                .alias("mean_citedness_2yr"),
            col("summary_stats")
                .struct_()
                .field_by_name("h_index")
                .alias("h_index"),
            col("summary_stats")
                .struct_()
                .field_by_name("i10_index")
                .alias("i10_index"),
            col("is_oa"),
            col("is_in_doaj"),
            col("is_high_oa_rate"),
            col("is_in_scielo"),
            col("is_ojs"),
            col("is_core"),
            col("first_publication_year"),
            col("last_publication_year"),
            col("homepage_url"),
            col("apc_usd"),
            col("country_code"),
            col("updated_date").alias("openalex_updated_at"),
            col("created_date").alias("openalex_created_at"),
        ])
        .collect()?;

    let sources_parquet = parquet_dir.join(TableName::Sources.parquet_file());
    write_parquet(&mut sources, &sources_parquet)?;

    let arrow_file = if options.build_arrow_cache {
        let path = arrow_dir.join(TableName::Sources.arrow_file());
        write_arrow_ipc(&mut sources, &path)?;
        Some(VaultTableFile {
            path: rel(output_dir, &path),
            size_bytes: fs::metadata(&path)?.len(),
        })
    } else {
        None
    };

    let mut tables = BTreeMap::new();
    tables.insert(
        TableName::Sources.as_str().to_string(),
        VaultTableManifest {
            rows: sources.height(),
            primary_key: vec!["openalex_id".to_string()],
            parquet: Some(VaultTableFile {
                path: rel(output_dir, &sources_parquet),
                size_bytes: fs::metadata(&sources_parquet)?.len(),
            }),
            arrow: arrow_file,
        },
    );

    let snapshot_date = read_raw_snapshot_date(&options.raw_sources_dir);
    let manifest = VaultManifest {
        format_version: "0.1.0".to_string(),
        vault_id: format!("openalex-sources-{}", Uuid::new_v4()),
        logical_schema_version: "0.1.0".to_string(),
        created_at: Utc::now().to_rfc3339(),
        source: VaultSourceProvenance {
            name: "OpenAlex".to_string(),
            entity: "sources".to_string(),
            snapshot_date,
            input_path: options.raw_sources_dir.to_string_lossy().to_string(),
        },
        tables,
    };

    // The manifest is the commit marker inside staging. It is written last and synced before
    // the staging directory is renamed to the requested output path.
    write_manifest_commit(&manifest, &output_dir.join("manifest.json"))?;
    Ok(manifest)
}

fn ensure_output_is_available(output_dir: &Path) -> Result<()> {
    if fs::symlink_metadata(output_dir).is_ok() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!(
                "import output already exists; choose a new Vault path: {}",
                output_dir.display()
            ),
        )
        .into());
    }
    Ok(())
}

fn with_staging_directory<T>(
    output_dir: &Path,
    build: impl FnOnce(&Path) -> Result<T>,
) -> Result<T> {
    let parent = output_dir.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let name = output_dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "import output must have a directory name: {}",
                    output_dir.display()
                ),
            )
        })?;
    let staging_dir = parent.join(format!(".{name}.cistella-import-{}.tmp", Uuid::new_v4()));
    fs::create_dir(&staging_dir)?;
    let mut workspace = ImportWorkspace {
        staging_dir,
        output_dir: output_dir.to_path_buf(),
        committed: false,
    };

    let result = build(&workspace.staging_dir)?;
    // A race creating the destination must fail without replacing anything. The guard removes
    // the complete staging directory on this or any earlier error.
    ensure_output_is_available(&workspace.output_dir)?;
    fs::rename(&workspace.staging_dir, &workspace.output_dir)?;
    workspace.committed = true;
    Ok(result)
}

struct ImportWorkspace {
    staging_dir: PathBuf,
    output_dir: PathBuf,
    committed: bool,
}

impl Drop for ImportWorkspace {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_dir_all(&self.staging_dir);
        }
    }
}

fn write_manifest_commit(manifest: &VaultManifest, path: &Path) -> Result<()> {
    let text = serde_json::to_string_pretty(manifest)?;
    let mut file = File::create(path)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

fn write_parquet(df: &mut DataFrame, path: &Path) -> Result<()> {
    let mut file = File::create(path)?;
    ParquetWriter::new(&mut file).finish(df)?;
    file.sync_all()?;
    Ok(())
}

fn write_arrow_ipc(df: &mut DataFrame, path: &Path) -> Result<()> {
    let mut file = File::create(path)?;
    IpcWriter::new(&mut file).finish(df)?;
    file.sync_all()?;
    Ok(())
}

fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn read_raw_snapshot_date(raw_sources_dir: &Path) -> Option<String> {
    let manifest_path = raw_sources_dir.join("manifest.json");
    let text = fs::read_to_string(manifest_path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value.get("date")?.as_str().map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn temp_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "{label}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn inspect_openalex_sources_finds_partitions() {
        let root = temp_root("cistella-import-test");
        let partition = root.join("updated_date=2026-08-25");
        fs::create_dir_all(&partition).unwrap();
        fs::write(partition.join("part_0000.parquet"), b"").unwrap();
        fs::write(root.join("manifest.json"), r#"{"date":"2026-08-25"}"#).unwrap();
        let preview = inspect_openalex_sources(&root).unwrap();
        assert_eq!(preview.partition_count, 1);
        assert!(preview.has_manifest);
        assert_eq!(preview.snapshot_date.as_deref(), Some("2026-08-25"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_staged_build_removes_temporary_output_and_publishes_nothing() {
        let parent = temp_root("cistella-import-stage-test");
        fs::create_dir_all(&parent).unwrap();
        let output = parent.join("vault");
        let result = with_staging_directory(&output, |staging| {
            fs::write(staging.join("partial.parquet"), b"partial").unwrap();
            // Even a manifest written in staging must not become visible at the requested
            // destination unless the final directory rename succeeds.
            fs::write(staging.join("manifest.json"), b"staged commit marker").unwrap();
            Err::<(), _>(
                std::io::Error::new(std::io::ErrorKind::Interrupted, "simulated interruption")
                    .into(),
            )
        });
        assert!(result.is_err());
        assert!(!output.exists());
        let leftovers: Vec<_> = fs::read_dir(&parent)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.path())
            .collect();
        assert!(leftovers.is_empty(), "staging leftovers: {leftovers:?}");
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn existing_output_is_not_touched_or_overwritten() {
        let parent = temp_root("cistella-import-existing-test");
        fs::create_dir_all(&parent).unwrap();
        let output = parent.join("vault");
        fs::create_dir_all(&output).unwrap();
        fs::write(output.join("sentinel"), b"keep").unwrap();
        let err = import_openalex_sources(ImportOptions {
            raw_sources_dir: parent.join("missing-input"),
            output_dir: output.clone(),
            build_arrow_cache: false,
        })
        .unwrap_err();
        assert!(err.to_string().contains("already exists"));
        assert_eq!(fs::read(output.join("sentinel")).unwrap(), b"keep");
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn failed_preflight_does_not_create_destination_or_staging_output() {
        let parent = temp_root("cistella-import-preflight-test");
        fs::create_dir_all(&parent).unwrap();
        let output = parent.join("vault");
        let err = import_openalex_sources(ImportOptions {
            raw_sources_dir: parent.join("missing-input"),
            output_dir: output.clone(),
            build_arrow_cache: false,
        })
        .unwrap_err();
        assert!(err.to_string().contains("no OpenAlex Sources partitions"));
        assert!(!output.exists());
        let leftovers: Vec<_> = fs::read_dir(&parent)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.path())
            .collect();
        assert!(leftovers.is_empty(), "staging leftovers: {leftovers:?}");
        fs::remove_dir_all(parent).unwrap();
    }
}
