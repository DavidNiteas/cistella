use std::{collections::BTreeMap, time::Duration};

use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde_json::Value;
use uuid::Uuid;

use crate::{CoreError, ExternalIdentifier, Result, import::source_record::SourceRecord};

use super::{
    Identifier, RemoteMetadataResolver, ResolveContext,
    cache::{load_cached, save_cached},
};

const DEFAULT_BASE_URL: &str = "https://api.openalex.org";
const USER_AGENT: &str = "cistella/0.1.0 (mailto:contact@example.com)";

/// OpenAlex API resolver.
///
/// Handles `Identifier::OpenAlexId` and `Identifier::DOI`.
#[derive(Debug, Clone)]
pub struct OpenAlexApiResolver {
    base_url: String,
}

impl Default for OpenAlexApiResolver {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
        }
    }
}

impl OpenAlexApiResolver {
    /// Creates a resolver pointing at a custom OpenAlex-compatible endpoint.
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
        }
    }

    fn url_for(&self, identifier: &Identifier) -> Option<String> {
        match identifier {
            Identifier::OpenAlexId(id) => Some(format!("{}/works/{}", self.base_url, id)),
            Identifier::DOI(doi) => {
                // Encode the whole "doi:<doi>" segment so slashes and colons are safe.
                let segment = format!("doi:{}", doi);
                let encoded = utf8_percent_encode(&segment, NON_ALPHANUMERIC).to_string();
                Some(format!("{}/works/{}", self.base_url, encoded))
            }
            _ => None,
        }
    }

    async fn fetch_with_retry(&self, ctx: &ResolveContext<'_>, url: &str) -> Result<Value> {
        let mut last_error: Option<CoreError> = None;

        for attempt in 1..=2 {
            let request = ctx
                .client
                .get(url)
                .timeout(ctx.timeout)
                .header("User-Agent", USER_AGENT)
                .send();

            match request.await {
                Ok(response) => {
                    let status = response.status();
                    if status == reqwest::StatusCode::NOT_FOUND {
                        return Err(CoreError::RemoteMetadataNotFound {
                            identifier: url.to_string(),
                        });
                    }
                    if !status.is_success() {
                        last_error = Some(CoreError::RemoteMetadataUnavailable {
                            resolver: self.name().to_string(),
                            reason: format!("HTTP {status}"),
                        });
                        if status.is_client_error() {
                            break;
                        }
                        continue;
                    }
                    match response.json::<Value>().await {
                        Ok(body) => return Ok(body),
                        Err(error) => {
                            last_error = Some(CoreError::RemoteMetadataMalformed {
                                identifier: url.to_string(),
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

impl RemoteMetadataResolver for OpenAlexApiResolver {
    fn name(&self) -> &'static str {
        "openalex_api"
    }

    fn can_resolve(&self, identifier: &Identifier) -> bool {
        matches!(identifier, Identifier::OpenAlexId(_) | Identifier::DOI(_))
    }

    fn resolve<'a>(
        &'a self,
        ctx: &'a ResolveContext<'a>,
        identifier: &'a Identifier,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<SourceRecord>> + Send + 'a>>
    {
        Box::pin(async move {
            let Some(url) = self.url_for(identifier) else {
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

            let body = self.fetch_with_retry(ctx, &url).await?;
            let record = openalex_response_to_source_record(identifier, &body)?;
            save_cached(ctx.vault_path, self.name(), identifier, &record, &body)?;
            Ok(record)
        })
    }
}

fn openalex_response_to_source_record(
    identifier: &Identifier,
    body: &Value,
) -> Result<SourceRecord> {
    let title = body.get("title").and_then(|v| v.as_str()).ok_or_else(|| {
        CoreError::RemoteMetadataMalformed {
            identifier: identifier.canonical(),
            reason: "missing title".to_string(),
        }
    })?;

    let authors: Vec<String> = body
        .get("authorships")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|authorship| {
                    authorship
                        .get("author")
                        .and_then(|a| a.get("display_name"))
                        .and_then(|v| v.as_str())
                        .map(String::from)
                })
                .collect()
        })
        .unwrap_or_default();

    let published_year = body
        .get("publication_year")
        .and_then(|v| v.as_i64())
        .map(|y| y as i32);

    let item_type = map_openalex_type(body.get("type").and_then(|v| v.as_str()));

    let container_title = body
        .get("primary_location")
        .and_then(|v| v.get("source"))
        .and_then(|v| v.get("display_name"))
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(|| {
            body.get("locations")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
                .and_then(|loc| loc.get("source"))
                .and_then(|v| v.get("display_name"))
                .and_then(|v| v.as_str())
                .map(String::from)
        });

    let ids = body
        .get("ids")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let doi = ids
        .get("doi")
        .and_then(|v| v.as_str())
        .map(normalize_doi)
        .or_else(|| body.get("doi").and_then(|v| v.as_str()).map(normalize_doi));
    let pmid = ids.get("pmid").and_then(|v| v.as_str()).map(String::from);
    let pmcid = ids
        .get("pmcid")
        .and_then(|v| v.as_str())
        .map(normalize_pmcid);
    let openalex_id = ids
        .get("openalex")
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(|| {
            body.get("id")
                .and_then(|v| v.as_str())
                .map(extract_openalex_id)
        });

    let mut external_identifiers = Vec::new();
    if let Some(openalex_id) = &openalex_id {
        external_identifiers.push(ExternalIdentifier {
            namespace: "openalex".to_string(),
            value: openalex_id.clone(),
        });
    }
    if let Some(doi) = &doi {
        external_identifiers.push(ExternalIdentifier {
            namespace: "doi".to_string(),
            value: doi.clone(),
        });
    }
    if let Some(pmid) = &pmid {
        external_identifiers.push(ExternalIdentifier {
            namespace: "pmid".to_string(),
            value: pmid.clone(),
        });
    }
    if let Some(pmcid) = &pmcid {
        external_identifiers.push(ExternalIdentifier {
            namespace: "pmcid".to_string(),
            value: pmcid.clone(),
        });
    }

    let external_id = match identifier {
        Identifier::OpenAlexId(_) => openalex_id.clone(),
        Identifier::DOI(doi) => Some(doi.clone()),
        _ => external_identifiers.first().map(|id| id.value.clone()),
    };

    let mut raw_fields = BTreeMap::new();
    if let Some(container) = &container_title {
        raw_fields.insert("container-title".to_string(), container.clone());
        raw_fields.insert("journal".to_string(), container.clone());
    }
    if let Some(doi) = &doi {
        raw_fields.insert("doi".to_string(), doi.clone());
    }
    if let Some(pmid) = &pmid {
        raw_fields.insert("pmid".to_string(), pmid.clone());
    }
    if let Some(pmcid) = &pmcid {
        raw_fields.insert("pmcid".to_string(), pmcid.clone());
    }
    if let Some(openalex_id) = &openalex_id {
        raw_fields.insert("openalex".to_string(), openalex_id.clone());
    }
    if let Some(ctype) = body.get("type").and_then(|v| v.as_str()) {
        raw_fields.insert("type".to_string(), ctype.to_string());
    }

    Ok(SourceRecord {
        record_id: Uuid::new_v4(),
        source_name: "OpenAlex API".to_string(),
        external_id,
        external_identifiers,
        title: title.to_string(),
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

fn normalize_doi(value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    lower
        .strip_prefix("https://doi.org/")
        .or_else(|| lower.strip_prefix("http://doi.org/"))
        .or_else(|| lower.strip_prefix("doi:"))
        .map(String::from)
        .unwrap_or_else(|| value.to_string())
}

fn normalize_pmcid(value: &str) -> String {
    value
        .to_ascii_lowercase()
        .strip_prefix("pmc")
        .map(String::from)
        .unwrap_or_else(|| value.to_string())
}

fn extract_openalex_id(url: &str) -> String {
    url.rsplit('/').next().unwrap_or(url).to_string()
}

fn map_openalex_type(value: Option<&str>) -> String {
    match value {
        Some("article") => "article".to_string(),
        Some("book") => "book".to_string(),
        Some("book-chapter") => "chapter".to_string(),
        Some(other) => other.to_string(),
        None => "other".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_openalex_body() -> Value {
        serde_json::json!({
            "id": "https://openalex.org/W123456789",
            "title": "A Test OpenAlex Article",
            "authorships": [
                {"author": {"display_name": "Smith, Alice"}},
                {"author": {"display_name": "Jones, Bob"}}
            ],
            "publication_year": 2023,
            "type": "article",
            "primary_location": {
                "source": {"display_name": "Journal of Examples"}
            },
            "ids": {
                "openalex": "W123456789",
                "doi": "https://doi.org/10.1234/example.12345",
                "pmid": "12345",
                "pmcid": "PMC67890"
            }
        })
    }

    #[test]
    fn parses_basic_openalex_response() {
        let record = openalex_response_to_source_record(
            &Identifier::OpenAlexId("W123456789".to_string()),
            &sample_openalex_body(),
        )
        .unwrap();
        assert_eq!(record.title, "A Test OpenAlex Article");
        assert_eq!(record.authors, vec!["Smith, Alice", "Jones, Bob"]);
        assert_eq!(record.published_year, Some(2023));
        assert_eq!(record.item_type, "article");
        assert_eq!(record.external_id, Some("W123456789".to_string()));
        assert!(
            record
                .external_identifiers
                .iter()
                .any(|id| id.namespace == "openalex")
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
                .any(|id| id.namespace == "pmid")
        );
    }

    #[test]
    fn missing_title_is_malformed() {
        let body = serde_json::json!({"id": "https://openalex.org/W1", "type": "article"});
        assert!(
            openalex_response_to_source_record(&Identifier::DOI("10.0000/x".to_string()), &body)
                .is_err()
        );
    }
}
