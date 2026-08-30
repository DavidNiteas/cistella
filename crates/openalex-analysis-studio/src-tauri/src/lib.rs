use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

use cistella_core::{
    AnnotationResolution, AppDirectories, CoreError, DEFAULT_SEARCH_PAGE_SIZE, DocumentAssetKind,
    ExportFormat, ExternalIdentifier, ImportFormat, LiteratureItemDraft, LiteratureItemType,
    MetricCode, Note as CoreNote, NoteDraft, OpenAlexSourcesAdapter, ReadingStatus, RecentVault,
    SearchFieldScope, SearchIndexTaskState, SearchIndexTaskStatus, SearchQuery, SourceAdapter,
    SourceRecord, SourceSearchQuery, Vault, VaultOpenOptions, available_source_adapters,
    backup_vault as backup_vault_core, export_dataframe, load_recent_vaults,
    migrate_vaults_to_installed as migrate_vaults_to_installed_core,
    migrate_vaults_to_portable as migrate_vaults_to_portable_core, resolve_recent_vault_paths,
    restore_vault as restore_vault_core,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri_plugin_opener::OpenerExt;

type CommandResult<T> = std::result::Result<T, String>;

#[derive(Default)]
struct VaultConnectionState {
    vault_path: Option<PathBuf>,
    latest_connection_generation: u64,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct SearchIndexTaskOwner {
    connection_generation: u64,
    vault_path: PathBuf,
}

struct SearchIndexTaskStore {
    task_id: Option<uuid::Uuid>,
    cancellation: Option<Arc<AtomicBool>>,
    state: SearchIndexTaskState,
}

impl Default for SearchIndexTaskStore {
    fn default() -> Self {
        Self {
            task_id: None,
            cancellation: None,
            state: idle_search_task_state(),
        }
    }
}

#[derive(Default)]
struct SearchIndexTaskRegistry {
    slots: HashMap<SearchIndexTaskOwner, SearchIndexTaskStore>,
}

#[derive(Default)]
struct AppState {
    connection: Mutex<VaultConnectionState>,
    search_tasks: Arc<Mutex<SearchIndexTaskRegistry>>,
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
struct ImportLiteratureRequest {
    preview: cistella_core::import::conflict::ImportPreview,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LocalSearchRequest {
    text: String,
    #[serde(default)]
    scopes: Vec<SearchFieldScope>,
    offset: Option<usize>,
    limit: Option<usize>,
}

/// Immutable WebView request snapshot for a Search task control action.
///
/// Task control has side effects, so it must not infer ownership from the
/// connection that happens to be current when a delayed IPC command arrives.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchIndexTaskControlRequest {
    expected_generation: u64,
    expected_vault_path: String,
}

/// Immutable WebView request snapshot for any Vault-side effect.
///
/// Notes and annotations are authoritative user data, so every write, controlled
/// open, and expensive PDF resolution must be gated by the generation + Vault
/// path that the WebView captured when the action was requested.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VaultRequestContext {
    expected_generation: u64,
    expected_vault_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppDirectoriesDto {
    config_dir: String,
    recent_vaults_path: String,
    cache_dir: String,
    is_portable_mode: bool,
    portable_root: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecentVaultDto {
    path: String,
    name: String,
    opened_at: Option<String>,
}

fn current_app_directories() -> CommandResult<AppDirectories> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe_dir = exe
        .parent()
        .ok_or_else(|| "failed to resolve executable directory".to_string())?;
    AppDirectories::from_exe_dir(exe_dir).map_err(|e| e.to_string())
}

fn app_directories_to_dto(dirs: &AppDirectories) -> CommandResult<AppDirectoriesDto> {
    Ok(AppDirectoriesDto {
        config_dir: dirs.config_dir().to_string_lossy().to_string(),
        recent_vaults_path: dirs.recent_vaults_path().to_string_lossy().to_string(),
        cache_dir: dirs.cache_dir().to_string_lossy().to_string(),
        is_portable_mode: dirs.is_portable_mode(),
        portable_root: dirs
            .portable_root()
            .map(|p| p.to_string_lossy().to_string()),
    })
}

fn recent_vault_from_dto(dto: RecentVaultDto) -> RecentVault {
    RecentVault {
        path: dto.path,
        name: dto.name,
        opened_at: dto.opened_at,
    }
}

fn recent_vault_to_dto(vault: RecentVault) -> RecentVaultDto {
    RecentVaultDto {
        path: vault.path,
        name: vault.name,
        opened_at: vault.opened_at,
    }
}

#[derive(Debug, Clone, Copy)]
enum SearchIndexTaskKind {
    Synchronize,
    Rebuild,
}

impl SearchIndexTaskKind {
    fn detail(self) -> &'static str {
        match self {
            Self::Synchronize => "synchronizing local search index",
            Self::Rebuild => "rebuilding local search index",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LiteratureItemRequest {
    title: String,
    authors: Vec<String>,
    published_year: Option<i32>,
    item_type: String,
    favorite: bool,
    reading_status: String,
    tags: Vec<String>,
    #[serde(default)]
    external_identifiers: Vec<ExternalIdentifier>,
}

fn parse_item_type(value: &str) -> CommandResult<LiteratureItemType> {
    match value {
        "article" => Ok(LiteratureItemType::Article),
        "book" => Ok(LiteratureItemType::Book),
        "chapter" => Ok(LiteratureItemType::Chapter),
        "other" => Ok(LiteratureItemType::Other),
        _ => Err(format!("Unsupported literature item type: {value}")),
    }
}

fn parse_reading_status(value: &str) -> CommandResult<ReadingStatus> {
    match value {
        "inbox" => Ok(ReadingStatus::Inbox),
        "reading" => Ok(ReadingStatus::Reading),
        "finished" => Ok(ReadingStatus::Finished),
        "archived" => Ok(ReadingStatus::Archived),
        _ => Err(format!("Unsupported reading status: {value}")),
    }
}

fn parse_document_asset_kind(value: &str) -> CommandResult<DocumentAssetKind> {
    match value {
        "primary" => Ok(DocumentAssetKind::Primary),
        "supplement" => Ok(DocumentAssetKind::Supplement),
        "version" => Ok(DocumentAssetKind::Version),
        "appendix" => Ok(DocumentAssetKind::Appendix),
        "other" => Ok(DocumentAssetKind::Other),
        _ => Err(format!("Unsupported document asset kind: {value}")),
    }
}

impl LiteratureItemRequest {
    fn draft(self) -> CommandResult<LiteratureItemDraft> {
        Ok(LiteratureItemDraft {
            title: self.title,
            authors: self.authors,
            published_year: self.published_year,
            item_type: parse_item_type(&self.item_type)?,
            favorite: self.favorite,
            reading_status: parse_reading_status(&self.reading_status)?,
            tags: self.tags,
            sources: Vec::new(),
            external_identifiers: self.external_identifiers,
        })
    }
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

fn vault_from_state(state: &tauri::State<AppState>) -> CommandResult<Vault> {
    let path = state
        .connection
        .lock()
        .map_err(|e| e.to_string())?
        .vault_path
        .clone()
        .ok_or_else(|| "No vault is connected".to_string())?;
    Vault::open_any(path, VaultOpenOptions::default()).map_err(|e| e.to_string())
}

fn idle_search_task_state() -> SearchIndexTaskState {
    SearchIndexTaskState {
        status: SearchIndexTaskStatus::Idle,
        generation_id: None,
        detail: None,
    }
}

fn current_search_task_owner(connection: &VaultConnectionState) -> Option<SearchIndexTaskOwner> {
    connection
        .vault_path
        .clone()
        .map(|vault_path| SearchIndexTaskOwner {
            connection_generation: connection.latest_connection_generation,
            vault_path,
        })
}

fn search_task_owner_for_request(
    connection: &VaultConnectionState,
    request: &SearchIndexTaskControlRequest,
) -> CommandResult<SearchIndexTaskOwner> {
    let Some(owner) = current_search_task_owner(connection) else {
        return Err("No vault is connected".to_string());
    };
    if owner.connection_generation != request.expected_generation
        || owner.vault_path != PathBuf::from(&request.expected_vault_path)
    {
        return Err("Stale local search task control request".to_string());
    }
    Ok(owner)
}

/// Validates that a mutable request still targets the current connection.
///
/// A delayed A→B or A→B→A request must fail before any Vault is opened or any
/// side effect is started.
fn validate_vault_request(
    connection: &VaultConnectionState,
    request: &VaultRequestContext,
) -> CommandResult<()> {
    let Some(ref path) = connection.vault_path else {
        return Err("No vault is connected".to_string());
    };
    if connection.latest_connection_generation != request.expected_generation
        || path != &PathBuf::from(&request.expected_vault_path)
    {
        return Err("Stale vault connection request".to_string());
    }
    Ok(())
}

/// Opens the current Vault while the connection lock is held through validation.
///
/// The closure receives an opened Vault; for write commands this keeps the
/// request bound to the connection generation/path that authorized it.
fn with_validated_vault<R>(
    state: &tauri::State<AppState>,
    request: &VaultRequestContext,
    f: impl FnOnce(Vault) -> CommandResult<R>,
) -> CommandResult<R> {
    let connection = state.connection.lock().map_err(|e| e.to_string())?;
    validate_vault_request(&connection, request)?;
    let path = connection
        .vault_path
        .clone()
        .expect("validated vault has path");
    let vault = Vault::open_any(&path, VaultOpenOptions::default()).map_err(|e| e.to_string())?;
    f(vault)
}

fn visible_search_task_state(state: &AppState) -> CommandResult<SearchIndexTaskState> {
    let connection = state.connection.lock().map_err(|e| e.to_string())?;
    let Some(owner) = current_search_task_owner(&connection) else {
        return Ok(idle_search_task_state());
    };
    let tasks = state.search_tasks.lock().map_err(|e| e.to_string())?;
    Ok(tasks
        .slots
        .get(&owner)
        .map(|task| task.state.clone())
        .unwrap_or_else(idle_search_task_state))
}

fn begin_search_index_task(
    tasks: &mut SearchIndexTaskRegistry,
    owner: &SearchIndexTaskOwner,
    task_id: uuid::Uuid,
    cancellation: Arc<AtomicBool>,
    task_state: SearchIndexTaskState,
) -> CommandResult<()> {
    let slot = tasks.slots.entry(owner.clone()).or_default();
    if slot.state.status == SearchIndexTaskStatus::Building {
        return Err(
            "A local search index task is already running for this vault connection".to_string(),
        );
    }
    slot.task_id = Some(task_id);
    slot.cancellation = Some(cancellation);
    slot.state = task_state;
    Ok(())
}

fn finish_search_index_task(
    tasks: &mut SearchIndexTaskRegistry,
    owner: &SearchIndexTaskOwner,
    task_id: uuid::Uuid,
    result: cistella_core::Result<cistella_core::SearchIndexState>,
    cancellation: &AtomicBool,
) {
    let Some(slot) = tasks.slots.get_mut(owner) else {
        return;
    };
    if slot.task_id != Some(task_id) {
        return;
    }
    let cancelled = cancellation.load(Ordering::SeqCst)
        || matches!(result, Err(CoreError::SearchIndexBuildCancelled));
    slot.cancellation = None;
    slot.state = if cancelled {
        SearchIndexTaskState {
            status: SearchIndexTaskStatus::Idle,
            generation_id: None,
            detail: Some("local search index task cancelled".to_string()),
        }
    } else {
        match result {
            Ok(index_state) => SearchIndexTaskState {
                status: SearchIndexTaskStatus::Succeeded,
                generation_id: index_state.active_generation,
                detail: None,
            },
            Err(error) => SearchIndexTaskState {
                status: SearchIndexTaskStatus::Failed,
                generation_id: None,
                detail: Some(error.to_string()),
            },
        }
    };
}

fn cancel_search_index_task_for_owner(
    tasks: &mut SearchIndexTaskRegistry,
    owner: &SearchIndexTaskOwner,
) -> SearchIndexTaskState {
    let Some(slot) = tasks.slots.get_mut(owner) else {
        return idle_search_task_state();
    };
    if slot.state.status == SearchIndexTaskStatus::Building {
        if let Some(cancellation) = &slot.cancellation {
            cancellation.store(true, Ordering::SeqCst);
        }
        slot.state.detail = Some("cancellation requested".to_string());
    }
    slot.state.clone()
}

fn cancel_search_index_task_for_request(
    state: &AppState,
    request: &SearchIndexTaskControlRequest,
) -> CommandResult<SearchIndexTaskState> {
    // Keep the connection lock through validation and task-slot selection. A
    // delayed command either targets its original connection or has no effect;
    // it can never be rebound to a newer connection between those operations.
    let connection = state.connection.lock().map_err(|e| e.to_string())?;
    let owner = search_task_owner_for_request(&connection, request)?;
    let mut tasks = state.search_tasks.lock().map_err(|e| e.to_string())?;
    Ok(cancel_search_index_task_for_owner(&mut tasks, &owner))
}

fn start_search_index_task(
    state: &AppState,
    request: &SearchIndexTaskControlRequest,
    kind: SearchIndexTaskKind,
) -> CommandResult<SearchIndexTaskState> {
    start_search_index_task_with_hooks(state, request, kind, || {}, || {})
}

/// Start control uses a prepare/commit protocol instead of holding the
/// connection mutex while reading the Vault manifest. Preparing the `Vault`
/// outside the mutex has no task-control side effect. The only linearization
/// point is the commit below: it reacquires the locks in the global
/// `connection -> search_tasks` order, revalidates the immutable IPC request,
/// selects the current owner, and occupies that owner's task slot as one
/// critical section. A connection change before commit therefore makes the
/// request stale and leaves no task slot or worker thread behind.
///
/// The hooks are no-ops in production. They make the pre-commit interleaving
/// directly testable without weakening the production synchronization.
fn start_search_index_task_with_hooks<BeforeCommit, BeforeWorkerStart>(
    state: &AppState,
    request: &SearchIndexTaskControlRequest,
    kind: SearchIndexTaskKind,
    before_commit: BeforeCommit,
    before_worker_start: BeforeWorkerStart,
) -> CommandResult<SearchIndexTaskState>
where
    BeforeCommit: FnOnce(),
    BeforeWorkerStart: FnOnce(),
{
    let prepared_owner = {
        let connection = state.connection.lock().map_err(|e| e.to_string())?;
        search_task_owner_for_request(&connection, request)?
    };
    let vault = Vault::open_any(&prepared_owner.vault_path, VaultOpenOptions::default())
        .map_err(|e| e.to_string())?;

    // No connection or task-registry lock is held here. A test (and a real
    // connection switch) can therefore advance the connection before commit.
    before_commit();

    let task_id = uuid::Uuid::new_v4();
    let cancellation = Arc::new(AtomicBool::new(false));
    let task_state = SearchIndexTaskState {
        status: SearchIndexTaskStatus::Building,
        generation_id: None,
        detail: Some(kind.detail().to_string()),
    };
    // All task-control paths acquire locks in this order. Retaining the
    // connection guard through both registry mutation and worker launch gives
    // this request one commit point: a newer connection cannot be selected
    // between request validation, owner selection, slot availability, slot
    // occupation, and `thread::spawn`.
    let connection = state.connection.lock().map_err(|e| e.to_string())?;
    let owner = search_task_owner_for_request(&connection, request)?;
    let mut tasks = state.search_tasks.lock().map_err(|e| e.to_string())?;
    begin_search_index_task(
        &mut tasks,
        &owner,
        task_id,
        cancellation.clone(),
        task_state.clone(),
    )?;

    before_worker_start();
    let task_registry = Arc::clone(&state.search_tasks);
    thread::spawn(move || {
        let result = match kind {
            SearchIndexTaskKind::Synchronize => {
                vault.reconcile_search_index_cancellable(|| cancellation.load(Ordering::SeqCst))
            }
            SearchIndexTaskKind::Rebuild => vault
                .rebuild_metadata_search_index_cancellable(|| cancellation.load(Ordering::SeqCst)),
        };
        let mut tasks = match task_registry.lock() {
            Ok(tasks) => tasks,
            Err(_) => return,
        };
        finish_search_index_task(&mut tasks, &owner, task_id, result, &cancellation);
    });
    drop(tasks);
    drop(connection);
    Ok(task_state)
}

/// Opens only a path that Core has resolved from a cistella item/asset pair.
/// The helper deliberately stays backend-only: the WebView never receives a
/// generic filesystem opener capability.
fn request_open_document_asset(app: &tauri::AppHandle, path: &Path) -> cistella_core::Result<()> {
    let path = path
        .to_str()
        .ok_or_else(|| CoreError::InvalidDocumentAssetPath(path.display().to_string()))?;
    app.opener()
        .open_path(path, None::<&str>)
        .map_err(|error| CoreError::Io(std::io::Error::other(error.to_string())))
}

#[tauri::command]
fn list_literature_items(state: tauri::State<AppState>) -> CommandResult<Value> {
    serde_json::to_value(
        vault_from_state(&state)?
            .load_literature_items()
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn create_literature_item(
    req: LiteratureItemRequest,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    serde_json::to_value(
        vault_from_state(&state)?
            .create_literature_item(req.draft()?)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn update_literature_item(
    item_id: String,
    req: LiteratureItemRequest,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    let id = uuid::Uuid::parse_str(&item_id).map_err(|e| e.to_string())?;
    let vault = vault_from_state(&state)?;
    let mut draft = req.draft()?;
    if draft.sources.is_empty() {
        draft.sources = vault
            .load_literature_items()
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|item| item.item_id == id)
            .map(|item| item.sources)
            .unwrap_or_default();
    }
    if draft.external_identifiers.is_empty() {
        draft.external_identifiers = vault
            .load_literature_items()
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|item| item.item_id == id)
            .map(|item| item.external_identifiers)
            .unwrap_or_default();
    }
    serde_json::to_value(
        vault
            .update_literature_item(id, draft)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn delete_literature_item(item_id: String, state: tauri::State<AppState>) -> CommandResult<Value> {
    let id = uuid::Uuid::parse_str(&item_id).map_err(|e| e.to_string())?;
    vault_from_state(&state)?
        .delete_literature_item(id)
        .map_err(|e| e.to_string())?;
    Ok(json!({ "itemId": item_id }))
}

#[tauri::command]
fn set_literature_item_favorite(
    item_id: String,
    favorite: bool,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    let id = uuid::Uuid::parse_str(&item_id).map_err(|e| e.to_string())?;
    serde_json::to_value(
        vault_from_state(&state)?
            .set_literature_item_favorite(id, favorite)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn set_literature_item_tags(
    item_id: String,
    tags: Vec<String>,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    let id = uuid::Uuid::parse_str(&item_id).map_err(|e| e.to_string())?;
    serde_json::to_value(
        vault_from_state(&state)?
            .set_literature_item_tags(id, tags)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn set_literature_item_reading_status(
    item_id: String,
    reading_status: String,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    let id = uuid::Uuid::parse_str(&item_id).map_err(|e| e.to_string())?;
    let status = parse_reading_status(&reading_status)?;
    serde_json::to_value(
        vault_from_state(&state)?
            .set_literature_item_reading_status(id, status)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn list_document_assets(state: tauri::State<AppState>) -> CommandResult<Value> {
    let vault = vault_from_state(&state)?;
    let entries = vault
        .load_document_assets()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|asset| {
            let status = asset.status(&vault);
            let mut value = serde_json::to_value(asset).map_err(|e| e.to_string())?;
            value
                .as_object_mut()
                .ok_or_else(|| "Document asset did not serialize as an object".to_string())?
                .insert(
                    "status".to_string(),
                    serde_json::to_value(status).map_err(|e| e.to_string())?,
                );
            Ok(value)
        })
        .collect::<CommandResult<Vec<_>>>()?;
    Ok(Value::Array(entries))
}

#[tauri::command]
fn import_document_asset(
    item_id: String,
    source_path: String,
    asset_kind: String,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    let item_id = uuid::Uuid::parse_str(&item_id).map_err(|e| e.to_string())?;
    serde_json::to_value(
        vault_from_state(&state)?
            .import_document_asset(
                item_id,
                PathBuf::from(source_path),
                parse_document_asset_kind(&asset_kind)?,
            )
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn link_external_document_asset(
    item_id: String,
    source_path: String,
    asset_kind: String,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    let item_id = uuid::Uuid::parse_str(&item_id).map_err(|e| e.to_string())?;
    serde_json::to_value(
        vault_from_state(&state)?
            .link_external_document_asset(
                item_id,
                PathBuf::from(source_path),
                parse_document_asset_kind(&asset_kind)?,
            )
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn set_document_asset_kind(
    item_id: String,
    asset_id: String,
    asset_kind: String,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    let item_id = uuid::Uuid::parse_str(&item_id).map_err(|e| e.to_string())?;
    let asset_id = uuid::Uuid::parse_str(&asset_id).map_err(|e| e.to_string())?;
    serde_json::to_value(
        vault_from_state(&state)?
            .set_document_asset_kind(item_id, asset_id, parse_document_asset_kind(&asset_kind)?)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn set_document_asset_default(
    item_id: String,
    asset_id: String,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    let item_id = uuid::Uuid::parse_str(&item_id).map_err(|e| e.to_string())?;
    let asset_id = uuid::Uuid::parse_str(&asset_id).map_err(|e| e.to_string())?;
    serde_json::to_value(
        vault_from_state(&state)?
            .set_document_asset_default(item_id, asset_id)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn remove_document_asset(
    item_id: String,
    asset_id: String,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    let item_id = uuid::Uuid::parse_str(&item_id).map_err(|e| e.to_string())?;
    let asset_id = uuid::Uuid::parse_str(&asset_id).map_err(|e| e.to_string())?;
    serde_json::to_value(
        vault_from_state(&state)?
            .remove_document_asset(item_id, asset_id)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn migrate_external_document_asset(
    item_id: String,
    asset_id: String,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    let item_id = uuid::Uuid::parse_str(&item_id).map_err(|e| e.to_string())?;
    let asset_id = uuid::Uuid::parse_str(&asset_id).map_err(|e| e.to_string())?;
    serde_json::to_value(
        vault_from_state(&state)?
            .migrate_external_document_asset(item_id, asset_id)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn open_document_asset(
    item_id: String,
    asset_id: String,
    state: tauri::State<AppState>,
    app: tauri::AppHandle,
) -> CommandResult<Value> {
    let item_id = uuid::Uuid::parse_str(&item_id).map_err(|e| e.to_string())?;
    let asset_id = uuid::Uuid::parse_str(&asset_id).map_err(|e| e.to_string())?;
    let path = vault_from_state(&state)?
        .resolve_document_asset_path(item_id, asset_id)
        .map_err(|e| e.to_string())?;
    let path = path.to_str().ok_or_else(|| {
        "Validated document asset path cannot be represented as Unicode".to_string()
    })?;
    app.opener()
        .open_path(path, None::<&str>)
        .map_err(|e| e.to_string())?;
    Ok(json!({ "itemId": item_id, "assetId": asset_id, "requestAccepted": true }))
}

#[tauri::command]
fn start_reading_session(
    item_id: String,
    asset_id: String,
    state: tauri::State<AppState>,
    app: tauri::AppHandle,
) -> CommandResult<Value> {
    let item_id = uuid::Uuid::parse_str(&item_id).map_err(|e| e.to_string())?;
    let asset_id = uuid::Uuid::parse_str(&asset_id).map_err(|e| e.to_string())?;
    let vault = vault_from_state(&state)?;
    let session = vault
        .start_reading_session(item_id, asset_id, |path| {
            request_open_document_asset(&app, path)
        })
        .map_err(|e| e.to_string())?;
    serde_json::to_value(session).map_err(|e| e.to_string())
}

#[tauri::command]
fn resume_reading_session(
    item_id: String,
    asset_id: String,
    state: tauri::State<AppState>,
    app: tauri::AppHandle,
) -> CommandResult<Value> {
    let item_id = uuid::Uuid::parse_str(&item_id).map_err(|e| e.to_string())?;
    let asset_id = uuid::Uuid::parse_str(&asset_id).map_err(|e| e.to_string())?;
    let vault = vault_from_state(&state)?;
    let session = vault
        .resume_reading_session(item_id, asset_id, |path| {
            request_open_document_asset(&app, path)
        })
        .map_err(|e| e.to_string())?;
    serde_json::to_value(session).map_err(|e| e.to_string())
}

#[tauri::command]
fn pause_reading_session(
    item_id: String,
    asset_id: String,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    let item_id = uuid::Uuid::parse_str(&item_id).map_err(|e| e.to_string())?;
    let asset_id = uuid::Uuid::parse_str(&asset_id).map_err(|e| e.to_string())?;
    serde_json::to_value(
        vault_from_state(&state)?
            .pause_reading_session(item_id, asset_id)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn end_reading_session(
    item_id: String,
    asset_id: String,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    let item_id = uuid::Uuid::parse_str(&item_id).map_err(|e| e.to_string())?;
    let asset_id = uuid::Uuid::parse_str(&asset_id).map_err(|e| e.to_string())?;
    serde_json::to_value(
        vault_from_state(&state)?
            .end_reading_session(item_id, asset_id)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn list_recent_reading_sessions(state: tauri::State<AppState>) -> CommandResult<Value> {
    serde_json::to_value(
        vault_from_state(&state)?
            .list_recent_reading_sessions()
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn continue_reading_target(state: tauri::State<AppState>) -> CommandResult<Value> {
    serde_json::to_value(
        vault_from_state(&state)?
            .continue_reading_target()
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn continue_reading_session(
    state: tauri::State<AppState>,
    app: tauri::AppHandle,
) -> CommandResult<Value> {
    let vault = vault_from_state(&state)?;
    let session = vault
        .continue_reading_session(|path| request_open_document_asset(&app, path))
        .map_err(|e| e.to_string())?;
    serde_json::to_value(session).map_err(|e| e.to_string())
}

#[tauri::command]
fn add_literature_vault_file(
    item_id: String,
    source_path: String,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    let item_id = uuid::Uuid::parse_str(&item_id).map_err(|e| e.to_string())?;
    serde_json::to_value(
        vault_from_state(&state)?
            .add_literature_vault_file(item_id, PathBuf::from(source_path))
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn add_literature_external_file(
    item_id: String,
    external_path: String,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    let item_id = uuid::Uuid::parse_str(&item_id).map_err(|e| e.to_string())?;
    serde_json::to_value(
        vault_from_state(&state)?
            .add_literature_external_file(item_id, PathBuf::from(external_path))
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn remove_literature_file(
    item_id: String,
    file_id: String,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    let item_id = uuid::Uuid::parse_str(&item_id).map_err(|e| e.to_string())?;
    let file_id = uuid::Uuid::parse_str(&file_id).map_err(|e| e.to_string())?;
    serde_json::to_value(
        vault_from_state(&state)?
            .remove_literature_file(item_id, file_id)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn set_literature_default_file(
    item_id: String,
    file_id: String,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    let item_id = uuid::Uuid::parse_str(&item_id).map_err(|e| e.to_string())?;
    let file_id = uuid::Uuid::parse_str(&file_id).map_err(|e| e.to_string())?;
    serde_json::to_value(
        vault_from_state(&state)?
            .set_literature_default_file(item_id, file_id)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn open_literature_file(
    item_id: String,
    file_id: String,
    state: tauri::State<AppState>,
    app: tauri::AppHandle,
) -> CommandResult<Value> {
    let item_id = uuid::Uuid::parse_str(&item_id).map_err(|e| e.to_string())?;
    let file_id = uuid::Uuid::parse_str(&file_id).map_err(|e| e.to_string())?;
    let path = vault_from_state(&state)?
        .resolve_literature_file_path(item_id, file_id)
        .map_err(|e| e.to_string())?;

    // `path` originates solely from core's item/file lookup and path validation.
    // The WebView never receives a generic opener capability or controls the target path.
    let path = path.to_str().ok_or_else(|| {
        "Validated literature file path cannot be represented as Unicode".to_string()
    })?;
    app.opener()
        .open_path(path, None::<&str>)
        .map_err(|e| e.to_string())?;

    // An accepted OS request is not evidence of reading and must not mutate Vault data.
    Ok(json!({ "itemId": item_id, "fileId": file_id, "requestAccepted": true }))
}

#[tauri::command]
fn inspect_literature_import(
    format: ImportFormat,
    bytes: Vec<u8>,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    let vault = vault_from_state(&state)?;
    let preview = vault
        .inspect_literature_import(format, &bytes)
        .map_err(|e| e.to_string())?;
    serde_json::to_value(preview).map_err(|e| e.to_string())
}

#[tauri::command]
fn import_literature_file(
    format: ImportFormat,
    req: ImportLiteratureRequest,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    let vault = vault_from_state(&state)?;
    let result = vault
        .commit_literature_import(format, req.preview)
        .map_err(|e| e.to_string())?;
    serde_json::to_value(result).map_err(|e| e.to_string())
}

#[tauri::command]
fn preview_openalex_work(
    record: SourceRecord,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    let vault = vault_from_state(&state)?;
    let preview = vault
        .preview_literature_records(ImportFormat::OpenAlexWorks, vec![record])
        .map_err(|e| e.to_string())?;
    serde_json::to_value(preview).map_err(|e| e.to_string())
}

#[tauri::command]
fn resolve_doi_local(
    raw_sources_dir: String,
    doi: String,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    let vault = vault_from_state(&state)?;
    let record = vault
        .resolve_doi_via_local_openalex(PathBuf::from(raw_sources_dir), &doi)
        .map_err(|e| e.to_string())?;
    serde_json::to_value(record).map_err(|e| e.to_string())
}

#[tauri::command]
fn list_local_openalex_works(
    raw_sources_dir: String,
    query: String,
    limit: usize,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    let vault = vault_from_state(&state)?;
    let records = vault
        .search_local_openalex_works(PathBuf::from(raw_sources_dir), &query, limit)
        .map_err(|e| e.to_string())?;
    serde_json::to_value(records).map_err(|e| e.to_string())
}

#[tauri::command]
fn local_search(req: LocalSearchRequest, state: tauri::State<AppState>) -> CommandResult<Value> {
    let query = SearchQuery {
        text: req.text,
        scopes: req.scopes,
    };
    let limit = req.limit.unwrap_or(DEFAULT_SEARCH_PAGE_SIZE);
    serde_json::to_value(
        vault_from_state(&state)?
            .query_search_index(&query, req.offset.unwrap_or(0), limit)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn local_search_index_state(state: tauri::State<AppState>) -> CommandResult<Value> {
    serde_json::to_value(vault_from_state(&state)?.search_index_state()).map_err(|e| e.to_string())
}

#[tauri::command]
fn local_search_index_issues(state: tauri::State<AppState>) -> CommandResult<Value> {
    let vault = vault_from_state(&state)?;
    let index_state = vault.search_index_state();
    if index_state.status != cistella_core::SearchIndexStatus::Ready {
        return Ok(json!({ "outcome": "unavailable", "indexState": index_state }));
    }
    let issues = vault.search_index_issues().map_err(|e| e.to_string())?;
    Ok(json!({ "outcome": "ready", "issues": issues }))
}

#[tauri::command]
fn local_search_task_state(state: tauri::State<AppState>) -> CommandResult<Value> {
    serde_json::to_value(visible_search_task_state(&state)?).map_err(|e| e.to_string())
}

#[tauri::command]
fn synchronize_local_search_index(
    req: SearchIndexTaskControlRequest,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    serde_json::to_value(start_search_index_task(
        &state,
        &req,
        SearchIndexTaskKind::Synchronize,
    )?)
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn rebuild_local_search_index(
    req: SearchIndexTaskControlRequest,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    serde_json::to_value(start_search_index_task(
        &state,
        &req,
        SearchIndexTaskKind::Rebuild,
    )?)
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn cancel_local_search_index_task(
    req: SearchIndexTaskControlRequest,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    serde_json::to_value(cancel_search_index_task_for_request(&state, &req)?)
        .map_err(|e| e.to_string())
}

fn note_to_value(note: &CoreNote) -> Value {
    json!({
        "noteId": note.note_id,
        "itemId": note.item_id,
        "createdAt": note.created_at.to_rfc3339(),
        "updatedAt": note.updated_at.to_rfc3339(),
        "archivedAt": note.archived_at.map(|dt| dt.to_rfc3339()),
        "title": note.title,
        "markdownBody": note.markdown_body,
        "revision": note.revision,
    })
}

/// Formats a note command error so the frontend can distinguish a revision
/// conflict from ordinary failures without relying on free-form message text.
fn note_command_error(error: CoreError) -> String {
    match error {
        CoreError::NoteConflict { note_id } => format!("NOTE_CONFLICT|{note_id}"),
        _ => error.to_string(),
    }
}

#[tauri::command]
fn list_notes(
    item_id: Option<String>,
    include_archived: bool,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    let item_id = item_id
        .map(|id| uuid::Uuid::parse_str(&id).map_err(|e| e.to_string()))
        .transpose()?;
    let vault = vault_from_state(&state)?;
    let notes = vault
        .list_notes(item_id, include_archived)
        .map_err(|e| e.to_string())?;
    Ok(Value::Array(notes.iter().map(note_to_value).collect()))
}

#[tauri::command]
fn get_note(note_id: String, state: tauri::State<AppState>) -> CommandResult<Value> {
    let id = uuid::Uuid::parse_str(&note_id).map_err(|e| e.to_string())?;
    let note = vault_from_state(&state)?
        .get_note(id)
        .map_err(|e| e.to_string())?;
    Ok(note_to_value(&note))
}

#[tauri::command]
fn create_note(
    item_id: String,
    title: String,
    markdown_body: String,
    req: VaultRequestContext,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    let item_id = uuid::Uuid::parse_str(&item_id).map_err(|e| e.to_string())?;
    with_validated_vault(&state, &req, |vault| {
        let note = vault
            .create_note(NoteDraft {
                item_id,
                title,
                markdown_body,
            })
            .map_err(|e| e.to_string())?;
        Ok(note_to_value(&note))
    })
}

#[tauri::command]
fn update_note(
    note_id: String,
    expected_revision: String,
    title: String,
    markdown_body: String,
    req: VaultRequestContext,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    let id = uuid::Uuid::parse_str(&note_id).map_err(|e| e.to_string())?;
    with_validated_vault(&state, &req, |vault| {
        let note = vault
            .update_note(id, &expected_revision, title, markdown_body)
            .map_err(note_command_error)?;
        Ok(note_to_value(&note))
    })
}

#[tauri::command]
fn archive_note(
    note_id: String,
    expected_revision: String,
    req: VaultRequestContext,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    let id = uuid::Uuid::parse_str(&note_id).map_err(|e| e.to_string())?;
    with_validated_vault(&state, &req, |vault| {
        let note = vault
            .archive_note(id, &expected_revision)
            .map_err(note_command_error)?;
        Ok(note_to_value(&note))
    })
}

#[tauri::command]
fn unarchive_note(
    note_id: String,
    expected_revision: String,
    req: VaultRequestContext,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    let id = uuid::Uuid::parse_str(&note_id).map_err(|e| e.to_string())?;
    with_validated_vault(&state, &req, |vault| {
        let note = vault
            .unarchive_note(id, &expected_revision)
            .map_err(note_command_error)?;
        Ok(note_to_value(&note))
    })
}

#[tauri::command]
fn list_annotations(
    item_id: Option<String>,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    let item_id = item_id
        .map(|id| uuid::Uuid::parse_str(&id).map_err(|e| e.to_string()))
        .transpose()?;
    let annotations = vault_from_state(&state)?
        .list_annotations(item_id)
        .map_err(|e| e.to_string())?;
    serde_json::to_value(annotations).map_err(|e| e.to_string())
}

#[tauri::command]
fn get_annotation(annotation_id: String, state: tauri::State<AppState>) -> CommandResult<Value> {
    let id = uuid::Uuid::parse_str(&annotation_id).map_err(|e| e.to_string())?;
    let annotation = vault_from_state(&state)?
        .get_annotation(id)
        .map_err(|e| e.to_string())?;
    serde_json::to_value(annotation).map_err(|e| e.to_string())
}

#[tauri::command]
fn create_annotation(
    item_id: String,
    asset_id: String,
    page_number: u32,
    selected_text: String,
    prefix_context: String,
    suffix_context: String,
    req: VaultRequestContext,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    let item_id = uuid::Uuid::parse_str(&item_id).map_err(|e| e.to_string())?;
    let asset_id = uuid::Uuid::parse_str(&asset_id).map_err(|e| e.to_string())?;
    with_validated_vault(&state, &req, |vault| {
        let annotation = vault
            .create_quote_annotation(
                item_id,
                asset_id,
                page_number,
                selected_text,
                prefix_context,
                suffix_context,
            )
            .map_err(|e| e.to_string())?;
        serde_json::to_value(annotation).map_err(|e| e.to_string())
    })
}

#[tauri::command]
fn delete_annotation(
    annotation_id: String,
    req: VaultRequestContext,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    let id = uuid::Uuid::parse_str(&annotation_id).map_err(|e| e.to_string())?;
    with_validated_vault(&state, &req, |vault| {
        vault.delete_annotation(id).map_err(|e| e.to_string())?;
        Ok(json!({ "annotationId": annotation_id }))
    })
}

#[tauri::command]
fn annotation_resolution(
    annotation_id: String,
    req: VaultRequestContext,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    let id = uuid::Uuid::parse_str(&annotation_id).map_err(|e| e.to_string())?;
    with_validated_vault(&state, &req, |vault| {
        let resolution = vault.resolve_annotation(id).map_err(|e| e.to_string())?;
        serde_json::to_value(resolution).map_err(|e| e.to_string())
    })
}

#[tauri::command]
fn open_annotation_asset(
    annotation_id: String,
    req: VaultRequestContext,
    state: tauri::State<AppState>,
    app: tauri::AppHandle,
) -> CommandResult<Value> {
    let id = uuid::Uuid::parse_str(&annotation_id).map_err(|e| e.to_string())?;
    with_validated_vault(&state, &req, |vault| {
        let annotation = vault.get_annotation(id).map_err(|e| e.to_string())?;
        let resolution = vault.resolve_annotation(id).map_err(|e| e.to_string())?;
        if resolution != AnnotationResolution::ResolvedExact {
            return Err(format!(
                "Annotation resolution is {resolution:?}; only resolved_exact can open the asset"
            ));
        }
        let session = vault
            .start_reading_session(annotation.item_id, annotation.asset_id, |path| {
                request_open_document_asset(&app, path)
            })
            .map_err(|e| e.to_string())?;
        serde_json::to_value(session).map_err(|e| e.to_string())
    })
}

#[tauri::command]
fn inspect_sources(raw_sources_dir: String) -> CommandResult<Value> {
    let adapter = OpenAlexSourcesAdapter;
    serde_json::to_value(
        adapter
            .inspect(PathBuf::from(raw_sources_dir).as_path())
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn import_sources(
    req: ImportSourcesRequest,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    let adapter = OpenAlexSourcesAdapter;
    adapter
        .import(
            PathBuf::from(&req.raw_sources_dir).as_path(),
            PathBuf::from(&req.output_dir).as_path(),
            req.build_arrow_cache,
        )
        .map_err(|e| e.to_string())?;
    let vault = Vault::open_any(PathBuf::from(&req.output_dir), VaultOpenOptions::default())
        .map_err(|e| e.to_string())?;
    state
        .connection
        .lock()
        .map_err(|e| e.to_string())?
        .vault_path = Some(PathBuf::from(req.output_dir));
    serde_json::to_value(vault.context()).map_err(|e| e.to_string())
}

fn stale_connection_error(generation: u64, latest_generation: u64) -> String {
    format!(
        "Stale vault connection generation {generation}; latest requested generation is {latest_generation}"
    )
}

fn register_connection_generation(
    connection: &mut VaultConnectionState,
    generation: u64,
) -> CommandResult<()> {
    if generation <= connection.latest_connection_generation {
        return Err(stale_connection_error(
            generation,
            connection.latest_connection_generation,
        ));
    }
    connection.latest_connection_generation = generation;
    Ok(())
}

fn commit_connected_vault(
    connection: &mut VaultConnectionState,
    generation: u64,
    path: PathBuf,
) -> CommandResult<()> {
    if generation != connection.latest_connection_generation {
        return Err(stale_connection_error(
            generation,
            connection.latest_connection_generation,
        ));
    }
    connection.vault_path = Some(path);
    Ok(())
}

#[tauri::command]
fn connect_vault(
    path: String,
    generation: u64,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    {
        let mut connection = state.connection.lock().map_err(|e| e.to_string())?;
        register_connection_generation(&mut connection, generation)?;
    }

    let vault = Vault::open_any(&path, VaultOpenOptions::default()).map_err(|e| e.to_string())?;
    let context = vault.context();
    {
        let mut connection = state.connection.lock().map_err(|e| e.to_string())?;
        commit_connected_vault(&mut connection, generation, PathBuf::from(path))?;
    }
    serde_json::to_value(context).map_err(|e| e.to_string())
}

#[tauri::command]
fn vault_overview(state: tauri::State<AppState>) -> CommandResult<Value> {
    vault_from_state(&state)?
        .overview_json()
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn vault_context(state: tauri::State<AppState>) -> CommandResult<Value> {
    let path = state
        .connection
        .lock()
        .map_err(|e| e.to_string())?
        .vault_path
        .clone()
        .ok_or_else(|| "No vault is connected".to_string())?;
    let vault =
        Vault::open_any(path.clone(), VaultOpenOptions::default()).map_err(|e| e.to_string())?;
    serde_json::to_value(vault.context()).map_err(|e| e.to_string())
}

#[tauri::command]
fn source_adapters() -> CommandResult<Value> {
    serde_json::to_value(available_source_adapters()).map_err(|e| e.to_string())
}

#[tauri::command]
fn app_directories() -> CommandResult<AppDirectoriesDto> {
    let dirs = current_app_directories()?;
    app_directories_to_dto(&dirs)
}

#[tauri::command]
fn is_portable_mode() -> CommandResult<bool> {
    Ok(current_app_directories()?.is_portable_mode())
}

fn tauri_conf_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tauri.conf.json")
}

fn update_json_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("update.json")
}

#[tauri::command]
fn get_app_version() -> CommandResult<String> {
    cistella_core::read_version_from_tauri_conf(&tauri_conf_path()).map_err(|e| e.to_string())
}

#[tauri::command]
fn check_update() -> CommandResult<Value> {
    let check = cistella_core::check_update(&tauri_conf_path(), &update_json_path())
        .map_err(|e| e.to_string())?;
    serde_json::to_value(check).map_err(|e| e.to_string())
}

#[tauri::command]
fn recent_vaults() -> CommandResult<Vec<RecentVaultDto>> {
    let dirs = current_app_directories()?;
    let vaults = load_recent_vaults(dirs.recent_vaults_path()).map_err(|e| e.to_string())?;
    let vaults = if dirs.is_portable_mode() {
        resolve_recent_vault_paths(dirs.portable_root().unwrap(), &vaults)
    } else {
        vaults
    };
    Ok(vaults.into_iter().map(recent_vault_to_dto).collect())
}

#[tauri::command]
fn update_recent_vaults(vaults: Vec<RecentVaultDto>) -> CommandResult<()> {
    let dirs = current_app_directories()?;
    let vaults: Vec<RecentVault> = vaults.into_iter().map(recent_vault_from_dto).collect();
    dirs.save_recent_vaults(&vaults).map_err(|e| e.to_string())
}

#[tauri::command]
fn migrate_vaults_to_portable(vaults: Vec<RecentVaultDto>) -> CommandResult<Vec<RecentVaultDto>> {
    let dirs = current_app_directories()?;
    let vaults: Vec<RecentVault> = vaults.into_iter().map(recent_vault_from_dto).collect();
    migrate_vaults_to_portable_core(&dirs, &vaults)
        .map_err(|e| e.to_string())
        .map(|migrated| migrated.into_iter().map(recent_vault_to_dto).collect())
}

#[tauri::command]
fn migrate_vaults_to_installed(vaults: Vec<RecentVaultDto>) -> CommandResult<Vec<RecentVaultDto>> {
    let dirs = current_app_directories()?;
    let vaults: Vec<RecentVault> = vaults.into_iter().map(recent_vault_from_dto).collect();
    migrate_vaults_to_installed_core(&dirs, &vaults)
        .map_err(|e| e.to_string())
        .map(|migrated| migrated.into_iter().map(recent_vault_to_dto).collect())
}

#[tauri::command]
fn backup_vault(vault_path: String, backup_path: String) -> CommandResult<()> {
    backup_vault_core(PathBuf::from(vault_path), PathBuf::from(backup_path))
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn restore_vault(backup_path: String, target_path: String) -> CommandResult<()> {
    restore_vault_core(PathBuf::from(backup_path), PathBuf::from(target_path))
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn search_sources(
    req: SearchSourcesRequest,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    vault_from_state(&state)?
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
fn top_sources(
    metric: String,
    source_type: String,
    limit: usize,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    vault_from_state(&state)?
        .top_sources_json(parse_metric(&metric), &source_type, limit)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn export_top_sources(
    output: String,
    metric: String,
    source_type: String,
    limit: usize,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    let vault = vault_from_state(&state)?;
    let output_path = PathBuf::from(output);
    let df = vault
        .top_sources_frame(parse_metric(&metric), &source_type, limit)
        .map_err(|e| e.to_string())?;
    export_dataframe(df, &output_path, ExportFormat::from_path(&output_path))
        .map_err(|e| e.to_string())?;
    Ok(json!({ "output": output_path.to_string_lossy(), "rows": limit }))
}

#[tauri::command]
fn export_search_sources(
    output: String,
    req: SearchSourcesRequest,
    state: tauri::State<AppState>,
) -> CommandResult<Value> {
    let vault = vault_from_state(&state)?;
    let output_path = PathBuf::from(output);
    let limit = req.limit.unwrap_or(1000);
    let df = vault
        .search_sources_frame(SourceSearchQuery {
            text: req.text,
            source_type: req.source_type,
            country_code: req.country_code,
            is_oa: req.is_oa,
            limit,
            offset: req.offset.unwrap_or(0),
        })
        .map_err(|e| e.to_string())?;
    export_dataframe(df, &output_path, ExportFormat::from_path(&output_path))
        .map_err(|e| e.to_string())?;
    Ok(json!({ "output": output_path.to_string_lossy(), "rows": limit }))
}

#[cfg(test)]
mod tests {
    use super::{
        AppState, SearchIndexTaskControlRequest, SearchIndexTaskKind, SearchIndexTaskOwner,
        SearchIndexTaskRegistry, VaultConnectionState, begin_search_index_task,
        cancel_search_index_task_for_owner, cancel_search_index_task_for_request,
        commit_connected_vault, current_search_task_owner, finish_search_index_task,
        register_connection_generation, start_search_index_task,
        start_search_index_task_with_hooks,
    };
    use cistella_core::{
        CoreError, SearchIndexState, SearchIndexStatus, SearchIndexTaskState,
        SearchIndexTaskStatus, VaultManifest, VaultSourceProvenance,
    };
    use std::{
        collections::BTreeMap,
        fs,
        path::{Path, PathBuf},
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc,
        },
        thread,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    fn owner(generation: u64, path: &str) -> SearchIndexTaskOwner {
        SearchIndexTaskOwner {
            connection_generation: generation,
            vault_path: PathBuf::from(path),
        }
    }

    fn building_task(detail: &str) -> SearchIndexTaskState {
        SearchIndexTaskState {
            status: SearchIndexTaskStatus::Building,
            generation_id: None,
            detail: Some(detail.to_string()),
        }
    }

    fn ready_index_state() -> SearchIndexState {
        SearchIndexState {
            status: SearchIndexStatus::Ready,
            active_generation: Some(uuid::Uuid::new_v4()),
            detail: None,
        }
    }

    #[test]
    fn stale_completion_cannot_overwrite_the_newer_connected_vault() {
        let mut connection = VaultConnectionState::default();
        register_connection_generation(&mut connection, 1).unwrap();
        register_connection_generation(&mut connection, 2).unwrap();
        commit_connected_vault(&mut connection, 2, PathBuf::from("vault-b")).unwrap();

        assert!(commit_connected_vault(&mut connection, 1, PathBuf::from("vault-a")).is_err());
        assert_eq!(connection.vault_path, Some(PathBuf::from("vault-b")));
        assert_eq!(connection.latest_connection_generation, 2);
    }

    #[test]
    fn search_task_control_plane_allows_b_while_a_runs_and_cancel_returns_only_b() {
        let owner_a = owner(1, "vault-a");
        let owner_b = owner(2, "vault-b");
        let mut tasks = SearchIndexTaskRegistry::default();
        let cancellation_a = Arc::new(AtomicBool::new(false));
        let cancellation_b = Arc::new(AtomicBool::new(false));
        let task_a = uuid::Uuid::new_v4();
        let task_b = uuid::Uuid::new_v4();

        begin_search_index_task(
            &mut tasks,
            &owner_a,
            task_a,
            cancellation_a.clone(),
            building_task("A synchronizing"),
        )
        .unwrap();
        // This is the same control-plane function used by the synchronize and
        // rebuild commands. An old Vault A task must not occupy Vault B's slot.
        begin_search_index_task(
            &mut tasks,
            &owner_b,
            task_b,
            cancellation_b.clone(),
            building_task("B rebuilding"),
        )
        .unwrap();

        let cancelled_b = cancel_search_index_task_for_owner(&mut tasks, &owner_b);
        assert_eq!(cancelled_b.status, SearchIndexTaskStatus::Building);
        assert_eq!(
            cancelled_b.detail.as_deref(),
            Some("cancellation requested")
        );
        assert!(cancellation_b.load(Ordering::SeqCst));
        assert!(!cancellation_a.load(Ordering::SeqCst));
        assert_eq!(
            tasks.slots.get(&owner_a).unwrap().state.status,
            SearchIndexTaskStatus::Building
        );

        finish_search_index_task(
            &mut tasks,
            &owner_a,
            task_a,
            Err(CoreError::SearchIndexBuildFailed("A failed".to_string())),
            &cancellation_a,
        );
        assert_eq!(
            tasks.slots.get(&owner_b).unwrap().state.status,
            SearchIndexTaskStatus::Building,
            "A completion must not write into B's currently-visible task slot"
        );

        finish_search_index_task(
            &mut tasks,
            &owner_b,
            task_b,
            Ok(ready_index_state()),
            &cancellation_b,
        );
        assert_eq!(
            tasks.slots.get(&owner_b).unwrap().state.status,
            SearchIndexTaskStatus::Idle,
            "a cancelled B task settles only B's own slot"
        );
    }

    #[test]
    fn a_to_b_to_a_old_task_terminal_states_never_pollute_or_block_current_generation() {
        let mut connection = VaultConnectionState::default();
        let mut tasks = SearchIndexTaskRegistry::default();
        let cancellation_a1 = Arc::new(AtomicBool::new(false));
        let cancellation_b = Arc::new(AtomicBool::new(false));
        let cancellation_a3 = Arc::new(AtomicBool::new(false));
        let task_a1 = uuid::Uuid::new_v4();
        let task_b = uuid::Uuid::new_v4();
        let task_a3 = uuid::Uuid::new_v4();

        register_connection_generation(&mut connection, 1).unwrap();
        commit_connected_vault(&mut connection, 1, PathBuf::from("vault-a")).unwrap();
        let owner_a1 = current_search_task_owner(&connection).unwrap();
        begin_search_index_task(
            &mut tasks,
            &owner_a1,
            task_a1,
            cancellation_a1.clone(),
            building_task("A1"),
        )
        .unwrap();

        register_connection_generation(&mut connection, 2).unwrap();
        commit_connected_vault(&mut connection, 2, PathBuf::from("vault-b")).unwrap();
        let owner_b = current_search_task_owner(&connection).unwrap();
        begin_search_index_task(
            &mut tasks,
            &owner_b,
            task_b,
            cancellation_b.clone(),
            building_task("B"),
        )
        .unwrap();

        register_connection_generation(&mut connection, 3).unwrap();
        commit_connected_vault(&mut connection, 3, PathBuf::from("vault-a")).unwrap();
        let owner_a3 = current_search_task_owner(&connection).unwrap();
        assert_ne!(
            owner_a1, owner_a3,
            "path reuse must not erase generation isolation"
        );
        begin_search_index_task(
            &mut tasks,
            &owner_a3,
            task_a3,
            cancellation_a3.clone(),
            building_task("A3"),
        )
        .unwrap();

        // Old A and B terminal feedback each updates its own historical slot,
        // leaving the current A3 control plane usable and unpolluted.
        finish_search_index_task(
            &mut tasks,
            &owner_a1,
            task_a1,
            Err(CoreError::SearchIndexBuildFailed(
                "old A failed".to_string(),
            )),
            &cancellation_a1,
        );
        finish_search_index_task(
            &mut tasks,
            &owner_b,
            task_b,
            Ok(ready_index_state()),
            &cancellation_b,
        );
        assert_eq!(
            tasks.slots.get(&owner_a3).unwrap().state.status,
            SearchIndexTaskStatus::Building
        );

        let cancelled_current = cancel_search_index_task_for_owner(&mut tasks, &owner_a3);
        assert_eq!(cancelled_current.status, SearchIndexTaskStatus::Building);
        assert!(cancellation_a3.load(Ordering::SeqCst));
        finish_search_index_task(
            &mut tasks,
            &owner_a3,
            task_a3,
            Err(CoreError::SearchIndexBuildCancelled),
            &cancellation_a3,
        );
        assert_eq!(
            tasks.slots.get(&owner_a3).unwrap().state.status,
            SearchIndexTaskStatus::Idle
        );
    }

    fn control_request(generation: u64, path: &str) -> SearchIndexTaskControlRequest {
        SearchIndexTaskControlRequest {
            expected_generation: generation,
            expected_vault_path: path.to_string(),
        }
    }

    fn connect_test_state(state: &AppState, generation: u64, path: &str) {
        let mut connection = state.connection.lock().unwrap();
        register_connection_generation(&mut connection, generation).unwrap();
        commit_connected_vault(&mut connection, generation, PathBuf::from(path)).unwrap();
    }

    fn temp_vault_root(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "{prefix}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn write_openable_test_vault(root: &Path) {
        fs::create_dir_all(root).unwrap();
        VaultManifest {
            format_version: "0.1.0".to_string(),
            vault_id: "desktop-search-task-race-test".to_string(),
            logical_schema_version: "0.1.0".to_string(),
            created_at: "2026-08-26T00:00:00Z".to_string(),
            source: VaultSourceProvenance {
                name: "Test source".to_string(),
                entity: "sources".to_string(),
                snapshot_date: Some("2026-08-26".to_string()),
                input_path: "input".to_string(),
            },
            tables: BTreeMap::new(),
        }
        .write_pretty(root.join("manifest.json"))
        .unwrap();
    }

    #[test]
    fn connection_switch_before_search_task_commit_leaves_old_request_without_slot_or_worker() {
        let vault_a = temp_vault_root("cistella-desktop-search-task-race");
        write_openable_test_vault(&vault_a);
        let state = Arc::new(AppState::default());
        let vault_a_text = vault_a.to_string_lossy().to_string();
        connect_test_state(&state, 1, &vault_a_text);

        let (prepared_tx, prepared_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let worker_started = Arc::new(AtomicBool::new(false));
        let start_state = Arc::clone(&state);
        let observed_worker_start = Arc::clone(&worker_started);
        let start_request = control_request(1, &vault_a_text);
        let start = thread::spawn(move || {
            start_search_index_task_with_hooks(
                &start_state,
                &start_request,
                SearchIndexTaskKind::Synchronize,
                move || {
                    // The implementation calls this after Vault::open_any but
                    // before the connection -> registry commit point.
                    prepared_tx.send(()).unwrap();
                    resume_rx.recv().unwrap();
                },
                move || observed_worker_start.store(true, Ordering::SeqCst),
            )
        });

        prepared_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        connect_test_state(&state, 2, "vault-b");
        let owner_b = {
            let connection = state.connection.lock().unwrap();
            current_search_task_owner(&connection).unwrap()
        };
        let cancellation_b = Arc::new(AtomicBool::new(false));
        {
            let mut tasks = state.search_tasks.lock().unwrap();
            begin_search_index_task(
                &mut tasks,
                &owner_b,
                uuid::Uuid::new_v4(),
                cancellation_b.clone(),
                building_task("B synchronizing"),
            )
            .unwrap();
        }

        resume_tx.send(()).unwrap();
        assert_eq!(
            start.join().unwrap().unwrap_err(),
            "Stale local search task control request"
        );
        assert!(
            !worker_started.load(Ordering::SeqCst),
            "a request made stale before commit must not start an index worker"
        );
        let owner_a = owner(1, &vault_a_text);
        let tasks = state.search_tasks.lock().unwrap();
        assert!(
            !tasks.slots.contains_key(&owner_a),
            "a request made stale before commit must not reserve or occupy A's task slot"
        );
        assert_eq!(
            tasks.slots[&owner_b].state.status,
            SearchIndexTaskStatus::Building,
            "the stale A request must not alter B's task control state"
        );
        assert!(
            !cancellation_b.load(Ordering::SeqCst),
            "the stale A request must not cancel B"
        );
        drop(tasks);
        fs::remove_dir_all(vault_a).unwrap();
    }

    #[test]
    fn delayed_a_cancel_command_cannot_cancel_b_task() {
        let state = AppState::default();
        connect_test_state(&state, 2, "vault-b");
        let owner_b = {
            let connection = state.connection.lock().unwrap();
            current_search_task_owner(&connection).unwrap()
        };
        let cancellation_b = Arc::new(AtomicBool::new(false));
        {
            let mut tasks = state.search_tasks.lock().unwrap();
            begin_search_index_task(
                &mut tasks,
                &owner_b,
                uuid::Uuid::new_v4(),
                cancellation_b.clone(),
                building_task("B synchronizing"),
            )
            .unwrap();
        }

        let error = cancel_search_index_task_for_request(&state, &control_request(1, "vault-a"))
            .unwrap_err();
        assert_eq!(error, "Stale local search task control request");
        assert!(
            !cancellation_b.load(Ordering::SeqCst),
            "an A cancel IPC command arriving after B connects must not cancel B"
        );
        assert_eq!(
            state.search_tasks.lock().unwrap().slots[&owner_b]
                .state
                .status,
            SearchIndexTaskStatus::Building
        );
    }

    #[test]
    fn delayed_a_synchronize_and_rebuild_commands_cannot_start_on_b() {
        let state = AppState::default();
        connect_test_state(&state, 2, "vault-b");
        let delayed_a = control_request(1, "vault-a");

        for kind in [
            SearchIndexTaskKind::Synchronize,
            SearchIndexTaskKind::Rebuild,
        ] {
            let error = start_search_index_task(&state, &delayed_a, kind).unwrap_err();
            assert_eq!(error, "Stale local search task control request");
        }
        assert!(
            state.search_tasks.lock().unwrap().slots.is_empty(),
            "stale start commands must fail before opening B or reserving B's task slot"
        );
    }

    #[test]
    fn a1_b2_a3_delayed_control_commands_cannot_affect_a3() {
        let state = AppState::default();
        connect_test_state(&state, 1, "vault-a");
        let delayed_a1 = control_request(1, "vault-a");
        connect_test_state(&state, 2, "vault-b");
        connect_test_state(&state, 3, "vault-a");
        let owner_a3 = {
            let connection = state.connection.lock().unwrap();
            current_search_task_owner(&connection).unwrap()
        };
        let cancellation_a3 = Arc::new(AtomicBool::new(false));
        {
            let mut tasks = state.search_tasks.lock().unwrap();
            begin_search_index_task(
                &mut tasks,
                &owner_a3,
                uuid::Uuid::new_v4(),
                cancellation_a3.clone(),
                building_task("A3 rebuilding"),
            )
            .unwrap();
        }

        for kind in [
            SearchIndexTaskKind::Synchronize,
            SearchIndexTaskKind::Rebuild,
        ] {
            assert_eq!(
                start_search_index_task(&state, &delayed_a1, kind).unwrap_err(),
                "Stale local search task control request"
            );
        }
        assert_eq!(
            cancel_search_index_task_for_request(&state, &delayed_a1).unwrap_err(),
            "Stale local search task control request"
        );
        assert!(!cancellation_a3.load(Ordering::SeqCst));
        assert_eq!(
            state.search_tasks.lock().unwrap().slots[&owner_a3]
                .state
                .status,
            SearchIndexTaskStatus::Building,
            "the older A1 request must not start, cancel, or mutate current A3"
        );
    }

    #[test]
    fn late_older_request_cannot_downgrade_the_latest_generation() {
        let mut connection = VaultConnectionState::default();
        register_connection_generation(&mut connection, 5).unwrap();

        assert!(register_connection_generation(&mut connection, 4).is_err());
        assert_eq!(connection.latest_connection_generation, 5);
        assert!(connection.vault_path.is_none());
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::default())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            list_literature_items,
            create_literature_item,
            update_literature_item,
            delete_literature_item,
            set_literature_item_favorite,
            set_literature_item_tags,
            set_literature_item_reading_status,
            list_document_assets,
            import_document_asset,
            link_external_document_asset,
            set_document_asset_kind,
            set_document_asset_default,
            remove_document_asset,
            migrate_external_document_asset,
            open_document_asset,
            start_reading_session,
            resume_reading_session,
            pause_reading_session,
            end_reading_session,
            list_recent_reading_sessions,
            continue_reading_target,
            continue_reading_session,
            add_literature_vault_file,
            add_literature_external_file,
            remove_literature_file,
            set_literature_default_file,
            open_literature_file,
            inspect_literature_import,
            import_literature_file,
            preview_openalex_work,
            resolve_doi_local,
            list_local_openalex_works,
            local_search,
            local_search_index_state,
            local_search_index_issues,
            local_search_task_state,
            synchronize_local_search_index,
            rebuild_local_search_index,
            cancel_local_search_index_task,
            list_notes,
            get_note,
            create_note,
            update_note,
            archive_note,
            unarchive_note,
            list_annotations,
            get_annotation,
            create_annotation,
            delete_annotation,
            annotation_resolution,
            open_annotation_asset,
            inspect_sources,
            import_sources,
            connect_vault,
            vault_overview,
            vault_context,
            source_adapters,
            app_directories,
            is_portable_mode,
            get_app_version,
            check_update,
            recent_vaults,
            update_recent_vaults,
            migrate_vaults_to_portable,
            migrate_vaults_to_installed,
            backup_vault,
            restore_vault,
            search_sources,
            top_sources,
            export_top_sources,
            export_search_sources
        ])
        .run(tauri::generate_context!())
        .expect("error while running cistella");
}
