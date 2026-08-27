use std::{
    fs,
    path::{Path, PathBuf},
};

use cistella_core::{
    AnnotationResolution, CoreError, DocumentAsset, DocumentAssetKind, DocumentAssetStorageKind,
    LiteratureItemDraft, LiteratureItemType, ReadingStatus, Vault, VaultOpenOptions,
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

fn write_vault(root: &Path) -> Vault {
    fs::write(
        root.join("manifest.json"),
        r#"{
  "format_version": "0.1.0",
  "vault_id": "work-order-11-m2",
  "logical_schema_version": "0.1.0",
  "created_at": "2026-08-27T00:00:00Z",
  "source": { "name": "test", "entity": "sources", "snapshot_date": null, "input_path": "fixture" },
  "tables": {}
}"#,
    )
    .expect("write manifest");
    Vault::open(root, VaultOpenOptions::default()).expect("open minimum Vault")
}

fn item_draft(title: &str) -> LiteratureItemDraft {
    LiteratureItemDraft {
        title: title.to_string(),
        authors: vec!["Ada Lovelace".to_string()],
        published_year: Some(1843),
        item_type: LiteratureItemType::Article,
        favorite: false,
        reading_status: ReadingStatus::Inbox,
        tags: vec!["m2".to_string()],
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

fn write_single_page_pdf(path: &Path, text: &str) {
    build_single_page_pdf(text).save(path).unwrap();
}

fn build_duplicate_text_pdf() -> lopdf::Document {
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
        Operation::new("Tj", vec![Object::string_literal("the alpha dog")]),
        Operation::new("ET", vec![]),
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 12.into()]),
        Operation::new("Td", vec![50.into(), 650.into()]),
        Operation::new("Tj", vec![Object::string_literal("the alpha cat")]),
        Operation::new("ET", vec![]),
    ];
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

fn import_test_pdf(vault: &Vault, item_id: Uuid, path: &Path) -> Uuid {
    let result = vault
        .import_document_asset(item_id, path, DocumentAssetKind::Primary)
        .expect("import PDF asset");
    match result {
        cistella_core::DocumentAssetImportResult::Imported { asset } => asset.asset_id,
        cistella_core::DocumentAssetImportResult::Duplicate { existing } => existing.asset_id,
    }
}

fn annotation_path(root: &Path, annotation_id: Uuid) -> PathBuf {
    root.join("user")
        .join("annotations")
        .join(format!("{annotation_id}.json"))
}

fn set_annotation_field(root: &Path, annotation_id: Uuid, field: &str, value: serde_json::Value) {
    let path = annotation_path(root, annotation_id);
    let mut json: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    if field.starts_with("anchor.") {
        let anchor_field = field.strip_prefix("anchor.").unwrap();
        json["anchor"][anchor_field] = value;
    } else {
        json[field] = value;
    }
    fs::write(&path, serde_json::to_vec_pretty(&json).unwrap()).unwrap();
}

#[test]
fn m2_create_and_resolve_exact_on_vault_pdf() {
    let root = temp_dir("cistella-work-order-11-m2-exact");
    let result = (|| {
        let vault = write_vault(&root);
        let item = vault
            .create_literature_item(item_draft("M2 Exact"))
            .unwrap();
        let pdf_path = root.join("source.pdf");
        write_single_page_pdf(&pdf_path, "anchor text here");
        let asset_id = import_test_pdf(&vault, item.item_id, &pdf_path);

        let annotation = vault
            .create_quote_annotation(
                item.item_id,
                asset_id,
                1,
                "anchor text".to_string(),
                String::new(),
                " here".to_string(),
            )
            .expect("create quote annotation");
        assert_eq!(annotation.item_id, item.item_id);
        assert_eq!(annotation.asset_id, asset_id);
        assert_eq!(annotation.anchor.selected_text, "anchor text");
        assert!(!annotation.anchor.asset_content_hash_at_capture.is_empty());
        assert!(!annotation.anchor.normalized_text_hash.is_empty());
        assert_eq!(
            vault.resolve_annotation(annotation.annotation_id).unwrap(),
            AnnotationResolution::ResolvedExact
        );
        Ok::<(), String>(())
    })();
    let _ = fs::remove_dir_all(&root);
    result.expect("M2 exact resolution");
}

