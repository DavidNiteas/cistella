use std::{path::PathBuf, process};

use cistella_core::{MetricCode, SearchFieldScope, SearchQuery, Vault, VaultOpenOptions};
use cistella_desktop_lib::commands::{HeadlessDoctorReport, headless_doctor_report};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use serde_json::{Value, json};

#[derive(Parser, Debug)]
#[command(
    name = "cistella-headless",
    version,
    about = "Headless utility entrypoint for cistella work-order automation"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Inspect the runtime directories and print a machine-readable summary.
    Doctor {
        /// Print JSON output instead of a human-readable report.
        #[arg(long)]
        json: bool,
    },
    /// Vault metadata commands.
    Vault {
        #[command(subcommand)]
        command: VaultCommand,
    },
    /// Source analysis queries over an existing Vault.
    Source {
        #[command(subcommand)]
        command: SourceCommand,
    },
    /// Local full-text/metadata search over an existing Vault.
    Search {
        #[arg(long)]
        vault: PathBuf,
        #[arg(long)]
        query: String,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Literature item queries.
    Literature {
        #[command(subcommand)]
        command: LiteratureCommand,
    },
    /// Reading session queries.
    Reading {
        #[command(subcommand)]
        command: ReadingCommand,
    },
    /// Note and annotation queries.
    Notes {
        #[command(subcommand)]
        command: NotesCommand,
    },
    /// Runtime settings and app directory information.
    Settings {
        #[command(subcommand)]
        command: SettingsCommand,
    },
}

#[derive(Subcommand, Debug)]
enum VaultCommand {
    /// Open a Vault and print its context.
    Open {
        #[arg(long)]
        path: PathBuf,
    },
}

