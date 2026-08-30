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
  "vault_id": "work-order-13-m1",
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

fn sample_crossref_body() -> String {
    serde_json::to_string(&serde_json::json!({
        "status": "ok",
        "message-type": "work",
        "message-version": "1.0.0",
        "message": {
            "title": ["A Test Paper From Crossref"],
            "author": [
                {"given": "Alice", "family": "Researcher"},
                {"given": "Bob", "family": "Collaborator"}
            ],
            "published-print": {"date-parts": [[2022, 5, 10]]},
            "type": "journal-article",
            "container-title": ["Journal of Examples"],
            "DOI": "10.1234/example.5678",
            "ISBN": ["978-3-030-99999-9"]
        }
    }))
    .unwrap()
}

fn crossref_http_response(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
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

fn mock_crossref_server(response_body: String) -> u16 {
    mock_server(response_body)
}

fn closed_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

#[test]
fn identifier_normalization_removes_doi_prefixes() {
    use cistella_core::Identifier;
    use cistella_core::import::remote::identifier::parse_identifier;

    assert_eq!(
        parse_identifier("doi:10.1234/EXAMPLE").unwrap(),
        Identifier::DOI("10.1234/example".to_string())
    );
    assert_eq!(
        parse_identifier("https://doi.org/10.1234/EXAMPLE").unwrap(),
        Identifier::DOI("10.1234/example".to_string())
    );
    assert_eq!(
        parse_identifier("  10.1234/EXAMPLE  ").unwrap(),
        Identifier::DOI("10.1234/example".to_string())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cache_hit_avoids_second_network_request() {
    let root = temp_vault_root("cistella-m1-cache-hit");
    let _vault = write_minimal_vault(&root);

    let port = mock_crossref_server(crossref_http_response(&sample_crossref_body()));
    let registry =
        RemoteResolverRegistry::with_crossref_base_url(format!("http://127.0.0.1:{port}"));
    let record = registry
        .resolve(&root, "10.1234/example.5678", false)
        .await
        .unwrap();
    assert_eq!(record.title, "A Test Paper From Crossref");

    // Second resolve targets a closed port; if the cache is used it must still succeed.
    let closed = closed_port();
    let offline_registry =
        RemoteResolverRegistry::with_crossref_base_url(format!("http://127.0.0.1:{closed}"));
    let cached = offline_registry
        .resolve(&root, "10.1234/example.5678", false)
        .await
        .unwrap();
    assert_eq!(cached.title, "A Test Paper From Crossref");

    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn force_refresh_ignores_cache_and_refetches() {
    let root = temp_vault_root("cistella-m1-force-refresh");
    let _vault = write_minimal_vault(&root);

    let first_port = mock_crossref_server(crossref_http_response(&sample_crossref_body()));
    let registry =
        RemoteResolverRegistry::with_crossref_base_url(format!("http://127.0.0.1:{first_port}"));
    registry
        .resolve(&root, "10.1234/example.5678", false)
        .await
        .unwrap();

    let updated_body = serde_json::to_string(&serde_json::json!({
        "message": {
            "title": ["Updated Title"],
            "author": [{"given": "C", "family": "Author"}],
            "published": {"date-parts": [[2023]]},
            "type": "journal-article",
            "DOI": "10.1234/example.5678"
        }
    }))
    .unwrap();
    let second_port = mock_crossref_server(crossref_http_response(&updated_body));
    let refreshed_registry =
        RemoteResolverRegistry::with_crossref_base_url(format!("http://127.0.0.1:{second_port}"));
    let record = refreshed_registry
        .resolve(&root, "10.1234/example.5678", true)
        .await
        .unwrap();
    assert_eq!(record.title, "Updated Title");

    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn network_failure_returns_remote_metadata_unavailable() {
    let root = temp_vault_root("cistella-m1-network-failure");
    let _vault = write_minimal_vault(&root);

    let port = closed_port();
    let registry =
        RemoteResolverRegistry::with_crossref_base_url(format!("http://127.0.0.1:{port}"));
    let err = registry
        .resolve(&root, "10.1234/example.5678", false)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("remote metadata unavailable"),
        "unexpected error: {err}"
    );

    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_404_returns_remote_metadata_not_found() {
    let root = temp_vault_root("cistella-m1-not-found");
    let _vault = write_minimal_vault(&root);

    let response = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    let port = mock_server(response);

    let registry =
        RemoteResolverRegistry::with_crossref_base_url(format!("http://127.0.0.1:{port}"));
    let err = registry
        .resolve(&root, "10.1234/missing", false)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("remote metadata not found"),
        "unexpected error: {err}"
    );

    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn response_parsing_populates_source_record_fields() {
    let root = temp_vault_root("cistella-m1-parsing");
    let _vault = write_minimal_vault(&root);

    let body = sample_crossref_body();
    let port = mock_crossref_server(crossref_http_response(&body));
    let registry =
        RemoteResolverRegistry::with_crossref_base_url(format!("http://127.0.0.1:{port}"));
    let record = registry
        .resolve(&root, "10.1234/example.5678", false)
        .await
        .unwrap();

    assert_eq!(record.title, "A Test Paper From Crossref");
    assert_eq!(
        record.authors,
        vec!["Researcher, Alice", "Collaborator, Bob"]
    );
    assert_eq!(record.published_year, Some(2022));
    assert_eq!(record.item_type, "article");
    assert_eq!(record.source_name, "Crossref");
    assert_eq!(record.external_id, Some("10.1234/example.5678".to_string()));
    assert!(
        record
            .external_identifiers
            .iter()
            .any(|id| id.namespace == "doi" && id.value == "10.1234/example.5678")
    );
    assert!(
        record
            .external_identifiers
            .iter()
            .any(|id| id.namespace == "isbn" && id.value == "978-3-030-99999-9")
    );
    assert!(
        record
            .raw_fields
            .get("container-title")
            .unwrap()
            .contains("Journal of Examples")
    );

    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn end_to_end_import_creates_item_with_doi() {
    let root = temp_vault_root("cistella-m1-e2e");
    let vault = write_minimal_vault(&root);

    let body = sample_crossref_body();
    let port = mock_crossref_server(crossref_http_response(&body));
    let registry =
        RemoteResolverRegistry::with_crossref_base_url(format!("http://127.0.0.1:{port}"));
    let record = registry
        .resolve(&root, "10.1234/example.5678", false)
        .await
        .unwrap();

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
    assert_eq!(items[0].title, "A Test Paper From Crossref");
    assert!(
        items[0]
            .external_identifiers
            .iter()
            .any(|id| id.namespace == "doi" && id.value == "10.1234/example.5678")
    );

    fs::remove_dir_all(root).unwrap();
}
