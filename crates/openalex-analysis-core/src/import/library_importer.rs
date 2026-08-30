use std::{fs, path::Path};

use chrono::Utc;
use uuid::Uuid;

use crate::{
    LiteratureItem, LiteratureItemDraft, LiteratureItemType, LiteratureSourceRef, Result,
    SearchIndexChange, Vault,
};

use super::{
    ImportFormat,
    conflict::{
        ConflictPolicy, ImportPreview, ImportPreviewItem, ImportResult, find_matching_item,
    },
    record_source::RecordSource,
    source_record::{SourceBatch, SourceRecord},
};

/// Imports a set of source records into the user's personal library.
pub trait LibraryImporter {
    /// Build an in-memory preview without mutating the Vault.
    fn preview(
        &self,
        vault: &Vault,
        records: Vec<crate::import::source_record::SourceRecord>,
    ) -> Result<ImportPreview>;

    /// Persist the preview according to each item's selected conflict policy.
    fn commit(&self, vault: &Vault, preview: ImportPreview) -> Result<ImportResult>;
}

/// Generic personal-library importer that dispatches parsing by format and
/// reuses the same conflict-detection, merge, and rollback logic for all
/// offline formats.
#[derive(Debug, Clone)]
pub struct LiteratureLibraryImporter {
    format: ImportFormat,
}

impl LiteratureLibraryImporter {
    pub fn new(format: ImportFormat) -> Self {
        Self { format }
    }

    pub fn format(&self) -> ImportFormat {
        self.format
    }

    /// Parse bytes for this importer's configured format.
    pub fn parse(&self, bytes: &[u8]) -> Result<Vec<SourceRecord>> {
        match self.format {
            ImportFormat::BibTeX => super::bibtex_source::BibTeXRecordSource::new().parse(bytes),
            ImportFormat::Ris => super::ris_source::RISRecordSource::new().parse(bytes),
            ImportFormat::OpenAlexWorks => {
                super::openalex_works_source::OpenAlexWorksRecordSource::new().parse(bytes)
            }
        }
    }
}

impl LibraryImporter for LiteratureLibraryImporter {
    fn preview(&self, vault: &Vault, records: Vec<SourceRecord>) -> Result<ImportPreview> {
        let existing = vault.load_literature_items()?;
        let source_name = self.format.source_name().to_string();
        let items = records
            .into_iter()
            .map(|source_record| {
                let matched_item_id =
                    find_matching_item(&source_record, &existing).map(|i| i.item_id);
                let record_id = source_record.record_id;
                ImportPreviewItem {
                    record_id,
                    source_record,
                    matched_item_id,
                    default_policy: ConflictPolicy::Merge,
                    selected_policy: ConflictPolicy::Merge,
                }
            })
            .collect();

        Ok(ImportPreview {
            batch_id: Uuid::new_v4(),
            source_name,
            items,
        })
    }

    fn commit(&self, vault: &Vault, preview: ImportPreview) -> Result<ImportResult> {
        commit_import_preview(vault, &preview)
    }
}

/// Persists a preview according to each item's selected conflict policy.
///
/// This is the implementation shared by `LiteratureLibraryImporter::commit` and
/// single-record remote imports.
pub fn commit_import_preview(vault: &Vault, preview: &ImportPreview) -> Result<ImportResult> {
    let batch = SourceBatch {
        batch_id: preview.batch_id,
        source_name: preview.source_name.clone(),
        imported_at: Utc::now(),
        records: preview
            .items
            .iter()
            .map(|i| i.source_record.clone())
            .collect(),
    };

    let batch_path = vault.sources_dir().join(format!("{}.json", batch.batch_id));
    let batch_backup = save_batch_with_backup(vault, &batch_path, &batch)?;

    let mut result = ImportResult {
        batch_id: batch.batch_id,
        ..ImportResult::default()
    };

    let commit_outcome = (|| -> Result<Vec<Uuid>> {
        let mut items = vault.load_literature_items()?;
        let mut changed_item_ids = Vec::new();
        for preview_item in &preview.items {
            match preview_item.selected_policy {
                ConflictPolicy::Skip => {
                    result.skipped += 1;
                }
                ConflictPolicy::Create => {
                    let item = create_item_from_record(&preview_item.source_record);
                    changed_item_ids.push(item.item_id);
                    items.push(item);
                    result.created += 1;
                }
                ConflictPolicy::Merge => {
                    if let Some(existing_id) = preview_item.matched_item_id {
                        if let Some(existing) = items.iter_mut().find(|i| i.item_id == existing_id)
                        {
                            merge_record_into_item(existing, &preview_item.source_record);
                            changed_item_ids.push(existing_id);
                            result.merged += 1;
                        } else {
                            // Item disappeared between preview and commit; create.
                            let item = create_item_from_record(&preview_item.source_record);
                            changed_item_ids.push(item.item_id);
                            items.push(item);
                            result.created += 1;
                        }
                    } else {
                        let item = create_item_from_record(&preview_item.source_record);
                        changed_item_ids.push(item.item_id);
                        items.push(item);
                        result.created += 1;
                    }
                }
            }
        }
        vault.save_literature_items(&items)?;
        Ok(changed_item_ids)
    })();

    match commit_outcome {
        Ok(changed_item_ids) => {
            for item_id in changed_item_ids {
                vault.try_apply_search_index_change(SearchIndexChange::MetadataChanged { item_id });
            }
            Ok(result)
        }
        Err(error) => {
            if let Some(backup) = batch_backup {
                if let Err(rollback_error) = restore_batch(&batch_path, &backup) {
                    return Err(crate::CoreError::LiteratureImportRollbackFailed {
                        commit_error: error.to_string(),
                        rollback_error: rollback_error.to_string(),
                    });
                }
            } else {
                let _ = fs::remove_file(&batch_path);
            }
            Err(error)
        }
    }
}

