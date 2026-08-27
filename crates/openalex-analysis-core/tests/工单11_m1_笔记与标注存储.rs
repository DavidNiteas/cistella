use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Barrier},
    thread,
};

use cistella_core::{
    ANNOTATIONS_RELATIVE_DIR, AnnotationDraft, CoreError, NOTES_RELATIVE_DIR, NoteDraft,
    QuoteAnchor, Vault, VaultOpenOptions,
};
use uuid::Uuid;

fn temp_dir(prefix: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("{prefix}-{}", Uuid::new_v4()));
    fs::create_dir_all(&path).expect("create temporary directory");
    path
}

fn write_vault(root: &Path) -> Vault {
    fs::write(
        root.join("manifest.json"),
        r#"{
  "format_version": "0.1.0",
  "vault_id": "work-order-11-m1",
  "logical_schema_version": "0.1.0",
  "created_at": "2026-08-26T00:00:00Z",
  "source": { "name": "test", "entity": "sources", "snapshot_date": null, "input_path": "fixture" },
  "tables": {}
}"#,
    )
    .expect("write manifest");
    Vault::open(root, VaultOpenOptions::default()).expect("open minimum Vault")
}

fn anchor(asset_id: Uuid) -> QuoteAnchor {
    QuoteAnchor {
        asset_id,
        asset_content_hash_at_capture: "a".repeat(64),
        extractor_version: "m1-fixture".to_string(),
        page_number: 3,
        selected_text: "portable quote".to_string(),
        normalized_text_hash: "b".repeat(64),
        prefix_context: "prefix".to_string(),
        suffix_context: "suffix".to_string(),
    }
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create copy destination");
    for entry in fs::read_dir(from).expect("read source tree") {
        let entry = entry.expect("read source entry");
        let source = entry.path();
        let destination = to.join(entry.file_name());
        if entry.file_type().expect("read source type").is_dir() {
            copy_tree(&source, &destination);
        } else {
            fs::copy(&source, &destination).expect("copy source file");
        }
    }
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
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
                    fs::read(path).expect("read snapshot file"),
                );
            }
        }
    }
    let mut values = BTreeMap::new();
    visit(root, root, &mut values);
    values
}

#[test]
fn m1_crud_archive_delete_conflict_and_portability() {
    let root = temp_dir("cistella-work-order-11-m1");
    let copied_root = temp_dir("cistella-work-order-11-m1-copy");
    let result = (|| {
        let vault = write_vault(&root);
        let item_id = Uuid::new_v4();
        let note = vault
            .create_note(NoteDraft {
                item_id,
                title: "M1 portable note".to_string(),
                markdown_body: "# body\n\nportable".to_string(),
            })
            .expect("create note");
        assert_eq!(note.archived_at, None);
        assert_eq!(
            vault.list_notes(Some(item_id), false).unwrap(),
            vec![note.clone()]
        );

        let updated = vault
            .update_note(
                note.note_id,
                &note.revision,
                "Edited note".to_string(),
                "edited markdown".to_string(),
            )
            .expect("update note");
        assert_ne!(updated.revision, note.revision);
        assert!(matches!(
            vault.update_note(
                note.note_id,
                &note.revision,
                "stale".to_string(),
                String::new(),
            ),
            Err(CoreError::NoteConflict { .. })
        ));

        let archived = vault
            .archive_note(note.note_id, &updated.revision)
            .expect("archive note");
        assert!(archived.archived_at.is_some());
        assert!(vault.list_notes(Some(item_id), false).unwrap().is_empty());
        assert_eq!(vault.get_note(note.note_id).unwrap(), archived);
        let restored = vault
            .unarchive_note(note.note_id, &archived.revision)
            .expect("restore note");
        assert!(restored.archived_at.is_none());

        let asset_id = Uuid::new_v4();
        let annotation = vault
            .create_annotation(AnnotationDraft {
                item_id,
                asset_id,
                anchor: anchor(asset_id),
            })
            .expect("create annotation");
        assert_eq!(
            vault.get_annotation(annotation.annotation_id).unwrap(),
            annotation
        );
        let updated_annotation = vault
            .update_annotation(
                annotation.annotation_id,
                AnnotationDraft {
                    item_id,
                    asset_id,
                    anchor: QuoteAnchor {
                        selected_text: "updated portable quote".to_string(),
                        ..anchor(asset_id)
                    },
                },
            )
            .expect("update annotation");
        assert_eq!(updated_annotation.annotation_id, annotation.annotation_id);
        assert_eq!(updated_annotation.created_at, annotation.created_at);
        assert_eq!(
            vault.list_annotations(Some(item_id)).unwrap(),
            vec![updated_annotation.clone()]
        );
        vault
            .delete_annotation(annotation.annotation_id)
            .expect("delete annotation");
        assert!(matches!(
            vault.delete_annotation(annotation.annotation_id),
            Err(CoreError::AnnotationNotFound(_))
        ));

        copy_tree(&root, &copied_root);
        let copied =
            Vault::open(&copied_root, VaultOpenOptions::default()).expect("open copied Vault");
        assert_eq!(copied.get_note(note.note_id).unwrap(), restored);
        Ok::<(), String>(())
    })();
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&copied_root);
    result.expect("M1 CRUD/portable validation");
}

