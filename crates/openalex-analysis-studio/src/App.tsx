import { invoke } from '@tauri-apps/api/core';
import { open, save } from '@tauri-apps/plugin-dialog';
import ReactECharts from 'echarts-for-react';
import { useEffect, useMemo, useRef, useState } from 'react';
import { createLocalSearchTaskPoller } from './localSearchPolling';
import './style.css';

type Row = Record<string, unknown>;
type Adapter = { name: string; kind: string; is_default: boolean };
type Workspace = 'vault' | 'reading' | 'search' | 'source' | 'settings' | 'notes';
type SourceTab = 'overview' | 'table' | 'visual' | 'metrics' | 'export';
type Lang = 'zh' | 'en';
type Overview = {
  source_count?: number;
  journal_count?: number;
  conference_count?: number;
  oa_count?: number;
  works_count?: number;
  cited_by_count?: number;
};

type VaultSummary = {
  vault_id?: string;
  created_at?: string;
  source?: {
    name?: string;
    entity?: string;
    snapshot_date?: string | null;
    input_path?: string;
  };
};

type VaultContext = {
  root?: string;
  tableCount?: number;
  manifest?: VaultSummary;
};
type VaultRequestContext = {
  expectedGeneration: number;
  expectedVaultPath: string;
};

type ImportPreview = {
  rawSourcesDir?: string;
  partitionCount?: number;
  hasManifest?: boolean;
  snapshotDate?: string | null;
};

type ImportRequest = {
  rawSourcesDir: string;
  outputDir: string;
  buildArrowCache: boolean;
};

