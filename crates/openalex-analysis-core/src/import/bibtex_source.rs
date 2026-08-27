use std::collections::BTreeMap;

use uuid::Uuid;

use crate::{
    CoreError, ExternalIdentifier, Result,
    import::{record_source::RecordSource, source_record::SourceRecord},
};

/// Parses BibTeX/BibLaTeX byte input into source records.
///
/// This implementation deliberately avoids external network dependencies and
/// provides best-effort field extraction for the namespaces required by M1:
/// `title`, `author`, `year`, `doi`, `isbn`, `pmid`, `url`, and entry type.
#[derive(Debug, Clone, Default)]
pub struct BibTeXRecordSource;

impl BibTeXRecordSource {
    pub fn new() -> Self {
        Self
    }
}

impl RecordSource for BibTeXRecordSource {
    fn parse(&self, bytes: &[u8]) -> Result<Vec<SourceRecord>> {
        let text = std::str::from_utf8(bytes)
            .map_err(|e| CoreError::InvalidLiteratureImport(format!("invalid UTF-8: {e}")))?;
        parse_bibtex(text)
    }
}

fn parse_bibtex(text: &str) -> Result<Vec<SourceRecord>> {
    let mut records = Vec::new();
    let mut chars = text.char_indices().peekable();

    while let Some((start, ch)) = chars.next() {
        if ch != '@' {
            continue;
        }

        // Skip comments: @comment{ ... }
        if text[start..]
            .get(1..)
            .map(|rest| {
                rest.trim_start()
                    .to_ascii_lowercase()
                    .starts_with("comment")
            })
            .unwrap_or(false)
        {
            skip_braced_block(&mut chars)?;
            continue;
        }

        // Parse entry type.
        let entry_type = parse_identifier(&mut chars)?.ok_or_else(|| {
            CoreError::InvalidLiteratureImport("missing entry type after @".to_string())
        })?;

        skip_whitespace(&mut chars);
        expect_char(&mut chars, '{')?;
        skip_whitespace(&mut chars);

        // Parse cite key.
        let key = parse_identifier(&mut chars)?
            .ok_or_else(|| CoreError::InvalidLiteratureImport("missing cite key".to_string()))?;

        skip_whitespace(&mut chars);
        expect_char(&mut chars, ',')?;

        let mut raw_fields: BTreeMap<String, String> = BTreeMap::new();

        loop {
            skip_whitespace_and_commas(&mut chars);
            if let Some(&(_, '}')) = chars.peek() {
                chars.next();
                break;
            }
            let (name, value) = parse_field(&mut chars)?;
            raw_fields.insert(name.to_ascii_lowercase(), value);
        }

        records.push(build_source_record(&entry_type, &key, raw_fields));
    }

    if records.is_empty() && !text.trim().is_empty() {
        return Err(CoreError::InvalidLiteratureImport(
            "no valid BibTeX entries found".to_string(),
        ));
    }

    Ok(records)
}

fn build_source_record(
    entry_type: &str,
    key: &str,
    raw_fields: BTreeMap<String, String>,
) -> SourceRecord {
    let title = raw_fields.get("title").cloned().unwrap_or_default();
    let authors = raw_fields
        .get("author")
        .map(|s| split_authors(s))
        .unwrap_or_default();
    let published_year = raw_fields.get("year").and_then(|s| s.parse::<i32>().ok());

    let mut external_identifiers = Vec::new();
    for ns in ["doi", "isbn", "pmid", "url"] {
        if let Some(value) = raw_fields.get(ns).filter(|v| !v.is_empty()) {
            external_identifiers.push(ExternalIdentifier {
                namespace: ns.to_string(),
                value: value.clone(),
            });
        }
    }

    let item_type = normalize_item_type(entry_type);

    SourceRecord {
        record_id: Uuid::new_v4(),
        source_name: "BibTeX".to_string(),
        external_id: Some(key.to_string()),
        external_identifiers,
        title,
        authors,
        published_year,
        item_type,
        abstract_text: String::new(),
        keywords: Vec::new(),
        pages: String::new(),
        volume: String::new(),
        raw_fields,
    }
}

fn normalize_item_type(entry_type: &str) -> String {
    match entry_type.to_ascii_lowercase().as_str() {
        "article" | "inproceedings" | "conference" => "article".to_string(),
        "book" => "book".to_string(),
        "inbook" | "incollection" | "chapter" => "chapter".to_string(),
        _ => "other".to_string(),
    }
}

