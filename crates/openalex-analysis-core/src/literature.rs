use std::{
    ffi::OsStr,
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    error::{CoreError, Result},
    import::{
        ImportFormat,
        conflict::{ImportPreview, ImportResult},
        doi_resolver::{list_local_openalex_works, resolve_local_openalex_doi},
        library_importer::{
            LibraryImporter, LiteratureLibraryImporter,
            load_source_batches as load_source_batches_impl,
        },
        source_record::SourceBatch,
    },
    search::SearchIndexChange,
    vault::Vault,
};

pub const LITERATURE_ITEMS_RELATIVE_PATH: &str = "user/literature_items.json";
pub const DOCUMENT_ASSETS_RELATIVE_PATH: &str = "user/document_assets.json";
pub const READING_SESSIONS_RELATIVE_PATH: &str = "user/reading_sessions.json";
pub const SOURCE_BATCHES_RELATIVE_DIR: &str = "user/sources";
pub const VAULT_FILES_RELATIVE_DIR: &str = "files";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LiteratureItemType {
    Article,
    Book,
    Chapter,
    Other,
}

impl Default for LiteratureItemType {
    fn default() -> Self {
        Self::Article
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReadingStatus {
    Inbox,
    Reading,
    Finished,
    Archived,
}

impl Default for ReadingStatus {
    fn default() -> Self {
        Self::Inbox
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LiteratureFileKind {
    Vault,
    External,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LiteratureFileRef {
    pub file_id: Uuid,
    pub kind: LiteratureFileKind,
    pub path: String,
    pub display_name: String,
}

/// The semantic role of a concrete document file within a literature item.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DocumentAssetKind {
    Primary,
    Supplement,
    Version,
    Appendix,
    Other,
}

impl Default for DocumentAssetKind {
    fn default() -> Self {
        Self::Primary
    }
}

/// How a document asset is stored relative to the Vault.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DocumentAssetStorageKind {
    Vault,
    External,
}

impl Default for DocumentAssetStorageKind {
    fn default() -> Self {
        Self::Vault
    }
}

/// A runtime health result. It is deliberately not persisted: the filesystem is
/// the authority for availability, not a stale value in user JSON.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum DocumentAssetStatus {
    Available,
    Missing,
    Unreadable,
    Invalid,
    ExternalUnavailable,
}

/// Lifecycle state for a user-owned reading behavior record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReadingSessionState {
    Active,
    Paused,
    Closed,
}

/// A portable record of one reading behavior, referring only to cistella IDs.
///
/// It deliberately does not persist a file path or runtime asset health. A
/// missing or unreadable file can therefore leave its session history intact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReadingSession {
    pub session_id: Uuid,
    pub item_id: Uuid,
    pub asset_id: Uuid,
    pub started_at: DateTime<Utc>,
    pub last_opened_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub state: ReadingSessionState,
}

/// A reading-session record together with the current runtime health of its
/// referenced asset. The status is derived on each read and is never persisted
/// into the portable session JSON.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReadingSessionSummary {
    pub session: ReadingSession,
    pub asset_status: DocumentAssetStatus,
}

/// A concrete file belonging to exactly one LiteratureItem.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DocumentAsset {
    pub asset_id: Uuid,
    pub item_id: Uuid,
    pub asset_kind: DocumentAssetKind,
    pub storage_kind: DocumentAssetStorageKind,
    /// Vault-relative for `vault`, absolute for `external`.
    pub path: String,
    pub display_name: String,
    pub media_type: String,
    pub file_size: Option<u64>,
    pub content_hash: Option<String>,
    pub imported_at: Option<DateTime<Utc>>,
    /// Compatibility replacement for LiteratureItem.default_file_id.
    #[serde(default)]
    pub is_default: bool,
}

/// A successful result from importing a local document into a Vault.
///
/// A duplicate is an explicit non-mutating result: cistella returns the
/// existing same-item asset and does not create a second file or JSON record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "outcome", rename_all = "camelCase")]
pub enum DocumentAssetImportResult {
    Imported { asset: DocumentAsset },
    Duplicate { existing: DocumentAsset },
}