#[test]
fn m1_rejects_capacity_format_version_identity_and_unsafe_layouts() {
    let root = temp_dir("cistella-work-order-11-m1-safety");
    let outside = temp_dir("cistella-work-order-11-m1-outside");
    let result = (|| {
        let vault = write_vault(&root);
        let item_id = Uuid::new_v4();
        assert!(matches!(
            vault.create_note(NoteDraft {
                item_id,
                title: "x".repeat(241),
                markdown_body: String::new(),
            }),
            Err(CoreError::InvalidNoteInput(_))
        ));
        let asset_id = Uuid::new_v4();
        let mut too_large = anchor(asset_id);
        too_large.selected_text = "x".repeat(8_193);
        assert!(matches!(
            vault.create_annotation(AnnotationDraft {
                item_id,
                asset_id,
                anchor: too_large
            }),
            Err(CoreError::InvalidAnnotationInput(_))
        ));

        let note = vault
            .create_note(NoteDraft {
                item_id,
                title: "Valid".to_string(),
                markdown_body: String::new(),
            })
            .unwrap();
        let note_path = root
            .join(NOTES_RELATIVE_DIR)
            .join(format!("{}.md", note.note_id));
        let raw = fs::read_to_string(&note_path).unwrap();
        fs::write(
            &note_path,
            raw.replace("cistella: note/v1", "cistella: note/v9"),
        )
        .unwrap();
        assert!(matches!(
            vault.get_note(note.note_id),
            Err(CoreError::UnsupportedNoteVersion(_))
        ));
        fs::write(
            &note_path,
            raw.replace(&note.note_id.to_string(), &Uuid::new_v4().to_string()),
        )
        .unwrap();
        assert!(matches!(
            vault.get_note(note.note_id),
            Err(CoreError::NoteIdentityMismatch)
        ));

        let annotation = vault
            .create_annotation(AnnotationDraft {
                item_id,
                asset_id,
                anchor: anchor(asset_id),
            })
            .unwrap();
        let annotation_path = root
            .join(ANNOTATIONS_RELATIVE_DIR)
            .join(format!("{}.json", annotation.annotation_id));
        let json = fs::read_to_string(&annotation_path).unwrap();
        fs::write(
            &annotation_path,
            json.replace("annotation/v1", "annotation/v9"),
        )
        .unwrap();
        assert!(matches!(
            vault.get_annotation(annotation.annotation_id),
            Err(CoreError::UnsupportedAnnotationVersion(_))
        ));

        fs::remove_dir_all(root.join(NOTES_RELATIVE_DIR)).unwrap();
        fs::write(root.join(NOTES_RELATIVE_DIR), b"not a directory").unwrap();
        assert!(matches!(
            vault.create_note(NoteDraft {
                item_id,
                title: "blocked".to_string(),
                markdown_body: String::new()
            }),
            Err(CoreError::UnsafeUserRecordPath)
        ));

        // Reparse/symlink test follows the established project pattern. On a
        // Windows host without link privilege this is skipped after the
        // ordinary-object boundary above has already proven no-follow layout
        // rejection; that environment limitation is documented in the log.
        fs::remove_file(root.join(NOTES_RELATIVE_DIR)).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, root.join(NOTES_RELATIVE_DIR)).unwrap();
            assert!(matches!(
                vault.create_note(NoteDraft {
                    item_id,
                    title: "link".to_string(),
                    markdown_body: String::new()
                }),
                Err(CoreError::UnsafeUserRecordPath)
            ));
        }
        #[cfg(windows)]
        {
            if std::os::windows::fs::symlink_dir(&outside, root.join(NOTES_RELATIVE_DIR)).is_ok() {
                assert!(matches!(
                    vault.create_note(NoteDraft {
                        item_id,
                        title: "link".to_string(),
                        markdown_body: String::new()
                    }),
                    Err(CoreError::UnsafeUserRecordPath)
                ));
            }
        }
        Ok::<(), String>(())
    })();
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&outside);
    result.expect("M1 safety validation");
}

