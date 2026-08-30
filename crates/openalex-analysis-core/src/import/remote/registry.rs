use std::path::Path;

use reqwest::Client;

use crate::{AppConfig, AppDirectories, CoreError, Result, import::source_record::SourceRecord};

use super::{
    CrossrefResolver, Identifier, OpenAlexApiResolver, PubMedResolver, RemoteMetadataResolver,
    ResolveContext, SemanticScholarResolver, identifier::parse_identifier,
};

/// Registry of remote metadata resolvers.
///
/// Selects the first resolver that claims it can handle the normalized
/// identifier, manages cache lookups/writes, and returns the resolved
/// `SourceRecord`.
#[derive(Default)]
pub struct RemoteResolverRegistry {
    resolvers: Vec<Box<dyn RemoteMetadataResolver>>,
    client: Client,
}

impl RemoteResolverRegistry {
    /// Creates a registry with all default resolvers enabled.
    pub fn new() -> Self {
        Self {
            resolvers: vec![
                Box::new(CrossrefResolver::default()),
                Box::new(PubMedResolver::default()),
                Box::new(SemanticScholarResolver::default()),
                Box::new(OpenAlexApiResolver::default()),
            ],
            client: Client::new(),
        }
    }

    /// Creates a registry from application configuration, registering only the
    /// resolvers that are enabled. Missing entries default to enabled.
    pub fn from_app_dirs(app_dirs: &AppDirectories) -> Result<Self> {
        let config = crate::load_app_config(app_dirs.app_config_path())?;
        Ok(Self::with_app_config(&config))
    }

    /// Creates a registry filtered by the supplied configuration.
    pub fn with_app_config(config: &AppConfig) -> Self {
        let all: Vec<Box<dyn RemoteMetadataResolver>> = vec![
            Box::new(CrossrefResolver::default()),
            Box::new(PubMedResolver::default()),
            Box::new(SemanticScholarResolver::default()),
            Box::new(OpenAlexApiResolver::default()),
        ];
        Self {
            resolvers: all
                .into_iter()
                .filter(|r| config.is_resolver_enabled(r.name()))
                .collect(),
            client: Client::new(),
        }
    }

    /// Creates a registry with a custom HTTP client (used in tests with a mock
    /// server) and all default resolvers.
    pub fn with_client(client: Client) -> Self {
        Self {
            resolvers: vec![
                Box::new(CrossrefResolver::default()),
                Box::new(PubMedResolver::default()),
                Box::new(SemanticScholarResolver::default()),
                Box::new(OpenAlexApiResolver::default()),
            ],
            client,
        }
    }

    /// Creates a registry with a Crossref resolver pointing at a custom base URL.
    pub fn with_crossref_base_url(base_url: impl Into<String>) -> Self {
        Self {
            resolvers: vec![Box::new(CrossrefResolver::with_base_url(base_url))],
            client: Client::new(),
        }
    }

    /// Creates a registry with a Crossref resolver pointing at a custom base URL
    /// and a custom HTTP client.
    pub fn with_crossref_base_url_and_client(base_url: impl Into<String>, client: Client) -> Self {
        Self {
            resolvers: vec![Box::new(CrossrefResolver::with_base_url(base_url))],
            client,
        }
    }

    /// Creates a registry with a PubMed resolver pointing at custom PubMed and
    /// Europe PMC base URLs.
    pub fn with_pubmed_base_urls(
        pubmed_url: impl Into<String>,
        europepmc_url: impl Into<String>,
    ) -> Self {
        Self {
            resolvers: vec![
                Box::new(CrossrefResolver::default()),
                Box::new(PubMedResolver::with_base_urls(pubmed_url, europepmc_url)),
            ],
            client: Client::new(),
        }
    }

    /// Creates a registry with a PubMed resolver pointing at custom PubMed and
    /// Europe PMC base URLs and a custom HTTP client.
    pub fn with_pubmed_base_urls_and_client(
        pubmed_url: impl Into<String>,
        europepmc_url: impl Into<String>,
        client: Client,
    ) -> Self {
        Self {
            resolvers: vec![
                Box::new(CrossrefResolver::default()),
                Box::new(PubMedResolver::with_base_urls(pubmed_url, europepmc_url)),
            ],
            client,
        }
    }

