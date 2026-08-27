use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use cistella_core::{
    DocumentAssetImportResult, DocumentAssetKind, DocumentAssetStatus, LiteratureItemDraft,
    LiteratureItemType, ReadingSessionState, ReadingStatus, Vault, VaultOpenOptions,
};
use uuid::Uuid;

fn temp_dir(prefix: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("{prefix}-{}", Uuid::new_v4()));
    fs::create_dir_all(&path).expect("create temporary directory");
    path
}

fn write_vault(root: &Path) -> Vault {
    fs::create_dir_all(root.join("tables")).expect("create source table directory");
    fs::write(
        root.join("tables/sources.parquet"),
        b"source table must remain outside reading-session writes",
    )
    .expect("write source table");
    fs::write(
        root.join("manifest.json"),
        r#"{
  "format_version": "0.1.0",
  "vault_id": "m4-reading-session-validation",
  "logical_schema_version": "0.1.0",
  "created_at": "2026-08-26T00:00:00Z",
  "source": {
    "name": "M4 reading-session integration test",
    "entity": "sources",
    "snapshot_date": null,
    "input_path": "fixtures/sources"
  },
  "tables": {
    "sources": {
      "rows": 0,
      "primary_key": ["openalex_id"],
      "parquet": { "path": "tables/sources.parquet", "size_bytes": 56 },
      "arrow": null
    }
  }
}"#,
    )
    .expect("write manifest");
    Vault::open(root, VaultOpenOptions::default()).expect("open minimal valid Vault")
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create copy destination");
    for entry in fs::read_dir(from).expect("read source tree") {
        let entry = entry.expect("read source tree entry");
        let destination = to.join(entry.file_name());
        if entry.file_type().expect("read source entry type").is_dir() {
            copy_tree(&entry.path(), &destination);
        } else {
            fs::copy(entry.path(), destination).expect("copy source file");
        }
    }
}

fn draft(title: &str) -> LiteratureItemDraft {
    LiteratureItemDraft {
        title: title.to_string(),
        authors: vec!["M4 reading-session integration".to_string()],
        published_year: Some(2026),
        item_type: LiteratureItemType::Article,
        favorite: false,
        reading_status: ReadingStatus::Inbox,
        tags: vec!["m4".to_string()],
        sources: Vec::new(),
        external_identifiers: Vec::new(),
    }
}

fn imported(result: DocumentAssetImportResult) -> cistella_core::DocumentAsset {
    match result {
        DocumentAssetImportResult::Imported { asset } => asset,
        other => panic!("expected imported Vault asset, got {other:?}"),
    }
}

fn write_pdf(path: &Path, content: &[u8]) {
    fs::write(path, [b"%PDF-1.4\n", content].concat()).expect("write test PDF");
}

const READING_SESSIONS_RELATIVE_PATH: &str = "user/reading_sessions.json";

fn vault_file_snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn collect(root: &Path, current: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(current).expect("read Vault tree") {
            let entry = entry.expect("read Vault tree entry");
            let path = entry.path();
            if entry
                .file_type()
                .expect("read Vault tree entry type")
                .is_dir()
            {
                collect(root, &path, files);
            } else {
                files.insert(
                    path.strip_prefix(root)
                        .expect("Vault file remains below root")
                        .to_path_buf(),
                    fs::read(&path).expect("read Vault file for byte snapshot"),
                );
            }
        }
    }

    let mut files = BTreeMap::new();
    collect(root, root, &mut files);
    files
}

fn assert_only_reading_sessions_file_may_change(
    root: &Path,
    before: &BTreeMap<PathBuf, Vec<u8>>,
    history_may_be_new: bool,
) {
    let history_path = PathBuf::from(READING_SESSIONS_RELATIVE_PATH);
    let after = vault_file_snapshot(root);
    let mut expected_paths = before.keys().cloned().collect::<BTreeSet<_>>();

    if history_may_be_new {
        assert!(
            !before.contains_key(&history_path),
            "the initial Vault snapshot must precede the first reading history write"
        );
        expected_paths.insert(history_path.clone());
    } else {
        assert!(
            before.contains_key(&history_path),
            "the copied Vault snapshot must include its existing reading history"
        );
    }

    assert_eq!(
        after.keys().cloned().collect::<BTreeSet<_>>(),
        expected_paths,
        "reading-session operations must neither add nor delete any path except the initial user/reading_sessions.json"
    );
    assert!(
        after.contains_key(&history_path),
        "an accepted reading-session operation must persist user/reading_sessions.json"
    );

    for (relative_path, expected_bytes) in before {
        if relative_path != &history_path {
            assert_eq!(
                after.get(relative_path),
                Some(expected_bytes),
                "reading-session operations must not rewrite {relative_path:?}; this protects literature metadata and ReadingStatus, Vault-owned PDFs, and every other pre-existing Vault file"
            );
        }
    }
}

