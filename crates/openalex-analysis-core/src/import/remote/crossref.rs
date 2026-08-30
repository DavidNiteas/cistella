use std::{collections::BTreeMap, time::Duration};

use serde_json::Value;
use uuid::Uuid;

use crate::{CoreError, ExternalIdentifier, Result, import::source_record::SourceRecord};

use super::{
    Identifier, RemoteMetadataResolver, ResolveContext,
    cache::{load_cached, save_cached},
};

const DEFAULT_BASE_URL: &str = "https://api.crossref.org";
const USER_AGENT: &str = "cistella/0.1.0 (mailto:contact@example.com)";

/// Crossref DOI resolver.
#[derive(Debug, Clone)]
pub struct CrossrefResolver {
    base_url: String,
}

impl Default for CrossrefResolver {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
        }
    }
}

impl CrossrefResolver {
    /// Creates a resolver that talks to a custom Crossref-compatible endpoint.
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
        }
    }

    fn url_for(&self, doi: &str) -> String {
        format!("{}/works/{}", self.base_url, doi)
    }

    async fn fetch_with_retry(&self, ctx: &ResolveContext<'_>, doi: &str) -> Result<Value> {
        let url = self.url_for(doi);
        let mut last_error: Option<CoreError> = None;

        for attempt in 1..=2 {
            let request = ctx
                .client
                .get(&url)
                .timeout(ctx.timeout)
                .header("User-Agent", USER_AGENT)
                .send();

            match request.await {
                Ok(response) => {
                    let status = response.status();
                    if status == reqwest::StatusCode::NOT_FOUND {
                        return Err(CoreError::RemoteMetadataNotFound {
                            identifier: doi.to_string(),
                        });
                    }
                    if !status.is_success() {
                        last_error = Some(CoreError::RemoteMetadataUnavailable {
                            resolver: self.name().to_string(),
                            reason: format!("HTTP {status}"),
                        });
                        if status.is_client_error() {
                            // Client errors are not worth retrying.
                            break;
                        }
                        continue;
                    }
                    match response.json::<Value>().await {
                        Ok(body) => return Ok(body),
                        Err(error) => {
                            last_error = Some(CoreError::RemoteMetadataMalformed {
                                identifier: doi.to_string(),
                                reason: format!("invalid JSON: {error}"),
                            });
                        }
                    }
                }
                Err(error) => {
                    let reason = if error.is_timeout() {
                        "request timed out".to_string()
                    } else if error.is_connect() {
                        "connection failed".to_string()
                    } else {
                        error.to_string()
                    };
                    last_error = Some(CoreError::RemoteMetadataUnavailable {
                        resolver: self.name().to_string(),
                        reason,
                    });
                }
            }

            if attempt == 1 {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }

        Err(
            last_error.unwrap_or_else(|| CoreError::RemoteMetadataUnavailable {
                resolver: self.name().to_string(),
                reason: "unknown network error".to_string(),
            }),
        )
    }
}

impl RemoteMetadataResolver for CrossrefResolver {
    fn name(&self) -> &'static str {
        "crossref"
    }

    fn can_resolve(&self, identifier: &Identifier) -> bool {
        matches!(identifier, Identifier::DOI(_))
    }

    fn resolve<'a>(
        &'a self,
        ctx: &'a ResolveContext<'a>,
        identifier: &'a Identifier,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<SourceRecord>> + Send + 'a>>
    {
        Box::pin(async move {
            let Identifier::DOI(doi) = identifier else {
                return Err(CoreError::RemoteMetadataNotFound {
                    identifier: identifier.canonical(),
                });
            };

            let max_age = Duration::from_secs(30 * 24 * 60 * 60);
            if !ctx.force_refresh {
                if let Some(cached) = load_cached(ctx.vault_path, self.name(), identifier, max_age)?
                {
                    return Ok(cached);
                }
            }

            let body = self.fetch_with_retry(ctx, doi).await?;
            let record = crossref_response_to_source_record(doi, &body)?;
            save_cached(ctx.vault_path, self.name(), identifier, &record, &body)?;
            Ok(record)
        })
    }
}

