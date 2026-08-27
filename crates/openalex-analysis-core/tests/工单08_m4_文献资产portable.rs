use std::{
    fs,
    path::{Path, PathBuf},
};

use cistella_core::{
    DocumentAssetImportResult, DocumentAssetKind, DocumentAssetStatus, DocumentAssetStorageKind,
    LiteratureFileKind, LiteratureFileRef, LiteratureItem, LiteratureItemDraft, LiteratureItemType,
    ReadingStatus, Vault, VaultOpenOptions,
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
        b"source table must stay unchanged",
    )
    .expect("write source table");
    fs::write(
        root.join("manifest.json"),
        r#"{
  "format_version": "0.1.0",
  "vault_id": "m4-asset-validation",
  "logical_schema_version": "0.1.0",
  "created_at": "2026-08-26T00:00:00Z",
  "source": {
    "name": "M4 integration test",
    "entity": "sources",
    "snapshot_date": null,
    "input_path": "fixtures/sources"
  },
  "tables": {
    "sources": {
      "rows": 0,
      "primary_key": ["openalex_id"],
      "parquet": { "path": "tables/sources.parquet", "size_bytes": 32 },
      "arrow": null
    }
  }
}"#,
    )
    .expect("write manifest");
    Vault::open(root, VaultOpenOptions::default()).expect("open vault")
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create copy destination");
    for entry in fs::read_dir(from).expect("read source tree") {
        let entry = entry.expect("read source entry");
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
        authors: vec!["M4 asset integration".to_string()],
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
        other => panic!("expected an imported asset, got {other:?}"),
    }
}

fn vault_file_count(root: &Path, item_id: Uuid) -> usize {
    let item_dir = root.join("files").join(item_id.to_string());
    if !item_dir.exists() {
        return 0;
    }
    fs::read_dir(item_dir)
        .expect("read item asset directory")
        .count()
}

fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(root).expect("read directory while checking cleanup") {
        let entry = entry.expect("read entry while checking cleanup");
        let path = entry.path();
        if entry.file_type().expect("read entry type").is_dir() {
            files.extend(walk_files(&path));
        } else {
            files.push(path);
        }
    }
    files
}

fn assert_no_asset_temps(root: &Path) {
    let temporary_files = walk_files(root)
        .into_iter()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    (name.starts_with(".document_assets.")
                        || name.starts_with(".literature_items."))
                        && name.ends_with(".tmp")
                })
        })
        .collect::<Vec<_>>();
    assert!(
        temporary_files.is_empty(),
        "temporary user-data files remain: {temporary_files:?}"
    );
}

