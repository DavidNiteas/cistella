use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, Utc};
use lopdf::Document;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

use crate::{
    error::{CoreError, Result},
    literature::{DocumentAssetStatus, DocumentAssetStorageKind},
    vault::Vault,
};

/// The only on-disk location owned by the local search subsystem. Everything
/// below it is derived data and may be deleted without affecting a Vault.
pub const DERIVED_SEARCH_RELATIVE_DIR: &str = "derived/search";
/// M2 changes the generation schema from M1's pending-only content records to
/// concrete extraction outcomes. M1 generations therefore become safely stale
/// and are rebuilt instead of being interpreted with a mismatched contract.
pub const SEARCH_INDEX_FORMAT_VERSION: u32 = 2;
pub const METADATA_ANALYZER_VERSION: &str = "metadata-v2";
pub const CONTENT_ANALYZER_VERSION: &str = "content-v1";
pub const PDF_TEXT_EXTRACTOR_VERSION: &str = "lopdf-0.44-text-v1";

/// Fixed M2 resource limits, selected against the deterministic test corpus
/// recorded in the work-order status ledger. Extraction remains single-threaded
/// in Core; M3 may schedule these bounded batches asynchronously without
/// exposing paths or the extractor implementation.
pub const PDF_MAX_FILE_SIZE_BYTES: u64 = 32 * 1024 * 1024;
pub const PDF_MAX_PAGES: usize = 500;
pub const PDF_MAX_EXTRACTED_CHARS: usize = 2_000_000;
pub const PDF_MAX_DECOMPRESSED_BYTES_PER_PAGE: usize = 8 * 1024 * 1024;
pub const PDF_MAX_EXTRACTION_DURATION: Duration = Duration::from_secs(5);
pub const PDF_EXTRACTION_BATCH_SIZE: usize = 8;
pub const PDF_EXTRACTION_CONCURRENCY: usize = 1;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SearchIndexStatus {
    Missing,
    Ready,
    Stale,
    Building,
    Degraded,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SearchIndexState {
    pub status: SearchIndexStatus,
    pub active_generation: Option<Uuid>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SearchIndexManifest {
    /// On-disk manifest and generation schema version.
    pub format_version: u32,
    pub metadata_analyzer_version: String,
    pub pdf_text_extractor_version: String,
    pub active_generation: Uuid,
    pub built_at: DateTime<Utc>,
}

/// A non-authoritative marker for an initial build that did not complete. It
/// is never written when a ready generation already exists, because the ready
/// generation remains usable after a failed rebuild.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct SearchIndexFailure {
    format_version: u32,
    failed_at: DateTime<Utc>,
}

/// Fields that can be selected by a query. M1 defines this portable domain
/// contract only; execution and public query APIs remain M3 work.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub enum SearchFieldScope {
    All,
    Metadata,
    Title,
    Authors,
    Tags,
    Content,
}

/// A query value plus the fields it intends to search. It is deliberately a
/// data contract in M1, not an executable search endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SearchQuery {
    pub text: String,
    pub scopes: Vec<SearchFieldScope>,
}

/// A concrete field that contributed to a literature-level match.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub enum SearchMatchField {
    Title,
    Authors,
    Tags,
    Content,
}

/// A path-free explanation of one field match. Content matches use `asset_id`;
/// metadata matches leave it absent. M3 will populate these from query work.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SearchFieldMatch {
    pub field: SearchMatchField,
    pub matched_terms: Vec<String>,
    pub asset_id: Option<Uuid>,
    /// Present only for content matches. This is derived index health, never a
    /// filesystem path or an opener capability.
    #[serde(default)]
    pub asset_state: Option<AssetContentIndexState>,
    pub excerpt: Option<String>,
}

/// One literature-level result. Multiple matching assets remain attached as
/// field matches instead of becoming duplicate literature results.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub item_id: Uuid,
    pub field_matches: Vec<SearchFieldMatch>,
}

/// A bounded, reproducible slice of a ready local search generation. The
/// result remains literature-level: all matching metadata fields and assets
/// for an item are attached to one `SearchHit`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SearchQueryPage {
    pub index_state: SearchIndexState,
    pub hits: Vec<SearchHit>,
    pub total_hits: usize,
    pub offset: usize,
    pub limit: usize,
}

/// Querying a missing, stale, degraded, building, or failed index is an
/// explicit stateful outcome, never an empty-result impersonation. Clients can
/// render the health and offer a deliberate sync/rebuild action.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "outcome", rename_all = "camelCase")]
pub enum SearchQueryResult {
    Ready { page: SearchQueryPage },
    Unavailable { index_state: SearchIndexState },
}

pub const DEFAULT_SEARCH_PAGE_SIZE: usize = 50;
pub const MAX_SEARCH_PAGE_SIZE: usize = 100;
pub const SEARCH_EXCERPT_MAX_CHARS: usize = 240;

/// The task lifecycle contract used by later asynchronous rebuild control.
/// M1 has no public task runner or cancellation API.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SearchIndexTaskStatus {
    Idle,
    Building,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SearchIndexTaskState {
    pub status: SearchIndexTaskStatus,
    pub generation_id: Option<Uuid>,
    pub detail: Option<String>,
}

/// M2 reports a failure or exclusion for an individual asset through this
/// path-free contract; it never escalates an asset issue into Vault failure.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub enum SearchIndexIssueKind {
    /// Kept solely so stale M1 generations remain deserializable long enough
    /// to be classified as stale. M2 never emits this outcome.
    PendingExtraction,
    NoText,
    Missing,
    ExternalUnavailable,
    Encrypted,
    Unsupported,
    ParseFailed,
    LimitExceeded,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SearchIndexIssue {
    pub item_id: Uuid,
    pub asset_id: Uuid,
    pub kind: SearchIndexIssueKind,
    pub detail: Option<String>,
}

/// A path-free authoritative change that can be folded into an already-ready
/// derived generation. The change describes only stable Vault identities; it
/// never carries a resolved file path or caller-owned payload.
///
/// Core treats these as best-effort derived work. A failed incremental publish
/// must not undo the authoritative literature or asset operation that emitted
/// the change; a later full reconciliation remains the convergence backstop.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum SearchIndexChange {
    MetadataChanged { item_id: Uuid },
    AssetChanged { item_id: Uuid, asset_id: Uuid },
    AssetRemoved { item_id: Uuid, asset_id: Uuid },
    ItemRemoved { item_id: Uuid },
}

#[derive(Debug, Default)]
struct SearchIndexAffectedSet {
    metadata_changed: BTreeSet<Uuid>,
    asset_changed: BTreeSet<(Uuid, Uuid)>,
    asset_removed: BTreeSet<(Uuid, Uuid)>,
    item_removed: BTreeSet<Uuid>,
}

impl SearchIndexAffectedSet {
    fn from_changes(changes: &[SearchIndexChange]) -> Self {
        let mut affected = Self::default();
        for change in changes {
            match *change {
                SearchIndexChange::MetadataChanged { item_id } => {
                    affected.metadata_changed.insert(item_id);
                }
                SearchIndexChange::AssetChanged { item_id, asset_id } => {
                    affected.asset_changed.insert((item_id, asset_id));
                }
                SearchIndexChange::AssetRemoved { item_id, asset_id } => {
                    affected.asset_removed.insert((item_id, asset_id));
                }
                SearchIndexChange::ItemRemoved { item_id } => {
                    affected.item_removed.insert(item_id);
                }
            }
        }
        affected
    }

    fn is_empty(&self) -> bool {
        self.metadata_changed.is_empty()
            && self.asset_changed.is_empty()
            && self.asset_removed.is_empty()
            && self.item_removed.is_empty()
    }
}

/// Stable analyzed terms for one metadata field. Raw display values remain in
/// `MetadataIndexRecord`; terms are portable and contain no filesystem data.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MetadataAnalyzedField {
    pub field: SearchMatchField,
    pub terms: Vec<String>,
}

/// A portable, searchable representation of user-owned literature metadata.
/// It intentionally contains stable cistella identity and user metadata only;
/// neither the Vault root nor an asset path is part of this record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MetadataIndexRecord {
    pub item_id: Uuid,
    pub title: String,
    pub authors: Vec<String>,
    pub tags: Vec<String>,
    pub metadata_fingerprint: String,
    pub analyzed_fields: Vec<MetadataAnalyzedField>,
}

/// M2 extraction outcome for one asset. The deprecated pending state keeps
/// M1 JSON backward-deserializable; M2 only writes concrete outcomes.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AssetContentIndexState {
    PendingExtraction,
    Indexed,
    NoText,
    Missing,
    ExternalUnavailable,
    Encrypted,
    Unsupported,
    ParseFailed,
    LimitExceeded,
    Cancelled,
}

impl AssetContentIndexState {
    fn issue_kind(self) -> Option<SearchIndexIssueKind> {
        match self {
            Self::PendingExtraction => Some(SearchIndexIssueKind::PendingExtraction),
            Self::Indexed => None,
            Self::NoText => Some(SearchIndexIssueKind::NoText),
            Self::Missing => Some(SearchIndexIssueKind::Missing),
            Self::ExternalUnavailable => Some(SearchIndexIssueKind::ExternalUnavailable),
            Self::Encrypted => Some(SearchIndexIssueKind::Encrypted),
            Self::Unsupported => Some(SearchIndexIssueKind::Unsupported),
            Self::ParseFailed => Some(SearchIndexIssueKind::ParseFailed),
            Self::LimitExceeded => Some(SearchIndexIssueKind::LimitExceeded),
            Self::Cancelled => Some(SearchIndexIssueKind::Cancelled),
        }
    }

    fn is_reusable_success(self) -> bool {
        self == Self::Indexed
    }
}

/// The content side of the index contract. The fields are identity-only plus
/// derived values; no runtime filesystem path is persisted or returned.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AssetContentIndexRecord {
    pub item_id: Uuid,
    pub asset_id: Uuid,
    pub content: Option<String>,
    /// Hash of the bytes actually resolved through the Vault asset boundary.
    pub content_hash: Option<String>,
    /// Fingerprint of the owning metadata at this generation. It lets
    /// reconciliation update metadata without coupling a content extraction to
    /// a file path or to stale display fields.
    #[serde(default)]
    pub metadata_fingerprint: String,
    pub extractor_version: String,
    pub analyzer_version: String,
    pub state: AssetContentIndexState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SearchIndexGeneration {
    pub format_version: u32,
    pub metadata_analyzer_version: String,
    pub pdf_text_extractor_version: String,
    pub generation_id: Uuid,
    pub built_at: DateTime<Utc>,
    pub metadata_records: Vec<MetadataIndexRecord>,
    pub asset_content_records: Vec<AssetContentIndexRecord>,
    #[serde(default)]
    pub issues: Vec<SearchIndexIssue>,
}

impl Vault {
    /// Reports index health without creating or repairing derived data. A
    /// missing, malformed, or stale index never prevents the Vault itself from
    /// being opened or used for its authoritative data.
    pub fn search_index_state(&self) -> SearchIndexState {
        let root = self.search_index_root();
        let derived = self.root_path().join("derived");
        // Do not use `Path::exists()` here: it follows a dangling directory
        // link and would turn an unsafe `derived/` or `derived/search/` object
        // into a false `Missing` result. `symlink_metadata` lets absence remain
        // benign while rejecting every existing link/reparse/non-directory
        // object before any later status path can inspect it.
        let derived_is_present = match ensure_owned_directory_if_present(&derived) {
            Ok(present) => present,
            Err(_) => return degraded_search_layout_state(),
        };
        if !derived_is_present {
            return SearchIndexState {
                status: SearchIndexStatus::Missing,
                active_generation: None,
                detail: None,
            };
        }
        let root_is_present = match ensure_owned_directory_if_present(&root) {
            Ok(present) => present,
            Err(_) => return degraded_search_layout_state(),
        };
        if !root_is_present {
            return SearchIndexState {
                status: SearchIndexStatus::Missing,
                active_generation: None,
                detail: None,
            };
        }
        if self.ensure_existing_search_root_is_safe().is_err() {
            return SearchIndexState {
                status: SearchIndexStatus::Degraded,
                active_generation: None,
                detail: Some(
                    "derived search directory has an unsafe link or reparse layout".to_string(),
                ),
            };
        }
        // A ready active generation is not sufficient evidence that the whole
        // derived index is safe. Every existing working and committed
        // generation is cistella-owned state, so status must reject an unsafe
        // inactive entry instead of presenting the index as healthy.
        if self.ensure_existing_search_index_layout_is_safe().is_err() {
            return SearchIndexState {
                status: SearchIndexStatus::Degraded,
                active_generation: None,
                detail: Some(
                    "search index has an unsafe existing generation link or reparse layout"
                        .to_string(),
                ),
            };
        }

        // Validate every owned working-generation entry before inspecting the
        // manifest. An unsafe child must never be mistaken for a normal
        // in-progress build, even when an older committed generation is ready.
        let has_working_generation = match self.has_incomplete_search_working_generation(&root) {
            Ok(value) => value,
            Err(_) => {
                return SearchIndexState {
                    status: SearchIndexStatus::Degraded,
                    active_generation: None,
                    detail: Some(
                        "search working directory has an unsafe link or reparse layout".to_string(),
                    ),
                };
            }
        };

        let manifest_path = root.join("manifest.json");
        let failure_path = root.join("failure.json");
        // These are cistella-owned files, not just paths beneath an owned
        // directory. Validate the file object before a JSON read can follow a
        // file symlink or Windows reparse point outside the index boundary.
        if ensure_owned_index_file_if_present(&manifest_path).is_err() {
            return SearchIndexState {
                status: SearchIndexStatus::Degraded,
                active_generation: None,
                detail: Some("search index has an unsafe file link or reparse layout".to_string()),
            };
        }
        let has_failure = match ensure_owned_index_file_if_present(&failure_path) {
            Ok(present) => present,
            Err(_) => {
                return SearchIndexState {
                    status: SearchIndexStatus::Degraded,
                    active_generation: None,
                    detail: Some(
                        "search index has an unsafe file link or reparse layout".to_string(),
                    ),
                };
            }
        };
        let manifest = match read_owned_json::<SearchIndexManifest>(&manifest_path) {
            Ok(manifest) => manifest,
            Err(_) if has_working_generation => {
                return SearchIndexState {
                    status: SearchIndexStatus::Building,
                    active_generation: None,
                    detail: Some("initial search generation is being built".to_string()),
                };
            }
            Err(_) if has_failure => {
                return SearchIndexState {
                    status: SearchIndexStatus::Failed,
                    active_generation: None,
                    detail: Some("initial search generation did not complete".to_string()),
                };
            }
            // An intact derived/search layout without a manifest, working
            // generation, or failure marker is simply an index that has not
            // been built (or whose first build was intentionally cancelled).
            Err(_) => {
                return SearchIndexState {
                    status: SearchIndexStatus::Missing,
                    active_generation: None,
                    detail: None,
                };
            }
        };
        if manifest.format_version != SEARCH_INDEX_FORMAT_VERSION
            || manifest.metadata_analyzer_version != METADATA_ANALYZER_VERSION
            || manifest.pdf_text_extractor_version != PDF_TEXT_EXTRACTOR_VERSION
        {
            return SearchIndexState {
                status: SearchIndexStatus::Stale,
                active_generation: Some(manifest.active_generation),
                detail: Some("search index format version is incompatible".to_string()),
            };
        }

        let generation = self.search_generation_path(manifest.active_generation);
        if self
            .ensure_search_layout_is_safe(&root, Some(&generation))
            .is_err()
        {
            return SearchIndexState {
                status: SearchIndexStatus::Degraded,
                active_generation: Some(manifest.active_generation),
                detail: Some(
                    "active search generation has an unsafe link or reparse layout".to_string(),
                ),
            };
        }
        let metadata_path = generation.join("metadata.json");
        if ensure_existing_owned_regular_file(&metadata_path).is_err() {
            return SearchIndexState {
                status: SearchIndexStatus::Degraded,
                active_generation: Some(manifest.active_generation),
                detail: Some(
                    "active search generation has an unsafe file link or reparse layout"
                        .to_string(),
                ),
            };
        }
        match read_owned_json::<SearchIndexGeneration>(&metadata_path) {
            Ok(snapshot)
                if snapshot.format_version == SEARCH_INDEX_FORMAT_VERSION
                    && snapshot.metadata_analyzer_version == METADATA_ANALYZER_VERSION
                    && snapshot.pdf_text_extractor_version == PDF_TEXT_EXTRACTOR_VERSION
                    && snapshot.generation_id == manifest.active_generation =>
            {
                SearchIndexState {
                    status: SearchIndexStatus::Ready,
                    active_generation: Some(manifest.active_generation),
                    detail: None,
                }
            }
            Ok(_) => SearchIndexState {
                status: SearchIndexStatus::Stale,
                active_generation: Some(manifest.active_generation),
                detail: Some("search generation format is incompatible".to_string()),
            },
            Err(_) => SearchIndexState {
                status: SearchIndexStatus::Degraded,
                active_generation: Some(manifest.active_generation),
                detail: Some("active search generation is missing or unreadable".to_string()),
            },
        }
    }

