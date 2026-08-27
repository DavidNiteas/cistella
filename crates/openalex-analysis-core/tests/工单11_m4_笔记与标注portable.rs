use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use cistella_core::{
    AnnotationDraft, AnnotationResolution, CoreError, DocumentAsset, DocumentAssetImportResult,
    DocumentAssetKind, DocumentAssetStatus, LiteratureItemDraft, LiteratureItemType, NoteDraft,
    QuoteAnchor, ReadingSessionState, ReadingStatus, Vault, VaultOpenOptions,
};
use lopdf::{
    Object, Stream,
    content::{Content, Operation},
    dictionary,
};
use uuid::Uuid;

fn temp_dir(prefix: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("{prefix}-{}", Uuid::new_v4()));
    fs::create_dir_all(&path).expect("create temporary directory");
    path
}

fn write_vault(root: &Path, vault_id: &str) -> Vault {
    fs::create_dir_all(root.join("tables")).expect("create table directory");
    fs::write(
        root.join("tables/sources.parquet"),
        b"M4 portable source table bytes",
    )
    .expect("write source table");
    fs::write(
        root.join("manifest.json"),
        format!(
            r#"{{
  "format_version": "0.1.0",
  "vault_id": "{vault_id}",
  "logical_schema_version": "0.1.0",
  "created_at": "2026-08-27T00:00:00Z",
  "source": {{
    "name": "M4 portable test",
    "entity": "sources",
    "snapshot_date": null,
    "input_path": "fixtures/sources"
  }},
  "tables": {{
    "sources": {{
      "rows": 0,
      "primary_key": ["openalex_id"],
      "parquet": {{ "path": "tables/sources.parquet", "size_bytes": 32 }},
      "arrow": null
    }}
  }}
}}"#
        ),
    )
    .expect("write manifest");
    Vault::open(root, VaultOpenOptions::default()).expect("open vault")
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create copy destination");
    for entry in fs::read_dir(from).expect("read source tree") {
        let entry = entry.expect("read source entry");
        let destination = to.join(entry.file_name());
        if entry.file_type().expect("read source entry type").is_dir() {
            copy_tree(&entry.path(), &destination);
        } else {
            fs::copy(entry.path(), destination).expect("copy source file");
        }
    }
}

fn item_draft(title: &str) -> LiteratureItemDraft {
    LiteratureItemDraft {
        title: title.to_string(),
        authors: vec!["M4 Portable".to_string()],
        published_year: Some(2026),
        item_type: LiteratureItemType::Article,
        favorite: false,
        reading_status: ReadingStatus::Inbox,
        tags: vec!["m4".to_string(), "portable".to_string()],
        sources: Vec::new(),
        external_identifiers: Vec::new(),
    }
}

fn build_single_page_pdf(text: &str) -> lopdf::Document {
    let mut document = lopdf::Document::with_version("1.5");
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
    let operations = vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 12.into()]),
        Operation::new("Td", vec![50.into(), 700.into()]),
        Operation::new("Tj", vec![Object::string_literal(text)]),
        Operation::new("ET", vec![]),
    ];
    let contents_id = document.add_object(Stream::new(
        dictionary! {},
        Content { operations }
            .encode()
            .expect("encode PDF operations"),
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

fn write_single_page_pdf(path: &Path, text: &str) {
    build_single_page_pdf(text)
        .save(path)
        .expect("write single page PDF");
}

fn import_pdf(vault: &Vault, item_id: Uuid, path: &Path) -> DocumentAsset {
    match vault
        .import_document_asset(item_id, path, DocumentAssetKind::Primary)
        .expect("import Vault PDF")
    {
        DocumentAssetImportResult::Imported { asset } => asset,
        DocumentAssetImportResult::Duplicate { existing } => existing,
    }
}

fn note_path(root: &Path, note_id: Uuid) -> PathBuf {
    root.join("user")
        .join("notes")
        .join(format!("{note_id}.md"))
}

fn annotation_path(root: &Path, annotation_id: Uuid) -> PathBuf {
    root.join("user")
        .join("annotations")
        .join(format!("{annotation_id}.json"))
}

fn snapshot_files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, current: &Path, values: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(current).expect("read snapshot directory") {
            let entry = entry.expect("read snapshot entry");
            let path = entry.path();
            if entry.file_type().expect("read snapshot type").is_dir() {
                visit(root, &path, values);
            } else {
                values.insert(
                    path.strip_prefix(root)
                        .expect("relative snapshot path")
                        .to_path_buf(),
                    fs::read(&path).expect("read snapshot file"),
                );
            }
        }
    }
    let mut values = BTreeMap::new();
    visit(root, root, &mut values);
    values
}

fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(root).expect("read tree") {
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

fn assert_no_note_or_annotation_temps(root: &Path) {
    let temps: Vec<_> = walk_files(root)
        .into_iter()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    (name.starts_with(".cistella-note") || name.starts_with(".cistella-annotation"))
                        && name.ends_with(".tmp")
                })
        })
        .collect();
    assert!(
        temps.is_empty(),
        "note/annotation temporary files remain: {temps:?}"
    );
}

fn set_readonly(path: &Path, readonly: bool) {
    let mut perms = fs::metadata(path)
        .expect("read file metadata")
        .permissions();
    perms.set_readonly(readonly);
    fs::set_permissions(path, perms).expect("set file readonly attribute");
}

fn assert_bytes_lack_absolute_roots(bytes: &[u8], roots: &[&PathBuf]) {
    let text = String::from_utf8_lossy(bytes);
    for root in roots {
        let canonical = fs::canonicalize(*root)
            .unwrap_or_else(|_| (*root).clone())
            .to_string_lossy()
            .to_string();
        assert!(
            !text.contains(&canonical),
            "record bytes must not contain absolute root path: {canonical}"
        );
        // Also check the non-canonical form in case the path was stored verbatim.
        let raw = root.to_string_lossy().to_string();
        if raw != canonical {
            assert!(
                !text.contains(&raw),
                "record bytes must not contain raw root path: {raw}"
            );
        }
    }
}

#[test]
fn m4_portable_copy_keeps_note_and_annotation_ids_readable() {
    let original_root = temp_dir("cistella-work-order-11-m4-original");
    let copied_root = temp_dir("cistella-work-order-11-m4-copy");
    let incoming_root = temp_dir("cistella-work-order-11-m4-incoming");

    let result = (|| {
        let vault = write_vault(&original_root, "work-order-11-m4-portable");
        let item = vault
            .create_literature_item(item_draft("Portable item"))
            .expect("create literature item");
        let pdf_source = incoming_root.join("source.pdf");
        write_single_page_pdf(&pdf_source, "portable annotation text here");
        let asset = import_pdf(&vault, item.item_id, &pdf_source);

        let note = vault
            .create_note(NoteDraft {
                item_id: item.item_id,
                title: "Portable note".to_string(),
                markdown_body: "# Portable body\n\ncistella portable note.".to_string(),
            })
            .expect("create note");
        let annotation = vault
            .create_quote_annotation(
                item.item_id,
                asset.asset_id,
                1,
                "portable annotation".to_string(),
                String::new(),
                " text here".to_string(),
            )
            .expect("create quote annotation");

        copy_tree(&original_root, &copied_root);
        let copied =
            Vault::open(&copied_root, VaultOpenOptions::default()).expect("open copied vault");

        let copied_note = copied.get_note(note.note_id).expect("read copied note");
        assert_eq!(copied_note.note_id, note.note_id);
        assert_eq!(copied_note.item_id, item.item_id);
        assert_eq!(copied_note.title, note.title);
        assert_eq!(copied_note.markdown_body, note.markdown_body);
        assert_eq!(copied_note.revision, note.revision);
        assert_eq!(
            copied
                .list_notes(Some(item.item_id), true)
                .expect("list copied notes"),
            vec![copied_note.clone()]
        );

        let copied_annotation = copied
            .get_annotation(annotation.annotation_id)
            .expect("read copied annotation");
        assert_eq!(copied_annotation.annotation_id, annotation.annotation_id);
        assert_eq!(copied_annotation.item_id, item.item_id);
        assert_eq!(copied_annotation.asset_id, asset.asset_id);
        assert_eq!(
            copied_annotation.anchor.selected_text,
            annotation.anchor.selected_text
        );
        assert_eq!(
            copied
                .list_annotations(Some(item.item_id))
                .expect("list copied annotations"),
            vec![copied_annotation.clone()]
        );

        // No absolute Vault path is persisted in the portable records.
        let note_bytes = fs::read(note_path(&copied_root, note.note_id)).expect("read copied note");
        let annotation_bytes = fs::read(annotation_path(&copied_root, annotation.annotation_id))
            .expect("read copied annotation");
        assert_bytes_lack_absolute_roots(&note_bytes, &[&original_root, &copied_root]);
        assert_bytes_lack_absolute_roots(&annotation_bytes, &[&original_root, &copied_root]);

        Ok::<(), String>(())
    })();

    let _ = fs::remove_dir_all(&original_root);
    let _ = fs::remove_dir_all(&copied_root);
    let _ = fs::remove_dir_all(&incoming_root);
    result.expect("portable copy readability failed");
}

