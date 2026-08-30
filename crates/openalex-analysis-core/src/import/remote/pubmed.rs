use std::{collections::BTreeMap, time::Duration};

use serde_json::Value;
use uuid::Uuid;

use crate::{CoreError, ExternalIdentifier, Result, import::source_record::SourceRecord};

use super::{
    Identifier, RemoteMetadataResolver, ResolveContext,
    cache::{load_cached, save_cached},
};

const DEFAULT_PUBMED_BASE_URL: &str = "https://eutils.ncbi.nlm.nih.gov/entrez/eutils";
const DEFAULT_EUROPEPMC_BASE_URL: &str = "https://www.ebi.ac.uk/europepmc/webservices/rest";
const USER_AGENT: &str = "cistella/0.1.0 (mailto:contact@example.com)";

/// PubMed / Europe PMC resolver.
///
/// - `PMID`: queries PubMed E-utilities first, then falls back to Europe PMC.
/// - `PMCID`: queries Europe PMC directly.
#[derive(Debug, Clone)]
pub struct PubMedResolver {
    pubmed_base_url: String,
    europepmc_base_url: String,
}

impl Default for PubMedResolver {
    fn default() -> Self {
        Self {
            pubmed_base_url: DEFAULT_PUBMED_BASE_URL.to_string(),
            europepmc_base_url: DEFAULT_EUROPEPMC_BASE_URL.to_string(),
        }
    }
}

impl PubMedResolver {
    /// Creates a resolver pointing at custom PubMed- and Europe PMC-compatible
    /// endpoints (used in tests with a mock server).
    pub fn with_base_urls(
        pubmed_base_url: impl Into<String>,
        europepmc_base_url: impl Into<String>,
    ) -> Self {
        Self {
            pubmed_base_url: pubmed_base_url.into(),
            europepmc_base_url: europepmc_base_url.into(),
        }
    }

    fn namespace_for(identifier: &Identifier) -> &'static str {
        match identifier {
            Identifier::PMID(_) => "pubmed",
            Identifier::PMCID(_) => "pmc",
            _ => "pubmed",
        }
    }

    fn pubmed_summary_url(&self, pmid: &str) -> String {
        format!(
            "{}/esummary.fcgi?db=pubmed&id={}&retmode=json",
            self.pubmed_base_url, pmid
        )
    }

    fn europepmc_search_url(&self, query: &str) -> String {
        format!(
            "{}/search?query={query}&format=json",
            self.europepmc_base_url
        )
    }

    async fn fetch_json(&self, ctx: &ResolveContext<'_>, url: &str) -> Result<Value> {
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

    async fn resolve_pmid(
        &self,
        ctx: &ResolveContext<'_>,
        identifier: &Identifier,
        pmid: &str,
    ) -> Result<SourceRecord> {
        let pubmed_url = self.pubmed_summary_url(pmid);
        match self.fetch_json(ctx, &pubmed_url).await {
            Ok(body) => match pubmed_summary_to_source_record(pmid, &body) {
                Ok(record) => {
                    save_cached(
                        ctx.vault_path,
                        Self::namespace_for(identifier),
                        identifier,
                        &record,
                        &body,
                    )?;
                    Ok(record)
                }
                Err(_) => self.resolve_pmid_via_europepmc(ctx, identifier, pmid).await,
            },
            Err(_) => self.resolve_pmid_via_europepmc(ctx, identifier, pmid).await,
        }
    }

    async fn resolve_pmid_via_europepmc(
        &self,
        ctx: &ResolveContext<'_>,
        identifier: &Identifier,
        pmid: &str,
    ) -> Result<SourceRecord> {
        let url = self.europepmc_search_url(&format!("EXT_ID:{pmid}"));
        let body = self.fetch_json(ctx, &url).await?;
        let record = europepmc_search_to_source_record(&body, Some(pmid), None)?;
        save_cached(
            ctx.vault_path,
            Self::namespace_for(identifier),
            identifier,
            &record,
            &body,
        )?;
        Ok(record)
    }

    async fn resolve_pmcid(
        &self,
        ctx: &ResolveContext<'_>,
        identifier: &Identifier,
        pmcid: &str,
    ) -> Result<SourceRecord> {
        let url = self.europepmc_search_url(&format!("PMCID:{pmcid}"));
        let body = self.fetch_json(ctx, &url).await?;
        let record = europepmc_search_to_source_record(&body, None, Some(pmcid))?;
        save_cached(
            ctx.vault_path,
            Self::namespace_for(identifier),
            identifier,
            &record,
            &body,
        )?;
        Ok(record)
    }
}

