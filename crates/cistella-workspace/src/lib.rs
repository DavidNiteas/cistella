//! cistella Workspace facade.
//!
//! This crate exposes the product-level Workspace API that desktop, Tauri, and
//! front-end code can consume without depending on `tree-space` internals.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use cistella_core::{VaultManifest, backup_vault};
use cistella_tree_space::{register as register_tree_space_plugins, registered_block_names};
use serde::{Deserialize, Serialize};
use tree_space::error::{ErrorCode, Result, TreeSpaceError};

const WORKSPACE_SCHEMA_VERSION: &str = "1.0.0";
const WORKSPACE_MANIFEST_FILE: &str = "workspace.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceContract {
    pub schema_version: String,
    pub manifest_file: String,
    pub default_mode: WorkspaceMode,
    pub default_status: WorkspaceStatus,
    pub standard_kinds: Vec<WorkspaceKind>,
    pub registered_blocks: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Ord, PartialOrd, Hash)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceKind {
    Literature,
    References,
    Writing,
    Assets,
    Analysis,
    Imports,
    System,
    Cache,
}

impl WorkspaceKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Literature => "literature",
            Self::References => "references",
            Self::Writing => "writing",
            Self::Assets => "assets",
            Self::Analysis => "analysis",
            Self::Imports => "imports",
            Self::System => "system",
            Self::Cache => "cache",
        }
    }

    pub const fn standard() -> [Self; 8] {
        [
            Self::Literature,
            Self::References,
            Self::Writing,
            Self::Assets,
            Self::Analysis,
            Self::Imports,
            Self::System,
            Self::Cache,
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceSubtreeState {
    Ready,
    Missing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceStatus {
    Ready,
    Degraded,
    ReadOnly,
    Failed,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceMode {
    ReadWrite,
    ReadOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSubtreeManifest {
    pub kind: WorkspaceKind,
    pub directory: String,
    pub state: WorkspaceSubtreeState,
    pub registered_blocks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceManifest {
    pub schema_version: String,
    pub workspace_id: String,
    pub title: String,
    pub mode: WorkspaceMode,
    pub subtrees: Vec<WorkspaceSubtreeManifest>,
    pub registered_blocks: Vec<String>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

impl WorkspaceManifest {
    fn new(root: &Path, title: String, mode: WorkspaceMode) -> Self {
        let now = now_ms();
        let subtrees = WorkspaceKind::standard()
            .into_iter()
            .map(|kind| WorkspaceSubtreeManifest {
                kind,
                directory: kind.as_str().to_string(),
                state: WorkspaceSubtreeState::Ready,
                registered_blocks: match kind {
                    WorkspaceKind::Literature => vec!["cistella.literature.items".to_string()],
                    WorkspaceKind::Assets => vec!["cistella.assets.object".to_string()],
                    _ => Vec::new(),
                },
            })
            .collect();
        Self {
            schema_version: WORKSPACE_SCHEMA_VERSION.to_string(),
            workspace_id: root.to_string_lossy().to_string(),
            title,
            mode,
            subtrees,
            registered_blocks: registered_block_names()
                .into_iter()
                .map(str::to_string)
                .collect(),
            created_at_ms: now,
            updated_at_ms: now,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSummary {
    pub workspace_id: String,
    pub title: String,
    pub root: String,
    pub mode: WorkspaceMode,
    pub status: WorkspaceStatus,
    pub subtrees: Vec<WorkspaceSubtreeManifest>,
    pub registered_blocks: Vec<String>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone)]
pub struct WorkspaceHandle {
    root: PathBuf,
    manifest: WorkspaceManifest,
    status: WorkspaceStatus,
}

impl WorkspaceHandle {
    pub fn create(root: impl AsRef<Path>, title: impl Into<String>) -> Result<Self> {
        register_tree_space_plugins().map_err(map_tree_space_error)?;
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root).map_err(io_error("create workspace root"))?;
        for kind in WorkspaceKind::standard() {
            fs::create_dir_all(root.join(kind.as_str())).map_err(io_error("create subtree"))?;
        }
        let manifest = WorkspaceManifest::new(&root, title.into(), WorkspaceMode::ReadWrite);
        write_manifest(&root, &manifest)?;
        Ok(Self {
            root,
            manifest,
            status: WorkspaceStatus::Ready,
        })
    }

    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        register_tree_space_plugins().map_err(map_tree_space_error)?;
        let root = root.as_ref().to_path_buf();
        let manifest = read_manifest(&root)?;
        let status = evaluate_status(&root, &manifest);
        Ok(Self {
            root,
            manifest,
            status,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn title(&self) -> &str {
        &self.manifest.title
    }

    pub fn status(&self) -> WorkspaceStatus {
        self.status
    }

    pub fn subtrees(&self) -> &[WorkspaceSubtreeManifest] {
        &self.manifest.subtrees
    }

    pub fn summary(&self) -> WorkspaceSummary {
        WorkspaceSummary {
            workspace_id: self.manifest.workspace_id.clone(),
            title: self.manifest.title.clone(),
            root: self.root.to_string_lossy().to_string(),
            mode: self.manifest.mode,
            status: self.status,
            subtrees: self.manifest.subtrees.clone(),
            registered_blocks: self.manifest.registered_blocks.clone(),
            created_at_ms: self.manifest.created_at_ms,
            updated_at_ms: self.manifest.updated_at_ms,
        }
    }

    pub fn close(mut self) -> Result<WorkspaceSummary> {
        self.manifest.updated_at_ms = now_ms();
        write_manifest(&self.root, &self.manifest)?;
        self.status = WorkspaceStatus::Closed;
        Ok(self.summary())
    }

    pub fn refresh(&mut self) -> Result<WorkspaceStatus> {
        self.status = evaluate_status(&self.root, &self.manifest);
        self.manifest.updated_at_ms = now_ms();
        write_manifest(&self.root, &self.manifest)?;
        Ok(self.status)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyVaultCompatibilityReport {
    pub root: String,
    pub manifest_path: String,
    pub vault_id: String,
    pub title: String,
    pub source_name: String,
    pub source_entity: String,
    pub table_count: usize,
    pub recommended_workspace_root: String,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyVaultMigrationPlan {
    pub source_root: String,
    pub target_root: String,
    pub backup_archive: String,
    pub steps: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyVaultMigrationReceipt {
    pub plan: LegacyVaultMigrationPlan,
    pub compatibility: LegacyVaultCompatibilityReport,
    pub workspace: WorkspaceSummary,
}

pub fn contract() -> WorkspaceContract {
    WorkspaceContract {
        schema_version: WORKSPACE_SCHEMA_VERSION.to_string(),
        manifest_file: WORKSPACE_MANIFEST_FILE.to_string(),
        default_mode: WorkspaceMode::ReadWrite,
        default_status: WorkspaceStatus::Ready,
        standard_kinds: WorkspaceKind::standard().to_vec(),
        registered_blocks: registered_block_names()
            .into_iter()
            .map(str::to_string)
            .collect(),
    }
}

pub fn default_workspace_kinds() -> [WorkspaceKind; 8] {
    WorkspaceKind::standard()
}

pub fn workspace_manifest_path(root: &Path) -> PathBuf {
    root.join(WORKSPACE_MANIFEST_FILE)
}

pub fn read_manifest(root: impl AsRef<Path>) -> Result<WorkspaceManifest> {
    let path = workspace_manifest_path(root.as_ref());
    let text = fs::read_to_string(&path).map_err(io_error("read workspace manifest"))?;
    let manifest: WorkspaceManifest = serde_json::from_str(&text).map_err(json_error)?;
    validate_manifest(&manifest, root.as_ref())?;
    Ok(manifest)
}

pub fn write_manifest(root: impl AsRef<Path>, manifest: &WorkspaceManifest) -> Result<()> {
    validate_manifest(manifest, root.as_ref())?;
    let path = workspace_manifest_path(root.as_ref());
    let text = serde_json::to_string_pretty(manifest).map_err(json_error)?;
    fs::write(path, text).map_err(io_error("write workspace manifest"))?;
    Ok(())
}

pub fn inspect_legacy_vault(root: impl AsRef<Path>) -> Result<LegacyVaultCompatibilityReport> {
    let root = root.as_ref();
    let manifest_path = root.join("manifest.json");
    let manifest = VaultManifest::read(&manifest_path).map_err(map_legacy_error)?;
    Ok(LegacyVaultCompatibilityReport {
        root: root.to_string_lossy().to_string(),
        manifest_path: manifest_path.to_string_lossy().to_string(),
        vault_id: manifest.vault_id.clone(),
        title: manifest.source.name.clone(),
        source_name: manifest.source.name.clone(),
        source_entity: manifest.source.entity.clone(),
        table_count: manifest.tables.len(),
        recommended_workspace_root: suggested_workspace_root(root).to_string_lossy().to_string(),
        notes: build_compatibility_notes(&manifest),
    })
}

pub fn build_legacy_vault_migration_plan(
    source_root: impl AsRef<Path>,
    target_root: impl AsRef<Path>,
) -> Result<LegacyVaultMigrationPlan> {
    let source_root = source_root.as_ref();
    let target_root = target_root.as_ref();
    let compatibility = inspect_legacy_vault(source_root)?;
    Ok(LegacyVaultMigrationPlan {
        source_root: source_root.to_string_lossy().to_string(),
        target_root: target_root.to_string_lossy().to_string(),
        backup_archive: suggested_backup_archive(target_root)
            .to_string_lossy()
            .to_string(),
        steps: vec![
            format!("backup legacy vault for {}", compatibility.vault_id),
            "create a workspace root with standard subtrees".to_string(),
            "persist the legacy compatibility report under system/".to_string(),
            "persist the legacy backup archive under system/".to_string(),
        ],
    })
}

pub fn migrate_legacy_vault_to_workspace(
    source_root: impl AsRef<Path>,
    target_root: impl AsRef<Path>,
) -> Result<LegacyVaultMigrationReceipt> {
    let source_root = source_root.as_ref();
    let target_root = target_root.as_ref();
    if target_root.exists() {
        return Err(TreeSpaceError::new(
            ErrorCode::TargetFrozen,
            "workspace migration target already exists",
        )
        .with_context("target", target_root.to_string_lossy().to_string()));
    }

    let compatibility = inspect_legacy_vault(source_root)?;
    let plan = build_legacy_vault_migration_plan(source_root, target_root)?;
    let backup_archive = PathBuf::from(&plan.backup_archive);
    if let Some(parent) = backup_archive.parent() {
        fs::create_dir_all(parent).map_err(io_error("prepare legacy backup archive"))?;
    }
    backup_vault(source_root, &backup_archive).map_err(map_legacy_error)?;

    let staging_root = migration_staging_root(target_root);
    let result = (|| {
        let workspace = WorkspaceHandle::create(&staging_root, compatibility.title.clone())?;
        let system_dir = staging_root.join("system");
        fs::copy(&backup_archive, system_dir.join("legacy-vault.zip"))
            .map_err(io_error("copy legacy vault archive"))?;
        let report = LegacyVaultMigrationReceipt {
            plan: plan.clone(),
            compatibility: compatibility.clone(),
            workspace: workspace.summary(),
        };
        let report_text = serde_json::to_string_pretty(&report).map_err(json_error)?;
        fs::write(system_dir.join("legacy-vault-migration.json"), report_text)
            .map_err(io_error("write legacy migration report"))?;
        let mut publish_manifest = read_manifest(&staging_root)?;
        publish_manifest.workspace_id = target_root.to_string_lossy().to_string();
        publish_manifest.updated_at_ms = now_ms();
        let publish_text = serde_json::to_string_pretty(&publish_manifest).map_err(json_error)?;
        fs::write(workspace_manifest_path(&staging_root), publish_text)
            .map_err(io_error("prepare migrated workspace manifest"))?;
        fs::rename(&staging_root, target_root).map_err(io_error("publish migrated workspace"))?;
        let published = WorkspaceHandle::open(target_root)?;
        Ok(LegacyVaultMigrationReceipt {
            workspace: published.summary(),
            ..report
        })
    })();

    if result.is_err() && staging_root.exists() {
        let _ = fs::remove_dir_all(&staging_root);
    }
    result
}

fn validate_manifest(manifest: &WorkspaceManifest, root: &Path) -> Result<()> {
    if manifest.schema_version != WORKSPACE_SCHEMA_VERSION {
        return Err(TreeSpaceError::new(
            ErrorCode::Unsupported,
            "workspace manifest schema version is not supported",
        )
        .with_context("version", manifest.schema_version.clone()));
    }
    if manifest.workspace_id.trim().is_empty() {
        return Err(TreeSpaceError::new(
            ErrorCode::ManifestInvalid,
            "workspace id must not be empty",
        ));
    }
    let expected = root.to_string_lossy().to_string();
    if manifest.workspace_id != expected {
        return Err(TreeSpaceError::new(
            ErrorCode::ManifestInvalid,
            "workspace id does not match workspace root",
        )
        .with_context("expected", expected)
        .with_context("actual", manifest.workspace_id.clone()));
    }
    for subtree in &manifest.subtrees {
        if subtree.directory.trim().is_empty() {
            return Err(TreeSpaceError::new(
                ErrorCode::ManifestInvalid,
                "workspace subtree directory must not be empty",
            ));
        }
    }
    Ok(())
}

fn evaluate_status(root: &Path, manifest: &WorkspaceManifest) -> WorkspaceStatus {
    if manifest.mode == WorkspaceMode::ReadOnly {
        return WorkspaceStatus::ReadOnly;
    }
    if manifest
        .subtrees
        .iter()
        .any(|subtree| !root.join(&subtree.directory).is_dir())
    {
        WorkspaceStatus::Degraded
    } else {
        WorkspaceStatus::Ready
    }
}

fn migration_staging_root(target_root: &Path) -> PathBuf {
    let parent = target_root.parent().unwrap_or_else(|| Path::new("."));
    let name = target_root
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("workspace");
    parent.join(format!(".{name}.migration-{}", now_ms()))
}

fn suggested_workspace_root(source_root: &Path) -> PathBuf {
    let name = source_root
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("workspace");
    source_root.with_file_name(format!("{name}-workspace"))
}

fn suggested_backup_archive(target_root: &Path) -> PathBuf {
    let name = target_root
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("workspace");
    target_root.with_file_name(format!("{name}-legacy-vault.zip"))
}

fn build_compatibility_notes(manifest: &VaultManifest) -> Vec<String> {
    let mut notes = vec![
        "legacy Vault is read-only compatible and should not be written directly".to_string(),
        "authority data should move into the Workspace tree; derived caches can be rebuilt"
            .to_string(),
    ];
    if manifest.tables.is_empty() {
        notes.push("no legacy table manifests were found".to_string());
    } else {
        notes.push(format!(
            "{} legacy table manifest(s) detected",
            manifest.tables.len()
        ));
    }
    notes
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn io_error(action: &'static str) -> impl Fn(std::io::Error) -> TreeSpaceError {
    move |error| {
        TreeSpaceError::new(ErrorCode::StorageCorrupt, format!("failed to {action}"))
            .with_context("detail", error.to_string())
    }
}

fn json_error(error: impl std::fmt::Display) -> TreeSpaceError {
    TreeSpaceError::new(
        ErrorCode::PayloadMalformed,
        "workspace manifest JSON is malformed",
    )
    .with_context("detail", error.to_string())
}

fn map_legacy_error(error: cistella_core::CoreError) -> TreeSpaceError {
    TreeSpaceError::new(ErrorCode::ManifestInvalid, error.to_string())
}

fn map_tree_space_error(error: tree_space::error::TreeSpaceError) -> TreeSpaceError {
    TreeSpaceError::new(error.code, error.message).with_context(
        "tree_space",
        serde_json::to_string(&error.context).unwrap_or_else(|_| "{}".to_string()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "cistella-workspace-{name}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn sample_legacy_manifest() -> VaultManifest {
        VaultManifest {
            format_version: "0.1.0".to_string(),
            vault_id: "legacy-vault".to_string(),
            logical_schema_version: "0.1.0".to_string(),
            created_at: "2026-09-01T00:00:00Z".to_string(),
            source: cistella_core::VaultSourceProvenance {
                name: "Legacy Library".to_string(),
                entity: "sources".to_string(),
                snapshot_date: Some("2026-09-01".to_string()),
                input_path: "input".to_string(),
            },
            tables: std::collections::BTreeMap::new(),
        }
    }

    fn write_legacy_vault(root: &Path) {
        fs::create_dir_all(root.join("tables")).unwrap();
        fs::create_dir_all(root.join("user")).unwrap();
        sample_legacy_manifest()
            .write_pretty(root.join("manifest.json"))
            .unwrap();
        fs::write(root.join("user").join("literature_items.json"), b"[]").unwrap();
    }

    #[test]
    fn inspect_legacy_vault_returns_compatibility_report() {
        let root = temp_root("legacy-inspect");
        fs::create_dir_all(&root).unwrap();
        write_legacy_vault(&root);
        let report = inspect_legacy_vault(&root).unwrap();
        assert_eq!(report.vault_id, "legacy-vault");
        assert_eq!(report.source_name, "Legacy Library");
        assert_eq!(report.table_count, 0);
        assert!(report.recommended_workspace_root.contains("workspace"));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn migrate_legacy_vault_to_workspace_creates_workspace_and_archive() {
        let source = temp_root("legacy-source");
        let target = temp_root("legacy-target");
        fs::create_dir_all(&source).unwrap();
        write_legacy_vault(&source);
        let receipt = migrate_legacy_vault_to_workspace(&source, &target).unwrap();
        assert_eq!(receipt.workspace.title, "Legacy Library");
        assert!(target.join("workspace.json").exists());
        assert!(target.join("system").join("legacy-vault.zip").exists());
        assert!(
            target
                .join("system")
                .join("legacy-vault-migration.json")
                .exists()
        );
        fs::remove_dir_all(&source).unwrap();
        fs::remove_dir_all(&target).unwrap();
    }

    #[test]
    fn migrate_legacy_vault_refuses_existing_target() {
        let source = temp_root("legacy-existing-source");
        let target = temp_root("legacy-existing-target");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&target).unwrap();
        write_legacy_vault(&source);
        fs::write(target.join("keep.txt"), b"keep").unwrap();
        let error = migrate_legacy_vault_to_workspace(&source, &target).unwrap_err();
        assert_eq!(error.code, ErrorCode::TargetFrozen);
        assert_eq!(fs::read(target.join("keep.txt")).unwrap(), b"keep");
        fs::remove_dir_all(&source).unwrap();
        fs::remove_dir_all(&target).unwrap();
    }

    #[test]
    fn create_writes_workspace_manifest_and_standard_subtrees() {
        let root = temp_root("create");
        let handle = WorkspaceHandle::create(&root, "My Workspace").unwrap();
        assert_eq!(handle.status(), WorkspaceStatus::Ready);
        assert_eq!(handle.subtrees().len(), 8);
        assert!(workspace_manifest_path(&root).is_file());
        assert!(root.join("literature").is_dir());
        assert!(root.join("assets").is_dir());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn open_roundtrips_manifest_and_summary() {
        let root = temp_root("open");
        let created = WorkspaceHandle::create(&root, "My Workspace").unwrap();
        let opened = WorkspaceHandle::open(&root).unwrap();
        assert_eq!(opened.summary().title, "My Workspace");
        assert_eq!(
            opened.summary().registered_blocks,
            created.summary().registered_blocks
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn missing_subtree_marks_workspace_degraded() {
        let root = temp_root("degraded");
        let _created = WorkspaceHandle::create(&root, "My Workspace").unwrap();
        fs::remove_dir_all(root.join("assets")).unwrap();
        let reopened = WorkspaceHandle::open(&root).unwrap();
        assert_eq!(reopened.status(), WorkspaceStatus::Degraded);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn read_only_manifest_forces_read_only_status() {
        let root = temp_root("readonly");
        let mut created = WorkspaceHandle::create(&root, "My Workspace").unwrap();
        created.manifest.mode = WorkspaceMode::ReadOnly;
        write_manifest(&root, &created.manifest).unwrap();
        let reopened = WorkspaceHandle::open(&root).unwrap();
        assert_eq!(reopened.status(), WorkspaceStatus::ReadOnly);
        fs::remove_dir_all(&root).unwrap();
    }
}
