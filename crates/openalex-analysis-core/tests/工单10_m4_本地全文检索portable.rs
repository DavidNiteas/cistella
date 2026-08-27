use std::{
    cell::RefCell,
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use cistella_core::{
    CoreError, DERIVED_SEARCH_RELATIVE_DIR, DocumentAsset, DocumentAssetImportResult,
    DocumentAssetKind, DocumentAssetStatus, LiteratureItemDraft, LiteratureItemType, ReadingStatus,
    SearchFieldScope, SearchIndexStatus, SearchMatchField, SearchQuery, SearchQueryResult, Vault,
    VaultOpenOptions,
};
use lopdf::{
    Document, Object, Stream,
    content::{Content, Operation},
    dictionary,
};
use uuid::Uuid;

const PORTABLE_BODY: &str = "cistella m4 portable body token";
const REPLACED_BODY: &str = "cistella m4 replacement body token";
const ADDED_BODY: &str = "cistella m4 incremental added token";
const REMOVED_BODY: &str = "cistella m4 removed asset token";
const BROKEN_BODY: &str = "cistella m4 stale content ghost token";
const MISSING_BODY: &str = "cistella m4 missing content token";
const EXTERNAL_BODY: &str = "cistella m4 external content token";

#[derive(Debug, Clone, PartialEq, Eq)]
struct TreeEntry {
    is_dir: bool,
    bytes: Option<Vec<u8>>,
}

fn temp_dir(prefix: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("{prefix}-{}", Uuid::new_v4()));
    fs::create_dir_all(&path).expect("create temporary directory");
    path
}

fn write_vault(root: &Path) -> Vault {
    fs::create_dir_all(root.join("tables")).expect("create source table directory");
    fs::write(
        root.join("tables/sources.parquet"),
        b"M4 source table bytes must remain authority data",
    )
    .expect("write source table");
    fs::write(
        root.join("manifest.json"),
        r#"{
  "format_version": "0.1.0",
  "vault_id": "m4-local-search-validation",
  "logical_schema_version": "0.1.0",
  "created_at": "2026-08-26T00:00:00Z",
  "source": {
    "name": "Work order 10 M4 integration test",
    "entity": "sources",
    "snapshot_date": null,
    "input_path": "fixtures/sources"
  },
  "tables": {
    "sources": {
      "rows": 0,
      "primary_key": ["openalex_id"],
      "parquet": { "path": "tables/sources.parquet", "size_bytes": 45 },
      "arrow": null
    }
  }
}"#,
    )
    .expect("write manifest");
    Vault::open(root, VaultOpenOptions::default()).expect("open minimum Vault")
}

fn draft(title: &str) -> LiteratureItemDraft {
    LiteratureItemDraft {
        title: title.to_string(),
        authors: vec!["M4 Local Search Validation".to_string()],
        published_year: Some(2026),
        item_type: LiteratureItemType::Article,
        favorite: false,
        reading_status: ReadingStatus::Inbox,
        tags: vec!["m4".to_string(), "portable".to_string()],
        sources: Vec::new(),
        external_identifiers: Vec::new(),
    }
}

fn build_test_pdf(text: Option<&str>) -> Document {
    let mut document = Document::with_version("1.5");
    let pages_id = document.new_object_id();
    let font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
        "Encoding" => "WinAnsiEncoding",
    });
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });
    let operations = text.map_or_else(Vec::new, |text| {
        vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 12.into()]),
            Operation::new("Td", vec![50.into(), 700.into()]),
            Operation::new("Tj", vec![Object::string_literal(text)]),
            Operation::new("ET", vec![]),
        ]
    });
    let contents_id = document.add_object(Stream::new(
        dictionary! {},
        Content { operations }
            .encode()
            .expect("encode PDF operations"),
    ));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Resources" => resources_id,
        "Contents" => contents_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![page_id.into()],
            "Count" => 1,
        }),
    );
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    document.trailer.set("Root", catalog_id);
    document
}

fn write_test_pdf(path: &Path, text: Option<&str>) {
    build_test_pdf(text).save(path).expect("write test PDF");
}

