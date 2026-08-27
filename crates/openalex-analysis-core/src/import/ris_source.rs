use std::collections::BTreeMap;

use uuid::Uuid;

use crate::{
    CoreError, ExternalIdentifier, Result,
    import::{record_source::RecordSource, source_record::SourceRecord},
};

/// Parses RIS (Research Information Systems) byte input into source records.
///
/// This implementation deliberately avoids external network dependencies and
/// provides best-effort field extraction for the namespaces required by M2:
/// `title`, `author`, `year`, `doi`, `isbn`, `issn`, `pages`, `volume`,
/// `abstract`, `keywords`, and entry type.
#[derive(Debug, Clone, Default)]
pub struct RISRecordSource;

impl RISRecordSource {
    pub fn new() -> Self {
        Self
    }
}

impl RecordSource for RISRecordSource {
    fn parse(&self, bytes: &[u8]) -> Result<Vec<SourceRecord>> {
        let text = std::str::from_utf8(bytes)
            .map_err(|e| CoreError::InvalidLiteratureImport(format!("invalid UTF-8: {e}")))?;
        parse_ris(text)
    }
}

fn parse_ris(text: &str) -> Result<Vec<SourceRecord>> {
    let mut records = Vec::new();
    let mut current: Option<RecordBuilder> = None;

    for raw_line in text.lines() {
        let line = raw_line.trim_end();
        if line.is_empty() {
            continue;
        }

        if let Some((tag, value)) = split_tag_line(line) {
            let tag_upper = tag.to_ascii_uppercase();

            if tag_upper == "TY" {
                if let Some(builder) = current.take() {
                    records.push(builder.build()?);
                }
                current = Some(RecordBuilder::new(value.to_string()));
                continue;
            }

            // `ER  - ` marks the explicit end of a record; skip it.
            if tag_upper == "ER" {
                if let Some(builder) = current.take() {
                    records.push(builder.build()?);
                }
                continue;
            }

            if let Some(ref mut builder) = current {
                builder.add_field(&tag_upper, value);
            }
        } else if let Some(ref mut builder) = current {
            // Continuation line: append to the most recently added raw field.
            builder.continue_last(line);
        }
    }

    if let Some(builder) = current.take() {
        records.push(builder.build()?);
    }

    if records.is_empty() && !text.trim().is_empty() {
        return Err(CoreError::InvalidLiteratureImport(
            "no valid RIS records found".to_string(),
        ));
    }

    Ok(records)
}

/// Splits a line into a two-character tag and its value.
///
/// RIS tag lines use `XX  - value` (two spaces, hyphen, optional space/value).
/// The tag itself is case-insensitive and must be two characters long.
fn split_tag_line(line: &str) -> Option<(&str, &str)> {
    let bytes = line.as_bytes();
    if bytes.len() < 5 {
        return None;
    }
    // Tag characters must be letters or digits.
    if !bytes[0].is_ascii_alphanumeric() || !bytes[1].is_ascii_alphanumeric() {
        return None;
    }
    if &bytes[2..5] != b"  -" {
        return None;
    }
    let tag = &line[..2];
    let value = line[5..].trim_start();
    Some((tag, value))
}

struct RecordBuilder {
    entry_type: String,
    raw_fields: BTreeMap<String, String>,
    authors: Vec<String>,
    keywords: Vec<String>,
    last_key: Option<String>,
}

impl RecordBuilder {
    fn new(entry_type: String) -> Self {
        Self {
            entry_type,
            raw_fields: BTreeMap::new(),
            authors: Vec::new(),
            keywords: Vec::new(),
            last_key: None,
        }
    }

    fn add_field(&mut self, tag: &str, value: &str) {
        let key = tag.to_ascii_lowercase();
        self.last_key = Some(key.clone());
        self.raw_fields
            .insert(key.clone(), value.trim_end().to_string());

        let trimmed = value.trim().to_string();
        match key.as_str() {
            "au" | "a1" | "a2" | "a3" => {
                if !trimmed.is_empty() {
                    self.authors.push(trimmed);
                }
            }
            "kw" => {
                if !trimmed.is_empty() {
                    self.keywords.push(trimmed);
                }
            }
            _ => {}
        }
    }

    fn continue_last(&mut self, line: &str) {
        if let Some(ref key) = self.last_key {
            let trimmed = line.trim();
            if let Some(existing) = self.raw_fields.get_mut(key) {
                if !existing.is_empty() && !trimmed.is_empty() {
                    existing.push(' ');
                }
                existing.push_str(trimmed);
            }
            match key.as_str() {
                "au" | "a1" | "a2" | "a3" => {
                    if let Some(last) = self.authors.last_mut() {
                        if !last.is_empty() && !trimmed.is_empty() {
                            last.push(' ');
                        }
                        last.push_str(trimmed);
                    }
                }
                "kw" => {
                    if let Some(last) = self.keywords.last_mut() {
                        if !last.is_empty() && !trimmed.is_empty() {
                            last.push(' ');
                        }
                        last.push_str(trimmed);
                    }
                }
                _ => {}
            }
        }
    }

