use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::import::source_record::SourceRecord;

/// Supported personal-library import formats.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ImportFormat {
    BibTeX,
    Ris,
    #[serde(rename = "openalex_works")]
    OpenAlexWorks,
}

impl ImportFormat {
    pub fn source_name(self) -> &'static str {
        match self {
            ImportFormat::BibTeX => "BibTeX",
            ImportFormat::Ris => "RIS",
            ImportFormat::OpenAlexWorks => "OpenAlex",
        }
    }

    pub fn from_source_name(name: &str) -> Option<Self> {
        if name.eq_ignore_ascii_case("bibtex") {
            Some(ImportFormat::BibTeX)
        } else if name.eq_ignore_ascii_case("ris") {
            Some(ImportFormat::Ris)
        } else if name.eq_ignore_ascii_case("openalex_works")
            || name.eq_ignore_ascii_case("openalex")
        {
            Some(ImportFormat::OpenAlexWorks)
        } else {
            None
        }
    }
}

/// Parses an external byte stream into a list of source-independent records.
pub trait RecordSource {
    fn parse(&self, bytes: &[u8]) -> Result<Vec<SourceRecord>>;
}
