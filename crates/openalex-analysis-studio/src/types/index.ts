export type Row = Record<string, unknown>;
export type Adapter = { name: string; kind: string; is_default: boolean };
export type Workspace = 'vault' | 'reading' | 'search' | 'source' | 'settings' | 'notes';
export type WorkspaceKind = 'literature' | 'references' | 'writing' | 'assets' | 'analysis' | 'imports' | 'system' | 'cache';
export type WorkspaceStatus = 'ready' | 'degraded' | 'read_only' | 'failed' | 'closed';
export type WorkspaceMode = 'read_write' | 'read_only';
export type WorkspaceContract = {
  schemaVersion: string;
  manifestFile: string;
  defaultMode: WorkspaceMode;
  defaultStatus: WorkspaceStatus;
  standardKinds: WorkspaceKind[];
  registeredBlocks: string[];
};

export type SourceTab = 'overview' | 'table' | 'visual' | 'metrics' | 'export';
export type Lang = 'zh' | 'en';
export type MigrationMode = 'portable' | 'installed';

export type Overview = {
  source_count?: number;
  journal_count?: number;
  conference_count?: number;
  oa_count?: number;
  works_count?: number;
  cited_by_count?: number;
};

export type VaultSummary = {
  vault_id?: string;
  created_at?: string;
  source?: {
    name?: string;
    entity?: string;
    snapshot_date?: string | null;
    input_path?: string;
  };
};

export type VaultContext = {
  root?: string;
  tableCount?: number;
  manifest?: VaultSummary;
};

export type VaultRequestContext = {
  expectedGeneration: number;
  expectedVaultPath: string;
};

export interface VaultConnection {
  busy: boolean;
  setBusy: (value: boolean) => void;
  status: string;
  setStatus: (value: string) => void;
  restored: boolean;
  vaultError: string;
  setVaultError: (value: string) => void;
  vaultPath: string;
  setVaultPath: (value: string) => void;
  hasVault: boolean;
  recentVaults: RecentVaultDto[];
  setRecentVaults: (value: RecentVaultDto[]) => void;
  appDirs: AppDirectoriesDto | null;
  setAppDirs: (value: AppDirectoriesDto | null) => void;
  appVersion: string | null;
  updateCheck: UpdateCheck | null;
  migratePortableOpen: boolean;
  setMigratePortableOpen: (value: boolean) => void;
  migrateInstalledOpen: boolean;
  setMigrateInstalledOpen: (value: boolean) => void;
  generationRef: { current: number };
  run: (fn: () => Promise<unknown>) => Promise<void>;
  captureVaultRequest: () => VaultRequestContext;
  isCurrentVaultRequest: (request: VaultRequestContext) => boolean;
  beginVaultConnection: () => number;
  isCurrentVaultConnection: (generation: number) => boolean;
  clearVaultContext: (invalidatePending?: boolean) => void;
  connect: (path: string) => Promise<VaultContext | null>;
  refreshVaultContext: () => Promise<boolean>;
  confirmMigrateToPortable: () => Promise<void>;
  confirmMigrateToInstalled: () => Promise<void>;
  migrateToPortable: () => void;
  migrateToInstalled: () => void;
}

export type AppDirectoriesDto = {
  configDir: string;
  recentVaultsPath: string;
  cacheDir: string;
  isPortableMode: boolean;
  portableRoot: string | null;
};

export type UpdateCheck = { currentVersion: string; latestVersion: string; hasUpdate: boolean };

export type RecentVaultDto = {
  path: string;
  name: string;
  openedAt: string | null;
};

export type ImportPreview = {
  rawSourcesDir?: string;
  partitionCount?: number;
  hasManifest?: boolean;
  snapshotDate?: string | null;
};

export type ImportRequest = {
  rawSourcesDir: string;
  outputDir: string;
  buildArrowCache: boolean;
};

export type LiteratureImportPreviewItem = {
  recordId: string;
  sourceRecord: {
    recordId: string;
    sourceName: string;
    externalId?: string | null;
    externalIdentifiers: { namespace: string; value: string }[];
    title: string;
    authors: string[];
    publishedYear: number | null;
    itemType: string;
    rawFields: Record<string, string>;
  };
  matchedItemId?: string | null;
  defaultPolicy: 'merge' | 'skip' | 'create';
  selectedPolicy: 'merge' | 'skip' | 'create';
};

export type LiteratureImportPreview = {
  batchId: string;
  sourceName: string;
  items: LiteratureImportPreviewItem[];
};

export type LiteratureImportResult = {
  created: number;
  merged: number;
  skipped: number;
  errors: number;
  batchId: string;
};

export type OpenAlexWorkCandidate = {
  recordId: string;
  sourceName: string;
  externalId?: string | null;
  externalIdentifiers: { namespace: string; value: string }[];
  title: string;
  authors: string[];
  publishedYear: number | null;
  itemType: string;
  abstractText: string;
  keywords: string[];
  pages: string;
  volume: string;
  rawFields: Record<string, string>;
};

