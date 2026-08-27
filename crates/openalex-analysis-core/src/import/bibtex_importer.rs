use crate::{Result, Vault};

use super::{
    ImportFormat,
    conflict::{ImportPreview, ImportResult},
    library_importer::{LibraryImporter, LiteratureLibraryImporter},
    source_record::SourceRecord,
};

/// Imports BibTeX source records into the user's personal library.
///
/// This is a backwards-compatible wrapper around the generic
/// `LiteratureLibraryImporter`; new code should prefer
/// `LiteratureLibraryImporter::new(ImportFormat::BibTeX)`.
#[derive(Debug, Clone, Default)]
pub struct BibTeXLibraryImporter;

impl BibTeXLibraryImporter {
    pub fn new() -> Self {
        Self
    }
}

impl LibraryImporter for BibTeXLibraryImporter {
    fn preview(&self, vault: &Vault, records: Vec<SourceRecord>) -> Result<ImportPreview> {
        LiteratureLibraryImporter::new(ImportFormat::BibTeX).preview(vault, records)
    }

    fn commit(&self, vault: &Vault, preview: ImportPreview) -> Result<ImportResult> {
        LiteratureLibraryImporter::new(ImportFormat::BibTeX).commit(vault, preview)
    }
}

/// Loads all persisted source batches from the Vault.
pub use super::library_importer::load_source_batches;
