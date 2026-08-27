use std::{fs, path::PathBuf};

use cistella_core::import::conflict::find_matching_item;
use cistella_core::{
    ConflictPolicy, ExternalIdentifier, LiteratureItemDraft, LiteratureItemType, ReadingStatus,
    SearchFieldScope, SearchMatchField, SearchQuery, Vault, VaultOpenOptions,
};
use uuid::Uuid;

fn temp_vault_root(prefix: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("{prefix}-{}", Uuid::new_v4()));
    fs::create_dir_all(&path).unwrap();
    path
}

fn write_minimal_vault(root: &PathBuf) -> Vault {
    fs::write(
        root.join("manifest.json"),
        r#"{
  "format_version": "0.1.0",
  "vault_id": "work-order-12-m1",
  "logical_schema_version": "0.1.0",
  "created_at": "2026-08-26T00:00:00Z",
  "source": { "name": "test", "entity": "sources", "snapshot_date": null, "input_path": "fixture" },
  "tables": {}
}"#,
    )
    .unwrap();
    fs::write(root.join("sources.parquet"), b"").unwrap();
    Vault::open_sources_file(root.join("sources.parquet"), VaultOpenOptions::default()).unwrap()
}

fn sample_bib() -> &'static [u8] {
    br#"
@article{sample2023,
  title = {Sample Article},
  author = {Alice Author and Bob Writer},
  year = {2023},
  doi = {10.1000/sample},
}

@book{another2022,
  title = {Another Book},
  author = {Carol Editor},
  year = {2022},
  isbn = {978-3-030-00000-0},
}
"#
}

fn sample_bib_duplicate_doi() -> &'static [u8] {
    br#"
@article{sample2023v2,
  title = {Updated Sample Article},
  author = {Alice Author and Bob Writer and Charlie Contributor},
  year = {2023},
  doi = {10.1000/sample},
}
"#
}

