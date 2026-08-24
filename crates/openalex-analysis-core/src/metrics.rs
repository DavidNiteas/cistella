use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricCode {
    WorksCount,
    CitedByCount,
    HIndex,
    I10Index,
    MeanCitedness2Yr,
    CitationsPerWork,
    OaRate,
}

impl MetricCode {
    pub fn expression_name(self) -> &'static str {
        match self {
            MetricCode::WorksCount => "works_count",
            MetricCode::CitedByCount => "cited_by_count",
            MetricCode::HIndex => "h_index",
            MetricCode::I10Index => "i10_index",
            MetricCode::MeanCitedness2Yr => "mean_citedness_2yr",
            MetricCode::CitationsPerWork => "citations_per_work",
            MetricCode::OaRate => "oa_rate",
        }
    }
}
