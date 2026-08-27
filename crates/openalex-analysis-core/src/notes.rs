use std::io::Read;
use std::path::Path;

use chrono::{DateTime, SecondsFormat, Timelike, Utc};
use lopdf::Document;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

use crate::{
    error::{CoreError, Result},
    literature::{DocumentAsset, DocumentAssetStatus, DocumentAssetStorageKind},
    search::PDF_TEXT_EXTRACTOR_VERSION,
    secure_user_records::SecureUserRecords,
    vault::Vault,
};

pub const NOTES_RELATIVE_DIR: &str = "user/notes";
pub const ANNOTATIONS_RELATIVE_DIR: &str = "user/annotations";
pub const NOTE_FORMAT_VERSION: &str = "note/v1";
pub const ANNOTATION_FORMAT_VERSION: &str = "annotation/v1";
pub const MAX_NOTE_TITLE_SCALARS: usize = 240;
pub const MAX_NOTE_BODY_BYTES: usize = 512 * 1024;
pub const MAX_SELECTED_TEXT_SCALARS: usize = 8_192;
pub const MAX_CONTEXT_SCALARS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub note_id: Uuid,
    pub item_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub archived_at: Option<DateTime<Utc>>,
    pub title: String,
    pub markdown_body: String,
    /// SHA-256 of the exact persisted note bytes. This is a concurrency token,
    /// not a persisted front-matter field.
    pub revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteDraft {
    pub item_id: Uuid,
    pub title: String,
    pub markdown_body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuoteAnchor {
    pub asset_id: Uuid,
    pub asset_content_hash_at_capture: String,
    pub extractor_version: String,
    pub page_number: u32,
    pub selected_text: String,
    pub normalized_text_hash: String,
    pub prefix_context: String,
    pub suffix_context: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Annotation {
    pub format_version: String,
    pub annotation_id: Uuid,
    pub item_id: Uuid,
    pub asset_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub kind: AnnotationKind,
    pub anchor: QuoteAnchor,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AnnotationKind {
    Quote,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnotationDraft {
    pub item_id: Uuid,
    pub asset_id: Uuid,
    pub anchor: QuoteAnchor,
}

/// Runtime resolution status for a quote annotation. These values are computed
/// on demand and never persisted into the annotation file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnnotationResolution {
    ResolvedExact,
    UnavailableMissingAsset,
    UnavailableExternalAsset,
    UnavailableUnreadableAsset,
    InvalidatedContentChanged,
    InvalidatedPageOutOfRange,
    InvalidatedTextNotFound,
    InvalidatedAmbiguousText,
    UnsupportedExtractorVersion,
    OrphanedItem,
}

impl Vault {
    pub fn create_note(&self, draft: NoteDraft) -> Result<Note> {
        validate_note_payload(&draft.title, &draft.markdown_body)?;
        let timestamp = now_utc_seconds();
        let note = Note {
            note_id: Uuid::new_v4(),
            item_id: draft.item_id,
            created_at: timestamp,
            updated_at: timestamp,
            archived_at: None,
            title: draft.title,
            markdown_body: draft.markdown_body,
            revision: String::new(),
        };
        let bytes = serialize_note(&note)?;
        SecureUserRecords::notes(self.secure_user_root()).replace(
            note.note_id,
            "md",
            &bytes,
            ".cistella-note",
        )?;
        parse_note_bytes(&bytes, Some(note.note_id))
    }
    pub fn get_note(&self, note_id: Uuid) -> Result<Note> {
        let bytes =
            SecureUserRecords::notes(self.secure_user_root()).read(note_id, "md", "note")?;
        parse_note_bytes(&bytes, Some(note_id))
    }
    pub fn list_notes(&self, item_id: Option<Uuid>, include_archived: bool) -> Result<Vec<Note>> {
        let records = SecureUserRecords::notes(self.secure_user_root()).list("md", "note")?;
        records
            .into_iter()
            .map(|id| self.get_note(id))
            .filter(|r| match r {
                Ok(note) => {
                    item_id.is_none_or(|x| x == note.item_id)
                        && (include_archived || note.archived_at.is_none())
                }
                Err(_) => true,
            })
            .collect()
    }
    pub fn update_note(
        &self,
        note_id: Uuid,
        expected_revision: &str,
        title: String,
        markdown_body: String,
    ) -> Result<Note> {
        validate_note_payload(&title, &markdown_body)?;
        let records = SecureUserRecords::notes(self.secure_user_root());
        records.with_note_cas_lock(note_id, || {
            // Read, revision comparison, and native relative replace are one
            // cross-thread/cross-process critical section. Never move this
            // read outside the lock: it is the CAS observation point.
            let previous = parse_note_bytes(&records.read(note_id, "md", "note")?, Some(note_id))?;
            ensure_expected_revision(note_id, expected_revision, &previous.revision)?;
            let next = Note {
                note_id,
                item_id: previous.item_id,
                created_at: previous.created_at,
                updated_at: now_utc_seconds(),
                archived_at: previous.archived_at,
                title,
                markdown_body,
                revision: String::new(),
            };
            let bytes = serialize_note(&next)?;
            records.replace(note_id, "md", &bytes, ".cistella-note")?;
            parse_note_bytes(&bytes, Some(note_id))
        })
    }
    pub fn archive_note(&self, note_id: Uuid, expected_revision: &str) -> Result<Note> {
        self.set_note_archived(note_id, expected_revision, true)
    }
    pub fn unarchive_note(&self, note_id: Uuid, expected_revision: &str) -> Result<Note> {
        self.set_note_archived(note_id, expected_revision, false)
    }
    fn set_note_archived(
        &self,
        note_id: Uuid,
        expected_revision: &str,
        archived: bool,
    ) -> Result<Note> {
        let records = SecureUserRecords::notes(self.secure_user_root());
        records.with_note_cas_lock(note_id, || {
            let previous = parse_note_bytes(&records.read(note_id, "md", "note")?, Some(note_id))?;
            ensure_expected_revision(note_id, expected_revision, &previous.revision)?;
            let now = now_utc_seconds();
            let next = Note {
                note_id,
                item_id: previous.item_id,
                created_at: previous.created_at,
                updated_at: now,
                archived_at: archived.then_some(now),
                title: previous.title,
                markdown_body: previous.markdown_body,
                revision: String::new(),
            };
            let bytes = serialize_note(&next)?;
            records.replace(note_id, "md", &bytes, ".cistella-note")?;
            parse_note_bytes(&bytes, Some(note_id))
        })
    }
    pub fn create_annotation(&self, draft: AnnotationDraft) -> Result<Annotation> {
        validate_quote_anchor(&draft.asset_id, &draft.anchor)?;
        let timestamp = now_utc_seconds();
        let annotation = Annotation {
            format_version: ANNOTATION_FORMAT_VERSION.to_string(),
            annotation_id: Uuid::new_v4(),
            item_id: draft.item_id,
            asset_id: draft.asset_id,
            created_at: timestamp,
            updated_at: timestamp,
            kind: AnnotationKind::Quote,
            anchor: draft.anchor,
        };
        let bytes = serialize_annotation(&annotation)?;
        SecureUserRecords::annotations(self.secure_user_root()).replace(
            annotation.annotation_id,
            "json",
            &bytes,
            ".cistella-annotation",
        )?;
        parse_annotation_bytes(&bytes, Some(annotation.annotation_id))
    }
    pub fn get_annotation(&self, annotation_id: Uuid) -> Result<Annotation> {
        let bytes = SecureUserRecords::annotations(self.secure_user_root()).read(
            annotation_id,
            "json",
            "annotation",
        )?;
        parse_annotation_bytes(&bytes, Some(annotation_id))
    }
    pub fn update_annotation(
        &self,
        annotation_id: Uuid,
        draft: AnnotationDraft,
    ) -> Result<Annotation> {
        validate_quote_anchor(&draft.asset_id, &draft.anchor)?;
        let previous = self.get_annotation(annotation_id)?;
        let next = Annotation {
            format_version: ANNOTATION_FORMAT_VERSION.to_string(),
            annotation_id,
            item_id: draft.item_id,
            asset_id: draft.asset_id,
            created_at: previous.created_at,
            updated_at: now_utc_seconds(),
            kind: AnnotationKind::Quote,
            anchor: draft.anchor,
        };
        let bytes = serialize_annotation(&next)?;
        SecureUserRecords::annotations(self.secure_user_root()).replace(
            annotation_id,
            "json",
            &bytes,
            ".cistella-annotation",
        )?;
        parse_annotation_bytes(&bytes, Some(annotation_id))
    }
    pub fn list_annotations(&self, item_id: Option<Uuid>) -> Result<Vec<Annotation>> {
        let records =
            SecureUserRecords::annotations(self.secure_user_root()).list("json", "annotation")?;
        records
            .into_iter()
            .map(|id| self.get_annotation(id))
            .filter(|r| match r {
                Ok(x) => item_id.is_none_or(|i| i == x.item_id),
                Err(_) => true,
            })
            .collect()
    }
    pub fn delete_annotation(&self, annotation_id: Uuid) -> Result<()> {
        SecureUserRecords::annotations(self.secure_user_root()).delete(
            annotation_id,
            "json",
            "annotation",
        )
    }

    /// Creates a quote annotation only after validating item/asset ownership,
    /// Vault storage, availability, and the exact location of the selected text
    /// on the requested page. All anchor fields are computed inside Core; no
    /// absolute path is exposed.
    pub fn create_quote_annotation(
        &self,
        item_id: Uuid,
        asset_id: Uuid,
        page_number: u32,
        selected_text: String,
        prefix_context: String,
        suffix_context: String,
    ) -> Result<Annotation> {
        validate_quote_anchor_input(
            &selected_text,
            &prefix_context,
            &suffix_context,
            page_number,
        )?;

        let asset = self.validate_reading_session_association(item_id, asset_id)?;
        ensure_annotatable_vault_pdf(&asset, item_id, asset_id, self)?;

        let path = self.resolve_document_asset_path(item_id, asset_id)?;
        let page_text = extract_pdf_page_text(&path, page_number, item_id, asset_id)?;
        match locate_text_on_page(&page_text, &selected_text, &prefix_context, &suffix_context) {
            TextLocation::Found(_) => {}
            TextLocation::NotFound => return Err(CoreError::AnnotationAnchorNotFound),
            TextLocation::Ambiguous => return Err(CoreError::AnnotationAnchorAmbiguous),
        }

        let asset_content_hash = sha256_file(&path)?;
        let normalized_text_hash = sha256_hex_string(&normalize_for_anchor(&selected_text));

        let anchor = QuoteAnchor {
            asset_id,
            asset_content_hash_at_capture: asset_content_hash,
            extractor_version: PDF_TEXT_EXTRACTOR_VERSION.to_string(),
            page_number,
            selected_text,
            normalized_text_hash,
            prefix_context,
            suffix_context,
        };
        self.create_annotation(AnnotationDraft {
            item_id,
            asset_id,
            anchor,
        })
    }

    /// Resolves an annotation at runtime without modifying its file. The result
    /// reflects the current state of the owning item, the asset, and the PDF
    /// content; failures never rewrite the annotation.
    pub fn resolve_annotation(&self, annotation_id: Uuid) -> Result<AnnotationResolution> {
        let annotation = self.get_annotation(annotation_id)?;
        let item_id = annotation.item_id;
        let asset_id = annotation.asset_id;

        let items = self.load_literature_items()?;
        if !items.iter().any(|item| item.item_id == item_id) {
            return Ok(AnnotationResolution::OrphanedItem);
        }

        let assets = self.load_document_assets()?;
        let Some(asset) = assets
            .iter()
            .find(|asset| asset.item_id == item_id && asset.asset_id == asset_id)
        else {
            return Ok(AnnotationResolution::UnavailableMissingAsset);
        };

        if asset.storage_kind == DocumentAssetStorageKind::External {
            return Ok(AnnotationResolution::UnavailableExternalAsset);
        }
        match asset.status(self) {
            DocumentAssetStatus::Available => {}
            DocumentAssetStatus::Missing => {
                return Ok(AnnotationResolution::UnavailableMissingAsset);
            }
            DocumentAssetStatus::Unreadable
            | DocumentAssetStatus::Invalid
            | DocumentAssetStatus::ExternalUnavailable => {
                return Ok(AnnotationResolution::UnavailableUnreadableAsset);
            }
        }

        let path = match self.resolve_document_asset_path(item_id, asset_id) {
            Ok(path) => path,
            Err(_) => return Ok(AnnotationResolution::UnavailableMissingAsset),
        };
        let current_hash = match sha256_file(&path) {
            Ok(hash) => hash,
            Err(_) => return Ok(AnnotationResolution::UnavailableUnreadableAsset),
        };
        if current_hash != annotation.anchor.asset_content_hash_at_capture {
            return Ok(AnnotationResolution::InvalidatedContentChanged);
        }

        if annotation.anchor.extractor_version != PDF_TEXT_EXTRACTOR_VERSION {
            return Ok(AnnotationResolution::UnsupportedExtractorVersion);
        }

        let page_text =
            match extract_pdf_page_text(&path, annotation.anchor.page_number, item_id, asset_id) {
                Ok(text) => text,
                Err(CoreError::AnnotationAnchorOutOfRange { .. }) => {
                    return Ok(AnnotationResolution::InvalidatedPageOutOfRange);
                }
                Err(_) => return Ok(AnnotationResolution::UnavailableUnreadableAsset),
            };

        Ok(
            match locate_text_on_page(
                &page_text,
                &annotation.anchor.selected_text,
                &annotation.anchor.prefix_context,
                &annotation.anchor.suffix_context,
            ) {
                TextLocation::Found(_) => AnnotationResolution::ResolvedExact,
                TextLocation::NotFound => AnnotationResolution::InvalidatedTextNotFound,
                TextLocation::Ambiguous => AnnotationResolution::InvalidatedAmbiguousText,
            },
        )
    }
}
fn now_utc_seconds() -> DateTime<Utc> {
    Utc::now().with_nanosecond(0).unwrap_or_else(Utc::now)
}

fn validate_note_payload(title: &str, markdown_body: &str) -> Result<()> {
    if title.is_empty()
        || title.chars().count() > MAX_NOTE_TITLE_SCALARS
        || title.contains(['\r', '\n'])
        || title.chars().any(char::is_control)
    {
        return Err(CoreError::InvalidNoteInput(
            "title must be one non-empty plain-text line within 240 Unicode scalars".to_string(),
        ));
    }
    if markdown_body.len() > MAX_NOTE_BODY_BYTES {
        return Err(CoreError::InvalidNoteInput(
            "markdown body exceeds 512 KiB UTF-8".to_string(),
        ));
    }
    Ok(())
}

fn validate_quote_anchor(asset_id: &Uuid, anchor: &QuoteAnchor) -> Result<()> {
    if &anchor.asset_id != asset_id {
        return Err(CoreError::InvalidAnnotationInput(
            "top-level asset_id must equal anchor.asset_id".to_string(),
        ));
    }
    if anchor.page_number == 0 {
        return Err(CoreError::InvalidAnnotationInput(
            "page_number must be 1-based".to_string(),
        ));
    }
    validate_scalar_range(
        &anchor.selected_text,
        1,
        MAX_SELECTED_TEXT_SCALARS,
        "selected_text",
    )?;
    validate_scalar_range(
        &anchor.prefix_context,
        0,
        MAX_CONTEXT_SCALARS,
        "prefix_context",
    )?;
    validate_scalar_range(
        &anchor.suffix_context,
        0,
        MAX_CONTEXT_SCALARS,
        "suffix_context",
    )?;
    validate_sha256_hex(
        &anchor.asset_content_hash_at_capture,
        "asset_content_hash_at_capture",
    )?;
    validate_sha256_hex(&anchor.normalized_text_hash, "normalized_text_hash")?;
    if anchor.extractor_version.is_empty()
        || anchor.extractor_version.chars().count() > 128
        || anchor.extractor_version.contains(['\r', '\n'])
        || anchor.extractor_version.chars().any(char::is_control)
    {
        return Err(CoreError::InvalidAnnotationInput(
            "extractor_version must be one non-empty line within 128 Unicode scalars".to_string(),
        ));
    }
    Ok(())
}

fn validate_quote_anchor_input(
    selected_text: &str,
    prefix_context: &str,
    suffix_context: &str,
    page_number: u32,
) -> Result<()> {
    if page_number == 0 {
        return Err(CoreError::AnnotationAnchorOutOfRange {
            field: "page_number".to_string(),
        });
    }
    validate_scalar_range(selected_text, 1, MAX_SELECTED_TEXT_SCALARS, "selected_text")?;
    validate_scalar_range(prefix_context, 0, MAX_CONTEXT_SCALARS, "prefix_context")?;
    validate_scalar_range(suffix_context, 0, MAX_CONTEXT_SCALARS, "suffix_context")?;
    Ok(())
}

fn ensure_annotatable_vault_pdf(
    asset: &DocumentAsset,
    item_id: Uuid,
    asset_id: Uuid,
    vault: &Vault,
) -> Result<()> {
    if asset.storage_kind == DocumentAssetStorageKind::External {
        return Err(CoreError::AnnotationExternalAssetForbidden {
            item_id: item_id.to_string(),
            asset_id: asset_id.to_string(),
        });
    }
    if !asset.media_type.eq_ignore_ascii_case("application/pdf") {
        return Err(CoreError::AnnotationAssetUnavailable {
            item_id: item_id.to_string(),
            asset_id: asset_id.to_string(),
            reason: "asset is not a PDF".to_string(),
        });
    }
    match asset.status(vault) {
        DocumentAssetStatus::Available => Ok(()),
        DocumentAssetStatus::Missing => Err(CoreError::AnnotationAssetUnavailable {
            item_id: item_id.to_string(),
            asset_id: asset_id.to_string(),
            reason: "asset file is missing".to_string(),
        }),
        DocumentAssetStatus::Unreadable
        | DocumentAssetStatus::Invalid
        | DocumentAssetStatus::ExternalUnavailable => Err(CoreError::AnnotationAssetUnavailable {
            item_id: item_id.to_string(),
            asset_id: asset_id.to_string(),
            reason: "asset file is unreadable or invalid".to_string(),
        }),
    }
}

fn extract_pdf_page_text(
    path: &Path,
    page_number: u32,
    item_id: Uuid,
    asset_id: Uuid,
) -> Result<String> {
    let document = Document::load(path).map_err(|_| CoreError::AnnotationAssetUnavailable {
        item_id: item_id.to_string(),
        asset_id: asset_id.to_string(),
        reason: "PDF could not be loaded".to_string(),
    })?;
    let pages = document.get_pages();
    if page_number == 0 || page_number > pages.len() as u32 {
        return Err(CoreError::AnnotationAnchorOutOfRange {
            field: "page_number".to_string(),
        });
    }
    document
        .extract_text(&[page_number])
        .map_err(|_| CoreError::AnnotationAssetUnavailable {
            item_id: item_id.to_string(),
            asset_id: asset_id.to_string(),
            reason: "PDF text layer could not be extracted".to_string(),
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextLocation {
    Found(usize),
    NotFound,
    Ambiguous,
}

fn locate_text_on_page(
    page_text: &str,
    selected_text: &str,
    prefix_context: &str,
    suffix_context: &str,
) -> TextLocation {
    let pattern = if prefix_context.is_empty() && suffix_context.is_empty() {
        selected_text.to_string()
    } else {
        format!("{prefix_context}{selected_text}{suffix_context}")
    };
    if pattern.is_empty() {
        return TextLocation::NotFound;
    }
    let mut positions = Vec::new();
    let mut start = 0usize;
    while let Some(pos) = page_text[start..].find(&pattern) {
        positions.push(start + pos);
        start += pos + pattern.len().max(1);
    }
    match positions.len() {
        0 => TextLocation::NotFound,
        1 => TextLocation::Found(positions[0]),
        _ => TextLocation::Ambiguous,
    }
}

fn normalize_for_anchor(text: &str) -> String {
    text.nfkc().collect::<String>()
}

fn sha256_hex_string(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    format!("{digest:x}")
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn validate_scalar_range(value: &str, min: usize, max: usize, field: &str) -> Result<()> {
    let length = value.chars().count();
    if length < min || length > max {
        return Err(CoreError::InvalidAnnotationInput(format!(
            "{field} must contain {min}..={max} Unicode scalars"
        )));
    }
    Ok(())
}

fn validate_sha256_hex(value: &str, field: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(CoreError::InvalidAnnotationInput(format!(
            "{field} must be a 64-character SHA-256 hex string"
        )));
    }
    Ok(())
}

fn ensure_expected_revision(note_id: Uuid, expected: &str, actual: &str) -> Result<()> {
    if expected != actual {
        return Err(CoreError::NoteConflict {
            note_id: note_id.to_string(),
        });
    }
    Ok(())
}

fn serialize_note(note: &Note) -> Result<Vec<u8>> {
    validate_note_payload(&note.title, &note.markdown_body)?;
    let archived_at = note
        .archived_at
        .map(format_timestamp)
        .unwrap_or_else(|| "null".to_string());
    Ok(format!(
        "---\ncistella: {NOTE_FORMAT_VERSION}\nnote_id: {}\nitem_id: {}\ncreated_at: {}\nupdated_at: {}\narchived_at: {archived_at}\ntitle: {}\n---\n{}",
        note.note_id,
        note.item_id,
        format_timestamp(note.created_at),
        format_timestamp(note.updated_at),
        note.title,
        note.markdown_body
    )
    .into_bytes())
}

fn parse_note_bytes(bytes: &[u8], expected_id: Option<Uuid>) -> Result<Note> {
    let raw = std::str::from_utf8(bytes).map_err(|_| CoreError::MalformedNote)?;
    let (front_matter, markdown_body) = raw
        .strip_prefix("---\n")
        .and_then(|remaining| remaining.split_once("\n---\n"))
        .ok_or(CoreError::MalformedNote)?;
    let lines: Vec<&str> = front_matter.split('\n').collect();
    if lines.len() != 7 {
        return Err(CoreError::MalformedNote);
    }
    let cistella = parse_fixed_field(lines[0], "cistella")?;
    if cistella != NOTE_FORMAT_VERSION {
        return Err(CoreError::UnsupportedNoteVersion(cistella.to_string()));
    }
    let note_id = parse_uuid_field(lines[1], "note_id", true)?;
    if expected_id.is_some_and(|expected| expected != note_id) {
        return Err(CoreError::NoteIdentityMismatch);
    }
    let item_id = parse_uuid_field(lines[2], "item_id", true)?;
    let created_at = parse_utc_field(lines[3], "created_at")?;
    let updated_at = parse_utc_field(lines[4], "updated_at")?;
    let archived_value = parse_fixed_field(lines[5], "archived_at")?;
    let archived_at = if archived_value == "null" {
        None
    } else {
        Some(parse_utc_value(archived_value).ok_or(CoreError::MalformedNote)?)
    };
    let title = parse_fixed_field(lines[6], "title")?.to_string();
    validate_note_payload(&title, markdown_body)?;
    if updated_at < created_at || archived_at.is_some_and(|value| value < created_at) {
        return Err(CoreError::MalformedNote);
    }
    Ok(Note {
        note_id,
        item_id,
        created_at,
        updated_at,
        archived_at,
        title,
        markdown_body: markdown_body.to_string(),
        revision: sha256_hex(bytes),
    })
}

fn parse_fixed_field<'a>(line: &'a str, key: &str) -> Result<&'a str> {
    let prefix = format!("{key}: ");
    line.strip_prefix(&prefix)
        .filter(|value| !value.contains(['\r', '\n']))
        .ok_or(CoreError::MalformedNote)
}

fn parse_uuid_field(line: &str, key: &str, note_error: bool) -> Result<Uuid> {
    let value = parse_fixed_field(line, key)?;
    Uuid::parse_str(value).map_err(|_| {
        if note_error {
            CoreError::MalformedNote
        } else {
            CoreError::MalformedAnnotation
        }
    })
}

fn parse_utc_field(line: &str, key: &str) -> Result<DateTime<Utc>> {
    parse_utc_value(parse_fixed_field(line, key)?).ok_or(CoreError::MalformedNote)
}

fn parse_utc_value(value: &str) -> Option<DateTime<Utc>> {
    let parsed = DateTime::parse_from_rfc3339(value).ok()?;
    (parsed.offset().local_minus_utc() == 0).then(|| parsed.with_timezone(&Utc))
}

fn format_timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AnnotationFile {
    format_version: String,
    annotation_id: Uuid,
    item_id: Uuid,
    asset_id: Uuid,
    created_at: String,
    updated_at: String,
    kind: AnnotationKind,
    anchor: QuoteAnchor,
}

impl From<&Annotation> for AnnotationFile {
    fn from(annotation: &Annotation) -> Self {
        Self {
            format_version: annotation.format_version.clone(),
            annotation_id: annotation.annotation_id,
            item_id: annotation.item_id,
            asset_id: annotation.asset_id,
            created_at: format_timestamp(annotation.created_at),
            updated_at: format_timestamp(annotation.updated_at),
            kind: annotation.kind.clone(),
            anchor: annotation.anchor.clone(),
        }
    }
}

fn serialize_annotation(annotation: &Annotation) -> Result<Vec<u8>> {
    validate_annotation(annotation)?;
    serde_json::to_vec_pretty(&AnnotationFile::from(annotation)).map_err(Into::into)
}

fn parse_annotation_bytes(bytes: &[u8], expected_id: Option<Uuid>) -> Result<Annotation> {
    let file: AnnotationFile =
        serde_json::from_slice(bytes).map_err(|_| CoreError::MalformedAnnotation)?;
    let annotation = Annotation {
        format_version: file.format_version,
        annotation_id: file.annotation_id,
        item_id: file.item_id,
        asset_id: file.asset_id,
        created_at: parse_utc_value(&file.created_at).ok_or(CoreError::MalformedAnnotation)?,
        updated_at: parse_utc_value(&file.updated_at).ok_or(CoreError::MalformedAnnotation)?,
        kind: file.kind,
        anchor: file.anchor,
    };
    if expected_id.is_some_and(|expected| expected != annotation.annotation_id) {
        return Err(CoreError::AnnotationIdentityMismatch);
    }
    validate_annotation(&annotation)?;
    Ok(annotation)
}

fn validate_annotation(annotation: &Annotation) -> Result<()> {
    if annotation.format_version != ANNOTATION_FORMAT_VERSION {
        return Err(CoreError::UnsupportedAnnotationVersion(
            annotation.format_version.clone(),
        ));
    }
    if annotation.kind != AnnotationKind::Quote {
        return Err(CoreError::MalformedAnnotation);
    }
    if annotation.updated_at < annotation.created_at {
        return Err(CoreError::MalformedAnnotation);
    }
    validate_quote_anchor(&annotation.asset_id, &annotation.anchor)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("{digest:x}")
}