#[derive(Subcommand, Debug)]
enum SourceCommand {
    /// Rank sources by one metric.
    Rank {
        #[arg(long)]
        vault: PathBuf,
        #[arg(long, value_enum)]
        metric: CliMetric,
        #[arg(long, default_value = "journal")]
        r#type: String,
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
}

#[derive(Subcommand, Debug)]
enum LiteratureCommand {
    /// List literature items stored in a Vault.
    List {
        #[arg(long)]
        vault: PathBuf,
        #[arg(long)]
        keyword: Option<String>,
        #[arg(long)]
        status: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
}

#[derive(Subcommand, Debug)]
enum ReadingCommand {
    /// List recent reading sessions.
    Sessions {
        #[arg(long)]
        vault: PathBuf,
    },
}

#[derive(Subcommand, Debug)]
enum NotesCommand {
    /// List notes.
    List {
        #[arg(long)]
        vault: PathBuf,
        #[arg(long)]
        item_id: Option<String>,
        #[arg(long, default_value_t = false)]
        include_archived: bool,
    },
}

#[derive(Subcommand, Debug)]
enum SettingsCommand {
    /// Print runtime settings/app directory information.
    Info,
}

#[derive(Clone, Debug, ValueEnum)]
enum CliMetric {
    WorksCount,
    CitedByCount,
    HIndex,
    I10Index,
    MeanCitedness2Yr,
    CitationsPerWork,
    OaRate,
}

impl From<CliMetric> for MetricCode {
    fn from(metric: CliMetric) -> Self {
        match metric {
            CliMetric::WorksCount => Self::WorksCount,
            CliMetric::CitedByCount => Self::CitedByCount,
            CliMetric::HIndex => Self::HIndex,
            CliMetric::I10Index => Self::I10Index,
            CliMetric::MeanCitedness2Yr => Self::MeanCitedness2Yr,
            CliMetric::CitationsPerWork => Self::CitationsPerWork,
            CliMetric::OaRate => Self::OaRate,
        }
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("cistella-headless: {error}");
        process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Doctor { json }) => run_doctor(json),
        Some(Commands::Vault { command }) => run_vault(command),
        Some(Commands::Source { command }) => run_source(command),
        Some(Commands::Search {
            vault,
            query,
            offset,
            limit,
        }) => {
            let vault = open_vault(vault)?;
            let result = vault
                .query_search_index(
                    &SearchQuery {
                        text: query,
                        scopes: vec![SearchFieldScope::All],
                    },
                    offset,
                    limit,
                )
                .map_err(|error| error.to_string())?;
            print_json(serde_json::to_value(result).map_err(|error| error.to_string())?)
        }
        Some(Commands::Literature { command }) => run_literature(command),
        Some(Commands::Reading { command }) => run_reading(command),
        Some(Commands::Notes { command }) => run_notes(command),
        Some(Commands::Settings { command }) => run_settings(command),
        None => {
            Cli::command()
                .print_help()
                .map_err(|error| error.to_string())?;
            println!();
            Ok(())
        }
    }
}

fn run_doctor(json: bool) -> Result<(), String> {
    let report = headless_doctor_report(env!("CARGO_PKG_VERSION"))?;

    if json {
        print_json(serde_json::to_value(report).map_err(|error| error.to_string())?)
    } else {
        print_doctor_report(&report);
        Ok(())
    }
}

fn run_vault(command: VaultCommand) -> Result<(), String> {
    match command {
        VaultCommand::Open { path } => {
            let vault = open_vault(path)?;
            print_json(serde_json::to_value(vault.context()).map_err(|error| error.to_string())?)
        }
    }
}

fn run_source(command: SourceCommand) -> Result<(), String> {
    match command {
        SourceCommand::Rank {
            vault,
            metric,
            r#type,
            limit,
        } => {
            let vault = open_vault(vault)?;
            print_json(
                vault
                    .top_sources_json(metric.into(), &r#type, limit)
                    .map_err(|error| error.to_string())?,
            )
        }
    }
}

fn run_literature(command: LiteratureCommand) -> Result<(), String> {
    match command {
        LiteratureCommand::List {
            vault,
            keyword,
            status,
            limit,
        } => {
            let vault = open_vault(vault)?;
            let keyword = keyword.map(|value| value.to_lowercase());
            let status = status.map(|value| value.to_lowercase());
            let mut items = vault
                .load_literature_items()
                .map_err(|error| error.to_string())?;
            items.retain(|item| {
                let keyword_matches = keyword.as_ref().is_none_or(|keyword| {
                    item.title.to_lowercase().contains(keyword)
                        || item
                            .authors
                            .iter()
                            .any(|author| author.to_lowercase().contains(keyword))
                        || item
                            .tags
                            .iter()
                            .any(|tag| tag.to_lowercase().contains(keyword))
                });
                let status_matches = status.as_ref().is_none_or(|status| {
                    format!("{:?}", item.reading_status).to_lowercase() == *status
                });
                keyword_matches && status_matches
            });
            items.truncate(limit);
            print_json(serde_json::to_value(items).map_err(|error| error.to_string())?)
        }
    }
}

fn run_reading(command: ReadingCommand) -> Result<(), String> {
    match command {
        ReadingCommand::Sessions { vault } => {
            let vault = open_vault(vault)?;
            print_json(
                serde_json::to_value(
                    vault
                        .list_recent_reading_sessions()
                        .map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?,
            )
        }
    }
}

fn run_notes(command: NotesCommand) -> Result<(), String> {
    match command {
        NotesCommand::List {
            vault,
            item_id,
            include_archived,
        } => {
            let vault = open_vault(vault)?;
            let item_id = item_id
                .map(|id| uuid::Uuid::parse_str(&id).map_err(|error| error.to_string()))
                .transpose()?;
            let notes = vault
                .list_notes(item_id, include_archived)
                .map_err(|error| error.to_string())?;
            let notes = notes
                .into_iter()
                .map(|note| {
                    json!({
                        "noteId": note.note_id,
                        "itemId": note.item_id,
                        "createdAt": note.created_at.to_rfc3339(),
                        "updatedAt": note.updated_at.to_rfc3339(),
                        "archivedAt": note.archived_at.map(|value| value.to_rfc3339()),
                        "title": note.title,
                        "markdownBody": note.markdown_body,
                        "revision": note.revision,
                    })
                })
                .collect::<Vec<_>>();
            print_json(Value::Array(notes))
        }
    }
}

fn run_settings(command: SettingsCommand) -> Result<(), String> {
    match command {
        SettingsCommand::Info => run_doctor(true),
    }
}

fn open_vault(path: PathBuf) -> Result<Vault, String> {
    Vault::open_any(path, VaultOpenOptions::default()).map_err(|error| error.to_string())
}

fn print_json(value: Value) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?
    );
    Ok(())
}

fn print_doctor_report(report: &HeadlessDoctorReport) {
    println!("cistella-headless doctor");
    println!("version: {}", report.version);
    println!("executable: {}", report.executable);
    println!("executable dir: {}", report.executable_dir);
    println!("mode: {}", report.mode);
    if let Some(portable_root) = report.portable_root.as_deref() {
        println!("portable root: {}", portable_root);
    }
    println!("config dir: {}", report.config_dir);
    println!("cache dir: {}", report.cache_dir);
    println!("recent vaults: {}", report.recent_vaults_path);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metric_mapping_keeps_cli_contract_stable() {
        assert_eq!(MetricCode::from(CliMetric::HIndex), MetricCode::HIndex);
        assert_eq!(MetricCode::from(CliMetric::OaRate), MetricCode::OaRate);
    }
}
