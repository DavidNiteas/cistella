pub mod commands;

#[cfg(feature = "bin-test-frontend")]
use std::path::{Component, Path, PathBuf};

#[cfg(feature = "bin-test-frontend")]
use tauri::http;

#[cfg(feature = "bin-test-frontend")]
const BIN_TEST_FRONTEND_DIR: &str = "cistella-frontend";
#[cfg(feature = "bin-test-frontend")]
const BIN_TEST_FRONTEND_PROTOCOL: &str = "cistella-bin-test";

#[cfg(feature = "bin-test-frontend")]
fn bin_test_frontend_root() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe_dir = exe.parent()?;
    let root = exe_dir.join(BIN_TEST_FRONTEND_DIR);
    root.join("index.html").is_file().then_some(root)
}

#[cfg(feature = "bin-test-frontend")]
fn safe_frontend_path(root: &Path, request_path: &str) -> PathBuf {
    let relative = request_path.trim_start_matches('/');
    let relative = if relative.is_empty() {
        "index.html"
    } else {
        relative
    };
    let mut path = root.to_path_buf();
    for component in Path::new(relative).components() {
        if let Component::Normal(part) = component {
            path.push(part);
        }
    }
    if path.is_file() {
        path
    } else {
        root.join("index.html")
    }
}

#[cfg(feature = "bin-test-frontend")]
fn mime_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
    {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

#[cfg(feature = "bin-test-frontend")]
fn bin_test_frontend_response(request: http::Request<Vec<u8>>) -> http::Response<Vec<u8>> {
    let Some(root) = bin_test_frontend_root() else {
        return http::Response::builder()
            .status(http::StatusCode::NOT_FOUND)
            .header(http::header::CONTENT_TYPE, "text/plain; charset=utf-8")
            .body(b"bin_test frontend directory not found".to_vec())
            .expect("valid not-found response");
    };

    let path = safe_frontend_path(&root, request.uri().path());
    match std::fs::read(&path) {
        Ok(bytes) => http::Response::builder()
            .header(http::header::CONTENT_TYPE, mime_type(&path))
            .body(bytes)
            .expect("valid frontend asset response"),
        Err(error) => http::Response::builder()
            .status(http::StatusCode::INTERNAL_SERVER_ERROR)
            .header(http::header::CONTENT_TYPE, "text/plain; charset=utf-8")
            .body(format!("failed to read frontend asset: {error}").into_bytes())
            .expect("valid frontend asset error response"),
    }
}

#[cfg(feature = "bin-test-frontend")]
fn initial_frontend_url() -> tauri::Result<tauri::WebviewUrl> {
    if bin_test_frontend_root().is_none() {
        return Err(tauri::Error::InvalidWebviewUrl(
            "bin_test frontend directory missing; run build-bin-test.js",
        ));
    }
    let url = tauri::Url::parse(&format!(
        "{BIN_TEST_FRONTEND_PROTOCOL}://localhost/index.html"
    ))
    .map_err(|_| tauri::Error::InvalidWebviewUrl("invalid bin_test frontend url"))?;
    Ok(tauri::WebviewUrl::CustomProtocol(url))
}

#[cfg(not(feature = "bin-test-frontend"))]
fn initial_frontend_url() -> tauri::Result<tauri::WebviewUrl> {
    Ok(tauri::WebviewUrl::App("index.html".into()))
}

fn create_main_window(app: &tauri::App) -> tauri::Result<()> {
    let url = initial_frontend_url()?;
    tauri::WebviewWindowBuilder::new(app, "main", url)
        .title("cistella")
        .inner_size(1280.0, 820.0)
        .resizable(true)
        .build()?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // bin-test builds must NOT embed the real frontend: the whole point of the
    // pipeline is frontend/backend decoupling, and a stale embedded copy could
    // silently mask hot updates. Embed only a tiny stub instead; the real
    // frontend is served from disk via the cistella-bin-test protocol.
    #[cfg(feature = "bin-test-frontend")]
    let context = tauri::generate_context!("tauri.bin-test.conf.json");
    #[cfg(not(feature = "bin-test-frontend"))]
    let context = tauri::generate_context!();

    let builder = tauri::Builder::default();
    #[cfg(feature = "bin-test-frontend")]
    let builder = builder
        .register_uri_scheme_protocol(BIN_TEST_FRONTEND_PROTOCOL, |_ctx, request| {
            bin_test_frontend_response(request)
        });

    builder
        .manage(commands::AppState::default())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            create_main_window(app)?;
            Ok(())
        })
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            commands::list_literature_items,
            commands::create_literature_item,
            commands::update_literature_item,
            commands::delete_literature_item,
            commands::set_literature_item_favorite,
            commands::set_literature_item_tags,
            commands::set_literature_item_reading_status,
            commands::list_document_assets,
            commands::import_document_asset,
            commands::link_external_document_asset,
            commands::set_document_asset_kind,
            commands::set_document_asset_default,
            commands::remove_document_asset,
            commands::migrate_external_document_asset,
            commands::open_document_asset,
            commands::start_reading_session,
            commands::resume_reading_session,
            commands::pause_reading_session,
            commands::end_reading_session,
            commands::list_recent_reading_sessions,
            commands::continue_reading_target,
            commands::continue_reading_session,
            commands::add_literature_vault_file,
            commands::add_literature_external_file,
            commands::remove_literature_file,
            commands::set_literature_default_file,
            commands::open_literature_file,
            commands::inspect_literature_import,
            commands::import_literature_file,
            commands::preview_openalex_work,
            commands::resolve_remote_metadata,
            commands::import_by_identifier,
            commands::clear_remote_metadata_cache,
            commands::resolve_doi_local,
            commands::list_local_openalex_works,
            commands::local_search,
            commands::local_search_index_state,
            commands::local_search_index_issues,
            commands::local_search_task_state,
            commands::synchronize_local_search_index,
            commands::rebuild_local_search_index,
            commands::cancel_local_search_index_task,
            commands::list_notes,
            commands::get_note,
            commands::create_note,
            commands::update_note,
            commands::archive_note,
            commands::unarchive_note,
            commands::list_annotations,
            commands::get_annotation,
            commands::create_annotation,
            commands::delete_annotation,
            commands::annotation_resolution,
            commands::open_annotation_asset,
            commands::inspect_sources,
            commands::import_sources,
            commands::import_to_library,
            commands::connect_vault,
            commands::connect_library,
            commands::vault_overview,
            commands::library_overview,
            commands::vault_context,
            commands::library_context,
            commands::source_adapters,
            commands::app_directories,
            commands::is_portable_mode,
            commands::get_app_version,
            commands::check_update,
            commands::recent_vaults,
            commands::recent_libraries,
            commands::update_recent_vaults,
            commands::update_recent_libraries,
            commands::migrate_vaults_to_portable,
            commands::migrate_vaults_to_installed,
            commands::backup_vault,
            commands::backup_library,
            commands::workspace_contract,
            commands::library_contract,
            commands::inspect_legacy_vault,
            commands::migrate_legacy_vault_to_workspace,
            commands::restore_vault,
            commands::restore_library,
            commands::search_sources,
            commands::top_sources,
            commands::export_top_sources,
            commands::export_search_sources
        ])
        .run(context)
        .expect("error while running cistella");
}
