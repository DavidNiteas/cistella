use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{LiteratureItem, import::source_record::SourceRecord};

/// Strategy chosen by the user for an individual conflicting record.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ConflictPolicy {
    /// Merge into the existing item when a match is found; otherwise create.
    #[default]
    Merge,
    /// Skip this record.
    Skip,
    /// Always create a new item, even if a match exists.
    Create,
}

/// One item in the import preview, pairing a source record with its likely
/// conflict resolution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreviewItem {
    pub record_id: Uuid,
    pub source_record: SourceRecord,
    pub matched_item_id: Option<Uuid>,
    pub default_policy: ConflictPolicy,
    pub selected_policy: ConflictPolicy,
}

/// In-memory preview of an import batch before it is committed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreview {
    pub batch_id: Uuid,
    pub source_name: String,
    pub items: Vec<ImportPreviewItem>,
}

/// Result of committing an import batch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    pub created: usize,
    pub merged: usize,
    pub skipped: usize,
    pub errors: usize,
    pub batch_id: Uuid,
}

/// Detect a conflict by comparing DOI/ISBN/ISSN/PMID/OpenAlex namespaces.
pub fn find_matching_item<'a>(
    record: &SourceRecord,
    items: &'a [LiteratureItem],
) -> Option<&'a LiteratureItem> {
    record
        .external_identifiers
        .iter()
        .filter(|id| {
            matches!(
                id.namespace.as_str(),
                "doi" | "isbn" | "issn" | "pmid" | "openalex"
            )
        })
        .find_map(|record_id| {
            items.iter().find(|item| {
                item.external_identifiers.iter().any(|item_id| {
                    item_id.namespace == record_id.namespace
                        && item_id.value.eq_ignore_ascii_case(&record_id.value)
                })
            })
        })
}