    fn build(self) -> Result<SourceRecord> {
        if self.entry_type.trim().is_empty() {
            return Err(CoreError::InvalidLiteratureImport(
                "RIS record missing TY (type)".to_string(),
            ));
        }

        let title = self
            .first_value(&["ti", "t1", "ct"])
            .cloned()
            .unwrap_or_default();

        let published_year = self
            .first_value(&["py", "y1"])
            .and_then(|s| extract_year(s));

        let abstract_text = self.first_value(&["ab"]).cloned().unwrap_or_default();
        let volume = self.first_value(&["vl"]).cloned().unwrap_or_default();

        let pages = match (self.first_value(&["sp"]), self.first_value(&["ep"])) {
            (Some(start), Some(end)) => format!("{start}-{end}"),
            (Some(start), None) => start.clone(),
            (None, Some(end)) => end.clone(),
            (None, None) => String::new(),
        };

        let item_type = normalize_item_type(&self.entry_type);

        let mut external_identifiers = Vec::new();
        if let Some(doi) = self.first_value(&["do", "doi"]) {
            external_identifiers.push(ExternalIdentifier {
                namespace: "doi".to_string(),
                value: doi.clone(),
            });
        }
        // RIS `SN` is ISBN/ISSN: ISSN for serials, ISBN for books/chapters.
        if let Some(sn) = self.first_value(&["sn"]) {
            let namespace = if item_type == "article" {
                "issn"
            } else {
                "isbn"
            };
            external_identifiers.push(ExternalIdentifier {
                namespace: namespace.to_string(),
                value: sn.clone(),
            });
        }
        // RIS `IS` is the issue number, not ISSN; keep it in raw_fields only.

        // RIS has no natural cite key; use the first available external
        // identifier as the record locator so repeated imports of the same
        // record can be de-duplicated by source_ref.
        let external_id = external_identifiers
            .first()
            .map(|id| id.value.clone())
            .or_else(|| self.first_value(&["ti", "t1", "ct"]).cloned());

        Ok(SourceRecord {
            record_id: Uuid::new_v4(),
            source_name: "RIS".to_string(),
            external_id,
            external_identifiers,
            title,
            authors: self.authors,
            published_year,
            item_type,
            abstract_text,
            keywords: self.keywords,
            pages,
            volume,
            raw_fields: self.raw_fields,
        })
    }

    fn first_value(&self, tags: &[&str]) -> Option<&String> {
        tags.iter().find_map(|tag| self.raw_fields.get(*tag))
    }
}

fn extract_year(value: &str) -> Option<i32> {
    let digits: String = value
        .chars()
        .filter(|c| c.is_ascii_digit())
        .take(4)
        .collect();
    if digits.len() == 4 {
        digits.parse::<i32>().ok()
    } else {
        None
    }
}

fn normalize_item_type(entry_type: &str) -> String {
    match entry_type.to_ascii_lowercase().as_str() {
        "jour" | "journal" => "article".to_string(),
        "book" => "book".to_string(),
        "chap" | "chapter" => "chapter".to_string(),
        "conf" | "conference" => "article".to_string(),
        _ => "other".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_journal_article_with_identifiers() {
        let src = r#"TY  - JOUR
TI  - A Test Paper
AU  - Alice Smith
AU  - Bob Jones
PY  - 2020
DO  - 10.1000/xyz
SN  - 1234-5678
IS  - 3
VL  - 42
SP  - 1
EP  - 10
AB  - This is the abstract.
KW  - testing
KW  - RIS
ER  -
"#;
        let records = RISRecordSource::new().parse(src.as_bytes()).unwrap();
        assert_eq!(records.len(), 1);
        let rec = &records[0];
        assert_eq!(rec.title, "A Test Paper");
        assert_eq!(rec.authors, vec!["Alice Smith", "Bob Jones"]);
        assert_eq!(rec.published_year, Some(2020));
        assert_eq!(rec.item_type, "article");
        assert_eq!(rec.volume, "42");
        assert_eq!(rec.pages, "1-10");
        assert_eq!(rec.abstract_text, "This is the abstract.");
        assert_eq!(rec.keywords, vec!["testing", "RIS"]);
        assert!(
            rec.external_identifiers
                .iter()
                .any(|id| id.namespace == "doi" && id.value == "10.1000/xyz")
        );
        // RIS `SN` on a journal maps to ISSN.
        assert!(
            rec.external_identifiers
                .iter()
                .any(|id| id.namespace == "issn" && id.value == "1234-5678")
        );
        // RIS `IS` is the issue number and must not be treated as ISSN.
        assert!(
            !rec.external_identifiers
                .iter()
                .any(|id| id.namespace == "issn" && id.value == "3")
        );
        assert_eq!(rec.raw_fields.get("is"), Some(&"3".to_string()));
    }

    #[test]
    fn maps_sn_to_isbn_for_books() {
        let src = r#"TY  - BOOK
TI  - A Test Book
AU  - Alice Author
PY  - 2021
SN  - 978-3-030-00000-0
ER  -
"#;
        let records = RISRecordSource::new().parse(src.as_bytes()).unwrap();
        let rec = &records[0];
        assert_eq!(rec.item_type, "book");
        assert!(
            rec.external_identifiers
                .iter()
                .any(|id| id.namespace == "isbn" && id.value == "978-3-030-00000-0")
        );
        assert!(
            !rec.external_identifiers
                .iter()
                .any(|id| id.namespace == "issn")
        );
    }

    #[test]
    fn maps_sn_to_issn_for_journals() {
        let src = r#"TY  - JOUR
TI  - A Test Article
AU  - Alice Author
PY  - 2021
SN  - 1234-5678
ER  -
"#;
        let records = RISRecordSource::new().parse(src.as_bytes()).unwrap();
        let rec = &records[0];
        assert_eq!(rec.item_type, "article");
        assert!(
            rec.external_identifiers
                .iter()
                .any(|id| id.namespace == "issn" && id.value == "1234-5678")
        );
        assert!(
            !rec.external_identifiers
                .iter()
                .any(|id| id.namespace == "isbn")
        );
    }

    #[test]
    fn rejects_malformed_input() {
        let src = b"not a ris file";
        assert!(RISRecordSource::new().parse(src).is_err());
    }
}