export type LiteratureImportFormat = 'bibtex' | 'ris' | 'openalex_works';

export type RemoteSourceRecord = {
  recordId: string;
  sourceName: string;
  externalId?: string | null;
  externalIdentifiers: { namespace: string; value: string }[];
  title: string;
  authors: string[];
  publishedYear: number | null;
  itemType: string;
  rawFields: Record<string, string>;
};

export type QuerySnapshot = {
  sourceType: string;
  metric: string;
  text: string;
  country: string;
  oaFilter: 'all' | 'oa' | 'non_oa';
};

export type LiteratureFile = {
  fileId: string;
  kind: 'vault' | 'external';
  path: string;
  displayName: string;
};

export type DocumentAssetKind = 'primary' | 'supplement' | 'version' | 'appendix' | 'other';
export type DocumentAssetStorageKind = 'vault' | 'external';
export type DocumentAssetStatus = 'available' | 'missing' | 'unreadable' | 'invalid' | 'externalUnavailable';

export type DocumentAsset = {
  assetId: string;
  itemId: string;
  assetKind: DocumentAssetKind;
  storageKind: DocumentAssetStorageKind;
  path: string;
  displayName: string;
  mediaType: string;
  fileSize: number | null;
  contentHash: string | null;
  importedAt: string | null;
  isDefault: boolean;
  status: DocumentAssetStatus;
};

export type ReadingSessionState = 'active' | 'paused' | 'closed';

export type ReadingSession = {
  sessionId: string;
  itemId: string;
  assetId: string;
  startedAt: string;
  lastOpenedAt: string;
  endedAt: string | null;
  state: ReadingSessionState;
};

export type ReadingSessionSummary = {
  session: ReadingSession;
  assetStatus: DocumentAssetStatus;
};

export type Note = {
  noteId: string;
  itemId: string;
  createdAt: string;
  updatedAt: string;
  archivedAt: string | null;
  title: string;
  markdownBody: string;
  revision: string;
};

export type Annotation = {
  annotationId: string;
  itemId: string;
  assetId: string;
  createdAt: string;
  updatedAt: string;
  kind: string;
  anchor: {
    assetId: string;
    pageNumber: number;
    selectedText: string;
    prefixContext: string;
    suffixContext: string;
  };
  resolution?: AnnotationResolution;
};

export type AnnotationResolution =
  | 'resolved_exact'
  | 'unavailable_missing_asset'
  | 'unavailable_external_asset'
  | 'unavailable_unreadable_asset'
  | 'invalidated_content_changed'
  | 'invalidated_page_out_of_range'
  | 'invalidated_text_not_found'
  | 'invalidated_ambiguous_text'
  | 'unsupported_extractor_version'
  | 'orphaned_item';

export type LiteratureItem = {
  itemId: string;
  title: string;
  authors: string[];
  publishedYear: number | null;
  itemType: 'article' | 'book' | 'chapter' | 'other';
  favorite: boolean;
  readingStatus: 'inbox' | 'reading' | 'finished' | 'archived';
  tags: string[];
  externalIdentifiers: { namespace: string; value: string }[];
  sources: { sourceName: string; externalId?: string | null; originalLocator?: string | null }[];
  files: LiteratureFile[];
  defaultFileId: string | null;
};

export type LiteratureDraft = Omit<LiteratureItem, 'itemId' | 'sources' | 'files' | 'defaultFileId'>;

export type LocalSearchScope = 'all' | 'title' | 'authors' | 'tags' | 'content';

export type LocalSearchIndexState = {
  status: 'missing' | 'ready' | 'stale' | 'building' | 'degraded' | 'failed';
  activeGeneration?: string | null;
  detail?: string | null;
};

export type LocalSearchTask = {
  status: 'idle' | 'building' | 'succeeded' | 'failed';
  generationId?: string | null;
  detail?: string | null;
};

export type LocalSearchFieldMatch = {
  field: 'title' | 'authors' | 'tags' | 'content';
  matchedTerms: string[];
  assetId?: string | null;
  assetState?: string | null;
  excerpt?: string | null;
};

export type LocalSearchHit = { itemId: string; fieldMatches: LocalSearchFieldMatch[] };

export type LocalSearchOutcome =
  | { outcome: 'ready'; page: { indexState: LocalSearchIndexState; hits: LocalSearchHit[]; totalHits: number; offset: number; limit: number } }
  | { outcome: 'unavailable'; indexState: LocalSearchIndexState };

export type LocalSearchIssues =
  | { outcome: 'ready'; issues: { itemId: string; assetId: string; kind: string; detail?: string | null }[] }
  | { outcome: 'unavailable'; indexState: LocalSearchIndexState };
