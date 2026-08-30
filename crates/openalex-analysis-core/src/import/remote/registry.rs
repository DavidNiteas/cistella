use std::path::Path;

use reqwest::Client;

use crate::{CoreError, Result, import::source_record::SourceRecord};

use super::{
    CrossrefResolver, Identifier, PubMedResolver, RemoteMetadataResolver, ResolveContext,
    identifier::parse_identifier,
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
    /// Creates a registry with the default Crossref and PubMed resolvers.
    pub fn new() -> Self {
        Self {
            resolvers: vec![
                Box::new(CrossrefResolver::default()),
                Box::new(PubMedResolver::default()),
            ],
            client: Client::new(),
        }
    }

    /// Creates a registry with a custom HTTP client (used in tests with a mock
    /// server) and the default Crossref and PubMed resolvers.
    pub fn with_client(client: Client) -> Self {
        Self {
            resolvers: vec![
                Box::new(CrossrefResolver::default()),
                Box::new(PubMedResolver::default()),
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

    /// Returns the first resolver that can handle the identifier, if any.
    pub fn find_resolver(&self, identifier: &Identifier) -> Option<&dyn RemoteMetadataResolver> {
        self.resolvers
            .iter()
            .find(|r| r.can_resolve(identifier))
            .map(|r| r.as_ref())
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
}