impl RemoteMetadataResolver for PubMedResolver {
    fn name(&self) -> &'static str {
        "pubmed"
    }

    fn can_resolve(&self, identifier: &Identifier) -> bool {
        matches!(identifier, Identifier::PMID(_) | Identifier::PMCID(_))
    }

    fn resolve<'a>(
        &'a self,
        ctx: &'a ResolveContext<'a>,
        identifier: &'a Identifier,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<SourceRecord>> + Send + 'a>>
    {
        Box::pin(async move {
            let max_age = Duration::from_secs(30 * 24 * 60 * 60);
            if !ctx.force_refresh {
                if let Some(cached) = load_cached(
                    ctx.vault_path,
                    Self::namespace_for(identifier),
                    identifier,
                    max_age,
                )? {
                    return Ok(cached);
                }
            }

            match identifier {
                Identifier::PMID(pmid) => self.resolve_pmid(ctx, identifier, pmid).await,
                Identifier::PMCID(pmcid) => self.resolve_pmcid(ctx, identifier, pmcid).await,
                _ => Err(CoreError::RemoteMetadataNotFound {
                    identifier: identifier.canonical(),
                }),
            }
        })
    }
}

fn pubmed_summary_to_source_record(pmid: &str, body: &Value) -> Result<SourceRecord> {
    let result_obj = body
        .get("result")
        .ok_or_else(|| CoreError::RemoteMetadataMalformed {
            identifier: pmid.to_string(),
            reason: "missing 'result' object".to_string(),
        })?;

    let uids = result_obj
        .get("uids")
        .and_then(|v| v.as_array())
        .ok_or_else(|| CoreError::RemoteMetadataMalformed {
            identifier: pmid.to_string(),
            reason: "missing 'result.uids'".to_string(),
        })?;

    if uids.is_empty() {
        return Err(CoreError::RemoteMetadataNotFound {
            identifier: pmid.to_string(),
        });
    }

    let uid = uids[0]
        .as_str()
        .ok_or_else(|| CoreError::RemoteMetadataMalformed {
            identifier: pmid.to_string(),
            reason: "non-string uid".to_string(),
        })?;

    let article = result_obj
        .get(uid)
        .ok_or_else(|| CoreError::RemoteMetadataMalformed {
            identifier: pmid.to_string(),
            reason: format!("missing article object for uid {uid}"),
        })?;

    let title = article
        .get("title")
        .and_then(|v| v.as_str())
        .ok_or_else(|| CoreError::RemoteMetadataMalformed {
            identifier: pmid.to_string(),
            reason: "missing title".to_string(),
        })?;

    let authors: Vec<String> = article
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

    let published_year = article
        .get("pubdate")
        .and_then(|v| v.as_str())
        .and_then(|s| s.chars().take(4).collect::<String>().parse::<i32>().ok());

    let item_type =
        map_pubmed_type(article.get("doctype").and_then(|v| v.as_str()).or_else(|| {
            article
                .get("pubtype")
                .and_then(|v| v.as_array()?.first()?.as_str())
        }));

    let container_title = article
        .get("source")
        .and_then(|v| v.as_str())
        .map(String::from);

    let article_ids = article.get("articleids").and_then(|v| v.as_array());
    let mut doi = None;
    let mut pmcid = None;
    if let Some(ids) = article_ids {
        for id_entry in ids {
            let id_type = id_entry
                .get("idtype")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let value = id_entry.get("value").and_then(|v| v.as_str());
            match id_type {
                "doi" => doi = value.map(String::from),
                "pmc" => pmcid = value.map(normalize_pmcid_value),
                _ => {}
            }
        }
    }

    let mut external_identifiers = vec![ExternalIdentifier {
        namespace: "pmid".to_string(),
        value: pmid.to_string(),
    }];
    if let Some(pmcid) = &pmcid {
        external_identifiers.push(ExternalIdentifier {
            namespace: "pmcid".to_string(),
            value: pmcid.clone(),
        });
    }
    if let Some(doi) = &doi {
        external_identifiers.push(ExternalIdentifier {
            namespace: "doi".to_string(),
            value: doi.clone(),
        });
    }

    let mut raw_fields = BTreeMap::new();
    raw_fields.insert("pmid".to_string(), pmid.to_string());
    if let Some(container) = &container_title {
        raw_fields.insert("journal".to_string(), container.clone());
    }
    if let Some(doi) = &doi {
        raw_fields.insert("doi".to_string(), doi.clone());
    }
    if let Some(pmcid) = &pmcid {
        raw_fields.insert("pmcid".to_string(), pmcid.clone());
    }

    Ok(SourceRecord {
        record_id: Uuid::new_v4(),
        source_name: "PubMed".to_string(),
        external_id: Some(pmid.to_string()),
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

fn europepmc_search_to_source_record(
    body: &Value,
    expected_pmid: Option<&str>,
    expected_pmcid: Option<&str>,
) -> Result<SourceRecord> {
    let result = body
        .get("resultList")
        .and_then(|v| v.get("result"))
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .ok_or_else(|| {
            let id = expected_pmid
                .or(expected_pmcid)
                .unwrap_or("unknown")
                .to_string();
            CoreError::RemoteMetadataNotFound { identifier: id }
        })?;

    let title = result
        .get("title")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            let id = expected_pmid
                .or(expected_pmcid)
                .unwrap_or("unknown")
                .to_string();
            CoreError::RemoteMetadataMalformed {
                identifier: id,
                reason: "missing title".to_string(),
            }
        })?;

    let authors: Vec<String> = result
        .get("authorString")
        .and_then(|v| v.as_str())
        .map(|s| s.split(", ").map(String::from).collect())
        .unwrap_or_default();

    let published_year = result
        .get("pubYear")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<i32>().ok());

    let item_type = map_pubmed_type(result.get("pubType").and_then(|v| v.as_str()));

    let container_title = result
        .get("journalTitle")
        .and_then(|v| v.as_str())
        .map(String::from);

    let pmid = result
        .get("pmid")
        .and_then(|v| v.as_str())
        .map(String::from);
    let pmcid = result
        .get("pmcid")
        .and_then(|v| v.as_str())
        .map(normalize_pmcid_value);
    let doi = result.get("doi").and_then(|v| v.as_str()).map(String::from);

    let primary_id = expected_pmcid
        .map(String::from)
        .or_else(|| pmid.clone())
        .unwrap_or_default();

    let mut external_identifiers = Vec::new();
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
    if let Some(doi) = &doi {
        external_identifiers.push(ExternalIdentifier {
            namespace: "doi".to_string(),
            value: doi.clone(),
        });
    }

    if external_identifiers.is_empty() {
        return Err(CoreError::RemoteMetadataMalformed {
            identifier: primary_id.clone(),
            reason: "no usable external identifiers".to_string(),
        });
    }

    let mut raw_fields = BTreeMap::new();
    if let Some(pmid) = &pmid {
        raw_fields.insert("pmid".to_string(), pmid.clone());
    }
    if let Some(pmcid) = &pmcid {
        raw_fields.insert("pmcid".to_string(), pmcid.clone());
    }
    if let Some(doi) = &doi {
        raw_fields.insert("doi".to_string(), doi.clone());
    }
    if let Some(container) = &container_title {
        raw_fields.insert("journal".to_string(), container.clone());
    }

    Ok(SourceRecord {
        record_id: Uuid::new_v4(),
        source_name: "Europe PMC".to_string(),
        external_id: Some(primary_id),
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

fn normalize_pmcid_value(value: &str) -> String {
    value
        .to_ascii_lowercase()
        .strip_prefix("pmc")
        .map(String::from)
        .unwrap_or_else(|| value.to_string())
}

fn map_pubmed_type(value: Option<&str>) -> String {
    match value {
        Some(v) => {
            let lower = v.to_ascii_lowercase();
            if lower.contains("journal article") || lower.contains("journal-article") {
                "article".to_string()
            } else if lower.contains("book chapter") || lower.contains("book-chapter") {
                "chapter".to_string()
            } else if lower.contains("book") {
                "book".to_string()
            } else {
                "other".to_string()
            }
        }
        None => "other".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_pubmed_body() -> Value {
        serde_json::json!({
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
        })
    }

    fn sample_europepmc_body() -> Value {
        serde_json::json!({
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
        })
    }

    #[test]
    fn parses_basic_pubmed_response() {
        let record = pubmed_summary_to_source_record("12345", &sample_pubmed_body()).unwrap();
        assert_eq!(record.title, "A Test PubMed Article");
        assert_eq!(record.authors, vec!["Smith AB", "Doe J"]);
        assert_eq!(record.published_year, Some(2021));
        assert_eq!(record.item_type, "article");
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
    }

    #[test]
    fn parses_basic_europepmc_response() {
        let record =
            europepmc_search_to_source_record(&sample_europepmc_body(), Some("12345"), None)
                .unwrap();
        assert_eq!(record.title, "A Test Europe PMC Article");
        assert_eq!(record.authors, vec!["Smith AB", "Doe J"]);
        assert_eq!(record.published_year, Some(2022));
        assert_eq!(record.item_type, "article");
        assert_eq!(record.source_name, "Europe PMC");
        assert!(
            record
                .external_identifiers
                .iter()
                .any(|id| id.namespace == "pmid")
        );
    }

    #[test]
    fn missing_pubmed_article_falls_back_to_not_found() {
        let body = serde_json::json!({"result": {"uids": []}});
        let err = pubmed_summary_to_source_record("12345", &body).unwrap_err();
        assert!(err.to_string().contains("remote metadata not found"));
    }

    #[test]
    fn missing_europepmc_result_is_not_found() {
        let body = serde_json::json!({"resultList": {"result": []}});
        let err = europepmc_search_to_source_record(&body, Some("12345"), None).unwrap_err();
        assert!(err.to_string().contains("remote metadata not found"));
    }
}
