use std::{collections::BTreeMap, fs::File, path::{Path, PathBuf}};

use chrono::Utc;
use polars::prelude::*;
use uuid::Uuid;

use crate::{
    error::Result,
    manifest::{DatasetManifest, SourceProvenance, TableFile, TableManifest},
    schema::TableName,
};

#[derive(Debug, Clone)]
pub struct ImportOptions {
    pub raw_sources_dir: PathBuf,
    pub output_dir: PathBuf,
    pub build_arrow_cache: bool,
}

pub fn import_openalex_sources(options: ImportOptions) -> Result<DatasetManifest> {
    let parquet_dir = options.output_dir.join("parquet");
    let arrow_dir = options.output_dir.join("arrow");
    std::fs::create_dir_all(&parquet_dir)?;
    if options.build_arrow_cache {
        std::fs::create_dir_all(&arrow_dir)?;
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
            col("summary_stats").struct_().field_by_name("2yr_mean_citedness").alias("mean_citedness_2yr"),
            col("summary_stats").struct_().field_by_name("h_index").alias("h_index"),
            col("summary_stats").struct_().field_by_name("i10_index").alias("i10_index"),
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
        Some(TableFile { path: rel(&options.output_dir, &path), size_bytes: std::fs::metadata(&path)?.len() })
    } else {
        None
    };

    let mut tables = BTreeMap::new();
    tables.insert(
        TableName::Sources.as_str().to_string(),
        TableManifest {
            rows: sources.height(),
            primary_key: vec!["openalex_id".to_string()],
            parquet: Some(TableFile { path: rel(&options.output_dir, &sources_parquet), size_bytes: std::fs::metadata(&sources_parquet)?.len() }),
            arrow: arrow_file,
        },
    );

    let snapshot_date = read_raw_snapshot_date(&options.raw_sources_dir);
    let manifest = DatasetManifest {
        format_version: "0.1.0".to_string(),
        dataset_id: format!("openalex-sources-{}", Uuid::new_v4()),
        logical_schema_version: "0.1.0".to_string(),
        created_at: Utc::now().to_rfc3339(),
        source: SourceProvenance {
            name: "OpenAlex".to_string(),
            entity: "sources".to_string(),
            snapshot_date,
            input_path: options.raw_sources_dir.to_string_lossy().to_string(),
        },
        tables,
    };
    manifest.write_pretty(options.output_dir.join("manifest.json"))?;
    Ok(manifest)
}

fn write_parquet(df: &mut DataFrame, path: &Path) -> Result<()> {
    let mut file = File::create(path)?;
    ParquetWriter::new(&mut file).finish(df)?;
    Ok(())
}

fn write_arrow_ipc(df: &mut DataFrame, path: &Path) -> Result<()> {
    let mut file = File::create(path)?;
    IpcWriter::new(&mut file).finish(df)?;
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
    let text = std::fs::read_to_string(manifest_path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value.get("date")?.as_str().map(ToString::to_string)
}


