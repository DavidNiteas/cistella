use std::{fs, path::PathBuf};

use cistella_core::import::conflict::find_matching_item;
use cistella_core::{
    ConflictPolicy, ExternalIdentifier, ImportFormat, LiteratureItemDraft, LiteratureItemType,
    ReadingStatus, Vault, VaultOpenOptions,
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
  "vault_id": "work-order-12-m2",
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

fn sample_ris_journal() -> &'static [u8] {
    br#"TY  - JOUR
TI  - Sample RIS Article
AU  - Alice Author
AU  - Bob Writer
PY  - 2023
DO  - 10.1000/ris-sample
SN  - 1234-5678
IS  - 3
VL  - 7
SP  - 11
EP  - 20
AB  - This article demonstrates RIS import.
KW  - RIS
KW  - import
ER  -

TY  - BOOK
TI  - Another RIS Book
AU  - Carol Editor
PY  - 2022
SN  - 978-3-030-00000-0
ER  -
"#
}

fn sample_ris_duplicate_doi() -> &'static [u8] {
    br#"TY  - JOUR
TI  - Updated RIS Article
AU  - Alice Author
AU  - Bob Writer
AU  - Charlie Contributor
PY  - 2023
DO  - 10.1000/ris-sample
ER  -
"#
}

#[test]
fn parses_common_ris_tags() {
    let root = temp_vault_root("cistella-m2-parse");
    let vault = write_minimal_vault(&root);

    let preview = vault
        .inspect_literature_import(ImportFormat::Ris, sample_ris_journal())
        .unwrap();
    assert_eq!(preview.items.len(), 2);
    assert_eq!(preview.source_name, "RIS");

    let article = preview
        .items
        .iter()
        .find(|i| i.source_record.title == "Sample RIS Article")
        .unwrap();
    assert_eq!(
        article.source_record.authors,
        vec!["Alice Author", "Bob Writer"]
    );
    assert_eq!(article.source_record.published_year, Some(2023));
    assert_eq!(article.source_record.item_type, "article");
    assert_eq!(article.source_record.volume, "7");
    assert_eq!(article.source_record.pages, "11-20");
    assert_eq!(
        article.source_record.abstract_text,
        "This article demonstrates RIS import."
    );
    assert_eq!(article.source_record.keywords, vec!["RIS", "import"]);
    assert!(
        article
            .source_record
            .external_identifiers
            .iter()
            .any(|id| id.namespace == "doi" && id.value == "10.1000/ris-sample")
    );
    // RIS `SN` on a journal maps to ISSN.
    assert!(
        article
            .source_record
            .external_identifiers
            .iter()
            .any(|id| id.namespace == "issn" && id.value == "1234-5678")
    );
    // RIS `IS` is the issue number and must not be treated as ISSN.
    assert!(
        !article
            .source_record
            .external_identifiers
            .iter()
            .any(|id| id.namespace == "issn" && id.value == "3")
    );
    assert_eq!(
        article.source_record.raw_fields.get("is"),
        Some(&"3".to_string())
    );

    let book = preview
        .items
        .iter()
        .find(|i| i.source_record.title == "Another RIS Book")
        .unwrap();
    assert_eq!(book.source_record.item_type, "book");
    assert!(
        book.source_record
            .external_identifiers
            .iter()
            .any(|id| id.namespace == "isbn" && id.value == "978-3-030-00000-0")
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn sn_maps_to_issn_for_journals_and_isbn_for_books() {
    let root = temp_vault_root("cistella-m2-sn-ns");
    let vault = write_minimal_vault(&root);

    let ris = br#"TY  - JOUR
TI  - Journal Article
PY  - 2023
SN  - 1234-5678
ER  -

TY  - BOOK
TI  - A Book
PY  - 2022
SN  - 978-3-030-00000-0
ER  -
"#;
    let preview = vault
        .inspect_literature_import(ImportFormat::Ris, ris)
        .unwrap();
    assert_eq!(preview.items.len(), 2);

    let journal = preview
        .items
        .iter()
        .find(|i| i.source_record.title == "Journal Article")
        .unwrap();
    assert!(
        journal
            .source_record
            .external_identifiers
            .iter()
            .any(|id| id.namespace == "issn" && id.value == "1234-5678")
    );
    assert!(
        !journal
            .source_record
            .external_identifiers
            .iter()
            .any(|id| id.namespace == "isbn")
    );

    let book = preview
        .items
        .iter()
        .find(|i| i.source_record.title == "A Book")
        .unwrap();
    assert!(
        book.source_record
            .external_identifiers
            .iter()
            .any(|id| id.namespace == "isbn" && id.value == "978-3-030-00000-0")
    );
    assert!(
        !book
            .source_record
            .external_identifiers
            .iter()
            .any(|id| id.namespace == "issn")
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn is_issue_number_does_not_create_issn_identifier() {
    let root = temp_vault_root("cistella-m2-is-issue");
    let vault = write_minimal_vault(&root);

    let ris = br#"TY  - JOUR
TI  - Journal Article
PY  - 2023
IS  - 3
ER  -
"#;
    let preview = vault
        .inspect_literature_import(ImportFormat::Ris, ris)
        .unwrap();
    assert_eq!(preview.items.len(), 1);

    let rec = &preview.items[0].source_record;
    assert!(
        !rec.external_identifiers
            .iter()
            .any(|id| id.namespace == "issn")
    );
    assert_eq!(rec.raw_fields.get("is"), Some(&"3".to_string()));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn empty_vault_import_creates_new_items() {
    let root = temp_vault_root("cistella-m2-empty");
    let vault = write_minimal_vault(&root);

    let preview = vault
        .inspect_literature_import(ImportFormat::Ris, sample_ris_journal())
        .unwrap();
    assert_eq!(preview.items.len(), 2);
    assert!(preview.items.iter().all(|i| i.matched_item_id.is_none()));

    let result = vault
        .commit_literature_import(ImportFormat::Ris, preview)
        .unwrap();
    assert_eq!(result.created, 2);
    assert_eq!(result.merged, 0);
    assert_eq!(result.skipped, 0);

    let items = vault.load_literature_items().unwrap();
    assert_eq!(items.len(), 2);
    assert!(items.iter().any(|i| i.title == "Sample RIS Article"));
    assert!(items.iter().any(|i| i.title == "Another RIS Book"));

    let sample = items
        .iter()
        .find(|i| i.title == "Sample RIS Article")
        .unwrap();
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
            .any(|id| id.namespace == "doi" && id.value == "10.1000/ris-sample")
    );
    assert_eq!(sample.sources.len(), 1);
    assert_eq!(sample.sources[0].source_name, "RIS");

    let batches = vault.load_source_batches().unwrap();
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].source_name, "RIS");
    assert_eq!(batches[0].records.len(), 2);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn duplicate_import_merge_respects_existing_item() {
    let root = temp_vault_root("cistella-m2-merge");
    let vault = write_minimal_vault(&root);

    let first = vault
        .inspect_literature_import(ImportFormat::Ris, sample_ris_journal())
        .unwrap();
    vault
        .commit_literature_import(ImportFormat::Ris, first)
        .unwrap();

    let second = vault
        .inspect_literature_import(ImportFormat::Ris, sample_ris_duplicate_doi())
        .unwrap();
    assert_eq!(second.items.len(), 1);
    assert!(second.items[0].matched_item_id.is_some());

    let result = vault
        .commit_literature_import(ImportFormat::Ris, second)
        .unwrap();
    assert_eq!(result.created, 0);
    assert_eq!(result.merged, 1);

    let items = vault.load_literature_items().unwrap();
    assert_eq!(items.len(), 2);
    let sample = items
        .iter()
        .find(|i| i.title == "Sample RIS Article")
        .unwrap();
    // Both RIS records share the same DOI, so they resolve to the same
    // source_ref and are de-duplicated.
    assert_eq!(sample.sources.len(), 1);
    assert_eq!(sample.authors, vec!["Alice Author", "Bob Writer"]);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn conflict_skip_strategy_skips_record() {
    let root = temp_vault_root("cistella-m2-skip");
    let vault = write_minimal_vault(&root);

    let first = vault
        .inspect_literature_import(ImportFormat::Ris, sample_ris_journal())
        .unwrap();
    vault
        .commit_literature_import(ImportFormat::Ris, first)
        .unwrap();

    let mut second = vault
        .inspect_literature_import(ImportFormat::Ris, sample_ris_duplicate_doi())
        .unwrap();
    second.items[0].selected_policy = ConflictPolicy::Skip;

    let result = vault
        .commit_literature_import(ImportFormat::Ris, second)
        .unwrap();
    assert_eq!(result.skipped, 1);
    assert_eq!(result.created, 0);
    assert_eq!(result.merged, 0);

    let items = vault.load_literature_items().unwrap();
    let sample = items
        .iter()
        .find(|i| i.title == "Sample RIS Article")
        .unwrap();
    assert_eq!(sample.sources.len(), 1);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn conflict_create_strategy_forces_new_item() {
    let root = temp_vault_root("cistella-m2-create");
    let vault = write_minimal_vault(&root);

    let first = vault
        .inspect_literature_import(ImportFormat::Ris, sample_ris_journal())
        .unwrap();
    vault
        .commit_literature_import(ImportFormat::Ris, first)
        .unwrap();

    let mut second = vault
        .inspect_literature_import(ImportFormat::Ris, sample_ris_duplicate_doi())
        .unwrap();
    second.items[0].selected_policy = ConflictPolicy::Create;

    let result = vault
        .commit_literature_import(ImportFormat::Ris, second)
        .unwrap();
    assert_eq!(result.created, 1);
    assert_eq!(result.merged, 0);

    let items = vault.load_literature_items().unwrap();
    assert_eq!(items.len(), 3);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn commit_failure_restores_source_batches() {
    let root = temp_vault_root("cistella-m2-rollback");
    let vault = write_minimal_vault(&root);

    let preview = vault
        .inspect_literature_import(ImportFormat::Ris, sample_ris_journal())
        .unwrap();

    // Make literature_items.json unwritable by creating a directory at that path.
    fs::create_dir_all(vault.literature_items_path()).unwrap();

    let result = vault.commit_literature_import(ImportFormat::Ris, preview);
    assert!(result.is_err());

    let batches = vault.load_source_batches().unwrap();
    assert!(
        batches.is_empty(),
        "batch must be rolled back after failed commit"
    );

    let _ = fs::remove_dir_all(vault.literature_items_path());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn malformed_ris_returns_structured_error() {
    let root = temp_vault_root("cistella-m2-malformed");
    let vault = write_minimal_vault(&root);

    let err = vault
        .inspect_literature_import(ImportFormat::Ris, b"not a ris file")
        .unwrap_err();
    assert!(err.to_string().contains("invalid literature import"));

    let err = vault
        .inspect_literature_import(ImportFormat::Ris, b"ER  - \nER  - ")
        .unwrap_err();
    assert!(err.to_string().contains("invalid literature import"));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn ris_and_bibtex_records_conflict_by_doi() {
    let root = temp_vault_root("cistella-m2-cross-format");
    let vault = write_minimal_vault(&root);

    // Import a BibTeX record first.
    let bib = br#"
@article{sample2023,
  title = {Sample Article},
  author = {Alice Author and Bob Writer},
  year = {2023},
  doi = {10.1000/cross},
}
"#;
    vault
        .commit_bibtex_import(vault.inspect_bibtex_import(bib).unwrap())
        .unwrap();

    // The same DOI arriving from a RIS record must match the existing item.
    let ris = br#"TY  - JOUR
TI  - Sample From RIS
AU  - Alice Author
PY  - 2023
DO  - 10.1000/cross
ER  -
"#;
    let preview = vault
        .inspect_literature_import(ImportFormat::Ris, ris)
        .unwrap();
    assert_eq!(preview.items.len(), 1);
    assert!(
        preview.items[0].matched_item_id.is_some(),
        "RIS record with matching DOI must conflict with existing BibTeX item"
    );

    let result = vault
        .commit_literature_import(ImportFormat::Ris, preview)
        .unwrap();
    assert_eq!(result.merged, 1);
    assert_eq!(result.created, 0);

    let items = vault.load_literature_items().unwrap();
    assert_eq!(items.len(), 1);
    let item = &items[0];
    assert_eq!(item.sources.len(), 2);
    assert!(item.sources.iter().any(|s| s.source_name == "BibTeX"));
    assert!(item.sources.iter().any(|s| s.source_name == "RIS"));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn issn_namespace_is_used_for_conflict_matching() {
    let root = temp_vault_root("cistella-m2-issn-match");
    let vault = write_minimal_vault(&root);

    let item = cistella_core::LiteratureItem::from_draft(LiteratureItemDraft {
        title: "Journal Article".to_string(),
        authors: vec!["Author".to_string()],
        published_year: Some(2023),
        item_type: LiteratureItemType::Article,
        favorite: false,
        reading_status: ReadingStatus::Inbox,
        tags: vec![],
        sources: vec![],
        external_identifiers: vec![ExternalIdentifier {
            namespace: "issn".to_string(),
            value: "1234-5678".to_string(),
        }],
    });
    vault.save_literature_items(&[item.clone()]).unwrap();

    let record = cistella_core::SourceRecord {
        record_id: Uuid::new_v4(),
        source_name: "RIS".to_string(),
        external_id: None,
        external_identifiers: vec![ExternalIdentifier {
            namespace: "issn".to_string(),
            value: "1234-5678".to_string(),
        }],
        title: "Journal Article".to_string(),
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
        "RIS ISSN must match an existing item by namespace"
    );
    assert_eq!(matched.unwrap().item_id, item.item_id);

    fs::remove_dir_all(root).unwrap();
}