#[test]
fn copied_vault_keeps_reading_history_portable_and_session_writes_stay_isolated() {
    let original_root = temp_dir("cistella-work-order-09-m4-original");
    let incoming_root = temp_dir("cistella-work-order-09-m4-incoming");
    let copied_root = temp_dir("cistella-work-order-09-m4-copy");

    let result = (|| {
        let vault = write_vault(&original_root);
        let item = vault
            .create_literature_item(draft("Portable reading session"))
            .expect("create literature item");
        let incoming_pdf = incoming_root.join("portable-reading.pdf");
        write_pdf(&incoming_pdf, b"portable reading content");
        let asset = imported(
            vault
                .import_document_asset(item.item_id, &incoming_pdf, DocumentAssetKind::Primary)
                .expect("import Vault PDF asset"),
        );

        let original_snapshot_before_sessions = vault_file_snapshot(&original_root);
        assert!(
            !original_snapshot_before_sessions
                .contains_key(&PathBuf::from(READING_SESSIONS_RELATIVE_PATH)),
            "reading history must not exist before the first accepted opener request"
        );

        let canonical_original_root =
            fs::canonicalize(&original_root).expect("canonicalize original Vault root");
        let started = vault
            .start_reading_session(item.item_id, asset.asset_id, |path| {
                assert!(path.is_file(), "Core must offer the Vault-owned PDF");
                assert!(path.starts_with(&canonical_original_root));
                Ok(())
            })
            .expect("start reading after accepted opener request");
        assert_eq!(started.item_id, item.item_id);
        assert_eq!(started.asset_id, asset.asset_id);
        assert_eq!(started.state, ReadingSessionState::Active);
        let paused = vault
            .pause_reading_session(item.item_id, asset.asset_id)
            .expect("pause active reading session");
        assert_eq!(paused.session_id, started.session_id);
        assert_eq!(paused.state, ReadingSessionState::Paused);

        let sessions_before_copy = fs::read(vault.reading_sessions_path())
            .expect("read persisted portable session history");
        assert_only_reading_sessions_file_may_change(
            &original_root,
            &original_snapshot_before_sessions,
            true,
        );

        copy_tree(&original_root, &copied_root);
        let copied_vault =
            Vault::open(&copied_root, VaultOpenOptions::default()).expect("open copied Vault");
        assert_eq!(
            fs::read(copied_vault.reading_sessions_path()).expect("read copied session JSON"),
            sessions_before_copy,
            "copying a Vault must preserve the portable ID-only session record byte-for-byte"
        );
        let copied_sessions = copied_vault
            .load_reading_sessions()
            .expect("load copied reading sessions");
        assert_eq!(copied_sessions.len(), 1);
        assert_eq!(copied_sessions[0].session_id, started.session_id);
        assert_eq!(copied_sessions[0].item_id, item.item_id);
        assert_eq!(copied_sessions[0].asset_id, asset.asset_id);
        assert_eq!(copied_sessions[0].state, ReadingSessionState::Paused);

        let copied_snapshot_before_resume = vault_file_snapshot(&copied_root);
        let canonical_copied_root =
            fs::canonicalize(&copied_root).expect("canonicalize copied Vault root");
        let resumed = copied_vault
            .resume_reading_session(item.item_id, asset.asset_id, |path| {
                assert!(path.is_file(), "copied Vault asset must remain readable");
                assert!(path.starts_with(&canonical_copied_root));
                assert!(
                    !path.starts_with(&canonical_original_root),
                    "copied session recovery must not use the original machine path"
                );
                Ok(())
            })
            .expect("resume copied reading session with accepted opener request");
        assert_eq!(resumed.session_id, started.session_id);
        assert_eq!(resumed.state, ReadingSessionState::Active);
        assert_eq!(
            copied_vault
                .list_recent_reading_sessions()
                .expect("list copied recent sessions")[0]
                .asset_status,
            DocumentAssetStatus::Available
        );
        assert_only_reading_sessions_file_may_change(
            &copied_root,
            &copied_snapshot_before_resume,
            false,
        );
        Ok::<(), String>(())
    })();

    let _ = fs::remove_dir_all(&original_root);
    let _ = fs::remove_dir_all(&incoming_root);
    let _ = fs::remove_dir_all(&copied_root);
    result.expect("M4 portable reading-session validation failed");
}