    /// Creates a registry with a Semantic Scholar resolver pointing at a custom
    /// base URL.
    pub fn with_semantic_scholar_base_url(base_url: impl Into<String>) -> Self {
        Self {
            resolvers: vec![Box::new(SemanticScholarResolver::with_base_url(base_url))],
            client: Client::new(),
        }
    }

    /// Creates a registry with a Semantic Scholar resolver pointing at a custom
    /// base URL and a custom HTTP client.
    pub fn with_semantic_scholar_base_url_and_client(
        base_url: impl Into<String>,
        client: Client,
    ) -> Self {
        Self {
            resolvers: vec![Box::new(SemanticScholarResolver::with_base_url(base_url))],
            client,
        }
    }

    /// Creates a registry with an OpenAlex API resolver pointing at a custom
    /// base URL.
    pub fn with_openalex_base_url(base_url: impl Into<String>) -> Self {
        Self {
            resolvers: vec![Box::new(OpenAlexApiResolver::with_base_url(base_url))],
            client: Client::new(),
        }
    }

    /// Creates a registry with an OpenAlex API resolver pointing at a custom
    /// base URL and a custom HTTP client.
    pub fn with_openalex_base_url_and_client(base_url: impl Into<String>, client: Client) -> Self {
        Self {
            resolvers: vec![Box::new(OpenAlexApiResolver::with_base_url(base_url))],
            client,
        }
    }

    /// Creates a registry with all resolvers pointing at custom base URLs (used
    /// in multi-resolver tests with mock servers).
    pub fn with_all_base_urls(
        crossref_url: impl Into<String>,
        pubmed_url: impl Into<String>,
        europepmc_url: impl Into<String>,
        semantic_scholar_url: impl Into<String>,
        openalex_url: impl Into<String>,
    ) -> Self {
        Self {
            resolvers: vec![
                Box::new(CrossrefResolver::with_base_url(crossref_url)),
                Box::new(PubMedResolver::with_base_urls(pubmed_url, europepmc_url)),
                Box::new(SemanticScholarResolver::with_base_url(semantic_scholar_url)),
                Box::new(OpenAlexApiResolver::with_base_url(openalex_url)),
            ],
            client: Client::new(),
        }
    }

    /// Creates a registry with all resolvers pointing at custom base URLs and a
    /// custom HTTP client.
    #[allow(clippy::too_many_arguments)]
    pub fn with_all_base_urls_and_client(
        crossref_url: impl Into<String>,
        pubmed_url: impl Into<String>,
        europepmc_url: impl Into<String>,
        semantic_scholar_url: impl Into<String>,
        openalex_url: impl Into<String>,
        client: Client,
    ) -> Self {
        Self {
            resolvers: vec![
                Box::new(CrossrefResolver::with_base_url(crossref_url)),
                Box::new(PubMedResolver::with_base_urls(pubmed_url, europepmc_url)),
                Box::new(SemanticScholarResolver::with_base_url(semantic_scholar_url)),
                Box::new(OpenAlexApiResolver::with_base_url(openalex_url)),
            ],
            client,
        }
    }

    /// Returns the first resolver that can handle the identifier, if any.
    pub fn find_resolver(&self, identifier: &Identifier) -> Option<&dyn RemoteMetadataResolver> {
        self.resolvers
            .iter()
            .find(|r| r.can_resolve(identifier))
            .map(|r| r.as_ref())
    }

    /// Returns the ordered list of enabled resolvers that can handle the
    /// identifier, according to the preferred resolution strategy.
    fn ordered_resolvers_for(&self, identifier: &Identifier) -> Vec<&dyn RemoteMetadataResolver> {
        let order: &[&str] = match identifier {
            Identifier::DOI(_) => &["crossref", "semantic_scholar", "openalex_api"],
            Identifier::PMID(_) => &["pubmed", "semantic_scholar", "openalex_api"],
            Identifier::OpenAlexId(_) => &["openalex_api"],
            Identifier::PMCID(_) => &["pubmed"],
            Identifier::ISBN(_) => &[],
        };

        order
            .iter()
            .filter_map(|name| {
                self.resolvers
                    .iter()
                    .find(|r| r.name() == *name && r.can_resolve(identifier))
                    .map(|r| r.as_ref())
            })
            .collect()
    }

