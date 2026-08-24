use std::{path::PathBuf, sync::Mutex};

use openalex_analysis_core::{
    export_dataframe, import_openalex_sources, Dataset, DatasetOpenOptions, ExportFormat, ImportOptions,
    MetricCode, SourceSearchQuery,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

type CommandResult<T> = std::result::Result<T, String>;

#[derive(Default)]
struct AppState {
    dataset_path: Mutex<Option<PathBuf>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImportSourcesRequest {
    raw_sources_dir: String,
    output_dir: String,
    build_arrow_cache: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchSourcesRequest {
    text: Option<String>,
    source_type: Option<String>,
    country_code: Option<String>,
    is_oa: Option<bool>,
    limit: Option<usize>,
    offset: Option<usize>,
}

fn parse_metric(metric: &str) -> MetricCode {
    match metric {
        "works_count" => MetricCode::WorksCount,
        "cited_by_count" => MetricCode::CitedByCount,
        "i10_index" => MetricCode::I10Index,
        "mean_citedness_2yr" => MetricCode::MeanCitedness2Yr,
        _ => MetricCode::HIndex,
    }
}

fn dataset_from_state(state: &tauri::State<AppState>) -> CommandResult<Dataset> {
    let path = state
        .dataset_path
        .lock()
        .map_err(|e| e.to_string())?
        .clone()
        .ok_or_else(|| "No dataset is connected".to_string())?;
    Dataset::open_any(path, DatasetOpenOptions::default()).map_err(|e| e.to_string())
}

#[tauri::command]
fn import_sources(req: ImportSourcesRequest, state: tauri::State<AppState>) -> CommandResult<Value> {
    let manifest = import_openalex_sources(ImportOptions {
        raw_sources_dir: PathBuf::from(&req.raw_sources_dir),
        output_dir: PathBuf::from(&req.output_dir),
        build_arrow_cache: req.build_arrow_cache,
    })
    .map_err(|e| e.to_string())?;
    *state.dataset_path.lock().map_err(|e| e.to_string())? = Some(PathBuf::from(req.output_dir));
    serde_json::to_value(manifest).map_err(|e| e.to_string())
}

#[tauri::command]
fn connect_dataset(path: String, state: tauri::State<AppState>) -> CommandResult<Value> {
    let dataset = Dataset::open_any(&path, DatasetOpenOptions::default()).map_err(|e| e.to_string())?;
    *state.dataset_path.lock().map_err(|e| e.to_string())? = Some(PathBuf::from(path));
    serde_json::to_value(dataset.manifest()).map_err(|e| e.to_string())
}

#[tauri::command]
fn dataset_overview(state: tauri::State<AppState>) -> CommandResult<Value> {
    dataset_from_state(&state)?.overview_json().map_err(|e| e.to_string())
}

#[tauri::command]
fn search_sources(req: SearchSourcesRequest, state: tauri::State<AppState>) -> CommandResult<Value> {
    dataset_from_state(&state)?
        .search_sources_json(SourceSearchQuery {
            text: req.text,
            source_type: req.source_type,
            country_code: req.country_code,
            is_oa: req.is_oa,
            limit: req.limit.unwrap_or(50),
            offset: req.offset.unwrap_or(0),
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn top_sources(metric: String, source_type: String, limit: usize, state: tauri::State<AppState>) -> CommandResult<Value> {
    dataset_from_state(&state)?
        .top_sources_json(parse_metric(&metric), &source_type, limit)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn export_top_sources(output: String, metric: String, source_type: String, limit: usize, state: tauri::State<AppState>) -> CommandResult<Value> {
    let dataset = dataset_from_state(&state)?;
    let output_path = PathBuf::from(output);
    let df = dataset.top_sources_frame(parse_metric(&metric), &source_type, limit).map_err(|e| e.to_string())?;
    export_dataframe(df, &output_path, ExportFormat::from_path(&output_path)).map_err(|e| e.to_string())?;
    Ok(json!({ "output": output_path.to_string_lossy(), "rows": limit }))
}

#[tauri::command]
fn export_search_sources(output: String, req: SearchSourcesRequest, state: tauri::State<AppState>) -> CommandResult<Value> {
    let dataset = dataset_from_state(&state)?;
    let output_path = PathBuf::from(output);
    let limit = req.limit.unwrap_or(1000);
    let df = dataset
        .search_sources_frame(SourceSearchQuery {
            text: req.text,
            source_type: req.source_type,
            country_code: req.country_code,
            is_oa: req.is_oa,
            limit,
            offset: req.offset.unwrap_or(0),
        })
        .map_err(|e| e.to_string())?;
    export_dataframe(df, &output_path, ExportFormat::from_path(&output_path)).map_err(|e| e.to_string())?;
    Ok(json!({ "output": output_path.to_string_lossy(), "rows": limit }))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::default())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            import_sources,
            connect_dataset,
            dataset_overview,
            search_sources,
            top_sources,
            export_top_sources,
            export_search_sources
        ])
        .run(tauri::generate_context!())
        .expect("error while running OpenAlex Analysis Studio");
}


