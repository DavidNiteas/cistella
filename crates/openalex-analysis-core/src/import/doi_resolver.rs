use std::{collections::HashMap, path::Path};

use crate::{CoreError, Result, SourceRecord};

use super::openalex_works_source::scan_work_files;

/// Resolves a DOI to an external source record.
pub trait DoiResolver {
    fn resolve(&self, doi: &str) -> Result<Option<SourceRecord>>;
}

/// DOI resolver backed by a local OpenAlex works snapshot.
///
/// The resolver builds an in-memory `doi -> SourceRecord` index by scanning all
/// `*.jsonl` / `*.jsonl.gz` works files under the configured directory. It does
/// not perform any network calls.
#[derive(Debug, Clone)]
pub struct LocalOpenAlexDoiResolver {
    index: HashMap<String, SourceRecord>,
}

impl LocalOpenAlexDoiResolver {
    /// Scans `dir` and builds the DOI index.
    pub fn scan(dir: impl AsRef<Path>) -> Result<Self> {
        let mut index: HashMap<String, SourceRecord> = HashMap::new();
        scan_work_files(dir, |record| {
            for id in &record.external_identifiers {
                if id.namespace.eq_ignore_ascii_case("doi") {
                    index.insert(id.value.to_ascii_lowercase(), record.clone());
                }
            }
            Ok(())
        })?;
        Ok(Self { index })
    }

    pub fn len(&self) -> usize {
        self.index.len()
    }

    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }
}

impl DoiResolver for LocalOpenAlexDoiResolver {
    fn resolve(&self, doi: &str) -> Result<Option<SourceRecord>> {
        Ok(self.index.get(&doi.to_ascii_lowercase()).cloned())
    }
}

/// Lists local OpenAlex works whose title, DOI, or OpenAlex ID contains `query`
/// (case-insensitive). Returns at most `limit` records.
pub fn list_local_openalex_works(
    dir: impl AsRef<Path>,
    query: &str,
    limit: usize,
) -> Result<Vec<SourceRecord>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return Ok(Vec::new());
    }

    let mut matches = Vec::new();
    scan_work_files(dir, |record| {
        if matches_query(&record, &q) {
            matches.push(record);
            if matches.len() >= limit {
                return Err(CoreError::InvalidLiteratureImport(
                    "limit reached".to_string(),
                ));
            }
        }
        Ok(())
    })
    .or_else(|e| {
        // The artificial "limit reached" error is our signal to stop scanning.
        let message = e.to_string();
        if message.contains("limit reached") {
            Ok(())
        } else {
            Err(e)
        }
    })?;

    Ok(matches)
}

fn matches_query(record: &SourceRecord, query: &str) -> bool {
    if record.title.to_ascii_lowercase().contains(query) {
        return true;
    }
    for id in &record.external_identifiers {
        if id.value.to_ascii_lowercase().contains(query) {
            return true;
        }
    }
    if let Some(external_id) = record.external_id.as_deref() {
        if external_id.to_ascii_lowercase().contains(query) {
            return true;
        }
    }
    false
}

/// Resolves a DOI against a local OpenAlex works directory.
pub fn resolve_local_openalex_doi(
    dir: impl AsRef<Path>,
    doi: &str,
) -> Result<Option<SourceRecord>> {
    let resolver = LocalOpenAlexDoiResolver::scan(dir)?;
    resolver.resolve(doi)
}

/// Scans a local OpenAlex works directory and returns all parsed records.
///
/// Useful when callers need their own filtering logic.
pub fn scan_local_openalex_works(dir: impl AsRef<Path>) -> Result<Vec<SourceRecord>> {
    let mut records = Vec::new();
    scan_work_files(dir, |record| {
        records.push(record);
        Ok(())
    })?;
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf};
    use uuid::Uuid;

    fn temp_works_dir() -> PathBuf {
        let root = std::env::temp_dir().join(format!("cistella-works-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn work_json(id: &str, doi: &str, title: &str) -> String {
        format!(
            r#"{{"id":"https://openalex.org/{id}","doi":"{doi}","title":"{title}","publication_year":2023,"type":"article","authorships":[{{"author":{{"display_name":"A Author"}}}}]}}"#
        )
    }

    #[test]
    fn resolver_finds_doi() {
        let dir = temp_works_dir();
        let line = work_json("W1", "https://doi.org/10.1000/found", "Found Work");
        fs::write(dir.join("works.jsonl"), line).unwrap();

        let resolver = LocalOpenAlexDoiResolver::scan(&dir).unwrap();
        assert_eq!(resolver.len(), 1);

        let found = resolver.resolve("10.1000/found").unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().title, "Found Work");

        let missing = resolver.resolve("10.1000/missing").unwrap();
        assert!(missing.is_none());

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn list_works_filters_by_title() {
        let dir = temp_works_dir();
        let lines = format!(
            "{}\n{}\n",
            work_json("W1", "https://doi.org/10.1000/a", "Alpha Paper"),
            work_json("W2", "https://doi.org/10.1000/b", "Beta Paper"),
        );
        fs::write(dir.join("works.jsonl"), lines).unwrap();

        let result = list_local_openalex_works(&dir, "alpha", 10).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].title, "Alpha Paper");

        fs::remove_dir_all(dir).unwrap();
    }
}