    /// Resolves an identifier string using the current vault and a forced-refresh
    /// flag.
    pub async fn resolve(
        &self,
        vault_path: &Path,
        identifier: &str,
        force_refresh: bool,
    ) -> Result<SourceRecord> {
        let ctx = ResolveContext::new(vault_path, force_refresh, self.client.clone());
        self.resolve_with_context(&ctx, identifier).await
    }

    /// Resolves an identifier string using an explicit context.
    pub async fn resolve_with_context(
        &self,
        ctx: &ResolveContext<'_>,
        identifier: &str,
    ) -> Result<SourceRecord> {
        let parsed = parse_identifier(identifier)?;
        let resolver =
            self.find_resolver(&parsed)
                .ok_or_else(|| CoreError::RemoteMetadataNotFound {
                    identifier: parsed.canonical(),
                })?;
        resolver.resolve(ctx, &parsed).await
    }

    /// Resolves an identifier using multiple resolvers and merges the results by
    /// field completeness.
    ///
    /// Resolver order:
    /// - DOI: Crossref → Semantic Scholar → OpenAlex API
    /// - PMID: PubMed → Semantic Scholar → OpenAlex API
    /// - OpenAlex ID: OpenAlex API
    /// - PMCID: PubMed
    pub async fn resolve_best(
        &self,
        vault_path: &Path,
        identifier: &str,
        force_refresh: bool,
    ) -> Result<SourceRecord> {
        let ctx = ResolveContext::new(vault_path, force_refresh, self.client.clone());
        self.resolve_best_with_context(&ctx, identifier).await
    }

    /// Resolves an identifier using multiple resolvers with an explicit context.
    pub async fn resolve_best_with_context(
        &self,
        ctx: &ResolveContext<'_>,
        identifier: &str,
    ) -> Result<SourceRecord> {
        let parsed = parse_identifier(identifier)?;
        let resolvers = self.ordered_resolvers_for(&parsed);
        if resolvers.is_empty() {
            return Err(CoreError::RemoteMetadataNotFound {
                identifier: parsed.canonical(),
            });
        }

        let mut results = Vec::new();
        let mut errors = Vec::new();

        for resolver in resolvers {
            match resolver.resolve(ctx, &parsed).await {
                Ok(record) => results.push((resolver.name(), record)),
                Err(err) => errors.push(err),
            }
        }

        if results.is_empty() {
            return Err(prioritized_error(errors));
        }

        if results.len() == 1 {
            return Ok(results.into_iter().next().unwrap().1);
        }

        Ok(merge_source_records(results))
    }
}

fn prioritized_error(errors: Vec<CoreError>) -> CoreError {
    fn priority(error: &CoreError) -> i32 {
        match error {
            CoreError::RemoteMetadataNotFound { .. } => 3,
            CoreError::RemoteMetadataMalformed { .. } => 2,
            CoreError::RemoteMetadataUnavailable { .. } => 1,
            _ => 0,
        }
    }

    errors
        .into_iter()
        .max_by_key(|e| priority(e))
        .unwrap_or_else(|| CoreError::RemoteMetadataUnavailable {
            resolver: "registry".to_string(),
            reason: "all resolvers failed without a detailed error".to_string(),
        })
}