#[test]
fn document_assets_are_portable_while_external_assets_are_explicitly_nonportable() {
    let original_root = temp_dir("cistella-work-order-08-m4-original");
    let external_root = temp_dir("cistella-work-order-08-m4-external");
    let copied_root = temp_dir("cistella-work-order-08-m4-copy");

    let result = (|| {
        let vault = write_vault(&original_root);
        let source_table_before = fs::read(original_root.join("tables/sources.parquet"))
            .expect("read source table before asset operations");
        let manifest_before = fs::read(original_root.join("manifest.json"))
            .expect("read manifest before asset operations");
        let item = vault
            .create_literature_item(draft("Portable asset item"))
            .expect("create literature item");

        let incoming_pdf = external_root.join("incoming.pdf");
        fs::write(&incoming_pdf, b"portable asset bytes").expect("write import source");
        let vault_asset = imported(
            vault
                .import_document_asset(item.item_id, &incoming_pdf, DocumentAssetKind::Primary)
                .expect("import Vault asset"),
        );
        assert_eq!(vault_asset.storage_kind, DocumentAssetStorageKind::Vault);
        assert!(!Path::new(&vault_asset.path).is_absolute());

        let assets_before_duplicate = vault
            .load_document_assets()
            .expect("load assets before duplicate");
        let files_before_duplicate = vault_file_count(&original_root, item.item_id);
        match vault
            .import_document_asset(item.item_id, &incoming_pdf, DocumentAssetKind::Version)
            .expect("report same-item duplicate")
        {
            DocumentAssetImportResult::Duplicate { existing } => {
                assert_eq!(existing.asset_id, vault_asset.asset_id);
                assert_eq!(existing.asset_kind, DocumentAssetKind::Primary);
            }
            other => panic!("same-item hash must not create a second copy: {other:?}"),
        }
        assert_eq!(
            vault
                .load_document_assets()
                .expect("load assets after duplicate"),
            assets_before_duplicate,
            "same-item duplicate must not add an asset record"
        );
        assert_eq!(
            vault_file_count(&original_root, item.item_id),
            files_before_duplicate,
            "same-item duplicate must not copy a second Vault file"
        );

        let external_pdf = external_root.join("linked.pdf");
        fs::write(&external_pdf, b"external-only bytes").expect("write external source");
        let external_asset = vault
            .link_external_document_asset(
                item.item_id,
                &external_pdf,
                DocumentAssetKind::Supplement,
            )
            .expect("link external asset");
        assert_eq!(
            external_asset.storage_kind,
            DocumentAssetStorageKind::External
        );
        assert!(Path::new(&external_asset.path).is_absolute());
        assert!(!Path::new(&external_asset.path).starts_with(&original_root));

        assert_eq!(
            fs::read(original_root.join("tables/sources.parquet"))
                .expect("read source table after asset operations"),
            source_table_before,
            "DocumentAsset operations must not write Source table data"
        );
        assert_eq!(
            fs::read(original_root.join("manifest.json"))
                .expect("read manifest after asset operations"),
            manifest_before,
            "DocumentAsset operations must not write manifest.json"
        );
        let manifest_text = String::from_utf8(manifest_before).expect("manifest is UTF-8");
        assert!(
            !manifest_text.contains("document_assets") && !manifest_text.contains("DocumentAsset"),
            "manifest must not contain user asset metadata"
        );

        copy_tree(&original_root, &copied_root);
        let copied_vault =
            Vault::open(&copied_root, VaultOpenOptions::default()).expect("open copied Vault");
        let copied_assets = copied_vault
            .load_document_assets()
            .expect("load copied asset store");
        let copied_vault_asset = copied_assets
            .iter()
            .find(|asset| asset.asset_id == vault_asset.asset_id)
            .expect("find copied Vault asset");
        let copied_external_asset = copied_assets
            .iter()
            .find(|asset| asset.asset_id == external_asset.asset_id)
            .expect("find copied external asset");

        let copied_path = copied_vault
            .resolve_document_asset_path(item.item_id, copied_vault_asset.asset_id)
            .expect("copied Vault asset resolves");
        let copied_root_canonical =
            fs::canonicalize(&copied_root).expect("canonicalize copied root");
        assert!(copied_path.is_file());
        assert!(copied_path.starts_with(&copied_root_canonical));
        assert_eq!(
            copied_vault_asset.status(&copied_vault),
            DocumentAssetStatus::Available
        );

        assert_eq!(
            copied_external_asset.storage_kind,
            DocumentAssetStorageKind::External
        );
        assert!(Path::new(&copied_external_asset.path).is_absolute());
        assert!(!Path::new(&copied_external_asset.path).starts_with(&copied_root));
        assert_eq!(
            copied_external_asset.status(&copied_vault),
            DocumentAssetStatus::Available
        );
        fs::remove_file(&external_pdf).expect("break external link");
        assert_eq!(
            copied_external_asset.status(&copied_vault),
            DocumentAssetStatus::ExternalUnavailable,
            "a broken external path must be explicit rather than portable"
        );
        assert!(
            copied_vault
                .resolve_document_asset_path(item.item_id, copied_external_asset.asset_id)
                .is_err(),
            "broken external link must not resolve as a local asset"
        );

        fs::remove_file(&copied_path).expect("remove copied Vault PDF to simulate missing file");
        assert_eq!(
            copied_vault_asset.status(&copied_vault),
            DocumentAssetStatus::Missing,
            "missing Vault file must be visible without mutating the user record"
        );
        assert!(
            copied_vault
                .resolve_document_asset_path(item.item_id, copied_vault_asset.asset_id)
                .is_err(),
            "missing Vault file must not resolve"
        );
        assert_eq!(
            copied_vault
                .load_document_assets()
                .expect("reload copied records"),
            copied_assets,
            "runtime health checks must not rewrite user asset records"
        );
        assert_no_asset_temps(&copied_root);
        Ok::<(), String>(())
    })();

    let _ = fs::remove_dir_all(&original_root);
    let _ = fs::remove_dir_all(&external_root);
    let _ = fs::remove_dir_all(&copied_root);
    result.expect("M4 portable document-asset validation failed");
}