#[test]
fn unavailable_vault_and_external_assets_keep_session_history_without_continue_writes() {
    let root = temp_dir("cistella-work-order-09-m4-unavailable");
    let incoming_root = temp_dir("cistella-work-order-09-m4-unavailable-incoming");

    let result = (|| {
        let vault = write_vault(&root);
        let vault_item = vault
            .create_literature_item(draft("Missing Vault asset history"))
            .expect("create Vault asset item");
        let external_item = vault
            .create_literature_item(draft("Broken external asset history"))
            .expect("create external asset item");

        let incoming_pdf = incoming_root.join("will-be-missing.pdf");
        write_pdf(&incoming_pdf, b"will become missing");
        let vault_asset = imported(
            vault
                .import_document_asset(
                    vault_item.item_id,
                    &incoming_pdf,
                    DocumentAssetKind::Primary,
                )
                .expect("import Vault asset"),
        );
        let external_pdf = incoming_root.join("will-be-disconnected.pdf");
        write_pdf(&external_pdf, b"will become disconnected");
        let external_asset = vault
            .link_external_document_asset(
                external_item.item_id,
                &external_pdf,
                DocumentAssetKind::Primary,
            )
            .expect("link external asset");

        vault
            .start_reading_session(vault_item.item_id, vault_asset.asset_id, |_path| Ok(()))
            .expect("start Vault-backed reading session");
        vault
            .start_reading_session(external_item.item_id, external_asset.asset_id, |_path| {
                Ok(())
            })
            .expect("start externally linked reading session");
        let vault_asset_path = vault
            .resolve_document_asset_path(vault_item.item_id, vault_asset.asset_id)
            .expect("resolve Vault asset before removing it");
        fs::remove_file(vault_asset_path).expect("remove Vault asset after recording history");
        fs::remove_file(&external_pdf).expect("break external link after recording history");

        let summaries = vault
            .list_recent_reading_sessions()
            .expect("list unavailable session history");
        assert_eq!(summaries.len(), 2, "unavailable history must remain listed");
        assert!(summaries.iter().any(|summary| {
            summary.session.item_id == vault_item.item_id
                && summary.session.asset_id == vault_asset.asset_id
                && summary.asset_status == DocumentAssetStatus::Missing
        }));
        assert!(summaries.iter().any(|summary| {
            summary.session.item_id == external_item.item_id
                && summary.session.asset_id == external_asset.asset_id
                && summary.asset_status == DocumentAssetStatus::ExternalUnavailable
        }));

        let sessions_before_continue = fs::read(vault.reading_sessions_path())
            .expect("read history before unavailable continue request");
        assert!(
            vault
                .continue_reading_session(|_path| {
                    panic!("unavailable assets must not reach the opener callback")
                })
                .is_err(),
            "continue must fail when its retained history points to an unavailable asset"
        );
        assert_eq!(
            fs::read(vault.reading_sessions_path()).expect("read history after failed continue"),
            sessions_before_continue,
            "failed continue must not rewrite or delete retained unavailable history"
        );
        assert_eq!(
            vault
                .load_reading_sessions()
                .expect("reload retained unavailable history")
                .len(),
            2,
            "failed continue must preserve all session records"
        );
        Ok::<(), String>(())
    })();

    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&incoming_root);
    result.expect("M4 unavailable-session validation failed");
}
