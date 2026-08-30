use std::fmt;

use crate::error::{CoreError, Result};

/// A normalized external literature identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Identifier {
    DOI(String),
    PMID(String),
    PMCID(String),
    ISBN(String),
    OpenAlexId(String),
}

impl Identifier {
    /// Returns the canonical form used for cache keys and external identifiers.
    pub fn canonical(&self) -> String {
        match self {
            Identifier::DOI(v) => v.clone(),
            Identifier::PMID(v) => v.clone(),
            Identifier::PMCID(v) => v.clone(),
            Identifier::ISBN(v) => v.clone(),
            Identifier::OpenAlexId(v) => v.clone(),
        }
    }

    /// Returns the lowercase namespace used in `ExternalIdentifier` records.
    pub fn namespace(&self) -> &'static str {
        match self {
            Identifier::DOI(_) => "doi",
            Identifier::PMID(_) => "pmid",
            Identifier::PMCID(_) => "pmcid",
            Identifier::ISBN(_) => "isbn",
            Identifier::OpenAlexId(_) => "openalex",
        }
    }
}

impl fmt::Display for Identifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.namespace(), self.canonical())
    }
}

/// Parses a free-form identifier string into a normalized `Identifier`.
///
/// Supports DOI, PMID/PMCID, ISBN and OpenAlex ID with common prefixes and
/// separators stripped.
pub fn parse_identifier(input: &str) -> Result<Identifier> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(CoreError::RemoteMetadataNotFound {
            identifier: input.to_string(),
        });
    }

    if let Some(openalex) = parse_openalex_id(trimmed) {
        return Ok(Identifier::OpenAlexId(openalex));
    }
    if let Some(doi) = parse_doi(trimmed) {
        return Ok(Identifier::DOI(doi));
    }
    if let Some(pmid) = parse_pmid(trimmed) {
        return Ok(Identifier::PMID(pmid));
    }
    if let Some(pmcid) = parse_pmcid(trimmed) {
        return Ok(Identifier::PMCID(pmcid));
    }
    if let Some(isbn) = parse_isbn(trimmed) {
        return Ok(Identifier::ISBN(isbn));
    }

    Err(CoreError::RemoteMetadataNotFound {
        identifier: input.to_string(),
    })
}

fn parse_doi(input: &str) -> Option<String> {
    let mut s = input.to_ascii_lowercase();
    for prefix in ["doi:", "https://doi.org/", "http://doi.org/"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.to_string();
        }
    }
    s = s.trim().to_string();
    // DOIs must contain at least one slash and are case-insensitive.
    if s.contains('/') && !s.is_empty() {
        Some(s)
    } else {
        None
    }
}

fn parse_pmid(input: &str) -> Option<String> {
    let s = input
        .to_ascii_lowercase()
        .replace("pmid:", "")
        .trim()
        .to_string();
    if s.chars().all(|c| c.is_ascii_digit()) && !s.is_empty() {
        Some(s)
    } else {
        None
    }
}

fn parse_pmcid(input: &str) -> Option<String> {
    let mut s = input.to_ascii_lowercase();
    for prefix in ["pmcid:", "pmc:"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.to_string();
        }
    }
    s = s.trim().to_string();
    // Some inputs use the redundant form "PMC:PMC12345".
    if let Some(rest) = s.strip_prefix("pmc") {
        s = rest.to_string();
    }
    s = s.trim().to_string();
    if s.chars().all(|c| c.is_ascii_digit()) && !s.is_empty() {
        Some(s)
    } else {
        None
    }
}

fn parse_isbn(input: &str) -> Option<String> {
    let s: String = input
        .to_ascii_uppercase()
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == 'X')
        .collect();
    if s.len() == 10 || s.len() == 13 {
        Some(s)
    } else {
        None
    }
}

fn parse_openalex_id(input: &str) -> Option<String> {
    let mut s = input.to_ascii_uppercase();
    for prefix in ["W", "HTTPS://OPENALEX.ORG/", "HTTP://OPENALEX.ORG/"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.to_string();
        }
    }
    s = s.trim().to_string();
    if s.starts_with('W') && s[1..].chars().all(|c| c.is_ascii_digit()) && s.len() > 1 {
        Some(s)
    } else if s.chars().all(|c| c.is_ascii_digit()) && !s.is_empty() {
        Some(format!("W{s}"))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doi_prefixes_are_removed_and_lowercased() {
        assert_eq!(
            parse_identifier("doi:10.1038/S41586-021-03819-2").unwrap(),
            Identifier::DOI("10.1038/s41586-021-03819-2".to_string())
        );
        assert_eq!(
            parse_identifier("https://doi.org/10.1038/S41586-021-03819-2").unwrap(),
            Identifier::DOI("10.1038/s41586-021-03819-2".to_string())
        );
        assert_eq!(
            parse_identifier("10.1038/S41586-021-03819-2").unwrap(),
            Identifier::DOI("10.1038/s41586-021-03819-2".to_string())
        );
    }

    #[test]
    fn pmid_and_pmcid_are_normalized() {
        assert_eq!(
            parse_identifier("pmid:12345").unwrap(),
            Identifier::PMID("12345".to_string())
        );
        assert_eq!(
            parse_identifier("PMC:PMC12345").unwrap(),
            Identifier::PMCID("12345".to_string())
        );
        assert_eq!(
            parse_identifier("pmcid:12345").unwrap(),
            Identifier::PMCID("12345".to_string())
        );
    }

    #[test]
    fn isbn_hyphens_and_spaces_are_removed() {
        assert_eq!(
            parse_identifier("978-0-13-468599-1").unwrap(),
            Identifier::ISBN("9780134685991".to_string())
        );
        assert_eq!(
            parse_identifier("0-13-468599-7").unwrap(),
            Identifier::ISBN("0134685997".to_string())
        );
    }

    #[test]
    fn openalex_id_is_normalized() {
        assert_eq!(
            parse_identifier("W12345").unwrap(),
            Identifier::OpenAlexId("W12345".to_string())
        );
        assert_eq!(
            parse_identifier("https://openalex.org/W12345").unwrap(),
            Identifier::OpenAlexId("W12345".to_string())
        );
    }

    #[test]
    fn unrecognized_identifier_fails() {
        assert!(parse_identifier("").is_err());
        assert!(parse_identifier("not-an-id").is_err());
    }
}
