# cistella Rust architecture

## Architectural position

cistella is a portable desktop workbench for a personal literature vault. The vault is the system’s first-class ownership and portability boundary; workspaces provide focused views over the current vault rather than independent standalone tool entry points.

## Crate split

- `cistella-core`
  - Core library for vaults, manifests, storage layout, source import, query and export.
  - Owns portable data semantics and reusable application capabilities.
  - Treats external providers such as OpenAlex as source adapters, not as the product model.
- `cistella-desktop`
  - Tauri 2 desktop application with an embedded React/Vite frontend.
  - The Rust side lives in `crates/openalex-analysis-studio/src-tauri` and exposes desktop commands backed by the core library.
  - The frontend lives in `crates/openalex-analysis-studio/src` and is bundled into the desktop application at build time.

The `openalex-analysis-studio` directory names are retained for now as historical repository paths. They are compatibility/path facts, not user-facing product names.

## Product workspace model

cistella centers on workspaces and the current vault:

1. **Vault workspace**
   - Open, create, connect to, or recover a local library vault.
   - Acts as the control point for the user’s personal literature asset.
2. **Reading workspace**
   - Manage reading flow, annotations, tags and notes.
3. **Source analysis workspace**
   - Connect or import supported external source data into the local vault.
   - Query, inspect, summarize and export source-level analytics.
4. **Settings**
   - Manage language, portable preferences and general behavior.

## Storage principles

1. A vault is the portable unit of ownership.
2. Parquet is the canonical compressed storage format for packaging and transfer.
3. Arrow IPC is the local serving cache for fast reads and desktop interaction.
4. A single logical schema is shared across physical layouts.
5. Runtime queries use Polars `LazyFrame` over the selected layout.
6. Source provenance is retained so imported data can be understood and traced without making the source provider the application’s identity.

## Source adapters

OpenAlex is an important supported data source. Its importer performs source-specific inspection and normalization into cistella vault data. The core adapter boundary is intentionally provider-neutral so additional sources can be added without redefining the vault model.

## Desktop-first delivery

The desktop application is the sole public product entry point. Any remaining scripted commands are development or build support only, not supported product workflows.

## Build and packaging

```powershell
pnpm build
cargo check -p cistella-desktop
cargo test -p cistella-core
```

Release builds should produce a portable desktop application centered on the cistella vault model.