    /// Builds a fresh M2 metadata-and-PDF generation and commits it only after
    /// the working generation is complete. Existing committed generations stay
    /// selected if the rebuild itself fails before manifest publication.
    pub fn rebuild_metadata_search_index(&self) -> Result<SearchIndexState> {
        self.rebuild_metadata_search_index_with(|_| Ok(()))
    }

    /// Same full rebuild contract, but lets the desktop task coordinator stop
    /// between assets and before publication. A cancelled build removes its
    /// working generation and leaves any prior ready generation selected.
    pub fn rebuild_metadata_search_index_cancellable<F>(
        &self,
        is_cancelled: F,
    ) -> Result<SearchIndexState>
    where
        F: Fn() -> bool,
    {
        self.rebuild_metadata_search_index_with_cancellation(&is_cancelled, |_| Ok(()))
    }

    /// Applies a bounded, identity-only authoritative change to an already
    /// ready derived index. It reuses all unaffected records and only resolves,
    /// hashes, or extracts the explicitly changed Vault assets. Missing, stale,
    /// degraded, or failed indexes are intentionally not created or repaired
    /// here: callers keep their successful authoritative operation and let the
    /// full reconciliation backstop converge later.
    pub fn update_search_index_incrementally(
        &self,
        changes: &[SearchIndexChange],
    ) -> Result<SearchIndexState> {
        let affected = SearchIndexAffectedSet::from_changes(changes);
        if affected.is_empty() {
            return Ok(self.search_index_state());
        }
        let state = self.search_index_state();
        if state.status != SearchIndexStatus::Ready {
            return Err(CoreError::SearchIndexUnavailable(
                state
                    .detail
                    .unwrap_or_else(|| format!("status is {:?}", state.status)),
            ));
        }
        self.commit_search_index_generation_with(
            |_| Ok(()),
            |generation_id| self.build_incremental_search_generation(generation_id, &affected),
        )
    }

    /// Fully re-reads authoritative literature and asset state to reconcile
    /// changes that were missed while the derived index was absent, unavailable,
    /// or failed to publish. This explicit synchronous Core operation is never
    /// called by `Vault::open`; M3 will schedule it in the desktop task layer.
    /// It is the convergence backstop, not the normal mutation update path.
    pub fn reconcile_search_index(&self) -> Result<SearchIndexState> {
        self.rebuild_metadata_search_index()
    }

    /// Cancellable form of explicit full reconciliation. It is intentionally
    /// opt-in: opening a Vault and normal authority mutations never trigger
    /// this full scan implicitly.
    pub fn reconcile_search_index_cancellable<F>(&self, is_cancelled: F) -> Result<SearchIndexState>
    where
        F: Fn() -> bool,
    {
        self.rebuild_metadata_search_index_cancellable(is_cancelled)
    }

    /// Executes the deliberately small M3 local-query contract over the
    /// committed derived generation. No index engine query syntax, path, or
    /// file handle crosses this boundary.
    pub fn query_search_index(
        &self,
        query: &SearchQuery,
        offset: usize,
        limit: usize,
    ) -> Result<SearchQueryResult> {
        let index_state = self.search_index_state();
        if index_state.status != SearchIndexStatus::Ready {
            return Ok(SearchQueryResult::Unavailable { index_state });
        }
        let generation = self.load_search_index_generation()?;
        let terms = analyzed_metadata_field(SearchMatchField::Content, &query.text).terms;
        let limit = limit.clamp(1, MAX_SEARCH_PAGE_SIZE);
        if terms.is_empty() {
            return Ok(SearchQueryResult::Ready {
                page: SearchQueryPage {
                    index_state,
                    hits: Vec::new(),
                    total_hits: 0,
                    offset,
                    limit,
                },
            });
        }
        let scopes = normalized_query_scopes(&query.scopes);
        let mut grouped = BTreeMap::<Uuid, Vec<SearchFieldMatch>>::new();
        for record in &generation.metadata_records {
            for analyzed in &record.analyzed_fields {
                if !scope_includes_match_field(&scopes, analyzed.field) {
                    continue;
                }
                let matched_terms = matched_query_terms(&terms, &analyzed.terms);
                if matched_terms.len() == terms.len() {
                    grouped
                        .entry(record.item_id)
                        .or_default()
                        .push(SearchFieldMatch {
                            field: analyzed.field,
                            matched_terms,
                            asset_id: None,
                            asset_state: None,
                            excerpt: None,
                        });
                }
            }
        }
        if scope_includes_match_field(&scopes, SearchMatchField::Content) {
            for record in &generation.asset_content_records {
                let Some(content) = record.content.as_deref() else {
                    continue;
                };
                if record.state != AssetContentIndexState::Indexed {
                    continue;
                }
                let analyzed = analyzed_metadata_field(SearchMatchField::Content, content);
                let matched_terms = matched_query_terms(&terms, &analyzed.terms);
                if matched_terms.len() == terms.len() {
                    grouped
                        .entry(record.item_id)
                        .or_default()
                        .push(SearchFieldMatch {
                            field: SearchMatchField::Content,
                            matched_terms,
                            asset_id: Some(record.asset_id),
                            asset_state: Some(record.state),
                            excerpt: Some(search_excerpt(content, &terms)),
                        });
                }
            }
        }
        let mut all_hits = grouped
            .into_iter()
            .map(|(item_id, mut field_matches)| {
                field_matches.sort_by_key(|entry| {
                    (search_match_field_sort_key(entry.field), entry.asset_id)
                });
                SearchHit {
                    item_id,
                    field_matches,
                }
            })
            .collect::<Vec<_>>();
        // C-layer M3 ordering: metadata matches rank before content-only
        // matches; ties are stable `item_id` order. No score is exposed.
        all_hits.sort_by_key(|hit| {
            let metadata_rank = hit
                .field_matches
                .iter()
                .any(|entry| entry.field != SearchMatchField::Content);
            (!metadata_rank, hit.item_id)
        });
        let total_hits = all_hits.len();
        let hits = all_hits.into_iter().skip(offset).take(limit).collect();
        Ok(SearchQueryResult::Ready {
            page: SearchQueryPage {
                index_state,
                hits,
                total_hits,
                offset,
                limit,
            },
        })
    }

    /// Returns asset-level derived issues only when a ready generation exists.
    /// Callers must use `search_index_state`/`query_search_index` to distinguish
    /// unavailable index health from an empty issue list.
    pub fn search_index_issues(&self) -> Result<Vec<SearchIndexIssue>> {
        Ok(self.load_search_index_generation()?.issues)
    }

    /// Loads the committed M1 generation for later query work. This is an
    /// inspection API, not a path API: consumers only receive cistella IDs and
    /// portable derived values.
    pub fn load_search_index_generation(&self) -> Result<SearchIndexGeneration> {
        let root = self.search_index_root();
        let state = self.search_index_state();
        if state.status != SearchIndexStatus::Ready {
            return Err(CoreError::SearchIndexUnavailable(
                state
                    .detail
                    .unwrap_or_else(|| format!("status is {:?}", state.status)),
            ));
        }
        let generation_id = state.active_generation.ok_or_else(|| {
            CoreError::SearchIndexUnavailable("active generation is missing".to_string())
        })?;
        // Recheck the complete existing layout after the status read. This is
        // intentionally independent of `search_index_state`: an inactive or
        // working generation must not become a link/reparse object between the
        // status result and the generation read.
        self.ensure_existing_search_index_layout_is_safe()
            .map_err(|_| {
                CoreError::SearchIndexUnavailable(
                    "search index has an unsafe existing generation link or reparse layout"
                        .to_string(),
                )
            })?;
        let generation_path = root.join("generations").join(generation_id.to_string());
        self.ensure_search_layout_is_safe(&root, Some(&generation_path))
            .map_err(|_| {
                CoreError::SearchIndexUnavailable(
                    "active search generation has an unsafe link or reparse layout".to_string(),
                )
            })?;
        let metadata_path = generation_path.join("metadata.json");
        ensure_existing_owned_regular_file(&metadata_path).map_err(|_| {
            CoreError::SearchIndexUnavailable(
                "active search generation has an unsafe file link or reparse layout".to_string(),
            )
        })?;
        let generation = read_owned_json::<SearchIndexGeneration>(&metadata_path).map_err(
            |error| match error {
                CoreError::InvalidSearchIndexPath => CoreError::SearchIndexUnavailable(
                    "active search generation has an unsafe file link or reparse layout"
                        .to_string(),
                ),
                other => other,
            },
        )?;
        if generation.format_version != SEARCH_INDEX_FORMAT_VERSION
            || generation.metadata_analyzer_version != METADATA_ANALYZER_VERSION
            || generation.pdf_text_extractor_version != PDF_TEXT_EXTRACTOR_VERSION
            || generation.generation_id != generation_id
        {
            return Err(CoreError::SearchIndexVersionIncompatible);
        }
        Ok(generation)
    }

    fn rebuild_metadata_search_index_with<F>(&self, before_commit: F) -> Result<SearchIndexState>
    where
        F: FnOnce(&Path) -> Result<()>,
    {
        self.rebuild_metadata_search_index_with_cancellation(&|| false, before_commit)
    }

    fn rebuild_metadata_search_index_with_cancellation<F>(
        &self,
        is_cancelled: &dyn Fn() -> bool,
        before_commit: F,
    ) -> Result<SearchIndexState>
    where
        F: FnOnce(&Path) -> Result<()>,
    {
        self.commit_search_index_generation_with(
            |working| {
                if is_cancelled() {
                    return Err(CoreError::SearchIndexBuildCancelled);
                }
                before_commit(working)
            },
            |generation_id| self.build_metadata_generation(generation_id, is_cancelled),
        )
    }

    /// Shares the M1 generation-safe commit protocol between the full
    /// reconciliation builder and bounded mutation updates. The builder is the
    /// only varying part; all writes retain the working-generation, no-follow,
    /// atomic-manifest guarantees.
    fn commit_search_index_generation_with<F, Build>(
        &self,
        before_commit: F,
        build: Build,
    ) -> Result<SearchIndexState>
    where
        F: FnOnce(&Path) -> Result<()>,
        Build: FnOnce(Uuid) -> Result<SearchIndexGeneration>,
    {
        let previous_ready = self.search_index_state().status == SearchIndexStatus::Ready;
        let root = self.prepare_search_root()?;
        if !previous_ready {
            remove_owned_index_file_if_present(&root.join("failure.json"))?;
        }
        let generation_id = Uuid::new_v4();
        let working = root.join("working").join(generation_id.to_string());
        self.ensure_generation_slot_is_safe(&root, &working)?;
        ensure_directory_component_is_owned(&working)?;
        self.ensure_search_layout_is_safe(&root, Some(&working))?;

        let result = (|| -> Result<SearchIndexState> {
            let snapshot = build(generation_id)?;
            self.ensure_search_layout_is_safe(&root, Some(&working))?;
            write_json_atomic(
                &working.join("metadata.json"),
                &snapshot,
                ".search_generation",
            )?;
            before_commit(&working)?;

            let generations = root.join("generations");
            self.ensure_search_layout_is_safe(&root, Some(&working))?;
            self.ensure_generation_slot_is_safe(
                &root,
                &generations.join(generation_id.to_string()),
            )?;
            let committed = generations.join(generation_id.to_string());
            fs::rename(&working, &committed)?;
            self.ensure_search_layout_is_safe(&root, Some(&committed))?;

            let manifest = SearchIndexManifest {
                format_version: SEARCH_INDEX_FORMAT_VERSION,
                metadata_analyzer_version: METADATA_ANALYZER_VERSION.to_string(),
                pdf_text_extractor_version: PDF_TEXT_EXTRACTOR_VERSION.to_string(),
                active_generation: generation_id,
                built_at: snapshot.built_at,
            };
            self.ensure_search_layout_is_safe(&root, Some(&committed))?;
            if let Err(error) =
                write_json_atomic(&root.join("manifest.json"), &manifest, ".search_manifest")
            {
                let _ = fs::remove_dir_all(&committed);
                return Err(error);
            }
            Ok(SearchIndexState {
                status: SearchIndexStatus::Ready,
                active_generation: Some(generation_id),
                detail: None,
            })
        })();

        if working.exists()
            && self
                .ensure_search_layout_is_safe(&root, Some(&working))
                .is_ok()
        {
            let _ = fs::remove_dir_all(&working);
        }
        match result {
            Ok(state) => {
                remove_owned_index_file_if_present(&root.join("failure.json"))?;
                Ok(state)
            }
            Err(error) => {
                // Cancellation is an intentional non-publication outcome, not
                // an index failure. With no prior ready generation it leaves
                // the index structurally missing; with one it keeps that
                // generation selected.
                if !previous_ready && !matches!(error, CoreError::SearchIndexBuildCancelled) {
                    let failure = SearchIndexFailure {
                        format_version: SEARCH_INDEX_FORMAT_VERSION,
                        failed_at: Utc::now(),
                    };
                    if self.ensure_search_layout_is_safe(&root, None).is_ok() {
                        let failure_path = root.join("failure.json");
                        // A failure marker is derived and optional, but an
                        // unsafe object at its expected path is never
                        // disposable. Surface the layout violation instead of
                        // replacing a link/reparse point with a fresh marker.
                        ensure_owned_index_file_if_present(&failure_path)?;
                        if let Err(write_error) =
                            write_json_atomic(&failure_path, &failure, ".search_failure")
                        {
                            if matches!(write_error, CoreError::InvalidSearchIndexPath) {
                                return Err(write_error);
                            }
                        }
                    }
                }
                Err(error)
            }
        }
    }

