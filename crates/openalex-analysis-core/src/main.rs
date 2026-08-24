use std::path::PathBuf;

use openalex_analysis_core::{
    export_dataframe, import_openalex_sources, Dataset, DatasetOpenOptions, ExportFormat, ImportOptions,
    MetricCode, SourceSearchQuery,
};

fn metric_from_str(s: &str) -> MetricCode {
    match s {
        "works_count" => MetricCode::WorksCount,
        "cited_by_count" => MetricCode::CitedByCount,
        "i10_index" => MetricCode::I10Index,
        "mean_citedness_2yr" => MetricCode::MeanCitedness2Yr,
        _ => MetricCode::HIndex,
    }
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("import-sources") => {
            let raw = PathBuf::from(args.next().unwrap_or_else(|| "openalex-sources".to_string()));
            let out = PathBuf::from(args.next().unwrap_or_else(|| "openalex-library".to_string()));
            let build_arrow_cache = !matches!(args.next().as_deref(), Some("--no-arrow"));
            let manifest = import_openalex_sources(ImportOptions { raw_sources_dir: raw, output_dir: out.clone(), build_arrow_cache })?;
            println!("{}", serde_json::to_string_pretty(&manifest)?);
        }
        Some("top") => {
            let path = PathBuf::from(args.next().unwrap_or_else(|| "openalex-library".to_string()));
            let source_type = args.next().unwrap_or_else(|| "journal".to_string());
            let metric = metric_from_str(&args.next().unwrap_or_else(|| "h_index".to_string()));
            let limit = args.next().and_then(|s| s.parse().ok()).unwrap_or(20);
            let dataset = Dataset::open_any(path, DatasetOpenOptions::default())?;
            println!("{}", dataset.top_sources_json(metric, &source_type, limit)?);
        }
        Some("search") => {
            let path = PathBuf::from(args.next().unwrap_or_else(|| "openalex-library".to_string()));
            let source_type = args.next().unwrap_or_else(|| "journal".to_string());
            let limit = args.next().and_then(|s| s.parse().ok()).unwrap_or(20);
            let text = args.next();
            let dataset = Dataset::open_any(path, DatasetOpenOptions::default())?;
            println!("{}", dataset.search_sources_json(SourceSearchQuery { text, source_type: Some(source_type), limit, ..Default::default() })?);
        }
        Some("overview") => {
            let path = PathBuf::from(args.next().unwrap_or_else(|| "openalex-library".to_string()));
            let dataset = Dataset::open_any(path, DatasetOpenOptions::default())?;
            println!("{}", serde_json::to_string_pretty(&dataset.overview_json()?)?);
        }
        Some("export-top") => {
            let path = PathBuf::from(args.next().unwrap_or_else(|| "openalex-library".to_string()));
            let output = PathBuf::from(args.next().unwrap_or_else(|| "top_sources.csv".to_string()));
            let source_type = args.next().unwrap_or_else(|| "journal".to_string());
            let metric = metric_from_str(&args.next().unwrap_or_else(|| "h_index".to_string()));
            let limit = args.next().and_then(|s| s.parse().ok()).unwrap_or(100);
            let dataset = Dataset::open_any(path, DatasetOpenOptions::default())?;
            let df = dataset.top_sources_frame(metric, &source_type, limit)?;
            export_dataframe(df, &output, ExportFormat::from_path(&output))?;
            println!("exported {}", output.display());
        }
        Some("export-search") => {
            let path = PathBuf::from(args.next().unwrap_or_else(|| "openalex-library".to_string()));
            let output = PathBuf::from(args.next().unwrap_or_else(|| "sources.csv".to_string()));
            let source_type = args.next().unwrap_or_else(|| "journal".to_string());
            let limit = args.next().and_then(|s| s.parse().ok()).unwrap_or(1000);
            let text = args.next();
            let dataset = Dataset::open_any(path, DatasetOpenOptions::default())?;
            let df = dataset.search_sources_frame(SourceSearchQuery { text, source_type: Some(source_type), limit, ..Default::default() })?;
            export_dataframe(df, &output, ExportFormat::from_path(&output))?;
            println!("exported {}", output.display());
        }
        _ => {
            eprintln!("Usage:");
            eprintln!("  openalex-analysis-core import-sources <raw-openalex-sources-dir> <output-library-dir> [--no-arrow]");
            eprintln!("  openalex-analysis-core overview <library-dir|sources.arrow|sources.parquet>");
            eprintln!("  openalex-analysis-core top <library-or-file> [journal|conference] [h_index|cited_by_count|works_count|i10_index|mean_citedness_2yr] [limit]");
            eprintln!("  openalex-analysis-core search <library-or-file> [journal|conference] [limit] [text]");
            eprintln!("  openalex-analysis-core export-top <library-or-file> <output.csv|output.xlsx> [journal|conference] [metric] [limit]");
            eprintln!("  openalex-analysis-core export-search <library-or-file> <output.csv|output.xlsx> [journal|conference] [limit] [text]");
        }
    }
    Ok(())
}
