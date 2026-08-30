use thiserror::Error;

pub type Result<T> = std::result::Result<T, CoreError>;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Polars error: {0}")]
    Polars(#[from] polars::error::PolarsError),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("XLSX error: {0}")]
    Xlsx(#[from] rust_xlsxwriter::XlsxError),
    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("missing table in manifest: {0}")]
    MissingTable(String),
    #[error("invalid table file path in manifest: {0}")]
    InvalidTablePath(String),
    #[error("invalid user data path: {0}")]
    InvalidUserDataPath(String),
    #[error("literature item not found: {0}")]
    LiteratureItemNotFound(String),
    #[error(
        "failed to delete literature item safely: item write failed ({item_write_error}); asset rollback failed ({asset_restore_error})"
    )]
    LiteratureItemDeleteRollbackFailed {
        item_write_error: String,
        asset_restore_error: String,
    },
    #[error("literature file not found: {0}")]
    LiteratureFileNotFound(String),
    #[error("reading session asset {asset_id} does not belong to literature item {item_id}")]
    InvalidReadingSessionAssociation { item_id: String, asset_id: String },
    #[error("no reading session found for literature item {item_id} and asset {asset_id}")]
    ReadingSessionNotFound { item_id: String, asset_id: String },
    #[error("no reading session history is available")]
    NoReadingSessionHistory,
    #[error("invalid literature file path: {0}")]
    InvalidLiteratureFilePath(String),
    #[error("invalid document asset path: {0}")]
    InvalidDocumentAssetPath(String),
    #[error("only PDF literature files are supported: {0}")]
    UnsupportedLiteratureFile(String),
    #[error("unsupported metric: {0}")]
    UnsupportedMetric(String),
    #[error("note not found: {0}")]
    NoteNotFound(String),
    #[error("annotation not found: {0}")]
    AnnotationNotFound(String),
    #[error("note revision conflict: {note_id}")]
    NoteConflict { note_id: String },
    #[error("malformed note record")]
    MalformedNote,
    #[error("malformed annotation record")]
    MalformedAnnotation,
    #[error("note file identity does not match note_id")]
    NoteIdentityMismatch,
    #[error("annotation file identity does not match annotation_id")]
    AnnotationIdentityMismatch,
    #[error("unsupported note format version: {0}")]
    UnsupportedNoteVersion(String),
    #[error("unsupported annotation format version: {0}")]
    UnsupportedAnnotationVersion(String),
    #[error("note CAS lock unavailable: {0}")]
    NoteCasLockUnavailable(String),
    #[error("unsafe user record path or filesystem object")]
    UnsafeUserRecordPath,
    #[error("invalid note input: {0}")]
    InvalidNoteInput(String),
    #[error("invalid annotation input: {0}")]
    InvalidAnnotationInput(String),
    #[error("external assets cannot be annotated: item {item_id}, asset {asset_id}")]
    AnnotationExternalAssetForbidden { item_id: String, asset_id: String },
    #[error("annotation asset is unavailable: item {item_id}, asset {asset_id}: {reason}")]
    AnnotationAssetUnavailable {
        item_id: String,
        asset_id: String,
        reason: String,
    },
    #[error("annotation anchor out of range: {field}")]
    AnnotationAnchorOutOfRange { field: String },
    #[error("annotation anchor text not found on page")]
    AnnotationAnchorNotFound,
    #[error("annotation anchor text matches multiple locations on page")]
    AnnotationAnchorAmbiguous,
    #[error("search index is unavailable: {0}")]
    SearchIndexUnavailable(String),
    #[error("search index format is incompatible")]
    SearchIndexVersionIncompatible,
    #[error("invalid derived search index layout")]
    InvalidSearchIndexPath,
    #[error("search index build failed: {0}")]
    SearchIndexBuildFailed(String),
    #[error("search index build cancelled")]
    SearchIndexBuildCancelled,
    #[error("invalid literature import: {0}")]
    InvalidLiteratureImport(String),
    #[error(
        "failed to commit literature import safely: commit failed ({commit_error}); batch rollback failed ({rollback_error})"
    )]
    LiteratureImportRollbackFailed {
        commit_error: String,
        rollback_error: String,
    },
    #[error("failed to detect portable mode: {0}")]
    PortableModeDetectionFailed(String),
    #[error("failed to parse recent vaults: {0}")]
    RecentVaultsParseFailed(String),
    #[error("failed to parse app config: {0}")]
    AppConfigParseFailed(String),
    #[error("vault migration failed: {0}")]
    MigrationFailed(String),
    #[error("update check failed: {0}")]
    UpdateCheckFailed(String),
}
