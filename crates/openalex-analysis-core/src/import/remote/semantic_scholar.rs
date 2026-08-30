use std::{collections::BTreeMap, time::Duration};

use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde_json::Value;
use uuid::Uuid;

use crate::{CoreError, ExternalIdentifier, Result, import::source_record::SourceRecord};

use super::{
    Identifier, RemoteMetadataResolver, ResolveContext,
    cache::{load_cached, save_cached},
};

const DEFAULT_BASE_URL: &str = "https://api.semanticscholar.org/graph/v1/paper";
const USER_AGENT: &str = "cistella/0.1.0 (mailto:contact@example.com)";
const FIELDS: &str = "title,authors,year,publicationTypes,publicationDate,externalIds,venue";

/// Semantic Scholar resolver.
///
/// Handles `Identifier::DOI` and `Identifier::PMID`.
#[derive(Debug, Clone)]
pub struct SemanticScholarResolver {
    base_url: String,
}

impl Default for SemanticScholarResolver {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
        }
    }
}

impl SemanticScholarResolver {
    /// Creates a resolver pointing at a custom Semantic Scholar-compatible endpoint.
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
        }
    }

    fn url_for(&self, identifier: &Identifier) -> Option<String> {
        match identifier {
            Identifier::DOI(doi) => {
                let segment = format!("DOI:{}", doi);
                let encoded = utf8_percent_encode(&segment, NON_ALPHANUMERIC).to_string();
                Some(format!("{}/{}?fields={}", self.base_url, encoded, FIELDS))
            }
            Identifier::PMID(pmid) => {
                Some(format!("{}/PMID:{}?fields={}", self.base_url, pmid, FIELDS))
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

impl RemoteMetadataResolver for SemanticScholarResolver {
    fn name(&self) -> &'static str {
        "semantic_scholar"
    }

    fn can_resolve(&self, identifier: &Identifier) -> bool {
        matches!(identifier, Identifier::DOI(_) | Identifier::PMID(_))
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
            let record = semantic_scholar_response_to_source_record(identifier, &body)?;
            save_cached(ctx.vault_path, self.name(), identifier, &record, &body)?;
            Ok(record)
        })
    }
}

fn semantic_scholar_response_to_source_record(
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
        .get("authors")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|author| {
                    author
                        .get("name")
                        .and_then(|v| v.as_str())
                        .map(String::from)
                })
                .collect()
        })
        .unwrap_or_default();

    let published_year = body
        .get("year")
        .and_then(|v| v.as_i64())
        .map(|y| y as i32)
        .or_else(|| {
            body.get("publicationDate")
                .and_then(|v| v.as_str())
                .and_then(|s| s.chars().take(4).collect::<String>().parse::<i32>().ok())
        });

    let item_type =
        map_semantic_scholar_type(body.get("publicationTypes").and_then(|v| v.as_array()));

    let container_title = body.get("venue").and_then(|v| v.as_str()).map(String::from);

    let external_ids = body
        .get("externalIds")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let doi = external_ids
        .get("DOI")
        .and_then(|v| v.as_str())
        .map(String::from)
        .map(|s| s.to_ascii_lowercase());
    let pmid = external_ids
        .get("PMID")
        .and_then(|v| v.as_str())
        .map(String::from);
    let pmcid = external_ids
        .get("PMCID")
        .and_then(|v| v.as_str())
        .map(normalize_pmcid);

    let mut external_identifiers = Vec::new();
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
        Identifier::DOI(doi) => Some(doi.clone()),
        Identifier::PMID(pmid) => Some(pmid.clone()),
        _ => external_identifiers.first().map(|id| id.value.clone()),
    };

    let mut raw_fields = BTreeMap::new();
    if let Some(container) = &container_title {
        raw_fields.insert("journal".to_string(), container.clone());
        raw_fields.insert("container-title".to_string(), container.clone());
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
    if let Some(ctype) = body.get("publicationTypes").and_then(|v| v.as_array()) {
        if let Some(first) = ctype.iter().find_map(|v| v.as_str()) {
            raw_fields.insert("publication-types".to_string(), first.to_string());
        }
    }

    Ok(SourceRecord {
        record_id: Uuid::new_v4(),
        source_name: "Semantic Scholar".to_string(),
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

fn normalize_pmcid(value: &str) -> String {
    value
        .to_ascii_lowercase()
        .strip_prefix("pmc")
        .map(String::from)
        .unwrap_or_else(|| value.to_string())
}

fn map_semantic_scholar_type(publication_types: Option<&Vec<Value>>) -> String {
    let Some(first) = publication_types.and_then(|arr| arr.first()) else {
        return "other".to_string();
    };
    let Some(value) = first.as_str() else {
        return "other".to_string();
    };
    let lower = value.to_ascii_lowercase();
    if lower.contains("journalarticle") || lower.contains("journal article") {
        "article".to_string()
    } else if lower.contains("bookchapter") || lower.contains("book chapter") {
        "chapter".to_string()
    } else if lower.contains("book") {
        "book".to_string()
    } else {
        "other".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_semantic_scholar_body() -> Value {
        serde_json::json!({
            "paperId": "abc123",
            "title": "A Test Semantic Scholar Article",
            "authors": [
                {"name": "Smith, Alice"},
                {"name": "Jones, Bob"}
            ],
            "year": 2022,
            "publicationTypes": ["JournalArticle"],
            "publicationDate": "2022-06-15",
            "externalIds": {
                "DOI": "10.1234/example.12345",
                "PMID": "12345",
                "PMCID": "PMC67890"
            },
            "venue": "Journal of Examples"
        })
    }

    #[test]
    fn parses_basic_semantic_scholar_response() {
        let record = semantic_scholar_response_to_source_record(
            &Identifier::DOI("10.1234/example.12345".to_string()),
            &sample_semantic_scholar_body(),
        )
        .unwrap();
        assert_eq!(record.title, "A Test Semantic Scholar Article");
        assert_eq!(record.authors, vec!["Smith, Alice", "Jones, Bob"]);
        assert_eq!(record.published_year, Some(2022));
        assert_eq!(record.item_type, "article");
        assert_eq!(
            record.external_id,
            Some("10.1234/example.12345".to_string())
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
        let body = serde_json::json!({"paperId": "abc"});
        assert!(
            semantic_scholar_response_to_source_record(
                &Identifier::PMID("12345".to_string()),
                &body
            )
            .is_err()
        );
    }
}
