pub mod dataset;
pub mod error;
pub mod export;
pub mod import;
pub mod layout;
pub mod manifest;
pub mod metrics;
pub mod query;
pub mod schema;

pub use dataset::{Dataset, DatasetOpenOptions};
pub use error::{CoreError, Result};
pub use export::{export_dataframe, ExportFormat};
pub use import::{ImportOptions, import_openalex_sources};
pub use layout::StorageLayout;
pub use manifest::{DatasetManifest, TableManifest};
pub use metrics::MetricCode;
pub use query::{SourceSearchQuery, SourceSummary};
pub use schema::TableName;