#[test]
fn m1_writes_only_approved_single_record_directories() {
    let root = temp_dir("cistella-work-order-11-m1-whitelist");
    let result = (|| {
        let vault = write_vault(&root);
        let item_id = Uuid::new_v4();
        let before = snapshot(&root);
        let note = vault
            .create_note(NoteDraft {
                item_id,
                title: "whitelist".to_string(),
                markdown_body: "body".to_string(),
            })
            .unwrap();
        let asset_id = Uuid::new_v4();
        let annotation = vault
            .create_annotation(AnnotationDraft {
                item_id,
                asset_id,
                anchor: anchor(asset_id),
            })
            .unwrap();
        vault
            .update_note(
                note.note_id,
                &note.revision,
                "changed".to_string(),
                "body".to_string(),
            )
            .unwrap();
        vault.delete_annotation(annotation.annotation_id).unwrap();
        let after = snapshot(&root);
        for (path, bytes) in &after {
            if before.get(path) != Some(bytes) {
                assert!(
                    path.starts_with("user/notes") || path.starts_with("user/annotations"),
                    "unexpected write outside approved user record directories: {}",
                    path.display()
                );
            }
        }
        assert!(after.keys().all(|path| {
            before.contains_key(path)
                || path.starts_with("user/notes")
                || path.starts_with("user/annotations")
        }));
        Ok::<(), String>(())
    })();
    let _ = fs::remove_dir_all(&root);
    result.expect("M1 whitelist validation");
}

#[test]
fn m1_rejects_unknown_annotation_fields_without_rewriting_authority_bytes() {
    let root = temp_dir("cistella-work-order-11-m1-unknown-annotation-fields");
    let result = (|| {
        let vault = write_vault(&root);
        let item_id = Uuid::new_v4();
        let asset_id = Uuid::new_v4();
        let annotation = vault
            .create_annotation(AnnotationDraft {
                item_id,
                asset_id,
                anchor: anchor(asset_id),
            })
            .unwrap();
        let path = root
            .join(ANNOTATIONS_RELATIVE_DIR)
            .join(format!("{}.json", annotation.annotation_id));
        let original = fs::read(&path).unwrap();

        let mut top_level: serde_json::Value = serde_json::from_slice(&original).unwrap();
        top_level
            .as_object_mut()
            .unwrap()
            .insert("unexpected".to_string(), serde_json::Value::Bool(true));
        let top_level_bytes = serde_json::to_vec_pretty(&top_level).unwrap();
        fs::write(&path, &top_level_bytes).unwrap();
        assert!(matches!(
            vault.get_annotation(annotation.annotation_id),
            Err(CoreError::MalformedAnnotation)
        ));
        assert_eq!(fs::read(&path).unwrap(), top_level_bytes);

        let mut nested: serde_json::Value = serde_json::from_slice(&original).unwrap();
        nested["anchor"]
            .as_object_mut()
            .unwrap()
            .insert("unexpected".to_string(), serde_json::Value::Bool(true));
        let nested_bytes = serde_json::to_vec_pretty(&nested).unwrap();
        fs::write(&path, &nested_bytes).unwrap();
        assert!(matches!(
            vault.get_annotation(annotation.annotation_id),
            Err(CoreError::MalformedAnnotation)
        ));
        assert_eq!(fs::read(&path).unwrap(), nested_bytes);
        Ok::<(), String>(())
    })();
    let _ = fs::remove_dir_all(&root);
    result.expect("unknown annotation fields must remain malformed and untouched");
}