#[test]
fn m4_annotation_resolves_in_copy_and_invalidates_after_pdf_changes() {
    let original_root = temp_dir("cistella-work-order-11-m4-resolve-original");
    let copied_root = temp_dir("cistella-work-order-11-m4-resolve-copy");
    let incoming_root = temp_dir("cistella-work-order-11-m4-resolve-incoming");

    let result = (|| {
        let vault = write_vault(&original_root, "work-order-11-m4-resolve");
        let item = vault
            .create_literature_item(item_draft("Resolution item"))
            .expect("create literature item");
        let pdf_source = incoming_root.join("resolve.pdf");
        write_single_page_pdf(&pdf_source, "resolution anchor text here");
        let asset = import_pdf(&vault, item.item_id, &pdf_source);

        let annotation = vault
            .create_quote_annotation(
                item.item_id,
                asset.asset_id,
                1,
                "resolution anchor".to_string(),
                String::new(),
                " text here".to_string(),
            )
            .expect("create annotation");

        copy_tree(&original_root, &copied_root);
        let copied =
            Vault::open(&copied_root, VaultOpenOptions::default()).expect("open copied vault");

        assert_eq!(
            copied
                .resolve_annotation(annotation.annotation_id)
                .expect("resolve in copy"),
            AnnotationResolution::ResolvedExact
        );

        let copied_pdf_path = copied
            .resolve_document_asset_path(item.item_id, asset.asset_id)
            .expect("resolve copied PDF path");
        write_single_page_pdf(&copied_pdf_path, "different content after copy");
        assert_eq!(
            copied
                .resolve_annotation(annotation.annotation_id)
                .expect("resolve after replacement"),
            AnnotationResolution::InvalidatedContentChanged
        );

        fs::remove_file(&copied_pdf_path).expect("remove copied PDF");
        assert_eq!(
            copied
                .resolve_annotation(annotation.annotation_id)
                .expect("resolve after deletion"),
            AnnotationResolution::UnavailableMissingAsset
        );

        let annotation_bytes = fs::read(annotation_path(&copied_root, annotation.annotation_id))
            .expect("annotation file remains");
        assert_bytes_lack_absolute_roots(&annotation_bytes, &[&original_root, &copied_root]);

        Ok::<(), String>(())
    })();

    let _ = fs::remove_dir_all(&original_root);
    let _ = fs::remove_dir_all(&copied_root);
    let _ = fs::remove_dir_all(&incoming_root);
    result.expect("annotation resolution in copy failed");
}

