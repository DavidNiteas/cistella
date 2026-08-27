use std::{fs, path::PathBuf};

use cistella_core::{
    ConflictPolicy, ExternalIdentifier, ImportFormat, LiteratureItemDraft, LiteratureItemType,
    LocalOpenAlexDoiResolver, ReadingStatus, Vault, VaultOpenOptions,
    import::{
        doi_resolver::{DoiResolver, list_local_openalex_works},
        record_source::RecordSource,
    },
};
use flate2::{Compression, write::GzEncoder};
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
  "vault_id": "work-order-12-m3",
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

fn sample_work_json(id: &str, doi: &str, title: &str) -> String {
    serde_json::json!({
        "id": format!("https://openalex.org/{id}"),
        "doi": format!("https://doi.org/{doi}"),
        "title": title,
        "publication_year": 2023,
        "type": "article",
        "authorships": [
            {"author": {"display_name": "Alice Author"}},
            {"author": {"display_name": "Bob Writer"}}
        ],
        "host_venue": {"issn_l": "1234-5678"},
        "biblio": {"volume": "7", "first_page": "11", "last_page": "20"},
        "abstract_inverted_index": {"Hello": [0], "world": [1], "abstract": [2]},
        "keywords": [{"keyword": "testing"}, {"keyword": "openalex"}]
    })
    .to_string()
}

fn sample_work_bytes() -> Vec<u8> {
    sample_work_json("W123456789", "10.1000/openalex", "OpenAlex Work").into_bytes()
}

fn write_works_jsonl(root: &PathBuf, lines: &str) -> PathBuf {
    let dir = root.join("updated_date=2026-08-27").join("work");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("part_000.jsonl");
    fs::write(&path, lines).unwrap();
    path
}

#[test]
fn parses_openalex_work_json() {
    let records = cistella_core::OpenAlexWorksRecordSource::new()
        .parse(&sample_work_bytes())
        .unwrap();
    assert_eq!(records.len(), 1);
    let rec = &records[0];
    assert_eq!(rec.source_name, "OpenAlex");
    assert_eq!(rec.title, "OpenAlex Work");
    assert_eq!(rec.authors, vec!["Alice Author", "Bob Writer"]);
    assert_eq!(rec.published_year, Some(2023));
    assert_eq!(rec.item_type, "article");
    assert_eq!(rec.volume, "7");
    assert_eq!(rec.pages, "11-20");
    assert_eq!(rec.abstract_text, "Hello world abstract");
    assert_eq!(rec.keywords, vec!["testing", "openalex"]);
    assert!(
        rec.external_identifiers
            .iter()
            .any(|id| id.namespace == "openalex" && id.value == "W123456789")
    );
    assert!(
        rec.external_identifiers
            .iter()
            .any(|id| id.namespace == "doi" && id.value == "10.1000/openalex")
    );
    assert!(
        rec.external_identifiers
            .iter()
            .any(|id| id.namespace == "issn" && id.value == "1234-5678")
    );
    assert_eq!(rec.external_id, Some("W123456789".to_string()));
}

