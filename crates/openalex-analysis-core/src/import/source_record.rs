use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ExternalIdentifier;

/// A raw record produced by an external source before it is normalized into a
/// user-owned `LiteratureItem`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SourceRecord {
    pub record_id: Uuid,
    pub source_name: String,
    pub external_id: Option<String>,
    pub external_identifiers: Vec<ExternalIdentifier>,
    pub title: String,
    pub authors: Vec<String>,
    pub published_year: Option<i32>,
    pub item_type: String,
    #[serde(default)]
    pub abstract_text: String,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub pages: String,
    #[serde(default)]
    pub volume: String,
    /// All original fields from the source, keyed by lowercase field name.
    pub raw_fields: BTreeMap<String, String>,
}

/// Batch-level metadata together with the raw records it contains.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SourceBatch {
    pub batch_id: Uuid,
    pub source_name: String,
    pub imported_at: DateTime<Utc>,
    pub records: Vec<SourceRecord>,
}