#[test]
fn m4_resolved_exact_opens_copied_asset_through_controlled_reading_path() {
    let original_root = temp_dir("cistella-work-order-11-m4-open-original");
    let copied_root = temp_dir("cistella-work-order-11-m4-open-copy");
    let incoming_root = temp_dir("cistella-work-order-11-m4-open-incoming");

    let result = (|| {
        let vault = write_vault(&original_root, "work-order-11-m4-open");
        let item = vault
            .create_literature_item(item_draft("Open asset item"))
            .expect("create literature item");
        let pdf_source = incoming_root.join("open.pdf");
        write_single_page_pdf(&pdf_source, "open asset text here");
        let asset = import_pdf(&vault, item.item_id, &pdf_source);

        let annotation = vault
            .create_quote_annotation(
                item.item_id,
                asset.asset_id,
                1,
                "open asset".to_string(),
                String::new(),
                " text here".to_string(),
            )
            .expect("create annotation");

        copy_tree(&original_root, &copied_root);
        let copied =
            Vault::open(&copied_root, VaultOpenOptions::default()).expect("open copied vault");

        assert_eq!(
            copied
                .resolve_annotation(annotation.annotation_id)
                .expect("resolve before open"),
            AnnotationResolution::ResolvedExact
        );

        let canonical_copied_root =
            fs::canonicalize(&copied_root).expect("canonicalize copied root");
        let opened_path = std::cell::RefCell::new(None);
        let session = copied
            .start_reading_session(item.item_id, asset.asset_id, |path| {
                let canonical = fs::canonicalize(path).expect("canonicalize opened path");
                assert!(canonical.is_file());
                assert!(
                    canonical.starts_with(&canonical_copied_root),
                    "opened path must be inside copied vault"
                );
                assert!(
                    !canonical.starts_with(&fs::canonicalize(&original_root).unwrap()),
                    "opened path must not leak back to original vault"
                );
                *opened_path.borrow_mut() = Some(canonical.clone());
                Ok(())
            })
            .expect("open copied asset through reading session");
        assert_eq!(session.item_id, item.item_id);
        assert_eq!(session.asset_id, asset.asset_id);
        assert_eq!(session.state, ReadingSessionState::Active);
        assert_eq!(
            opened_path.into_inner(),
            Some(
                copied
                    .resolve_document_asset_path(item.item_id, asset.asset_id)
                    .expect("resolve copied asset")
            )
        );

        Ok::<(), String>(())
    })();

    let _ = fs::remove_dir_all(&original_root);
    let _ = fs::remove_dir_all(&copied_root);
    let _ = fs::remove_dir_all(&incoming_root);
    result.expect("controlled open of copied asset failed");
}

