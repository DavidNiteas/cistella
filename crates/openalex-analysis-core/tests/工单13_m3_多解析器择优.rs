use std::{fs, path::PathBuf};

use cistella_core::{ConflictPolicy, RemoteResolverRegistry, Vault, VaultOpenOptions};
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
  "vault_id": "work-order-13-m3",
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

fn http_response(body: &str, status: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}

fn mock_server(response: impl Into<Vec<u8>>) -> u16 {
    use std::io::{Read, Write};
    use std::time::Duration;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let response = response.into();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 1024];
            loop {
                match stream.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if buf[..n].windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = stream.write_all(&response);
            let _ = stream.flush();
            std::thread::sleep(Duration::from_millis(500));
        }
    });
    port
}

fn closed_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

fn sample_openalex_body() -> String {
    serde_json::to_string(&serde_json::json!({
        "id": "https://openalex.org/W123456789",
        "title": "A Test OpenAlex Article",
        "authorships": [
            {"author": {"display_name": "OpenAlex, Alice"}}
        ],
        "publication_year": 2023,
        "type": "article",
        "primary_location": {
            "source": {"display_name": "OpenAlex Journal"}
        },
        "ids": {
            "openalex": "W123456789",
            "doi": "10.1234/example.12345"
        }
    }))
    .unwrap()
}