fn split_authors(value: &str) -> Vec<String> {
    value
        .split(" and ")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn parse_identifier(
    chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>,
) -> Result<Option<String>> {
    skip_whitespace(chars);
    let mut result = String::new();
    while let Some(&(_, ch)) = chars.peek() {
        if ch.is_alphanumeric() || ch == '_' || ch == '-' || ch == ':' || ch == '.' {
            result.push(ch);
            chars.next();
        } else {
            break;
        }
    }
    if result.is_empty() {
        Ok(None)
    } else {
        Ok(Some(result))
    }
}

fn skip_whitespace(chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>) {
    while let Some(&(_, ch)) = chars.peek() {
        if ch.is_whitespace() {
            chars.next();
        } else {
            break;
        }
    }
}

fn skip_whitespace_and_commas(chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>) {
    while let Some(&(_, ch)) = chars.peek() {
        if ch.is_whitespace() || ch == ',' {
            chars.next();
        } else {
            break;
        }
    }
}

fn expect_char(
    chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>,
    expected: char,
) -> Result<()> {
    skip_whitespace(chars);
    match chars.peek() {
        Some(&(_, ch)) if ch == expected => {
            chars.next();
            Ok(())
        }
        Some(&(_, ch)) => Err(CoreError::InvalidLiteratureImport(format!(
            "expected '{expected}', found '{ch}'"
        ))),
        None => Err(CoreError::InvalidLiteratureImport(format!(
            "expected '{expected}', reached end of input"
        ))),
    }
}

fn parse_field(
    chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>,
) -> Result<(String, String)> {
    let name = parse_identifier(chars)?
        .ok_or_else(|| CoreError::InvalidLiteratureImport("missing field name".to_string()))?;

    skip_whitespace(chars);
    expect_char(chars, '=')?;
    skip_whitespace(chars);

    let value = parse_value(chars)?;
    skip_whitespace_and_commas(chars);
    Ok((name, value))
}

fn parse_value(chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>) -> Result<String> {
    skip_whitespace(chars);
    match chars.peek() {
        Some(&(_, '{')) => parse_braced_value(chars),
        Some(&(_, '"')) => parse_quoted_value(chars),
        Some(&(_, ch)) if ch.is_ascii_digit() || ch == '-' => parse_number_value(chars),
        _ => Err(CoreError::InvalidLiteratureImport(
            "field value must start with '{', '\"' or a number".to_string(),
        )),
    }
}

fn parse_braced_value(
    chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>,
) -> Result<String> {
    expect_char(chars, '{')?;
    let mut depth = 1_usize;
    let mut result = String::new();

    while let Some((_, ch)) = chars.next() {
        match ch {
            '{' => {
                depth += 1;
                result.push(ch);
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
                result.push(ch);
            }
            _ => result.push(ch),
        }
    }

    if depth != 0 {
        return Err(CoreError::InvalidLiteratureImport(
            "unclosed '{' in field value".to_string(),
        ));
    }

    Ok(result.trim().to_string())
}

fn parse_quoted_value(
    chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>,
) -> Result<String> {
    expect_char(chars, '"')?;
    let mut result = String::new();

    while let Some((_, ch)) = chars.next() {
        if ch == '"' {
            break;
        }
        result.push(ch);
    }

    Ok(result.trim().to_string())
}

fn parse_number_value(
    chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>,
) -> Result<String> {
    let mut result = String::new();
    while let Some(&(_, ch)) = chars.peek() {
        if ch.is_ascii_digit() || ch == '-' {
            result.push(ch);
            chars.next();
        } else {
            break;
        }
    }
    Ok(result)
}

fn skip_braced_block(chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>) -> Result<()> {
    skip_whitespace(chars);
    if !matches!(chars.peek(), Some(&(_, '{'))) {
        return Ok(());
    }
    chars.next();
    let mut depth = 1_usize;
    while let Some((_, ch)) = chars.next() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_article_with_identifiers() {
        let src = r#"
@article{test2020,
  title = {A Test Paper},
  author = {Alice Smith and Bob Jones},
  year = {2020},
  doi = {10.1000/xyz},
}
"#;
        let records = BibTeXRecordSource::new().parse(src.as_bytes()).unwrap();
        assert_eq!(records.len(), 1);
        let rec = &records[0];
        assert_eq!(rec.title, "A Test Paper");
        assert_eq!(rec.authors, vec!["Alice Smith", "Bob Jones"]);
        assert_eq!(rec.published_year, Some(2020));
        assert_eq!(rec.item_type, "article");
        assert!(
            rec.external_identifiers
                .iter()
                .any(|id| id.namespace == "doi")
        );
    }

    #[test]
    fn rejects_malformed_input() {
        let src = b"@article{key, title = {missing brace";
        assert!(BibTeXRecordSource::new().parse(src).is_err());
    }
}
