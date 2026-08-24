# OpenAlex Analysis Rust architecture

## Crate split

- `openalex-analysis-core`
  - Library + fixed CLI export.
  - Owns the logical schema, manifest, storage layout abstraction, import/export pipeline and Polars runtime query layer.
  - CLI binary: `openalex-analysis-core`.
- `openalex-analysis-studio`
  - Tauri 2 desktop application with embedded React/Vite frontend.
  - Rust side lives in `crates/openalex-analysis-studio/src-tauri` and calls `openalex-analysis-core` through Tauri commands.
  - Frontend lives in `crates/openalex-analysis-studio/src` and is bundled from `dist/` into the Tauri application at build time.

## Product workspace model

Studio has two separate workspaces plus settings:

1. **Data Processing workspace**
   - Select a raw OpenAlex Sources directory.
   - Import and build an Analysis Library.
   - Write compressed Parquet and optionally an Arrow IPC serving cache.
   - Export current ranking/search results to CSV/XLSX for Excel or external analysis.
2. **Analysis workspace**
   - Connect to an external library directory containing `manifest.json`, or directly to a single `sources.arrow`/`sources.ipc`/`sources.parquet` file.
   - The application does not bundle, hardcode or own user libraries; it only connects to paths chosen by the user.
   - Provides overview cards, metric ranking chart and searchable source table.
3. **Settings**
   - Bilingual UI: Chinese / English toggle, persisted in browser local storage.

## Storage principles

1. Parquet layout is the canonical compressed storage format for packaging, transfer and archive.
2. Arrow IPC layout is a local serving cache for fast startup, memory-mapped/zero-copy-oriented reads and GUI interaction.
3. Both layouts share one logical schema and one manifest; only physical layout differs.
4. Runtime queries use Polars `LazyFrame`; Parquet scans are decompressed/collected into memory, while Arrow IPC scans are the path intended for local low-copy/mmap serving.

## Dataset layout

```text
openalex-library/
├── manifest.json
├── parquet/
│   └── sources.parquet
└── arrow/
    └── sources.arrow
```

Planned logical tables:

- `sources` *(implemented)*
- `source_year_metrics`
- `source_topics`
- `source_issns`
- `metric_definitions`
- `ranking_snapshots`

## Core CLI

```powershell
cargo run -p openalex-analysis-core -- import-sources openalex-sources openalex-library
cargo run -p openalex-analysis-core -- overview openalex-library
cargo run -p openalex-analysis-core -- overview openalex-library\arrow\sources.arrow
cargo run -p openalex-analysis-core -- top openalex-library journal h_index 20
cargo run -p openalex-analysis-core -- search openalex-library journal 20 Nature
cargo run -p openalex-analysis-core -- export-top openalex-library exports\top_sources.xlsx journal h_index 100
cargo run -p openalex-analysis-core -- export-search openalex-library exports\sources.csv journal 1000 Nature
```

## Studio frontend and Tauri

```powershell
cd crates/openalex-analysis-studio
pnpm install
pnpm build
cd ../..
cargo build -p openalex-analysis-studio
```

Build release executables:

```powershell
cargo build --release -p openalex-analysis-core
cargo build --release -p openalex-analysis-studio
```

For a full Tauri installer/bundle, use the Tauri CLI from the studio directory:

```powershell
pnpm tauri build
```