fn merge_source_records(results: Vec<(&str, SourceRecord)>) -> SourceRecord {
    let source_names: Vec<&str> = results.iter().map(|(name, _)| *name).collect();
    let mut merged = results[0].1.clone();

    for (_name, record) in results.iter().skip(1) {
        if merged.title.trim().is_empty() && !record.title.trim().is_empty() {
            merged.title = record.title.clone();
        }
        if merged.authors.is_empty() && !record.authors.is_empty() {
            merged.authors = record.authors.clone();
        }
        if merged.published_year.is_none() && record.published_year.is_some() {
            merged.published_year = record.published_year;
        }
        if merged.item_type.is_empty() || merged.item_type == "other" {
            if !record.item_type.is_empty() && record.item_type != "other" {
                merged.item_type = record.item_type.clone();
            }
        }

        // Prefer a non-empty container title from any resolver.
        let merged_container = merged
            .raw_fields
            .get("container-title")
            .or_else(|| merged.raw_fields.get("journal"));
        let record_container = record
            .raw_fields
            .get("container-title")
            .or_else(|| record.raw_fields.get("journal"));
        if merged_container
            .map(|s| s.trim().is_empty())
            .unwrap_or(true)
        {
            if let Some(container) = record_container {
                if !container.trim().is_empty() {
                    merged
                        .raw_fields
                        .insert("container-title".to_string(), container.clone());
                    merged
                        .raw_fields
                        .insert("journal".to_string(), container.clone());
                }
            }
        }

        // Merge external identifiers, deduplicated by namespace + case-insensitive value.
        for id in &record.external_identifiers {
            if !merged.external_identifiers.iter().any(|existing| {
                existing.namespace == id.namespace && existing.value.eq_ignore_ascii_case(&id.value)
            }) {
                merged.external_identifiers.push(id.clone());
            }
        }

        // Merge raw fields: fill missing keys or overwrite empty values.
        for (key, value) in &record.raw_fields {
            if value.trim().is_empty() {
                continue;
            }
            match merged.raw_fields.get(key) {
                Some(existing) if !existing.trim().is_empty() => {}
                _ => {
                    merged.raw_fields.insert(key.clone(), value.clone());
                }
            }
        }
    }

    merged.source_name = source_names.join(" + ");
    merged
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::ExternalIdentifier;

    fn record_with_title(title: &str) -> SourceRecord {
        SourceRecord {
            record_id: uuid::Uuid::new_v4(),
            source_name: "Test".to_string(),
            external_id: None,
            external_identifiers: Vec::new(),
            title: title.to_string(),
            authors: Vec::new(),
            published_year: None,
            item_type: "other".to_string(),
            abstract_text: String::new(),
            keywords: Vec::new(),
            pages: String::new(),
            volume: String::new(),
            raw_fields: BTreeMap::new(),
        }
    }

    #[test]
    fn merge_prefers_non_empty_title_and_year() {
        let first = record_with_title("");
        let mut second = record_with_title("Filled Title");
        second.published_year = Some(2021);
        second.item_type = "article".to_string();

        let merged = merge_source_records(vec![("a", first), ("b", second)]);
        assert_eq!(merged.title, "Filled Title");
        assert_eq!(merged.published_year, Some(2021));
        assert_eq!(merged.item_type, "article");
        assert_eq!(merged.source_name, "a + b");
    }

    #[test]
    fn merge_deduplicates_external_identifiers() {
        let mut first = record_with_title("First");
        first.external_identifiers = vec![
            ExternalIdentifier {
                namespace: "doi".to_string(),
                value: "10.1234/example".to_string(),
            },
            ExternalIdentifier {
                namespace: "pmid".to_string(),
                value: "12345".to_string(),
            },
        ];
        let mut second = record_with_title("Second");
        second.external_identifiers = vec![
            ExternalIdentifier {
                namespace: "doi".to_string(),
                value: "10.1234/EXAMPLE".to_string(),
            },
            ExternalIdentifier {
                namespace: "pmcid".to_string(),
                value: "67890".to_string(),
            },
        ];

        let merged = merge_source_records(vec![("a", first), ("b", second)]);
        assert_eq!(merged.external_identifiers.len(), 3);
        assert!(
            merged
                .external_identifiers
                .iter()
                .any(|id| id.namespace == "doi")
        );
        assert!(
            merged
                .external_identifiers
                .iter()
                .any(|id| id.namespace == "pmid")
        );
        assert!(
            merged
                .external_identifiers
                .iter()
                .any(|id| id.namespace == "pmcid")
        );
    }

    #[test]
    fn app_config_filters_disabled_resolvers() {
        let mut config = AppConfig::default();
        config
            .remote_resolvers_enabled
            .insert("crossref".to_string(), false);
        config
            .remote_resolvers_enabled
            .insert("openalex_api".to_string(), false);

        let registry = RemoteResolverRegistry::with_app_config(&config);
        assert!(
            registry
                .find_resolver(&Identifier::DOI("10.1234/x".to_string()))
                .is_some()
        );
        assert!(
            registry
                .find_resolver(&Identifier::OpenAlexId("W1".to_string()))
                .is_none()
        );
        assert!(
            registry
                .find_resolver(&Identifier::DOI("10.1234/x".to_string()))
                .unwrap()
                .name()
                != "crossref"
        );
    }
}
