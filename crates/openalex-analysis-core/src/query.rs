use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{error::Result, metrics::MetricCode, schema::TableName, vault::Vault};
use polars::prelude::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceSearchQuery {
    pub text: Option<String>,
    pub source_type: Option<String>,
    pub country_code: Option<String>,
    pub is_oa: Option<bool>,
    pub limit: usize,
    pub offset: usize,
}

impl Default for SourceSearchQuery {
    fn default() -> Self {
        Self {
            text: None,
            source_type: Some("journal".to_string()),
            country_code: None,
            is_oa: None,
            limit: 50,
            offset: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceSummary {
    pub openalex_id: String,
    pub display_name: Option<String>,
    pub source_type: Option<String>,
    pub country_code: Option<String>,
    pub works_count: Option<i64>,
    pub cited_by_count: Option<i64>,
}

impl Vault {
    pub fn sources_frame(&self) -> Result<DataFrame> {
        self.load_table(TableName::Sources)
    }

    pub fn overview_json(&self) -> Result<Value> {
        let df = self
            .scan_table(TableName::Sources)?
            .select([
                len().alias("source_count"),
                col("source_type")
                    .eq(lit("journal"))
                    .sum()
                    .alias("journal_count"),
                col("source_type")
                    .eq(lit("conference"))
                    .sum()
                    .alias("conference_count"),
                col("is_oa").sum().alias("oa_count"),
                col("works_count")
                    .cast(DataType::Int64)
                    .sum()
                    .alias("works_count"),
                col("cited_by_count")
                    .cast(DataType::Int64)
                    .sum()
                    .alias("cited_by_count"),
            ])
            .collect()?;
        dataframe_to_json_value(df)
    }

    pub fn search_sources_frame(&self, query: SourceSearchQuery) -> Result<DataFrame> {
        let mut lf = self.scan_table(TableName::Sources)?;
        if let Some(source_type) = query.source_type {
            lf = lf.filter(col("source_type").eq(lit(source_type)));
        }
        if let Some(country_code) = query.country_code {
            lf = lf.filter(col("country_code").eq(lit(country_code.to_uppercase())));
        }
        if let Some(is_oa) = query.is_oa {
            lf = lf.filter(col("is_oa").eq(lit(is_oa)));
        }
        if let Some(text) = query.text.filter(|s| !s.trim().is_empty()) {
            // Regex search for now; a dedicated Tantivy/lowercase Arrow index can replace this later.
            lf = lf.filter(
                col("display_name")
                    .str()
                    .contains(lit(format!("(?i){}", regex_escape_minimal(&text))), false),
            );
        }
        Ok(lf
            .select([
                col("openalex_id"),
                col("display_name"),
                col("source_type"),
                col("issn_l"),
                col("country_code"),
                col("works_count"),
                col("oa_works_count"),
                col("cited_by_count"),
                col("mean_citedness_2yr"),
                col("h_index"),
                col("i10_index"),
                col("is_oa"),
                col("is_in_doaj"),
            ])
            .slice(query.offset as i64, query.limit as u32)
            .collect()?)
    }

    pub fn search_sources_json(&self, query: SourceSearchQuery) -> Result<Value> {
        dataframe_to_json_value(self.search_sources_frame(query)?)
    }

    pub fn top_sources_frame(
        &self,
        metric: MetricCode,
        source_type: &str,
        limit: usize,
    ) -> Result<DataFrame> {
        let metric_name = metric.expression_name();
        let lf = self
            .scan_table(TableName::Sources)?
            .filter(col("source_type").eq(lit(source_type)))
            .select([
                col("openalex_id"),
                col("display_name"),
                col("source_type"),
                col("country_code"),
                col("works_count"),
                col("cited_by_count"),
                col("h_index"),
                col("i10_index"),
                col("mean_citedness_2yr"),
                col(metric_name).alias("metric_value"),
            ])
            .sort(
                ["metric_value"],
                SortMultipleOptions::default().with_order_descending(true),
            )
            .limit(limit as u32);
        Ok(lf.collect()?)
    }

    pub fn top_sources_json(
        &self,
        metric: MetricCode,
        source_type: &str,
        limit: usize,
    ) -> Result<Value> {
        dataframe_to_json_value(self.top_sources_frame(metric, source_type, limit)?)
    }
}

pub fn dataframe_to_json_value(mut df: DataFrame) -> Result<Value> {
    let mut buf = Vec::new();
    JsonWriter::new(&mut buf)
        .with_json_format(JsonFormat::Json)
        .finish(&mut df)?;
    Ok(serde_json::from_slice(&buf)?)
}

fn regex_escape_minimal(input: &str) -> String {
    input
        .chars()
        .flat_map(|ch| match ch {
            '.' | '+' | '*' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$' | '|' | '\\' => {
                vec!['\\', ch]
            }
            _ => vec![ch],
        })
        .collect()
}
