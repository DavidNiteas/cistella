use std::path::Path;

use serde::Serialize;

use crate::{
    error::Result,
    import::{
        ImportOptions, OpenAlexSourcesPreview, import_openalex_sources, inspect_openalex_sources,
    },
    manifest::VaultManifest,
};

#[derive(Debug, Clone, Copy, Serialize)]
pub struct SourceAdapterDescriptor {
    pub name: &'static str,
    pub kind: &'static str,
    pub is_default: bool,
}

pub trait SourceAdapter {
    fn descriptor(&self) -> SourceAdapterDescriptor;
    fn inspect(&self, raw_sources_dir: &Path) -> Result<OpenAlexSourcesPreview>;
    fn import(
        &self,
        raw_sources_dir: &Path,
        output_dir: &Path,
        build_arrow_cache: bool,
    ) -> Result<VaultManifest>;
}

pub const SOURCE_ADAPTERS: &[SourceAdapterDescriptor] = &[SourceAdapterDescriptor {
    name: "OpenAlex",
    kind: "source-import",
    is_default: true,
}];

pub fn available_source_adapters() -> &'static [SourceAdapterDescriptor] {
    SOURCE_ADAPTERS
}

#[derive(Debug, Default, Clone, Copy)]
pub struct OpenAlexSourcesAdapter;

pub fn openalex_sources_adapter() -> OpenAlexSourcesAdapter {
    OpenAlexSourcesAdapter
}

impl SourceAdapter for OpenAlexSourcesAdapter {
    fn descriptor(&self) -> SourceAdapterDescriptor {
        SourceAdapterDescriptor {
            name: "OpenAlex",
            kind: "source-import",
            is_default: true,
        }
    }

    fn inspect(&self, raw_sources_dir: &Path) -> Result<OpenAlexSourcesPreview> {
        inspect_openalex_sources(raw_sources_dir)
    }

    fn import(
        &self,
        raw_sources_dir: &Path,
        output_dir: &Path,
        build_arrow_cache: bool,
    ) -> Result<VaultManifest> {
        import_openalex_sources(ImportOptions {
            raw_sources_dir: raw_sources_dir.to_path_buf(),
            output_dir: output_dir.to_path_buf(),
            build_arrow_cache,
        })
    }
}