fn crossref_response_to_source_record(doi: &str, body: &Value) -> Result<SourceRecord> {
    let message = body
        .get("message")
        .ok_or_else(|| CoreError::RemoteMetadataMalformed {
            identifier: doi.to_string(),
            reason: "missing 'message' object".to_string(),
        })?;

    let title = first_string_in_array(message.get("title")).ok_or_else(|| {
        CoreError::RemoteMetadataMalformed {
            identifier: doi.to_string(),
            reason: "missing title".to_string(),
        }
    })?;

    let authors: Vec<String> = message
        .get("author")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|author| {
                    let given = author.get("given").and_then(|v| v.as_str());
                    let family = author.get("family").and_then(|v| v.as_str());
                    match (given, family) {
                        (Some(g), Some(f)) => Some(format!("{f}, {g}")),
                        (None, Some(f)) => Some(f.to_string()),
                        (Some(g), None) => Some(g.to_string()),
                        (None, None) => None,
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    let published_year = extract_published_year(message);
    let item_type = map_crossref_type(message.get("type").and_then(|v| v.as_str()));
    let container_title = first_string_in_array(message.get("container-title"));
    let isbn = first_string_in_array(message.get("ISBN"));

    let mut external_identifiers = vec![ExternalIdentifier {
        namespace: "doi".to_string(),
        value: doi.to_string(),
    }];
    if let Some(isbn) = &isbn {
        external_identifiers.push(ExternalIdentifier {
            namespace: "isbn".to_string(),
            value: isbn.clone(),
        });
    }

    let mut raw_fields = BTreeMap::new();
    if let Some(container) = &container_title {
        raw_fields.insert("container-title".to_string(), container.clone());
    }
    if let Some(isbn) = &isbn {
        raw_fields.insert("isbn".to_string(), isbn.clone());
    }
    if let Some(ctype) = message.get("type").and_then(|v| v.as_str()) {
        raw_fields.insert("type".to_string(), ctype.to_string());
    }

    Ok(SourceRecord {
        record_id: Uuid::new_v4(),
        source_name: "Crossref".to_string(),
        external_id: Some(doi.to_string()),
        external_identifiers,
        title,
        authors,
        published_year,
        item_type,
        abstract_text: String::new(),
        keywords: Vec::new(),
        pages: String::new(),
        volume: String::new(),
        raw_fields,
    })
}

fn first_string_in_array(value: Option<&Value>) -> Option<String> {
    value
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.iter().find_map(|v| v.as_str().map(ToString::to_string)))
}

fn extract_published_year(message: &Value) -> Option<i32> {
    for key in ["published-print", "published", "issued"] {
        if let Some(year) = message
            .get(key)
            .and_then(|v| v.get("date-parts"))
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|v| v.as_i64())
        {
            return Some(year as i32);
        }
    }
    None
}

fn map_crossref_type(value: Option<&str>) -> String {
    match value {
        Some("journal-article") => "article".to_string(),
        Some("book") => "book".to_string(),
        Some("book-chapter") => "chapter".to_string(),
        Some(other) => other.to_string(),
        None => "other".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_response() -> Value {
        serde_json::json!({
            "message": {
                "title": ["Sample Article"],
                "author": [
                    {"given": "Alice", "family": "Smith"},
                    {"given": "Bob", "family": "Jones"}
                ],
                "published-print": {"date-parts": [[2021, 8, 15]]},
                "type": "journal-article",
                "container-title": ["Nature"],
                "DOI": "10.1038/s41586-021-03819-2",
                "ISBN": ["978-3-030-12345-6"]
            }
        })
    }

    #[test]
    fn parses_basic_crossref_response() {
        let record =
            crossref_response_to_source_record("10.1038/s41586-021-03819-2", &sample_response())
                .unwrap();
        assert_eq!(record.title, "Sample Article");
        assert_eq!(record.authors, vec!["Smith, Alice", "Jones, Bob"]);
        assert_eq!(record.published_year, Some(2021));
        assert_eq!(record.item_type, "article");
        assert_eq!(
            record.external_id,
            Some("10.1038/s41586-021-03819-2".to_string())
        );
        assert!(
            record
                .external_identifiers
                .iter()
                .any(|id| id.namespace == "doi")
        );
        assert!(
            record
                .external_identifiers
                .iter()
                .any(|id| id.namespace == "isbn")
        );
    }

    #[test]
    fn missing_title_is_malformed() {
        let body = serde_json::json!({"message": {"type": "journal-article"}});
        assert!(crossref_response_to_source_record("10.0000/x", &body).is_err());
    }
}