#[test]
fn empty_vault_import_creates_new_items() {
    let root = temp_vault_root("cistella-m1-empty");
    let vault = write_minimal_vault(&root);

    let preview = vault.inspect_bibtex_import(sample_bib()).unwrap();
    assert_eq!(preview.items.len(), 2);
    assert!(preview.items.iter().all(|i| i.matched_item_id.is_none()));

    let result = vault.commit_bibtex_import(preview).unwrap();
    assert_eq!(result.created, 2);
    assert_eq!(result.merged, 0);
    assert_eq!(result.skipped, 0);

    let items = vault.load_literature_items().unwrap();
    assert_eq!(items.len(), 2);
    assert!(items.iter().any(|i| i.title == "Sample Article"));
    assert!(items.iter().any(|i| i.title == "Another Book"));

    let sample = items.iter().find(|i| i.title == "Sample Article").unwrap();
    assert_eq!(sample.authors, vec!["Alice Author", "Bob Writer"]);
    assert_eq!(sample.published_year, Some(2023));
    assert_eq!(sample.item_type, LiteratureItemType::Article);
    assert_eq!(sample.reading_status, ReadingStatus::Inbox);
    assert!(!sample.favorite);
    assert!(sample.tags.is_empty());
    assert!(
        sample
            .external_identifiers
            .iter()
            .any(|id| id.namespace == "doi" && id.value == "10.1000/sample")
    );
    assert_eq!(sample.sources.len(), 1);
    assert_eq!(sample.sources[0].source_name, "BibTeX");

    let batches = vault.load_source_batches().unwrap();
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].records.len(), 2);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn duplicate_import_merge_appends_source_ref() {
    let root = temp_vault_root("cistella-m1-merge");
    let vault = write_minimal_vault(&root);

    let first = vault.inspect_bibtex_import(sample_bib()).unwrap();
    vault.commit_bibtex_import(first).unwrap();

    let second = vault
        .inspect_bibtex_import(sample_bib_duplicate_doi())
        .unwrap();
    assert_eq!(second.items.len(), 1);
    assert!(second.items[0].matched_item_id.is_some());

    let result = vault.commit_bibtex_import(second).unwrap();
    assert_eq!(result.created, 0);
    assert_eq!(result.merged, 1);

    let items = vault.load_literature_items().unwrap();
    assert_eq!(items.len(), 2);
    let sample = items.iter().find(|i| i.title == "Sample Article").unwrap();
    assert_eq!(sample.sources.len(), 2);
    assert_eq!(sample.authors, vec!["Alice Author", "Bob Writer"]);

    let batches = vault.load_source_batches().unwrap();
    assert_eq!(batches.len(), 2);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn conflict_skip_strategy_skips_record() {
    let root = temp_vault_root("cistella-m1-skip");
    let vault = write_minimal_vault(&root);

    let first = vault.inspect_bibtex_import(sample_bib()).unwrap();
    vault.commit_bibtex_import(first).unwrap();

    let mut second = vault
        .inspect_bibtex_import(sample_bib_duplicate_doi())
        .unwrap();
    second.items[0].selected_policy = ConflictPolicy::Skip;

    let result = vault.commit_bibtex_import(second).unwrap();
    assert_eq!(result.skipped, 1);
    assert_eq!(result.created, 0);
    assert_eq!(result.merged, 0);

    let items = vault.load_literature_items().unwrap();
    let sample = items.iter().find(|i| i.title == "Sample Article").unwrap();
    assert_eq!(sample.sources.len(), 1);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn conflict_create_strategy_forces_new_item() {
    let root = temp_vault_root("cistella-m1-create");
    let vault = write_minimal_vault(&root);

    let first = vault.inspect_bibtex_import(sample_bib()).unwrap();
    vault.commit_bibtex_import(first).unwrap();

    let mut second = vault
        .inspect_bibtex_import(sample_bib_duplicate_doi())
        .unwrap();
    second.items[0].selected_policy = ConflictPolicy::Create;

    let result = vault.commit_bibtex_import(second).unwrap();
    assert_eq!(result.created, 1);
    assert_eq!(result.merged, 0);

    let items = vault.load_literature_items().unwrap();
    assert_eq!(items.len(), 3);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn commit_failure_restores_source_batches() {
    let root = temp_vault_root("cistella-m1-rollback");
    let vault = write_minimal_vault(&root);

    let preview = vault.inspect_bibtex_import(sample_bib()).unwrap();

    // Make literature_items.json unwritable by creating a directory at that path.
    fs::create_dir_all(vault.literature_items_path()).unwrap();

    let result = vault.commit_bibtex_import(preview);
    assert!(result.is_err());

    // The new batch file must not exist because there was no original to restore.
    let batches = vault.load_source_batches().unwrap();
    assert!(
        batches.is_empty(),
        "batch must be rolled back after failed commit"
    );

    // Clean up: remove the directory so temp cleanup can proceed.
    let _ = fs::remove_dir_all(vault.literature_items_path());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn malformed_bib_returns_structured_error() {
    let root = temp_vault_root("cistella-m1-malformed");
    let vault = write_minimal_vault(&root);

    let err = vault
        .inspect_bibtex_import(b"@article{key, title = {no close brace")
        .unwrap_err();
    assert!(err.to_string().contains("invalid literature import"));

    let err = vault.inspect_bibtex_import(b"not a bib file").unwrap_err();
    assert!(err.to_string().contains("invalid literature import"));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn source_analysis_tables_and_manifest_untouched() {
    let root = temp_vault_root("cistella-m1-isolation");
    let vault = write_minimal_vault(&root);

    let manifest_before = fs::read_to_string(root.join("manifest.json")).unwrap();
    vault
        .commit_bibtex_import(vault.inspect_bibtex_import(sample_bib()).unwrap())
        .unwrap();
    let manifest_after = fs::read_to_string(root.join("manifest.json")).unwrap();

    assert_eq!(manifest_before, manifest_after);
    assert!(!root.join("parquet").exists());
    assert!(vault.sources_dir().exists());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn user_sources_only_contains_batches_and_literature_items_updated() {
    let root = temp_vault_root("cistella-m1-layout");
    let vault = write_minimal_vault(&root);

    vault
        .commit_bibtex_import(vault.inspect_bibtex_import(sample_bib()).unwrap())
        .unwrap();

    let sources_dir = vault.sources_dir();
    assert!(sources_dir.exists());
    let entries: Vec<_> = fs::read_dir(&sources_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].path().extension().and_then(|e| e.to_str()) == Some("json"));

    assert!(vault.literature_items_path().exists());
    let items = vault.load_literature_items().unwrap();
    assert_eq!(items.len(), 2);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn merge_only_fills_empty_fields_and_preserves_user_state() {
    let root = temp_vault_root("cistella-m1-merge-respect");
    let vault = write_minimal_vault(&root);

    // Create an existing item with the same DOI but custom user fields.
    let mut item = cistella_core::LiteratureItem::from_draft(cistella_core::LiteratureItemDraft {
        title: "Existing Title".to_string(),
        authors: vec!["Existing Author".to_string()],
        published_year: Some(2000),
        item_type: LiteratureItemType::Book,
        favorite: true,
        reading_status: ReadingStatus::Finished,
        tags: vec!["user-tag".to_string()],
        sources: vec![],
        external_identifiers: vec![ExternalIdentifier {
            namespace: "doi".to_string(),
            value: "10.1000/sample".to_string(),
        }],
    });
    item.published_year = None; // Leave year empty to be filled.
    vault.save_literature_items(&[item.clone()]).unwrap();

    let preview = vault.inspect_bibtex_import(sample_bib()).unwrap();
    let matched = preview
        .items
        .iter()
        .find(|i| i.source_record.title == "Sample Article")
        .unwrap();
    assert_eq!(matched.matched_item_id, Some(item.item_id));

    vault.commit_bibtex_import(preview).unwrap();

    let items = vault.load_literature_items().unwrap();
    let updated = items.iter().find(|i| i.item_id == item.item_id).unwrap();
    assert_eq!(updated.title, "Existing Title");
    assert_eq!(updated.authors, vec!["Existing Author"]);
    assert_eq!(updated.published_year, Some(2023)); // filled because empty
    assert_eq!(updated.item_type, LiteratureItemType::Book);
    assert!(updated.favorite);
    assert_eq!(updated.reading_status, ReadingStatus::Finished);
    assert_eq!(updated.tags, vec!["user-tag"]);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn editing_imported_item_preserves_external_identifiers() {
    let root = temp_vault_root("cistella-m1-edit-preserve");
    let vault = write_minimal_vault(&root);

    vault
        .commit_bibtex_import(vault.inspect_bibtex_import(sample_bib()).unwrap())
        .unwrap();

    let items = vault.load_literature_items().unwrap();
    let sample = items
        .iter()
        .find(|i| i.title == "Sample Article")
        .unwrap()
        .clone();
    assert!(
        sample
            .external_identifiers
            .iter()
            .any(|id| id.namespace == "doi" && id.value == "10.1000/sample")
    );

    // Simulate a UI edit that does not send external_identifiers back.
    let draft = LiteratureItemDraft {
        title: "Updated Sample Article".to_string(),
        authors: sample.authors.clone(),
        published_year: sample.published_year,
        item_type: sample.item_type,
        favorite: sample.favorite,
        reading_status: sample.reading_status,
        tags: sample.tags.clone(),
        sources: sample.sources.clone(),
        external_identifiers: Vec::new(),
    };
    vault.update_literature_item(sample.item_id, draft).unwrap();

    let items = vault.load_literature_items().unwrap();
    let updated = items.iter().find(|i| i.item_id == sample.item_id).unwrap();
    assert_eq!(updated.title, "Updated Sample Article");
    assert!(
        updated
            .external_identifiers
            .iter()
            .any(|id| id.namespace == "doi" && id.value == "10.1000/sample"),
        "external_identifiers must be preserved when the edit payload omits them"
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn import_commit_notifies_local_search_index() {
    let root = temp_vault_root("cistella-m1-search-index");
    let vault = write_minimal_vault(&root);

    // Build an initial ready generation so incremental updates can apply.
    vault.rebuild_metadata_search_index().unwrap();

    vault
        .commit_bibtex_import(vault.inspect_bibtex_import(sample_bib()).unwrap())
        .unwrap();

    let query = SearchQuery {
        text: "Sample Article".to_string(),
        scopes: vec![SearchFieldScope::Title],
    };
    let result = vault.query_search_index(&query, 0, 100).unwrap();
    match result {
        cistella_core::SearchQueryResult::Ready { page } => {
            assert_eq!(
                page.total_hits, 1,
                "newly imported item must be reachable through the local search index"
            );
            assert_eq!(page.hits[0].field_matches[0].field, SearchMatchField::Title);
        }
        cistella_core::SearchQueryResult::Unavailable { .. } => {
            panic!("search index must be ready after import commit");
        }
    }

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn openalex_namespace_is_used_for_conflict_matching() {
    let root = temp_vault_root("cistella-m1-openalex-match");
    let vault = write_minimal_vault(&root);

    let item = cistella_core::LiteratureItem::from_draft(LiteratureItemDraft {
        title: "OpenAlex Work".to_string(),
        authors: vec!["Author".to_string()],
        published_year: Some(2023),
        item_type: LiteratureItemType::Article,
        favorite: false,
        reading_status: ReadingStatus::Inbox,
        tags: vec![],
        sources: vec![],
        external_identifiers: vec![ExternalIdentifier {
            namespace: "openalex".to_string(),
            value: "W123456789".to_string(),
        }],
    });
    vault.save_literature_items(&[item.clone()]).unwrap();

    let record = cistella_core::SourceRecord {
        record_id: uuid::Uuid::new_v4(),
        source_name: "BibTeX".to_string(),
        external_id: None,
        external_identifiers: vec![ExternalIdentifier {
            namespace: "openalex".to_string(),
            value: "W123456789".to_string(),
        }],
        title: "OpenAlex Work".to_string(),
        authors: vec!["Author".to_string()],
        published_year: Some(2023),
        item_type: "article".to_string(),
        abstract_text: String::new(),
        keywords: Vec::new(),
        pages: String::new(),
        volume: String::new(),
        raw_fields: std::collections::BTreeMap::new(),
    };

    let items = [item.clone()];
    let matched = find_matching_item(&record, &items);
    assert!(
        matched.is_some(),
        "two SourceRecords sharing an openalex id must match the same item"
    );
    assert_eq!(matched.unwrap().item_id, item.item_id);

    fs::remove_dir_all(root).unwrap();
}