#[test]
fn m1_keeps_notes_rooted_at_the_opened_vault_directory_handle() {
    let root = temp_dir("cistella-work-order-11-m1-root-handle");
    let moved = temp_dir("cistella-work-order-11-m1-root-handle-moved");
    let result = (|| {
        let vault = write_vault(&root);
        // This replacement uses ordinary directories, so it is available even
        // where symlink/junction creation is prohibited. It deterministically
        // proves that Notes does not call root_path() after Vault::open.
        fs::rename(&root, &moved).unwrap();
        fs::create_dir_all(&root).unwrap();
        let note = vault
            .create_note(NoteDraft {
                item_id: Uuid::new_v4(),
                title: "handle rooted".to_string(),
                markdown_body: String::new(),
            })
            .unwrap();
        assert!(
            moved
                .join(NOTES_RELATIVE_DIR)
                .join(format!("{}.md", note.note_id))
                .is_file()
        );
        assert!(!root.join(NOTES_RELATIVE_DIR).exists());
        Ok::<(), String>(())
    })();
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&moved);
    result.expect("Notes must retain the opened Vault root handle");
}

#[test]
fn m1_note_revision_cas_is_atomic_across_independent_vault_instances() {
    let root = temp_dir("cistella-work-order-11-m1-cas");
    let result = (|| {
        let vault = write_vault(&root);
        let note = vault
            .create_note(NoteDraft {
                item_id: Uuid::new_v4(),
                title: "before concurrent CAS".to_string(),
                markdown_body: "before".to_string(),
            })
            .unwrap();
        let root_a = root.clone();
        let root_b = root.clone();
        // Both independent Vault instances are held at the same barrier before
        // entering update_note. This deterministically puts two calls carrying
        // the same revision into the competing CAS path.
        let start = Arc::new(Barrier::new(3));
        let a_start = Arc::clone(&start);
        let b_start = Arc::clone(&start);
        let note_id = note.note_id;
        let revision = note.revision.clone();
        let a = thread::spawn(move || {
            let vault = Vault::open(&root_a, VaultOpenOptions::default()).unwrap();
            a_start.wait();
            vault.update_note(
                note_id,
                &revision,
                "writer A".to_string(),
                "body A".to_string(),
            )
        });
        let revision = note.revision.clone();
        let b = thread::spawn(move || {
            let vault = Vault::open(&root_b, VaultOpenOptions::default()).unwrap();
            b_start.wait();
            vault.update_note(
                note_id,
                &revision,
                "writer B".to_string(),
                "body B".to_string(),
            )
        });
        start.wait();
        let first = a.join().expect("writer A must not panic");
        let second = b.join().expect("writer B must not panic");
        let winner = match (first, second) {
            (Ok(winner), Err(CoreError::NoteConflict { .. }))
            | (Err(CoreError::NoteConflict { .. }), Ok(winner)) => winner,
            results => panic!("exactly one concurrent CAS writer must win: {results:?}"),
        };
        assert_eq!(vault.get_note(note_id).unwrap(), winner);

        // archive/unarchive use the same locked CAS helper. Two independent
        // unarchive requests with one expected revision must likewise have
        // exactly one winner.
        let archived = vault.archive_note(note_id, &winner.revision).unwrap();
        let root_a = root.clone();
        let root_b = root.clone();
        let start = Arc::new(Barrier::new(3));
        let a_start = Arc::clone(&start);
        let b_start = Arc::clone(&start);
        let revision = archived.revision.clone();
        let a = thread::spawn(move || {
            let vault = Vault::open(&root_a, VaultOpenOptions::default()).unwrap();
            a_start.wait();
            vault.unarchive_note(note_id, &revision)
        });
        let revision = archived.revision.clone();
        let b = thread::spawn(move || {
            let vault = Vault::open(&root_b, VaultOpenOptions::default()).unwrap();
            b_start.wait();
            vault.unarchive_note(note_id, &revision)
        });
        start.wait();
        let first = a.join().expect("unarchive A must not panic");
        let second = b.join().expect("unarchive B must not panic");
        let restored = match (first, second) {
            (Ok(restored), Err(CoreError::NoteConflict { .. }))
            | (Err(CoreError::NoteConflict { .. }), Ok(restored)) => restored,
            results => panic!("exactly one concurrent unarchive CAS must win: {results:?}"),
        };
        assert_eq!(vault.get_note(note_id).unwrap(), restored);
        assert!(restored.archived_at.is_none());

        let names: Vec<_> = fs::read_dir(root.join(NOTES_RELATIVE_DIR))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec![format!("{note_id}.md")]);
        Ok::<(), String>(())
    })();
    let _ = fs::remove_dir_all(&root);
    result.expect("Note CAS must serialize without temporary or lock-file residue");
}