#[test]
fn m4_crud_only_writes_to_user_notes_and_user_annotations() {
    let root = temp_dir("cistella-work-order-11-m4-whitelist");
    let incoming_root = temp_dir("cistella-work-order-11-m4-whitelist-incoming");

    let result = (|| {
        let vault = write_vault(&root, "work-order-11-m4-whitelist");
        let item = vault
            .create_literature_item(item_draft("Whitelist item"))
            .expect("create literature item");
        let pdf_source = incoming_root.join("whitelist.pdf");
        write_single_page_pdf(&pdf_source, "whitelist annotation text");
        let asset = import_pdf(&vault, item.item_id, &pdf_source);

        let before = snapshot_files(&root);

        let note = vault
            .create_note(NoteDraft {
                item_id: item.item_id,
                title: "Whitelist note".to_string(),
                markdown_body: "body".to_string(),
            })
            .expect("create note");
        let annotation = vault
            .create_quote_annotation(
                item.item_id,
                asset.asset_id,
                1,
                "whitelist".to_string(),
                String::new(),
                " annotation text".to_string(),
            )
            .expect("create annotation");

        let updated_note = vault
            .update_note(
                note.note_id,
                &note.revision,
                "Updated whitelist note".to_string(),
                "updated body".to_string(),
            )
            .expect("update note");
        let archived = vault
            .archive_note(note.note_id, &updated_note.revision)
            .expect("archive note");
        let restored = vault
            .unarchive_note(note.note_id, &archived.revision)
            .expect("unarchive note");

        let updated_annotation = vault
            .update_annotation(
                annotation.annotation_id,
                AnnotationDraft {
                    item_id: item.item_id,
                    asset_id: asset.asset_id,
                    anchor: QuoteAnchor {
                        selected_text: "updated whitelist".to_string(),
                        ..annotation.anchor.clone()
                    },
                },
            )
            .expect("update annotation");
        assert_eq!(updated_annotation.annotation_id, annotation.annotation_id);
        vault
            .delete_annotation(annotation.annotation_id)
            .expect("delete annotation");

        let after = snapshot_files(&root);

        // Every changed, added or removed file must live in one of the two
        // approved user-record directories.
        for (path, bytes) in &after {
            if before.get(path) != Some(bytes) || !before.contains_key(path) {
                assert!(
                    path.starts_with("user/notes") || path.starts_with("user/annotations"),
                    "unexpected write outside approved directories: {}",
                    path.display()
                );
            }
        }
        for path in before.keys() {
            if !after.contains_key(path) {
                assert!(
                    path.starts_with("user/notes") || path.starts_with("user/annotations"),
                    "unexpected deletion outside approved directories: {}",
                    path.display()
                );
            }
        }

        assert_eq!(
            vault.list_notes(Some(item.item_id), true).unwrap(),
            vec![restored]
        );
        assert!(
            vault
                .list_annotations(Some(item.item_id))
                .unwrap()
                .is_empty()
        );

        Ok::<(), String>(())
    })();

    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&incoming_root);
    result.expect("whitelist validation failed");
}

#[test]
fn m4_failed_writes_preserve_old_files_and_clean_temporary_files() {
    let root = temp_dir("cistella-work-order-11-m4-failure");
    let incoming_root = temp_dir("cistella-work-order-11-m4-failure-incoming");

    let result = (|| {
        let vault = write_vault(&root, "work-order-11-m4-failure");
        let item = vault
            .create_literature_item(item_draft("Failure item"))
            .expect("create literature item");
        let pdf_source = incoming_root.join("failure.pdf");
        write_single_page_pdf(&pdf_source, "failure annotation text");
        let asset = import_pdf(&vault, item.item_id, &pdf_source);

        let note = vault
            .create_note(NoteDraft {
                item_id: item.item_id,
                title: "Failure note".to_string(),
                markdown_body: "original body".to_string(),
            })
            .expect("create note");
        let annotation = vault
            .create_quote_annotation(
                item.item_id,
                asset.asset_id,
                1,
                "failure".to_string(),
                String::new(),
                " annotation text".to_string(),
            )
            .expect("create annotation");

        let note_file = note_path(&root, note.note_id);
        let annotation_file = annotation_path(&root, annotation.annotation_id);
        let note_bytes_before = fs::read(&note_file).expect("read note before failure");
        let annotation_bytes_before =
            fs::read(&annotation_file).expect("read annotation before failure");

        // Make the persisted records read-only so the atomic rename-replace
        // must fail while the temporary file can still be created and cleaned.
        set_readonly(&note_file, true);
        let note_error = vault
            .update_note(
                note.note_id,
                &note.revision,
                "should not commit".to_string(),
                "should not commit".to_string(),
            )
            .unwrap_err();
        set_readonly(&note_file, false);
        assert!(
            !matches!(note_error, CoreError::NoteConflict { .. }),
            "read-only failure must not be reported as a revision conflict"
        );
        assert_eq!(
            fs::read(&note_file).expect("read note after failed update"),
            note_bytes_before,
            "failed note update must keep the old record bytes"
        );

        set_readonly(&annotation_file, true);
        let annotation_error = vault
            .update_annotation(
                annotation.annotation_id,
                AnnotationDraft {
                    item_id: item.item_id,
                    asset_id: asset.asset_id,
                    anchor: QuoteAnchor {
                        selected_text: "should not commit".to_string(),
                        ..annotation.anchor.clone()
                    },
                },
            )
            .unwrap_err();
        set_readonly(&annotation_file, false);
        assert_eq!(
            fs::read(&annotation_file).expect("read annotation after failed update"),
            annotation_bytes_before,
            "failed annotation update must keep the old record bytes"
        );
        let _ = (annotation_error,);

        assert_no_note_or_annotation_temps(&root);

        // After restoring writability, normal operations succeed.
        vault
            .update_note(
                note.note_id,
                &note.revision,
                "recovered note".to_string(),
                "recovered body".to_string(),
            )
            .expect("update note after restoring writability");
        Ok::<(), String>(())
    })();

    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&incoming_root);
    result.expect("failed-write preservation and cleanup validation failed");
}