#[test]
fn parses_gzipped_jsonl() {
    let root = temp_vault_root("cistella-m3-gz");
    let dir = root.join("work");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("part_000.jsonl.gz");
    let data = sample_work_json("W999", "10.1000/gz", "Gzipped Work");
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    std::io::Write::write_all(&mut encoder, data.as_bytes()).unwrap();
    fs::write(&path, encoder.finish().unwrap()).unwrap();

    let resolver = LocalOpenAlexDoiResolver::scan(&root).unwrap();
    let found = resolver.resolve("10.1000/gz").unwrap();
    assert!(found.is_some());
    assert_eq!(found.unwrap().title, "Gzipped Work");

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn empty_vault_import_creates_new_item() {
    let root = temp_vault_root("cistella-m3-empty");
    let vault = write_minimal_vault(&root);

    let preview = vault
        .inspect_literature_import(ImportFormat::OpenAlexWorks, &sample_work_bytes())
        .unwrap();
    assert_eq!(preview.items.len(), 1);
    assert!(preview.items[0].matched_item_id.is_none());
    assert_eq!(preview.source_name, "OpenAlex");

    let result = vault
        .commit_literature_import(ImportFormat::OpenAlexWorks, preview)
        .unwrap();
    assert_eq!(result.created, 1);
    assert_eq!(result.merged, 0);
    assert_eq!(result.skipped, 0);

    let items = vault.load_literature_items().unwrap();
    assert_eq!(items.len(), 1);
    let item = &items[0];
    assert_eq!(item.title, "OpenAlex Work");
    assert_eq!(item.item_type, LiteratureItemType::Article);
    assert_eq!(item.reading_status, ReadingStatus::Inbox);
    assert!(!item.favorite);
    assert!(
        item.external_identifiers
            .iter()
            .any(|id| id.namespace == "openalex" && id.value == "W123456789")
    );
    assert!(
        item.external_identifiers
            .iter()
            .any(|id| id.namespace == "doi" && id.value == "10.1000/openalex")
    );

    let batches = vault.load_source_batches().unwrap();
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].source_name, "OpenAlex");
    assert_eq!(batches[0].records.len(), 1);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn duplicate_doi_merge_appends_source_ref() {
    let root = temp_vault_root("cistella-m3-merge");
    let vault = write_minimal_vault(&root);

    let first = vault
        .inspect_literature_import(ImportFormat::OpenAlexWorks, &sample_work_bytes())
        .unwrap();
    vault
        .commit_literature_import(ImportFormat::OpenAlexWorks, first)
        .unwrap();

    let duplicate = sample_work_json("W987654321", "10.1000/openalex", "Updated OpenAlex Work");
    let second = vault
        .inspect_literature_import(ImportFormat::OpenAlexWorks, duplicate.as_bytes())
        .unwrap();
    assert_eq!(second.items.len(), 1);
    assert!(second.items[0].matched_item_id.is_some());

    let result = vault
        .commit_literature_import(ImportFormat::OpenAlexWorks, second)
        .unwrap();
    assert_eq!(result.created, 0);
    assert_eq!(result.merged, 1);

    let items = vault.load_literature_items().unwrap();
    assert_eq!(items.len(), 1);
    let item = &items[0];
    assert_eq!(item.title, "OpenAlex Work");
    assert_eq!(item.sources.len(), 2);
    assert!(item.sources.iter().any(|s| s.source_name == "OpenAlex"));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn conflict_skip_strategy_skips_record() {
    let root = temp_vault_root("cistella-m3-skip");
    let vault = write_minimal_vault(&root);

    let first = vault
        .inspect_literature_import(ImportFormat::OpenAlexWorks, &sample_work_bytes())
        .unwrap();
    vault
        .commit_literature_import(ImportFormat::OpenAlexWorks, first)
        .unwrap();

    let duplicate = sample_work_json("W999", "10.1000/openalex", "Another");
    let mut second = vault
        .inspect_literature_import(ImportFormat::OpenAlexWorks, duplicate.as_bytes())
        .unwrap();
    second.items[0].selected_policy = ConflictPolicy::Skip;

    let result = vault
        .commit_literature_import(ImportFormat::OpenAlexWorks, second)
        .unwrap();
    assert_eq!(result.skipped, 1);
    assert_eq!(result.created, 0);
    assert_eq!(result.merged, 0);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn conflict_create_strategy_forces_new_item() {
    let root = temp_vault_root("cistella-m3-create");
    let vault = write_minimal_vault(&root);

    let first = vault
        .inspect_literature_import(ImportFormat::OpenAlexWorks, &sample_work_bytes())
        .unwrap();
    vault
        .commit_literature_import(ImportFormat::OpenAlexWorks, first)
        .unwrap();

    let duplicate = sample_work_json("W999", "10.1000/openalex", "Another");
    let mut second = vault
        .inspect_literature_import(ImportFormat::OpenAlexWorks, duplicate.as_bytes())
        .unwrap();
    second.items[0].selected_policy = ConflictPolicy::Create;

    let result = vault
        .commit_literature_import(ImportFormat::OpenAlexWorks, second)
        .unwrap();
    assert_eq!(result.created, 1);
    assert_eq!(result.merged, 0);

    let items = vault.load_literature_items().unwrap();
    assert_eq!(items.len(), 2);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn openalex_id_conflict_matching() {
    let root = temp_vault_root("cistella-m3-openalex-match");
    let vault = write_minimal_vault(&root);

    let item = cistella_core::LiteratureItem::from_draft(LiteratureItemDraft {
        title: "Existing".to_string(),
        authors: vec!["Author".to_string()],
        published_year: Some(2020),
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

    let preview = vault
        .inspect_literature_import(ImportFormat::OpenAlexWorks, &sample_work_bytes())
        .unwrap();
    assert_eq!(preview.items[0].matched_item_id, Some(item.item_id));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn commit_failure_restores_source_batches() {
    let root = temp_vault_root("cistella-m3-rollback");
    let vault = write_minimal_vault(&root);

    let preview = vault
        .inspect_literature_import(ImportFormat::OpenAlexWorks, &sample_work_bytes())
        .unwrap();

    fs::create_dir_all(vault.literature_items_path()).unwrap();

    let result = vault.commit_literature_import(ImportFormat::OpenAlexWorks, preview);
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
fn malformed_work_returns_structured_error() {
    let root = temp_vault_root("cistella-m3-malformed");
    let vault = write_minimal_vault(&root);

    let err = vault
        .inspect_literature_import(ImportFormat::OpenAlexWorks, b"not json")
        .unwrap_err();
    assert!(err.to_string().contains("invalid literature import"));

    let err = vault
        .inspect_literature_import(ImportFormat::OpenAlexWorks, b"\"string\"")
        .unwrap_err();
    assert!(err.to_string().contains("invalid literature import"));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn local_doi_resolver_hit_and_miss() {
    let root = temp_vault_root("cistella-m3-resolver");
    let lines = format!(
        "{}\n{}\n",
        sample_work_json("W1", "10.1000/hit", "Hit"),
        sample_work_json("W2", "10.1000/other", "Other")
    );
    write_works_jsonl(&root, &lines);

    let resolver = LocalOpenAlexDoiResolver::scan(&root).unwrap();
    let hit = resolver.resolve("10.1000/hit").unwrap();
    assert!(hit.is_some());
    assert_eq!(hit.unwrap().title, "Hit");

    let miss = resolver.resolve("10.1000/missing").unwrap();
    assert!(miss.is_none());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn list_local_openalex_works_search() {
    let root = temp_vault_root("cistella-m3-search");
    let lines = format!(
        "{}\n{}\n",
        sample_work_json("W1", "10.1000/alpha", "Alpha Work"),
        sample_work_json("W2", "10.1000/beta", "Beta Work")
    );
    write_works_jsonl(&root, &lines);

    let by_title = list_local_openalex_works(&root, "alpha", 10).unwrap();
    assert_eq!(by_title.len(), 1);
    assert_eq!(by_title[0].title, "Alpha Work");

    let by_doi = list_local_openalex_works(&root, "10.1000/beta", 10).unwrap();
    assert_eq!(by_doi.len(), 1);
    assert_eq!(by_doi[0].title, "Beta Work");

    let by_id = list_local_openalex_works(&root, "W1", 10).unwrap();
    assert_eq!(by_id.len(), 1);

    let limited = list_local_openalex_works(&root, "Work", 1).unwrap();
    assert_eq!(limited.len(), 1);

    fs::remove_dir_all(root).unwrap();
}