    /// Builds a new generation by cloning an existing ready generation and
    /// changing only the supplied authoritative identities. This method may
    /// load the authoritative JSON stores to locate those identities, but it
    /// never resolves, hashes, or extracts an unrelated PDF.
    fn build_incremental_search_generation(
        &self,
        generation_id: Uuid,
        affected: &SearchIndexAffectedSet,
    ) -> Result<SearchIndexGeneration> {
        let previous = self.load_search_index_generation()?;
        let items = self.load_literature_items()?;
        let assets = self.load_document_assets()?;
        let items_by_id = items
            .iter()
            .map(|item| (item.item_id, item))
            .collect::<BTreeMap<_, _>>();
        let assets_by_id = assets
            .iter()
            .map(|asset| ((asset.item_id, asset.asset_id), asset))
            .collect::<BTreeMap<_, _>>();

        let mut metadata_records = previous
            .metadata_records
            .into_iter()
            .map(|record| (record.item_id, record))
            .collect::<BTreeMap<_, _>>();
        let mut asset_records = previous
            .asset_content_records
            .into_iter()
            .map(|record| ((record.item_id, record.asset_id), record))
            .collect::<BTreeMap<_, _>>();
        let mut issues = previous
            .issues
            .into_iter()
            .map(|issue| ((issue.item_id, issue.asset_id), issue))
            .collect::<BTreeMap<_, _>>();

        // Item removal takes precedence over any stale companion event.
        for item_id in &affected.item_removed {
            metadata_records.remove(item_id);
            asset_records.retain(|(record_item_id, _), _| record_item_id != item_id);
            issues.retain(|(record_item_id, _), _| record_item_id != item_id);
        }

        // A metadata-only mutation changes display fields, analyzed fields, and
        // every already-derived asset record's metadata fingerprint. It does
        // not inspect any PDF because content bytes are unaffected.
        for item_id in &affected.metadata_changed {
            if affected.item_removed.contains(item_id) {
                continue;
            }
            let Some(item) = items_by_id.get(item_id) else {
                metadata_records.remove(item_id);
                asset_records.retain(|(record_item_id, _), _| record_item_id != item_id);
                issues.retain(|(record_item_id, _), _| record_item_id != item_id);
                continue;
            };
            let record = Self::metadata_record_from_item(item);
            let fingerprint = record.metadata_fingerprint.clone();
            metadata_records.insert(*item_id, record);
            for ((record_item_id, _), asset_record) in asset_records.iter_mut() {
                if record_item_id == item_id {
                    asset_record.metadata_fingerprint = fingerprint.clone();
                }
            }
        }

        for (item_id, asset_id) in &affected.asset_removed {
            if affected.item_removed.contains(item_id) {
                continue;
            }
            asset_records.remove(&(*item_id, *asset_id));
            issues.remove(&(*item_id, *asset_id));
        }

        // Changed/new assets are the only branch allowed to resolve a Vault
        // path and calculate a content hash. All unaffected records stay as
        // carried portable values from the previous committed generation.
        for (item_id, asset_id) in &affected.asset_changed {
            if affected.item_removed.contains(item_id)
                || affected.asset_removed.contains(&(*item_id, *asset_id))
            {
                continue;
            }
            let Some(item) = items_by_id.get(item_id) else {
                metadata_records.remove(item_id);
                asset_records.retain(|(record_item_id, _), _| record_item_id != item_id);
                issues.retain(|(record_item_id, _), _| record_item_id != item_id);
                continue;
            };
            let metadata_record = metadata_records
                .entry(*item_id)
                .or_insert_with(|| Self::metadata_record_from_item(item));
            let metadata_fingerprint = metadata_record.metadata_fingerprint.clone();
            let key = (*item_id, *asset_id);
            let Some(asset) = assets_by_id.get(&key) else {
                asset_records.remove(&key);
                issues.remove(&key);
                continue;
            };
            let outcome =
                self.index_asset_content(asset, &metadata_fingerprint, asset_records.get(&key));
            if let Some(kind) = outcome.record.state.issue_kind() {
                issues.insert(
                    key,
                    SearchIndexIssue {
                        item_id: outcome.record.item_id,
                        asset_id: outcome.record.asset_id,
                        kind,
                        detail: outcome.detail,
                    },
                );
            } else {
                issues.remove(&key);
            }
            asset_records.insert(key, outcome.record);
        }

        Ok(SearchIndexGeneration {
            format_version: SEARCH_INDEX_FORMAT_VERSION,
            metadata_analyzer_version: METADATA_ANALYZER_VERSION.to_string(),
            pdf_text_extractor_version: PDF_TEXT_EXTRACTOR_VERSION.to_string(),
            generation_id,
            built_at: Utc::now(),
            metadata_records: metadata_records.into_values().collect(),
            asset_content_records: asset_records.into_values().collect(),
            issues: issues.into_values().collect(),
        })
    }

    fn build_metadata_generation(
        &self,
        generation_id: Uuid,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<SearchIndexGeneration> {
        let items = self.load_literature_items()?;
        let assets = self.load_document_assets()?;
        let previous = self.load_search_index_generation().ok();

        let mut metadata_records = items
            .iter()
            .map(Self::metadata_record_from_item)
            .collect::<Vec<_>>();
        metadata_records.sort_by_key(|record| record.item_id);

        let known_metadata = metadata_records
            .iter()
            .map(|record| (record.item_id, record.metadata_fingerprint.clone()))
            .collect::<BTreeMap<_, _>>();
        let previous_records = previous
            .as_ref()
            .map(|generation| {
                generation
                    .asset_content_records
                    .iter()
                    .map(|record| ((record.item_id, record.asset_id), record))
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();

        let mut asset_content_records = Vec::new();
        let mut issues = Vec::new();
        for asset in assets {
            if is_cancelled() {
                return Err(CoreError::SearchIndexBuildCancelled);
            }
            let Some(metadata_fingerprint) = known_metadata.get(&asset.item_id) else {
                // A deleted literature item makes the asset relationship an
                // orphan. It deliberately has no derived content record.
                continue;
            };
            let previous_record = previous_records
                .get(&(asset.item_id, asset.asset_id))
                .copied();
            let outcome = self.index_asset_content(&asset, metadata_fingerprint, previous_record);
            if let Some(kind) = outcome.record.state.issue_kind() {
                issues.push(SearchIndexIssue {
                    item_id: outcome.record.item_id,
                    asset_id: outcome.record.asset_id,
                    kind,
                    detail: outcome.detail,
                });
            }
            asset_content_records.push(outcome.record);
        }
        asset_content_records.sort_by_key(|record| (record.item_id, record.asset_id));
        issues.sort_by_key(|issue| (issue.item_id, issue.asset_id, issue.kind));

        Ok(SearchIndexGeneration {
            format_version: SEARCH_INDEX_FORMAT_VERSION,
            metadata_analyzer_version: METADATA_ANALYZER_VERSION.to_string(),
            pdf_text_extractor_version: PDF_TEXT_EXTRACTOR_VERSION.to_string(),
            generation_id,
            built_at: Utc::now(),
            metadata_records,
            asset_content_records,
            issues,
        })
    }

    /// Produces a complete path-free metadata record without inspecting any
    /// asset. Both full reconciliation and metadata-only incremental updates
    /// use this exact normalization contract.
    fn metadata_record_from_item(item: &crate::literature::LiteratureItem) -> MetadataIndexRecord {
        let analyzed_fields = vec![
            analyzed_metadata_field(SearchMatchField::Title, &item.title),
            analyzed_metadata_field(SearchMatchField::Authors, &item.authors.join("\u{001f}")),
            analyzed_metadata_field(SearchMatchField::Tags, &item.tags.join("\u{001f}")),
        ];
        MetadataIndexRecord {
            item_id: item.item_id,
            metadata_fingerprint: metadata_fingerprint(&item.title, &item.authors, &item.tags),
            title: item.title.clone(),
            authors: item.authors.clone(),
            tags: item.tags.clone(),
            analyzed_fields,
        }
    }

    /// Converts every authoritative document-asset relationship into a
    /// path-free derived record. Only this method resolves a Vault PDF, and it
    /// does so through `item_id + asset_id`; external assets are never opened.
    fn index_asset_content(
        &self,
        asset: &crate::literature::DocumentAsset,
        metadata_fingerprint: &str,
        previous: Option<&AssetContentIndexRecord>,
    ) -> AssetIndexOutcome {
        let base = |state, content, content_hash| AssetContentIndexRecord {
            item_id: asset.item_id,
            asset_id: asset.asset_id,
            content,
            content_hash,
            metadata_fingerprint: metadata_fingerprint.to_string(),
            extractor_version: PDF_TEXT_EXTRACTOR_VERSION.to_string(),
            analyzer_version: CONTENT_ANALYZER_VERSION.to_string(),
            state,
        };

        if asset.storage_kind == DocumentAssetStorageKind::External {
            return AssetIndexOutcome::issue(
                base(AssetContentIndexState::ExternalUnavailable, None, None),
                "external assets are not eligible for local full-text extraction",
            );
        }
        if !asset.media_type.eq_ignore_ascii_case("application/pdf") {
            return AssetIndexOutcome::issue(
                base(AssetContentIndexState::Unsupported, None, None),
                "only Vault PDF text layers are supported",
            );
        }

        match asset.status(self) {
            DocumentAssetStatus::Missing => {
                return AssetIndexOutcome::issue(
                    base(AssetContentIndexState::Missing, None, None),
                    "Vault PDF is missing",
                );
            }
            DocumentAssetStatus::ExternalUnavailable => {
                return AssetIndexOutcome::issue(
                    base(AssetContentIndexState::ExternalUnavailable, None, None),
                    "external asset is unavailable",
                );
            }
            DocumentAssetStatus::Invalid => {
                return AssetIndexOutcome::issue(
                    base(AssetContentIndexState::Unsupported, None, None),
                    "asset is not a supported Vault PDF",
                );
            }
            DocumentAssetStatus::Unreadable => {
                return AssetIndexOutcome::issue(
                    base(AssetContentIndexState::ParseFailed, None, None),
                    "Vault PDF cannot be read",
                );
            }
            DocumentAssetStatus::Available => {}
        }

        record_pdf_index_attempt(asset.item_id, asset.asset_id);
        let path = match self.resolve_document_asset_path(asset.item_id, asset.asset_id) {
            Ok(path) => path,
            Err(_) => {
                return AssetIndexOutcome::issue(
                    base(AssetContentIndexState::Missing, None, None),
                    "Vault PDF could not be resolved",
                );
            }
        };
        let file_size = match fs::metadata(&path) {
            Ok(metadata) => metadata.len(),
            Err(_) => {
                return AssetIndexOutcome::issue(
                    base(AssetContentIndexState::Missing, None, None),
                    "Vault PDF is missing",
                );
            }
        };
        if file_size > PDF_MAX_FILE_SIZE_BYTES {
            return AssetIndexOutcome::issue(
                base(AssetContentIndexState::LimitExceeded, None, None),
                "PDF file size exceeds the M2 extraction limit",
            );
        }
        record_pdf_hash_attempt(asset.item_id, asset.asset_id);
        let content_hash = match sha256_file(&path) {
            Ok(hash) => hash,
            Err(_) => {
                return AssetIndexOutcome::issue(
                    base(AssetContentIndexState::ParseFailed, None, None),
                    "Vault PDF could not be hashed",
                );
            }
        };
        if let Some(previous) = previous
            && previous.item_id == asset.item_id
            && previous.asset_id == asset.asset_id
            && previous.content_hash.as_deref() == Some(content_hash.as_str())
            && previous.extractor_version == PDF_TEXT_EXTRACTOR_VERSION
            && previous.analyzer_version == CONTENT_ANALYZER_VERSION
            && previous.state.is_reusable_success()
        {
            return AssetIndexOutcome::ok(AssetContentIndexRecord {
                metadata_fingerprint: metadata_fingerprint.to_string(),
                ..previous.clone()
            });
        }

        match extract_pdf_text_layer(&path) {
            PdfTextExtraction::Indexed(text) => AssetIndexOutcome::ok(base(
                AssetContentIndexState::Indexed,
                Some(text),
                Some(content_hash),
            )),
            PdfTextExtraction::NoText => AssetIndexOutcome::issue(
                base(AssetContentIndexState::NoText, None, Some(content_hash)),
                "PDF has no extractable text layer",
            ),
            PdfTextExtraction::Encrypted => AssetIndexOutcome::issue(
                base(AssetContentIndexState::Encrypted, None, Some(content_hash)),
                "PDF is encrypted",
            ),
            PdfTextExtraction::LimitExceeded(detail) => AssetIndexOutcome::issue(
                base(
                    AssetContentIndexState::LimitExceeded,
                    None,
                    Some(content_hash),
                ),
                detail,
            ),
            PdfTextExtraction::ParseFailed => AssetIndexOutcome::issue(
                base(
                    AssetContentIndexState::ParseFailed,
                    None,
                    Some(content_hash),
                ),
                "PDF text layer could not be parsed",
            ),
        }
    }

    fn search_index_root(&self) -> PathBuf {
        self.root_path().join("derived").join("search")
    }

    fn search_generation_path(&self, generation_id: Uuid) -> PathBuf {
        self.search_index_root()
            .join("generations")
            .join(generation_id.to_string())
    }

    fn prepare_search_root(&self) -> Result<PathBuf> {
        // This preflight deliberately happens before creating even a missing
        // `derived/`, `search/`, `working/`, or `generations/` component. A
        // rebuild must reject an unsafe pre-existing generation rather than
        // publish a new manifest that merely routes around it.
        self.ensure_existing_search_index_layout_is_safe()?;

        let derived = self.root_path().join("derived");
        ensure_directory_component_is_owned(&derived)?;
        let root = derived.join("search");
        ensure_directory_component_is_owned(&root)?;
        self.ensure_search_root_boundary_is_safe(&root)?;
        for directory in [root.join("working"), root.join("generations")] {
            ensure_directory_component_is_owned(&directory)?;
        }
        self.ensure_search_layout_is_safe(&root, None)?;
        // Repeat the whole no-follow scan after filling in absent expected
        // directories. This is also the last gate before the caller can
        // delete a failure marker or create its fresh working generation.
        self.ensure_existing_search_index_layout_is_safe()?;
        Ok(root)
    }

    /// Validates every *existing* index-owned object that a rebuild could
    /// otherwise bypass. Missing index components remain normal for a fresh
    /// Vault, but an existing component must be an owned directory/file and is
    /// never followed through a symlink, junction, or Windows reparse point.
    ///
    /// In particular this scans both `working/*` and `generations/*`; an
    /// unsafe inactive generation is still an unsafe index layout, not an
    /// object a later manifest publication may silently abandon.
    fn ensure_existing_search_index_layout_is_safe(&self) -> Result<()> {
        let derived = self.root_path().join("derived");
        if !ensure_owned_directory_if_present(&derived)? {
            return Ok(());
        }

        let root = derived.join("search");
        if !ensure_owned_directory_if_present(&root)? {
            return Ok(());
        }

        self.ensure_search_root_index_files_are_safe_for_write(&root)?;
        for directory in [root.join("working"), root.join("generations")] {
            if ensure_owned_directory_if_present(&directory)? {
                self.ensure_existing_search_generation_entries_are_safe(&directory)?;
            }
        }
        Ok(())
    }

    fn ensure_search_root_index_files_are_safe_for_write(&self, root: &Path) -> Result<()> {
        for file_name in ["manifest.json", "failure.json"] {
            ensure_owned_index_file_if_present(&root.join(file_name))?;
        }
        Ok(())
    }

    fn ensure_existing_search_generation_entries_are_safe(&self, directory: &Path) -> Result<()> {
        ensure_existing_owned_directory(directory)?;
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let generation = entry.path();
            ensure_existing_owned_directory(&generation)?;
            ensure_owned_index_file_if_present(&generation.join("metadata.json"))?;
        }
        Ok(())
    }

    /// Returns whether an owned working generation exists. Every entry below
    /// `working/` is expected to be a real directory owned by this subsystem;
    /// symlinks, junctions, other reparse points, and stray files are unsafe
    /// rather than valid `Building` evidence.
    fn has_incomplete_search_working_generation(&self, root: &Path) -> Result<bool> {
        self.ensure_search_layout_is_safe(root, None)?;
        self.ensure_existing_search_generation_entries_are_safe(&root.join("working"))?;
        Ok(fs::read_dir(root.join("working"))?
            .next()
            .transpose()?
            .is_some())
    }

    fn ensure_existing_search_root_is_safe(&self) -> Result<()> {
        let root = self.search_index_root();
        self.ensure_search_layout_is_safe(&root, None)
    }

    /// Validates only the pre-existing boundary components required before
    /// creating `working/` and `generations/` for a new index root.
    fn ensure_search_root_boundary_is_safe(&self, root: &Path) -> Result<()> {
        ensure_existing_owned_directory(&self.root_path().join("derived"))?;
        ensure_existing_owned_directory(root)
    }

    /// Verifies the exact, expected search-directory chain without resolving
    /// links. `canonicalize + starts_with` is intentionally not used here:
    /// a link to `user/` still sits inside the Vault but is never an owned
    /// derived directory.
    fn ensure_search_layout_is_safe(&self, root: &Path, generation: Option<&Path>) -> Result<()> {
        let derived = self.root_path().join("derived");
        ensure_existing_owned_directory(&derived)?;
        ensure_existing_owned_directory(root)?;
        ensure_existing_owned_directory(&root.join("working"))?;
        ensure_existing_owned_directory(&root.join("generations"))?;
        if let Some(generation) = generation {
            if generation.parent() != Some(root.join("working").as_path())
                && generation.parent() != Some(root.join("generations").as_path())
            {
                return Err(CoreError::InvalidSearchIndexPath);
            }
            ensure_existing_owned_directory(generation)?;
        }
        Ok(())
    }

    fn ensure_generation_slot_is_safe(&self, root: &Path, generation: &Path) -> Result<()> {
        self.ensure_search_layout_is_safe(root, None)?;
        if generation.exists() {
            return Err(CoreError::InvalidSearchIndexPath);
        }
        Ok(())
    }
}

/// Creates one expected directory only after its existing path is confirmed to
/// be a real directory rather than a symlink, Windows junction, or any other
/// reparse point. Creating one component at a time avoids `create_dir_all`
/// following an unvalidated `derived/` chain.
fn ensure_directory_component_is_owned(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => ensure_owned_directory_metadata(&metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path.parent().ok_or(CoreError::InvalidSearchIndexPath)?;
            if !parent.exists() {
                return Err(CoreError::InvalidSearchIndexPath);
            }
            ensure_existing_owned_directory(parent)?;
            match fs::create_dir(path) {
                Ok(()) => ensure_existing_owned_directory(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    ensure_existing_owned_directory(path)
                }
                Err(error) => Err(error.into()),
            }
        }
        Err(error) => Err(error.into()),
    }
}

fn ensure_existing_owned_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    ensure_owned_directory_metadata(&metadata)
}