impl DocumentAsset {
    pub fn status(&self, vault: &Vault) -> DocumentAssetStatus {
        if !self.media_type.eq_ignore_ascii_case("application/pdf")
            || !is_pdf_path(Path::new(&self.path))
        {
            return DocumentAssetStatus::Invalid;
        }

        match self.storage_kind {
            DocumentAssetStorageKind::Vault => {
                let relative = match validate_vault_item_relative_path(self.item_id, &self.path) {
                    Ok(relative) => relative,
                    Err(_) => return DocumentAssetStatus::Invalid,
                };
                let target = vault.root_path().join(relative);
                let canonical_root = match fs::canonicalize(vault.root_path()) {
                    Ok(root) => root,
                    Err(_) => return DocumentAssetStatus::Invalid,
                };
                let canonical_target = match fs::canonicalize(&target) {
                    Ok(target) => target,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        return DocumentAssetStatus::Missing;
                    }
                    Err(_) => return DocumentAssetStatus::Unreadable,
                };
                let canonical_item_dir = match fs::canonicalize(
                    vault
                        .root_path()
                        .join(VAULT_FILES_RELATIVE_DIR)
                        .join(self.item_id.to_string()),
                ) {
                    Ok(item_dir) => item_dir,
                    Err(_) => return DocumentAssetStatus::Invalid,
                };
                if !canonical_target.starts_with(&canonical_root)
                    || !canonical_target.starts_with(&canonical_item_dir)
                {
                    return DocumentAssetStatus::Invalid;
                }
                match fs::metadata(canonical_target) {
                    Ok(metadata) if metadata.is_file() => DocumentAssetStatus::Available,
                    Ok(_) => DocumentAssetStatus::Invalid,
                    Err(_) => DocumentAssetStatus::Unreadable,
                }
            }
            DocumentAssetStorageKind::External => {
                let path = Path::new(&self.path);
                if !path.is_absolute() {
                    return DocumentAssetStatus::Invalid;
                }
                match fs::metadata(path) {
                    Ok(metadata) if metadata.is_file() => DocumentAssetStatus::Available,
                    Ok(_) => DocumentAssetStatus::Invalid,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        DocumentAssetStatus::ExternalUnavailable
                    }
                    Err(_) => DocumentAssetStatus::Unreadable,
                }
            }
        }
    }

    fn from_legacy(item_id: Uuid, legacy: &LiteratureFileRef, is_default: bool) -> Self {
        Self {
            // Legacy file_id was already a cistella-generated UUID. Reusing it
            // makes the compatibility projection stable across every read.
            asset_id: legacy.file_id,
            item_id,
            asset_kind: DocumentAssetKind::Primary,
            storage_kind: match legacy.kind {
                LiteratureFileKind::Vault => DocumentAssetStorageKind::Vault,
                LiteratureFileKind::External => DocumentAssetStorageKind::External,
            },
            path: legacy.path.clone(),
            display_name: legacy.display_name.clone(),
            media_type: "application/pdf".to_string(),
            file_size: None,
            content_hash: None,
            imported_at: None,
            is_default,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LiteratureSourceRef {
    pub source_name: String,
    pub external_id: Option<String>,
    pub original_locator: Option<String>,
}

/// An external identifier attached to a literature item.
///
/// Namespace is lowercase and stable (e.g. `doi`, `isbn`, `pmid`, `openalex`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct ExternalIdentifier {
    pub namespace: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LiteratureItem {
    pub item_id: Uuid,
    pub title: String,
    pub authors: Vec<String>,
    pub published_year: Option<i32>,
    pub item_type: LiteratureItemType,
    pub favorite: bool,
    pub reading_status: ReadingStatus,
    pub tags: Vec<String>,
    pub sources: Vec<LiteratureSourceRef>,
    #[serde(default)]
    pub files: Vec<LiteratureFileRef>,
    #[serde(default)]
    pub default_file_id: Option<Uuid>,
    #[serde(default)]
    pub external_identifiers: Vec<ExternalIdentifier>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LiteratureItemDraft {
    pub title: String,
    pub authors: Vec<String>,
    pub published_year: Option<i32>,
    pub item_type: LiteratureItemType,
    pub favorite: bool,
    pub reading_status: ReadingStatus,
    pub tags: Vec<String>,
    #[serde(default)]
    pub sources: Vec<LiteratureSourceRef>,
    #[serde(default)]
    pub external_identifiers: Vec<ExternalIdentifier>,
}

impl LiteratureItem {
    pub fn from_draft(draft: LiteratureItemDraft) -> Self {
        Self {
            item_id: Uuid::new_v4(),
            title: draft.title,
            authors: draft.authors,
            published_year: draft.published_year,
            item_type: draft.item_type,
            favorite: draft.favorite,
            reading_status: draft.reading_status,
            tags: draft.tags,
            sources: draft.sources,
            files: Vec::new(),
            default_file_id: None,
            external_identifiers: draft.external_identifiers,
        }
    }
}

impl Vault {
    pub fn literature_items_path(&self) -> PathBuf {
        self.root_path().join(LITERATURE_ITEMS_RELATIVE_PATH)
    }

    pub fn document_assets_path(&self) -> PathBuf {
        self.root_path().join(DOCUMENT_ASSETS_RELATIVE_PATH)
    }

    pub fn reading_sessions_path(&self) -> PathBuf {
        self.root_path().join(READING_SESSIONS_RELATIVE_PATH)
    }

    pub fn sources_dir(&self) -> PathBuf {
        self.root_path().join(SOURCE_BATCHES_RELATIVE_DIR)
    }

    /// Inspects a byte payload in the given format and returns an in-memory
    /// import preview.
    pub fn inspect_literature_import(
        &self,
        format: ImportFormat,
        bytes: &[u8],
    ) -> Result<ImportPreview> {
        let importer = LiteratureLibraryImporter::new(format);
        let records = importer.parse(bytes)?;
        importer.preview(self, records)
    }

    /// Commits a previously inspected import preview, writing both the source
    /// batch and the updated literature items atomically.
    pub fn commit_literature_import(
        &self,
        format: ImportFormat,
        preview: ImportPreview,
    ) -> Result<ImportResult> {
        LiteratureLibraryImporter::new(format).commit(self, preview)
    }

    /// Builds an in-memory import preview from already-normalized source records.
    pub fn preview_literature_records(
        &self,
        format: ImportFormat,
        records: Vec<crate::SourceRecord>,
    ) -> Result<ImportPreview> {
        LiteratureLibraryImporter::new(format).preview(self, records)
    }

    /// Resolves a DOI using a local OpenAlex works snapshot.
    pub fn resolve_doi_via_local_openalex(
        &self,
        raw_sources_dir: impl AsRef<Path>,
        doi: &str,
    ) -> Result<Option<crate::SourceRecord>> {
        resolve_local_openalex_doi(raw_sources_dir, doi)
    }

    /// Searches a local OpenAlex works snapshot by title, DOI, or OpenAlex ID.
    pub fn search_local_openalex_works(
        &self,
        raw_sources_dir: impl AsRef<Path>,
        query: &str,
        limit: usize,
    ) -> Result<Vec<crate::SourceRecord>> {
        list_local_openalex_works(raw_sources_dir, query, limit)
    }

    /// Inspects a BibTeX byte payload and returns an in-memory import preview.
    pub fn inspect_bibtex_import(&self, bytes: &[u8]) -> Result<ImportPreview> {
        self.inspect_literature_import(ImportFormat::BibTeX, bytes)
    }

    /// Commits a previously inspected BibTeX import preview.
    pub fn commit_bibtex_import(&self, preview: ImportPreview) -> Result<ImportResult> {
        self.commit_literature_import(ImportFormat::BibTeX, preview)
    }

    /// Loads all persisted source batches from the Vault.
    pub fn load_source_batches(&self) -> Result<Vec<SourceBatch>> {
        load_source_batches_impl(self)
    }

    /// Loads the new asset store when present. If it does not exist, projects
    /// the legacy LiteratureItem.files records without rewriting them. A
    /// present (even empty) document_assets.json is authoritative and never
    /// merged with the legacy file.
    pub fn load_document_assets(&self) -> Result<Vec<DocumentAsset>> {
        let path = self.document_assets_path();
        if path.exists() {
            let bytes = fs::read(path)?;
            return Ok(serde_json::from_slice(&bytes)?);
        }

        let items = self.load_literature_items()?;
        Ok(items
            .iter()
            .flat_map(|item| {
                item.files.iter().map(|file| {
                    DocumentAsset::from_legacy(
                        item.item_id,
                        file,
                        item.default_file_id == Some(file.file_id),
                    )
                })
            })
            .collect())
    }

    /// Writes only the new asset store. Legacy LiteratureItem JSON is left
    /// untouched so older clients can continue reading their original data.
    pub fn save_document_assets(&self, assets: &[DocumentAsset]) -> Result<()> {
        let path = self.document_assets_path();
        let parent = path
            .parent()
            .ok_or_else(|| CoreError::InvalidUserDataPath(path.display().to_string()))?;
        fs::create_dir_all(parent)?;
        let payload = serde_json::to_vec_pretty(assets)?;
        atomic_replace_named(&path, &payload, ".document_assets")
    }

    /// Reads portable reading behavior records. A missing session file is an
    /// empty history; records are not discarded merely because an asset's
    /// current file has gone missing or become unreadable.
    pub fn load_reading_sessions(&self) -> Result<Vec<ReadingSession>> {
        let path = self.reading_sessions_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let bytes = fs::read(path)?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// Atomically persists reading behavior records after proving that every
    /// `item_id + asset_id` pair names an existing association. It intentionally
    /// does not resolve the file path, so an otherwise valid session survives a
    /// missing, unreadable, or disconnected asset target.
    pub fn save_reading_sessions(&self, sessions: &[ReadingSession]) -> Result<()> {
        self.save_reading_sessions_with(sessions, |path, payload| {
            atomic_replace_named(path, payload, ".reading_sessions")
        })
    }

    /// Checks that a session references an existing item and an asset owned by
    /// exactly that item. This is relationship validation, not availability
    /// validation: no filesystem path is opened here.
    pub fn validate_reading_session_association(
        &self,
        item_id: Uuid,
        asset_id: Uuid,
    ) -> Result<DocumentAsset> {
        if !self
            .load_literature_items()?
            .iter()
            .any(|item| item.item_id == item_id)
        {
            return Err(CoreError::LiteratureItemNotFound(item_id.to_string()));
        }
        self.load_document_assets()?
            .into_iter()
            .find(|asset| asset.item_id == item_id && asset.asset_id == asset_id)
            .ok_or_else(|| CoreError::InvalidReadingSessionAssociation {
                item_id: item_id.to_string(),
                asset_id: asset_id.to_string(),
            })
    }

    fn save_reading_sessions_with<F>(&self, sessions: &[ReadingSession], persist: F) -> Result<()>
    where
        F: FnOnce(&Path, &[u8]) -> Result<()>,
    {
        for session in sessions {
            self.validate_reading_session_association(session.item_id, session.asset_id)?;
        }
        let path = self.reading_sessions_path();
        let parent = path
            .parent()
            .ok_or_else(|| CoreError::InvalidUserDataPath(path.display().to_string()))?;
        fs::create_dir_all(parent)?;
        let payload = serde_json::to_vec_pretty(sessions)?;
        persist(&path, &payload)
    }

    /// Returns portable session history ordered from most recently opened to
    /// least recently opened. Broken or removed asset associations stay visible
    /// as `invalid`, while missing or disconnected files retain their normal
    /// runtime health result.
    pub fn list_recent_reading_sessions(&self) -> Result<Vec<ReadingSessionSummary>> {
        let assets = self.load_document_assets()?;
        let mut summaries = self
            .load_reading_sessions()?
            .into_iter()
            .map(|session| ReadingSessionSummary {
                asset_status: reading_session_asset_status(self, &assets, &session),
                session,
            })
            .collect::<Vec<_>>();
        summaries.sort_by(|left, right| reading_session_recency(&right.session, &left.session));
        Ok(summaries)
    }

    /// Selects the deterministic target for the Reading workspace's continue
    /// action: the most recently opened non-closed session, otherwise the most
    /// recently opened historical session. The method never opens a file or
    /// writes user data.
    pub fn continue_reading_target(&self) -> Result<Option<ReadingSessionSummary>> {
        let sessions = self.load_reading_sessions()?;
        let selected = sessions
            .iter()
            .filter(|session| session.state != ReadingSessionState::Closed)
            .max_by(|left, right| reading_session_recency(left, right))
            .or_else(|| {
                sessions
                    .iter()
                    .max_by(|left, right| reading_session_recency(left, right))
            })
            .cloned();
        let assets = self.load_document_assets()?;
        Ok(selected.map(|session| ReadingSessionSummary {
            asset_status: reading_session_asset_status(self, &assets, &session),
            session,
        }))
    }

    /// Starts a new active session only after `request_open` accepts a
    /// controlled, validated asset path. The callback is deliberately supplied
    /// by the desktop shell so Core never grants a raw-path opening capability.
    /// A rejected request leaves `reading_sessions.json` untouched.
    pub fn start_reading_session<F>(
        &self,
        item_id: Uuid,
        asset_id: Uuid,
        request_open: F,
    ) -> Result<ReadingSession>
    where
        F: FnOnce(&Path) -> Result<()>,
    {
        self.validate_reading_session_association(item_id, asset_id)?;
        let path = self.resolve_document_asset_path(item_id, asset_id)?;
        request_open(&path)?;

        let now = Utc::now();
        let session = ReadingSession {
            session_id: Uuid::new_v4(),
            item_id,
            asset_id,
            started_at: now,
            last_opened_at: now,
            ended_at: None,
            state: ReadingSessionState::Active,
        };
        let mut sessions = self.load_reading_sessions()?;
        sessions.push(session.clone());
        self.save_reading_sessions(&sessions)?;
        Ok(session)
    }

    /// Resumes the newest non-closed session for one explicit item/asset pair.
    /// The state and `last_opened_at` change only after the supplied controlled
    /// open request succeeds.
    pub fn resume_reading_session<F>(
        &self,
        item_id: Uuid,
        asset_id: Uuid,
        request_open: F,
    ) -> Result<ReadingSession>
    where
        F: FnOnce(&Path) -> Result<()>,
    {
        self.validate_reading_session_association(item_id, asset_id)?;
        let mut sessions = self.load_reading_sessions()?;
        let index = most_recent_reading_session_index(&sessions, |session| {
            session.item_id == item_id
                && session.asset_id == asset_id
                && session.state != ReadingSessionState::Closed
        })
        .ok_or_else(|| CoreError::ReadingSessionNotFound {
            item_id: item_id.to_string(),
            asset_id: asset_id.to_string(),
        })?;
        let path = self.resolve_document_asset_path(item_id, asset_id)?;
        request_open(&path)?;

        let session = &mut sessions[index];
        session.state = ReadingSessionState::Active;
        session.ended_at = None;
        session.last_opened_at = Utc::now();
        let result = session.clone();
        self.save_reading_sessions(&sessions)?;
        Ok(result)
    }

    /// Pauses the newest active session for one explicit item/asset pair. This
    /// is a lifecycle update only and intentionally does not resolve or open a
    /// file, so a session can be paused while an external target is unavailable.
    pub fn pause_reading_session(&self, item_id: Uuid, asset_id: Uuid) -> Result<ReadingSession> {
        self.validate_reading_session_association(item_id, asset_id)?;
        let mut sessions = self.load_reading_sessions()?;
        let index = most_recent_reading_session_index(&sessions, |session| {
            session.item_id == item_id
                && session.asset_id == asset_id
                && session.state == ReadingSessionState::Active
        })
        .ok_or_else(|| CoreError::ReadingSessionNotFound {
            item_id: item_id.to_string(),
            asset_id: asset_id.to_string(),
        })?;
        sessions[index].state = ReadingSessionState::Paused;
        let result = sessions[index].clone();
        self.save_reading_sessions(&sessions)?;
        Ok(result)
    }

    /// Closes the newest non-closed session for one explicit item/asset pair.
    /// Closing never changes `last_opened_at` and never attempts to delete a
    /// file or an asset record.
    pub fn end_reading_session(&self, item_id: Uuid, asset_id: Uuid) -> Result<ReadingSession> {
        self.validate_reading_session_association(item_id, asset_id)?;
        let mut sessions = self.load_reading_sessions()?;
        let index = most_recent_reading_session_index(&sessions, |session| {
            session.item_id == item_id
                && session.asset_id == asset_id
                && session.state != ReadingSessionState::Closed
        })
        .ok_or_else(|| CoreError::ReadingSessionNotFound {
            item_id: item_id.to_string(),
            asset_id: asset_id.to_string(),
        })?;
        sessions[index].state = ReadingSessionState::Closed;
        sessions[index].ended_at = Some(Utc::now());
        let result = sessions[index].clone();
        self.save_reading_sessions(&sessions)?;
        Ok(result)
    }

    /// Continues the deterministic target returned by
    /// `continue_reading_target`. An existing non-closed target is reactivated;
    /// when only closed history exists, a new active session starts against the
    /// selected historical item/asset pair. In either case no timestamp changes
    /// until the controlled open request succeeds.
    pub fn continue_reading_session<F>(&self, request_open: F) -> Result<ReadingSession>
    where
        F: FnOnce(&Path) -> Result<()>,
    {
        let target = self
            .continue_reading_target()?
            .ok_or(CoreError::NoReadingSessionHistory)?;
        let item_id = target.session.item_id;
        let asset_id = target.session.asset_id;
        self.validate_reading_session_association(item_id, asset_id)?;
        let mut sessions = self.load_reading_sessions()?;
        let index = sessions
            .iter()
            .position(|session| session.session_id == target.session.session_id)
            .ok_or(CoreError::NoReadingSessionHistory)?;
        let path = self.resolve_document_asset_path(item_id, asset_id)?;
        request_open(&path)?;

        let now = Utc::now();
        let result = if sessions[index].state == ReadingSessionState::Closed {
            let session = ReadingSession {
                session_id: Uuid::new_v4(),
                item_id,
                asset_id,
                started_at: now,
                last_opened_at: now,
                ended_at: None,
                state: ReadingSessionState::Active,
            };
            sessions.push(session.clone());
            session
        } else {
            let session = &mut sessions[index];
            session.state = ReadingSessionState::Active;
            session.ended_at = None;
            session.last_opened_at = now;
            session.clone()
        };
        self.save_reading_sessions(&sessions)?;
        Ok(result)
    }

    pub fn load_literature_items(&self) -> Result<Vec<LiteratureItem>> {
        let path = self.literature_items_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let bytes = fs::read(path)?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub fn save_literature_items(&self, items: &[LiteratureItem]) -> Result<()> {
        let path = self.literature_items_path();
        let parent = path
            .parent()
            .ok_or_else(|| CoreError::InvalidUserDataPath(path.display().to_string()))?;
        fs::create_dir_all(parent)?;
        let payload = serde_json::to_vec_pretty(items)?;
        atomic_replace(&path, &payload)
    }

    pub fn create_literature_item(&self, draft: LiteratureItemDraft) -> Result<LiteratureItem> {
        let mut items = self.load_literature_items()?;
        let item = LiteratureItem::from_draft(draft);
        items.push(item.clone());
        self.save_literature_items(&items)?;
        self.try_apply_search_index_change(SearchIndexChange::MetadataChanged {
            item_id: item.item_id,
        });
        Ok(item)
    }

    pub fn update_literature_item(
        &self,
        item_id: Uuid,
        draft: LiteratureItemDraft,
    ) -> Result<LiteratureItem> {
        let mut items = self.load_literature_items()?;
        let item = self.find_literature_item_mut(&mut items, item_id)?;
        item.title = draft.title;
        item.authors = draft.authors;
        item.published_year = draft.published_year;
        item.item_type = draft.item_type;
        item.favorite = draft.favorite;
        item.reading_status = draft.reading_status;
        item.tags = draft.tags;
        item.sources = draft.sources;
        if !draft.external_identifiers.is_empty() {
            item.external_identifiers = draft.external_identifiers;
        }
        let result = item.clone();
        self.save_literature_items(&items)?;
        self.try_apply_search_index_change(SearchIndexChange::MetadataChanged { item_id });
        Ok(result)
    }

    /// Deletes one literature item together with its first-class asset records.
    ///
    /// Physical Vault files and external targets deliberately remain untouched.
    /// When the independent asset store exists, its filtered contents are
    /// published before the item list so a failed item write can never leave a
    /// deleted item behind with invisible, unmanageable DocumentAsset records.
    /// A failed second write is compensated by restoring the original asset
    /// store; if that restoration itself fails, the item remains and the
    /// resulting state still has no orphaned asset records.
    pub fn delete_literature_item(&self, item_id: Uuid) -> Result<()> {
        self.delete_literature_item_with(
            item_id,
            |vault, assets| vault.save_document_assets(assets),
            |vault, items| vault.save_literature_items(items),
            |vault, assets| vault.save_document_assets(assets),
        )?;
        self.try_apply_search_index_change(SearchIndexChange::ItemRemoved { item_id });
        Ok(())
    }

    fn delete_literature_item_with<PublishAssets, PublishItems, RestoreAssets>(
        &self,
        item_id: Uuid,
        publish_assets: PublishAssets,
        publish_items: PublishItems,
        restore_assets: RestoreAssets,
    ) -> Result<()>
    where
        PublishAssets: FnOnce(&Vault, &[DocumentAsset]) -> Result<()>,
        PublishItems: FnOnce(&Vault, &[LiteratureItem]) -> Result<()>,
        RestoreAssets: FnOnce(&Vault, &[DocumentAsset]) -> Result<()>,
    {
        let mut items = self.load_literature_items()?;
        let old_len = items.len();
        items.retain(|item| item.item_id != item_id);
        if items.len() == old_len {
            return Err(CoreError::LiteratureItemNotFound(item_id.to_string()));
        }

        // If the new store does not yet exist, the only asset associations are
        // the legacy fields inside LiteratureItem and the single item JSON
        // write below removes them atomically with the item. Do not create a
        // new authoritative asset store merely because a legacy item is being
        // deleted.
        if !self.document_assets_path().exists() {
            return publish_items(self, &items);
        }

        let original_assets = self.load_document_assets()?;
        let remaining_assets = original_assets
            .iter()
            .filter(|asset| asset.item_id != item_id)
            .cloned()
            .collect::<Vec<_>>();

        // Asset-first publication prevents the unsafe direction: a removed
        // item must never become committed while its DocumentAsset records
        // still point at it.
        publish_assets(self, &remaining_assets)?;
        if let Err(item_write_error) = publish_items(self, &items) {
            if let Err(asset_restore_error) = restore_assets(self, &original_assets) {
                return Err(CoreError::LiteratureItemDeleteRollbackFailed {
                    item_write_error: item_write_error.to_string(),
                    asset_restore_error: asset_restore_error.to_string(),
                });
            }
            return Err(item_write_error);
        }

        Ok(())
    }

    pub fn set_literature_item_favorite(
        &self,
        item_id: Uuid,
        favorite: bool,
    ) -> Result<LiteratureItem> {
        self.mutate_literature_item(item_id, |item| item.favorite = favorite)
    }

    pub fn set_literature_item_tags(
        &self,
        item_id: Uuid,
        tags: Vec<String>,
    ) -> Result<LiteratureItem> {
        self.mutate_literature_item(item_id, |item| item.tags = tags)
    }

    pub fn set_literature_item_reading_status(
        &self,
        item_id: Uuid,
        status: ReadingStatus,
    ) -> Result<LiteratureItem> {
        self.mutate_literature_item(item_id, |item| item.reading_status = status)
    }

    /// Imports a selected PDF as a first-class DocumentAsset.
    ///
    /// The source is validated and hashed before any Vault file or user JSON
    /// changes. The destination is a unique, item-scoped path and is published
    /// without overwriting a pre-existing file.
    pub fn import_document_asset(
        &self,
        item_id: Uuid,
        source_path: impl AsRef<Path>,
        asset_kind: DocumentAssetKind,
    ) -> Result<DocumentAssetImportResult> {
        let result = self.import_document_asset_with(
            item_id,
            source_path.as_ref(),
            asset_kind,
            None,
            |vault, assets| vault.save_document_assets(assets),
        )?;
        if let DocumentAssetImportResult::Imported { asset } = &result {
            self.try_apply_search_index_change(SearchIndexChange::AssetChanged {
                item_id: asset.item_id,
                asset_id: asset.asset_id,
            });
        }
        Ok(result)
    }

    fn import_document_asset_with<F>(
        &self,
        item_id: Uuid,
        source_path: &Path,
        asset_kind: DocumentAssetKind,
        exclude_duplicate_asset_id: Option<Uuid>,
        publish_assets: F,
    ) -> Result<DocumentAssetImportResult>
    where
        F: FnOnce(&Vault, &[DocumentAsset]) -> Result<()>,
    {
        let source_path = validate_existing_pdf(source_path)?;
        let source_metadata = fs::metadata(&source_path)?;
        if !source_metadata.file_type().is_file() {
            return Err(CoreError::InvalidLiteratureFilePath(
                source_path.display().to_string(),
            ));
        }
        let source_name = source_path.file_name().ok_or_else(|| {
            CoreError::InvalidLiteratureFilePath(source_path.display().to_string())
        })?;
        let safe_name = portable_pdf_file_name(source_name)?;
        let content_hash = sha256_file(&source_path)?;

        self.load_literature_items()?
            .iter()
            .find(|item| item.item_id == item_id)
            .ok_or_else(|| CoreError::LiteratureItemNotFound(item_id.to_string()))?;
        let mut assets = self.load_document_assets()?;

        if let Some(existing) = assets.iter().find(|asset| {
            asset.item_id == item_id
                && Some(asset.asset_id) != exclude_duplicate_asset_id
                && asset.content_hash.as_deref() == Some(content_hash.as_str())
        }) {
            return Ok(DocumentAssetImportResult::Duplicate {
                existing: existing.clone(),
            });
        }

        let asset_id = Uuid::new_v4();
        let relative_path = Path::new(VAULT_FILES_RELATIVE_DIR)
            .join(item_id.to_string())
            .join(format!("{asset_id}-{safe_name}"));
        let destination = self.prepare_document_asset_destination(item_id, &relative_path)?;
        copy_to_new_vault_file(&source_path, &destination)?;

        let asset = DocumentAsset {
            asset_id,
            item_id,
            asset_kind,
            storage_kind: DocumentAssetStorageKind::Vault,
            path: portable_relative_path(&relative_path)?,
            display_name: source_name.to_string_lossy().to_string(),
            media_type: "application/pdf".to_string(),
            file_size: Some(source_metadata.len()),
            content_hash: Some(content_hash),
            imported_at: Some(Utc::now()),
            is_default: !assets
                .iter()
                .any(|candidate| candidate.item_id == item_id && candidate.is_default),
        };
        assets.push(asset.clone());
        if let Err(error) = publish_assets(self, &assets) {
            let _ = fs::remove_file(&destination);
            return Err(error);
        }

        Ok(DocumentAssetImportResult::Imported { asset })
    }

    /// Creates a first-class external PDF asset. The selected file is not copied
    /// and remains an explicitly non-portable link.
    pub fn link_external_document_asset(
        &self,
        item_id: Uuid,
        external_path: impl AsRef<Path>,
        asset_kind: DocumentAssetKind,
    ) -> Result<DocumentAsset> {
        self.load_literature_items()?
            .iter()
            .find(|item| item.item_id == item_id)
            .ok_or_else(|| CoreError::LiteratureItemNotFound(item_id.to_string()))?;
        let path = validate_existing_pdf(external_path.as_ref())?;
        if !path.is_absolute() {
            return Err(CoreError::InvalidDocumentAssetPath(
                path.display().to_string(),
            ));
        }
        let metadata = fs::metadata(&path)?;
        if !metadata.is_file() {
            return Err(CoreError::InvalidDocumentAssetPath(
                path.display().to_string(),
            ));
        }
        let display_name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .filter(|name| !name.is_empty())
            .ok_or_else(|| CoreError::InvalidDocumentAssetPath(path.display().to_string()))?;
        let mut assets = self.load_document_assets()?;
        let asset = DocumentAsset {
            asset_id: Uuid::new_v4(),
            item_id,
            asset_kind,
            storage_kind: DocumentAssetStorageKind::External,
            path: path.to_string_lossy().to_string(),
            display_name,
            media_type: "application/pdf".to_string(),
            file_size: Some(metadata.len()),
            content_hash: Some(sha256_file(&path)?),
            imported_at: Some(Utc::now()),
            is_default: !assets
                .iter()
                .any(|candidate| candidate.item_id == item_id && candidate.is_default),
        };
        assets.push(asset.clone());
        self.save_document_assets(&assets)?;
        self.try_apply_search_index_change(SearchIndexChange::AssetChanged {
            item_id: asset.item_id,
            asset_id: asset.asset_id,
        });
        Ok(asset)
    }

    /// Imports an existing external link as a new Vault asset. The original
    /// external asset remains intact; users must remove it explicitly.
    pub fn migrate_external_document_asset(
        &self,
        item_id: Uuid,
        asset_id: Uuid,
    ) -> Result<DocumentAssetImportResult> {
        let assets = self.load_document_assets()?;
        let external = assets
            .iter()
            .find(|asset| asset.item_id == item_id && asset.asset_id == asset_id)
            .ok_or_else(|| CoreError::LiteratureFileNotFound(asset_id.to_string()))?;
        if external.storage_kind != DocumentAssetStorageKind::External {
            return Err(CoreError::InvalidDocumentAssetPath(external.path.clone()));
        }
        let result = self.import_document_asset_with(
            item_id,
            Path::new(&external.path),
            external.asset_kind.clone(),
            Some(asset_id),
            |vault, next_assets| vault.save_document_assets(next_assets),
        )?;
        if let DocumentAssetImportResult::Imported { asset } = &result {
            self.try_apply_search_index_change(SearchIndexChange::AssetChanged {
                item_id: asset.item_id,
                asset_id: asset.asset_id,
            });
        }
        Ok(result)
    }

    /// Changes only the semantic asset role. The physical file and all other
    /// metadata remain untouched.
    pub fn set_document_asset_kind(
        &self,
        item_id: Uuid,
        asset_id: Uuid,
        asset_kind: DocumentAssetKind,
    ) -> Result<DocumentAsset> {
        let mut assets = self.load_document_assets()?;
        let asset = assets
            .iter_mut()
            .find(|asset| asset.item_id == item_id && asset.asset_id == asset_id)
            .ok_or_else(|| CoreError::LiteratureFileNotFound(asset_id.to_string()))?;
        asset.asset_kind = asset_kind;
        let result = asset.clone();
        self.save_document_assets(&assets)?;
        self.try_apply_search_index_change(SearchIndexChange::AssetChanged { item_id, asset_id });
        Ok(result)
    }

    /// Sets one explicit default per item. Array ordering is never used as a
    /// fallback default.
    pub fn set_document_asset_default(
        &self,
        item_id: Uuid,
        asset_id: Uuid,
    ) -> Result<DocumentAsset> {
        let mut assets = self.load_document_assets()?;
        if !assets
            .iter()
            .any(|asset| asset.item_id == item_id && asset.asset_id == asset_id)
        {
            return Err(CoreError::LiteratureFileNotFound(asset_id.to_string()));
        }
        let mut result = None;
        for asset in assets.iter_mut().filter(|asset| asset.item_id == item_id) {
            asset.is_default = asset.asset_id == asset_id;
            if asset.is_default {
                result = Some(asset.clone());
            }
        }
        self.save_document_assets(&assets)?;
        let result =
            result.ok_or_else(|| CoreError::LiteratureFileNotFound(asset_id.to_string()))?;
        self.try_apply_search_index_change(SearchIndexChange::AssetChanged { item_id, asset_id });
        Ok(result)
    }

    /// Removes only the asset relationship and record. It never deletes an
    /// external target or a portable Vault file implicitly.
    pub fn remove_document_asset(&self, item_id: Uuid, asset_id: Uuid) -> Result<DocumentAsset> {
        let mut assets = self.load_document_assets()?;
        let index = assets
            .iter()
            .position(|asset| asset.item_id == item_id && asset.asset_id == asset_id)
            .ok_or_else(|| CoreError::LiteratureFileNotFound(asset_id.to_string()))?;
        let removed = assets.remove(index);
        self.save_document_assets(&assets)?;
        self.try_apply_search_index_change(SearchIndexChange::AssetRemoved { item_id, asset_id });
        Ok(removed)
    }

    /// Resolves a first-class DocumentAsset through its item and asset IDs.
    /// No caller can supply an arbitrary open path.
    pub fn resolve_document_asset_path(&self, item_id: Uuid, asset_id: Uuid) -> Result<PathBuf> {
        let assets = self.load_document_assets()?;
        let asset = assets
            .iter()
            .find(|asset| asset.item_id == item_id && asset.asset_id == asset_id)
            .ok_or_else(|| CoreError::LiteratureFileNotFound(asset_id.to_string()))?;
        match asset.storage_kind {
            DocumentAssetStorageKind::Vault => self.resolve_vault_file_path(item_id, &asset.path),
            DocumentAssetStorageKind::External => resolve_external_file_path(&asset.path),
        }
    }

    /// Copies a selected PDF into this Vault and records the portable relative association.
    pub fn add_literature_vault_file(
        &self,
        item_id: Uuid,
        source_path: impl AsRef<Path>,
    ) -> Result<LiteratureFileRef> {
        let source_path = source_path.as_ref();
        let canonical_source = validate_existing_pdf(source_path)?;
        let source_name =
            portable_pdf_file_name(canonical_source.file_name().ok_or_else(|| {
                CoreError::InvalidLiteratureFilePath(canonical_source.display().to_string())
            })?)?;

        let mut items = self.load_literature_items()?;
        self.find_literature_item_mut(&mut items, item_id)?;

        let relative_path = self.next_vault_file_relative_path(item_id, &source_name)?;
        let destination = self.prepare_vault_copy_destination(&relative_path)?;
        fs::copy(&canonical_source, &destination)?;

        let file = LiteratureFileRef {
            file_id: Uuid::new_v4(),
            kind: LiteratureFileKind::Vault,
            path: portable_relative_path(&relative_path)?,
            display_name: source_name,
        };

        let save_result = {
            let item = self.find_literature_item_mut(&mut items, item_id)?;
            if item.default_file_id.is_none() {
                item.default_file_id = Some(file.file_id);
            }
            item.files.push(file.clone());
            self.save_literature_items(&items)
        };
        if let Err(error) = save_result {
            let _ = fs::remove_file(&destination);
            return Err(error);
        }
        Ok(file)
    }

    /// Records an explicit, non-portable absolute PDF link without copying it into the Vault.
    pub fn add_literature_external_file(
        &self,
        item_id: Uuid,
        external_path: impl AsRef<Path>,
    ) -> Result<LiteratureFileRef> {
        let canonical_path = validate_existing_pdf(external_path.as_ref())?;
        if !canonical_path.is_absolute() {
            return Err(CoreError::InvalidLiteratureFilePath(
                canonical_path.display().to_string(),
            ));
        }
        let display_name = canonical_path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .filter(|name| !name.is_empty())
            .ok_or_else(|| {
                CoreError::InvalidLiteratureFilePath(canonical_path.display().to_string())
            })?;
        let file = LiteratureFileRef {
            file_id: Uuid::new_v4(),
            kind: LiteratureFileKind::External,
            path: canonical_path.to_string_lossy().to_string(),
            display_name,
        };
        let mut items = self.load_literature_items()?;
        let item = self.find_literature_item_mut(&mut items, item_id)?;
        if item.default_file_id.is_none() {
            item.default_file_id = Some(file.file_id);
        }
        item.files.push(file.clone());
        self.save_literature_items(&items)?;
        Ok(file)
    }

    /// Removes only the association. Vault user assets are never deleted implicitly.
    pub fn remove_literature_file(&self, item_id: Uuid, file_id: Uuid) -> Result<LiteratureItem> {
        let mut items = self.load_literature_items()?;
        let item = self.find_literature_item_mut(&mut items, item_id)?;
        let index = item
            .files
            .iter()
            .position(|file| file.file_id == file_id)
            .ok_or_else(|| CoreError::LiteratureFileNotFound(file_id.to_string()))?;
        item.files.remove(index);
        if item.default_file_id == Some(file_id) {
            // A replacement default is intentionally never guessed from array order.
            item.default_file_id = None;
        }
        let result = item.clone();
        self.save_literature_items(&items)?;
        Ok(result)
    }

    pub fn set_literature_default_file(
        &self,
        item_id: Uuid,
        file_id: Uuid,
    ) -> Result<LiteratureItem> {
        let mut items = self.load_literature_items()?;
        let item = self.find_literature_item_mut(&mut items, item_id)?;
        if !item.files.iter().any(|file| file.file_id == file_id) {
            return Err(CoreError::LiteratureFileNotFound(file_id.to_string()));
        }
        item.default_file_id = Some(file_id);
        let result = item.clone();
        self.save_literature_items(&items)?;
        Ok(result)
    }

    /// Resolves an association to one verified local PDF path. Vault entries are revalidated on
    /// every call so edited JSON and symlink escapes cannot turn into arbitrary open targets.
    pub fn resolve_literature_file_path(&self, item_id: Uuid, file_id: Uuid) -> Result<PathBuf> {
        let items = self.load_literature_items()?;
        let item = items
            .iter()
            .find(|item| item.item_id == item_id)
            .ok_or_else(|| CoreError::LiteratureItemNotFound(item_id.to_string()))?;
        let file = item
            .files
            .iter()
            .find(|file| file.file_id == file_id)
            .ok_or_else(|| CoreError::LiteratureFileNotFound(file_id.to_string()))?;
        match file.kind {
            LiteratureFileKind::Vault => self.resolve_vault_file_path(item_id, &file.path),
            LiteratureFileKind::External => resolve_external_file_path(&file.path),
        }
    }

    fn resolve_vault_file_path(&self, item_id: Uuid, stored_path: &str) -> Result<PathBuf> {
        let relative_path = validate_vault_item_relative_path(item_id, stored_path)?;
        validate_pdf_path(&relative_path)?;
        let root = fs::canonicalize(self.root_path())?;
        let candidate = self.root_path().join(&relative_path);
        let canonical_candidate = fs::canonicalize(&candidate)?;
        if !canonical_candidate.starts_with(&root) {
            return Err(CoreError::InvalidLiteratureFilePath(
                stored_path.to_string(),
            ));
        }
        if !canonical_candidate.is_file() {
            return Err(CoreError::InvalidLiteratureFilePath(
                stored_path.to_string(),
            ));
        }
        validate_pdf_path(&canonical_candidate)?;
        Ok(canonical_candidate)
    }

    fn next_vault_file_relative_path(&self, item_id: Uuid, file_name: &str) -> Result<PathBuf> {
        let directory = Path::new(VAULT_FILES_RELATIVE_DIR).join(item_id.to_string());
        let stem = Path::new(file_name)
            .file_stem()
            .and_then(OsStr::to_str)
            .filter(|stem| !stem.is_empty())
            .ok_or_else(|| CoreError::InvalidLiteratureFilePath(file_name.to_string()))?;
        let extension = Path::new(file_name)
            .extension()
            .and_then(OsStr::to_str)
            .ok_or_else(|| CoreError::UnsupportedLiteratureFile(file_name.to_string()))?;
        for suffix in 0_u32.. {
            let candidate_name = if suffix == 0 {
                file_name.to_string()
            } else {
                format!("{stem} ({suffix}).{extension}")
            };
            let relative = directory.join(candidate_name);
            validate_vault_relative_path(&portable_relative_path(&relative)?)?;
            if !self.root_path().join(&relative).exists() {
                return Ok(relative);
            }
        }
        unreachable!("unbounded suffix iterator always returns")
    }

    fn prepare_document_asset_destination(
        &self,
        item_id: Uuid,
        relative_path: &Path,
    ) -> Result<PathBuf> {
        let relative_text = portable_relative_path(relative_path)?;
        let validated_relative = validate_vault_item_relative_path(item_id, &relative_text)
            .map_err(|_| CoreError::InvalidDocumentAssetPath(relative_text.clone()))?;
        let destination = self.root_path().join(&validated_relative);
        if destination.exists() {
            return Err(CoreError::InvalidDocumentAssetPath(relative_text));
        }
        let parent = destination
            .parent()
            .ok_or_else(|| CoreError::InvalidDocumentAssetPath(relative_text.clone()))?;
        fs::create_dir_all(parent)?;
        let canonical_root = fs::canonicalize(self.root_path())?;
        let canonical_parent = fs::canonicalize(parent)?;
        if !canonical_parent.starts_with(&canonical_root) {
            return Err(CoreError::InvalidDocumentAssetPath(relative_text));
        }
        Ok(destination)
    }

    fn prepare_vault_copy_destination(&self, relative_path: &Path) -> Result<PathBuf> {
        let relative_text = portable_relative_path(relative_path)?;
        let validated_relative = validate_vault_relative_path(&relative_text)?;
        let destination = self.root_path().join(&validated_relative);
        let parent = destination
            .parent()
            .ok_or_else(|| CoreError::InvalidLiteratureFilePath(relative_text.clone()))?;
        fs::create_dir_all(parent)?;
        let canonical_root = fs::canonicalize(self.root_path())?;
        let canonical_parent = fs::canonicalize(parent)?;
        if !canonical_parent.starts_with(&canonical_root) {
            return Err(CoreError::InvalidLiteratureFilePath(relative_text));
        }
        Ok(destination)
    }

    fn find_literature_item_mut<'a>(
        &self,
        items: &'a mut [LiteratureItem],
        item_id: Uuid,
    ) -> Result<&'a mut LiteratureItem> {
        items
            .iter_mut()
            .find(|item| item.item_id == item_id)
            .ok_or_else(|| CoreError::LiteratureItemNotFound(item_id.to_string()))
    }

    fn mutate_literature_item<F>(&self, item_id: Uuid, mutate: F) -> Result<LiteratureItem>
    where
        F: FnOnce(&mut LiteratureItem),
    {
        let mut items = self.load_literature_items()?;
        let item = self.find_literature_item_mut(&mut items, item_id)?;
        mutate(item);
        let result = item.clone();
        self.save_literature_items(&items)?;
        self.try_apply_search_index_change(SearchIndexChange::MetadataChanged { item_id });
        Ok(result)
    }

    /// Derived search publication is deliberately best-effort. Authority has
    /// already been atomically persisted when this runs, so an unavailable or
    /// failed index must never roll back literature/asset state. Full
    /// reconciliation later folds the same IDs back into a generation.
    pub(crate) fn try_apply_search_index_change(&self, change: SearchIndexChange) {
        let _ = self.update_search_index_incrementally(&[change]);
    }
}

fn validate_vault_relative_path(path: &str) -> Result<PathBuf> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(CoreError::InvalidLiteratureFilePath(path.to_string()));
    }
    let candidate = Path::new(trimmed);
    let valid = !candidate.is_absolute()
        && candidate
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    if !valid {
        return Err(CoreError::InvalidLiteratureFilePath(path.to_string()));
    }
    Ok(candidate.to_path_buf())
}

fn validate_vault_item_relative_path(item_id: Uuid, path: &str) -> Result<PathBuf> {
    let relative = validate_vault_relative_path(path)?;
    let components = relative.components().collect::<Vec<_>>();
    let expected_item_id = item_id.to_string();
    let has_expected_shape = components.len() >= 3
        && matches!(components.first(), Some(Component::Normal(part)) if *part == OsStr::new(VAULT_FILES_RELATIVE_DIR))
        && matches!(components.get(1), Some(Component::Normal(part)) if *part == OsStr::new(expected_item_id.as_str()));
    if !has_expected_shape {
        return Err(CoreError::InvalidLiteratureFilePath(path.to_string()));
    }
    Ok(relative)
}

fn reading_session_asset_status(
    vault: &Vault,
    assets: &[DocumentAsset],
    session: &ReadingSession,
) -> DocumentAssetStatus {
    assets
        .iter()
        .find(|asset| asset.item_id == session.item_id && asset.asset_id == session.asset_id)
        .map(|asset| asset.status(vault))
        .unwrap_or(DocumentAssetStatus::Invalid)
}

fn reading_session_recency(left: &ReadingSession, right: &ReadingSession) -> std::cmp::Ordering {
    left.last_opened_at
        .cmp(&right.last_opened_at)
        .then_with(|| left.started_at.cmp(&right.started_at))
        .then_with(|| left.session_id.cmp(&right.session_id))
}

fn most_recent_reading_session_index<F>(sessions: &[ReadingSession], predicate: F) -> Option<usize>
where
    F: Fn(&ReadingSession) -> bool,
{
    sessions
        .iter()
        .enumerate()
        .filter(|(_, session)| predicate(session))
        .max_by(|(_, left), (_, right)| reading_session_recency(left, right))
        .map(|(index, _)| index)
}

fn portable_relative_path(path: &Path) -> Result<String> {
    let components = path
        .components()
        .map(|component| match component {
            Component::Normal(part) => Ok(part.to_string_lossy().to_string()),
            _ => Err(CoreError::InvalidLiteratureFilePath(
                path.display().to_string(),
            )),
        })
        .collect::<Result<Vec<_>>>()?;
    if components.is_empty() || components.iter().any(|part| part.is_empty()) {
        return Err(CoreError::InvalidLiteratureFilePath(
            path.display().to_string(),
        ));
    }
    Ok(components.join("/"))
}

fn portable_pdf_file_name(file_name: &OsStr) -> Result<String> {
    let raw = file_name.to_string_lossy();
    let portable = raw
        .chars()
        .map(|character| {
            if character.is_control()
                || matches!(
                    character,
                    '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
                )
            {
                '_'
            } else {
                character
            }
        })
        .collect::<String>()
        .trim_matches(|character| character == '.' || character == ' ')
        .to_string();
    if portable.is_empty() || !is_pdf_path(Path::new(&portable)) {
        return Err(CoreError::UnsupportedLiteratureFile(raw.to_string()));
    }
    Ok(portable)
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Copies through a same-directory temporary file, then publishes with a hard
/// link. `hard_link` refuses to replace a target, which preserves the
/// no-overwrite contract even if another process races this import.
fn copy_to_new_vault_file(source: &Path, destination: &Path) -> Result<()> {
    let file_name = destination
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| CoreError::InvalidDocumentAssetPath(destination.display().to_string()))?;
    let temp = destination.with_file_name(format!(".{file_name}.tmp"));
    let copy_result = (|| -> std::io::Result<()> {
        let mut input = fs::File::open(source)?;
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        std::io::copy(&mut input, &mut output)?;
        output.sync_all()?;
        Ok(())
    })();
    if let Err(error) = copy_result {
        let _ = fs::remove_file(&temp);
        return Err(error.into());
    }

    if let Err(error) = fs::hard_link(&temp, destination) {
        let _ = fs::remove_file(&temp);
        return Err(error.into());
    }
    if let Err(error) = fs::remove_file(&temp) {
        let _ = fs::remove_file(destination);
        return Err(error.into());
    }
    Ok(())
}

fn validate_existing_pdf(path: &Path) -> Result<PathBuf> {
    let canonical = fs::canonicalize(path)?;
    if !canonical.is_file() {
        return Err(CoreError::InvalidLiteratureFilePath(
            path.display().to_string(),
        ));
    }
    validate_pdf_path(&canonical)?;
    Ok(canonical)
}

fn resolve_external_file_path(stored_path: &str) -> Result<PathBuf> {
    let path = Path::new(stored_path);
    if stored_path.trim().is_empty() || !path.is_absolute() {
        return Err(CoreError::InvalidLiteratureFilePath(
            stored_path.to_string(),
        ));
    }
    validate_existing_pdf(path)
}

fn validate_pdf_path(path: &Path) -> Result<()> {
    if is_pdf_path(path) {
        Ok(())
    } else {
        Err(CoreError::UnsupportedLiteratureFile(
            path.display().to_string(),
        ))
    }
}

fn is_pdf_path(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
}

pub(crate) fn atomic_replace(path: &Path, payload: &[u8]) -> Result<()> {
    atomic_replace_with(path, payload, replace_file)
}

pub(crate) fn atomic_replace_named(path: &Path, payload: &[u8], prefix: &str) -> Result<()> {
    atomic_replace_named_with(path, payload, prefix, replace_file)
}

fn atomic_replace_named_with<F>(path: &Path, payload: &[u8], prefix: &str, replace: F) -> Result<()>
where
    F: FnOnce(&Path, &Path) -> Result<()>,
{
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp = path.with_file_name(format!("{prefix}.{nonce}.tmp"));
    let write_result = (|| -> std::io::Result<()> {
        let mut file = fs::File::create(&temp)?;
        file.write_all(payload)?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temp);
        return Err(error.into());
    }
    let result = replace(&temp, path);
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

pub(crate) fn atomic_replace_with<F>(path: &Path, payload: &[u8], replace: F) -> Result<()>
where
    F: FnOnce(&Path, &Path) -> Result<()>,
{
    atomic_replace_named_with(path, payload, ".literature_items", replace)
}

#[cfg(not(windows))]
fn replace_file(temp: &Path, target: &Path) -> Result<()> {
    fs::rename(temp, target)?;
    Ok(())
}

#[cfg(windows)]
fn replace_file(temp: &Path, target: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let to_wide = |p: &Path| {
        p.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<u16>>()
    };
    let from = to_wide(temp);
    let to = to_wide(target);
    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }
    let ok = unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } != 0;
    if ok {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_vault(name: &str) -> (Vault, PathBuf) {
        let root = std::env::temp_dir().join(format!("{name}-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("sources.parquet");
        fs::write(&source, b"").unwrap();
        let vault =
            Vault::open_sources_file(&source, crate::vault::VaultOpenOptions::default()).unwrap();
        (vault, root)
    }

    fn draft(title: &str) -> LiteratureItemDraft {
        LiteratureItemDraft {
            title: title.to_string(),
            authors: vec!["Ada Lovelace".to_string()],
            published_year: Some(1843),
            item_type: LiteratureItemType::Article,
            favorite: false,
            reading_status: ReadingStatus::Inbox,
            tags: vec!["history".to_string()],
            sources: vec![LiteratureSourceRef {
                source_name: "OpenAlex".to_string(),
                external_id: Some("S1".to_string()),
                original_locator: Some("https://example.test/S1".to_string()),
            }],
            external_identifiers: vec![ExternalIdentifier {
                namespace: "openalex".to_string(),
                value: "S1".to_string(),
            }],
        }
    }

    fn write_pdf(path: &Path, contents: &[u8]) {
        fs::write(path, contents).unwrap();
    }

    #[cfg(unix)]
    fn create_file_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(windows)]
    fn create_file_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_file(target, link)
    }

    #[test]
    fn empty_vault_returns_empty_literature_collection() {
        let (vault, root) = temp_vault("cistella-literature-empty");
        assert!(vault.load_literature_items().unwrap().is_empty());
        assert_eq!(
            vault.literature_items_path(),
            root.join("user/literature_items.json")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn legacy_items_without_file_fields_read_without_migration() {
        let (vault, root) = temp_vault("cistella-literature-legacy");
        let item_id = Uuid::new_v4();
        fs::create_dir_all(root.join("user")).unwrap();
        fs::write(
            vault.literature_items_path(),
            format!(r#"[{{"itemId":"{item_id}","title":"Legacy","authors":[],"publishedYear":null,"itemType":"article","favorite":false,"readingStatus":"inbox","tags":[],"sources":[]}}]"#),
        )
        .unwrap();
        let item = vault.load_literature_items().unwrap().pop().unwrap();
        assert!(item.files.is_empty());
        assert_eq!(item.default_file_id, None);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn literature_items_round_trip_and_user_mutations_persist() {
        let (vault, root) = temp_vault("cistella-literature-roundtrip");
        let created = vault.create_literature_item(draft("First title")).unwrap();
        let reopened = Vault::open_sources_file(
            root.join("sources.parquet"),
            crate::vault::VaultOpenOptions::default(),
        )
        .unwrap();
        assert_eq!(
            reopened.load_literature_items().unwrap(),
            vec![created.clone()]
        );
        let updated = reopened
            .update_literature_item(
                created.item_id,
                LiteratureItemDraft {
                    title: "Edited".into(),
                    ..draft("ignored")
                },
            )
            .unwrap();
        assert_eq!(updated.title, "Edited");
        let updated = reopened
            .set_literature_item_favorite(created.item_id, true)
            .unwrap();
        let updated = reopened
            .set_literature_item_tags(updated.item_id, vec!["important".into()])
            .unwrap();
        let updated = reopened
            .set_literature_item_reading_status(updated.item_id, ReadingStatus::Reading)
            .unwrap();
        assert!(updated.favorite);
        assert_eq!(updated.tags, vec!["important"]);
        assert_eq!(updated.reading_status, ReadingStatus::Reading);
        reopened.delete_literature_item(created.item_id).unwrap();
        assert!(reopened.load_literature_items().unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn vault_pdf_copy_uses_portable_item_directory_and_resolves_after_copying_vault() {
        let (vault, root) = temp_vault("cistella-literature-vault-copy");
        let item = vault.create_literature_item(draft("Portable")).unwrap();
        let original = root.join("incoming.PDF");
        write_pdf(&original, b"first pdf");
        let file = vault
            .add_literature_vault_file(item.item_id, &original)
            .unwrap();
        assert_eq!(file.kind, LiteratureFileKind::Vault);
        assert!(file.path.starts_with(&format!("files/{}/", item.item_id)));
        assert_eq!(
            fs::read(
                vault
                    .resolve_literature_file_path(item.item_id, file.file_id)
                    .unwrap()
            )
            .unwrap(),
            b"first pdf"
        );

        let moved = std::env::temp_dir().join(format!("cistella-moved-{}", Uuid::new_v4()));
        fs::create_dir_all(&moved).unwrap();
        copy_directory(&root, &moved);
        let moved_vault = Vault::open_sources_file(
            moved.join("sources.parquet"),
            crate::vault::VaultOpenOptions::default(),
        )
        .unwrap();
        assert_eq!(
            fs::read(
                moved_vault
                    .resolve_literature_file_path(item.item_id, file.file_id)
                    .unwrap()
            )
            .unwrap(),
            b"first pdf"
        );
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(moved).unwrap();
    }

    #[test]
    fn external_pdf_is_explicit_absolute_nonportable_link() {
        let (vault, root) = temp_vault("cistella-literature-external");
        let item = vault.create_literature_item(draft("External")).unwrap();
        let external =
            std::env::temp_dir().join(format!("cistella-external-{}.pdf", Uuid::new_v4()));
        write_pdf(&external, b"external pdf");
        let file = vault
            .add_literature_external_file(item.item_id, &external)
            .unwrap();
        assert_eq!(file.kind, LiteratureFileKind::External);
        assert!(Path::new(&file.path).is_absolute());
        assert_eq!(
            vault
                .resolve_literature_file_path(item.item_id, file.file_id)
                .unwrap(),
            fs::canonicalize(&external).unwrap()
        );
        fs::remove_dir_all(root).unwrap();
        fs::remove_file(external).unwrap();
    }

    #[test]
    fn first_file_becomes_default_and_removing_it_does_not_guess_a_replacement() {
        let (vault, root) = temp_vault("cistella-literature-default");
        let item = vault.create_literature_item(draft("Defaults")).unwrap();
        let one = root.join("one.pdf");
        let two = root.join("two.pdf");
        write_pdf(&one, b"one");
        write_pdf(&two, b"two");
        let first = vault.add_literature_vault_file(item.item_id, &one).unwrap();
        let second = vault.add_literature_vault_file(item.item_id, &two).unwrap();
        assert_eq!(
            vault.load_literature_items().unwrap()[0].default_file_id,
            Some(first.file_id)
        );
        vault
            .set_literature_default_file(item.item_id, second.file_id)
            .unwrap();
        assert_eq!(
            vault.load_literature_items().unwrap()[0].default_file_id,
            Some(second.file_id)
        );
        let result = vault
            .remove_literature_file(item.item_id, second.file_id)
            .unwrap();
        assert_eq!(result.default_file_id, None);
        assert_eq!(result.files, vec![first]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn malformed_vault_paths_and_non_pdf_files_are_rejected_without_mutating_items() {
        let (vault, root) = temp_vault("cistella-literature-safety");
        let item = vault.create_literature_item(draft("Safety")).unwrap();
        let before = vault.load_literature_items().unwrap();
        let text = root.join("not-pdf.txt");
        fs::write(&text, "not a pdf").unwrap();
        assert!(
            vault
                .add_literature_vault_file(item.item_id, &text)
                .is_err()
        );
        assert_eq!(vault.load_literature_items().unwrap(), before);

        let mut malicious = before[0].clone();
        malicious.files.push(LiteratureFileRef {
            file_id: Uuid::new_v4(),
            kind: LiteratureFileKind::Vault,
            path: "../escaped.pdf".to_string(),
            display_name: "escaped.pdf".to_string(),
        });
        let malicious_file_id = malicious.files[0].file_id;
        vault.save_literature_items(&[malicious]).unwrap();
        assert!(
            vault
                .resolve_literature_file_path(item.item_id, malicious_file_id)
                .is_err()
        );

        let mut absolute = vault.load_literature_items().unwrap()[0].clone();
        absolute.files[0].path = root.join("absolute.pdf").to_string_lossy().to_string();
        vault.save_literature_items(&[absolute]).unwrap();
        assert!(
            vault
                .resolve_literature_file_path(item.item_id, malicious_file_id)
                .is_err()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn vault_file_symlink_escaping_vault_is_rejected() {
        let (vault, root) = temp_vault("cistella-literature-symlink");
        let item = vault.create_literature_item(draft("Symlink")).unwrap();
        let outside = std::env::temp_dir().join(format!("cistella-outside-{}.pdf", Uuid::new_v4()));
        write_pdf(&outside, b"outside vault");

        let link = root
            .join(VAULT_FILES_RELATIVE_DIR)
            .join(item.item_id.to_string())
            .join("escaped.pdf");
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        create_file_symlink(&outside, &link).expect("failed to create test symlink");

        let file_id = Uuid::new_v4();
        let mut stored = vault.load_literature_items().unwrap();
        stored[0].files.push(LiteratureFileRef {
            file_id,
            kind: LiteratureFileKind::Vault,
            path: format!("files/{}/escaped.pdf", item.item_id),
            display_name: "escaped.pdf".to_string(),
        });
        vault.save_literature_items(&stored).unwrap();

        let error = vault
            .resolve_literature_file_path(item.item_id, file_id)
            .unwrap_err();
        assert!(matches!(error, CoreError::InvalidLiteratureFilePath(_)));

        fs::remove_dir_all(root).unwrap();
        fs::remove_file(outside).unwrap();
    }

    #[test]
    fn missing_file_and_failed_file_lookup_preserve_literature_data() {
        let (vault, root) = temp_vault("cistella-literature-missing");
        let item = vault.create_literature_item(draft("Missing")).unwrap();
        let source = root.join("missing.pdf");
        write_pdf(&source, b"missing");
        let file = vault
            .add_literature_vault_file(item.item_id, &source)
            .unwrap();
        fs::remove_file(root.join(&file.path)).unwrap();
        let before = vault.load_literature_items().unwrap();
        assert!(
            vault
                .resolve_literature_file_path(item.item_id, file.file_id)
                .is_err()
        );
        assert_eq!(vault.load_literature_items().unwrap(), before);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn atomic_replace_cleans_temp_after_replace_phase_failure() {
        let root = std::env::temp_dir().join(format!(
            "cistella-literature-atomic-cleanup-{}",
            Uuid::new_v4()
        ));
        fs::create_dir_all(&root).unwrap();
        let user_path = root.join("literature_items.json");
        let original = br#"[{"itemId":"original"}]"#;
        fs::write(&user_path, original).unwrap();

        let mut observed_temp = None;
        let error = atomic_replace_with(&user_path, b"new payload", |temp, target| {
            assert!(
                temp.is_file(),
                "replacement phase must receive a written temp file"
            );
            assert_eq!(fs::read(temp).unwrap(), b"new payload");
            assert_eq!(target, user_path.as_path());
            observed_temp = Some(temp.to_path_buf());
            Err(CoreError::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "injected replace failure",
            )))
        })
        .unwrap_err();

        assert!(matches!(error, CoreError::Io(_)));
        let temp = observed_temp.expect("replace phase should have observed the temp file");
        assert!(
            !temp.exists(),
            "failed replacement must remove the written temp file"
        );
        assert_eq!(fs::read(&user_path).unwrap(), original);
        let remaining_temps = fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.starts_with(".literature_items.") && name.ends_with(".tmp")
                    })
            })
            .collect::<Vec<_>>();
        assert!(
            remaining_temps.is_empty(),
            "temporary files remain: {remaining_temps:?}"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn atomic_replace_cleans_temp_when_actual_replace_fails_after_write() {
        let root = std::env::temp_dir().join(format!(
            "cistella-literature-atomic-real-failure-{}",
            Uuid::new_v4()
        ));
        fs::create_dir_all(&root).unwrap();
        let target = root.join("literature_items.json");
        fs::create_dir(&target).unwrap();

        let mut observed_temp = None;
        let error = atomic_replace_with(&target, b"new payload", |temp, destination| {
            assert!(
                temp.is_file(),
                "replacement phase must receive a written temp file"
            );
            assert_eq!(fs::read(temp).unwrap(), b"new payload");
            assert_eq!(destination, target.as_path());
            observed_temp = Some(temp.to_path_buf());
            replace_file(temp, destination)
        })
        .unwrap_err();

        assert!(matches!(error, CoreError::Io(_)));
        assert!(
            target.is_dir(),
            "failed replacement must leave the target untouched"
        );
        let temp = observed_temp.expect("replace phase should have observed the temp file");
        assert!(
            !temp.exists(),
            "failed replacement must remove the written temp file"
        );
        let remaining_temps = fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.starts_with(".literature_items.") && name.ends_with(".tmp")
                    })
            })
            .collect::<Vec<_>>();
        assert!(
            remaining_temps.is_empty(),
            "temporary files remain: {remaining_temps:?}"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn atomic_replace_failure_keeps_existing_user_file() {
        let (vault, root) = temp_vault("cistella-literature-atomic");
        let item = vault.create_literature_item(draft("Original")).unwrap();
        let user_path = vault.literature_items_path();
        let original = fs::read(&user_path).unwrap();
        let blocked_parent = root.join("blocked");
        fs::write(&blocked_parent, b"not a directory").unwrap();
        assert!(atomic_replace(&blocked_parent.join("literature_items.json"), b"new").is_err());
        assert_eq!(fs::read(user_path).unwrap(), original);
        assert_eq!(vault.load_literature_items().unwrap(), vec![item]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn deleting_item_removes_its_asset_records_but_preserves_physical_files() {
        let (vault, root) = temp_vault("cistella-assets-delete-item");
        let deleted_item = vault
            .create_literature_item(draft("Delete this item"))
            .unwrap();
        let retained_item = vault
            .create_literature_item(draft("Keep this item"))
            .unwrap();

        let deleted_path = root
            .join(VAULT_FILES_RELATIVE_DIR)
            .join(deleted_item.item_id.to_string())
            .join("retained-on-disk.pdf");
        let retained_path = root
            .join(VAULT_FILES_RELATIVE_DIR)
            .join(retained_item.item_id.to_string())
            .join("still-managed.pdf");
        fs::create_dir_all(deleted_path.parent().unwrap()).unwrap();
        fs::create_dir_all(retained_path.parent().unwrap()).unwrap();
        write_pdf(&deleted_path, b"deleted item physical file");
        write_pdf(&retained_path, b"retained item physical file");
        let deleted_asset = document_asset(
            deleted_item.item_id,
            Uuid::new_v4(),
            DocumentAssetKind::Primary,
            DocumentAssetStorageKind::Vault,
            format!("files/{}/retained-on-disk.pdf", deleted_item.item_id),
        );
        let retained_asset = document_asset(
            retained_item.item_id,
            Uuid::new_v4(),
            DocumentAssetKind::Supplement,
            DocumentAssetStorageKind::Vault,
            format!("files/{}/still-managed.pdf", retained_item.item_id),
        );
        vault
            .save_document_assets(&[deleted_asset.clone(), retained_asset.clone()])
            .unwrap();

        vault.delete_literature_item(deleted_item.item_id).unwrap();

        assert_eq!(vault.load_literature_items().unwrap(), vec![retained_item]);
        assert_eq!(vault.load_document_assets().unwrap(), vec![retained_asset]);
        assert!(
            deleted_path.is_file(),
            "deleting a literature item must not delete its Vault file"
        );
        assert!(retained_path.is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn deleting_item_aborts_before_item_write_when_asset_store_write_fails() {
        let (vault, root) = temp_vault("cistella-assets-delete-item-asset-failure");
        let item = vault
            .create_literature_item(draft("Keep after failure"))
            .unwrap();
        let asset = document_asset(
            item.item_id,
            Uuid::new_v4(),
            DocumentAssetKind::Primary,
            DocumentAssetStorageKind::External,
            "C:/papers/keep-after-failure.pdf".to_string(),
        );
        vault
            .save_document_assets(std::slice::from_ref(&asset))
            .unwrap();
        let items_before = vault.load_literature_items().unwrap();
        let assets_before = vault.load_document_assets().unwrap();

        let error = vault
            .delete_literature_item_with(
                item.item_id,
                |_vault, _assets| {
                    Err(CoreError::Io(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "injected asset-store write failure",
                    )))
                },
                |_vault, _items| -> Result<()> {
                    panic!("item store must not be written after asset-store failure")
                },
                |_vault, _assets| -> Result<()> {
                    panic!("rollback must not run before an asset-store publish")
                },
            )
            .unwrap_err();

        assert!(matches!(error, CoreError::Io(_)));
        assert_eq!(vault.load_literature_items().unwrap(), items_before);
        assert_eq!(vault.load_document_assets().unwrap(), assets_before);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn deleting_item_restores_assets_when_item_store_write_fails() {
        let (vault, root) = temp_vault("cistella-assets-delete-item-item-failure");
        let item = vault
            .create_literature_item(draft("Restore after failure"))
            .unwrap();
        let asset = document_asset(
            item.item_id,
            Uuid::new_v4(),
            DocumentAssetKind::Primary,
            DocumentAssetStorageKind::External,
            "C:/papers/restore-after-failure.pdf".to_string(),
        );
        vault
            .save_document_assets(std::slice::from_ref(&asset))
            .unwrap();
        let items_before = vault.load_literature_items().unwrap();
        let assets_before = vault.load_document_assets().unwrap();

        let error = vault
            .delete_literature_item_with(
                item.item_id,
                |vault, assets| vault.save_document_assets(assets),
                |_vault, _items| {
                    Err(CoreError::Io(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "injected item-store write failure",
                    )))
                },
                |vault, assets| vault.save_document_assets(assets),
            )
            .unwrap_err();

        assert!(matches!(error, CoreError::Io(_)));
        assert_eq!(vault.load_literature_items().unwrap(), items_before);
        assert_eq!(vault.load_document_assets().unwrap(), assets_before);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn deleting_item_never_orphans_assets_when_compensation_also_fails() {
        let (vault, root) = temp_vault("cistella-assets-delete-item-rollback-failure");
        let item = vault
            .create_literature_item(draft("Safe after rollback failure"))
            .unwrap();
        let asset = document_asset(
            item.item_id,
            Uuid::new_v4(),
            DocumentAssetKind::Primary,
            DocumentAssetStorageKind::External,
            "C:/papers/safe-after-rollback-failure.pdf".to_string(),
        );
        vault
            .save_document_assets(std::slice::from_ref(&asset))
            .unwrap();

        let error = vault
            .delete_literature_item_with(
                item.item_id,
                |vault, assets| vault.save_document_assets(assets),
                |_vault, _items| {
                    Err(CoreError::Io(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "injected item-store write failure",
                    )))
                },
                |_vault, _assets| {
                    Err(CoreError::Io(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "injected asset rollback failure",
                    )))
                },
            )
            .unwrap_err();

        assert!(matches!(
            error,
            CoreError::LiteratureItemDeleteRollbackFailed { .. }
        ));
        assert_eq!(vault.load_literature_items().unwrap(), vec![item]);
        assert!(
            vault.load_document_assets().unwrap().is_empty(),
            "the safe fallback retains the item rather than leaving a deleted item with assets"
        );
        fs::remove_dir_all(root).unwrap();
    }

    fn document_asset(
        item_id: Uuid,
        asset_id: Uuid,
        kind: DocumentAssetKind,
        storage_kind: DocumentAssetStorageKind,
        path: String,
    ) -> DocumentAsset {
        DocumentAsset {
            asset_id,
            item_id,
            asset_kind: kind,
            storage_kind,
            path,
            display_name: "paper.pdf".to_string(),
            media_type: "application/pdf".to_string(),
            file_size: Some(12),
            content_hash: Some("abc123".to_string()),
            imported_at: Some(Utc::now()),
            is_default: false,
        }
    }

    fn reading_session(
        item_id: Uuid,
        asset_id: Uuid,
        state: ReadingSessionState,
    ) -> ReadingSession {
        let started_at = DateTime::parse_from_rfc3339("2026-08-26T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        ReadingSession {
            session_id: Uuid::new_v4(),
            item_id,
            asset_id,
            started_at,
            last_opened_at: started_at,
            ended_at: None,
            state,
        }
    }

    #[test]
    fn reading_sessions_round_trip_atomically_and_keep_missing_asset_history() {
        let (vault, root) = temp_vault("cistella-reading-sessions-roundtrip");
        let item = vault
            .create_literature_item(draft("Reading history"))
            .unwrap();
        let asset = document_asset(
            item.item_id,
            Uuid::new_v4(),
            DocumentAssetKind::Primary,
            DocumentAssetStorageKind::Vault,
            format!("files/{}/missing.pdf", item.item_id),
        );
        vault
            .save_document_assets(std::slice::from_ref(&asset))
            .unwrap();
        let session = reading_session(item.item_id, asset.asset_id, ReadingSessionState::Paused);

        assert!(vault.load_reading_sessions().unwrap().is_empty());
        vault
            .save_reading_sessions(std::slice::from_ref(&session))
            .unwrap();
        assert_eq!(
            vault.load_reading_sessions().unwrap(),
            vec![session.clone()]
        );
        assert_eq!(asset.status(&vault), DocumentAssetStatus::Missing);

        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(vault.reading_sessions_path()).unwrap()).unwrap();
        assert_eq!(value[0]["state"], "paused");
        assert_eq!(value[0]["itemId"], item.item_id.to_string());
        assert_eq!(value[0]["assetId"], asset.asset_id.to_string());
        let temps = fs::read_dir(root.join("user"))
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.starts_with(".reading_sessions.") && name.ends_with(".tmp"))
            .collect::<Vec<_>>();
        assert!(temps.is_empty(), "temporary files remain: {temps:?}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reading_sessions_reject_invalid_item_asset_association_without_mutation() {
        let (vault, root) = temp_vault("cistella-reading-sessions-association");
        let first = vault.create_literature_item(draft("First")).unwrap();
        let second = vault.create_literature_item(draft("Second")).unwrap();
        let first_asset = document_asset(
            first.item_id,
            Uuid::new_v4(),
            DocumentAssetKind::Primary,
            DocumentAssetStorageKind::Vault,
            format!("files/{}/first.pdf", first.item_id),
        );
        let second_asset = document_asset(
            second.item_id,
            Uuid::new_v4(),
            DocumentAssetKind::Primary,
            DocumentAssetStorageKind::Vault,
            format!("files/{}/second.pdf", second.item_id),
        );
        vault
            .save_document_assets(&[first_asset.clone(), second_asset.clone()])
            .unwrap();
        let original = reading_session(
            first.item_id,
            first_asset.asset_id,
            ReadingSessionState::Active,
        );
        vault
            .save_reading_sessions(std::slice::from_ref(&original))
            .unwrap();
        let before = fs::read(vault.reading_sessions_path()).unwrap();

        let invalid = reading_session(
            first.item_id,
            second_asset.asset_id,
            ReadingSessionState::Active,
        );
        let error = vault
            .save_reading_sessions(&[original, invalid])
            .unwrap_err();
        assert!(matches!(
            error,
            CoreError::InvalidReadingSessionAssociation { .. }
        ));
        assert_eq!(fs::read(vault.reading_sessions_path()).unwrap(), before);
        assert!(matches!(
            vault.validate_reading_session_association(Uuid::new_v4(), first_asset.asset_id),
            Err(CoreError::LiteratureItemNotFound(_))
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reading_session_write_failure_preserves_existing_json_and_cleans_temp() {
        let (vault, root) = temp_vault("cistella-reading-sessions-write-failure");
        let item = vault
            .create_literature_item(draft("Write protection"))
            .unwrap();
        let asset = document_asset(
            item.item_id,
            Uuid::new_v4(),
            DocumentAssetKind::Primary,
            DocumentAssetStorageKind::Vault,
            format!("files/{}/paper.pdf", item.item_id),
        );
        vault
            .save_document_assets(std::slice::from_ref(&asset))
            .unwrap();
        let original = reading_session(item.item_id, asset.asset_id, ReadingSessionState::Active);
        vault
            .save_reading_sessions(std::slice::from_ref(&original))
            .unwrap();
        let before = fs::read(vault.reading_sessions_path()).unwrap();
        let replacement = ReadingSession {
            state: ReadingSessionState::Closed,
            ended_at: Some(Utc::now()),
            ..original
        };
        let mut observed_temp = None;

        let error = vault
            .save_reading_sessions_with(std::slice::from_ref(&replacement), |path, payload| {
                atomic_replace_named_with(path, payload, ".reading_sessions", |temp, _target| {
                    observed_temp = Some(temp.to_path_buf());
                    Err(CoreError::Io(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "injected reading-session replacement failure",
                    )))
                })
            })
            .unwrap_err();
        assert!(matches!(error, CoreError::Io(_)));
        assert_eq!(fs::read(vault.reading_sessions_path()).unwrap(), before);
        assert!(
            !observed_temp
                .expect("replacement phase must observe temporary JSON")
                .exists(),
            "failed replacement must clean the temporary JSON"
        );
        let temps = fs::read_dir(root.join("user"))
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.starts_with(".reading_sessions.") && name.ends_with(".tmp"))
            .collect::<Vec<_>>();
        assert!(temps.is_empty(), "temporary files remain: {temps:?}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn document_asset_json_uses_stable_kind_and_storage_names() {
        let item_id = Uuid::new_v4();
        let asset = document_asset(
            item_id,
            Uuid::new_v4(),
            DocumentAssetKind::Supplement,
            DocumentAssetStorageKind::External,
            "C:/papers/paper.pdf".to_string(),
        );
        let value = serde_json::to_value(&asset).unwrap();
        assert_eq!(value["assetKind"], "supplement");
        assert_eq!(value["storageKind"], "external");
        assert_eq!(value["mediaType"], "application/pdf");
        assert_eq!(value["fileSize"], 12);
        assert_eq!(value["contentHash"], "abc123");
        assert!(!value["isDefault"].as_bool().unwrap());
        assert_eq!(
            serde_json::from_value::<DocumentAsset>(value).unwrap(),
            asset
        );
    }

    #[test]
    fn legacy_files_and_default_are_projected_to_stable_assets() {
        let (vault, root) = temp_vault("cistella-assets-legacy-projection");
        let item_id = Uuid::new_v4();
        let vault_file_id = Uuid::new_v4();
        let external_file_id = Uuid::new_v4();
        let item = LiteratureItem {
            item_id,
            title: "Legacy item".to_string(),
            authors: vec!["Author".to_string()],
            published_year: None,
            item_type: LiteratureItemType::Article,
            favorite: false,
            reading_status: ReadingStatus::Inbox,
            tags: Vec::new(),
            sources: Vec::new(),
            files: vec![
                LiteratureFileRef {
                    file_id: vault_file_id,
                    kind: LiteratureFileKind::Vault,
                    path: format!("files/{item_id}/paper.pdf"),
                    display_name: "paper.pdf".to_string(),
                },
                LiteratureFileRef {
                    file_id: external_file_id,
                    kind: LiteratureFileKind::External,
                    path: "C:/external/paper.pdf".to_string(),
                    display_name: "paper.pdf".to_string(),
                },
            ],
            default_file_id: Some(external_file_id),
            external_identifiers: Vec::new(),
        };
        vault.save_literature_items(&[item]).unwrap();

        let assets = vault.load_document_assets().unwrap();
        assert_eq!(assets.len(), 2);
        assert_eq!(assets[0].asset_id, vault_file_id);
        assert_eq!(assets[0].item_id, item_id);
        assert_eq!(assets[0].asset_kind, DocumentAssetKind::Primary);
        assert_eq!(assets[0].storage_kind, DocumentAssetStorageKind::Vault);
        assert_eq!(assets[0].media_type, "application/pdf");
        assert_eq!(assets[0].file_size, None);
        assert!(!assets[0].is_default);
        assert_eq!(assets[1].asset_id, external_file_id);
        assert_eq!(assets[1].storage_kind, DocumentAssetStorageKind::External);
        assert!(assets[1].is_default);
        assert_eq!(assets[1].content_hash, None);
        assert_eq!(assets[1].imported_at, None);

        // Compatibility projection is read-only: the legacy file remains the
        // source of data until the new store is explicitly published.
        assert!(!vault.document_assets_path().exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn document_assets_file_is_authoritative_when_present() {
        let (vault, root) = temp_vault("cistella-assets-authority");
        let item = vault.create_literature_item(draft("Authority")).unwrap();
        let legacy_id = Uuid::new_v4();
        let mut legacy = vault.load_literature_items().unwrap().pop().unwrap();
        legacy.files.push(LiteratureFileRef {
            file_id: legacy_id,
            kind: LiteratureFileKind::Vault,
            path: format!("files/{}/legacy.pdf", item.item_id),
            display_name: "legacy.pdf".to_string(),
        });
        legacy.default_file_id = Some(legacy_id);
        vault.save_literature_items(&[legacy]).unwrap();

        let new_id = Uuid::new_v4();
        let new_asset = document_asset(
            item.item_id,
            new_id,
            DocumentAssetKind::Version,
            DocumentAssetStorageKind::Vault,
            format!("files/{}/new.pdf", item.item_id),
        );
        vault
            .save_document_assets(std::slice::from_ref(&new_asset))
            .unwrap();
        assert_eq!(vault.load_document_assets().unwrap(), vec![new_asset]);

        // An explicitly empty new store still wins over legacy files.
        vault.save_document_assets(&[]).unwrap();
        assert!(vault.load_document_assets().unwrap().is_empty());
        assert!(vault.literature_items_path().exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn document_assets_round_trip_uses_atomic_user_file() {
        let (vault, root) = temp_vault("cistella-assets-roundtrip");
        let item_id = Uuid::new_v4();
        let assets = vec![
            document_asset(
                item_id,
                Uuid::new_v4(),
                DocumentAssetKind::Primary,
                DocumentAssetStorageKind::Vault,
                format!("files/{item_id}/paper.pdf"),
            ),
            document_asset(
                item_id,
                Uuid::new_v4(),
                DocumentAssetKind::Appendix,
                DocumentAssetStorageKind::External,
                "C:/papers/appendix.pdf".to_string(),
            ),
        ];
        vault.save_document_assets(&assets).unwrap();
        assert_eq!(vault.load_document_assets().unwrap(), assets);
        let bytes = fs::read(vault.document_assets_path()).unwrap();
        assert!(bytes.starts_with(b"["));
        let temp_files = fs::read_dir(root.join("user"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter_map(|name| name.into_string().ok())
            .filter(|name| name.starts_with(".document_assets.") && name.ends_with(".tmp"))
            .collect::<Vec<_>>();
        assert!(
            temp_files.is_empty(),
            "temporary files remain: {temp_files:?}"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn document_asset_status_recomputes_from_current_filesystem() {
        let (vault, root) = temp_vault("cistella-assets-status");
        let item_id = Uuid::new_v4();
        let item_dir = root
            .join(VAULT_FILES_RELATIVE_DIR)
            .join(item_id.to_string());
        fs::create_dir_all(&item_dir).unwrap();
        let paper = item_dir.join("paper.pdf");
        write_pdf(&paper, b"pdf");

        let available = document_asset(
            item_id,
            Uuid::new_v4(),
            DocumentAssetKind::Primary,
            DocumentAssetStorageKind::Vault,
            format!("files/{item_id}/paper.pdf"),
        );
        assert_eq!(available.status(&vault), DocumentAssetStatus::Available);

        let missing = DocumentAsset {
            path: format!("files/{item_id}/missing.pdf"),
            ..available.clone()
        };
        assert_eq!(missing.status(&vault), DocumentAssetStatus::Missing);

        let invalid = DocumentAsset {
            media_type: "text/plain".to_string(),
            ..available.clone()
        };
        assert_eq!(invalid.status(&vault), DocumentAssetStatus::Invalid);

        let external_path = root.join("external.pdf");
        write_pdf(&external_path, b"external");
        let external = DocumentAsset {
            storage_kind: DocumentAssetStorageKind::External,
            path: fs::canonicalize(&external_path)
                .unwrap()
                .to_string_lossy()
                .to_string(),
            ..available.clone()
        };
        assert_eq!(external.status(&vault), DocumentAssetStatus::Available);
        fs::remove_file(&external_path).unwrap();
        assert_eq!(
            external.status(&vault),
            DocumentAssetStatus::ExternalUnavailable
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn document_assets_atomic_replace_failure_cleans_named_temp_file() {
        let root =
            std::env::temp_dir().join(format!("cistella-assets-atomic-failure-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let target = root.join("document_assets.json");
        fs::create_dir(&target).unwrap();
        let mut observed_temp = None;
        let error = atomic_replace_named_with(
            &target,
            b"new payload",
            ".document_assets",
            |temp, destination| {
                assert!(temp.is_file());
                observed_temp = Some(temp.to_path_buf());
                replace_file(temp, destination)
            },
        )
        .unwrap_err();
        assert!(matches!(error, CoreError::Io(_)));
        assert!(target.is_dir());
        assert!(!observed_temp.unwrap().exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn document_asset_import_hashes_to_item_scoped_portable_asset_and_deduplicates() {
        let (vault, root) = temp_vault("cistella-assets-import");
        let item = vault
            .create_literature_item(draft("Imported paper"))
            .unwrap();
        let source = root.join("incoming paper.pdf");
        write_pdf(&source, b"same PDF bytes");

        let first = vault
            .import_document_asset(item.item_id, &source, DocumentAssetKind::Primary)
            .unwrap();
        let asset = match first {
            DocumentAssetImportResult::Imported { asset } => asset,
            other => panic!("expected import, got {other:?}"),
        };
        assert_eq!(asset.item_id, item.item_id);
        assert_eq!(asset.media_type, "application/pdf");
        assert_eq!(asset.file_size, Some(b"same PDF bytes".len() as u64));
        assert_eq!(
            asset.content_hash.as_deref(),
            Some("07dbe48c5da487890a8eeb3ec25e5aa88d1a490d6a696a77eca5c3719fdc4170")
        );
        assert!(asset.is_default);
        assert!(asset.path.starts_with(&format!("files/{}/", item.item_id)));
        assert!(asset.path.contains(&asset.asset_id.to_string()));
        assert!(asset.path.ends_with("-incoming paper.pdf"));
        assert!(vault.root_path().join(&asset.path).is_file());

        let before_assets = vault.load_document_assets().unwrap();
        let second = vault
            .import_document_asset(item.item_id, &source, DocumentAssetKind::Version)
            .unwrap();
        assert_eq!(
            second,
            DocumentAssetImportResult::Duplicate {
                existing: asset.clone()
            }
        );
        assert_eq!(vault.load_document_assets().unwrap(), before_assets);
        assert_eq!(
            fs::read_dir(root.join("files").join(item.item_id.to_string()))
                .unwrap()
                .count(),
            1
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn document_asset_import_failure_cleans_copy_and_preserves_asset_store() {
        let (vault, root) = temp_vault("cistella-assets-import-failure");
        let item = vault
            .create_literature_item(draft("Failed import"))
            .unwrap();
        let source = root.join("incoming.pdf");
        write_pdf(&source, b"failure bytes");
        let error = vault
            .import_document_asset_with(
                item.item_id,
                &source,
                DocumentAssetKind::Primary,
                None,
                |_vault, _assets| {
                    Err(CoreError::Io(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "injected publish failure",
                    )))
                },
            )
            .unwrap_err();
        assert!(matches!(error, CoreError::Io(_)));
        assert!(!vault.document_assets_path().exists());
        let item_dir = root.join("files").join(item.item_id.to_string());
        assert!(fs::read_dir(&item_dir).unwrap().next().is_none());
        assert!(fs::read_dir(&item_dir).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn document_asset_import_rejects_non_pdf_and_directories_before_mutation() {
        let (vault, root) = temp_vault("cistella-assets-import-validation");
        let item = vault.create_literature_item(draft("Validation")).unwrap();
        let text = root.join("not-paper.txt");
        fs::write(&text, b"not pdf").unwrap();
        assert!(matches!(
            vault.import_document_asset(item.item_id, &text, DocumentAssetKind::Primary),
            Err(CoreError::UnsupportedLiteratureFile(_))
        ));
        let directory = root.join("directory.pdf");
        fs::create_dir(&directory).unwrap();
        assert!(
            vault
                .import_document_asset(item.item_id, &directory, DocumentAssetKind::Primary)
                .is_err()
        );
        assert!(!vault.document_assets_path().exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn external_asset_migration_preserves_link_and_creates_portable_asset() {
        let (vault, root) = temp_vault("cistella-assets-migrate-external");
        let item = vault
            .create_literature_item(draft("External migration"))
            .unwrap();
        let external = root.join("outside.pdf");
        write_pdf(&external, b"external source bytes");

        let link = vault
            .link_external_document_asset(item.item_id, &external, DocumentAssetKind::Supplement)
            .unwrap();
        assert_eq!(link.storage_kind, DocumentAssetStorageKind::External);
        assert!(link.is_default);

        let imported = vault
            .migrate_external_document_asset(item.item_id, link.asset_id)
            .unwrap();
        let portable = match imported {
            DocumentAssetImportResult::Imported { asset } => asset,
            other => panic!("expected migrated Vault asset, got {other:?}"),
        };
        assert_ne!(portable.asset_id, link.asset_id);
        assert_eq!(portable.storage_kind, DocumentAssetStorageKind::Vault);
        assert_eq!(portable.asset_kind, DocumentAssetKind::Supplement);
        assert!(!portable.is_default);
        assert!(
            vault
                .resolve_document_asset_path(item.item_id, portable.asset_id)
                .is_ok()
        );

        let assets = vault.load_document_assets().unwrap();
        assert_eq!(assets.len(), 2);
        assert!(assets.iter().any(|asset| asset.asset_id == link.asset_id));
        assert!(external.is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn document_asset_management_never_deletes_physical_file_or_guesses_default() {
        let (vault, root) = temp_vault("cistella-assets-manage");
        let item = vault
            .create_literature_item(draft("Asset management"))
            .unwrap();
        let first_source = root.join("first.pdf");
        let second_source = root.join("second.pdf");
        write_pdf(&first_source, b"first");
        write_pdf(&second_source, b"second");
        let first = match vault
            .import_document_asset(item.item_id, &first_source, DocumentAssetKind::Primary)
            .unwrap()
        {
            DocumentAssetImportResult::Imported { asset } => asset,
            other => panic!("expected import, got {other:?}"),
        };
        let second = match vault
            .import_document_asset(item.item_id, &second_source, DocumentAssetKind::Other)
            .unwrap()
        {
            DocumentAssetImportResult::Imported { asset } => asset,
            other => panic!("expected import, got {other:?}"),
        };
        let second = vault
            .set_document_asset_kind(item.item_id, second.asset_id, DocumentAssetKind::Version)
            .unwrap();
        assert_eq!(second.asset_kind, DocumentAssetKind::Version);
        let default = vault
            .set_document_asset_default(item.item_id, second.asset_id)
            .unwrap();
        assert!(default.is_default);

        let first_path = vault
            .resolve_document_asset_path(item.item_id, first.asset_id)
            .unwrap();
        let removed = vault
            .remove_document_asset(item.item_id, second.asset_id)
            .unwrap();
        assert_eq!(removed.asset_id, second.asset_id);
        assert!(first_path.is_file());
        assert!(
            vault
                .resolve_document_asset_path(item.item_id, second.asset_id)
                .is_err()
        );
        let remaining = vault.load_document_assets().unwrap();
        assert_eq!(remaining.len(), 1);
        assert!(!remaining[0].is_default);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reading_session_commands_mutate_only_after_accepted_open_request() {
        let (vault, root) = temp_vault("cistella-reading-session-commands");
        let item = vault
            .create_literature_item(draft("Command lifecycle"))
            .unwrap();
        let source = root.join("command.pdf");
        write_pdf(&source, b"command lifecycle");
        let asset = match vault
            .import_document_asset(item.item_id, &source, DocumentAssetKind::Primary)
            .unwrap()
        {
            DocumentAssetImportResult::Imported { asset } => asset,
            other => panic!("expected imported asset, got {other:?}"),
        };
        let mut opened_path = None;
        let started = vault
            .start_reading_session(item.item_id, asset.asset_id, |path| {
                opened_path = Some(path.to_path_buf());
                Ok(())
            })
            .unwrap();
        assert_eq!(
            opened_path.unwrap(),
            vault
                .resolve_document_asset_path(item.item_id, asset.asset_id)
                .unwrap()
        );
        assert_eq!(started.state, ReadingSessionState::Active);
        assert!(started.ended_at.is_none());

        let paused = vault
            .pause_reading_session(item.item_id, asset.asset_id)
            .unwrap();
        assert_eq!(paused.session_id, started.session_id);
        assert_eq!(paused.state, ReadingSessionState::Paused);

        let resumed = vault
            .resume_reading_session(item.item_id, asset.asset_id, |_path| Ok(()))
            .unwrap();
        assert_eq!(resumed.session_id, started.session_id);
        assert_eq!(resumed.state, ReadingSessionState::Active);
        assert!(resumed.last_opened_at >= started.last_opened_at);

        let ended = vault
            .end_reading_session(item.item_id, asset.asset_id)
            .unwrap();
        assert_eq!(ended.session_id, started.session_id);
        assert_eq!(ended.state, ReadingSessionState::Closed);
        assert!(ended.ended_at.is_some());
        assert!(matches!(
            vault.resume_reading_session(item.item_id, asset.asset_id, |_path| Ok(())),
            Err(CoreError::ReadingSessionNotFound { .. })
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn continue_target_prefers_non_closed_then_latest_history_and_closed_history_starts_new_session()
     {
        let (vault, root) = temp_vault("cistella-reading-session-continue-target");
        let item = vault
            .create_literature_item(draft("Continue target"))
            .unwrap();
        let first_source = root.join("first.pdf");
        let second_source = root.join("second.pdf");
        write_pdf(&first_source, b"first");
        write_pdf(&second_source, b"second");
        let first = match vault
            .import_document_asset(item.item_id, &first_source, DocumentAssetKind::Primary)
            .unwrap()
        {
            DocumentAssetImportResult::Imported { asset } => asset,
            other => panic!("expected first asset, got {other:?}"),
        };
        let second = match vault
            .import_document_asset(item.item_id, &second_source, DocumentAssetKind::Supplement)
            .unwrap()
        {
            DocumentAssetImportResult::Imported { asset } => asset,
            other => panic!("expected second asset, got {other:?}"),
        };
        let now = Utc::now();
        let older_closed = ReadingSession {
            session_id: Uuid::new_v4(),
            item_id: item.item_id,
            asset_id: first.asset_id,
            started_at: now - chrono::Duration::minutes(30),
            last_opened_at: now - chrono::Duration::minutes(20),
            ended_at: Some(now - chrono::Duration::minutes(10)),
            state: ReadingSessionState::Closed,
        };
        let latest_closed = ReadingSession {
            session_id: Uuid::new_v4(),
            item_id: item.item_id,
            asset_id: second.asset_id,
            started_at: now - chrono::Duration::minutes(9),
            last_opened_at: now - chrono::Duration::minutes(5),
            ended_at: Some(now - chrono::Duration::minutes(4)),
            state: ReadingSessionState::Closed,
        };
        let paused = ReadingSession {
            session_id: Uuid::new_v4(),
            item_id: item.item_id,
            asset_id: first.asset_id,
            started_at: now - chrono::Duration::minutes(40),
            last_opened_at: now - chrono::Duration::minutes(25),
            ended_at: None,
            state: ReadingSessionState::Paused,
        };
        vault
            .save_reading_sessions(&[older_closed.clone(), latest_closed.clone(), paused.clone()])
            .unwrap();
        let target = vault.continue_reading_target().unwrap().unwrap();
        assert_eq!(target.session.session_id, paused.session_id);
        assert_eq!(target.asset_status, DocumentAssetStatus::Available);

        vault
            .end_reading_session(item.item_id, first.asset_id)
            .unwrap();
        let historical_target = vault.continue_reading_target().unwrap().unwrap();
        assert_eq!(
            historical_target.session.session_id,
            latest_closed.session_id
        );
        let continued = vault.continue_reading_session(|_path| Ok(())).unwrap();
        assert_ne!(continued.session_id, latest_closed.session_id);
        assert_eq!(continued.item_id, latest_closed.item_id);
        assert_eq!(continued.asset_id, latest_closed.asset_id);
        assert_eq!(continued.state, ReadingSessionState::Active);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejected_or_unavailable_continue_request_keeps_existing_session_json_unchanged() {
        let (vault, root) = temp_vault("cistella-reading-session-open-failure");
        let item = vault.create_literature_item(draft("Open failure")).unwrap();
        let source = root.join("unavailable.pdf");
        write_pdf(&source, b"unavailable");
        let asset = match vault
            .import_document_asset(item.item_id, &source, DocumentAssetKind::Primary)
            .unwrap()
        {
            DocumentAssetImportResult::Imported { asset } => asset,
            other => panic!("expected imported asset, got {other:?}"),
        };
        let started = vault
            .start_reading_session(item.item_id, asset.asset_id, |_path| Ok(()))
            .unwrap();
        let before_rejection = fs::read(vault.reading_sessions_path()).unwrap();
        let rejection = vault
            .resume_reading_session(item.item_id, asset.asset_id, |_path| {
                Err(CoreError::Io(std::io::Error::other(
                    "simulated opener rejection",
                )))
            })
            .unwrap_err();
        assert!(matches!(rejection, CoreError::Io(_)));
        assert_eq!(
            fs::read(vault.reading_sessions_path()).unwrap(),
            before_rejection
        );
        assert_eq!(vault.load_reading_sessions().unwrap()[0], started);

        let asset_path = vault
            .resolve_document_asset_path(item.item_id, asset.asset_id)
            .unwrap();
        fs::remove_file(asset_path).unwrap();
        let summary = vault.list_recent_reading_sessions().unwrap().pop().unwrap();
        assert_eq!(summary.asset_status, DocumentAssetStatus::Missing);
        let before_unavailable = fs::read(vault.reading_sessions_path()).unwrap();
        assert!(
            vault
                .continue_reading_session(|_path| panic!(
                    "unavailable asset must not request opening"
                ))
                .is_err()
        );
        assert_eq!(
            fs::read(vault.reading_sessions_path()).unwrap(),
            before_unavailable
        );
        assert_eq!(vault.load_reading_sessions().unwrap()[0], started);
        fs::remove_dir_all(root).unwrap();
    }

    fn copy_directory(from: &Path, to: &Path) {
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let destination = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                fs::create_dir_all(&destination).unwrap();
                copy_directory(&entry.path(), &destination);
            } else {
                fs::copy(entry.path(), destination).unwrap();
            }
        }
    }
}
