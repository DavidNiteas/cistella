pub mod app_dirs;
pub mod backup;
pub mod dataset;
pub mod error;
pub mod export;
pub mod import;
pub mod layout;
pub mod literature;
pub mod manifest;
pub mod metrics;
pub mod notes;
pub mod query;
pub mod release;
pub mod schema;
pub mod search;
mod secure_user_records;
pub mod source_adapter;
pub mod vault;

pub use app_dirs::{
    AppConfig, AppDirectories, RecentVault, WindowSize, load_app_config, load_recent_vaults,
    make_vault_paths_portable, migrate_vaults_to_installed, migrate_vaults_to_portable,
    resolve_recent_vault_paths, save_app_config, save_recent_vaults,
};
pub use backup::{backup_vault, restore_vault};
pub use dataset::{Dataset, DatasetOpenOptions};
pub use error::{CoreError, Result};
pub use export::{ExportFormat, export_dataframe};
pub use import::remote::{
    CrossrefResolver, Identifier, PubMedResolver, RemoteMetadataResolver, RemoteResolverRegistry,
    ResolveContext,
};
pub use import::{
    ImportFormat,
    conflict::{ConflictPolicy, ImportPreview, ImportPreviewItem, ImportResult},
    doi_resolver::{DoiResolver, LocalOpenAlexDoiResolver, list_local_openalex_works},
    openalex_works_source::OpenAlexWorksRecordSource,
    source_record::{SourceBatch, SourceRecord},
};
pub use import::{
    ImportOptions, OpenAlexSourcesPreview, import_openalex_sources, inspect_openalex_sources,
};
pub use layout::StorageLayout;
pub use literature::{
    DOCUMENT_ASSETS_RELATIVE_PATH, DocumentAsset, DocumentAssetImportResult, DocumentAssetKind,
    DocumentAssetStatus, DocumentAssetStorageKind, ExternalIdentifier,
    LITERATURE_ITEMS_RELATIVE_PATH, LiteratureFileKind, LiteratureFileRef, LiteratureItem,
    LiteratureItemDraft, LiteratureItemType, LiteratureSourceRef, READING_SESSIONS_RELATIVE_PATH,
    ReadingSession, ReadingSessionState, ReadingSessionSummary, ReadingStatus,
    SOURCE_BATCHES_RELATIVE_DIR, VAULT_FILES_RELATIVE_DIR,
};
pub use manifest::{
    DatasetManifest, TableManifest, VaultManifest, VaultSourceProvenance, VaultTableFile,
    VaultTableManifest,
};
pub use metrics::MetricCode;
pub use notes::{
    ANNOTATION_FORMAT_VERSION, ANNOTATIONS_RELATIVE_DIR, Annotation, AnnotationDraft,
    AnnotationKind, AnnotationResolution, MAX_CONTEXT_SCALARS, MAX_NOTE_BODY_BYTES,
    MAX_NOTE_TITLE_SCALARS, MAX_SELECTED_TEXT_SCALARS, NOTE_FORMAT_VERSION, NOTES_RELATIVE_DIR,
    Note, NoteDraft, QuoteAnchor,
};
pub use query::{SourceSearchQuery, SourceSummary};
pub use release::{
    UpdateCheck, check_update, is_newer_version, read_latest_version_from_json,
    read_version_from_tauri_conf,
};
pub use schema::TableName;
pub use search::{
    AssetContentIndexRecord, AssetContentIndexState, CONTENT_ANALYZER_VERSION,
    DEFAULT_SEARCH_PAGE_SIZE, DERIVED_SEARCH_RELATIVE_DIR, MAX_SEARCH_PAGE_SIZE,
    METADATA_ANALYZER_VERSION, MetadataAnalyzedField, MetadataIndexRecord,
    PDF_EXTRACTION_BATCH_SIZE, PDF_EXTRACTION_CONCURRENCY, PDF_MAX_DECOMPRESSED_BYTES_PER_PAGE,
    PDF_MAX_EXTRACTED_CHARS, PDF_MAX_EXTRACTION_DURATION, PDF_MAX_FILE_SIZE_BYTES, PDF_MAX_PAGES,
    PDF_TEXT_EXTRACTOR_VERSION, SEARCH_EXCERPT_MAX_CHARS, SEARCH_INDEX_FORMAT_VERSION,
    SearchFieldMatch, SearchFieldScope, SearchHit, SearchIndexChange, SearchIndexGeneration,
    SearchIndexIssue, SearchIndexIssueKind, SearchIndexManifest, SearchIndexState,
    SearchIndexStatus, SearchIndexTaskState, SearchIndexTaskStatus, SearchMatchField, SearchQuery,
    SearchQueryPage, SearchQueryResult,
};
pub use source_adapter::{
    OpenAlexSourcesAdapter, SourceAdapter, SourceAdapterDescriptor, available_source_adapters,
    openalex_sources_adapter,
};
pub use vault::{Vault, VaultContext, VaultOpenOptions};
