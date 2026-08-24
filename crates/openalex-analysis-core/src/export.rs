use std::{fs::File, path::Path};

use polars::prelude::*;
use rust_xlsxwriter::Workbook;

use crate::error::Result;

#[derive(Debug, Clone, Copy)]
pub enum ExportFormat {
    Csv,
    Xlsx,
}

impl ExportFormat {
    pub fn from_path(path: &Path) -> Self {
        match path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "xlsx" => Self::Xlsx,
            _ => Self::Csv,
        }
    }

    pub fn from_name_or_path(value: &str) -> Self {
        match value.to_ascii_lowercase().as_str() {
            "xlsx" => Self::Xlsx,
            "csv" => Self::Csv,
            _ => Self::from_path(Path::new(value)),
        }
    }
}

pub fn export_dataframe(mut df: DataFrame, output: impl AsRef<Path>, format: ExportFormat) -> Result<()> {
    let output = output.as_ref();
    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    match format {
        ExportFormat::Csv => export_csv(&mut df, output),
        ExportFormat::Xlsx => export_xlsx(&df, output),
    }
}

fn export_csv(df: &mut DataFrame, output: &Path) -> Result<()> {
    let mut file = File::create(output)?;
    CsvWriter::new(&mut file).include_header(true).finish(df)?;
    Ok(())
}

fn export_xlsx(df: &DataFrame, output: &Path) -> Result<()> {
    let mut workbook = Workbook::new();
    let worksheet = workbook.add_worksheet();
    let columns = df.columns();

    for (col_idx, col) in columns.iter().enumerate() {
        worksheet.write_string(0, col_idx as u16, col.name().as_str())?;
    }

    for row_idx in 0..df.height() {
        for (col_idx, col) in columns.iter().enumerate() {
            let value = match col.get(row_idx) {
                Ok(v) => v.to_string(),
                Err(_) => String::new(),
            };
            worksheet.write_string((row_idx + 1) as u32, col_idx as u16, &value)?;
        }
    }

    workbook.save(output)?;
    Ok(())
}
