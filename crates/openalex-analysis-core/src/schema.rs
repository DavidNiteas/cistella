use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TableName {
    Sources,
    SourceYearMetrics,
    SourceTopics,
    SourceIssns,
}

impl TableName {
    pub fn as_str(self) -> &'static str {
        match self {
            TableName::Sources => "sources",
            TableName::SourceYearMetrics => "source_year_metrics",
            TableName::SourceTopics => "source_topics",
            TableName::SourceIssns => "source_issns",
        }
    }

    pub fn parquet_file(self) -> &'static str {
        match self {
            TableName::Sources => "sources.parquet",
            TableName::SourceYearMetrics => "source_year_metrics.parquet",
            TableName::SourceTopics => "source_topics.parquet",
            TableName::SourceIssns => "source_issns.parquet",
        }
    }
    pub fn arrow_file(self) -> &'static str {
        match self {
            TableName::Sources => "sources.arrow",
            TableName::SourceYearMetrics => "source_year_metrics.arrow",
            TableName::SourceTopics => "source_topics.arrow",
            TableName::SourceIssns => "source_issns.arrow",
        }
    }
}
