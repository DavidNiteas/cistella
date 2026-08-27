use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
};

use flate2::read::GzDecoder;
use serde_json::Value;
use uuid::Uuid;

use crate::{
    CoreError, ExternalIdentifier, Result,
    import::{record_source::RecordSource, source_record::SourceRecord},
};

/// Parses OpenAlex works JSONL (plain or gzip) into source records.
#[derive(Debug, Clone, Default)]
pub struct OpenAlexWorksRecordSource;

impl OpenAlexWorksRecordSource {
    pub fn new() -> Self {
        Self
    }
}

impl RecordSource for OpenAlexWorksRecordSource {
    fn parse(&self, bytes: &[u8]) -> Result<Vec<SourceRecord>> {
        let text = std::str::from_utf8(bytes)
            .map_err(|e| CoreError::InvalidLiteratureImport(format!("invalid UTF-8: {e}")))?;
        parse_openalex_works_text(text)
    }
}

fn parse_openalex_works_text(text: &str) -> Result<Vec<SourceRecord>> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    // Support a single JSON object, a JSON array, or JSONL.
    if trimmed.starts_with('[') {
        let works: Vec<Value> = serde_json::from_str(trimmed).map_err(|e| {
            CoreError::InvalidLiteratureImport(format!("invalid OpenAlex works array: {e}"))
        })?;
        works.iter().map(parse_openalex_work).collect()
    } else if trimmed.starts_with('{') {
        // Try to parse the entire payload as one pretty-printed object first.
        if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
            return Ok(vec![parse_openalex_work(&value)?]);
        }
        // Otherwise treat it as JSONL.
        let mut records = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let value: Value = serde_json::from_str(line).map_err(|e| {
                CoreError::InvalidLiteratureImport(format!("invalid OpenAlex work JSON: {e}"))
            })?;
            records.push(parse_openalex_work(&value)?);
        }
        if records.is_empty() && !trimmed.is_empty() {
            return Err(CoreError::InvalidLiteratureImport(
                "no valid OpenAlex works found".to_string(),
            ));
        }
        Ok(records)
    } else {
        Err(CoreError::InvalidLiteratureImport(
            "OpenAlex works input must be a JSON object, array, or JSONL".to_string(),
        ))
    }
}