fn import_pdf(vault: &Vault, item_id: Uuid, path: &Path) -> DocumentAsset {
    match vault
        .import_document_asset(item_id, path, DocumentAssetKind::Primary)
        .expect("import Vault PDF")
    {
        DocumentAssetImportResult::Imported { asset } => asset,
        DocumentAssetImportResult::Duplicate { .. } => panic!("unexpected duplicate test asset"),
    }
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create copy destination");
    for entry in fs::read_dir(from).expect("read source tree") {
        let entry = entry.expect("read source entry");
        let destination = to.join(entry.file_name());
        if entry.file_type().expect("source entry type").is_dir() {
            copy_tree(&entry.path(), &destination);
        } else {
            fs::copy(entry.path(), destination).expect("copy source file");
        }
    }
}

/// A full relative-path + bytes snapshot of every Vault path except the exact
/// Search-owned `derived/search/` subtree. The snapshot deliberately traverses
/// `derived/` so sibling engines remain protected authority-like data.
/// Directory entries are retained as well as file contents, so an index
/// operation cannot quietly create, delete, or replace a non-Search path.
fn snapshot_authority_tree(root: &Path) -> BTreeMap<PathBuf, TreeEntry> {
    fn visit(root: &Path, current: &Path, snapshot: &mut BTreeMap<PathBuf, TreeEntry>) {
        for entry in fs::read_dir(current).expect("read snapshot directory") {
            let entry = entry.expect("read snapshot entry");
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .expect("strip snapshot root")
                .to_path_buf();
            if relative.starts_with(DERIVED_SEARCH_RELATIVE_DIR) {
                continue;
            }
            let file_type = entry.file_type().expect("read snapshot entry type");
            if file_type.is_dir() {
                snapshot.insert(
                    relative.clone(),
                    TreeEntry {
                        is_dir: true,
                        bytes: None,
                    },
                );
                visit(root, &path, snapshot);
            } else {
                snapshot.insert(
                    relative,
                    TreeEntry {
                        is_dir: false,
                        bytes: Some(fs::read(&path).expect("read snapshot file")),
                    },
                );
            }
        }
    }

    let mut snapshot = BTreeMap::new();
    visit(root, root, &mut snapshot);
    snapshot
}

fn content_query(term: &str) -> SearchQuery {
    SearchQuery {
        text: term.to_string(),
        scopes: vec![SearchFieldScope::Content],
    }
}

fn title_query(term: &str) -> SearchQuery {
    SearchQuery {
        text: term.to_string(),
        scopes: vec![SearchFieldScope::Title],
    }
}

fn ready_hits(vault: &Vault, query: SearchQuery) -> Vec<cistella_core::SearchHit> {
    match vault
        .query_search_index(&query, 0, 100)
        .expect("query local Search index")
    {
        SearchQueryResult::Ready { page } => page.hits,
        SearchQueryResult::Unavailable { index_state } => {
            panic!("expected ready Search index, received {index_state:?}")
        }
    }
}

fn assert_content_hit(vault: &Vault, term: &str, item_id: Uuid, asset_id: Uuid) {
    let hits = ready_hits(vault, content_query(term));
    assert_eq!(hits.len(), 1, "expected one hit for {term}");
    assert_eq!(hits[0].item_id, item_id, "wrong item for {term}");
    assert!(hits[0].field_matches.iter().any(|field| {
        field.field == SearchMatchField::Content && field.asset_id == Some(asset_id)
    }));
}

fn assert_no_content_hit(vault: &Vault, term: &str) {
    assert!(
        ready_hits(vault, content_query(term)).is_empty(),
        "stale or ghost content remains searchable for {term}"
    );
}

fn assert_authority_unchanged(
    root: &Path,
    expected: &BTreeMap<PathBuf, TreeEntry>,
    operation: &str,
) {
    assert_eq!(
        snapshot_authority_tree(root),
        *expected,
        "{operation} changed an authority path outside derived/search"
    );
}