type LiteratureImportPreviewItem = {
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

type LiteratureImportPreview = {
  batchId: string;
  sourceName: string;
  items: LiteratureImportPreviewItem[];
};

type LiteratureImportResult = {
  created: number;
  merged: number;
  skipped: number;
  errors: number;
  batchId: string;
};

type OpenAlexWorkCandidate = {
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

type LiteratureImportFormat = 'bibtex' | 'ris' | 'openalex_works';

type QuerySnapshot = {
  sourceType: string;
  metric: string;
  text: string;
  country: string;
  oaFilter: 'all' | 'oa' | 'non_oa';
};


type LiteratureFile = {
  fileId: string;
  kind: 'vault' | 'external';
  path: string;
  displayName: string;
};

type DocumentAssetKind = 'primary' | 'supplement' | 'version' | 'appendix' | 'other';
type DocumentAssetStorageKind = 'vault' | 'external';
type DocumentAssetStatus = 'available' | 'missing' | 'unreadable' | 'invalid' | 'externalUnavailable';
type DocumentAsset = {
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

type ReadingSessionState = 'active' | 'paused' | 'closed';
type ReadingSession = {
  sessionId: string;
  itemId: string;
  assetId: string;
  startedAt: string;
  lastOpenedAt: string;
  endedAt: string | null;
  state: ReadingSessionState;
};
type ReadingSessionSummary = {
  session: ReadingSession;
  assetStatus: DocumentAssetStatus;
};

type Note = {
  noteId: string;
  itemId: string;
  createdAt: string;
  updatedAt: string;
  archivedAt: string | null;
  title: string;
  markdownBody: string;
  revision: string;
};

type Annotation = {
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

type AnnotationResolution =
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

type LiteratureItem = {
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
type LiteratureDraft = Omit<LiteratureItem, 'itemId' | 'sources' | 'files' | 'defaultFileId'>;
type LocalSearchScope = 'all' | 'title' | 'authors' | 'tags' | 'content';
type LocalSearchIndexState = { status: 'missing' | 'ready' | 'stale' | 'building' | 'degraded' | 'failed'; activeGeneration?: string | null; detail?: string | null };
type LocalSearchTask = { status: 'idle' | 'building' | 'succeeded' | 'failed'; generationId?: string | null; detail?: string | null };
type LocalSearchFieldMatch = { field: 'title' | 'authors' | 'tags' | 'content'; matchedTerms: string[]; assetId?: string | null; assetState?: string | null; excerpt?: string | null };
type LocalSearchHit = { itemId: string; fieldMatches: LocalSearchFieldMatch[] };
type LocalSearchOutcome = { outcome: 'ready'; page: { indexState: LocalSearchIndexState; hits: LocalSearchHit[]; totalHits: number; offset: number; limit: number } } | { outcome: 'unavailable'; indexState: LocalSearchIndexState };
type LocalSearchIssues = { outcome: 'ready'; issues: { itemId: string; assetId: string; kind: string; detail?: string | null }[] } | { outcome: 'unavailable'; indexState: LocalSearchIndexState };

type Dict = typeof zh;

const zh = {
  brand: 'cistella',
  sub: '本地化个人文献保管库 + 分析工作台',
  vault: '库',
  reading: '阅读',
  source: 'Source 分析',
  settings: '设置',
  ready: '就绪',
  busy: '处理中',
  failed: '操作失败',
  restoring: '正在恢复最近库…',
  connecting: '正在连接库…',
  chooseFirst: '请先选择路径。',
  connected: '已连接',
  imported: '导入完成',
  noData: '尚未连接库',
  connectForAnalysis: '请先连接一个库，再开始分析。',
  analysisFailed: 'Source 分析失败，结果已清空，请重试。',
  queryPending: '筛选已修改，点击查询后更新结果。',
  openForAnalysis: '连接库',
  vaultTitle: '库工作区',
  vaultDesc: '打开、连接、导入或管理你的本地 cistella 库。',
  sourceTitle: 'Source 分析工作区',
  sourceDesc: '在独立工作区里浏览、筛选、排行并导出 source 数据。',
  readingTitle: '阅读工作区',
  readingDesc: '浏览和维护当前库中的个人文献条目。',
  literatureCount: '文献条目',
  literatureKeyword: '关键词筛选',
  addLiterature: '新建条目',
  importLiterature: '导入文献',
  importLiteratureTitle: '导入文献',
  importLiteratureDesc: '选择文件预览并导入到当前库。',
  importLiteratureFormat: '导入格式',
  chooseBibtex: '选择 .bib 文件',
  chooseRis: '选择 .ris 文件',
  openAlexWorksFormat: 'OpenAlex works',
  openAlexWorksTitle: '本地 OpenAlex works',
  openAlexWorksQuery: 'DOI / OpenAlex ID / 标题',
  searchOpenAlexWorks: '搜索本地 works',
  importPreviewTitle: '导入预览',
  importCommit: '确认导入',
  importPolicyMerge: '合并',
  importPolicySkip: '跳过',
  importPolicyCreate: '新建',
  importConflictMatched: '已匹配现有条目',
  importConflictNew: '新条目',
  importResultTitle: '导入结果',
  editLiterature: '编辑条目',
  saveLiterature: '保存条目',
  cancel: '取消',
  noLiterature: '当前库还没有文献条目。',
  noVaultReading: '请先连接一个库，再管理文献条目。',
  title: '标题',
  authors: '作者（每行一位）',
  publishedYear: '出版年',
  itemType: '类型',
  tags: '标签（逗号分隔）',
  readingStatus: '阅读状态',
  favorite: '收藏',
  deleteLiterature: '删除',
  addToVault: '加入 Vault',
  linkExternal: '仅链接外部文件',
  openDefault: '打开默认文件',
  openFile: '打开',
  setDefaultFile: '设为默认',
  removeFile: '移除关联',
  noFiles: '尚未关联 PDF 文件。',
  noDefaultFile: '没有有效的默认文件；请从文件列表中选择。',
  externalFile: '外部链接 · 不随库复制',
  vaultFile: '库内副本 · 可随库复制',
  fileRequestAccepted: '已向系统默认 PDF 阅读器发起打开请求。',
  fileOperationFailed: '文件操作失败',
  linkedFiles: '关联的 PDF 文件',
  saveBeforeFiles: '请先保存条目，再关联 PDF 文件。',
  defaultFile: '默认文件',
  assetCenter: '文献资产',
  assetsInVault: '当前 Vault 中的 PDF 资产',
  importAsset: '收纳 PDF',
  linkExternalAsset: '添加外部链接',
  assetKind: '资产类型',
  assetStorage: '存储方式',
  assetHealth: '健康状态',
  assetHash: 'SHA-256',
  assetSize: '大小',
  assetPath: '位置',
  migrateToVault: '复制入 Vault',
  removeAsset: '移除资产记录',
  assetMigrated: '已复制为新的 Vault 资产；原外部链接仍被保留。',
  assetLinked: '已添加外部 PDF 链接；该文件不会随 Vault 复制。',
  assetDuplicate: 'Vault 中已有相同内容的资产；原外部链接仍被保留。',
  assetStatuses: { available: '可用', missing: '文件缺失', unreadable: '无法读取', invalid: '无效', externalUnavailable: '外部链接不可用' },
  assetKinds: { primary: '正文', supplement: '补充材料', version: '版本', appendix: '附录', other: '其他' },
  recentReading: '最近阅读',
  recentReadingDesc: '会话会随 Vault 保存；资产暂时不可用时，历史仍会保留。',
  continueReading: '继续阅读',
  startReading: '开始阅读',
  resumeReading: '恢复阅读',
  pauseReading: '暂停会话',
  endReading: '结束会话',
  noRecentReading: '尚无阅读会话。请从下方的文献资产开始阅读。',
  readingSessionState: '会话状态',
  readingSessionStates: { active: '进行中', paused: '已暂停', closed: '已结束' },
  lastOpenedAt: '最近打开',
  startedAt: '开始于',
  sessionUnavailable: '当前资产不可用；会话历史已保留，无法继续打开。',
  readingSessionOpened: '已向系统默认 PDF 阅读器发起阅读请求。',
  readingSessionUpdated: '阅读会话已更新。',
  readingAssets: '可开始阅读的资产',
  manageAssetsInVault: '资产收纳与管理请前往 Vault 工作区。',
  inbox: '待处理',
  readingNow: '阅读中',
  finished: '已完成',
  archived: '已归档',
  noteArchived: '已归档',
  recentVaults: '最近库',
  openVault: '打开库',
  chooseVault: '选择库',
  chooseDir: '选择目录',
  refreshVault: '刷新库信息',
  currentVault: '当前库',
  vaultId: '库 ID',
  chooseFile: '选择文件',
  connect: '连接',
  importFromOpenAlex: '从 OpenAlex 导入',
  inspectSources: '检查来源',
  importPreview: '导入预检',
  importFailed: '导入失败，可重试',
  retryImport: '重试导入',
  buildVault: '构建库',
  buildCache: '生成快速缓存',
  sourceOverview: '库概览',
  layout: '数据布局',
  raw: '原始 OpenAlex Sources 目录',
  output: '输出库目录',
  rawTip: '请选择包含 updated_date=*/part_0000.parquet 的 OpenAlex Sources 目录。',
  outTip: '输出目录会写入 manifest 与派生布局。',
  parquet: 'Parquet：压缩、归档、打包和迁移',
  arrow: 'Arrow：本地快速读取，面向零拷贝/mmap',
  manifest: 'Manifest：记录逻辑 schema、物理文件和来源信息',
  overview: '总览',
  table: '数据表',
  visual: '可视化',
  metrics: '指标分析',
  export: '导出',
  type: '类型',
  journal: '期刊',
  conference: '会议',
  metric: '指标',
  keyword: '名称关键词',
  country: '国家代码',
  oaFilter: 'OA 筛选',
  all: '全部',
  onlyOa: '仅 OA',
  onlyNonOa: '非 OA',
  run: '查询',
  ranking: '影响力排行',
  results: '结果表',
  recentQueries: '最近查询',
  replayQuery: '回放查询',
  noRecentQueries: '暂无最近查询。',
  exportRank: '导出排行',
  exportSearch: '导出检索',
  sources: 'Sources',
  journals: '期刊',
  conferences: '会议',
  oa: 'OA 来源',
  works: 'Works',
  cited: '总被引',
  metricGuide: '指标解读',
  hTip: 'H-index：稳健综合影响力，适合初筛和横向比较。',
  citedTip: '总被引：反映长期累积影响，但受出版年限和规模影响。',
  worksTip: 'Works 数：反映产出规模，不等于质量。',
  i10Tip: 'i10-index：至少被引 10 次的作品数量，反映稳定产出。',
  meanTip: '两年平均被引：更偏近期热度。',
  workflow: '工作流建议',
  language: '界面语言',
  chinese: '中文',
  english: 'English',
  metricOptions: [
    ['h_index', 'H-index'],
    ['cited_by_count', '总被引'],
    ['works_count', 'Works 数'],
    ['i10_index', 'i10-index'],
    ['mean_citedness_2yr', '两年平均被引'],
  ],
  notes: '笔记',
  notesTitle: 'Notes 工作区',
  notesDesc: '按文献管理 Markdown 笔记，并在标注面板查看引用状态。',
  noteItemFilter: '文献筛选',
  allItems: '全部文献',
  noteTitle: '标题',
  noteBody: '正文',
  newNote: '新建笔记',
  saveNote: '保存',
  savingNote: '保存中…',
  noteConflict: '保存冲突',
  noteConflictMessage: '该笔记已被其他实例修改。你的草稿仍保留在编辑器中；请刷新后确认是否覆盖。',
  refreshNote: '刷新',
  overwriteNote: '覆盖',
  archiveNote: '归档',
  unarchiveNote: '恢复',
  annotations: '标注',
  noNotes: '当前库还没有笔记。',
  noAnnotations: '该文献没有标注。',
  openAssociatedAsset: '打开关联资产',
  annotationPage: '页',
  annotationResolution: '解析状态',
  resolvedExact: '精确定位',
  annotationUnresolvable: '无法定位',
  chooseItemForNote: '请先选择要归属的文献条目。',
  noteLoadError: '笔记加载失败',
  annotationLoadError: '标注加载失败',
  noteSaved: '笔记已保存。',
  noteOperationFailed: '笔记操作失败',
};

const en: Dict = {
  ...zh,
  importLiterature: 'Import literature',
  importLiteratureTitle: 'Import literature',
  importLiteratureDesc: 'Choose a file to preview and import into the current vault.',
  importLiteratureFormat: 'Import format',
  chooseBibtex: 'Choose .bib file',
  chooseRis: 'Choose .ris file',
  openAlexWorksFormat: 'OpenAlex works',
  openAlexWorksTitle: 'Local OpenAlex works',
  openAlexWorksQuery: 'DOI / OpenAlex ID / title',
  searchOpenAlexWorks: 'Search local works',
  importPreviewTitle: 'Import preview',
  importCommit: 'Confirm import',
  importPolicyMerge: 'Merge',
  importPolicySkip: 'Skip',
  importPolicyCreate: 'Create',
  importConflictMatched: 'Matches existing item',
  importConflictNew: 'New item',
  importResultTitle: 'Import result',
  sub: 'Personal literature vault and analysis workbench',
  vault: 'Vault',
  reading: 'Reading',
  source: 'Source Analysis',
  settings: 'Settings',
  ready: 'Ready',
  busy: 'Working',
  failed: 'Failed',
  restoring: 'Restoring recent vault…',
  connecting: 'Connecting vault…',
  chooseFirst: 'Choose a path first.',
  connected: 'Connected',
  imported: 'Imported',
  noData: 'No vault connected',
  connectForAnalysis: 'Connect a vault before starting analysis.',
  analysisFailed: 'Source analysis failed; results were cleared. Retry when ready.',
  queryPending: 'Filters changed. Run the query to update results.',
  openForAnalysis: 'Connect vault',
  vaultTitle: 'Vault Workspace',
  vaultDesc: 'Open, connect, import or manage your local cistella vault.',
  sourceTitle: 'Source Analysis Workspace',
  sourceDesc: 'Browse, filter, rank and export source data in a dedicated workspace.',
  readingTitle: 'Reading Workspace',
  readingDesc: 'Browse and maintain personal literature items in the current vault.',
  literatureCount: 'literature items',
  literatureKeyword: 'Filter by keyword',
  addLiterature: 'New item',
  editLiterature: 'Edit item',
  saveLiterature: 'Save item',
  cancel: 'Cancel',
  noLiterature: 'This vault has no literature items yet.',
  noVaultReading: 'Connect a vault before managing literature items.',
  title: 'Title',
  authors: 'Authors (one per line)',
  publishedYear: 'Published year',
  itemType: 'Type',
  tags: 'Tags (comma separated)',
  readingStatus: 'Reading status',
  favorite: 'Favorite',
  deleteLiterature: 'Delete',
  addToVault: 'Add to Vault',
  linkExternal: 'Link external file',
  openDefault: 'Open default file',
  openFile: 'Open',
  setDefaultFile: 'Set default',
  removeFile: 'Remove link',
  noFiles: 'No PDF files are linked yet.',
  noDefaultFile: 'No valid default file. Choose one from the file list.',
  externalFile: 'External link · not copied with this vault',
  vaultFile: 'Vault copy · portable with this vault',
  fileRequestAccepted: 'An open request was sent to the system PDF reader.',
  fileOperationFailed: 'File operation failed',
  linkedFiles: 'Linked PDF files',
  saveBeforeFiles: 'Save the item before linking PDF files.',
  defaultFile: 'Default file',
  assetCenter: 'Document assets',
  assetsInVault: 'PDF assets in the current Vault',
  importAsset: 'Import PDF',
  linkExternalAsset: 'Add external link',
  assetKind: 'Asset kind',
  assetStorage: 'Storage',
  assetHealth: 'Health',
  assetHash: 'SHA-256',
  assetSize: 'Size',
  assetPath: 'Location',
  migrateToVault: 'Copy into Vault',
  removeAsset: 'Remove asset record',
  assetMigrated: 'A new Vault asset was created; the external link remains.',
  assetLinked: 'The external PDF link was added; this file will not travel with the Vault.',
  assetDuplicate: 'A matching Vault asset already exists; the external link remains.',
  assetStatuses: { available: 'Available', missing: 'Missing', unreadable: 'Unreadable', invalid: 'Invalid', externalUnavailable: 'External link unavailable' },
  assetKinds: { primary: 'Primary', supplement: 'Supplement', version: 'Version', appendix: 'Appendix', other: 'Other' },
  recentReading: 'Recent reading',
  recentReadingDesc: 'Sessions stay with the Vault; unavailable assets retain their history.',
  continueReading: 'Continue reading',
  startReading: 'Start reading',
  resumeReading: 'Resume reading',
  pauseReading: 'Pause session',
  endReading: 'End session',
  noRecentReading: 'No reading sessions yet. Start from a literature asset below.',
  readingSessionState: 'Session state',
  readingSessionStates: { active: 'Active', paused: 'Paused', closed: 'Closed' },
  lastOpenedAt: 'Last opened',
  startedAt: 'Started',
  sessionUnavailable: 'This asset is currently unavailable. Its session history was kept, but it cannot be opened.',
  readingSessionOpened: 'A reading request was sent to the system PDF reader.',
  readingSessionUpdated: 'The reading session was updated.',
  readingAssets: 'Assets ready to start reading',
  manageAssetsInVault: 'Use the Vault workspace to collect and manage assets.',
  inbox: 'Inbox',
  readingNow: 'Reading',
  finished: 'Finished',
  archived: 'Archived',
  noteArchived: 'Archived',
  recentVaults: 'Recent vaults',
  openVault: 'Open vault',
  chooseVault: 'Choose vault',
  chooseDir: 'Choose folder',
  refreshVault: 'Refresh vault',
  currentVault: 'Current vault',
  vaultId: 'Vault ID',
  chooseFile: 'Choose file',
  connect: 'Connect',
  importFromOpenAlex: 'Import from OpenAlex',
  inspectSources: 'Inspect source',
  importPreview: 'Import preview',
  importFailed: 'Import failed; retry is available',
  retryImport: 'Retry import',
  buildVault: 'Build vault',
  buildCache: 'Build fast cache',
  sourceOverview: 'Vault overview',
  layout: 'Storage layout',
  raw: 'Raw OpenAlex Sources folder',
  output: 'Output vault folder',
  rawTip: 'Choose a folder containing OpenAlex Sources partitions like updated_date=*/part_0000.parquet.',
  outTip: 'The output folder will contain manifest and derived layouts.',
  parquet: 'Parquet: compression, archive, package and transfer',
  arrow: 'Arrow: fast local reads, zero-copy/mmap-oriented',
  manifest: 'Manifest: logical schema, physical files and provenance',
  overview: 'Overview',
  table: 'Table',
  visual: 'Visualize',
  metrics: 'Metrics',
  export: 'Export',
  type: 'Type',
  journal: 'Journal',
  conference: 'Conference',
  metric: 'Metric',
  keyword: 'Name keyword',
  country: 'Country code',
  oaFilter: 'OA filter',
  all: 'All',
  onlyOa: 'OA only',
  onlyNonOa: 'Non-OA',
  run: 'Run',
  ranking: 'Impact ranking',
  results: 'Results table',
  exportRank: 'Export ranking',
  exportSearch: 'Export search',
  sources: 'Sources',
  journals: 'Journals',
  conferences: 'Conferences',
  oa: 'OA sources',
  works: 'Works',
  cited: 'Citations',
  metricGuide: 'Metric guide',
  hTip: 'H-index: robust overall impact for first-pass screening.',
  citedTip: 'Citations: long-term accumulated impact, biased by age and size.',
  worksTip: 'Works count: output scale, not quality.',
  i10Tip: 'i10-index: number of works cited at least ten times.',
  meanTip: '2-year mean citedness: recent momentum.',
  workflow: 'Workflow tips',
  language: 'Language',
  chinese: '中文',
  english: 'English',
  metricOptions: [
    ['h_index', 'H-index'],
    ['cited_by_count', 'Cited by'],
    ['works_count', 'Works'],
    ['i10_index', 'i10-index'],
    ['mean_citedness_2yr', '2-year mean citedness'],
  ],
  notes: 'Notes',
  notesTitle: 'Notes Workspace',
  notesDesc: 'Manage Markdown notes by literature item and inspect annotation resolution.',
  noteItemFilter: 'Item filter',
  allItems: 'All items',
  noteTitle: 'Title',
  noteBody: 'Body',
  newNote: 'New note',
  saveNote: 'Save',
  savingNote: 'Saving…',
  noteConflict: 'Note conflict',
  noteConflictMessage: 'This note was changed elsewhere. Your draft is kept in the editor; refresh before overwriting.',
  refreshNote: 'Refresh',
  overwriteNote: 'Overwrite',
  archiveNote: 'Archive',
  unarchiveNote: 'Restore',
  annotations: 'Annotations',
  noNotes: 'This vault has no notes yet.',
  noAnnotations: 'No annotations for this item.',
  openAssociatedAsset: 'Open associated asset',
  annotationPage: 'Page',
  annotationResolution: 'Resolution',
  resolvedExact: 'Exactly resolved',
  annotationUnresolvable: 'Cannot locate',
  chooseItemForNote: 'Choose a literature item first.',
  noteLoadError: 'Failed to load notes',
  annotationLoadError: 'Failed to load annotations',
  noteSaved: 'Note saved.',
  noteOperationFailed: 'Note operation failed',
};

const dict = { zh, en };

function loadWorkspace(): Workspace {
  const value = localStorage.getItem('workspace');
  return value === 'reading' || value === 'search' || value === 'source' || value === 'settings' || value === 'notes' ? value : 'vault';
}
function loadSourceTab(): SourceTab {
  const value = localStorage.getItem('sourceTab');
  return value === 'table' || value === 'visual' || value === 'metrics' || value === 'export' ? value : 'overview';
}

function rows(v: unknown): Row[] { return Array.isArray(v) ? v as Row[] : []; }
function first(v: unknown): Overview { return (Array.isArray(v) ? v[0] : v || {}) as Overview; }
function fmt(v: unknown) { return typeof v === 'number' ? Math.round(v).toLocaleString() : v == null || v === '' ? '—' : String(v); }
function short(p: string) { return !p ? '—' : p.length > 82 ? `…${p.slice(-79)}` : p; }
function formatSessionTime(value: string, lang: Lang) {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString(lang === 'zh' ? 'zh-CN' : 'en-US');
}
function parseNoteConflict(error: unknown): { noteId: string; message: string } | null {
  const raw = typeof error === 'string' ? error : (error as any)?.message;
  const message = String(raw ?? '');
  if (message.startsWith('NOTE_CONFLICT|')) {
    const noteId = message.slice('NOTE_CONFLICT|'.length);
    return { noteId, message: `note revision conflict: ${noteId}` };
  }
  return null;
}
function loadRecent(): string[] { try { return JSON.parse(localStorage.getItem('recentVaults') || '[]'); } catch { return []; } }
function remember(path: string) { const next = [path, ...loadRecent().filter(p => p !== path)].slice(0, 6); localStorage.setItem('recentVaults', JSON.stringify(next)); return next; }
function loadRecentQueries(): QuerySnapshot[] { try { return JSON.parse(localStorage.getItem('recentSourceQueries') || '[]'); } catch { return []; } }
function rememberQuery(snapshot: QuerySnapshot) { const next = [snapshot, ...loadRecentQueries().filter(q => JSON.stringify(q) !== JSON.stringify(snapshot))].slice(0, 5); localStorage.setItem('recentSourceQueries', JSON.stringify(next)); return next; }
function queryLabel(q: QuerySnapshot, lang: Lang) {
  const query = q.text.trim() ? q.text.trim() : (lang === 'zh' ? '空关键词' : 'empty');
  const country = q.country.trim() ? q.country.trim().toUpperCase() : (lang === 'zh' ? '不限国家' : 'any country');
  const oa = q.oaFilter === 'oa' ? 'OA' : q.oaFilter === 'non_oa' ? 'non-OA' : (lang === 'zh' ? '全部OA状态' : 'all OA states');
  return `${q.sourceType} · ${q.metric} · ${query} · ${country} · ${oa}`;
}
function summarizeManifest(v: unknown): VaultSummary {
  const m = v as any;
  return {
    vault_id: typeof m?.vault_id === 'string' ? m.vault_id : undefined,
    created_at: typeof m?.created_at === 'string' ? m.created_at : undefined,
    source: m?.source && typeof m.source === 'object' ? {
      name: typeof m.source.name === 'string' ? m.source.name : undefined,
      entity: typeof m.source.entity === 'string' ? m.source.entity : undefined,
      snapshot_date: typeof m.source.snapshot_date === 'string' ? m.source.snapshot_date : null,
      input_path: typeof m.source.input_path === 'string' ? m.source.input_path : undefined,
    } : undefined,
  };
}
async function dir() { const v = await open({ directory: true, multiple: false }); return typeof v === 'string' ? v : ''; }
async function file() { const v = await open({ multiple: false, filters: [{ name: 'OpenAlex data', extensions: ['arrow', 'ipc', 'parquet'] }] }); return typeof v === 'string' ? v : ''; }
async function pdfFile() { const v = await open({ multiple: false, filters: [{ name: 'PDF', extensions: ['pdf'] }] }); return typeof v === 'string' ? v : ''; }
async function literatureFile(format: LiteratureImportFormat) {
  const extensions = format === 'bibtex' ? ['bib'] : ['ris'];
  const name = format === 'bibtex' ? 'BibTeX' : 'RIS';
  const v = await open({ multiple: false, filters: [{ name, extensions }] });
  return typeof v === 'string' ? v : '';
}

function emptyLiteratureDraft(): LiteratureDraft {
  return { title: '', authors: [], publishedYear: null, itemType: 'article', favorite: false, readingStatus: 'inbox', tags: [], externalIdentifiers: [] };
}

export default function App() {
  const [lang, setLang] = useState<Lang>((localStorage.getItem('lang') as Lang) === 'en' ? 'en' : 'zh');
  const t = dict[lang];
  const [workspace, setWorkspace] = useState<Workspace>(loadWorkspace);
  const [sourceTab, setSourceTab] = useState<SourceTab>(loadSourceTab);
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState(t.ready);
  const [restored, setRestored] = useState(false);
  const [vaultError, setVaultError] = useState('');
  const [vaultPath, setVaultPath] = useState('');
  const vaultPathRef = useRef('');
  const literatureFileInputRef = useRef<HTMLInputElement>(null);
  // Each connection gets a generation. Any response from an earlier generation is ignored.
  const vaultConnectionGenerationRef = useRef(0);
  const [vaultSummary, setVaultSummary] = useState<VaultSummary | null>(null);
  const [vaultTableCount, setVaultTableCount] = useState<number | null>(null);
  const [rawDir, setRawDir] = useState('');
  const [importPreview, setImportPreview] = useState<ImportPreview | null>(null);
  const [lastImport, setLastImport] = useState<ImportRequest | null>(null);
  const [importError, setImportError] = useState('');
  const [recentVaults, setRecentVaults] = useState<string[]>(loadRecent());
  const [sourceAdapters, setSourceAdapters] = useState<Adapter[]>([{ name: 'OpenAlex', kind: 'source-import', is_default: true }]);
  const [overview, setOverview] = useState<Overview | null>(null);
  const [analysisError, setAnalysisError] = useState('');
  const [ranking, setRanking] = useState<Row[]>([]);
  const [results, setResults] = useState<Row[]>([]);
  const [recentQueries, setRecentQueries] = useState<QuerySnapshot[]>(loadRecentQueries());
  const [sourceType, setSourceType] = useState('journal');
  const [metric, setMetric] = useState('h_index');
  const [text, setText] = useState('');
  const [country, setCountry] = useState('');
  const [oaFilter, setOaFilter] = useState<'all' | 'oa' | 'non_oa'>('all');
  const [appliedQuery, setAppliedQuery] = useState<QuerySnapshot>({ sourceType: 'journal', metric: 'h_index', text: '', country: '', oaFilter: 'all' });
  const [literatureItems, setLiteratureItems] = useState<LiteratureItem[]>([]);
  const [documentAssets, setDocumentAssets] = useState<DocumentAsset[]>([]);
  const [readingSessions, setReadingSessions] = useState<ReadingSessionSummary[]>([]);
  const [readingFeedback, setReadingFeedback] = useState('');
  const [literatureKeyword, setLiteratureKeyword] = useState('');
  const [editingLiteratureId, setEditingLiteratureId] = useState<string | null>(null);
  const [literatureEditorOpen, setLiteratureEditorOpen] = useState(false);
  const [literatureDraft, setLiteratureDraft] = useState<LiteratureDraft>(emptyLiteratureDraft());
  const [literatureFeedback, setLiteratureFeedback] = useState('');
  const [literatureImportFormat, setLiteratureImportFormat] = useState<LiteratureImportFormat>('bibtex');
  const [literatureImportPreview, setLiteratureImportPreview] = useState<LiteratureImportPreview | null>(null);
  const [literatureImportError, setLiteratureImportError] = useState('');
  const [literatureImportResult, setLiteratureImportResult] = useState<LiteratureImportResult | null>(null);
  const [openAlexWorksDir, setOpenAlexWorksDir] = useState('');
  const [openAlexQuery, setOpenAlexQuery] = useState('');
  const [openAlexCandidates, setOpenAlexCandidates] = useState<OpenAlexWorkCandidate[]>([]);
  const [openAlexLoading, setOpenAlexLoading] = useState(false);
  const [openAlexError, setOpenAlexError] = useState('');
  // Search owns an independent, ID-only view. It is cleared before every
  // connection attempt and every asynchronous response is guarded by the same
  // generation + Vault-path snapshot used by reading and Source analysis.
  const [localSearchText, setLocalSearchText] = useState('');
  const [localSearchScope, setLocalSearchScope] = useState<LocalSearchScope>('all');
  const [localSearchOffset, setLocalSearchOffset] = useState(0);
  const [localSearchOutcome, setLocalSearchOutcome] = useState<LocalSearchOutcome | null>(null);
  const [localSearchIndexState, setLocalSearchIndexState] = useState<LocalSearchIndexState | null>(null);
  const [localSearchTask, setLocalSearchTask] = useState<LocalSearchTask | null>(null);
  const [localSearchIssues, setLocalSearchIssues] = useState<LocalSearchIssues | null>(null);
  const [localSearchLoading, setLocalSearchLoading] = useState(false);
  const [localSearchError, setLocalSearchError] = useState('');
  // Notes workspace state
  const [notes, setNotes] = useState<Note[]>([]);
  const [noteFilterItemId, setNoteFilterItemId] = useState<string>('all');
  const [selectedNoteId, setSelectedNoteId] = useState<string | null>(null);
  const [noteEditorTitle, setNoteEditorTitle] = useState('');
  const [noteEditorBody, setNoteEditorBody] = useState('');
  const [noteLoading, setNoteLoading] = useState(false);
  const [noteSaving, setNoteSaving] = useState(false);
  const [noteError, setNoteError] = useState('');
  const [noteFeedback, setNoteFeedback] = useState('');
  const [noteConflict, setNoteConflict] = useState<{ revision: string; message: string } | null>(null);
  const [annotations, setAnnotations] = useState<Annotation[]>([]);
  const [annotationLoading, setAnnotationLoading] = useState(false);
  const [annotationError, setAnnotationError] = useState('');
  const [buildArrow, setBuildArrow] = useState(true);
  const hasVault = Boolean(vaultPath && vaultSummary);
  const editingLiteratureItem = editingLiteratureId ? literatureItems.find(item => item.itemId === editingLiteratureId) : undefined;
  const assetsForItem = (itemId: string) => documentAssets.filter(asset => asset.itemId === itemId);
  const activeWorkspace = restored ? workspace : 'vault';

  const isOa = oaFilter === 'all' ? null : oaFilter === 'oa';
  const chart = useMemo(() => ({
    xAxis: { type: 'category', data: ranking.slice(0, 10).map((r) => String(r.display_name ?? r.openalex_id ?? '—')) },
    yAxis: { type: 'value' },
    series: [{ type: 'bar', data: ranking.slice(0, 10).map((r) => Number(r.metric_value ?? r[appliedQuery.metric] ?? 0)) }],
    grid: { left: 40, right: 20, top: 20, bottom: 90 },
  }), [ranking, appliedQuery.metric]);

  const loadAdapters = async () => { try { setSourceAdapters(rows(await invoke('source_adapters')).map(v => ({ name: String(v.name ?? 'Unknown'), kind: String(v.kind ?? 'source-import'), is_default: Boolean(v.is_default) }))); } catch { /* keep fallback */ } };

  useEffect(() => { void loadAdapters(); }, []);
  useEffect(() => { localStorage.setItem('workspace', workspace); }, [workspace]);
  useEffect(() => { localStorage.setItem('sourceTab', sourceTab); }, [sourceTab]);
  useEffect(() => { vaultPathRef.current = vaultPath; }, [vaultPath]);

  const captureVaultRequest = (): VaultRequestContext => ({
    expectedGeneration: vaultConnectionGenerationRef.current,
    expectedVaultPath: vaultPathRef.current,
  });
  const isCurrentVaultRequest = (request: VaultRequestContext) =>
    vaultConnectionGenerationRef.current === request.expectedGeneration
    && vaultPathRef.current === request.expectedVaultPath;

  useEffect(() => {
    if (!restored) return;
    const request = captureVaultRequest();
    if (activeWorkspace === 'reading' && hasVault) {
      void Promise.all([loadPersonalLibrary(request), loadReadingSessions(request)]);
      return;
    }
    if (activeWorkspace === 'notes' && hasVault) {
      void loadNotes(request);
      return;
    }
    if (activeWorkspace === 'vault' && hasVault) void loadPersonalLibrary(request);
    if (activeWorkspace === 'search' && hasVault) void refreshLocalSearchHealth(request);
    if ((activeWorkspace === 'reading' || activeWorkspace === 'vault' || activeWorkspace === 'search' || activeWorkspace === 'notes') && !hasVault && isCurrentVaultRequest(request)) {
      setLiteratureItems([]);
      setDocumentAssets([]);
      setReadingSessions([]);
      setNotes([]);
      setSelectedNoteId(null);
      setAnnotations([]);
    }
  }, [workspace, restored, hasVault, vaultPath]);

  useEffect(() => {
    let cancelled = false;
    const restoreRecentVault = async () => {
      setStatus(t.restoring);
      const candidates = loadRecent();
      for (const path of candidates) {
        const generation = beginVaultConnection();
        try {
          const ctx = await invoke('connect_vault', { path, generation }) as VaultContext;
          if (cancelled || !isCurrentVaultConnection(generation)) return;
          const nextPath = applyVaultContext(ctx, path, generation);
          if (!nextPath) return;
          const request = { expectedGeneration: generation, expectedVaultPath: nextPath };
          await loadPersonalLibrary(request);
          if (cancelled || !isCurrentVaultRequest(request)) return;
          setRecentVaults(remember(path));
          setVaultError('');
          setStatus(`${t.connected}: ${path}`);
          await refresh(undefined, true, request);
          if (!cancelled && isCurrentVaultRequest(request)) setRestored(true);
          return;
        } catch {
          if (cancelled || !isCurrentVaultConnection(generation)) return;
          // Continue through the recent list; a moved or disconnected vault must not block startup.
        }
      }
      if (!cancelled) {
        clearVaultContext();
        setWorkspace('vault');
        setStatus(t.ready);
        setRestored(true);
      }
    };
    void restoreRecentVault();
    return () => { cancelled = true; };
  }, []);

  useEffect(() => {
    if (restored && (activeWorkspace === 'source' || activeWorkspace === 'settings') && hasVault) void refreshVaultContext();
  }, [workspace, restored, hasVault]);

  const run = async (fn: () => Promise<unknown>) => {
    setBusy(true);
    try { await fn(); } catch (e: any) { setStatus(`${t.failed}: ${e?.message ?? e}`); } finally { setBusy(false); }
  };

  const snapshotQuery = (): QuerySnapshot => ({
    sourceType,
    metric,
    text,
    country,
    oaFilter,
  });

  const currentQuery = snapshotQuery();
  const currentQueryText = queryLabel(appliedQuery, lang);
  const queryDirty = JSON.stringify(currentQuery) !== JSON.stringify(appliedQuery);
  const resultCount = results.length;
  const rankingCount = ranking.length;

  const resetQuery = async () => {
    setSourceType('journal');
    setMetric('h_index');
    setText('');
    setCountry('');
    setOaFilter('all');
    await refresh({ sourceType: 'journal', metric: 'h_index', text: '', country: '', oaFilter: 'all' });
  };

  const refresh = async (
    snapshot?: QuerySnapshot,
    vaultReady = hasVault,
    request: VaultRequestContext = captureVaultRequest(),
  ) => {
    if (!vaultReady || !request.expectedVaultPath) {
      if (!isCurrentVaultRequest(request)) return false;
      setOverview(null);
      setRanking([]);
      setResults([]);
      setAnalysisError('');
      setStatus(t.connectForAnalysis);
      return false;
    }
    const current = snapshot ?? snapshotQuery();
    try {
      const [nextOverview, nextRanking, nextResults] = await Promise.all([
        invoke('vault_overview'),
        invoke('top_sources', { metric: current.metric, sourceType: current.sourceType, limit: 30 }),
        invoke('search_sources', { req: { text: current.text || null, sourceType: current.sourceType, countryCode: current.country || null, isOa: current.oaFilter === 'all' ? null : current.oaFilter === 'oa', limit: 100, offset: 0 } }),
      ]);
      // A source-analysis response belongs to the Vault generation that started it.
      if (!isCurrentVaultRequest(request)) return false;
      setOverview(first(nextOverview));
      setRanking(rows(nextRanking));
      setResults(rows(nextResults));
      setAppliedQuery(current);
      setRecentQueries(rememberQuery(current));
      setAnalysisError('');
      return true;
    } catch (e: any) {
      if (!isCurrentVaultRequest(request)) return false;
      const message = String(e?.message ?? e);
      setOverview(null);
      setRanking([]);
      setResults([]);
      setAnalysisError(message);
      setStatus(`${t.failed}: ${message}`);
      return false;
    }
  };

  const clearSearchContext = () => {
    setLocalSearchOutcome(null);
    setLocalSearchIndexState(null);
    setLocalSearchTask(null);
    setLocalSearchIssues(null);
    setLocalSearchOffset(0);
    setLocalSearchLoading(false);
    setLocalSearchError('');
  };

  const clearVaultContext = (invalidatePending = true) => {
    if (invalidatePending) vaultConnectionGenerationRef.current += 1;
    vaultPathRef.current = '';
    setVaultPath('');
    setVaultTableCount(null);
    setVaultSummary(null);
    setOverview(null);
    setAnalysisError('');
    setRanking([]);
    setResults([]);
    setLiteratureItems([]);
    setDocumentAssets([]);
    setReadingSessions([]);
    setLiteratureFeedback('');
    setReadingFeedback('');
    clearSearchContext();
    setNotes([]);
    setSelectedNoteId(null);
    setNoteEditorTitle('');
    setNoteEditorBody('');
    setNoteConflict(null);
    setNoteError('');
    setNoteFeedback('');
    setAnnotations([]);
    setAnnotationError('');
  };

  const beginVaultConnection = () => {
    const generation = vaultConnectionGenerationRef.current + 1;
    vaultConnectionGenerationRef.current = generation;
    // Isolate the UI before awaiting connect_vault: no old Vault data is current while connecting.
    clearVaultContext(false);
    return generation;
  };

  const isCurrentVaultConnection = (generation: number) => vaultConnectionGenerationRef.current === generation;

  const applyVaultContext = (ctx: VaultContext, fallbackPath = '', expectedGeneration = vaultConnectionGenerationRef.current) => {
    if (!isCurrentVaultConnection(expectedGeneration)) return '';
    const nextPath = typeof ctx?.root === 'string' ? ctx.root : fallbackPath;
    vaultPathRef.current = nextPath;
    setVaultPath(nextPath);
    setVaultTableCount(typeof ctx?.tableCount === 'number' ? ctx.tableCount : null);
    setVaultSummary(summarizeManifest(ctx?.manifest ?? {}));
    // The new Vault starts with an intentionally empty personal view until its own requests resolve.
    setLiteratureItems([]);
    setDocumentAssets([]);
    setReadingSessions([]);
    setLiteratureFeedback('');
    setReadingFeedback('');
    return nextPath;
  };

  const refreshVaultContext = async () => {
    const request = captureVaultRequest();
    if (!request.expectedVaultPath) return false;
    try {
      const ctx = await invoke('vault_context') as VaultContext;
      if (!isCurrentVaultRequest(request)) return false;
      const nextPath = applyVaultContext(ctx, '', request.expectedGeneration);
      if (!nextPath) return false;
      const refreshedRequest = { expectedGeneration: request.expectedGeneration, expectedVaultPath: nextPath };
      if (!isCurrentVaultRequest(refreshedRequest)) return false;
      setVaultError('');
      await Promise.all([refresh(undefined, true, refreshedRequest), loadPersonalLibrary(refreshedRequest)]);
      return isCurrentVaultRequest(refreshedRequest);
    } catch (e: any) {
      if (!isCurrentVaultRequest(request)) return false;
      const message = String(e?.message ?? e);
      clearVaultContext(false);
      setVaultError(message);
      setStatus(`${t.failed}: ${message}`);
      return false;
    }
  };

  const inspectSources = async (path: string) => {
    if (!path) { setStatus(t.chooseFirst); return; }
    try {
      const preview = await invoke('inspect_sources', { rawSourcesDir: path }) as ImportPreview;
      setImportPreview(preview);
      setRawDir(path);
      setStatus(`${t.inspectSources}: ${path}`);
    } catch (e: any) {
      setImportPreview(null);
      setStatus(`${t.failed}: ${e?.message ?? e}`);
    }
  };

  const connect = async (path: string) => {
    if (!path) { setStatus(t.chooseFirst); return false; }
    const generation = beginVaultConnection();
    setVaultError('');
    setStatus(t.connecting);
    try {
      const ctx = await invoke('connect_vault', { path, generation }) as VaultContext;
      if (!isCurrentVaultConnection(generation)) return false;
      const nextPath = applyVaultContext(ctx, path, generation);
      if (!nextPath) return false;
      const request = { expectedGeneration: generation, expectedVaultPath: nextPath };
      if (!isCurrentVaultRequest(request)) return false;
      setRecentVaults(remember(path));
      setStatus(`${t.connected}: ${path}`);
      setWorkspace('source');
      const [, refreshed] = await Promise.all([loadPersonalLibrary(request), refresh(undefined, true, request)]);
      if (!isCurrentVaultRequest(request)) return false;
      setRestored(true);
      return refreshed;
    } catch (e: any) {
      if (!isCurrentVaultConnection(generation)) return false;
      const message = String(e?.message ?? e);
      // Remain visibly disconnected after a failed switch; never restore old Vault state.
      clearVaultContext(false);
      setVaultError(message);
      setStatus(`${t.failed}: ${message}`);
      setRestored(true);
      return false;
    }
  };

  const performImport = async (request: ImportRequest) => {
    setLastImport(request);
    setImportError('');
    try {
      const ctx = await invoke('import_sources', { req: request }) as VaultContext;
      setVaultPath(typeof ctx?.root === 'string' ? ctx.root : request.outputDir);
      setVaultTableCount(typeof ctx?.tableCount === 'number' ? ctx.tableCount : null);
      const summary = summarizeManifest(ctx?.manifest ?? {});
      setVaultSummary(summary);
      setStatus(`${t.imported}: ${request.outputDir}${summary.vault_id ? ` · ${summary.vault_id}` : ''}`);
      const connected = await connect(request.outputDir);
      if (!connected) throw new Error(vaultError || t.failed);
    } catch (e: any) {
      const message = String(e?.message ?? e);
      setImportError(message);
      throw e;
    }
  };

  const importVault = () => run(async () => {
    if (!rawDir || !vaultPath) { setStatus(t.chooseFirst); return; }
    if (!importPreview) { await inspectSources(rawDir); return; }
    await performImport({ rawSourcesDir: rawDir, outputDir: vaultPath, buildArrowCache: buildArrow });
  });

  const retryImport = () => {
    if (!lastImport) return;
    void run(() => performImport(lastImport));
  };

  const loadLiteratureItems = async (request: VaultRequestContext) => {
    if (!request.expectedVaultPath) {
      if (isCurrentVaultRequest(request)) setLiteratureItems([]);
      return;
    }
    try {
      const value = await invoke('list_literature_items');
      if (isCurrentVaultRequest(request)) {
        setLiteratureItems(Array.isArray(value) ? value as LiteratureItem[] : []);
      }
    } catch (e: any) {
      if (isCurrentVaultRequest(request)) setStatus(`${t.failed}: ${e?.message ?? e}`);
    }
  };

  const loadDocumentAssets = async (request: VaultRequestContext) => {
    if (!request.expectedVaultPath) {
      if (isCurrentVaultRequest(request)) setDocumentAssets([]);
      return;
    }
    try {
      const value = await invoke('list_document_assets');
      if (isCurrentVaultRequest(request)) {
        setDocumentAssets(Array.isArray(value) ? value as DocumentAsset[] : []);
      }
    } catch (e: any) {
      if (isCurrentVaultRequest(request)) setStatus(`${t.failed}: ${e?.message ?? e}`);
    }
  };
  const loadPersonalLibrary = async (request: VaultRequestContext) => {
    await Promise.all([loadLiteratureItems(request), loadDocumentAssets(request)]);
  };
  const loadReadingSessions = async (request: VaultRequestContext) => {
    if (!request.expectedVaultPath) {
      if (isCurrentVaultRequest(request)) setReadingSessions([]);
      return;
    }
    try {
      const value = await invoke('list_recent_reading_sessions');
      if (isCurrentVaultRequest(request)) {
        setReadingSessions(Array.isArray(value) ? value as ReadingSessionSummary[] : []);
      }
    } catch (e: any) {
      // Keep the existing history visible if a refresh fails.
      if (isCurrentVaultRequest(request)) setReadingFeedback(`${t.failed}: ${e?.message ?? e}`);
    }
  };

  const inspectLiteratureImport = () => {
    if (literatureImportFormat === 'openalex_works') {
      // For OpenAlex works the UI shows its own search panel; focus is handled
      // by the inline controls below.
      return;
    }
    literatureFileInputRef.current?.click();
  };

  const pickOpenAlexWorksDir = async () => {
    const path = await dir();
    if (path) setOpenAlexWorksDir(path);
  };

  const searchOpenAlexWorks = async () => {
    if (!openAlexWorksDir || !openAlexQuery.trim()) return;
    setOpenAlexLoading(true);
    setOpenAlexError('');
    setOpenAlexCandidates([]);
    try {
      const records = await invoke('list_local_openalex_works', { rawSourcesDir: openAlexWorksDir, query: openAlexQuery.trim(), limit: 20 }) as OpenAlexWorkCandidate[];
      setOpenAlexCandidates(Array.isArray(records) ? records : []);
    } catch (e: any) {
      setOpenAlexError(String(e?.message ?? e));
    } finally {
      setOpenAlexLoading(false);
    }
  };

  const previewOpenAlexWork = async (candidate: OpenAlexWorkCandidate) => {
    setLiteratureImportError('');
    setLiteratureImportResult(null);
    setBusy(true);
    try {
      const preview = await invoke('preview_openalex_work', { record: candidate }) as LiteratureImportPreview;
      setLiteratureImportPreview(preview);
    } catch (e: any) {
      setLiteratureImportError(String(e?.message ?? e));
      setLiteratureImportPreview(null);
    } finally {
      setBusy(false);
    }
  };

  const handleLiteratureFileSelected = async (file: File) => {
    setLiteratureImportError('');
    setLiteratureImportResult(null);
    setBusy(true);
    try {
      const bytes = new Uint8Array(await file.arrayBuffer());
      const preview = await invoke('inspect_literature_import', { format: literatureImportFormat, bytes: Array.from(bytes) }) as LiteratureImportPreview;
      setLiteratureImportPreview(preview);
    } catch (e: any) {
      setLiteratureImportError(String(e?.message ?? e));
      setLiteratureImportPreview(null);
    } finally {
      setBusy(false);
    }
  };

  const commitLiteratureImport = async () => {
    if (!literatureImportPreview) return;
    setBusy(true);
    setLiteratureImportError('');
    try {
      const result = await invoke('import_literature_file', { format: literatureImportFormat, req: { preview: literatureImportPreview } }) as LiteratureImportResult;
      setLiteratureImportResult(result);
      setLiteratureImportPreview(null);
      const request = captureVaultRequest();
      await loadPersonalLibrary(request);
    } catch (e: any) {
      setLiteratureImportError(String(e?.message ?? e));
    } finally {
      setBusy(false);
    }
  };

  const updateImportPolicy = (recordId: string, policy: 'merge' | 'skip' | 'create') => {
    setLiteratureImportPreview(prev => {
      if (!prev) return prev;
      return {
        ...prev,
        items: prev.items.map(item => item.recordId === recordId ? { ...item, selectedPolicy: policy } : item),
      };
    });
  };

  const refreshLocalSearchHealth = async (request: VaultRequestContext = captureVaultRequest()): Promise<LocalSearchTask | null> => {
    if (!request.expectedVaultPath || !isCurrentVaultRequest(request)) return null;
    try {
      const [indexState, taskState] = await Promise.all([
        invoke('local_search_index_state') as Promise<LocalSearchIndexState>,
        invoke('local_search_task_state') as Promise<LocalSearchTask>,
      ]);
      if (!isCurrentVaultRequest(request)) return null;
      setLocalSearchIndexState(indexState);
      setLocalSearchTask(taskState);
      return taskState;
    } catch (e: any) {
      if (isCurrentVaultRequest(request)) setLocalSearchError(String(e?.message ?? e));
      return null;
    }
  };

  const runLocalSearch = async (offset = 0, request: VaultRequestContext = captureVaultRequest()) => {
    if (!request.expectedVaultPath || !isCurrentVaultRequest(request)) return;
    setLocalSearchLoading(true);
    setLocalSearchError('');
    try {
      const result = await invoke('local_search', { req: { text: localSearchText, scopes: [localSearchScope], offset, limit: 50 } }) as LocalSearchOutcome;
      if (!isCurrentVaultRequest(request)) return;
      setLocalSearchOutcome(result);
      setLocalSearchOffset(result.outcome === 'ready' ? result.page.offset : 0);
      setLocalSearchIndexState(result.outcome === 'ready' ? result.page.indexState : result.indexState);
    } catch (e: any) {
      if (isCurrentVaultRequest(request)) setLocalSearchError(String(e?.message ?? e));
    } finally {
      if (isCurrentVaultRequest(request)) setLocalSearchLoading(false);
    }
  };

  const loadLocalSearchIssues = async (request: VaultRequestContext = captureVaultRequest()) => {
    if (!request.expectedVaultPath || !isCurrentVaultRequest(request)) return;
    try {
      const issues = await invoke('local_search_index_issues') as LocalSearchIssues;
      if (isCurrentVaultRequest(request)) setLocalSearchIssues(issues);
    } catch (e: any) {
      if (isCurrentVaultRequest(request)) setLocalSearchError(String(e?.message ?? e));
    }
  };

  const runLocalSearchTask = async (command: 'synchronize_local_search_index' | 'rebuild_local_search_index') => {
    const request = captureVaultRequest();
    if (!request.expectedVaultPath || !isCurrentVaultRequest(request)) return;
    setLocalSearchError('');
    try {
      const task = await invoke(command, { req: request }) as LocalSearchTask;
      if (!isCurrentVaultRequest(request)) return;
      setLocalSearchTask(task);
      setLocalSearchIndexState({ status: 'building', activeGeneration: localSearchIndexState?.activeGeneration ?? null, detail: task.detail ?? null });
    } catch (e: any) {
      if (isCurrentVaultRequest(request)) setLocalSearchError(String(e?.message ?? e));
    }
  };

  const cancelLocalSearchTask = async () => {
    const request = captureVaultRequest();
    if (!request.expectedVaultPath || !isCurrentVaultRequest(request)) return;
    try {
      const task = await invoke('cancel_local_search_index_task', { req: request }) as LocalSearchTask;
      if (isCurrentVaultRequest(request)) setLocalSearchTask(task);
    } catch (e: any) {
      if (isCurrentVaultRequest(request)) setLocalSearchError(String(e?.message ?? e));
    }
  };

  const runNoteAction = async (action: (request: VaultRequestContext) => Promise<void>) => {
    const request = captureVaultRequest();
    if (!isCurrentVaultRequest(request)) return;
    setNoteSaving(true);
    setNoteError('');
    try {
      await action(request);
    } catch (e: any) {
      if (!isCurrentVaultRequest(request)) return;
      const conflict = parseNoteConflict(e?.message ?? e);
      if (conflict) {
        setNoteConflict({ revision: '', message: conflict.message });
      } else {
        setNoteError(String(e?.message ?? e));
      }
    } finally {
      if (isCurrentVaultRequest(request)) setNoteSaving(false);
    }
  };

  const loadNotes = async (request: VaultRequestContext) => {
    if (!request.expectedVaultPath || !isCurrentVaultRequest(request)) return;
    setNoteLoading(true);
    setNoteError('');
    try {
      const value = await invoke('list_notes', { itemId: noteFilterItemId === 'all' ? null : noteFilterItemId, includeArchived: true }) as Note[];
      if (!isCurrentVaultRequest(request)) return;
      setNotes(Array.isArray(value) ? value.sort((a, b) => new Date(b.updatedAt).getTime() - new Date(a.updatedAt).getTime()) : []);
    } catch (e: any) {
      if (isCurrentVaultRequest(request)) setNoteError(String(e?.message ?? e));
    } finally {
      if (isCurrentVaultRequest(request)) setNoteLoading(false);
    }
  };

  const loadAnnotations = async (request: VaultRequestContext, itemId: string) => {
    if (!request.expectedVaultPath || !isCurrentVaultRequest(request)) return;
    setAnnotationLoading(true);
    setAnnotationError('');
    try {
      const value = await invoke('list_annotations', { itemId }) as Annotation[];
      if (!isCurrentVaultRequest(request)) return;
      const list = Array.isArray(value) ? value : [];
      const resolved = await Promise.all(list.map(async (annotation) => {
        try {
          const resolution = await invoke('annotation_resolution', { annotationId: annotation.annotationId, req: request }) as AnnotationResolution;
          if (!isCurrentVaultRequest(request)) return annotation;
          return { ...annotation, resolution };
        } catch {
          return annotation;
        }
      }));
      if (isCurrentVaultRequest(request)) setAnnotations(resolved.sort((a, b) => new Date(b.updatedAt).getTime() - new Date(a.updatedAt).getTime()));
    } catch (e: any) {
      if (isCurrentVaultRequest(request)) setAnnotationError(String(e?.message ?? e));
    } finally {
      if (isCurrentVaultRequest(request)) setAnnotationLoading(false);
    }
  };

  const beginNewNote = () => {
    setSelectedNoteId(null);
    setNoteEditorTitle('');
    setNoteEditorBody('');
    setNoteConflict(null);
    setNoteError('');
  };

  const saveNote = () => runNoteAction(async (request) => {
    const title = noteEditorTitle.trim();
    if (!title) return;
    if (!selectedNoteId && noteFilterItemId === 'all') {
      setNoteError(t.chooseItemForNote);
      return;
    }
    const isConflict = Boolean(noteConflict);
    setNoteConflict(null);
    const body = noteEditorBody;
    if (selectedNoteId) {
      if (isConflict) {
        const refreshed = await invoke('get_note', { noteId: selectedNoteId }) as Note;
        if (!isCurrentVaultRequest(request)) return;
        const note = await invoke('update_note', { noteId: refreshed.noteId, expectedRevision: refreshed.revision, title, markdownBody: body, req: request }) as Note;
        if (!isCurrentVaultRequest(request)) return;
        setSelectedNoteId(note.noteId);
      } else {
        const current = notes.find(n => n.noteId === selectedNoteId);
        const note = await invoke('update_note', { noteId: selectedNoteId, expectedRevision: current?.revision ?? '', title, markdownBody: body, req: request }) as Note;
        if (!isCurrentVaultRequest(request)) return;
        setSelectedNoteId(note.noteId);
      }
    } else {
      const note = await invoke('create_note', { itemId: noteFilterItemId, title, markdownBody: body, req: request }) as Note;
      if (!isCurrentVaultRequest(request)) return;
      setSelectedNoteId(note.noteId);
    }
    setNoteFeedback(t.noteSaved);
    await loadNotes(request);
  });

  const refreshNote = () => runNoteAction(async (request) => {
    if (!selectedNoteId) return;
    const refreshed = await invoke('get_note', { noteId: selectedNoteId }) as Note;
    if (!isCurrentVaultRequest(request)) return;
    setNoteEditorTitle(refreshed.title);
    setNoteEditorBody(refreshed.markdownBody);
    setNoteConflict(null);
  });

  const overwriteNote = () => runNoteAction(async (request) => {
    if (!selectedNoteId) return;
    const refreshed = await invoke('get_note', { noteId: selectedNoteId }) as Note;
    if (!isCurrentVaultRequest(request)) return;
    const title = noteEditorTitle.trim();
    if (!title) return;
    const note = await invoke('update_note', { noteId: refreshed.noteId, expectedRevision: refreshed.revision, title, markdownBody: noteEditorBody, req: request }) as Note;
    if (!isCurrentVaultRequest(request)) return;
    setNoteConflict(null);
    setSelectedNoteId(note.noteId);
    setNoteFeedback(t.noteSaved);
    await loadNotes(request);
  });

  const archiveNote = (note: Note) => runNoteAction(async (request) => {
    await invoke('archive_note', { noteId: note.noteId, expectedRevision: note.revision, req: request });
    if (!isCurrentVaultRequest(request)) return;
    setNoteFeedback(t.archived);
    if (selectedNoteId === note.noteId) setSelectedNoteId(null);
    await loadNotes(request);
  });

  const restoreNote = (note: Note) => runNoteAction(async (request) => {
    await invoke('unarchive_note', { noteId: note.noteId, expectedRevision: note.revision, req: request });
    if (!isCurrentVaultRequest(request)) return;
    setNoteFeedback(t.unarchiveNote);
    await loadNotes(request);
  });

  const openAnnotationAsset = (annotation: Annotation) => runNoteAction(async (request) => {
    if (annotation.resolution !== 'resolved_exact') return;
    await invoke('open_annotation_asset', { annotationId: annotation.annotationId, req: request });
    if (!isCurrentVaultRequest(request)) return;
    setNoteFeedback(t.fileRequestAccepted);
  });

  useEffect(() => {
    if (!selectedNoteId) {
      setNoteEditorTitle('');
      setNoteEditorBody('');
      setNoteConflict(null);
      setAnnotations([]);
      return;
    }
    const note = notes.find(n => n.noteId === selectedNoteId);
    if (!note) return;
    setNoteEditorTitle(note.title);
    setNoteEditorBody(note.markdownBody);
    setNoteConflict(null);
    const request = captureVaultRequest();
    void loadAnnotations(request, note.itemId);
  }, [selectedNoteId, notes]);

  useEffect(() => {
    if (activeWorkspace !== 'notes' || !hasVault) return;
    const request = captureVaultRequest();
    void loadNotes(request);
  }, [activeWorkspace, hasVault, noteFilterItemId]);

  useEffect(() => {
    if (activeWorkspace !== 'search' || !hasVault || localSearchTask?.status !== 'building') return;
    const request = captureVaultRequest();
    return createLocalSearchTaskPoller({
      request,
      isCurrent: isCurrentVaultRequest,
      refresh: refreshLocalSearchHealth,
      onTerminal: async (terminalRequest) => {
        if (!isCurrentVaultRequest(terminalRequest)) return;
        // Refresh again after the terminal task state so index health is not a
        // stale sibling response from the last building poll; issues share the
        // same connection-generation + Vault-path gate.
        await refreshLocalSearchHealth(terminalRequest);
        await loadLocalSearchIssues(terminalRequest);
      },
    });
  }, [activeWorkspace, hasVault, localSearchTask?.status, vaultPath]);

  const beginNewLiterature = () => { setEditingLiteratureId(null); setLiteratureDraft(emptyLiteratureDraft()); setLiteratureEditorOpen(true); };
  const beginEditLiterature = (item: LiteratureItem) => {
    setEditingLiteratureId(item.itemId);
    setLiteratureDraft({ title: item.title, authors: item.authors, publishedYear: item.publishedYear, itemType: item.itemType, favorite: item.favorite, readingStatus: item.readingStatus, tags: item.tags, externalIdentifiers: item.externalIdentifiers }); setLiteratureEditorOpen(true);
  };
  const literatureRequest = () => ({ ...literatureDraft, authors: literatureDraft.authors.filter(Boolean), tags: literatureDraft.tags.filter(Boolean) });
  const saveLiterature = () => run(async () => {
    const request = captureVaultRequest();
    if (!hasVault || !literatureDraft.title.trim() || !isCurrentVaultRequest(request)) return;
    if (editingLiteratureId) await invoke('update_literature_item', { itemId: editingLiteratureId, req: literatureRequest() });
    else await invoke('create_literature_item', { req: literatureRequest() });
    await loadPersonalLibrary(request);
    if (!isCurrentVaultRequest(request)) return;
    setEditingLiteratureId(null); setLiteratureEditorOpen(false); setLiteratureDraft(emptyLiteratureDraft());
  });
  const deleteLiterature = (itemId: string) => run(async () => {
    if (!window.confirm(lang === 'zh' ? '确定删除这个条目吗？' : 'Delete this item?')) return;
    const request = captureVaultRequest();
    if (!isCurrentVaultRequest(request)) return;
    await invoke('delete_literature_item', { itemId });
    await loadPersonalLibrary(request);
  });
  const runLiteratureAction = async (action: (request: VaultRequestContext) => Promise<void>) => {
    const request = captureVaultRequest();
    if (!isCurrentVaultRequest(request)) return;
    setBusy(true);
    setLiteratureFeedback('');
    try {
      await action(request);
    } catch (e: any) {
      if (isCurrentVaultRequest(request)) setLiteratureFeedback(`${t.fileOperationFailed}: ${e?.message ?? e}`);
    } finally {
      setBusy(false);
    }
  };
  const importDocumentAsset = (item: LiteratureItem, assetKind: DocumentAssetKind = 'primary') => void runLiteratureAction(async (request) => {
    const sourcePath = await pdfFile();
    if (!sourcePath || !isCurrentVaultRequest(request)) return;
    const result = await invoke<{ outcome: string }>('import_document_asset', { itemId: item.itemId, sourcePath, assetKind });
    await loadDocumentAssets(request);
    if (isCurrentVaultRequest(request)) setLiteratureFeedback(result.outcome === 'duplicate' ? t.assetDuplicate : t.imported);
  });
  const linkExternalDocumentAsset = (item: LiteratureItem, assetKind: DocumentAssetKind = 'primary') => void runLiteratureAction(async (request) => {
    const sourcePath = await pdfFile();
    if (!sourcePath || !isCurrentVaultRequest(request)) return;
    await invoke('link_external_document_asset', { itemId: item.itemId, sourcePath, assetKind });
    await loadDocumentAssets(request);
    if (isCurrentVaultRequest(request)) setLiteratureFeedback(t.assetLinked);
  });
  const openDocumentAsset = (asset: DocumentAsset) => void runLiteratureAction(async (request) => {
    await invoke('open_document_asset', { itemId: asset.itemId, assetId: asset.assetId });
    if (isCurrentVaultRequest(request)) setLiteratureFeedback(t.fileRequestAccepted);
  });
  const openSearchHitAsset = (itemId: string, assetId: string) => void runLiteratureAction(async (request) => {
    await invoke('open_document_asset', { itemId, assetId });
    if (isCurrentVaultRequest(request)) setLocalSearchError('');
  });
  const setDocumentAssetKind = (asset: DocumentAsset, assetKind: DocumentAssetKind) => void runLiteratureAction(async (request) => {
    await invoke('set_document_asset_kind', { itemId: asset.itemId, assetId: asset.assetId, assetKind });
    await loadDocumentAssets(request);
  });
  const setDocumentAssetDefault = (asset: DocumentAsset) => void runLiteratureAction(async (request) => {
    await invoke('set_document_asset_default', { itemId: asset.itemId, assetId: asset.assetId });
    await loadDocumentAssets(request);
  });
  const migrateExternalDocumentAsset = (asset: DocumentAsset) => void runLiteratureAction(async (request) => {
    const result = await invoke<{ outcome: string }>('migrate_external_document_asset', { itemId: asset.itemId, assetId: asset.assetId });
    await loadDocumentAssets(request);
    if (isCurrentVaultRequest(request)) setLiteratureFeedback(result.outcome === 'duplicate' ? t.assetDuplicate : t.assetMigrated);
  });
  const removeDocumentAsset = (asset: DocumentAsset) => void runLiteratureAction(async (request) => {
    if (!window.confirm(lang === 'zh' ? `移除“${asset.displayName}”的资产记录？不会删除磁盘文件。` : `Remove the asset record for “${asset.displayName}”? The disk file will not be deleted.`)) return;
    if (!isCurrentVaultRequest(request)) return;
    await invoke('remove_document_asset', { itemId: asset.itemId, assetId: asset.assetId });
    await loadDocumentAssets(request);
  });
  const runReadingAction = async (action: (request: VaultRequestContext) => Promise<void>) => {
    const request = captureVaultRequest();
    if (!isCurrentVaultRequest(request)) return;
    setBusy(true);
    setReadingFeedback('');
    try {
      await action(request);
    } catch (e: any) {
      // Never clear local session history merely because an open or persistence request fails.
      if (isCurrentVaultRequest(request)) setReadingFeedback(`${t.failed}: ${e?.message ?? e}`);
    } finally {
      setBusy(false);
    }
  };
  const startReading = (asset: DocumentAsset) => void runReadingAction(async (request) => {
    await invoke('start_reading_session', { itemId: asset.itemId, assetId: asset.assetId });
    await loadReadingSessions(request);
    if (isCurrentVaultRequest(request)) setReadingFeedback(t.readingSessionOpened);
  });
  const resumeReading = (session: ReadingSession) => void runReadingAction(async (request) => {
    await invoke('resume_reading_session', { itemId: session.itemId, assetId: session.assetId });
    await loadReadingSessions(request);
    if (isCurrentVaultRequest(request)) setReadingFeedback(t.readingSessionOpened);
  });
  const pauseReading = (session: ReadingSession) => void runReadingAction(async (request) => {
    await invoke('pause_reading_session', { itemId: session.itemId, assetId: session.assetId });
    await loadReadingSessions(request);
    if (isCurrentVaultRequest(request)) setReadingFeedback(t.readingSessionUpdated);
  });
  const endReading = (session: ReadingSession) => void runReadingAction(async (request) => {
    await invoke('end_reading_session', { itemId: session.itemId, assetId: session.assetId });
    await loadReadingSessions(request);
    if (isCurrentVaultRequest(request)) setReadingFeedback(t.readingSessionUpdated);
  });
  const continueReading = () => void runReadingAction(async (request) => {
    await invoke('continue_reading_session');
    await loadReadingSessions(request);
    if (isCurrentVaultRequest(request)) setReadingFeedback(t.readingSessionOpened);
  });
  const toggleFavorite = (item: LiteratureItem) => run(async () => {
    const request = captureVaultRequest();
    if (!isCurrentVaultRequest(request)) return;
    await invoke('set_literature_item_favorite', { itemId: item.itemId, favorite: !item.favorite });
    await loadLiteratureItems(request);
  });

  const exportRows = (kind: 'top' | 'search') => run(async () => {
    if (!hasVault || !overview || analysisError) { setStatus(analysisError ? `${t.failed}: ${analysisError}` : t.connectForAnalysis); return; }
    const p = await save({ defaultPath: kind === 'top' ? 'cistella-ranking.csv' : 'cistella-results.csv', filters: [{ name: 'CSV / Excel', extensions: ['csv', 'xlsx'] }] });
    if (!p) return;
    if (kind === 'top') await invoke('export_top_sources', { output: p, metric: appliedQuery.metric, sourceType: appliedQuery.sourceType, limit: ranking.length });
    else await invoke('export_search_sources', { output: p, req: { text: appliedQuery.text || null, sourceType: appliedQuery.sourceType, countryCode: appliedQuery.country || null, isOa: appliedQuery.oaFilter === 'all' ? null : appliedQuery.oaFilter === 'oa', limit: results.length, offset: 0 } });
    setStatus(`${kind === 'top' ? t.exportRank : t.exportSearch}: ${p}`);
  });

  return <div className="app">
    <aside className="sidebar">
      <div>
        <div className="brand">{t.brand}</div>
        <div className="sub">{t.sub}</div>
      </div>
      <nav className="nav">
        {([
          ['vault', t.vault],
          ['reading', t.reading],
          ['notes', t.notes],
          ['search', lang === 'zh' ? '搜索' : 'Search'],
          ['source', t.source],
          ['settings', t.settings],
        ] as const).map(([k, label]) => <button key={k} className={activeWorkspace === k ? 'selected' : ''} onClick={() => setWorkspace(k)}>{label}</button>)}
      </nav>
      <div className="footer">{status}</div>
    </aside>

    <main className="main">
      {activeWorkspace === 'vault' && <section className="page">
        <div className="hero card span3">
          <h1>{t.vaultTitle}</h1>
          <p>{t.vaultDesc}</p>
          <div className="actions">
            <button className="secondary" onClick={async () => { const v = await dir(); if (v) setVaultPath(v); }} disabled={busy}>{t.chooseVault}</button>
            <button className="secondary" onClick={async () => { const v = await dir(); if (v) { setRawDir(v); await inspectSources(v); } }} disabled={busy}>{t.chooseDir}</button>
            <button onClick={() => importVault()} disabled={busy || !vaultPath || !rawDir || !importPreview}>{t.importFromOpenAlex}</button>
          </div>
        </div>
        {hasVault && <div className="card span3 assetCenter">
          <Head title={t.assetCenter} action={<span>{documentAssets.length} PDF</span>} />
          <p>{t.assetsInVault}</p>{literatureFeedback && <p className="literatureFeedback">{literatureFeedback}</p>}
          {literatureItems.length ? literatureItems.map(item => <div className="vaultAssetGroup" key={item.itemId}>
            <div className="cardHead"><div><strong>{item.title || '—'}</strong><small>{item.authors.join(', ') || '—'}</small></div><div className="fileActions"><button className="secondary" onClick={() => importDocumentAsset(item)} disabled={busy}>{t.importAsset}</button><button className="secondary" onClick={() => linkExternalDocumentAsset(item)} disabled={busy}>{t.linkExternalAsset}</button></div></div>
            <AssetList assets={assetsForItem(item.itemId)} t={t} busy={busy} onOpen={openDocumentAsset} onSetKind={setDocumentAssetKind} onSetDefault={setDocumentAssetDefault} onMigrate={migrateExternalDocumentAsset} onRemove={removeDocumentAsset} />
          </div>) : <p>{t.noLiterature}</p>}
        </div>}
        <div className="card span2 compact">
          <h2>{t.currentVault}</h2>
          <div className="sourceRow"><div className="path">{short(vaultPath)}</div><button className="light" onClick={() => void run(() => connect(vaultPath))} disabled={busy || !vaultPath}>{t.openVault}</button></div>
          <small>{vaultSummary?.vault_id ? `vault_id: ${vaultSummary.vault_id}` : t.noData}</small>
        </div>
        <div className="card span2 compact">
          <h2>{t.recentVaults}</h2>
          <div className="recent">{recentVaults.length ? recentVaults.map((r) => <button key={r} onClick={() => void run(() => connect(r))}>{short(r)}</button>) : <small>{t.noData}</small>}</div>
        </div>
        <div className="card span2">
          <h2>{t.layout}</h2>
          <p>{t.parquet}</p>
          <p>{t.arrow}</p>
          <p>{t.manifest}</p>
        </div>
        <div className="card span2">
          <h2>{t.buildVault}</h2>
          <Path label={t.raw} value={rawDir} button={t.chooseDir} onPick={async () => { const v = await dir(); if (v) setRawDir(v); }} disabled={busy} />
          <Path label={t.output} value={vaultPath} button={t.chooseVault} onPick={async () => { const v = await dir(); if (v) setVaultPath(v); }} disabled={busy} />
          <div className="actions"><button className="secondary" onClick={() => void inspectSources(rawDir)} disabled={busy || !rawDir}>{t.inspectSources}</button><button onClick={() => importVault()} disabled={busy || !vaultPath || !rawDir || !importPreview}>{t.importFromOpenAlex}</button></div>
          <label className="field"><span>{t.buildCache}</span><input type="checkbox" checked={buildArrow} onChange={(e) => setBuildArrow(e.target.checked)} /></label>
        </div>
        <div className="card span2"><h2>{t.importPreview}</h2>{importPreview ? <><p>{lang === 'zh' ? `分区数 ${importPreview.partitionCount ?? 0}` : `${importPreview.partitionCount ?? 0} partitions`}</p><p>{lang === 'zh' ? `清单 ${importPreview.hasManifest ? '存在' : '缺失'}` : `Manifest ${importPreview.hasManifest ? 'present' : 'missing'}`}</p><p>{lang === 'zh' ? `快照 ${importPreview.snapshotDate ?? '未知'}` : `Snapshot ${importPreview.snapshotDate ?? 'unknown'}`}</p></> : <p>{lang === 'zh' ? '请选择来源目录并执行预检。' : 'Choose a source folder and run inspection.'}</p>}</div>
        {importError && <div className="card span2"><h2>{t.importFailed}</h2><p>{importError}</p><button className="secondary" onClick={retryImport} disabled={busy || !lastImport}>{t.retryImport}</button></div>}
        <div className="card span3">
          <h2>{t.workflow}</h2>
          <ul><li>{t.rawTip}</li><li>{t.outTip}</li><li>{t.parquet}</li><li>{t.arrow}</li></ul>
        </div>
      </section>}

      {activeWorkspace === 'reading' && <section className="page readingPage">
        <div className="card span3 hero">
          <h1>{t.readingTitle}</h1><p>{t.readingDesc}</p>
          {!hasVault ? <><p>{t.noVaultReading}</p><button onClick={async () => { const path = await dir(); if (path) await run(() => connect(path)); }} disabled={busy}>{t.connect}</button></> : <p>{vaultSummary?.vault_id ?? vaultPath}</p>}
        </div>
        {hasVault && <div className="card span3 readingSessions">
          <Head title={t.recentReading} action={<button onClick={continueReading} disabled={busy || !readingSessions.length}>{t.continueReading}</button>} />
          <p>{t.recentReadingDesc}</p>
          {readingFeedback && <p className="literatureFeedback">{readingFeedback}</p>}
          {readingSessions.length ? <div className="readingSessionList">{readingSessions.map(summary => {
            const session = summary.session;
            const item = literatureItems.find(entry => entry.itemId === session.itemId);
            const asset = documentAssets.find(entry => entry.assetId === session.assetId && entry.itemId === session.itemId);
            const isAvailable = summary.assetStatus === 'available';
            return <article className={`readingSession ${isAvailable ? '' : 'unavailable'}`} key={session.sessionId}>
              <div className="readingSessionMeta"><strong>{item?.title || '—'}</strong><small>{asset?.displayName || session.assetId}</small>
                <small>{t.readingSessionState}: {t.readingSessionStates[session.state]} · {t.assetHealth}: {t.assetStatuses[summary.assetStatus]}</small>
                <small>{t.lastOpenedAt}: {formatSessionTime(session.lastOpenedAt, lang)} · {t.startedAt}: {formatSessionTime(session.startedAt, lang)}</small>
                {!isAvailable && <small className="sessionWarning">{t.sessionUnavailable}</small>}
              </div>
              <div className="fileActions">
                {isAvailable && <button className="secondary" onClick={() => resumeReading(session)} disabled={busy}>{t.resumeReading}</button>}
                {session.state === 'active' && <button className="secondary" onClick={() => pauseReading(session)} disabled={busy}>{t.pauseReading}</button>}
                {session.state !== 'closed' && <button className="secondary danger" onClick={() => endReading(session)} disabled={busy}>{t.endReading}</button>}
              </div>
            </article>;
          })}</div> : <small>{t.noRecentReading}</small>}
        </div>}
        {hasVault && <div className="card span3 literatureToolbar">
          <input placeholder={t.literatureKeyword} value={literatureKeyword} onChange={e => setLiteratureKeyword(e.target.value)} />
          <span>{literatureItems.length} {t.literatureCount}</span>
          <input ref={literatureFileInputRef} type="file" accept=".bib,.ris" style={{ display: 'none' }} onChange={e => { const file = e.target.files?.[0]; if (file) void handleLiteratureFileSelected(file); e.target.value = ''; }} />
          <Sel label={t.importLiteratureFormat} value={literatureImportFormat} set={(v) => setLiteratureImportFormat(v as LiteratureImportFormat)} opts={[['bibtex', 'BibTeX'], ['ris', 'RIS'], ['openalex_works', t.openAlexWorksFormat]]} />
          {literatureImportFormat === 'openalex_works' ? <>
            <button className="secondary" onClick={() => void pickOpenAlexWorksDir()} disabled={busy}>{t.chooseDir}</button>
            <small title={openAlexWorksDir}>{short(openAlexWorksDir)}</small>
            <input placeholder={t.openAlexWorksQuery} value={openAlexQuery} onChange={e => setOpenAlexQuery(e.target.value)} onKeyDown={e => { if (e.key === 'Enter') void searchOpenAlexWorks(); }} />
            <button className="secondary" onClick={() => void searchOpenAlexWorks()} disabled={busy || openAlexLoading || !openAlexWorksDir || !openAlexQuery.trim()}>{t.searchOpenAlexWorks}</button>
          </> : <button className="secondary" onClick={inspectLiteratureImport} disabled={busy}>{t.importLiterature}</button>}
          <button onClick={beginNewLiterature} disabled={busy}>{t.addLiterature}</button>
        </div>}
        {hasVault && literatureImportFormat === 'openalex_works' && (openAlexLoading || openAlexError || openAlexCandidates.length > 0) && <div className="card span3">
          <h2>{t.openAlexWorksTitle}</h2>
          {openAlexLoading && <p>{lang === 'zh' ? '搜索中…' : 'Searching…'}</p>}
          {openAlexError && <p className="literatureFeedback">{openAlexError}</p>}
          {openAlexCandidates.length > 0 && <div className="importPreviewList">{openAlexCandidates.map(candidate => {
            const ids = candidate.externalIdentifiers.map(id => `${id.namespace}: ${id.value}`).join(' · ');
            return <article key={candidate.recordId} className="importPreviewItem">
              <div>
                <strong>{candidate.title || '—'}</strong>
                <small>{candidate.authors.join(', ') || '—'}{candidate.publishedYear ? ` · ${candidate.publishedYear}` : ''}</small>
                {ids && <small>{ids}</small>}
              </div>
              <button className="secondary" onClick={() => void previewOpenAlexWork(candidate)} disabled={busy}>{t.importPreview}</button>
            </article>;
          })}</div>}
        </div>}
        {hasVault && literatureImportPreview && <div className="card span3 literatureImportPreview">
          <h2>{t.importLiteratureTitle}</h2>
          <p>{t.importLiteratureDesc}</p>
          <p>{t.importPreviewTitle}: {literatureImportPreview.items.length}</p>
          <div className="importPreviewList">{literatureImportPreview.items.map(item => {
            const matched = item.matchedItemId != null;
            const ids = item.sourceRecord.externalIdentifiers.map(id => `${id.namespace}: ${id.value}`).join(' · ');
            return <article key={item.recordId} className={`importPreviewItem ${matched ? 'matched' : ''}`}>
              <div>
                <strong>{item.sourceRecord.title || '—'}</strong>
                <small>{item.sourceRecord.authors.join(', ') || '—'}{item.sourceRecord.publishedYear ? ` · ${item.sourceRecord.publishedYear}` : ''}</small>
                {ids && <small>{ids}</small>}
                <small className="importConflict">{matched ? t.importConflictMatched : t.importConflictNew}</small>
              </div>
              <select value={item.selectedPolicy} onChange={e => updateImportPolicy(item.recordId, e.target.value as 'merge' | 'skip' | 'create')} disabled={busy}>
                <option value="merge">{t.importPolicyMerge}</option>
                <option value="skip">{t.importPolicySkip}</option>
                <option value="create">{t.importPolicyCreate}</option>
              </select>
            </article>;
          })}</div>
          <div className="actions">
            <button onClick={commitLiteratureImport} disabled={busy}>{t.importCommit}</button>
            <button className="secondary" onClick={() => setLiteratureImportPreview(null)} disabled={busy}>{t.cancel}</button>
          </div>
        </div>}
        {hasVault && literatureImportResult && <div className="card span3 literatureImportResult">
          <h2>{t.importResultTitle}</h2>
          <p>{lang === 'zh' ? `新建 ${literatureImportResult.created} · 合并 ${literatureImportResult.merged} · 跳过 ${literatureImportResult.skipped} · 错误 ${literatureImportResult.errors}` : `Created ${literatureImportResult.created} · Merged ${literatureImportResult.merged} · Skipped ${literatureImportResult.skipped} · Errors ${literatureImportResult.errors}`}</p>
          <button className="secondary" onClick={() => setLiteratureImportResult(null)}>{t.cancel}</button>
        </div>}
        {hasVault && literatureImportError && <div className="card span3"><h2>{t.failed}</h2><p>{literatureImportError}</p></div>}
        {hasVault && literatureEditorOpen ? <div className="card span3 literatureEditor">
          <h2>{editingLiteratureId ? t.editLiterature : t.addLiterature}</h2>
          <label className="field"><span>{t.title}</span><input value={literatureDraft.title} onChange={e => setLiteratureDraft(d => ({ ...d, title: e.target.value }))} /></label>
          <label className="field"><span>{t.authors}</span><textarea value={literatureDraft.authors.join('\n')} onChange={e => setLiteratureDraft(d => ({ ...d, authors: e.target.value.split('\n') }))} /></label>
          <div className="literatureFields"><label className="field"><span>{t.publishedYear}</span><input type="number" value={literatureDraft.publishedYear ?? ''} onChange={e => setLiteratureDraft(d => ({ ...d, publishedYear: e.target.value ? Number(e.target.value) : null }))} /></label>
          <Sel label={t.itemType} value={literatureDraft.itemType} set={(v) => setLiteratureDraft(d => ({ ...d, itemType: v }))} opts={[[ 'article', 'article' ], [ 'book', 'book' ], [ 'chapter', 'chapter' ], [ 'other', 'other' ]]} />
          <small>{t.readingStatus}: {t[literatureDraft.readingStatus]}</small></div>
          <label className="field"><span>{t.tags}</span><input value={literatureDraft.tags.join(', ')} onChange={e => setLiteratureDraft(d => ({ ...d, tags: e.target.value.split(',').map(x => x.trim()).filter(Boolean) }))} /></label>
          <label className="toggle"><input type="checkbox" checked={literatureDraft.favorite} onChange={e => setLiteratureDraft(d => ({ ...d, favorite: e.target.checked }))} />{t.favorite}</label>
          <div className="editorFiles"><h3>{t.readingAssets}</h3><small>{editingLiteratureItem ? t.manageAssetsInVault : t.saveBeforeFiles}</small></div>
          <div className="actions"><button onClick={saveLiterature} disabled={busy || !literatureDraft.title.trim()}>{t.saveLiterature}</button><button className="secondary" onClick={() => { setEditingLiteratureId(null); setLiteratureEditorOpen(false); setLiteratureDraft(emptyLiteratureDraft()); }}>{t.cancel}</button></div>
        </div> : null}
        {hasVault && <div className="card span3 literatureList">
          <h2>{t.literatureCount}</h2>{literatureFeedback && <p className="literatureFeedback">{literatureFeedback}</p>}{literatureItems.filter(item => { const q = literatureKeyword.trim().toLowerCase(); return !q || [item.title, ...item.authors, ...item.tags].join(' ').toLowerCase().includes(q); }).map(item => <article className="literatureItem" key={item.itemId}>
            <div className="literatureItemDetail"><h3>{item.title || '—'}</h3><p>{item.authors.join(', ') || '—'}{item.publishedYear ? ` · ${item.publishedYear}` : ''}</p><small>{item.itemType} · {item.tags.join(', ') || '—'}</small>
              <ReadingAssetList assets={assetsForItem(item.itemId)} t={t} busy={busy} onStart={startReading} />
            </div>
            <div className="itemActions"><button className="secondary" onClick={() => toggleFavorite(item)} disabled={busy}>{item.favorite ? '★' : '☆'}</button><button className="secondary" onClick={() => beginEditLiterature(item)} disabled={busy}>{t.editLiterature}</button><button className="secondary danger" onClick={() => deleteLiterature(item.itemId)} disabled={busy}>{t.deleteLiterature}</button></div>
          </article>)}{literatureItems.length === 0 && <p>{t.noLiterature}</p>}</div>}
      </section>}

      {activeWorkspace === 'search' && <section className="page">
        <div className="card span3 hero">
          <h1>{lang === 'zh' ? 'Search 工作区' : 'Search Workspace'}</h1>
          <p>{lang === 'zh' ? '检索当前库的标题、作者、标签和已建立索引的 PDF 正文。结果只返回文献和资产身份。' : 'Search titles, authors, tags, and indexed PDF text in the current Vault. Results contain identities only.'}</p>
          {!hasVault && <p>{t.connectForAnalysis}</p>}
        </div>
        {hasVault && <>
          <div className="card span3 controls localSearchControls">
            <label className="field searchInput"><span>{lang === 'zh' ? '查询' : 'Query'}</span><input value={localSearchText} onChange={e => setLocalSearchText(e.target.value)} onKeyDown={e => { if (e.key === 'Enter') void runLocalSearch(); }} placeholder={lang === 'zh' ? '输入关键词，回车搜索' : 'Enter keywords and press Enter'} /></label>
            <Sel label={lang === 'zh' ? '范围' : 'Scope'} value={localSearchScope} set={setLocalSearchScope} opts={[[ 'all', lang === 'zh' ? '全部字段' : 'All fields' ], [ 'title', lang === 'zh' ? '标题' : 'Title' ], [ 'authors', lang === 'zh' ? '作者' : 'Authors' ], [ 'tags', lang === 'zh' ? '标签' : 'Tags' ], [ 'content', lang === 'zh' ? '正文' : 'Content' ]]} />
            <button onClick={() => void runLocalSearch()} disabled={localSearchLoading}>{localSearchLoading ? (lang === 'zh' ? '搜索中…' : 'Searching…') : (lang === 'zh' ? '搜索' : 'Search')}</button>
          </div>
          <div className="card span3 compact">
            <Head title={lang === 'zh' ? '索引健康' : 'Index health'} action={<span className={`searchStatus ${localSearchIndexState?.status ?? 'missing'}`}>{localSearchIndexState?.status ?? 'missing'}</span>} />
            <p>{localSearchIndexState?.detail || (lang === 'zh' ? '索引操作需要由你明确发起；连接 Vault 不会隐式重建。' : 'Index work is explicit; connecting a Vault never rebuilds implicitly.')}</p>
            <div className="actions"><button onClick={() => void runLocalSearchTask('synchronize_local_search_index')} disabled={localSearchTask?.status === 'building'}>{lang === 'zh' ? '同步' : 'Sync'}</button><button className="secondary" onClick={() => void runLocalSearchTask('rebuild_local_search_index')} disabled={localSearchTask?.status === 'building'}>{lang === 'zh' ? '重建' : 'Rebuild'}</button><button className="secondary" onClick={() => void cancelLocalSearchTask()} disabled={localSearchTask?.status !== 'building'}>{lang === 'zh' ? '取消' : 'Cancel'}</button><button className="secondary" onClick={() => void loadLocalSearchIssues()}>{lang === 'zh' ? '查看问题' : 'Issues'}</button></div>
            {localSearchTask && <small>{lang === 'zh' ? '任务' : 'Task'}: {localSearchTask.status}{localSearchTask.detail ? ` · ${localSearchTask.detail}` : ''}</small>}
          </div>
          {localSearchError && <div className="card span3"><h2>{lang === 'zh' ? '搜索错误' : 'Search error'}</h2><p>{localSearchError}</p></div>}
          {localSearchOutcome?.outcome === 'unavailable' && <div className="card span3"><h2>{lang === 'zh' ? '索引不可用' : 'Index unavailable'}</h2><p>{localSearchOutcome.indexState.status}{localSearchOutcome.indexState.detail ? ` · ${localSearchOutcome.indexState.detail}` : ''}</p><p>{lang === 'zh' ? '这不是“无命中”。请显式同步或重建索引。' : 'This is not an empty result. Explicitly sync or rebuild the index.'}</p></div>}
          {localSearchOutcome?.outcome === 'ready' && <div className="card span3 localSearchResults"><Head title={lang === 'zh' ? `结果 ${localSearchOutcome.page.totalHits}` : `${localSearchOutcome.page.totalHits} results`} action={<span>{lang === 'zh' ? '每页 50；元数据优先，item_id 稳定排序' : '50/page; metadata first, stable item_id order'}</span>} />
            <div className="actions searchPagination"><button className="secondary" onClick={() => void runLocalSearch(Math.max(0, localSearchOffset - 50))} disabled={localSearchOffset === 0 || localSearchLoading}>{lang === 'zh' ? '上一页' : 'Previous'}</button><small>{localSearchOffset + 1}–{Math.min(localSearchOffset + 50, localSearchOutcome.page.totalHits)} / {localSearchOutcome.page.totalHits}</small><button className="secondary" onClick={() => void runLocalSearch(localSearchOffset + 50)} disabled={localSearchLoading || localSearchOffset + 50 >= localSearchOutcome.page.totalHits}>{lang === 'zh' ? '下一页' : 'Next'}</button></div>
            {localSearchOutcome.page.hits.map(hit => { const item = literatureItems.find(x => x.itemId === hit.itemId); return <article className="literatureItem" key={hit.itemId}><div className="literatureItemDetail"><h3>{item?.title || hit.itemId}</h3><p>{item?.authors.join(', ') || '—'}</p>{hit.fieldMatches.map((match, index) => { const assetId = match.assetId; return <div className="searchMatch" key={`${match.field}-${assetId ?? index}`}><small>{match.field}{match.matchedTerms.length ? ` · ${match.matchedTerms.join(', ')}` : ''}{match.assetState ? ` · ${match.assetState}` : ''}</small>{match.excerpt && <p>{match.excerpt}</p>}{assetId && match.assetState === 'indexed' && <button className="secondary" onClick={() => openSearchHitAsset(hit.itemId, assetId)}>{lang === 'zh' ? '受控打开命中资产' : 'Open matched asset'}</button>}</div>; })}</div></article>; })}{localSearchOutcome.page.hits.length === 0 && <p>{lang === 'zh' ? '没有匹配文献。' : 'No matching literature.'}</p>}</div>}
          {localSearchIssues?.outcome === 'ready' && <div className="card span3"><Head title={lang === 'zh' ? '资产级索引问题' : 'Asset index issues'} action={<span>{localSearchIssues.issues.length}</span>} />{localSearchIssues.issues.length ? localSearchIssues.issues.map(issue => <p key={`${issue.itemId}-${issue.assetId}`}>{issue.kind}{issue.detail ? ` · ${issue.detail}` : ''}</p>) : <p>{lang === 'zh' ? '没有资产级问题。' : 'No asset-level issues.'}</p>}</div>}
          {localSearchIssues?.outcome === 'unavailable' && <div className="card span3"><p>{lang === 'zh' ? '索引不可用，暂不能读取问题列表。' : 'The index is unavailable, so issues cannot be read yet.'}</p></div>}
        </>}
      </section>}

      {activeWorkspace === 'source' && <section className="page">
        <div className="card span3 hero">
          <h1>{t.sourceTitle}</h1>
          <p>{t.sourceDesc}</p>
          {hasVault ? <p>{vaultSummary?.source?.name ? `${vaultSummary.source.name} · ${vaultSummary.source.entity ?? 'source'} · ${vaultSummary.vault_id ?? 'vault'}` : t.noData}</p> : <><p>{t.connectForAnalysis}</p><div className="actions"><button onClick={async () => { const path = await dir(); if (path) await run(() => connect(path)); }} disabled={busy}>{t.openForAnalysis}</button></div></>}
        </div>
        <div className="card span3 compact">
          <h2>{t.currentVault}</h2>
          <div className="sourceRow"><div className="path">{short(vaultPath)}</div><button className="light" onClick={() => void run(() => connect(vaultPath))} disabled={busy || !vaultPath}>{t.connect}</button><button className="light" onClick={() => refreshVaultContext()} disabled={busy || !vaultPath}>{t.refreshVault}</button></div>
          <small>{vaultSummary?.vault_id ? `${t.vaultId}: ${vaultSummary.vault_id}` : t.noData}</small>
          <small>{vaultTableCount != null ? `tables: ${vaultTableCount}` : t.noData}</small>
          <small>{vaultSummary?.source?.input_path ? short(vaultSummary.source.input_path) : t.noData}</small>
        </div>
        {analysisError && <div className="card span3"><h2>{t.analysisFailed}</h2><p>{analysisError}</p><div className="actions"><button className="secondary" onClick={() => void refresh(appliedQuery)} disabled={busy || !hasVault}>{lang === 'zh' ? '重试分析' : 'Retry analysis'}</button></div></div>}
        <div className="tabs span3">
          {(['overview', 'table', 'visual', 'metrics', 'export'] as const).map((k) => <button key={k} className={sourceTab === k ? 'selected' : ''} onClick={() => setSourceTab(k)}>{t[k]}</button>)}
        </div>
        {sourceTab === 'overview' && <>
          {!hasVault && <div className="card span3"><h2>{t.connectForAnalysis}</h2><p>{t.noData}</p></div>}
          {hasVault && overview && !analysisError && <Stats t={t} overview={overview} />}
          <div className="card span2"><h2>{t.layout}</h2><p>{t.parquet}</p><p>{t.arrow}</p><p>{t.manifest}</p></div>
          <div className="card span2"><h2>{lang === 'zh' ? '来源适配器' : 'Source adapters'}</h2><p>{lang === 'zh' ? '当前可用的来源适配器列表。' : 'Available source adapters.'}</p><small>{sourceAdapters.map(a => `${a.name}${a.is_default ? ' · 默认' : ''}`).join(' | ')}</small></div>
          <div className="card span2"><h2>{lang === 'zh' ? '当前筛选摘要' : 'Current query summary'}</h2><p>{currentQueryText}</p>{queryDirty && <small>{t.queryPending}</small>}<div className="actions"><button className="secondary" onClick={() => void resetQuery()} disabled={busy}>{lang === 'zh' ? '重置筛选' : 'Reset filters'}</button><button className="secondary" onClick={() => void refresh(currentQuery)} disabled={busy || !overview}>{lang === 'zh' ? '重新查询' : 'Rerun query'}</button></div></div>
          <div className="card span2"><h2>{t.recentQueries}</h2><div className="recent">{recentQueries.length ? recentQueries.map((q, i) => <button key={`${q.sourceType}-${i}`} onClick={() => { setSourceType(q.sourceType); setMetric(q.metric); setText(q.text); setCountry(q.country); setOaFilter(q.oaFilter); void refresh(q); }}>{queryLabel(q, lang)}</button>) : <small>{t.noRecentQueries}</small>}</div></div>
        </>}
        {sourceTab === 'table' && <>
          {!hasVault ? <div className="card span3"><h2>{t.connectForAnalysis}</h2><p>{t.noData}</p></div> : <><Controls t={t} sourceType={sourceType} setSourceType={setSourceType} metric={metric} setMetric={setMetric} text={text} setText={setText} country={country} setCountry={setCountry} oaFilter={oaFilter} setOaFilter={setOaFilter} refresh={refresh} busy={busy} overview={overview} />
          <div className="card span3"><h2>{lang === 'zh' ? '当前筛选摘要' : 'Current query summary'}</h2><p>{currentQueryText}</p>{queryDirty && <small>{t.queryPending}</small>}<p>{lang === 'zh' ? `检索结果 ${resultCount} 条` : `${resultCount} results`}</p><div className="actions"><button className="secondary" onClick={() => void resetQuery()} disabled={busy}>{lang === 'zh' ? '重置筛选' : 'Reset filters'}</button><button className="secondary" onClick={() => void refresh(currentQuery)} disabled={busy || !overview}>{lang === 'zh' ? '重新查询' : 'Rerun query'}</button></div></div>
          <div className="card span3"><h2>{t.recentQueries}</h2><div className="recent">{recentQueries.length ? recentQueries.map((q, i) => <button key={`${q.sourceType}-${i}`} onClick={() => { setSourceType(q.sourceType); setMetric(q.metric); setText(q.text); setCountry(q.country); setOaFilter(q.oaFilter); void refresh(q); }}>{queryLabel(q, lang)}</button>) : <small>{t.noRecentQueries}</small>}</div></div>
          <DataTable rows={results} /></>}
        </>}
        {sourceTab === 'visual' && <>
          <Head title={t.ranking} action={<button className="textBtn" onClick={() => exportRows('top')} disabled={!hasVault || !overview || Boolean(analysisError) || busy}>{t.exportRank}</button>} />
          <div className="card span2"><h2>{lang === 'zh' ? '排行摘要' : 'Ranking summary'}</h2><p>{lang === 'zh' ? `排行结果 ${rankingCount} 条` : `${rankingCount} ranked items`}</p><p>{currentQueryText}</p>{queryDirty && <small>{t.queryPending}</small>}</div>
          <ReactECharts option={chart} style={{ height: 360 }} />
        </>}
        {sourceTab === 'metrics' && <>
          <div className="card span3 metricGrid"><h2>{t.metricGuide}</h2>{[t.hTip, t.citedTip, t.worksTip, t.i10Tip, t.meanTip].map((x, i) => <p key={i}>{x}</p>)}</div>
        </>}
        {sourceTab === 'export' && <>
          <div className="card span2"><h2>{t.export}</h2><p>{lang === 'zh' ? '导出当前已执行的筛选或排行结果。' : 'Export the currently executed ranking or search result.'}</p><p>{currentQueryText}</p>{queryDirty && <small>{t.queryPending}</small>}<p>{lang === 'zh' ? `检索结果 ${resultCount} 条，排行结果 ${rankingCount} 条` : `${resultCount} search results, ${rankingCount} ranked items`}</p><div className="actions"><button onClick={() => exportRows('top')} disabled={busy || !overview || Boolean(analysisError) }>{t.exportRank}</button><button className="secondary" onClick={() => exportRows('search')} disabled={busy || !overview || Boolean(analysisError)}>{t.exportSearch}</button></div></div>
        </>}
      </section>}

      {activeWorkspace === 'notes' && <section className="page">
        <div className="card span3 hero">
          <h1>{t.notesTitle}</h1><p>{t.notesDesc}</p>
          {!hasVault ? <p>{t.noVaultReading}</p> : <p>{vaultSummary?.vault_id ?? t.connected}</p>}
        </div>
        {hasVault && <div className="card span3 notesToolbar">
          <label className="field"><span>{t.noteItemFilter}</span>
            <select value={noteFilterItemId} onChange={e => { setNoteFilterItemId(e.target.value); setSelectedNoteId(null); }} disabled={noteLoading}>
              <option value="all">{t.allItems}</option>
              {literatureItems.map(item => <option key={item.itemId} value={item.itemId}>{item.title || '—'}</option>)}
            </select>
          </label>
          <button onClick={beginNewNote} disabled={noteSaving || noteLoading}>{t.newNote}</button>
        </div>}
        {hasVault && noteFeedback && <div className="card span3"><p className="literatureFeedback" onClick={() => setNoteFeedback('')}>{noteFeedback}</p></div>}
        {hasVault && noteError && <div className="card span3"><h2>{t.failed}</h2><p>{noteError}</p></div>}
        {hasVault && noteConflict && <div className="card span3"><h2>{t.noteConflict}</h2><p>{t.noteConflictMessage}</p><p>{noteConflict.message}</p><div className="actions"><button onClick={refreshNote} disabled={noteSaving}>{t.refreshNote}</button><button onClick={overwriteNote} disabled={noteSaving}>{t.overwriteNote}</button></div></div>}
        {hasVault && <div className="card span3 notesEditor">
          <label className="field"><span>{t.noteTitle}</span><input value={noteEditorTitle} onChange={e => setNoteEditorTitle(e.target.value)} disabled={noteSaving} /></label>
          <label className="field"><span>{t.noteBody}</span><textarea value={noteEditorBody} onChange={e => setNoteEditorBody(e.target.value)} disabled={noteSaving} rows={12} /></label>
          <div className="actions"><button onClick={saveNote} disabled={noteSaving || !noteEditorTitle.trim()}>{noteSaving ? t.savingNote : t.saveNote}</button></div>
        </div>}
        {hasVault && <div className="card span3 notesList">
          <h2>{t.notes}</h2>
          {noteLoading ? <p>{lang === 'zh' ? '加载中…' : 'Loading…'}</p> : (notes.length ? notes.map(note => <article className={`literatureItem ${note.archivedAt ? 'unavailable' : ''}`} key={note.noteId} onClick={() => setSelectedNoteId(note.noteId)}>
            <div className="literatureItemDetail">
              <h3>{note.title || '—'}{note.archivedAt && <small> · {t.noteArchived}</small>}</h3>
              <p>{new Date(note.updatedAt).toLocaleString(lang === 'zh' ? 'zh-CN' : 'en-US')}</p>
            </div>
            <div className="itemActions">
              {note.archivedAt
                ? <button className="secondary" onClick={(e) => { e.stopPropagation(); restoreNote(note); }} disabled={noteSaving}>{t.unarchiveNote}</button>
                : <button className="secondary danger" onClick={(e) => { e.stopPropagation(); archiveNote(note); }} disabled={noteSaving}>{t.archiveNote}</button>}
            </div>
          </article>) : <p>{t.noNotes}</p>)}
        </div>}
        {hasVault && selectedNoteId && <div className="card span3 annotationsPanel">
          <h2>{t.annotations}</h2>
          {annotationLoading ? <p>{lang === 'zh' ? '加载中…' : 'Loading…'}</p> : (annotationError ? <p>{t.annotationLoadError}: {annotationError}</p> : (annotations.length ? annotations.map(a => <article className="literatureFile" key={a.annotationId}>
            <div className="assetMeta">
              <strong>{t.annotationPage} {a.anchor.pageNumber}</strong>
              <small>{a.anchor.selectedText}</small>
              <small>{t.annotationResolution}: {annotationStatusLabel(a.resolution, t)}</small>
            </div>
            <div className="fileActions"><button className="secondary" onClick={() => openAnnotationAsset(a)} disabled={a.resolution !== 'resolved_exact' || noteSaving}>{t.openAssociatedAsset}</button></div>
          </article>) : <p>{t.noAnnotations}</p>))}
        </div>}
      </section>}

      {activeWorkspace === 'settings' && <section className="page">
        <div className="card span2">
          <h2>{t.language}</h2>
          <button className={lang === 'zh' ? 'selected secondary' : 'secondary'} onClick={() => { setLang('zh'); localStorage.setItem('lang', 'zh'); }}>{t.chinese}</button>
          <button className={lang === 'en' ? 'selected secondary' : 'secondary'} onClick={() => { setLang('en'); localStorage.setItem('lang', 'en'); }}>{t.english}</button>
        </div>
        <div className="card span2">
          <h2>{t.brand}</h2>
          <p>{lang === 'zh' ? '桌面优先、免安装、库即一切。' : 'Desktop-first, portable, vault-first.'}</p>
          <p>{lang === 'zh' ? `当前支持来源：${sourceAdapters.map(a => a.name).join('、')}` : `Supported sources: ${sourceAdapters.map(a => a.name).join(', ')}`}</p>
          <p>{vaultSummary?.vault_id ? `${t.vaultId}: ${vaultSummary.vault_id}` : (lang === 'zh' ? '尚未连接库。' : 'No vault connected yet.')}</p>
          <p>{vaultTableCount != null ? `tables: ${vaultTableCount}` : (lang === 'zh' ? '尚无表信息。' : 'No table info yet.')}</p>
          <small>{sourceAdapters.map(a => `${a.name}${a.is_default ? ' · 默认' : ''}`).join(' | ')}</small>
        </div>
      </section>}
    </main>
  </div>;
}

function formatFileSize(size: number | null) {
  if (size == null) return '—';
  if (size < 1024) return `${size} B`;
  if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} KB`;
  return `${(size / (1024 * 1024)).toFixed(1)} MB`;
}

function AssetList(p: {
  assets: DocumentAsset[]; t: Dict; busy: boolean;
  onOpen: (asset: DocumentAsset) => void;
  onSetKind: (asset: DocumentAsset, kind: DocumentAssetKind) => void;
  onSetDefault: (asset: DocumentAsset) => void;
  onMigrate: (asset: DocumentAsset) => void;
  onRemove: (asset: DocumentAsset) => void;
}) {
  if (!p.assets.length) return <small>{p.t.noFiles}</small>;
  return <div className="literatureFiles assetList">{p.assets.map(asset => <div className="literatureFile assetFile" key={asset.assetId}>
    <div className="assetMeta"><strong>{asset.displayName}</strong>
      <small>{p.t.assetStorage}: {asset.storageKind === 'vault' ? p.t.vaultFile : p.t.externalFile}{asset.isDefault ? ` · ${p.t.defaultFile}` : ''}</small>
      <small>{p.t.assetKind}: {p.t.assetKinds[asset.assetKind]} · {p.t.assetHealth}: {p.t.assetStatuses[asset.status]} · {p.t.assetSize}: {formatFileSize(asset.fileSize)}</small>
      <small>{p.t.assetHash}: {asset.contentHash ? `${asset.contentHash.slice(0, 12)}…` : '—'} · {p.t.assetPath}: {short(asset.path)}</small>
    </div>
    <div className="fileActions"><button className="secondary" onClick={() => p.onOpen(asset)} disabled={p.busy}>{p.t.openFile}</button>
      {!asset.isDefault && <button className="secondary" onClick={() => p.onSetDefault(asset)} disabled={p.busy}>{p.t.setDefaultFile}</button>}
      <select value={asset.assetKind} aria-label={p.t.assetKind} onChange={e => p.onSetKind(asset, e.target.value as DocumentAssetKind)} disabled={p.busy}>
        {(['primary', 'supplement', 'version', 'appendix', 'other'] as DocumentAssetKind[]).map(kind => <option key={kind} value={kind}>{p.t.assetKinds[kind]}</option>)}
      </select>
      {asset.storageKind === 'external' && <button className="secondary" onClick={() => p.onMigrate(asset)} disabled={p.busy}>{p.t.migrateToVault}</button>}
      <button className="secondary danger" onClick={() => p.onRemove(asset)} disabled={p.busy}>{p.t.removeAsset}</button>
    </div>
  </div>)}</div>;
}

function ReadingAssetList(p: { t: Dict; assets: DocumentAsset[]; busy: boolean; onStart: (asset: DocumentAsset) => void }) {
  if (!p.assets.length) return <small>{p.t.noFiles} {p.t.manageAssetsInVault}</small>;
  return <div className="literatureFiles readingAssetList">{p.assets.map(asset => {
    const available = asset.status === 'available';
    return <div className={`literatureFile readingAsset ${available ? '' : 'unavailable'}`} key={asset.assetId}>
      <div className="assetMeta"><strong>{asset.displayName}</strong>
        <small>{p.t.assetKind}: {p.t.assetKinds[asset.assetKind]} · {p.t.assetStorage}: {asset.storageKind === 'vault' ? p.t.vaultFile : p.t.externalFile}</small>
        <small>{p.t.assetHealth}: {p.t.assetStatuses[asset.status]} · {p.t.assetSize}: {formatFileSize(asset.fileSize)}</small>
        {!available && <small className="sessionWarning">{p.t.sessionUnavailable}</small>}
      </div>
      <div className="fileActions"><button onClick={() => p.onStart(asset)} disabled={p.busy || !available}>{p.t.startReading}</button></div>
    </div>;
  })}</div>;
}

function Stats({ t, overview }: { t: Dict; overview: Overview | null }) {
  return <div className="stats span2">{[
    [overview?.source_count, t.sources],
    [overview?.journal_count, t.journals],
    [overview?.conference_count, t.conferences],
    [overview?.oa_count, t.oa],
    [overview?.works_count, t.works],
    [overview?.cited_by_count, t.cited],
  ].map(([v, l]) => <div className="mini" key={String(l)}><span>{l}</span><b>{fmt(v)}</b></div>)}</div>;
}

function Controls(p: {
  t: Dict; sourceType: string; setSourceType: (v: string) => void; metric: string; setMetric: (v: string) => void; text: string; setText: (v: string) => void; country: string; setCountry: (v: string) => void; oaFilter: 'all' | 'oa' | 'non_oa'; setOaFilter: (v: 'all' | 'oa' | 'non_oa') => void; refresh: () => void; busy: boolean; overview: Overview | null;
}) {
  const t = p.t;
  return <div className="card span3 controls">
    <Sel label={t.type} value={p.sourceType} set={p.setSourceType} opts={[[ 'journal', t.journal ], [ 'conference', t.conference ]]} />
    <Sel label={t.metric} value={p.metric} set={p.setMetric} opts={t.metricOptions} />
    <Field label={t.keyword} value={p.text} set={p.setText} enter={p.refresh} />
    <Field label={t.country} value={p.country} set={(v) => p.setCountry(v.toUpperCase())} enter={p.refresh} />
    <Sel label={t.oaFilter} value={p.oaFilter} set={p.setOaFilter} opts={[[ 'all', t.all ], [ 'oa', t.onlyOa ], [ 'non_oa', t.onlyNonOa ]]} />
    <button onClick={p.refresh} disabled={p.busy || !p.overview}>{t.run}</button>
  </div>;
}

function Path(p: { label: string; value: string; button: string; onPick: () => void; disabled: boolean }) {
  return <div className="pathPick"><label>{p.label}</label><div><span>{short(p.value)}</span><button className="light" onClick={p.onPick} disabled={p.disabled}>{p.button}</button></div></div>;
}

function Head({ title, action }: { title: string; action: React.ReactNode }) {
  return <div className="cardHead"><h2>{title}</h2>{action}</div>;
}

function Sel(p: { label: string; value: string; set: (v: any) => void; opts: string[][] }) {
  return <label className="field"><span>{p.label}</span><select value={p.value} onChange={e => p.set(e.target.value)}>{p.opts.map(([v, l]) => <option key={v} value={v}>{l}</option>)}</select></label>;
}

function Field(p: { label: string; value: string; set: (v: string) => void; enter: () => void }) {
  return <label className="field"><span>{p.label}</span><input value={p.value} onChange={e => p.set(e.target.value)} onKeyDown={e => { if (e.key === 'Enter') p.enter(); }} /></label>;
}

function DataTable({ rows }: { rows: Row[] }) {
  const cols = ['display_name', 'source_type', 'country_code', 'h_index', 'i10_index', 'cited_by_count', 'works_count', 'mean_citedness_2yr'];
  return <div className="tableWrap"><table><thead><tr>{cols.map(c => <th key={c}>{c}</th>)}</tr></thead><tbody>{rows.map((r, i) => <tr key={String(r.openalex_id ?? i)}>{cols.map(c => <td key={c}>{fmt(r[c])}</td>)}</tr>)}</tbody></table></div>;
}

function annotationStatusLabel(resolution: AnnotationResolution | undefined, t: Dict) {
  if (resolution === 'resolved_exact') return t.resolvedExact;
  if (!resolution) return '—';
  const map: Record<string, string> = {
    unavailable_missing_asset: 'missing asset',
    unavailable_external_asset: 'external asset',
    unavailable_unreadable_asset: 'unreadable asset',
    invalidated_content_changed: 'content changed',
    invalidated_page_out_of_range: 'page out of range',
    invalidated_text_not_found: 'text not found',
    invalidated_ambiguous_text: 'ambiguous text',
    unsupported_extractor_version: 'unsupported extractor',
    orphaned_item: 'orphaned item',
  };
  return map[resolution] ?? resolution;
}
