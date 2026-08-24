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
    #[error("missing table in manifest: {0}")]
    MissingTable(String),
    #[error("unsupported metric: {0}")]
    UnsupportedMetric(String),
}

