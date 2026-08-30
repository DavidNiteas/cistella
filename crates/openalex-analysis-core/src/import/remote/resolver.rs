use std::{future::Future, path::Path, pin::Pin, time::Duration};

use reqwest::Client;

use crate::{Result, import::source_record::SourceRecord};

use super::identifier::Identifier;

/// Context passed to every remote metadata resolver invocation.
#[derive(Debug, Clone)]
pub struct ResolveContext<'a> {
    pub vault_path: &'a Path,
    pub force_refresh: bool,
    pub client: Client,
    pub timeout: Duration,
}

impl<'a> ResolveContext<'a> {
    /// Creates a context with the supplied `reqwest` client and a 10-second timeout.
    pub fn new(vault_path: &'a Path, force_refresh: bool, client: Client) -> Self {
        Self {
            vault_path,
            force_refresh,
            client,
            timeout: Duration::from_secs(10),
        }
    }
}

/// Resolves a normalized identifier into a `SourceRecord` using a remote API.
pub trait RemoteMetadataResolver: Send + Sync {
    /// Short, lowercase name used for logging and cache directory layout.
    fn name(&self) -> &'static str;

    /// Whether this resolver can handle the given identifier type.
    fn can_resolve(&self, identifier: &Identifier) -> bool;

    /// Resolve the identifier, returning a fully populated `SourceRecord`.
    fn resolve<'a>(
        &'a self,
        ctx: &'a ResolveContext<'a>,
        identifier: &'a Identifier,
    ) -> Pin<Box<dyn Future<Output = Result<SourceRecord>> + Send + 'a>>;
}
