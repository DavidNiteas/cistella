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
  "vault_id": "work-order-13-m2",
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

fn sample_pubmed_body() -> String {
    serde_json::to_string(&serde_json::json!({
        "result": {
            "uids": ["12345"],
            "12345": {
                "title": "A Test PubMed Article",
                "authors": [{"name": "Smith AB"}, {"name": "Doe J"}],
                "pubdate": "2021 May 10",
                "source": "Journal of Examples",
                "doctype": "Journal Article",
                "articleids": [
                    {"idtype": "pubmed", "value": "12345"},
                    {"idtype": "pmc", "value": "PMC67890"},
                    {"idtype": "doi", "value": "10.1234/example.12345"}
                ]
            }
        }
    }))
    .unwrap()
}

fn sample_europepmc_body() -> String {
    serde_json::to_string(&serde_json::json!({
        "resultList": {
            "result": [{
                "title": "A Test Europe PMC Article",
                "authorString": "Smith AB, Doe J",
                "pubYear": "2022",
                "pubType": "journal article",
                "journalTitle": "European Journal of Examples",
                "pmid": "12345",
                "pmcid": "PMC67890",
                "doi": "10.1234/example.12345"
            }]
        }
    }))
    .unwrap()
}

#[test]
fn pmid_normalization_removes_prefixes() {
    use cistella_core::Identifier;
    use cistella_core::import::remote::identifier::parse_identifier;

    assert_eq!(
        parse_identifier("pmid:12345").unwrap(),
        Identifier::PMID("12345".to_string())
    );
    assert_eq!(
        parse_identifier("PMID:12345").unwrap(),
        Identifier::PMID("12345".to_string())
    );
    assert_eq!(
        parse_identifier("  PMID:12345  ").unwrap(),
        Identifier::PMID("12345".to_string())
    );
}

