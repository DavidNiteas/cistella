pub mod cache;
pub mod crossref;
pub mod identifier;
pub mod openalex_api;
pub mod pubmed;
pub mod registry;
pub mod resolver;
pub mod semantic_scholar;

pub use crossref::CrossrefResolver;
pub use identifier::{Identifier, parse_identifier};
pub use openalex_api::OpenAlexApiResolver;
pub use pubmed::PubMedResolver;
pub use registry::RemoteResolverRegistry;
pub use resolver::{RemoteMetadataResolver, ResolveContext};
pub use semantic_scholar::SemanticScholarResolver;