#[test]
fn legacy_json_projects_assets_without_creating_the_new_authoritative_file() {
    let root = temp_dir("cistella-work-order-08-m4-legacy");
    let result = (|| {
        let vault = write_vault(&root);
        let item_id = Uuid::new_v4();
        let file_id = Uuid::new_v4();
        let legacy_path = root
            .join("files")
            .join(item_id.to_string())
            .join("legacy.pdf");
        fs::create_dir_all(legacy_path.parent().expect("legacy asset parent"))
            .expect("create legacy asset parent");
        fs::write(&legacy_path, b"legacy bytes").expect("write legacy PDF");
        let legacy_item = LiteratureItem {
            item_id,
            title: "Legacy asset item".to_string(),
            authors: vec!["Legacy author".to_string()],
            published_year: Some(2026),
            item_type: LiteratureItemType::Article,
            favorite: false,
            reading_status: ReadingStatus::Inbox,
            tags: vec!["legacy".to_string()],
            sources: Vec::new(),
            files: vec![LiteratureFileRef {
                file_id,
                kind: LiteratureFileKind::Vault,
                path: format!("files/{item_id}/legacy.pdf"),
                display_name: "legacy.pdf".to_string(),
            }],
            default_file_id: Some(file_id),
            external_identifiers: Vec::new(),
        };
        vault
            .save_literature_items(std::slice::from_ref(&legacy_item))
            .expect("write legacy literature JSON");
        let legacy_json_before = fs::read(vault.literature_items_path()).expect("read legacy JSON");

        assert!(!vault.document_assets_path().exists());
        let assets = vault
            .load_document_assets()
            .expect("project legacy asset view");
        assert_eq!(assets.len(), 1);
        assert_eq!(
            assets[0].asset_id, file_id,
            "legacy ID is the stable compatibility asset ID"
        );
        assert_eq!(assets[0].item_id, item_id);
        assert_eq!(assets[0].storage_kind, DocumentAssetStorageKind::Vault);
        assert!(assets[0].is_default);
        assert_eq!(assets[0].status(&vault), DocumentAssetStatus::Available);
        assert!(
            vault.resolve_document_asset_path(item_id, file_id).is_ok(),
            "legacy projection must use the public asset resolver"
        );
        assert!(!vault.document_assets_path().exists());
        assert_eq!(
            fs::read(vault.literature_items_path()).expect("read legacy JSON after projection"),
            legacy_json_before,
            "read-only compatibility projection must not rewrite old JSON"
        );
        assert_no_asset_temps(&root);
        Ok::<(), String>(())
    })();

    let _ = fs::remove_dir_all(&root);
    result.expect("M4 legacy compatibility validation failed");
}

#[cfg(windows)]
#[test]
fn locked_asset_store_failure_cleans_the_new_copy_and_preserves_user_data() {
    let root = temp_dir("cistella-work-order-08-m4-locked-user-store");
    let external_root = temp_dir("cistella-work-order-08-m4-locked-source");
    let result = (|| {
        let vault = write_vault(&root);
        let item = vault
            .create_literature_item(draft("Failed asset-store publication"))
            .expect("create literature item");
        vault
            .save_document_assets(&[])
            .expect("create initial authoritative asset store");
        let user_json_before =
            fs::read(vault.document_assets_path()).expect("read original asset store");
        let item_json_before =
            fs::read(vault.literature_items_path()).expect("read original item store");
        let source = external_root.join("failure.pdf");
        fs::write(
            &source,
            b"this copy must be cleaned after publication failure",
        )
        .expect("write source PDF");

        let lock = fs::File::open(vault.document_assets_path())
            .expect("open asset store without delete sharing");
        let import_result =
            vault.import_document_asset(item.item_id, &source, DocumentAssetKind::Primary);
        drop(lock);

        assert!(
            import_result.is_err(),
            "locked asset store must reject atomic replacement"
        );
        assert_eq!(
            fs::read(vault.document_assets_path()).expect("read preserved asset store"),
            user_json_before,
            "failed publication must preserve prior user asset JSON"
        );
        assert_eq!(
            fs::read(vault.literature_items_path()).expect("read preserved item store"),
            item_json_before,
            "asset import must not rewrite literature item JSON"
        );
        assert!(
            vault
                .load_document_assets()
                .expect("load preserved assets")
                .is_empty(),
            "failed publication must not create an asset record"
        );
        assert_eq!(
            vault_file_count(&root, item.item_id),
            0,
            "failed publication must remove the copied Vault PDF"
        );
        assert_no_asset_temps(&root);
        Ok::<(), String>(())
    })();

    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&external_root);
    result.expect("M4 failure-cleanup validation failed");
}