pub(crate) fn parse_openalex_work(value: &Value) -> Result<SourceRecord> {
    let work = value.as_object().ok_or_else(|| {
        CoreError::InvalidLiteratureImport("OpenAlex work must be a JSON object".to_string())
    })?;

    let title = work
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let authors = work
        .get("authorships")
        .and_then(|v| v.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|authorship| {
                    authorship
                        .get("author")
                        .and_then(|a| a.get("display_name"))
                        .and_then(|n| n.as_str())
                        .map(ToString::to_string)
                })
                .collect()
        })
        .unwrap_or_default();

    let published_year = work
        .get("publication_year")
        .and_then(|v| v.as_i64())
        .map(|y| y as i32);

    let item_type = work
        .get("type")
        .and_then(|v| v.as_str())
        .map(normalize_item_type)
        .unwrap_or_else(|| "other".to_string());

    let mut external_identifiers = Vec::new();

    if let Some(openalex_id) = extract_openalex_id(work.get("id")) {
        external_identifiers.push(ExternalIdentifier {
            namespace: "openalex".to_string(),
            value: openalex_id,
        });
    }

    if let Some(doi) = work.get("doi").and_then(|v| v.as_str()).map(normalize_doi) {
        if !doi.is_empty() {
            external_identifiers.push(ExternalIdentifier {
                namespace: "doi".to_string(),
                value: doi,
            });
        }
    }

    if let Some(issn) = extract_issn(work) {
        external_identifiers.push(ExternalIdentifier {
            namespace: "issn".to_string(),
            value: issn,
        });
    }

    let volume = work
        .get("biblio")
        .and_then(|b| b.get("volume"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let pages = {
        let biblio = work.get("biblio");
        let first = biblio
            .and_then(|b| b.get("first_page"))
            .and_then(|v| v.as_str());
        let last = biblio
            .and_then(|b| b.get("last_page"))
            .and_then(|v| v.as_str());
        match (first, last) {
            (Some(start), Some(end)) => format!("{start}-{end}"),
            (Some(start), None) => start.to_string(),
            (None, Some(end)) => end.to_string(),
            (None, None) => String::new(),
        }
    };

    let abstract_text = work
        .get("abstract_inverted_index")
        .map(invert_abstract)
        .unwrap_or_default();

    let keywords = work
        .get("keywords")
        .and_then(|v| v.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|kw| {
                    if let Some(s) = kw.as_str() {
                        Some(s.to_string())
                    } else {
                        kw.get("keyword")
                            .or_else(|| kw.get("name"))
                            .and_then(|n| n.as_str())
                            .map(ToString::to_string)
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    let external_id = extract_openalex_id(work.get("id"));

    let mut raw_fields: BTreeMap<String, String> = BTreeMap::new();
    for (key, value) in work {
        if is_consumed_field(key) {
            continue;
        }
        raw_fields.insert(key.to_string(), value.to_string());
    }

    Ok(SourceRecord {
        record_id: Uuid::new_v4(),
        source_name: "OpenAlex".to_string(),
        external_id,
        external_identifiers,
        title,
        authors,
        published_year,
        item_type,
        abstract_text,
        keywords,
        pages,
        volume,
        raw_fields,
    })
}

fn normalize_item_type(value: &str) -> String {
    match value.to_ascii_lowercase().as_str() {
        "article" => "article".to_string(),
        "book" => "book".to_string(),
        "chapter" => "chapter".to_string(),
        _ => "other".to_string(),
    }
}

fn normalize_doi(value: &str) -> String {
    let trimmed = value.trim();
    if let Some(rest) = trimmed.strip_prefix("https://doi.org/") {
        rest.to_string()
    } else if let Some(rest) = trimmed.strip_prefix("http://doi.org/") {
        rest.to_string()
    } else if let Some(rest) = trimmed.strip_prefix("doi:") {
        rest.to_string()
    } else {
        trimmed.to_string()
    }
}

fn extract_openalex_id(value: Option<&Value>) -> Option<String> {
    let id = value.and_then(|v| v.as_str())?;
    id.rsplit('/').next().map(ToString::to_string)
}

fn extract_issn(work: &serde_json::Map<String, Value>) -> Option<String> {
    // Older snapshots use `host_venue`; newer snapshots use `primary_location.source`.
    let source = work.get("host_venue").or_else(|| {
        work.get("primary_location")
            .and_then(|loc| loc.get("source"))
    });

    if let Some(source) = source {
        if let Some(issn_l) = source.get("issn_l").and_then(|v| v.as_str()) {
            if !issn_l.is_empty() {
                return Some(issn_l.to_string());
            }
        }
        if let Some(issn) = source.get("issn").and_then(|v| v.as_str()) {
            if !issn.is_empty() {
                return Some(issn.to_string());
            }
        }
        if let Some(issns) = source.get("issn").and_then(|v| v.as_array()) {
            for issn in issns {
                if let Some(s) = issn.as_str().filter(|s| !s.is_empty()) {
                    return Some(s.to_string());
                }
            }
        }
    }
    None
}

fn invert_abstract(index: &Value) -> String {
    let Some(obj) = index.as_object() else {
        return String::new();
    };
    let mut tokens: Vec<(usize, &str)> = Vec::new();
    for (word, positions) in obj {
        let Some(list) = positions.as_array() else {
            continue;
        };
        for pos in list {
            if let Some(p) = pos.as_u64() {
                tokens.push((p as usize, word.as_str()));
            }
        }
    }
    tokens.sort_by_key(|(pos, _)| *pos);
    tokens
        .into_iter()
        .map(|(_, word)| word)
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_consumed_field(key: &str) -> bool {
    matches!(
        key,
        "id" | "doi"
            | "title"
            | "authorships"
            | "publication_year"
            | "type"
            | "host_venue"
            | "primary_location"
            | "biblio"
            | "abstract_inverted_index"
            | "keywords"
    )
}

/// Scans `dir` for OpenAlex works JSONL files (`.jsonl` or `.jsonl.gz`) and
/// yields each parsed work. Files inside `updated_date=*/work/` are handled as
/// well as any direct `.jsonl`/`.jsonl.gz` files.
pub(crate) fn scan_work_files(
    dir: impl AsRef<Path>,
    mut on_work: impl FnMut(SourceRecord) -> Result<()>,
) -> Result<()> {
    let mut stack: Vec<PathBuf> = vec![dir.as_ref().to_path_buf()];

    while let Some(current) = stack.pop() {
        let entries = match std::fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };

        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            let metadata = entry.metadata()?;
            if metadata.is_dir() {
                stack.push(path);
                continue;
            }
            if !metadata.is_file() {
                continue;
            }
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if ext != "jsonl" && ext != "gz" {
                continue;
            }
            read_work_file(&path, &mut on_work)?;
        }
    }

    Ok(())
}

fn read_work_file(path: &Path, on_work: &mut impl FnMut(SourceRecord) -> Result<()>) -> Result<()> {
    let file = File::open(path)?;
    let is_gz = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("gz"))
        .unwrap_or(false);

    if is_gz {
        let decoder = GzDecoder::new(file);
        read_lines(decoder, on_work)
    } else {
        read_lines(file, on_work)
    }
}

fn read_lines<R: Read>(
    reader: R,
    on_work: &mut impl FnMut(SourceRecord) -> Result<()>,
) -> Result<()> {
    let buf = BufReader::new(reader);
    for line in buf.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(trimmed).map_err(|e| {
            CoreError::InvalidLiteratureImport(format!("invalid OpenAlex work JSON: {e}"))
        })?;
        on_work(parse_openalex_work(&value)?)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_work() {
        let json = r#"{
            "id": "https://openalex.org/W123456789",
            "doi": "https://doi.org/10.1000/abc",
            "title": "Test Work",
            "publication_year": 2023,
            "type": "article",
            "authorships": [
                {"author": {"display_name": "Alice Author"}},
                {"author": {"display_name": "Bob Writer"}}
            ],
            "biblio": {"volume": "42", "first_page": "1", "last_page": "10"},
            "abstract_inverted_index": {"Hello": [0], "world": [1]}
        }"#;
        let records = OpenAlexWorksRecordSource::new()
            .parse(json.as_bytes())
            .unwrap();
        assert_eq!(records.len(), 1);
        let rec = &records[0];
        assert_eq!(rec.title, "Test Work");
        assert_eq!(rec.authors, vec!["Alice Author", "Bob Writer"]);
        assert_eq!(rec.published_year, Some(2023));
        assert_eq!(rec.item_type, "article");
        assert_eq!(rec.volume, "42");
        assert_eq!(rec.pages, "1-10");
        assert_eq!(rec.abstract_text, "Hello world");
        assert!(
            rec.external_identifiers
                .iter()
                .any(|id| id.namespace == "openalex" && id.value == "W123456789")
        );
        assert!(
            rec.external_identifiers
                .iter()
                .any(|id| id.namespace == "doi" && id.value == "10.1000/abc")
        );
    }

    #[test]
    fn rejects_non_object_work() {
        let err = OpenAlexWorksRecordSource::new()
            .parse(b"\"not an object\"")
            .unwrap_err();
        assert!(err.to_string().contains("invalid literature import"));
    }
}
