use std::{
    fs,
    path::{Path, PathBuf},
};

use cistella_core::{
    LiteratureFileKind, LiteratureItemDraft, LiteratureItemType, ReadingStatus, Vault,
    VaultOpenOptions,
};
use uuid::Uuid;

fn temp_dir(prefix: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("{prefix}-{}", Uuid::new_v4()));
    fs::create_dir_all(&path).expect("create temporary directory");
    path
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create copy destination");
    for entry in fs::read_dir(from).expect("read source tree") {
        let entry = entry.expect("read source entry");
        let destination = to.join(entry.file_name());
        let file_type = entry.file_type().expect("read source entry type");
        if file_type.is_dir() {
            copy_tree(&entry.path(), &destination);
        } else {
            fs::copy(entry.path(), destination).expect("copy source file");
        }
    }
}

fn draft(title: &str) -> LiteratureItemDraft {
    LiteratureItemDraft {
        title: title.to_string(),
        authors: vec!["M4 Portable Check".to_string()],
        published_year: Some(2026),
        item_type: LiteratureItemType::Article,
        favorite: false,
        reading_status: ReadingStatus::Inbox,
        tags: vec!["m4".to_string()],
        sources: Vec::new(),
        external_identifiers: Vec::new(),
    }
}

#[test]
fn copied_vault_keeps_vault_pdf_and_exposes_external_link_as_nonportable() {
    let original_root = temp_dir("cistella-m4-vault");
    let external_root = temp_dir("cistella-m4-external");
    let copied_root = temp_dir("cistella-m4-copy");

    let result = (|| {
        let source_table = original_root.join("sources.parquet");
        fs::write(&source_table, b"").expect("create source table placeholder");
        let vault = Vault::open_sources_file(&source_table, VaultOpenOptions::default())
            .expect("open original vault");
        let item = vault
            .create_literature_item(draft("Portable literature"))
            .expect("create literature item");

        let vault_source = original_root.join("incoming-paper.pdf");
        fs::write(&vault_source, b"vault pdf").expect("create vault source PDF");
        let vault_file = vault
            .add_literature_vault_file(item.item_id, &vault_source)
            .expect("add vault PDF");

        let external_source = external_root.join("external-paper.pdf");
        fs::write(&external_source, b"external pdf").expect("create external source PDF");
        let external_file = vault
            .add_literature_external_file(item.item_id, &external_source)
            .expect("add external PDF");

        assert_eq!(vault_file.kind, LiteratureFileKind::Vault);
        assert_eq!(
            Path::new(&vault_file.path),
            Path::new("files")
                .join(item.item_id.to_string())
                .join("incoming-paper.pdf")
        );
        assert!(
            vault
                .resolve_literature_file_path(item.item_id, vault_file.file_id)
                .is_ok()
        );
        assert_eq!(external_file.kind, LiteratureFileKind::External);
        assert!(Path::new(&external_file.path).is_absolute());
        assert!(!Path::new(&external_file.path).starts_with(&original_root));

        copy_tree(&original_root, &copied_root);
        let copied_vault = Vault::open_sources_file(
            copied_root.join("sources.parquet"),
            VaultOpenOptions::default(),
        )
        .expect("open copied vault");
        let copied_item = copied_vault
            .load_literature_items()
            .expect("load copied literature items")
            .into_iter()
            .find(|candidate| candidate.item_id == item.item_id)
            .expect("find copied literature item");
        let copied_vault_file = copied_item
            .files
            .iter()
            .find(|file| file.file_id == vault_file.file_id)
            .expect("find copied vault file");
        let copied_external_file = copied_item
            .files
            .iter()
            .find(|file| file.file_id == external_file.file_id)
            .expect("find copied external file");

        let copied_path = copied_vault
            .resolve_literature_file_path(item.item_id, vault_file.file_id)
            .expect("resolve copied vault PDF");
        assert!(copied_path.is_file());
        let canonical_copied_root =
            fs::canonicalize(&copied_root).expect("canonicalize copied vault root");
        let expected_copied_path = fs::canonicalize(copied_root.join(&copied_vault_file.path))
            .expect("canonicalize copied vault PDF");
        assert!(copied_path.starts_with(&canonical_copied_root));
        assert_eq!(copied_path, expected_copied_path);

        assert_eq!(copied_external_file.kind, LiteratureFileKind::External);
        assert!(Path::new(&copied_external_file.path).is_absolute());
        assert!(!Path::new(&copied_external_file.path).starts_with(&copied_root));
        fs::remove_file(&external_source).expect("remove original external PDF");
        assert!(
            copied_vault
                .resolve_literature_file_path(item.item_id, external_file.file_id)
                .is_err()
        );

        let copied_items_before_failure = copied_vault
            .load_literature_items()
            .expect("load copied items before missing-file check");
        fs::remove_file(&copied_path).expect("remove copied vault PDF");
        assert!(
            copied_vault
                .resolve_literature_file_path(item.item_id, vault_file.file_id)
                .is_err()
        );
        assert_eq!(
            copied_vault
                .load_literature_items()
                .expect("load copied items after missing-file check"),
            copied_items_before_failure
        );

        let mut temporary_files = Vec::new();
        for entry in walk_files(&copied_root) {
            let name = entry
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if name.starts_with(".literature_items.") && name.ends_with(".tmp") {
                temporary_files.push(entry);
            }
        }
        assert!(
            temporary_files.is_empty(),
            "temporary files remain: {temporary_files:?}"
        );
        Ok::<(), String>(())
    })();

    let _ = fs::remove_dir_all(&original_root);
    let _ = fs::remove_dir_all(&external_root);
    let _ = fs::remove_dir_all(&copied_root);
    result.expect("portable validation failed");
}

fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(root).expect("read tree while checking temporary files") {
        let entry = entry.expect("read tree entry");
        let path = entry.path();
        if entry.file_type().expect("read tree entry type").is_dir() {
            files.extend(walk_files(&path));
        } else {
            files.push(path);
        }
    }
    files
}