#[test]
fn m4_portable_rebuild_recovery_boundary_and_incremental_no_ghosts() {
    let original_root = temp_dir("cistella-work-order-10-m4-original");
    let copied_root = temp_dir("cistella-work-order-10-m4-copy");
    let incoming_root = temp_dir("cistella-work-order-10-m4-incoming");

    let result = (|| {
        let vault = write_vault(&original_root);
        let item = vault
            .create_literature_item(draft("M4 original metadata title"))
            .expect("create literature item");
        let primary_source = incoming_root.join("portable-primary.pdf");
        write_test_pdf(&primary_source, Some(PORTABLE_BODY));
        let primary = import_pdf(&vault, item.item_id, &primary_source);

        // A real reading-session record is authority data that Search must not
        // touch. The callback stands in for the desktop system opener and only
        // receives the already validated, controlled item + asset path.
        vault
            .start_reading_session(item.item_id, primary.asset_id, |path| {
                assert!(path.is_file());
                Ok(())
            })
            .expect("start reading session");
        fs::write(
            original_root.join("user/source-analysis.json"),
            b"source-analysis authority bytes",
        )
        .expect("write source analysis authority fixture");
        // `derived/` itself is not a Search-owned write blanket. This sibling
        // fixture must remain byte-identical across initial build, query,
        // reconciliation, and rebuild; only `derived/search/` is exempt.
        fs::create_dir_all(original_root.join("derived/other-engine"))
            .expect("create sibling derived engine fixture directory");
        fs::write(
            original_root.join("derived/other-engine/authority-like.json"),
            b"other-engine authority-like bytes",
        )
        .expect("write sibling derived engine fixture");

        let authority_before_index = snapshot_authority_tree(&original_root);
        let first_build_started = Instant::now();
        vault
            .rebuild_metadata_search_index()
            .expect("build initial local index");
        let first_build_elapsed = first_build_started.elapsed();
        assert_authority_unchanged(
            &original_root,
            &authority_before_index,
            "initial Search index build",
        );
        assert_content_hit(
            &vault,
            "portable body token",
            item.item_id,
            primary.asset_id,
        );
        let initial_hits = ready_hits(&vault, content_query("portable body token"));
        let query_started = Instant::now();
        let query_hits = ready_hits(&vault, content_query("portable body token"));
        let query_elapsed = query_started.elapsed();
        assert_eq!(query_hits, initial_hits);
        assert_authority_unchanged(&original_root, &authority_before_index, "Search query");

        // Copy the whole Vault, including derived/search. The copied generation
        // must remain ID-based, queryable, and resolve the copied PDF rather
        // than retaining any source Vault path.
        copy_tree(&original_root, &copied_root);
        let copied_vault =
            Vault::open(&copied_root, VaultOpenOptions::default()).expect("open copied Vault");
        assert_content_hit(
            &copied_vault,
            "portable body token",
            item.item_id,
            primary.asset_id,
        );
        let copied_generation_json = serde_json::to_string(
            &copied_vault
                .load_search_index_generation()
                .expect("load copied generation"),
        )
        .expect("serialize copied generation");
        assert!(!copied_generation_json.contains(&original_root.to_string_lossy().to_string()));
        assert!(!copied_generation_json.contains(&copied_root.to_string_lossy().to_string()));
        let canonical_copy_root = fs::canonicalize(&copied_root).expect("canonicalize copy root");
        let opened_copy_path = RefCell::new(None);
        copied_vault
            .start_reading_session(item.item_id, primary.asset_id, |path| {
                let canonical =
                    fs::canonicalize(path).expect("canonicalize controlled opener path");
                assert!(canonical.starts_with(&canonical_copy_root));
                *opened_copy_path.borrow_mut() = Some(canonical);
                Ok(())
            })
            .expect("open copied asset through controlled reading path");
        assert_eq!(
            opened_copy_path.into_inner(),
            Some(
                copied_vault
                    .resolve_document_asset_path(item.item_id, primary.asset_id)
                    .expect("resolve copied asset")
            )
        );

        // Every recovery case must leave the copied Vault usable for authority
        // operations and restore the same literature-level result after rebuild.
        fs::remove_dir_all(copied_root.join(DERIVED_SEARCH_RELATIVE_DIR))
            .expect("delete copied derived index");
        let copied_without_index = Vault::open(&copied_root, VaultOpenOptions::default())
            .expect("open copied Vault without search index");
        assert_eq!(
            copied_without_index.search_index_state().status,
            SearchIndexStatus::Missing
        );
        assert_eq!(
            copied_without_index
                .load_literature_items()
                .expect("read authority without index")[0]
                .item_id,
            item.item_id
        );
        let copied_authority_before_rebuild = snapshot_authority_tree(&copied_root);
        let recovery_started = Instant::now();
        copied_without_index
            .rebuild_metadata_search_index()
            .expect("rebuild deleted copied index");
        let recovery_elapsed = recovery_started.elapsed();
        assert_authority_unchanged(
            &copied_root,
            &copied_authority_before_rebuild,
            "rebuild after derived/search deletion",
        );
        assert_eq!(
            ready_hits(&copied_without_index, content_query("portable body token")),
            initial_hits
        );

        let copied_manifest = copied_root
            .join(DERIVED_SEARCH_RELATIVE_DIR)
            .join("manifest.json");
        fs::write(&copied_manifest, b"not valid search manifest JSON")
            .expect("corrupt copied search manifest");
        let malformed = Vault::open(&copied_root, VaultOpenOptions::default())
            .expect("open Vault with malformed index manifest");
        assert_eq!(
            malformed.search_index_state().status,
            SearchIndexStatus::Missing
        );
        assert_eq!(
            malformed
                .load_literature_items()
                .expect("authority reads after manifest corruption")[0]
                .item_id,
            item.item_id
        );
        let authority_before_manifest_rebuild = snapshot_authority_tree(&copied_root);
        malformed
            .rebuild_metadata_search_index()
            .expect("recover malformed manifest");
        assert_authority_unchanged(
            &copied_root,
            &authority_before_manifest_rebuild,
            "rebuild after manifest corruption",
        );
        assert_eq!(
            ready_hits(&malformed, content_query("portable body token")),
            initial_hits
        );

        let mut incompatible_manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&copied_manifest).expect("read rebuilt manifest"))
                .expect("parse rebuilt manifest");
        incompatible_manifest["formatVersion"] = serde_json::json!(999_u32);
        fs::write(
            &copied_manifest,
            serde_json::to_vec_pretty(&incompatible_manifest)
                .expect("serialize incompatible manifest"),
        )
        .expect("write incompatible manifest");
        let incompatible = Vault::open(&copied_root, VaultOpenOptions::default())
            .expect("open Vault with incompatible search version");
        assert_eq!(
            incompatible.search_index_state().status,
            SearchIndexStatus::Stale
        );
        assert_eq!(
            incompatible
                .load_literature_items()
                .expect("authority reads after incompatible version")[0]
                .item_id,
            item.item_id
        );
        incompatible
            .rebuild_metadata_search_index()
            .expect("rebuild incompatible index");
        assert_eq!(
            ready_hits(&incompatible, content_query("portable body token")),
            initial_hits
        );

        let interrupted_working = copied_root
            .join(DERIVED_SEARCH_RELATIVE_DIR)
            .join("working")
            .join(Uuid::new_v4().to_string());
        fs::create_dir_all(&interrupted_working).expect("create interrupted working generation");
        fs::write(
            interrupted_working.join("metadata.json"),
            b"uncommitted test payload",
        )
        .expect("write interrupted working payload");
        let with_working = Vault::open(&copied_root, VaultOpenOptions::default())
            .expect("open Vault with uncommitted working generation");
        assert_eq!(
            with_working.search_index_state().status,
            SearchIndexStatus::Ready
        );
        assert_eq!(
            ready_hits(&with_working, content_query("portable body token")),
            initial_hits
        );
        fs::remove_dir_all(interrupted_working).expect("remove synthetic working generation");

        // A cancelled replacement cannot publish partial derived data over the
        // known-ready generation. A first cancelled build has no failure marker
        // and remains visibly missing rather than pretending it is ready.
        let cancelled = vault.rebuild_metadata_search_index_cancellable(|| true);
        assert!(matches!(
            cancelled,
            Err(CoreError::SearchIndexBuildCancelled)
        ));
        assert_eq!(vault.search_index_state().status, SearchIndexStatus::Ready);
        assert_eq!(
            ready_hits(&vault, content_query("portable body token")),
            initial_hits
        );
        let fresh_cancel_root = temp_dir("cistella-work-order-10-m4-first-cancel");
        let fresh_cancel_vault = write_vault(&fresh_cancel_root);
        fresh_cancel_vault
            .create_literature_item(draft("M4 first cancellation"))
            .expect("create fresh cancellation item");
        assert!(matches!(
            fresh_cancel_vault.rebuild_metadata_search_index_cancellable(|| true),
            Err(CoreError::SearchIndexBuildCancelled)
        ));
        assert_eq!(
            fresh_cancel_vault.search_index_state().status,
            SearchIndexStatus::Missing
        );
        fs::remove_dir_all(fresh_cancel_root).expect("remove fresh cancellation Vault");

        // M2 incremental and reconciliation outcomes: no old body can remain
        // after a PDF replacement, deletion, failure, absence, or external link.
        let replacement_path = vault
            .resolve_document_asset_path(item.item_id, primary.asset_id)
            .expect("resolve primary Vault PDF");
        write_test_pdf(&replacement_path, Some(REPLACED_BODY));
        let authority_before_reconcile = snapshot_authority_tree(&original_root);
        vault
            .reconcile_search_index()
            .expect("reconcile PDF replacement");
        assert_authority_unchanged(
            &original_root,
            &authority_before_reconcile,
            "reconciliation after PDF replacement",
        );
        assert_no_content_hit(&vault, "portable body token");
        assert_content_hit(
            &vault,
            "replacement body token",
            item.item_id,
            primary.asset_id,
        );

        let added_source = incoming_root.join("added.pdf");
        write_test_pdf(&added_source, Some(ADDED_BODY));
        let added = import_pdf(&vault, item.item_id, &added_source);
        assert_content_hit(
            &vault,
            "incremental added token",
            item.item_id,
            added.asset_id,
        );
        vault
            .remove_document_asset(item.item_id, added.asset_id)
            .expect("remove added asset");
        assert_no_content_hit(&vault, "incremental added token");

        let mut updated_draft = draft("M4 changed metadata title");
        updated_draft.tags.push("metadata-change".to_string());
        vault
            .update_literature_item(item.item_id, updated_draft)
            .expect("incrementally update literature metadata");
        assert!(ready_hits(&vault, title_query("original metadata title")).is_empty());
        assert_eq!(
            ready_hits(&vault, title_query("changed metadata title"))[0].item_id,
            item.item_id
        );

        let auxiliary_item = vault
            .create_literature_item(draft("M4 deletion-only metadata"))
            .expect("create item scheduled for deletion");
        let removed_source = incoming_root.join("removed.pdf");
        write_test_pdf(&removed_source, Some(REMOVED_BODY));
        let removed_asset = import_pdf(&vault, auxiliary_item.item_id, &removed_source);
        assert_content_hit(
            &vault,
            "removed asset token",
            auxiliary_item.item_id,
            removed_asset.asset_id,
        );
        vault
            .delete_literature_item(auxiliary_item.item_id)
            .expect("delete literature item");
        assert_no_content_hit(&vault, "removed asset token");
        assert!(ready_hits(&vault, title_query("deletion-only metadata")).is_empty());

        let blank_source = incoming_root.join("blank.pdf");
        let broken_source = incoming_root.join("broken.pdf");
        let missing_source = incoming_root.join("missing.pdf");
        let external_source = incoming_root.join("external.pdf");
        write_test_pdf(&blank_source, None);
        write_test_pdf(&broken_source, Some(BROKEN_BODY));
        write_test_pdf(&missing_source, Some(MISSING_BODY));
        write_test_pdf(&external_source, Some(EXTERNAL_BODY));
        let blank = import_pdf(&vault, item.item_id, &blank_source);
        let broken = import_pdf(&vault, item.item_id, &broken_source);
        let missing = import_pdf(&vault, item.item_id, &missing_source);
        let external = vault
            .link_external_document_asset(
                item.item_id,
                &external_source,
                DocumentAssetKind::Supplement,
            )
            .expect("link external asset");
        fs::write(
            vault
                .resolve_document_asset_path(item.item_id, broken.asset_id)
                .expect("resolve broken fixture"),
            b"not a PDF anymore",
        )
        .expect("corrupt Vault PDF");
        fs::remove_file(
            vault
                .resolve_document_asset_path(item.item_id, missing.asset_id)
                .expect("resolve missing fixture"),
        )
        .expect("remove missing fixture");
        let authority_before_outcome_reconcile = snapshot_authority_tree(&original_root);
        vault
            .reconcile_search_index()
            .expect("reconcile per-asset outcomes");
        assert_authority_unchanged(
            &original_root,
            &authority_before_outcome_reconcile,
            "reconciliation with no-text, broken, missing, and external assets",
        );
        let records = vault
            .load_search_index_generation()
            .expect("load per-asset Search records")
            .asset_content_records;
        let states = records
            .iter()
            .map(|record| (record.asset_id, record.state))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            states[&blank.asset_id],
            cistella_core::AssetContentIndexState::NoText
        );
        assert_eq!(
            states[&broken.asset_id],
            cistella_core::AssetContentIndexState::ParseFailed
        );
        assert_eq!(
            states[&missing.asset_id],
            cistella_core::AssetContentIndexState::Missing
        );
        assert_eq!(
            states[&external.asset_id],
            cistella_core::AssetContentIndexState::ExternalUnavailable
        );
        assert_no_content_hit(&vault, "stale content ghost token");
        assert_no_content_hit(&vault, "missing content token");
        assert_no_content_hit(&vault, "external content token");
        assert!(
            ready_hits(&vault, title_query("changed metadata title"))
                .iter()
                .any(|hit| hit.item_id == item.item_id),
            "asset-level failures must leave document metadata searchable"
        );
        assert!(matches!(
            external.status(&vault),
            DocumentAssetStatus::Available
        ));

        let authority_before_explicit_rebuild = snapshot_authority_tree(&original_root);
        vault
            .rebuild_metadata_search_index()
            .expect("final full rebuild");
        assert_authority_unchanged(
            &original_root,
            &authority_before_explicit_rebuild,
            "final Search rebuild",
        );
        assert_no_content_hit(&vault, "stale content ghost token");
        assert_no_content_hit(&vault, "missing content token");
        assert_no_content_hit(&vault, "external content token");
        assert_content_hit(
            &vault,
            "replacement body token",
            item.item_id,
            primary.asset_id,
        );

        let source_bytes = fs::metadata(&primary_source)
            .expect("read fixed primary source metadata")
            .len();
        let derived_bytes = fs::read_dir(original_root.join(DERIVED_SEARCH_RELATIVE_DIR))
            .expect("read derived root")
            .flat_map(|entry| entry.expect("read derived entry").metadata())
            .filter_map(|metadata| metadata.is_file().then_some(metadata.len()))
            .sum::<u64>();
        println!(
            "M4 portable fixed sample: vaults=2, indexed_pdf_assets=1, source_bytes={source_bytes}, first_build_ms={}, copied_rebuild_ms={}, query_ms={}, derived_root_file_bytes={derived_bytes}",
            first_build_elapsed.as_millis(),
            recovery_elapsed.as_millis(),
            query_elapsed.as_millis(),
        );
        assert!(first_build_elapsed < Duration::from_secs(5));
        assert!(recovery_elapsed < Duration::from_secs(5));
        assert!(query_elapsed < Duration::from_secs(1));

        Ok::<(), String>(())
    })();

    let _ = fs::remove_dir_all(&original_root);
    let _ = fs::remove_dir_all(&copied_root);
    let _ = fs::remove_dir_all(&incoming_root);
    result.expect("work order 10 M4 portable/search validation failed");
}