#[test]
fn m4_corruption_and_version_incompatibility_return_structured_errors() {
    let root = temp_dir("cistella-work-order-11-m4-corruption");
    let incoming_root = temp_dir("cistella-work-order-11-m4-corruption-incoming");

    let result = (|| {
        let vault = write_vault(&root, "work-order-11-m4-corruption");
        let item = vault
            .create_literature_item(item_draft("Corruption item"))
            .expect("create literature item");
        let pdf_source = incoming_root.join("corruption.pdf");
        write_single_page_pdf(&pdf_source, "corruption annotation text");
        let asset = import_pdf(&vault, item.item_id, &pdf_source);

        let note = vault
            .create_note(NoteDraft {
                item_id: item.item_id,
                title: "Corruption note".to_string(),
                markdown_body: "body".to_string(),
            })
            .expect("create note");
        let annotation = vault
            .create_quote_annotation(
                item.item_id,
                asset.asset_id,
                1,
                "corruption".to_string(),
                String::new(),
                " annotation text".to_string(),
            )
            .expect("create annotation");

        let note_file = note_path(&root, note.note_id);
        let annotation_file = annotation_path(&root, annotation.annotation_id);

        // Malformed note front matter.
        let original_note_bytes = fs::read(&note_file).expect("read original note");
        let malformed_note = b"---\ncistella: note/v1\nnote_id: not-a-uuid\n---\nbody".to_vec();
        fs::write(&note_file, &malformed_note).expect("write malformed note");
        assert!(matches!(
            vault.get_note(note.note_id),
            Err(CoreError::MalformedNote)
        ));
        assert_eq!(
            fs::read(&note_file).expect("note file unchanged"),
            malformed_note
        );

        // Unsupported note version.
        let versioned_note = String::from_utf8_lossy(&original_note_bytes)
            .replace("cistella: note/v1", "cistella: note/v9")
            .into_bytes();
        fs::write(&note_file, &versioned_note).expect("write versioned note");
        assert!(matches!(
            vault.get_note(note.note_id),
            Err(CoreError::UnsupportedNoteVersion(_))
        ));
        assert_eq!(
            fs::read(&note_file).expect("note file unchanged"),
            versioned_note
        );

        // Malformed annotation JSON.
        let original_annotation_bytes =
            fs::read(&annotation_file).expect("read original annotation");
        let malformed_annotation = b"{ not valid json".to_vec();
        fs::write(&annotation_file, &malformed_annotation).expect("write malformed annotation");
        assert!(matches!(
            vault.get_annotation(annotation.annotation_id),
            Err(CoreError::MalformedAnnotation)
        ));
        assert_eq!(
            fs::read(&annotation_file).expect("annotation file unchanged"),
            malformed_annotation
        );

        // Unsupported annotation version.
        let versioned_annotation = String::from_utf8_lossy(&original_annotation_bytes)
            .replace("annotation/v1", "annotation/v9")
            .into_bytes();
        fs::write(&annotation_file, &versioned_annotation).expect("write versioned annotation");
        assert!(matches!(
            vault.get_annotation(annotation.annotation_id),
            Err(CoreError::UnsupportedAnnotationVersion(_))
        ));
        assert_eq!(
            fs::read(&annotation_file).expect("annotation file unchanged"),
            versioned_annotation
        );

        Ok::<(), String>(())
    })();

    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&incoming_root);
    result.expect("corruption/version validation failed");
}