/// Validates an optional owned directory without following a dangling link.
/// This is for status checks where ordinary absence means `Missing`, while any
/// existing link/reparse/non-directory is an unsafe, degraded layout.
fn ensure_owned_directory_if_present(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            ensure_owned_directory_metadata(&metadata)?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn degraded_search_layout_state() -> SearchIndexState {
    SearchIndexState {
        status: SearchIndexStatus::Degraded,
        active_generation: None,
        detail: Some("derived search directory has an unsafe link or reparse layout".to_string()),
    }
}

/// Confirms an already-present index-owned JSON file is a normal file without
/// resolving a symlink or Windows reparse point. This is intentionally separate
/// from directory validation: a safe directory chain alone does not make its
/// contained files safe to read.
fn ensure_existing_owned_regular_file(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || is_reparse_point(&metadata)
    {
        return Err(CoreError::InvalidSearchIndexPath);
    }
    Ok(())
}

/// Validates an optional index-owned file without treating absence as an error.
/// Callers use this before status branching so an unsafe `manifest.json` or
/// `failure.json` cannot be mistaken for ordinary missing/corrupt state.
fn ensure_owned_index_file_if_present(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_file()
                || metadata.file_type().is_symlink()
                || is_reparse_point(&metadata)
            {
                return Err(CoreError::InvalidSearchIndexPath);
            }
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// Deletes an optional index-owned file only after verifying the entry itself
/// is an owned normal file. In particular this may not be used to "repair" a
/// symlink/junction/reparse object that redirects outside `derived/search/`.
fn remove_owned_index_file_if_present(path: &Path) -> Result<()> {
    if ensure_owned_index_file_if_present(path)? {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn ensure_owned_directory_metadata(metadata: &fs::Metadata) -> Result<()> {
    if !metadata.file_type().is_dir()
        || metadata.file_type().is_symlink()
        || is_reparse_point(metadata)
    {
        return Err(CoreError::InvalidSearchIndexPath);
    }
    Ok(())
}

#[cfg(windows)]
fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_: &fs::Metadata) -> bool {
    false
}

/// M1's fixed analyzer uses Unicode NFKC followed by lowercase conversion,
/// alphanumeric word boundaries, and overlapping bigrams for contiguous CJK
/// runs. The two-character window is a deliberately small, deterministic
/// baseline; ranking and query execution remain M3 work.
fn analyzed_metadata_field(field: SearchMatchField, value: &str) -> MetadataAnalyzedField {
    let normalized = value.nfkc().collect::<String>().to_lowercase();
    let characters = normalized.chars().collect::<Vec<_>>();
    let mut terms = Vec::new();
    let mut word = String::new();
    let mut cjk_run = Vec::new();

    let flush_word = |word: &mut String, terms: &mut Vec<String>| {
        if !word.is_empty() {
            push_unique_term(terms, std::mem::take(word));
        }
    };
    let flush_cjk = |run: &mut Vec<char>, terms: &mut Vec<String>| {
        if run.is_empty() {
            return;
        }
        if run.len() == 1 {
            push_unique_term(terms, run[0].to_string());
        } else {
            for pair in run.windows(2) {
                push_unique_term(terms, pair.iter().collect());
            }
        }
        run.clear();
    };

    for character in characters {
        if is_cjk(character) {
            flush_word(&mut word, &mut terms);
            cjk_run.push(character);
        } else if character.is_alphanumeric() {
            flush_cjk(&mut cjk_run, &mut terms);
            word.push(character);
        } else {
            flush_word(&mut word, &mut terms);
            flush_cjk(&mut cjk_run, &mut terms);
        }
    }
    flush_word(&mut word, &mut terms);
    flush_cjk(&mut cjk_run, &mut terms);

    MetadataAnalyzedField { field, terms }
}

fn push_unique_term(terms: &mut Vec<String>, term: String) {
    if !term.is_empty() && !terms.iter().any(|existing| existing == &term) {
        terms.push(term);
    }
}

fn is_cjk(character: char) -> bool {
    matches!(
        character as u32,
        0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xf900..=0xfaff | 0x20000..=0x2fa1f
            | 0x3040..=0x30ff | 0xac00..=0xd7af
    )
}

fn normalized_query_scopes(scopes: &[SearchFieldScope]) -> BTreeSet<SearchFieldScope> {
    if scopes.is_empty() || scopes.contains(&SearchFieldScope::All) {
        return BTreeSet::from([SearchFieldScope::All]);
    }
    scopes.iter().copied().collect()
}

fn scope_includes_match_field(
    scopes: &BTreeSet<SearchFieldScope>,
    field: SearchMatchField,
) -> bool {
    if scopes.contains(&SearchFieldScope::All) {
        return true;
    }
    match field {
        SearchMatchField::Title => {
            scopes.contains(&SearchFieldScope::Metadata)
                || scopes.contains(&SearchFieldScope::Title)
        }
        SearchMatchField::Authors => {
            scopes.contains(&SearchFieldScope::Metadata)
                || scopes.contains(&SearchFieldScope::Authors)
        }
        SearchMatchField::Tags => {
            scopes.contains(&SearchFieldScope::Metadata) || scopes.contains(&SearchFieldScope::Tags)
        }
        SearchMatchField::Content => scopes.contains(&SearchFieldScope::Content),
    }
}

fn matched_query_terms(query_terms: &[String], indexed_terms: &[String]) -> Vec<String> {
    query_terms
        .iter()
        .filter(|term| indexed_terms.iter().any(|indexed| indexed == *term))
        .cloned()
        .collect()
}

fn search_match_field_sort_key(field: SearchMatchField) -> u8 {
    match field {
        SearchMatchField::Title => 0,
        SearchMatchField::Authors => 1,
        SearchMatchField::Tags => 2,
        SearchMatchField::Content => 3,
    }
}

fn search_excerpt(content: &str, query_terms: &[String]) -> String {
    let normalized = content.nfkc().collect::<String>().to_lowercase();
    let anchor = query_terms
        .iter()
        .filter_map(|term| normalized.find(term))
        .min()
        .unwrap_or(0);
    let chars = content.chars().collect::<Vec<_>>();
    let start = anchor.saturating_sub(SEARCH_EXCERPT_MAX_CHARS / 3);
    let end = (start + SEARCH_EXCERPT_MAX_CHARS).min(chars.len());
    let prefix = if start > 0 { "…" } else { "" };
    let suffix = if end < chars.len() { "…" } else { "" };
    format!(
        "{prefix}{}{suffix}",
        chars[start..end].iter().collect::<String>()
    )
}

struct AssetIndexOutcome {
    record: AssetContentIndexRecord,
    detail: Option<String>,
}

impl AssetIndexOutcome {
    fn ok(record: AssetContentIndexRecord) -> Self {
        Self {
            record,
            detail: None,
        }
    }

    fn issue(record: AssetContentIndexRecord, detail: impl Into<String>) -> Self {
        Self {
            record,
            detail: Some(detail.into()),
        }
    }
}

enum PdfTextExtraction {
    Indexed(String),
    NoText,
    Encrypted,
    LimitExceeded(String),
    ParseFailed,
}

/// Extracts only a PDF text layer, with deterministic per-file, page, stream,
/// text, and elapsed-time limits. `lopdf` is a pure Rust MIT-licensed parser;
/// no helper process, network request, OCR, or external service is involved.
fn extract_pdf_text_layer(path: &Path) -> PdfTextExtraction {
    let started = Instant::now();
    let document = match Document::load(path) {
        Ok(document) => document,
        Err(_) => return PdfTextExtraction::ParseFailed,
    };
    if document.is_encrypted() {
        return PdfTextExtraction::Encrypted;
    }
    let pages = document.get_pages();
    if pages.len() > PDF_MAX_PAGES {
        return PdfTextExtraction::LimitExceeded(
            "PDF page count exceeds the M2 extraction limit".to_string(),
        );
    }

    let mut text = String::new();
    let page_numbers = pages.keys().copied().collect::<Vec<_>>();
    for chunk in page_numbers.chunks(PDF_EXTRACTION_BATCH_SIZE) {
        for fragment in
            document.extract_text_chunks_with_limit(chunk, PDF_MAX_DECOMPRESSED_BYTES_PER_PAGE)
        {
            let fragment = match fragment {
                Ok(fragment) => fragment,
                Err(lopdf::Error::Decompress(lopdf::DecompressError::MemoryLimitExceeded {
                    ..
                })) => {
                    return PdfTextExtraction::LimitExceeded(
                        "PDF decompressed page content exceeds the M2 extraction limit".to_string(),
                    );
                }
                Err(_) => return PdfTextExtraction::ParseFailed,
            };
            if text
                .chars()
                .count()
                .saturating_add(fragment.chars().count())
                > PDF_MAX_EXTRACTED_CHARS
            {
                return PdfTextExtraction::LimitExceeded(
                    "PDF extracted text exceeds the M2 extraction limit".to_string(),
                );
            }
            text.push_str(&fragment);
            if started.elapsed() > PDF_MAX_EXTRACTION_DURATION {
                return PdfTextExtraction::LimitExceeded(
                    "PDF extraction exceeds the M2 duration limit".to_string(),
                );
            }
        }
    }
    let text = text.trim().to_string();
    if text.is_empty() {
        PdfTextExtraction::NoText
    } else {
        PdfTextExtraction::Indexed(text)
    }
}

#[cfg(test)]
thread_local! {
    static PDF_INDEX_ATTEMPTS: std::cell::RefCell<Vec<(Uuid, Uuid)>> = const { std::cell::RefCell::new(Vec::new()) };
    static PDF_HASH_ATTEMPTS: std::cell::RefCell<Vec<(Uuid, Uuid)>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
fn clear_pdf_index_attempts() {
    PDF_INDEX_ATTEMPTS.with(|attempts| attempts.borrow_mut().clear());
}

#[cfg(test)]
fn take_pdf_index_attempts() -> Vec<(Uuid, Uuid)> {
    PDF_INDEX_ATTEMPTS.with(|attempts| std::mem::take(&mut *attempts.borrow_mut()))
}

#[cfg(test)]
fn record_pdf_index_attempt(item_id: Uuid, asset_id: Uuid) {
    PDF_INDEX_ATTEMPTS.with(|attempts| attempts.borrow_mut().push((item_id, asset_id)));
}

#[cfg(not(test))]
fn record_pdf_index_attempt(_: Uuid, _: Uuid) {}

#[cfg(test)]
fn clear_pdf_hash_attempts() {
    PDF_HASH_ATTEMPTS.with(|attempts| attempts.borrow_mut().clear());
}

#[cfg(test)]
fn take_pdf_hash_attempts() -> Vec<(Uuid, Uuid)> {
    PDF_HASH_ATTEMPTS.with(|attempts| std::mem::take(&mut *attempts.borrow_mut()))
}

#[cfg(test)]
fn record_pdf_hash_attempt(item_id: Uuid, asset_id: Uuid) {
    PDF_HASH_ATTEMPTS.with(|attempts| attempts.borrow_mut().push((item_id, asset_id)));
}

#[cfg(not(test))]
fn record_pdf_hash_attempt(_: Uuid, _: Uuid) {}

fn sha256_file(path: &Path) -> Result<String> {
    use std::io::Read;

    let mut file = fs::File::open(path)?;
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

fn metadata_fingerprint(title: &str, authors: &[String], tags: &[String]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(title.as_bytes());
    hasher.update([0]);
    for author in authors {
        hasher.update(author.as_bytes());
        hasher.update([0]);
    }
    hasher.update([0xff]);
    for tag in tags {
        hasher.update(tag.as_bytes());
        hasher.update([0]);
    }
    format!("{:x}", hasher.finalize())
}

fn read_owned_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    ensure_existing_owned_regular_file(path)?;
    let payload = fs::read(path)?;
    Ok(serde_json::from_slice(&payload)?)
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T, prefix: &str) -> Result<()> {
    let payload = serde_json::to_vec_pretty(value)?;
    write_atomic(path, &payload, prefix)
}

fn write_atomic(path: &Path, payload: &[u8], prefix: &str) -> Result<()> {
    let parent = path.parent().ok_or(CoreError::InvalidSearchIndexPath)?;
    ensure_existing_owned_directory(parent)?;
    // Revalidate the existing destination immediately before the atomic
    // replacement. `rename` can otherwise replace a file symlink without
    // following it, silently normalising a layout we must instead reject.
    ensure_owned_index_file_if_present(path)?;
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
    let result = replace_file(&temp, path);
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(not(windows))]
fn replace_file(temp: &Path, target: &Path) -> Result<()> {
    fs::rename(temp, target)?;
    Ok(())
}

#[cfg(windows)]
fn replace_file(temp: &Path, target: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;

    let to_wide = |path: &Path| {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<u16>>()
    };
    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }
    let source = to_wide(temp);
    let destination = to_wide(target);
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } != 0
    {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        DocumentAssetKind, LiteratureItemDraft, LiteratureItemType, ReadingStatus, VaultOpenOptions,
    };
    use lopdf::{
        Object, Stream,
        content::{Content, Operation},
        dictionary,
    };
    use std::collections::BTreeMap;

    fn temp_dir(prefix: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("{prefix}-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn write_vault(root: &Path) -> Vault {
        let sources = root.join("sources.parquet");
        fs::write(&sources, b"").unwrap();
        Vault::open_sources_file(sources, VaultOpenOptions::default()).unwrap()
    }

    fn draft(title: &str) -> LiteratureItemDraft {
        LiteratureItemDraft {
            title: title.to_string(),
            authors: vec!["Ada Lovelace".to_string()],
            published_year: Some(1843),
            item_type: LiteratureItemType::Article,
            favorite: false,
            reading_status: ReadingStatus::Inbox,
            tags: vec!["算法".to_string(), "notes".to_string()],
            sources: Vec::new(),
            external_identifiers: Vec::new(),
        }
    }

    fn build_test_pdf(text: Option<&str>) -> Document {
        let mut document = Document::with_version("1.5");
        let pages_id = document.new_object_id();
        let font_id = document.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
            "Encoding" => "WinAnsiEncoding",
        });
        let resources_id = document.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        });
        let operations = text.map_or_else(Vec::new, |text| {
            vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 12.into()]),
                Operation::new("Td", vec![50.into(), 700.into()]),
                Operation::new("Tj", vec![Object::string_literal(text)]),
                Operation::new("ET", vec![]),
            ]
        });
        let contents_id = document.add_object(Stream::new(
            dictionary! {},
            Content { operations }.encode().unwrap(),
        ));
        let page_id = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Resources" => resources_id,
            "Contents" => contents_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        });
        document.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![page_id.into()],
                "Count" => 1,
            }),
        );
        let catalog_id = document.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        document.trailer.set("Root", catalog_id);
        document
    }

    fn write_test_pdf(path: &Path, text: Option<&str>) {
        build_test_pdf(text).save(path).unwrap();
    }

    fn write_encrypted_test_pdf(path: &Path) {
        let mut document = build_test_pdf(Some("locked text"));
        document.trailer.set(
            "ID",
            Object::Array(vec![
                Object::String(vec![1_u8; 16], lopdf::StringFormat::Literal),
                Object::String(vec![2_u8; 16], lopdf::StringFormat::Literal),
            ]),
        );
        let encryption_version = lopdf::EncryptionVersion::V2 {
            document: &document,
            owner_password: "owner",
            user_password: "user",
            key_length: 128,
            permissions: lopdf::Permissions::all(),
        };
        let encryption_state = lopdf::EncryptionState::try_from(encryption_version).unwrap();
        document.encrypt(&encryption_state).unwrap();
        document.save(path).unwrap();
    }

    fn import_test_pdf(vault: &Vault, item_id: Uuid, source: &Path) -> crate::DocumentAsset {
        match vault
            .import_document_asset(item_id, source, DocumentAssetKind::Primary)
            .unwrap()
        {
            crate::DocumentAssetImportResult::Imported { asset } => asset,
            crate::DocumentAssetImportResult::Duplicate { .. } => panic!("unexpected duplicate"),
        }
    }

    fn copy_tree(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let destination = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_tree(&entry.path(), &destination);
            } else {
                fs::copy(entry.path(), destination).unwrap();
            }
        }
    }

    fn snapshot_non_derived_files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        fn visit(root: &Path, current: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in fs::read_dir(current).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                let relative = path.strip_prefix(root).unwrap().to_path_buf();
                if relative == Path::new("derived") {
                    continue;
                }
                if entry.file_type().unwrap().is_dir() {
                    visit(root, &path, files);
                } else {
                    files.insert(relative, fs::read(path).unwrap());
                }
            }
        }

        let mut files = BTreeMap::new();
        visit(root, root, &mut files);
        files
    }

    fn snapshot_entire_vault(
        root: &Path,
    ) -> BTreeMap<PathBuf, (bool, bool, bool, Option<Vec<u8>>)> {
        fn visit(
            root: &Path,
            current: &Path,
            entries: &mut BTreeMap<PathBuf, (bool, bool, bool, Option<Vec<u8>>)>,
        ) {
            for entry in fs::read_dir(current).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                let metadata = fs::symlink_metadata(&path).unwrap();
                let file_type = metadata.file_type();
                let is_reparse = is_reparse_point(&metadata);
                let relative = path.strip_prefix(root).unwrap().to_path_buf();
                let bytes = if file_type.is_file() && !file_type.is_symlink() && !is_reparse {
                    Some(fs::read(&path).unwrap())
                } else {
                    None
                };
                entries.insert(
                    relative,
                    (
                        file_type.is_dir(),
                        file_type.is_symlink(),
                        is_reparse,
                        bytes,
                    ),
                );
                if file_type.is_dir() && !file_type.is_symlink() && !is_reparse {
                    visit(root, &path, entries);
                }
            }
        }

        let mut entries = BTreeMap::new();
        visit(root, root, &mut entries);
        entries
    }

    #[cfg(windows)]
    fn make_junction(link: &Path, target: &Path) {
        use std::process::Command;

        let output = Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "failed to create test junction {} -> {}: {}",
            link.display(),
            target.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(windows)]
    fn remove_junction(link: &Path) {
        use std::process::Command;

        let output = Command::new("cmd")
            .args(["/C", "rmdir"])
            .arg(link)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "failed to remove test junction {}",
            link.display()
        );
    }

    #[cfg(windows)]
    fn make_file_symlink(link: &Path, target: &Path) {
        std::os::windows::fs::symlink_file(target, link).unwrap_or_else(|error| {
            panic!(
                "failed to create test file link {} -> {}: {error}",
                link.display(),
                target.display()
            )
        });
    }

    #[cfg(windows)]
    fn make_directory_symlink(link: &Path, target: &Path) {
        std::os::windows::fs::symlink_dir(target, link).unwrap_or_else(|error| {
            panic!(
                "failed to create test directory link {} -> {}: {error}",
                link.display(),
                target.display()
            )
        });
    }

    #[test]
    fn metadata_generation_only_writes_derived_search_and_keeps_id_only_records() {
        let root = temp_dir("cistella-search-m1-boundary");
        let vault = write_vault(&root);
        let item = vault
            .create_literature_item(draft("可检索的元数据"))
            .unwrap();
        let authority_before = snapshot_non_derived_files(&root);

        let state = vault.rebuild_metadata_search_index().unwrap();
        let snapshot = vault.load_search_index_generation().unwrap();
        assert_eq!(state.status, SearchIndexStatus::Ready);
        assert_eq!(snapshot.metadata_records.len(), 1);
        assert_eq!(snapshot.metadata_records[0].item_id, item.item_id);
        assert!(snapshot.asset_content_records.is_empty());
        assert_eq!(snapshot_non_derived_files(&root), authority_before);
        let derived_payload =
            fs::read_to_string(root.join(DERIVED_SEARCH_RELATIVE_DIR).join("manifest.json"))
                .unwrap();
        assert!(!derived_payload.contains(&root.to_string_lossy().to_string()));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn m2_indexes_vault_pdf_and_records_external_missing_and_parse_failures() {
        let root = temp_dir("cistella-search-m2-assets");
        let incoming = temp_dir("cistella-search-m2-incoming");
        let vault = write_vault(&root);
        let item = vault
            .create_literature_item(draft("Asset boundaries"))
            .unwrap();
        let ready_pdf = incoming.join("ready.pdf");
        let external_pdf = incoming.join("external.pdf");
        let gone_pdf = incoming.join("gone.pdf");
        let broken_pdf = incoming.join("broken.pdf");
        write_test_pdf(&ready_pdf, Some("M2 searchable text"));
        write_test_pdf(&external_pdf, Some("outside vault"));
        write_test_pdf(&gone_pdf, Some("soon missing"));
        write_test_pdf(&broken_pdf, Some("will be corrupted"));
        let ready = import_test_pdf(&vault, item.item_id, &ready_pdf);
        let external = vault
            .link_external_document_asset(
                item.item_id,
                &external_pdf,
                DocumentAssetKind::Supplement,
            )
            .unwrap();
        let gone = import_test_pdf(&vault, item.item_id, &gone_pdf);
        let broken = import_test_pdf(&vault, item.item_id, &broken_pdf);
        fs::remove_file(
            vault
                .resolve_document_asset_path(item.item_id, gone.asset_id)
                .unwrap(),
        )
        .unwrap();
        fs::write(
            vault
                .resolve_document_asset_path(item.item_id, broken.asset_id)
                .unwrap(),
            b"not a PDF",
        )
        .unwrap();

        vault.rebuild_metadata_search_index().unwrap();
        let snapshot = vault.load_search_index_generation().unwrap();
        let states = snapshot
            .asset_content_records
            .iter()
            .map(|record| (record.asset_id, (record.state, record.content.clone())))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(states[&ready.asset_id].0, AssetContentIndexState::Indexed);
        assert!(
            states[&ready.asset_id]
                .1
                .as_deref()
                .unwrap()
                .contains("M2 searchable text")
        );
        assert_eq!(
            states[&external.asset_id].0,
            AssetContentIndexState::ExternalUnavailable
        );
        assert_eq!(states[&gone.asset_id].0, AssetContentIndexState::Missing);
        assert_eq!(
            states[&broken.asset_id].0,
            AssetContentIndexState::ParseFailed
        );
        assert!(
            snapshot
                .issues
                .iter()
                .all(|issue| issue.asset_id != ready.asset_id)
        );
        assert_eq!(snapshot.issues.len(), 3);
        let payload = serde_json::to_string(&snapshot).unwrap();
        assert!(!payload.contains(&root.to_string_lossy().to_string()));
        assert!(!payload.contains(&incoming.to_string_lossy().to_string()));
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(incoming);
    }

    #[test]
    fn m4_commit_failure_cleans_working_and_keeps_old_ready_generation_queryable() {
        let root = temp_dir("cistella-search-m4-atomic");
        let vault = write_vault(&root);
        let item = vault
            .create_literature_item(draft("Atomic generation"))
            .unwrap();
        let query = SearchQuery {
            text: "atomic generation".to_string(),
            scopes: vec![SearchFieldScope::Title],
        };
        let ready = vault.rebuild_metadata_search_index().unwrap();
        let ready_result = vault.query_search_index(&query, 0, 100).unwrap();
        let error = vault
            .rebuild_metadata_search_index_with(|working| {
                assert!(working.exists());
                Err(CoreError::SearchIndexBuildFailed(
                    "injected failure".to_string(),
                ))
            })
            .unwrap_err();
        assert!(matches!(error, CoreError::SearchIndexBuildFailed(_)));
        assert_eq!(vault.search_index_state(), ready);
        assert_eq!(
            vault.query_search_index(&query, 0, 100).unwrap(),
            ready_result
        );
        let working = root.join(DERIVED_SEARCH_RELATIVE_DIR).join("working");
        assert!(fs::read_dir(working).unwrap().next().is_none());
        assert_eq!(
            vault
                .load_search_index_generation()
                .unwrap()
                .metadata_records
                .len(),
            1
        );
        assert_eq!(
            match ready_result {
                SearchQueryResult::Ready { page } => page.hits[0].item_id,
                SearchQueryResult::Unavailable { .. } =>
                    panic!("ready generation became unavailable"),
            },
            item.item_id
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn m4_first_commit_failure_is_failed_without_blocking_authoritative_vault_data() {
        let root = temp_dir("cistella-search-m4-failed");
        let vault = write_vault(&root);
        let item = vault
            .create_literature_item(draft("Failed initial build"))
            .unwrap();
        let error = vault
            .rebuild_metadata_search_index_with(|_| {
                Err(CoreError::SearchIndexBuildFailed(
                    "injected failure".to_string(),
                ))
            })
            .unwrap_err();
        assert!(matches!(error, CoreError::SearchIndexBuildFailed(_)));
        assert_eq!(vault.search_index_state().status, SearchIndexStatus::Failed);
        assert!(matches!(
            vault
                .query_search_index(
                    &SearchQuery {
                        text: "failed initial build".to_string(),
                        scopes: vec![SearchFieldScope::Title],
                    },
                    0,
                    100,
                )
                .unwrap(),
            SearchQueryResult::Unavailable { index_state }
                if index_state.status == SearchIndexStatus::Failed
        ));
        assert_eq!(
            vault.load_literature_items().unwrap()[0].item_id,
            item.item_id
        );
        assert!(
            root.join(DERIVED_SEARCH_RELATIVE_DIR)
                .join("failure.json")
                .is_file()
        );
        assert_eq!(
            vault.rebuild_metadata_search_index().unwrap().status,
            SearchIndexStatus::Ready
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn incompatible_manifest_is_stale_and_rebuild_restores_ready_generation() {
        let root = temp_dir("cistella-search-m1-stale");
        let vault = write_vault(&root);
        vault
            .create_literature_item(draft("Stale generation"))
            .unwrap();
        vault.rebuild_metadata_search_index().unwrap();
        let manifest = root.join(DERIVED_SEARCH_RELATIVE_DIR).join("manifest.json");
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
        value["formatVersion"] = serde_json::json!(SEARCH_INDEX_FORMAT_VERSION + 1);
        fs::write(&manifest, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        assert_eq!(vault.search_index_state().status, SearchIndexStatus::Stale);
        assert!(matches!(
            vault.load_search_index_generation(),
            Err(CoreError::SearchIndexUnavailable(_))
        ));
        assert_eq!(
            vault.rebuild_metadata_search_index().unwrap().status,
            SearchIndexStatus::Ready
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn generation_analyzer_or_extractor_version_drift_is_stale_before_loading() {
        let root = temp_dir("cistella-search-m1-generation-version-drift");
        let vault = write_vault(&root);
        vault
            .create_literature_item(draft("Version drift"))
            .unwrap();

        for field in ["metadataAnalyzerVersion", "pdfTextExtractorVersion"] {
            vault.rebuild_metadata_search_index().unwrap();
            let generation_id = vault.search_index_state().active_generation.unwrap();
            let metadata_path = root
                .join(DERIVED_SEARCH_RELATIVE_DIR)
                .join("generations")
                .join(generation_id.to_string())
                .join("metadata.json");
            let mut value: serde_json::Value =
                serde_json::from_slice(&fs::read(&metadata_path).unwrap()).unwrap();
            value[field] = serde_json::json!("incompatible-test-version");
            fs::write(&metadata_path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();

            assert_eq!(vault.search_index_state().status, SearchIndexStatus::Stale);
            assert!(matches!(
                vault.load_search_index_generation(),
                Err(CoreError::SearchIndexUnavailable(_))
            ));
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn metadata_analysis_is_unicode_stable_field_scoped_and_path_free() {
        let root = temp_dir("cistella-search-m1-metadata-analysis");
        let vault = write_vault(&root);
        let item = vault
            .create_literature_item(LiteratureItemDraft {
                title: "ＦＯＯ Rust 与机器学习".to_string(),
                authors: vec!["ÁLICE Müller".to_string(), "张伟".to_string()],
                published_year: Some(2026),
                item_type: LiteratureItemType::Article,
                favorite: false,
                reading_status: ReadingStatus::Inbox,
                tags: vec!["Data-Science".to_string(), "知识图谱".to_string()],
                sources: Vec::new(),
                external_identifiers: Vec::new(),
            })
            .unwrap();
        vault.rebuild_metadata_search_index().unwrap();
        let snapshot = vault.load_search_index_generation().unwrap();
        let record = snapshot
            .metadata_records
            .iter()
            .find(|record| record.item_id == item.item_id)
            .unwrap();

        assert_eq!(record.title, "ＦＯＯ Rust 与机器学习");
        assert_eq!(record.authors, vec!["ÁLICE Müller", "张伟"]);
        assert_eq!(record.tags, vec!["Data-Science", "知识图谱"]);
        assert_eq!(
            record.analyzed_fields,
            vec![
                MetadataAnalyzedField {
                    field: SearchMatchField::Title,
                    terms: vec![
                        "foo".to_string(),
                        "rust".to_string(),
                        "与机".to_string(),
                        "机器".to_string(),
                        "器学".to_string(),
                        "学习".to_string(),
                    ],
                },
                MetadataAnalyzedField {
                    field: SearchMatchField::Authors,
                    terms: vec![
                        "álice".to_string(),
                        "müller".to_string(),
                        "张伟".to_string(),
                    ],
                },
                MetadataAnalyzedField {
                    field: SearchMatchField::Tags,
                    terms: vec![
                        "data".to_string(),
                        "science".to_string(),
                        "知识".to_string(),
                        "识图".to_string(),
                        "图谱".to_string(),
                    ],
                },
            ]
        );
        assert_eq!(
            analyzed_metadata_field(SearchMatchField::Title, &record.title),
            record.analyzed_fields[0]
        );

        let first_asset = Uuid::new_v4();
        let second_asset = Uuid::new_v4();
        let hit = SearchHit {
            item_id: item.item_id,
            field_matches: vec![
                SearchFieldMatch {
                    field: SearchMatchField::Content,
                    matched_terms: vec!["机器".to_string()],
                    asset_id: Some(first_asset),
                    asset_state: Some(AssetContentIndexState::Indexed),
                    excerpt: Some("derived excerpt".to_string()),
                },
                SearchFieldMatch {
                    field: SearchMatchField::Content,
                    matched_terms: vec!["学习".to_string()],
                    asset_id: Some(second_asset),
                    asset_state: Some(AssetContentIndexState::Indexed),
                    excerpt: Some("second derived excerpt".to_string()),
                },
            ],
        };
        let query = SearchQuery {
            text: "机器 学习".to_string(),
            scopes: vec![SearchFieldScope::Title, SearchFieldScope::Content],
        };
        let issue = SearchIndexIssue {
            item_id: item.item_id,
            asset_id: first_asset,
            kind: SearchIndexIssueKind::Cancelled,
            detail: None,
        };
        let task = SearchIndexTaskState {
            status: SearchIndexTaskStatus::Idle,
            generation_id: None,
            detail: None,
        };
        assert_eq!(hit.item_id, item.item_id);
        assert_eq!(hit.field_matches.len(), 2);
        let payload = serde_json::to_string(&(snapshot, hit, query, issue, task)).unwrap();
        assert!(!payload.contains(&root.to_string_lossy().to_string()));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn dangling_derived_directory_links_are_degraded_without_reading_or_writing_targets() {
        for (label, link) in [
            ("derived", PathBuf::from("derived")),
            ("derived-search", PathBuf::from("derived").join("search")),
        ] {
            let root = temp_dir(&format!("cistella-search-m1-dangling-{label}"));
            let outside = temp_dir(&format!("cistella-search-m1-dangling-target-{label}"));
            let vault = write_vault(&root);
            vault
                .create_literature_item(draft("Dangling derived directory link"))
                .unwrap();

            if label == "derived-search" {
                fs::create_dir(root.join("derived")).unwrap();
            }
            let link = root.join(link);
            let target = outside.join("missing-target");
            assert!(!target.exists());
            make_directory_symlink(&link, &target);
            let vault_before = snapshot_entire_vault(&root);
            let outside_before = snapshot_entire_vault(&outside);

            // A dangling link must be rejected from no-follow metadata rather
            // than silently becoming `Missing` because `Path::exists()` follows
            // it to an absent target.
            assert_eq!(
                vault.search_index_state().status,
                SearchIndexStatus::Degraded,
                "{label}"
            );
            assert!(matches!(
                vault.load_search_index_generation(),
                Err(CoreError::SearchIndexUnavailable(_))
            ));
            assert_eq!(snapshot_entire_vault(&root), vault_before, "{label}");
            assert_eq!(snapshot_entire_vault(&outside), outside_before, "{label}");
            assert!(!target.exists(), "{label}: no target may be created");
            fs::remove_dir(&link).unwrap();
            let _ = fs::remove_dir_all(root);
            let _ = fs::remove_dir_all(outside);
        }
    }

    #[cfg(windows)]
    #[test]
    fn junction_redirects_are_rejected_before_any_search_write_and_preserve_vault() {
        for (name, link_parts, target_parts) in [
            ("search-to-user", vec!["derived", "search"], vec!["user"]),
            ("search-to-files", vec!["derived", "search"], vec!["files"]),
        ] {
            let root = temp_dir(&format!("cistella-search-m1-junction-{name}"));
            let vault = write_vault(&root);
            vault
                .create_literature_item(draft("Junction safety"))
                .unwrap();
            fs::create_dir_all(root.join("files")).unwrap();
            fs::create_dir_all(root.join("derived")).unwrap();
            let link = link_parts
                .iter()
                .fold(root.clone(), |path, part| path.join(part));
            let target = target_parts
                .iter()
                .fold(root.clone(), |path, part| path.join(part));
            make_junction(&link, &target);
            let before = snapshot_entire_vault(&root);

            assert!(matches!(
                vault.rebuild_metadata_search_index(),
                Err(CoreError::InvalidSearchIndexPath)
            ));
            assert_eq!(
                vault.search_index_state().status,
                SearchIndexStatus::Degraded
            );
            assert_eq!(snapshot_entire_vault(&root), before, "{name}");
            remove_junction(&link);
            let _ = fs::remove_dir_all(root);
        }

        let root = temp_dir("cistella-search-m1-junction-derived-outside");
        let outside = temp_dir("cistella-search-m1-junction-derived-target");
        let vault = write_vault(&root);
        vault
            .create_literature_item(draft("Outside derived junction"))
            .unwrap();
        let derived = root.join("derived");
        make_junction(&derived, &outside);
        let vault_before = snapshot_entire_vault(&root);
        let outside_before = snapshot_entire_vault(&outside);

        assert!(matches!(
            vault.rebuild_metadata_search_index(),
            Err(CoreError::InvalidSearchIndexPath)
        ));
        assert_eq!(
            vault.search_index_state().status,
            SearchIndexStatus::Degraded
        );
        assert_eq!(snapshot_entire_vault(&root), vault_before);
        assert_eq!(snapshot_entire_vault(&outside), outside_before);
        remove_junction(&derived);
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }

    #[cfg(windows)]
    #[test]
    fn committed_and_working_generation_junctions_are_degraded_without_reading_targets() {
        for target_kind in ["outside", "user", "files"] {
            let root = temp_dir(&format!(
                "cistella-search-m1-committed-junction-{target_kind}"
            ));
            let outside = temp_dir(&format!(
                "cistella-search-m1-committed-junction-target-{target_kind}"
            ));
            let vault = write_vault(&root);
            vault
                .create_literature_item(draft("Committed generation junction"))
                .unwrap();
            let ready = vault.rebuild_metadata_search_index().unwrap();
            let generation_id = ready.active_generation.unwrap();
            let generation = root
                .join("derived")
                .join("search")
                .join("generations")
                .join(generation_id.to_string());
            let target = match target_kind {
                "outside" => outside.clone(),
                "user" => root.join("user"),
                "files" => {
                    let files = root.join("files");
                    fs::create_dir_all(&files).unwrap();
                    files
                }
                _ => unreachable!(),
            };
            // Deliberately make the redirected location look loadable. A safe
            // state/load path must reject the junction before reading this file.
            fs::copy(
                generation.join("metadata.json"),
                target.join("metadata.json"),
            )
            .unwrap();
            fs::remove_dir_all(&generation).unwrap();
            make_junction(&generation, &target);
            let vault_before = snapshot_entire_vault(&root);
            let outside_before = snapshot_entire_vault(&outside);

            assert_eq!(
                vault.search_index_state().status,
                SearchIndexStatus::Degraded,
                "{target_kind}"
            );
            assert!(matches!(
                vault.load_search_index_generation(),
                Err(CoreError::SearchIndexUnavailable(_))
            ));
            assert_eq!(snapshot_entire_vault(&root), vault_before, "{target_kind}");
            assert_eq!(
                snapshot_entire_vault(&outside),
                outside_before,
                "{target_kind}"
            );
            remove_junction(&generation);
            let _ = fs::remove_dir_all(root);
            let _ = fs::remove_dir_all(outside);
        }

        let root = temp_dir("cistella-search-m1-working-junction");
        let outside = temp_dir("cistella-search-m1-working-junction-target");
        let vault = write_vault(&root);
        vault
            .create_literature_item(draft("Working generation junction"))
            .unwrap();
        let ready = vault.rebuild_metadata_search_index().unwrap();
        let committed = root
            .join("derived")
            .join("search")
            .join("generations")
            .join(ready.active_generation.unwrap().to_string());
        fs::copy(
            committed.join("metadata.json"),
            outside.join("metadata.json"),
        )
        .unwrap();
        let working = root
            .join("derived")
            .join("search")
            .join("working")
            .join(Uuid::new_v4().to_string());
        make_junction(&working, &outside);
        let vault_before = snapshot_entire_vault(&root);
        let outside_before = snapshot_entire_vault(&outside);

        // An unsafe working child is not ordinary `Building` evidence, even
        // while the manifest still identifies an otherwise ready generation.
        assert_eq!(
            vault.search_index_state().status,
            SearchIndexStatus::Degraded
        );
        assert!(matches!(
            vault.load_search_index_generation(),
            Err(CoreError::SearchIndexUnavailable(_))
        ));
        assert_eq!(snapshot_entire_vault(&root), vault_before);
        assert_eq!(snapshot_entire_vault(&outside), outside_before);
        remove_junction(&working);
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }

    #[cfg(windows)]
    #[test]
    fn file_links_are_rejected_before_search_json_is_read_or_loaded() {
        fn assert_unsafe_file_link(
            root: &Path,
            outside: &Path,
            vault: &Vault,
            link: &Path,
            target: &Path,
            label: &str,
        ) {
            make_file_symlink(link, target);
            let vault_before = snapshot_entire_vault(root);
            let outside_before = snapshot_entire_vault(outside);

            // The targets are deliberately valid JSON for their respective
            // roles. A Ready result could only occur by following the file
            // link, so Degraded proves the rejection happens before parsing.
            let state = vault.search_index_state();
            assert_eq!(state.status, SearchIndexStatus::Degraded, "{label}");
            assert!(
                state
                    .detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains("unsafe") && detail.contains("link")),
                "{label}: {state:?}"
            );
            assert!(matches!(
                vault.load_search_index_generation(),
                Err(CoreError::SearchIndexUnavailable(_))
            ));
            assert_eq!(snapshot_entire_vault(root), vault_before, "{label}");
            assert_eq!(snapshot_entire_vault(outside), outside_before, "{label}");
            fs::remove_file(link).unwrap();
        }

        // `manifest.json` linked to a compatible external manifest must be
        // rejected rather than treated as a portable, ready index.
        let root = temp_dir("cistella-search-m1-file-link-manifest");
        let outside = temp_dir("cistella-search-m1-file-link-manifest-target");
        let vault = write_vault(&root);
        vault
            .create_literature_item(draft("Manifest file link"))
            .unwrap();
        vault.rebuild_metadata_search_index().unwrap();
        let manifest = root.join(DERIVED_SEARCH_RELATIVE_DIR).join("manifest.json");
        let external_manifest = outside.join("compatible-manifest.json");
        fs::copy(&manifest, &external_manifest).unwrap();
        fs::remove_file(&manifest).unwrap();
        assert_unsafe_file_link(
            &root,
            &outside,
            &vault,
            &manifest,
            &external_manifest,
            "manifest-outside",
        );
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);

        // `metadata.json` linked outside and into Vault `user/` must both be
        // refused even when the target has a fully compatible generation.
        for target_kind in ["outside", "user"] {
            let root = temp_dir(&format!(
                "cistella-search-m1-file-link-metadata-{target_kind}"
            ));
            let outside = temp_dir(&format!(
                "cistella-search-m1-file-link-metadata-target-{target_kind}"
            ));
            let vault = write_vault(&root);
            vault
                .create_literature_item(draft("Metadata file link"))
                .unwrap();
            let ready = vault.rebuild_metadata_search_index().unwrap();
            let metadata = root
                .join(DERIVED_SEARCH_RELATIVE_DIR)
                .join("generations")
                .join(ready.active_generation.unwrap().to_string())
                .join("metadata.json");
            let target = match target_kind {
                "outside" => outside.join("compatible-metadata.json"),
                "user" => root.join("user").join("compatible-metadata.json"),
                _ => unreachable!(),
            };
            fs::copy(&metadata, &target).unwrap();
            fs::remove_file(&metadata).unwrap();
            assert_unsafe_file_link(
                &root,
                &outside,
                &vault,
                &metadata,
                &target,
                &format!("metadata-{target_kind}"),
            );
            let _ = fs::remove_dir_all(root);
            let _ = fs::remove_dir_all(outside);
        }

        // A stale failure marker is not read on the ready path today, but it
        // is still an index-owned status file and may never be a file link.
        let root = temp_dir("cistella-search-m1-file-link-failure");
        let outside = temp_dir("cistella-search-m1-file-link-failure-target");
        let vault = write_vault(&root);
        vault
            .create_literature_item(draft("Failure file link"))
            .unwrap();
        vault.rebuild_metadata_search_index().unwrap();
        let external_failure = outside.join("compatible-failure.json");
        let failure = SearchIndexFailure {
            format_version: SEARCH_INDEX_FORMAT_VERSION,
            failed_at: Utc::now(),
        };
        fs::write(
            &external_failure,
            serde_json::to_vec_pretty(&failure).unwrap(),
        )
        .unwrap();
        let failure_link = root.join(DERIVED_SEARCH_RELATIVE_DIR).join("failure.json");
        assert_unsafe_file_link(
            &root,
            &outside,
            &vault,
            &failure_link,
            &external_failure,
            "failure-outside",
        );
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }

    #[cfg(windows)]
    #[test]
    fn rebuild_rejects_index_owned_file_links_without_repairing_them() {
        for file_name in ["failure.json", "manifest.json"] {
            let root = temp_dir(&format!("cistella-search-m1-rebuild-file-link-{file_name}"));
            let outside = temp_dir(&format!(
                "cistella-search-m1-rebuild-file-link-target-{file_name}"
            ));
            let vault = write_vault(&root);
            vault
                .create_literature_item(draft("Rebuild must not repair file links"))
                .unwrap();
            vault.rebuild_metadata_search_index().unwrap();

            let link = root.join(DERIVED_SEARCH_RELATIVE_DIR).join(file_name);
            let target = outside.join(format!("compatible-{file_name}"));
            if file_name == "manifest.json" {
                fs::copy(&link, &target).unwrap();
                fs::remove_file(&link).unwrap();
            } else {
                let failure = SearchIndexFailure {
                    format_version: SEARCH_INDEX_FORMAT_VERSION,
                    failed_at: Utc::now(),
                };
                fs::write(&target, serde_json::to_vec_pretty(&failure).unwrap()).unwrap();
            }
            make_file_symlink(&link, &target);

            let vault_before = snapshot_entire_vault(&root);
            let outside_before = snapshot_entire_vault(&outside);
            assert!(matches!(
                vault.rebuild_metadata_search_index(),
                Err(CoreError::InvalidSearchIndexPath)
            ));
            // Rebuild must reject rather than removing the unsafe entry or
            // atomically replacing it with an owned JSON file.
            assert!(
                fs::symlink_metadata(&link)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert_eq!(snapshot_entire_vault(&root), vault_before, "{file_name}");
            assert_eq!(
                snapshot_entire_vault(&outside),
                outside_before,
                "{file_name}"
            );

            fs::remove_file(&link).unwrap();
            let _ = fs::remove_dir_all(root);
            let _ = fs::remove_dir_all(outside);
        }
    }

    #[cfg(windows)]
    #[test]
    fn rebuild_rejects_existing_generation_metadata_file_links_without_bypassing_them() {
        // A rebuild must validate all existing generations, not merely the
        // currently active one. The `working` case represents an interrupted
        // earlier build; the `nonactive` case proves a fresh manifest cannot
        // silently route around an unsafe committed generation.
        for generation_kind in ["active", "nonactive", "working"] {
            for target_kind in ["outside", "user"] {
                let root = temp_dir(&format!(
                    "cistella-search-m1-rebuild-existing-{generation_kind}-{target_kind}"
                ));
                let outside = temp_dir(&format!(
                    "cistella-search-m1-rebuild-existing-target-{generation_kind}-{target_kind}"
                ));
                let vault = write_vault(&root);
                vault
                    .create_literature_item(draft("Existing generation metadata link"))
                    .unwrap();
                let first = vault.rebuild_metadata_search_index().unwrap();
                let second = vault.rebuild_metadata_search_index().unwrap();
                let search_root = root.join(DERIVED_SEARCH_RELATIVE_DIR);
                let generations = search_root.join("generations");
                let active_id = second.active_generation.unwrap();
                let generation = match generation_kind {
                    "active" => generations.join(active_id.to_string()),
                    "nonactive" => generations.join(first.active_generation.unwrap().to_string()),
                    "working" => {
                        let working = search_root.join("working").join(Uuid::new_v4().to_string());
                        fs::create_dir(&working).unwrap();
                        working
                    }
                    _ => unreachable!(),
                };
                let metadata = generation.join("metadata.json");
                let source_metadata = generations
                    .join(active_id.to_string())
                    .join("metadata.json");
                if generation_kind == "working" {
                    fs::copy(&source_metadata, &metadata).unwrap();
                }
                let target = match target_kind {
                    "outside" => {
                        outside.join(format!("compatible-{generation_kind}-{target_kind}.json"))
                    }
                    "user" => root
                        .join("user")
                        .join(format!("compatible-{generation_kind}-{target_kind}.json")),
                    _ => unreachable!(),
                };
                fs::copy(&metadata, &target).unwrap();
                fs::remove_file(&metadata).unwrap();
                make_file_symlink(&metadata, &target);

                let vault_before = snapshot_entire_vault(&root);
                let outside_before = snapshot_entire_vault(&outside);
                // Status and loading must apply the same full existing-layout
                // scan as rebuild: active, inactive, and interrupted working
                // metadata links are all unsafe index state.
                assert_eq!(
                    vault.search_index_state().status,
                    SearchIndexStatus::Degraded,
                    "{generation_kind}-{target_kind}"
                );
                assert!(matches!(
                    vault.load_search_index_generation(),
                    Err(CoreError::SearchIndexUnavailable(_))
                ));
                assert!(
                    matches!(
                        vault.rebuild_metadata_search_index(),
                        Err(CoreError::InvalidSearchIndexPath)
                    ),
                    "{generation_kind}-{target_kind}"
                );
                assert!(
                    fs::symlink_metadata(&metadata)
                        .unwrap()
                        .file_type()
                        .is_symlink(),
                    "{generation_kind}-{target_kind}"
                );
                assert_eq!(
                    snapshot_entire_vault(&root),
                    vault_before,
                    "{generation_kind}-{target_kind}"
                );
                assert_eq!(
                    snapshot_entire_vault(&outside),
                    outside_before,
                    "{generation_kind}-{target_kind}"
                );

                fs::remove_file(&metadata).unwrap();
                let _ = fs::remove_dir_all(root);
                let _ = fs::remove_dir_all(outside);
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn all_existing_generation_directory_links_degrade_status_and_loader() {
        // The whole index layout is a security boundary. A stale inactive
        // generation or interrupted working generation cannot be ignored just
        // because the current active generation remains a valid directory.
        for generation_kind in ["active", "nonactive", "working"] {
            for target_kind in ["outside", "user"] {
                let root = temp_dir(&format!(
                    "cistella-search-m1-status-existing-directory-{generation_kind}-{target_kind}"
                ));
                let outside = temp_dir(&format!(
                    "cistella-search-m1-status-existing-directory-target-{generation_kind}-{target_kind}"
                ));
                let vault = write_vault(&root);
                vault
                    .create_literature_item(draft("Existing generation directory link"))
                    .unwrap();
                let first = vault.rebuild_metadata_search_index().unwrap();
                let second = vault.rebuild_metadata_search_index().unwrap();
                let search_root = root.join("derived").join("search");
                let generations = search_root.join("generations");
                let generation = match generation_kind {
                    "active" => generations.join(second.active_generation.unwrap().to_string()),
                    "nonactive" => generations.join(first.active_generation.unwrap().to_string()),
                    "working" => search_root.join("working").join(Uuid::new_v4().to_string()),
                    _ => unreachable!(),
                };
                let target = match target_kind {
                    "outside" => outside.join("compatible-generation"),
                    "user" => root.join("user").join("compatible-generation"),
                    _ => unreachable!(),
                };
                fs::create_dir(&target).unwrap();
                fs::copy(
                    generations
                        .join(second.active_generation.unwrap().to_string())
                        .join("metadata.json"),
                    target.join("metadata.json"),
                )
                .unwrap();
                if generation_kind != "working" {
                    fs::remove_dir_all(&generation).unwrap();
                }
                make_junction(&generation, &target);

                let vault_before = snapshot_entire_vault(&root);
                let outside_before = snapshot_entire_vault(&outside);
                assert_eq!(
                    vault.search_index_state().status,
                    SearchIndexStatus::Degraded,
                    "{generation_kind}-{target_kind}"
                );
                assert!(matches!(
                    vault.load_search_index_generation(),
                    Err(CoreError::SearchIndexUnavailable(_))
                ));
                assert!(matches!(
                    vault.rebuild_metadata_search_index(),
                    Err(CoreError::InvalidSearchIndexPath)
                ));
                assert_eq!(
                    snapshot_entire_vault(&root),
                    vault_before,
                    "{generation_kind}-{target_kind}"
                );
                assert_eq!(
                    snapshot_entire_vault(&outside),
                    outside_before,
                    "{generation_kind}-{target_kind}"
                );

                remove_junction(&generation);
                let _ = fs::remove_dir_all(root);
                let _ = fs::remove_dir_all(outside);
            }
        }
    }

    #[test]
    fn copied_vault_keeps_portable_metadata_generation_and_deleted_index_does_not_block_vault() {
        let root = temp_dir("cistella-search-m1-copy-source");
        let copied = temp_dir("cistella-search-m1-copy-destination");
        let vault = write_vault(&root);
        let item = vault
            .create_literature_item(draft("Portable search"))
            .unwrap();
        vault.rebuild_metadata_search_index().unwrap();
        copy_tree(&root, &copied);
        let copied_vault =
            Vault::open_sources_file(copied.join("sources.parquet"), VaultOpenOptions::default())
                .unwrap();
        let copied_snapshot = copied_vault.load_search_index_generation().unwrap();
        assert_eq!(copied_snapshot.metadata_records[0].item_id, item.item_id);
        let payload = serde_json::to_string(&copied_snapshot).unwrap();
        assert!(!payload.contains(&root.to_string_lossy().to_string()));
        fs::remove_dir_all(copied.join("derived").join("search")).unwrap();
        assert_eq!(
            copied_vault.search_index_state().status,
            SearchIndexStatus::Missing
        );
        assert_eq!(
            copied_vault.load_literature_items().unwrap()[0].item_id,
            item.item_id
        );
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(copied);
    }

    #[test]
    fn m2_handles_real_pdf_no_text_encrypted_unsupported_and_file_limit_without_authority_writes() {
        let root = temp_dir("cistella-search-m2-outcomes");
        let incoming = temp_dir("cistella-search-m2-outcomes-incoming");
        let vault = write_vault(&root);
        let item = vault
            .create_literature_item(draft("Outcome matrix"))
            .unwrap();
        let text_pdf = incoming.join("text.pdf");
        let blank_pdf = incoming.join("blank.pdf");
        let encrypted_pdf = incoming.join("encrypted.pdf");
        let unsupported_pdf = incoming.join("unsupported.pdf");
        let huge_pdf = incoming.join("huge.pdf");
        write_test_pdf(&text_pdf, Some("stable corpus golden text"));
        write_test_pdf(&blank_pdf, None);
        write_encrypted_test_pdf(&encrypted_pdf);
        write_test_pdf(
            &unsupported_pdf,
            Some("not treated as pdf after metadata change"),
        );
        write_test_pdf(&huge_pdf, Some("file limit is checked before parsing"));
        let text = import_test_pdf(&vault, item.item_id, &text_pdf);
        let blank = import_test_pdf(&vault, item.item_id, &blank_pdf);
        let encrypted = import_test_pdf(&vault, item.item_id, &encrypted_pdf);
        let unsupported = import_test_pdf(&vault, item.item_id, &unsupported_pdf);
        let huge = import_test_pdf(&vault, item.item_id, &huge_pdf);
        let huge_path = vault
            .resolve_document_asset_path(item.item_id, huge.asset_id)
            .unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(&huge_path)
            .unwrap()
            .set_len(PDF_MAX_FILE_SIZE_BYTES + 1)
            .unwrap();
        let mut assets = vault.load_document_assets().unwrap();
        assets
            .iter_mut()
            .find(|asset| asset.asset_id == unsupported.asset_id)
            .unwrap()
            .media_type = "application/octet-stream".to_string();
        vault.save_document_assets(&assets).unwrap();
        let authority_before = snapshot_non_derived_files(&root);

        vault.rebuild_metadata_search_index().unwrap();
        let generation = vault.load_search_index_generation().unwrap();
        let states = generation
            .asset_content_records
            .iter()
            .map(|record| (record.asset_id, record.state))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(states[&text.asset_id], AssetContentIndexState::Indexed);
        assert_eq!(states[&blank.asset_id], AssetContentIndexState::NoText);
        assert_eq!(
            states[&encrypted.asset_id],
            AssetContentIndexState::Encrypted
        );
        assert_eq!(
            states[&unsupported.asset_id],
            AssetContentIndexState::Unsupported
        );
        assert_eq!(
            states[&huge.asset_id],
            AssetContentIndexState::LimitExceeded
        );
        assert_eq!(snapshot_non_derived_files(&root), authority_before);
        assert_eq!(
            serde_json::to_string(&AssetContentIndexState::Cancelled).unwrap(),
            "\"cancelled\""
        );
        assert_eq!(
            AssetContentIndexState::Cancelled.issue_kind(),
            Some(SearchIndexIssueKind::Cancelled)
        );
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(incoming);
    }

    #[test]
    fn m2_full_reconciliation_converges_orphans_and_drops_old_text_when_replacement_fails() {
        let root = temp_dir("cistella-search-m2-reconcile");
        let incoming = temp_dir("cistella-search-m2-reconcile-incoming");
        let vault = write_vault(&root);
        let first = vault.create_literature_item(draft("First title")).unwrap();
        let second = vault.create_literature_item(draft("Second title")).unwrap();
        let first_source = incoming.join("first.pdf");
        let second_source = incoming.join("second.pdf");
        write_test_pdf(&first_source, Some("first original body"));
        write_test_pdf(&second_source, Some("second body"));
        let first_asset = import_test_pdf(&vault, first.item_id, &first_source);
        let second_asset = import_test_pdf(&vault, second.item_id, &second_source);
        let first_ready = vault.rebuild_metadata_search_index().unwrap();
        let first_snapshot = vault.load_search_index_generation().unwrap();
        let first_record = first_snapshot
            .asset_content_records
            .iter()
            .find(|record| record.asset_id == first_asset.asset_id)
            .unwrap()
            .clone();
        assert_eq!(first_record.state, AssetContentIndexState::Indexed);

        let mut changed = draft("First renamed");
        changed.tags.push("reconciled".to_string());
        vault
            .update_literature_item(first.item_id, changed)
            .unwrap();
        let metadata_update = vault.reconcile_search_index().unwrap();
        assert_ne!(
            metadata_update.active_generation,
            first_ready.active_generation
        );
        let metadata_snapshot = vault.load_search_index_generation().unwrap();
        let metadata_record = metadata_snapshot
            .asset_content_records
            .iter()
            .find(|record| record.asset_id == first_asset.asset_id)
            .unwrap();
        assert_eq!(metadata_record.state, AssetContentIndexState::Indexed);
        assert_eq!(metadata_record.content, first_record.content);
        assert_ne!(
            metadata_record.metadata_fingerprint,
            first_record.metadata_fingerprint
        );

        fs::write(
            vault
                .resolve_document_asset_path(first.item_id, first_asset.asset_id)
                .unwrap(),
            b"broken replacement",
        )
        .unwrap();
        vault.reconcile_search_index().unwrap();
        let failed_replacement = vault.load_search_index_generation().unwrap();
        let replacement_record = failed_replacement
            .asset_content_records
            .iter()
            .find(|record| record.asset_id == first_asset.asset_id)
            .unwrap();
        assert_eq!(
            replacement_record.state,
            AssetContentIndexState::ParseFailed
        );
        assert_eq!(
            replacement_record.content, None,
            "a changed failed PDF must not retain prior body text"
        );
        assert!(
            failed_replacement
                .issues
                .iter()
                .any(|issue| issue.asset_id == first_asset.asset_id
                    && issue.kind == SearchIndexIssueKind::ParseFailed)
        );

        let added_source = incoming.join("added.pdf");
        write_test_pdf(&added_source, Some("newly added body"));
        let added_asset = import_test_pdf(&vault, second.item_id, &added_source);
        vault.reconcile_search_index().unwrap();
        let after_add = vault.load_search_index_generation().unwrap();
        assert!(after_add.asset_content_records.iter().any(|record| {
            record.asset_id == added_asset.asset_id
                && record.state == AssetContentIndexState::Indexed
                && record
                    .content
                    .as_deref()
                    .is_some_and(|text| text.contains("newly added body"))
        }));

        vault
            .remove_document_asset(second.item_id, second_asset.asset_id)
            .unwrap();
        vault.reconcile_search_index().unwrap();
        let after_asset_delete = vault.load_search_index_generation().unwrap();
        assert!(
            !after_asset_delete
                .asset_content_records
                .iter()
                .any(|record| record.asset_id == second_asset.asset_id)
        );
        vault.delete_literature_item(first.item_id).unwrap();
        vault.reconcile_search_index().unwrap();
        let after_item_delete = vault.load_search_index_generation().unwrap();
        assert!(
            !after_item_delete
                .metadata_records
                .iter()
                .any(|record| record.item_id == first.item_id)
        );
        assert!(
            !after_item_delete
                .asset_content_records
                .iter()
                .any(|record| record.item_id == first.item_id)
        );
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(incoming);
    }

    #[test]
    fn m2_authoritative_mutations_publish_incrementally_without_touching_unaffected_pdfs() {
        let root = temp_dir("cistella-search-m2-authoritative-incremental");
        let incoming = temp_dir("cistella-search-m2-authoritative-incremental-incoming");
        let vault = write_vault(&root);
        let first = vault
            .create_literature_item(draft("First original"))
            .unwrap();
        let second = vault
            .create_literature_item(draft("Second original"))
            .unwrap();
        let first_source = incoming.join("first.pdf");
        let second_source = incoming.join("second.pdf");
        write_test_pdf(&first_source, Some("first original body"));
        write_test_pdf(&second_source, Some("second original body"));
        let first_asset = import_test_pdf(&vault, first.item_id, &first_source);
        let second_asset = import_test_pdf(&vault, second.item_id, &second_source);
        vault.rebuild_metadata_search_index().unwrap();

        // A metadata-only authority write automatically publishes a new
        // generation, but it must not resolve, hash, or extract either PDF.
        clear_pdf_index_attempts();
        clear_pdf_hash_attempts();
        let mut renamed = draft("First renamed incrementally");
        renamed.tags.push("incremental".to_string());
        vault
            .update_literature_item(first.item_id, renamed)
            .unwrap();
        assert!(
            take_pdf_index_attempts().is_empty(),
            "metadata-only update must not open any PDF"
        );
        assert!(
            take_pdf_hash_attempts().is_empty(),
            "metadata-only update must not hash any PDF"
        );
        let metadata_generation = vault.load_search_index_generation().unwrap();
        assert_eq!(
            metadata_generation
                .metadata_records
                .iter()
                .find(|record| record.item_id == first.item_id)
                .unwrap()
                .title,
            "First renamed incrementally"
        );
        assert!(
            metadata_generation
                .asset_content_records
                .iter()
                .any(|record| record.asset_id == second_asset.asset_id)
        );

        // A new Vault asset is the sole identity allowed through the content
        // pipeline. The previously indexed PDFs remain carried records.
        let added_source = incoming.join("added.pdf");
        write_test_pdf(&added_source, Some("added incrementally"));
        clear_pdf_index_attempts();
        clear_pdf_hash_attempts();
        let added_asset = import_test_pdf(&vault, first.item_id, &added_source);
        assert_eq!(
            take_pdf_index_attempts(),
            vec![(first.item_id, added_asset.asset_id)],
            "asset add must not open unrelated PDFs"
        );
        assert_eq!(
            take_pdf_hash_attempts(),
            vec![(first.item_id, added_asset.asset_id)],
            "asset add must not hash unrelated PDFs"
        );
        let after_add = vault.load_search_index_generation().unwrap();
        assert!(after_add.asset_content_records.iter().any(|record| {
            record.asset_id == added_asset.asset_id
                && record
                    .content
                    .as_deref()
                    .is_some_and(|text| text.contains("added incrementally"))
        }));

        // Remove and item-delete changes are identity-only record removal and
        // therefore also require no PDF access. This proves normal authority
        // operations no longer depend on a later full reconciliation.
        clear_pdf_index_attempts();
        clear_pdf_hash_attempts();
        vault
            .remove_document_asset(first.item_id, first_asset.asset_id)
            .unwrap();
        assert!(take_pdf_index_attempts().is_empty());
        assert!(take_pdf_hash_attempts().is_empty());
        assert!(
            !vault
                .load_search_index_generation()
                .unwrap()
                .asset_content_records
                .iter()
                .any(|record| record.asset_id == first_asset.asset_id)
        );

        vault.delete_literature_item(second.item_id).unwrap();
        let after_delete = vault.load_search_index_generation().unwrap();
        assert!(
            !after_delete
                .metadata_records
                .iter()
                .any(|record| record.item_id == second.item_id)
        );
        assert!(
            !after_delete
                .asset_content_records
                .iter()
                .any(|record| record.item_id == second.item_id)
        );
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(incoming);
    }

    #[test]
    fn m3_query_aggregates_fields_assets_and_reports_unavailable_explicitly() {
        let missing_root = temp_dir("cistella-search-m3-missing");
        let missing_vault = write_vault(&missing_root);
        let missing = missing_vault
            .query_search_index(
                &SearchQuery {
                    text: "needle".to_string(),
                    scopes: vec![SearchFieldScope::All],
                },
                0,
                DEFAULT_SEARCH_PAGE_SIZE,
            )
            .unwrap();
        assert!(matches!(
            missing,
            SearchQueryResult::Unavailable {
                index_state: SearchIndexState {
                    status: SearchIndexStatus::Missing,
                    ..
                }
            }
        ));

        let root = temp_dir("cistella-search-m3-query");
        let incoming = temp_dir("cistella-search-m3-query-incoming");
        let vault = write_vault(&root);
        let mut combined_draft = draft("Needle Catalog");
        combined_draft.authors = vec!["Grace Hopper".to_string()];
        combined_draft.tags = vec!["retrieval".to_string(), "catalog".to_string()];
        let combined = vault.create_literature_item(combined_draft).unwrap();
        let mut content_draft = draft("Body-only paper");
        content_draft.authors = vec!["Alan Turing".to_string()];
        content_draft.tags = vec!["logic".to_string()];
        let content_only = vault.create_literature_item(content_draft).unwrap();
        let combined_pdf = incoming.join("combined.pdf");
        let content_pdf = incoming.join("content.pdf");
        write_test_pdf(&combined_pdf, Some("A needle appears in this PDF body."));
        write_test_pdf(&content_pdf, Some("another needle in body text"));
        let combined_asset = import_test_pdf(&vault, combined.item_id, &combined_pdf);
        import_test_pdf(&vault, content_only.item_id, &content_pdf);
        vault.rebuild_metadata_search_index().unwrap();

        let all = vault
            .query_search_index(
                &SearchQuery {
                    text: "needle".to_string(),
                    scopes: vec![SearchFieldScope::All],
                },
                0,
                50,
            )
            .unwrap();
        let SearchQueryResult::Ready { page } = all else {
            panic!("ready index must return a ready page")
        };
        assert_eq!(page.total_hits, 2);
        assert_eq!(
            page.hits[0].item_id, combined.item_id,
            "metadata hits sort before content-only hits"
        );
        let combined_hit = page
            .hits
            .iter()
            .find(|hit| hit.item_id == combined.item_id)
            .unwrap();
        assert!(
            combined_hit
                .field_matches
                .iter()
                .any(|entry| entry.field == SearchMatchField::Title)
        );
        let content_match = combined_hit
            .field_matches
            .iter()
            .find(|entry| entry.field == SearchMatchField::Content)
            .unwrap();
        assert_eq!(content_match.asset_id, Some(combined_asset.asset_id));
        assert_eq!(
            content_match.asset_state,
            Some(AssetContentIndexState::Indexed)
        );
        assert!(
            content_match
                .excerpt
                .as_deref()
                .is_some_and(|text| text.contains("needle"))
        );

        for (scope, query, expected_item, expected_field) in [
            (
                SearchFieldScope::Authors,
                "grace",
                combined.item_id,
                SearchMatchField::Authors,
            ),
            (
                SearchFieldScope::Tags,
                "retrieval",
                combined.item_id,
                SearchMatchField::Tags,
            ),
            (
                SearchFieldScope::Content,
                "another",
                content_only.item_id,
                SearchMatchField::Content,
            ),
        ] {
            let result = vault
                .query_search_index(
                    &SearchQuery {
                        text: query.to_string(),
                        scopes: vec![scope],
                    },
                    0,
                    50,
                )
                .unwrap();
            let SearchQueryResult::Ready { page } = result else {
                panic!("scope query must be ready")
            };
            assert_eq!(page.total_hits, 1);
            assert_eq!(page.hits[0].item_id, expected_item);
            assert!(
                page.hits[0]
                    .field_matches
                    .iter()
                    .all(|entry| entry.field == expected_field)
            );
        }

        let paged = vault
            .query_search_index(
                &SearchQuery {
                    text: "needle".to_string(),
                    scopes: vec![SearchFieldScope::All],
                },
                1,
                1,
            )
            .unwrap();
        let SearchQueryResult::Ready { page } = paged else {
            panic!("paged query must be ready")
        };
        assert_eq!(
            (page.total_hits, page.offset, page.limit, page.hits.len()),
            (2, 1, 1, 1)
        );
        let payload = serde_json::to_string(&page).unwrap();
        assert!(!payload.contains(&root.to_string_lossy().to_string()));
        let _ = fs::remove_dir_all(missing_root);
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(incoming);
    }

    #[test]
    fn m3_cancelled_rebuild_preserves_previous_ready_generation() {
        let root = temp_dir("cistella-search-m3-cancel");
        let vault = write_vault(&root);
        vault.create_literature_item(draft("cancel guard")).unwrap();
        let ready = vault.rebuild_metadata_search_index().unwrap();
        let error = vault
            .rebuild_metadata_search_index_cancellable(|| true)
            .unwrap_err();
        assert!(matches!(error, CoreError::SearchIndexBuildCancelled));
        let after = vault.search_index_state();
        assert_eq!(after.status, SearchIndexStatus::Ready);
        assert_eq!(after.active_generation, ready.active_generation);

        let fresh_root = temp_dir("cistella-search-m3-initial-cancel");
        let fresh_vault = write_vault(&fresh_root);
        fresh_vault
            .create_literature_item(draft("initial cancel guard"))
            .unwrap();
        assert!(matches!(
            fresh_vault.rebuild_metadata_search_index_cancellable(|| true),
            Err(CoreError::SearchIndexBuildCancelled)
        ));
        assert_eq!(
            fresh_vault.search_index_state().status,
            SearchIndexStatus::Missing
        );
        let _ = fs::remove_dir_all(fresh_root);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn m2_incremental_failure_never_rolls_back_authority_and_reconciliation_converges() {
        let root = temp_dir("cistella-search-m2-incremental-failure");
        let incoming = temp_dir("cistella-search-m2-incremental-failure-incoming");
        let vault = write_vault(&root);
        let item = vault
            .create_literature_item(draft("Authority before failure"))
            .unwrap();
        let source = incoming.join("authority.pdf");
        write_test_pdf(&source, Some("authority body"));
        import_test_pdf(&vault, item.item_id, &source);
        vault.rebuild_metadata_search_index().unwrap();

        // Force only the derived publication path to fail. The authoritative
        // item JSON must remain committed after the best-effort mutation hook
        // returns; it may not be rolled back or contaminated by index failure.
        let manifest_path = root.join(DERIVED_SEARCH_RELATIVE_DIR).join("manifest.json");
        fs::remove_file(&manifest_path).unwrap();
        fs::create_dir(&manifest_path).unwrap();
        let authority_before = snapshot_non_derived_files(&root);
        let mut changed = draft("Authority survives derived failure");
        changed.tags.push("pending-reconciliation".to_string());
        let updated = vault.update_literature_item(item.item_id, changed).unwrap();
        assert_eq!(updated.title, "Authority survives derived failure");
        assert_eq!(
            vault.load_literature_items().unwrap()[0].title,
            "Authority survives derived failure"
        );
        assert_ne!(snapshot_non_derived_files(&root), authority_before);
        assert_eq!(
            vault.search_index_state().status,
            SearchIndexStatus::Degraded
        );

        // Once the transient derived-only obstruction is gone, the explicit
        // full reconciliation re-reads authority and converges the old index.
        fs::remove_dir(&manifest_path).unwrap();
        vault.reconcile_search_index().unwrap();
        let reconciled = vault.load_search_index_generation().unwrap();
        assert_eq!(
            reconciled
                .metadata_records
                .iter()
                .find(|record| record.item_id == item.item_id)
                .unwrap()
                .title,
            "Authority survives derived failure"
        );
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(incoming);
    }

    #[test]
    fn m2_schema_upgrade_marks_m1_generation_stale_and_rebuilds_portably() {
        let root = temp_dir("cistella-search-m2-schema-upgrade");
        let incoming = temp_dir("cistella-search-m2-schema-upgrade-incoming");
        let vault = write_vault(&root);
        let item = vault.create_literature_item(draft("Migration")).unwrap();
        let source = incoming.join("migration.pdf");
        write_test_pdf(&source, Some("portable M2 body"));
        let asset = import_test_pdf(&vault, item.item_id, &source);
        vault.rebuild_metadata_search_index().unwrap();
        let active = vault.search_index_state().active_generation.unwrap();
        let root_search = root.join(DERIVED_SEARCH_RELATIVE_DIR);
        let manifest_path = root_search.join("manifest.json");
        let generation_path = root_search
            .join("generations")
            .join(active.to_string())
            .join("metadata.json");
        for path in [&manifest_path, &generation_path] {
            let mut value: serde_json::Value =
                serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
            value["formatVersion"] = serde_json::json!(1);
            value["pdfTextExtractorVersion"] = serde_json::json!("pending-m2");
            fs::write(path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        }
        assert_eq!(vault.search_index_state().status, SearchIndexStatus::Stale);
        assert!(matches!(
            vault.load_search_index_generation(),
            Err(CoreError::SearchIndexUnavailable(_))
        ));
        vault.rebuild_metadata_search_index().unwrap();
        let rebuilt = vault.load_search_index_generation().unwrap();
        let record = rebuilt
            .asset_content_records
            .iter()
            .find(|record| record.asset_id == asset.asset_id)
            .unwrap();
        assert_eq!(record.state, AssetContentIndexState::Indexed);
        let copied = temp_dir("cistella-search-m2-schema-upgrade-copy");
        copy_tree(&root, &copied);
        let copied_vault =
            Vault::open_sources_file(copied.join("sources.parquet"), VaultOpenOptions::default())
                .unwrap();
        let copied_snapshot = copied_vault.load_search_index_generation().unwrap();
        assert!(copied_snapshot.asset_content_records.iter().any(|record| {
            record.asset_id == asset.asset_id
                && record
                    .content
                    .as_deref()
                    .is_some_and(|text| text.contains("portable M2 body"))
        }));
        let payload = serde_json::to_string(&copied_snapshot).unwrap();
        assert!(!payload.contains(&root.to_string_lossy().to_string()));
        assert!(!payload.contains(&copied.to_string_lossy().to_string()));
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(copied);
        let _ = fs::remove_dir_all(incoming);
    }

    #[test]
    fn m2_fixed_corpus_resource_golden_is_bounded_and_portable() {
        let root = temp_dir("cistella-search-m2-golden");
        let incoming = temp_dir("cistella-search-m2-golden-incoming");
        let vault = write_vault(&root);
        let item = vault
            .create_literature_item(draft("M2 resource golden"))
            .unwrap();
        let mut source_bytes = 0_u64;
        let mut expected_chars = 0_usize;
        for index in 0..PDF_EXTRACTION_BATCH_SIZE {
            let text = format!("fixed corpus page {index}: cistella PDF extraction golden");
            expected_chars += text.len();
            let source = incoming.join(format!("fixture-{index}.pdf"));
            write_test_pdf(&source, Some(&text));
            source_bytes += fs::metadata(&source).unwrap().len();
            import_test_pdf(&vault, item.item_id, &source);
        }
        let authority_before = snapshot_non_derived_files(&root);
        let first_started = Instant::now();
        vault.rebuild_metadata_search_index().unwrap();
        let first_elapsed = first_started.elapsed();
        let first = vault.load_search_index_generation().unwrap();
        let indexed_chars = first
            .asset_content_records
            .iter()
            .filter_map(|record| record.content.as_deref())
            .map(str::len)
            .sum::<usize>();
        let derived_bytes = fs::read_dir(root.join(DERIVED_SEARCH_RELATIVE_DIR))
            .unwrap()
            .flat_map(|entry| walk_file_sizes(&entry.unwrap().path()))
            .sum::<u64>();
        let mut changed = draft("M2 resource golden metadata changed");
        changed.tags.push("incremental".to_string());
        let incremental_started = Instant::now();
        vault.update_literature_item(item.item_id, changed).unwrap();
        let incremental_elapsed = incremental_started.elapsed();
        // The authoritative mutation itself publishes the bounded
        // metadata-only generation. The explicit reconciliation below is a
        // separate full-convergence check, not the normal update mechanism.
        assert_eq!(
            vault
                .load_search_index_generation()
                .unwrap()
                .metadata_records
                .iter()
                .find(|record| record.item_id == item.item_id)
                .unwrap()
                .title,
            "M2 resource golden metadata changed"
        );
        println!(
            "M2 fixed corpus golden: assets={}, pages={}, source_bytes={}, indexed_chars={}, first_ms={}, incremental_ms={}, derived_bytes={}, batch={}, concurrency={}, limits=file:{} pages:{} chars:{} decompressed_per_page:{} duration_ms:{}",
            PDF_EXTRACTION_BATCH_SIZE,
            PDF_EXTRACTION_BATCH_SIZE,
            source_bytes,
            indexed_chars,
            first_elapsed.as_millis(),
            incremental_elapsed.as_millis(),
            derived_bytes,
            PDF_EXTRACTION_BATCH_SIZE,
            PDF_EXTRACTION_CONCURRENCY,
            PDF_MAX_FILE_SIZE_BYTES,
            PDF_MAX_PAGES,
            PDF_MAX_EXTRACTED_CHARS,
            PDF_MAX_DECOMPRESSED_BYTES_PER_PAGE,
            PDF_MAX_EXTRACTION_DURATION.as_millis(),
        );
        assert!(indexed_chars >= expected_chars);
        assert!(first_elapsed < Duration::from_secs(5));
        assert!(incremental_elapsed < Duration::from_secs(5));
        // Reconciliation only writes its derived generation. The metadata edit
        // happened before this snapshot, so this verifies the M2 operation
        // itself cannot mutate source tables, user records, or Vault PDFs.
        let authority_after_metadata_change = snapshot_non_derived_files(&root);
        vault.reconcile_search_index().unwrap();
        assert_eq!(
            snapshot_non_derived_files(&root),
            authority_after_metadata_change
        );
        assert_ne!(authority_before, authority_after_metadata_change);
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(incoming);
    }

    fn walk_file_sizes(path: &Path) -> Vec<u64> {
        if path.is_file() {
            return vec![fs::metadata(path).unwrap().len()];
        }
        if path.is_dir() {
            return fs::read_dir(path)
                .unwrap()
                .flat_map(|entry| walk_file_sizes(&entry.unwrap().path()))
                .collect();
        }
        Vec::new()
    }
}