fn sample_semantic_scholar_body() -> String {
    serde_json::to_string(&serde_json::json!({
        "paperId": "abc123",
        "title": "A Test Semantic Scholar Article",
        "authors": [
            {"name": "Scholar, Bob"}
        ],
        "year": 2022,
        "publicationTypes": ["JournalArticle"],
        "externalIds": {
            "DOI": "10.1234/example.12345",
            "PMID": "12345",
            "PMCID": "PMC67890"
        },
        "venue": "Semantic Scholar Journal"
    }))
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn openalex_id_resolves_via_openalex_api() {
    let root = temp_vault_root("cistella-m3-openalex-id");
    let _vault = write_minimal_vault(&root);

    let body = sample_openalex_body();
    let port = mock_server(http_response(&body, "200 OK"));
    let registry =
        RemoteResolverRegistry::with_openalex_base_url(format!("http://127.0.0.1:{port}"));
    let record = registry
        .resolve_best(&root, "W123456789", false)
        .await
        .unwrap();

    assert_eq!(record.title, "A Test OpenAlex Article");
    assert_eq!(record.source_name, "OpenAlex API");
    assert!(
        record
            .external_identifiers
            .iter()
            .any(|id| id.namespace == "openalex" && id.value == "W123456789")
    );

    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doi_resolves_via_openalex_api() {
    let root = temp_vault_root("cistella-m3-openalex-doi");
    let _vault = write_minimal_vault(&root);

    let body = sample_openalex_body();
    let port = mock_server(http_response(&body, "200 OK"));
    let registry =
        RemoteResolverRegistry::with_openalex_base_url(format!("http://127.0.0.1:{port}"));
    let record = registry
        .resolve_best(&root, "10.1234/example.12345", false)
        .await
        .unwrap();

    assert_eq!(record.title, "A Test OpenAlex Article");
    assert_eq!(record.source_name, "OpenAlex API");
    assert!(
        record
            .external_identifiers
            .iter()
            .any(|id| id.namespace == "doi" && id.value == "10.1234/example.12345")
    );

    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn semantic_scholar_resolves_doi() {
    let root = temp_vault_root("cistella-m3-semantic-scholar-doi");
    let _vault = write_minimal_vault(&root);

    let body = sample_semantic_scholar_body();
    let port = mock_server(http_response(&body, "200 OK"));
    let registry =
        RemoteResolverRegistry::with_semantic_scholar_base_url(format!("http://127.0.0.1:{port}"));
    let record = registry
        .resolve_best(&root, "10.1234/example.12345", false)
        .await
        .unwrap();

    assert_eq!(record.title, "A Test Semantic Scholar Article");
    assert_eq!(record.source_name, "Semantic Scholar");
    assert_eq!(record.published_year, Some(2022));
    assert!(
        record
            .external_identifiers
            .iter()
            .any(|id| id.namespace == "pmcid" && id.value == "67890")
    );

    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn semantic_scholar_resolves_pmid() {
    let root = temp_vault_root("cistella-m3-semantic-scholar-pmid");
    let _vault = write_minimal_vault(&root);

    let body = sample_semantic_scholar_body();
    let port = mock_server(http_response(&body, "200 OK"));
    let registry =
        RemoteResolverRegistry::with_semantic_scholar_base_url(format!("http://127.0.0.1:{port}"));
    let record = registry
        .resolve_best(&root, "pmid:12345", false)
        .await
        .unwrap();

    assert_eq!(record.title, "A Test Semantic Scholar Article");
    assert_eq!(record.source_name, "Semantic Scholar");
    assert!(
        record
            .external_identifiers
            .iter()
            .any(|id| id.namespace == "pmid" && id.value == "12345")
    );

    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multi_resolver_merge_completes_partial_results() {
    let root = temp_vault_root("cistella-m3-merge");
    let _vault = write_minimal_vault(&root);

    // Crossref returns title but no year; Semantic Scholar returns year and PMCID.
    let crossref_body = serde_json::to_string(&serde_json::json!({
        "status": "ok",
        "message-type": "work",
        "message-version": "1.0.0",
        "message": {
            "title": ["Merged Test Article"],
            "author": [{"given": "A", "family": "Author"}],
            "type": "journal-article",
            "container-title": ["Crossref Journal"],
            "DOI": "10.1234/example.merge"
        }
    }))
    .unwrap();
    let semantic_body = serde_json::to_string(&serde_json::json!({
        "paperId": "abc",
        "title": "Merged Test Article",
        "authors": [{"name": "Author, A"}],
        "year": 2020,
        "publicationTypes": ["JournalArticle"],
        "externalIds": {
            "DOI": "10.1234/example.merge",
            "PMCID": "PMC99999"
        },
        "venue": "Semantic Scholar Journal"
    }))
    .unwrap();

    let crossref_port = mock_server(http_response(&crossref_body, "200 OK"));
    let semantic_port = mock_server(http_response(&semantic_body, "200 OK"));
    let closed = closed_port();
    let registry = RemoteResolverRegistry::with_all_base_urls(
        format!("http://127.0.0.1:{crossref_port}"),
        format!("http://127.0.0.1:{closed}"),
        format!("http://127.0.0.1:{closed}"),
        format!("http://127.0.0.1:{semantic_port}"),
        format!("http://127.0.0.1:{closed}"),
    );

    let record = registry
        .resolve_best(&root, "10.1234/example.merge", false)
        .await
        .unwrap();

    assert_eq!(record.title, "Merged Test Article");
    assert_eq!(record.published_year, Some(2020));
    assert_eq!(record.source_name, "crossref + semantic_scholar");
    assert!(
        record
            .external_identifiers
            .iter()
            .any(|id| id.namespace == "doi" && id.value == "10.1234/example.merge")
    );
    assert!(
        record
            .external_identifiers
            .iter()
            .any(|id| id.namespace == "pmcid" && id.value == "99999")
    );

    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabling_crossref_falls_back_to_semantic_scholar_for_doi() {
    let root = temp_vault_root("cistella-m3-disabled-crossref");
    let _vault = write_minimal_vault(&root);

    let body = sample_semantic_scholar_body();
    let port = mock_server(http_response(&body, "200 OK"));
    let closed = closed_port();

    let mut config = cistella_core::AppConfig::default();
    config
        .remote_resolvers_enabled
        .insert("crossref".to_string(), false);
    config
        .remote_resolvers_enabled
        .insert("openalex_api".to_string(), false);
    config
        .remote_resolvers_enabled
        .insert("pubmed".to_string(), false);

    let registry = RemoteResolverRegistry::with_all_base_urls(
        format!("http://127.0.0.1:{closed}"),
        format!("http://127.0.0.1:{closed}"),
        format!("http://127.0.0.1:{closed}"),
        format!("http://127.0.0.1:{port}"),
        format!("http://127.0.0.1:{closed}"),
    );
    let filtered = RemoteResolverRegistry::with_app_config(&config);
    // The app-config filtered registry should only contain semantic_scholar.
    assert_eq!(
        filtered
            .find_resolver(&cistella_core::Identifier::DOI(
                "10.1234/example.12345".to_string()
            ))
            .unwrap()
            .name(),
        "semantic_scholar"
    );

    // Use the all-base-url registry but simulate disabled crossref/openalex by
    // pointing them at closed ports; only semantic_scholar can answer.
    let record = registry
        .resolve_best(&root, "10.1234/example.12345", false)
        .await
        .unwrap();
    assert_eq!(record.source_name, "Semantic Scholar");
    assert_eq!(record.title, "A Test Semantic Scholar Article");

    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn all_resolvers_failed_returns_error() {
    let root = temp_vault_root("cistella-m3-all-failed");
    let _vault = write_minimal_vault(&root);

    let port = closed_port();
    let registry = RemoteResolverRegistry::with_all_base_urls(
        format!("http://127.0.0.1:{port}"),
        format!("http://127.0.0.1:{port}"),
        format!("http://127.0.0.1:{port}"),
        format!("http://127.0.0.1:{port}"),
        format!("http://127.0.0.1:{port}"),
    );
    let err = registry
        .resolve_best(&root, "10.1234/example.12345", false)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("remote metadata unavailable"),
        "unexpected error: {err}"
    );

    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clearing_cache_refetches_on_next_resolve() {
    let root = temp_vault_root("cistella-m3-cache-clear");
    let _vault = write_minimal_vault(&root);

    let body = sample_semantic_scholar_body();
    let port = mock_server(http_response(&body, "200 OK"));
    let registry =
        RemoteResolverRegistry::with_semantic_scholar_base_url(format!("http://127.0.0.1:{port}"));
    registry
        .resolve_best(&root, "10.1234/example.12345", false)
        .await
        .unwrap();

    // Clear the cache directory.
    let cache_dir = root.join("cache").join("remote_metadata");
    assert!(cache_dir.exists());
    fs::remove_dir_all(&cache_dir).unwrap();

    // Second resolve targets a closed port; without cache it must fail.
    let closed = closed_port();
    let offline_registry = RemoteResolverRegistry::with_semantic_scholar_base_url(format!(
        "http://127.0.0.1:{closed}"
    ));
    let err = offline_registry
        .resolve_best(&root, "10.1234/example.12345", false)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("remote metadata unavailable"),
        "unexpected error: {err}"
    );

    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn end_to_end_import_creates_item_with_merged_identifiers() {
    let root = temp_vault_root("cistella-m3-e2e");
    let vault = write_minimal_vault(&root);

    let body = sample_semantic_scholar_body();
    let port = mock_server(http_response(&body, "200 OK"));
    let registry =
        RemoteResolverRegistry::with_semantic_scholar_base_url(format!("http://127.0.0.1:{port}"));
    let record = registry
        .resolve_best(&root, "10.1234/example.12345", false)
        .await
        .unwrap();

    let result = cistella_core::import::library_importer::import_single_record(
        &vault,
        record,
        ConflictPolicy::Merge,
    )
    .unwrap();
    assert_eq!(result.created, 1);

    let items = vault.load_literature_items().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].title, "A Test Semantic Scholar Article");
    assert!(
        items[0]
            .external_identifiers
            .iter()
            .any(|id| id.namespace == "doi" && id.value == "10.1234/example.12345")
    );
    assert!(
        items[0]
            .external_identifiers
            .iter()
            .any(|id| id.namespace == "pmid" && id.value == "12345")
    );

    fs::remove_dir_all(root).unwrap();
}