#[test]
fn m4_search_rebuild_and_deletion_do_not_affect_notes_and_annotations() {
    let root = temp_dir("cistella-work-order-11-m4-search");
    let incoming_root = temp_dir("cistella-work-order-11-m4-search-incoming");

    let result = (|| {
        let vault = write_vault(&root, "work-order-11-m4-search");
        let item = vault
            .create_literature_item(item_draft("Search isolation item"))
            .expect("create literature item");
        let pdf_source = incoming_root.join("search.pdf");
        write_single_page_pdf(&pdf_source, "search isolation text");
        let asset = import_pdf(&vault, item.item_id, &pdf_source);

        let note = vault
            .create_note(NoteDraft {
                item_id: item.item_id,
                title: "Search isolation note".to_string(),
                markdown_body: "search isolation body".to_string(),
            })
            .expect("create note");
        let annotation = vault
            .create_quote_annotation(
                item.item_id,
                asset.asset_id,
                1,
                "search isolation".to_string(),
                String::new(),
                " text".to_string(),
            )
            .expect("create annotation");

        vault
            .rebuild_metadata_search_index()
            .expect("build search index");

        let before = snapshot_files(&root);

        // Delete the entire derived search subtree and rebuild it without any
        // Notes/Annotations write. Existing user records must stay byte-identical.
        fs::remove_dir_all(root.join("derived/search")).expect("delete derived search");
        let without_index =
            Vault::open(&root, VaultOpenOptions::default()).expect("open vault without search");

        assert_eq!(
            without_index
                .get_note(note.note_id)
                .expect("read note after search deletion"),
            note
        );
        assert_eq!(
            without_index
                .get_annotation(annotation.annotation_id)
                .expect("read annotation after search deletion"),
            annotation
        );
        assert_eq!(
            without_index
                .resolve_annotation(annotation.annotation_id)
                .expect("resolve annotation without search"),
            AnnotationResolution::ResolvedExact
        );

        without_index
            .rebuild_metadata_search_index()
            .expect("rebuild search index");
        let after_search_rebuild = snapshot_files(&root);
        for (path, bytes) in &before {
            if path.starts_with("user/notes") || path.starts_with("user/annotations") {
                assert_eq!(
                    after_search_rebuild.get(path),
                    Some(bytes),
                    "Search rebuild must not mutate {path:?}"
                );
            }
        }

        // Delete the index again and prove Notes/Annotations CRUD and resolution
        // do not depend on Search.
        fs::remove_dir_all(root.join("derived/search")).expect("delete derived search again");
        let still_without_index =
            Vault::open(&root, VaultOpenOptions::default()).expect("open vault without search");

        let new_note = still_without_index
            .create_note(NoteDraft {
                item_id: item.item_id,
                title: "Created without search".to_string(),
                markdown_body: "body".to_string(),
            })
            .expect("create note without search");
        let new_annotation = still_without_index
            .create_quote_annotation(
                item.item_id,
                asset.asset_id,
                1,
                "search isolation".to_string(),
                String::new(),
                " text".to_string(),
            )
            .expect("create annotation without search");
        let updated = still_without_index
            .update_note(
                note.note_id,
                &note.revision,
                "Updated without search".to_string(),
                "updated body".to_string(),
            )
            .expect("update note without search");

        assert_eq!(
            still_without_index
                .resolve_annotation(annotation.annotation_id)
                .expect("resolve original annotation without search"),
            AnnotationResolution::ResolvedExact
        );
        assert_eq!(
            still_without_index
                .resolve_annotation(new_annotation.annotation_id)
                .expect("resolve new annotation without search"),
            AnnotationResolution::ResolvedExact
        );

        // A final rebuild must not affect the Notes/Annotations that were created
        // and updated while Search was missing.
        still_without_index
            .rebuild_metadata_search_index()
            .expect("final search rebuild");
        assert_eq!(
            still_without_index
                .get_note(note.note_id)
                .expect("read updated note after final rebuild"),
            updated
        );
        assert_eq!(
            still_without_index
                .get_note(new_note.note_id)
                .expect("read new note after final rebuild"),
            new_note
        );
        assert_eq!(
            still_without_index
                .get_annotation(annotation.annotation_id)
                .expect("read original annotation after final rebuild"),
            annotation
        );
        assert_eq!(
            still_without_index
                .get_annotation(new_annotation.annotation_id)
                .expect("read new annotation after final rebuild"),
            new_annotation
        );

        Ok::<(), String>(())
    })();

    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&incoming_root);
    result.expect("search isolation validation failed");
}