#[test]
fn pmcid_normalization_removes_prefixes() {
    use cistella_core::Identifier;
    use cistella_core::import::remote::identifier::parse_identifier;

    assert_eq!(
        parse_identifier("pmc:12345").unwrap(),
        Identifier::PMCID("12345".to_string())
    );
    assert_eq!(
        parse_identifier("PMC:12345").unwrap(),
        Identifier::PMCID("12345".to_string())
    );
    assert_eq!(
        parse_identifier("PMCID:12345").unwrap(),
        Identifier::PMCID("12345".to_string())
    );
    assert_eq!(
        parse_identifier("PMC:PMC12345").unwrap(),
        Identifier::PMCID("12345".to_string())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pubmed_response_parsing_populates_source_record_fields() {
    let root = temp_vault_root("cistella-m2-pubmed-parsing");
    let _vault = write_minimal_vault(&root);

    let body = sample_pubmed_body();
    let port = mock_server(http_response(&body, "200 OK"));
    let registry = RemoteResolverRegistry::with_pubmed_base_urls(
        format!("http://127.0.0.1:{port}"),
        format!("http://127.0.0.1:{}", closed_port()),
    );
    let record = registry.resolve(&root, "pmid:12345", false).await.unwrap();

    assert_eq!(record.title, "A Test PubMed Article");
    assert_eq!(record.authors, vec!["Smith AB", "Doe J"]);
    assert_eq!(record.published_year, Some(2021));
    assert_eq!(record.item_type, "article");
    assert_eq!(record.source_name, "PubMed");
    assert_eq!(record.external_id, Some("12345".to_string()));
    assert!(
        record
            .external_identifiers
            .iter()
            .any(|id| id.namespace == "pmid" && id.value == "12345")
    );
    assert!(
        record
            .external_identifiers
            .iter()
            .any(|id| id.namespace == "pmcid" && id.value == "67890")
    );
    assert!(
        record
            .external_identifiers
            .iter()
            .any(|id| id.namespace == "doi" && id.value == "10.1234/example.12345")
    );
    assert!(
        record
            .raw_fields
            .get("journal")
            .unwrap()
            .contains("Journal of Examples")
    );

    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn europe_pmc_fallback_when_pubmed_fails() {
    let root = temp_vault_root("cistella-m2-europepmc-fallback");
    let _vault = write_minimal_vault(&root);

    let closed = closed_port();
    let body = sample_europepmc_body();
    let port = mock_server(http_response(&body, "200 OK"));
    let registry = RemoteResolverRegistry::with_pubmed_base_urls(
        format!("http://127.0.0.1:{closed}"),
        format!("http://127.0.0.1:{port}"),
    );
    let record = registry.resolve(&root, "pmid:12345", false).await.unwrap();

    assert_eq!(record.title, "A Test Europe PMC Article");
    assert_eq!(record.source_name, "Europe PMC");
    assert_eq!(record.published_year, Some(2022));
    assert!(
        record
            .external_identifiers
            .iter()
            .any(|id| id.namespace == "pmid")
    );

    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_404_returns_remote_metadata_not_found() {
    let root = temp_vault_root("cistella-m2-not-found");
    let _vault = write_minimal_vault(&root);

    let response = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    let port = mock_server(response);
    let registry = RemoteResolverRegistry::with_pubmed_base_urls(
        format!("http://127.0.0.1:{}", closed_port()),
        format!("http://127.0.0.1:{port}"),
    );
    let err = registry
        .resolve(&root, "pmcid:99999", false)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("remote metadata not found"),
        "unexpected error: {err}"
    );

    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn network_failure_returns_remote_metadata_unavailable() {
    let root = temp_vault_root("cistella-m2-network-failure");
    let _vault = write_minimal_vault(&root);

    let port = closed_port();
    let registry = RemoteResolverRegistry::with_pubmed_base_urls(
        format!("http://127.0.0.1:{port}"),
        format!("http://127.0.0.1:{port}"),
    );
    let err = registry
        .resolve(&root, "pmcid:99999", false)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("remote metadata unavailable"),
        "unexpected error: {err}"
    );

    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cache_hit_avoids_second_network_request() {
    let root = temp_vault_root("cistella-m2-cache-hit");
    let _vault = write_minimal_vault(&root);

    let port = mock_server(http_response(&sample_pubmed_body(), "200 OK"));
    let registry = RemoteResolverRegistry::with_pubmed_base_urls(
        format!("http://127.0.0.1:{port}"),
        format!("http://127.0.0.1:{}", closed_port()),
    );
    let record = registry.resolve(&root, "pmid:12345", false).await.unwrap();
    assert_eq!(record.title, "A Test PubMed Article");

    // Second resolve targets closed ports; if the cache is used it must still succeed.
    let closed = closed_port();
    let offline_registry = RemoteResolverRegistry::with_pubmed_base_urls(
        format!("http://127.0.0.1:{closed}"),
        format!("http://127.0.0.1:{closed}"),
    );
    let cached = offline_registry
        .resolve(&root, "pmid:12345", false)
        .await
        .unwrap();
    assert_eq!(cached.title, "A Test PubMed Article");

    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn end_to_end_import_creates_item_with_pmid_pmcid_doi() {
    let root = temp_vault_root("cistella-m2-e2e");
    let vault = write_minimal_vault(&root);

    let body = sample_pubmed_body();
    let port = mock_server(http_response(&body, "200 OK"));
    let registry = RemoteResolverRegistry::with_pubmed_base_urls(
        format!("http://127.0.0.1:{port}"),
        format!("http://127.0.0.1:{}", closed_port()),
    );
    let record = registry.resolve(&root, "pmid:12345", false).await.unwrap();

    let result = cistella_core::import::library_importer::import_single_record(
        &vault,
        record,
        ConflictPolicy::Merge,
    )
    .unwrap();
    assert_eq!(result.created, 1);
    assert_eq!(result.merged, 0);

    let items = vault.load_literature_items().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].title, "A Test PubMed Article");
    assert!(
        items[0]
            .external_identifiers
            .iter()
            .any(|id| id.namespace == "pmid" && id.value == "12345")
    );
    assert!(
        items[0]
            .external_identifiers
            .iter()
            .any(|id| id.namespace == "pmcid" && id.value == "67890")
    );
    assert!(
        items[0]
            .external_identifiers
            .iter()
            .any(|id| id.namespace == "doi" && id.value == "10.1234/example.12345")
    );

    fs::remove_dir_all(root).unwrap();
}
