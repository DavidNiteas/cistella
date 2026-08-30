pub mod cache;
pub mod crossref;
pub mod identifier;
pub mod registry;
pub mod resolver;

pub use crossref::CrossrefResolver;
pub use identifier::{Identifier, parse_identifier};
pub use registry::RemoteResolverRegistry;
pub use resolver::{RemoteMetadataResolver, ResolveContext};