#[test]
fn m4_pdf_missing_or_replacement_does_not_mutate_user_records() {
    let root = temp_dir("cistella-work-order-11-m4-pdf-mutation");
    let incoming_root = temp_dir("cistella-work-order-11-m4-pdf-mutation-incoming");

    let result = (|| {
        let vault = write_vault(&root, "work-order-11-m4-pdf-mutation");
        let item = vault
            .create_literature_item(item_draft("PDF mutation item"))
            .expect("create literature item");
        let pdf_source = incoming_root.join("mutation.pdf");
        write_single_page_pdf(&pdf_source, "mutation anchor text here");
        let asset = import_pdf(&vault, item.item_id, &pdf_source);

        vault
            .create_note(NoteDraft {
                item_id: item.item_id,
                title: "PDF mutation note".to_string(),
                markdown_body: "mutation body".to_string(),
            })
            .expect("create note");
        let annotation = vault
            .create_quote_annotation(
                item.item_id,
                asset.asset_id,
                1,
                "mutation anchor".to_string(),
                String::new(),
                " text here".to_string(),
            )
            .expect("create annotation");

        let notes_before = snapshot_files(&root.join("user/notes"));
        let annotations_before = snapshot_files(&root.join("user/annotations"));

        let pdf_path = vault
            .resolve_document_asset_path(item.item_id, asset.asset_id)
            .expect("resolve PDF path");

        // Replace PDF content.
        write_single_page_pdf(&pdf_path, "mutated content not matching anchor");
        assert_eq!(
            vault
                .resolve_annotation(annotation.annotation_id)
                .expect("resolve after replacement"),
            AnnotationResolution::InvalidatedContentChanged
        );
        assert_eq!(
            snapshot_files(&root.join("user/notes")),
            notes_before,
            "PDF replacement must not change notes"
        );
        assert_eq!(
            snapshot_files(&root.join("user/annotations")),
            annotations_before,
            "PDF replacement must not change annotations"
        );

        // Delete PDF entirely.
        fs::remove_file(&pdf_path).expect("remove PDF");
        assert_eq!(
            vault
                .resolve_annotation(annotation.annotation_id)
                .expect("resolve after deletion"),
            AnnotationResolution::UnavailableMissingAsset
        );
        assert_eq!(
            snapshot_files(&root.join("user/notes")),
            notes_before,
            "PDF deletion must not change notes"
        );
        assert_eq!(
            snapshot_files(&root.join("user/annotations")),
            annotations_before,
            "PDF deletion must not change annotations"
        );

        assert_eq!(
            asset.status(&vault),
            DocumentAssetStatus::Missing,
            "asset status must reflect missing file without rewriting records"
        );
        assert_eq!(
            vault.load_document_assets().expect("reload assets"),
            vec![asset],
            "asset record must stay unchanged"
        );

        Ok::<(), String>(())
    })();

    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&incoming_root);
    result.expect("PDF mutation boundary validation failed");
}