#[test]
fn m2_content_changed_after_pdf_replacement() {
    let root = temp_dir("cistella-work-order-11-m2-changed");
    let result = (|| {
        let vault = write_vault(&root);
        let item = vault
            .create_literature_item(item_draft("M2 Changed"))
            .unwrap();
        let pdf_path = root.join("source.pdf");
        write_single_page_pdf(&pdf_path, "anchor text here");
        let asset_id = import_test_pdf(&vault, item.item_id, &pdf_path);

        let annotation = vault
            .create_quote_annotation(
                item.item_id,
                asset_id,
                1,
                "anchor text".to_string(),
                String::new(),
                " here".to_string(),
            )
            .unwrap();

        // Replace the Vault PDF file with different content while keeping the
        // asset record intact.
        let vault_pdf_path = vault
            .resolve_document_asset_path(item.item_id, asset_id)
            .unwrap();
        write_single_page_pdf(&vault_pdf_path, "completely different content");
        let resolved = vault.resolve_annotation(annotation.annotation_id).unwrap();
        assert_eq!(resolved, AnnotationResolution::InvalidatedContentChanged);
        Ok::<(), String>(())
    })();
    let _ = fs::remove_dir_all(&root);
    result.expect("M2 content changed invalidation");
}

#[test]
fn m2_missing_asset_after_pdf_deletion() {
    let root = temp_dir("cistella-work-order-11-m2-missing");
    let result = (|| {
        let vault = write_vault(&root);
        let item = vault
            .create_literature_item(item_draft("M2 Missing"))
            .unwrap();
        let pdf_path = root.join("source.pdf");
        write_single_page_pdf(&pdf_path, "anchor text here");
        let asset_id = import_test_pdf(&vault, item.item_id, &pdf_path);

        let annotation = vault
            .create_quote_annotation(
                item.item_id,
                asset_id,
                1,
                "anchor text".to_string(),
                String::new(),
                " here".to_string(),
            )
            .unwrap();

        let asset_path = vault
            .resolve_document_asset_path(item.item_id, asset_id)
            .unwrap();
        fs::remove_file(&asset_path).unwrap();
        let resolved = vault.resolve_annotation(annotation.annotation_id).unwrap();
        assert_eq!(resolved, AnnotationResolution::UnavailableMissingAsset);
        Ok::<(), String>(())
    })();
    let _ = fs::remove_dir_all(&root);
    result.expect("M2 missing asset resolution");
}

#[test]
fn m2_external_asset_forbidden_for_quote_annotation() {
    let root = temp_dir("cistella-work-order-11-m2-external");
    let result = (|| {
        let vault = write_vault(&root);
        let item = vault
            .create_literature_item(item_draft("M2 External"))
            .unwrap();
        let external_pdf = root.join("external.pdf");
        write_single_page_pdf(&external_pdf, "external anchor text");
        let asset = vault
            .link_external_document_asset(item.item_id, &external_pdf, DocumentAssetKind::Primary)
            .unwrap();

        let error = vault
            .create_quote_annotation(
                item.item_id,
                asset.asset_id,
                1,
                "external anchor".to_string(),
                String::new(),
                " text".to_string(),
            )
            .unwrap_err();
        assert!(matches!(
            error,
            CoreError::AnnotationExternalAssetForbidden { .. }
        ));
        Ok::<(), String>(())
    })();
    let _ = fs::remove_dir_all(&root);
    result.expect("M2 external asset forbidden");
}

#[test]
fn m2_page_out_of_range_after_annotation_mutation() {
    let root = temp_dir("cistella-work-order-11-m2-page-range");
    let result = (|| {
        let vault = write_vault(&root);
        let item = vault
            .create_literature_item(item_draft("M2 Page Range"))
            .unwrap();
        let pdf_path = root.join("source.pdf");
        write_single_page_pdf(&pdf_path, "anchor text here");
        let asset_id = import_test_pdf(&vault, item.item_id, &pdf_path);

        let annotation = vault
            .create_quote_annotation(
                item.item_id,
                asset_id,
                1,
                "anchor text".to_string(),
                String::new(),
                " here".to_string(),
            )
            .unwrap();

        // Mutate the persisted annotation to point to a page that does not
        // exist in the unchanged PDF. The file hash stays the same, so the
        // resolver reaches the page-range check.
        set_annotation_field(
            &root,
            annotation.annotation_id,
            "anchor.pageNumber",
            2.into(),
        );
        let resolved = vault.resolve_annotation(annotation.annotation_id).unwrap();
        assert_eq!(resolved, AnnotationResolution::InvalidatedPageOutOfRange);
        Ok::<(), String>(())
    })();
    let _ = fs::remove_dir_all(&root);
    result.expect("M2 page out of range");
}