/// Imports a single `SourceRecord` directly using the given conflict policy.
pub fn import_single_record(
    vault: &Vault,
    source_record: SourceRecord,
    policy: ConflictPolicy,
) -> Result<ImportResult> {
    let existing = vault.load_literature_items()?;
    let matched_item_id = find_matching_item(&source_record, &existing).map(|item| item.item_id);
    let preview = ImportPreview {
        batch_id: Uuid::new_v4(),
        source_name: source_record.source_name.clone(),
        items: vec![ImportPreviewItem {
            record_id: source_record.record_id,
            source_record,
            matched_item_id,
            default_policy: policy,
            selected_policy: policy,
        }],
    };
    commit_import_preview(vault, &preview)
}

fn save_batch_with_backup(
    vault: &Vault,
    batch_path: &Path,
    batch: &SourceBatch,
) -> Result<Option<Vec<u8>>> {
    fs::create_dir_all(vault.sources_dir())?;
    let backup = if batch_path.exists() {
        Some(fs::read(batch_path)?)
    } else {
        None
    };

    let payload = serde_json::to_vec_pretty(batch)?;
    crate::literature::atomic_replace_named(batch_path, &payload, ".source_batch")?;
    Ok(backup)
}

fn restore_batch(batch_path: &Path, backup: &[u8]) -> Result<()> {
    crate::literature::atomic_replace_named(batch_path, backup, ".source_batch")
}

fn create_item_from_record(record: &SourceRecord) -> LiteratureItem {
    LiteratureItem::from_draft(draft_from_record(record))
}

fn draft_from_record(record: &SourceRecord) -> LiteratureItemDraft {
    LiteratureItemDraft {
        title: record.title.clone(),
        authors: record.authors.clone(),
        published_year: record.published_year,
        item_type: parse_item_type(&record.item_type),
        favorite: false,
        reading_status: crate::ReadingStatus::Inbox,
        tags: Vec::new(),
        sources: vec![LiteratureSourceRef {
            source_name: record.source_name.clone(),
            external_id: record.external_id.clone(),
            original_locator: record.external_id.clone(),
        }],
        external_identifiers: record.external_identifiers.clone(),
    }
}

fn merge_record_into_item(item: &mut LiteratureItem, record: &SourceRecord) {
    let source_ref = LiteratureSourceRef {
        source_name: record.source_name.clone(),
        external_id: record.external_id.clone(),
        original_locator: record.external_id.clone(),
    };
    if !item
        .sources
        .iter()
        .any(|s| s.source_name == source_ref.source_name && s.external_id == source_ref.external_id)
    {
        item.sources.push(source_ref);
    }

    if item.title.trim().is_empty() {
        item.title = record.title.clone();
    }
    if item.authors.is_empty() {
        item.authors = record.authors.clone();
    }
    if item.published_year.is_none() {
        item.published_year = record.published_year;
    }
    if matches!(item.item_type, LiteratureItemType::Other) {
        item.item_type = parse_item_type(&record.item_type);
    }

    for new_id in &record.external_identifiers {
        if !item.external_identifiers.iter().any(|existing| {
            existing.namespace == new_id.namespace
                && existing.value.eq_ignore_ascii_case(&new_id.value)
        }) {
            item.external_identifiers.push(new_id.clone());
        }
    }
}

fn parse_item_type(value: &str) -> LiteratureItemType {
    match value.to_ascii_lowercase().as_str() {
        "article" => LiteratureItemType::Article,
        "book" => LiteratureItemType::Book,
        "chapter" => LiteratureItemType::Chapter,
        _ => LiteratureItemType::Other,
    }
}

/// Loads all persisted source batches from the Vault.
pub fn load_source_batches(vault: &Vault) -> Result<Vec<SourceBatch>> {
    let dir = vault.sources_dir();
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut batches = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let bytes = fs::read(&path)?;
        let batch: SourceBatch = serde_json::from_slice(&bytes)?;
        batches.push(batch);
    }
    batches.sort_by_key(|b| b.imported_at);
    Ok(batches)
}