#[test]
fn m2_text_not_found_after_annotation_mutation() {
    let root = temp_dir("cistella-work-order-11-m2-text-not-found");
    let result = (|| {
        let vault = write_vault(&root);
        let item = vault
            .create_literature_item(item_draft("M2 Text Not Found"))
            .unwrap();
        let pdf_path = root.join("source.pdf");
        write_single_page_pdf(&pdf_path, "anchor text here");
        let asset_id = import_test_pdf(&vault, item.item_id, &pdf_path);

        let annotation = vault
            .create_quote_annotation(
                item.item_id,
                asset_id,
                1,
                "anchor text".to_string(),
                String::new(),
                " here".to_string(),
            )
            .unwrap();

        set_annotation_field(
            &root,
            annotation.annotation_id,
            "anchor.selectedText",
            "missing text".into(),
        );
        let resolved = vault.resolve_annotation(annotation.annotation_id).unwrap();
        assert_eq!(resolved, AnnotationResolution::InvalidatedTextNotFound);
        Ok::<(), String>(())
    })();
    let _ = fs::remove_dir_all(&root);
    result.expect("M2 text not found");
}

#[test]
fn m2_ambiguous_text_when_context_is_removed() {
    let root = temp_dir("cistella-work-order-11-m2-ambiguous");
    let result = (|| {
        let vault = write_vault(&root);
        let item = vault
            .create_literature_item(item_draft("M2 Ambiguous"))
            .unwrap();
        let pdf_path = root.join("source.pdf");
        build_duplicate_text_pdf().save(&pdf_path).unwrap();
        let asset_id = import_test_pdf(&vault, item.item_id, &pdf_path);

        // Context uniquely identifies the first occurrence at creation time.
        let annotation = vault
            .create_quote_annotation(
                item.item_id,
                asset_id,
                1,
                "alpha".to_string(),
                "the ".to_string(),
                " dog".to_string(),
            )
            .unwrap();

        // Remove contexts so the same selected text now matches twice.
        set_annotation_field(
            &root,
            annotation.annotation_id,
            "anchor.prefixContext",
            "".into(),
        );
        set_annotation_field(
            &root,
            annotation.annotation_id,
            "anchor.suffixContext",
            "".into(),
        );
        let resolved = vault.resolve_annotation(annotation.annotation_id).unwrap();
        assert_eq!(resolved, AnnotationResolution::InvalidatedAmbiguousText);
        Ok::<(), String>(())
    })();
    let _ = fs::remove_dir_all(&root);
    result.expect("M2 ambiguous text");
}

#[test]
fn m2_orphaned_item_after_literature_item_deletion() {
    let root = temp_dir("cistella-work-order-11-m2-orphan");
    let result = (|| {
        let vault = write_vault(&root);
        let item = vault
            .create_literature_item(item_draft("M2 Orphan"))
            .unwrap();
        let pdf_path = root.join("source.pdf");
        write_single_page_pdf(&pdf_path, "anchor text here");
        let asset_id = import_test_pdf(&vault, item.item_id, &pdf_path);

        let annotation = vault
            .create_quote_annotation(
                item.item_id,
                asset_id,
                1,
                "anchor text".to_string(),
                String::new(),
                " here".to_string(),
            )
            .unwrap();

        vault.delete_literature_item(item.item_id).unwrap();
        let resolved = vault.resolve_annotation(annotation.annotation_id).unwrap();
        assert_eq!(resolved, AnnotationResolution::OrphanedItem);
        Ok::<(), String>(())
    })();
    let _ = fs::remove_dir_all(&root);
    result.expect("M2 orphaned item");
}

#[test]
fn m2_non_pdf_asset_forbidden_for_quote_annotation() {
    let root = temp_dir("cistella-work-order-11-m2-non-pdf");
    let result = (|| {
        let vault = write_vault(&root);
        let item = vault
            .create_literature_item(item_draft("M2 Non PDF"))
            .unwrap();

        // Manually inject a non-PDF Vault asset record; the Core PDF import
        // path only accepts PDFs, so this is the only way to reach a
        // non-PDF asset.
        let asset_id = Uuid::new_v4();
        let relative_path = format!("files/{}/notes.txt", item.item_id);
        let asset = DocumentAsset {
            asset_id,
            item_id: item.item_id,
            asset_kind: DocumentAssetKind::Primary,
            storage_kind: DocumentAssetStorageKind::Vault,
            path: relative_path.clone(),
            display_name: "notes.txt".to_string(),
            media_type: "text/plain".to_string(),
            file_size: None,
            content_hash: None,
            imported_at: None,
            is_default: false,
        };
        vault.save_document_assets(&[asset]).unwrap();

        let text_path = root.join(&relative_path);
        fs::create_dir_all(text_path.parent().unwrap()).unwrap();
        fs::write(&text_path, "plain text").unwrap();

        let error = vault
            .create_quote_annotation(
                item.item_id,
                asset_id,
                1,
                "plain".to_string(),
                String::new(),
                String::new(),
            )
            .unwrap_err();
        assert!(matches!(
            error,
            CoreError::AnnotationAssetUnavailable { .. }
        ));
        Ok::<(), String>(())
    })();
    let _ = fs::remove_dir_all(&root);
    result.expect("M2 non-PDF asset forbidden");
}
