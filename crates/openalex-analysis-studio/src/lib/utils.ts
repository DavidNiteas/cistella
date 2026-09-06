import { open } from '@tauri-apps/plugin-dialog';
import type { Dict } from './i18n/dict';
import type {
  Lang,
  LiteratureDraft,
  LiteratureImportFormat,
  Overview,
  QuerySnapshot,
  RecentVaultDto,
  VaultSummary,
  Workspace,
  SourceTab,
} from '../types';
import { invoke } from './invoke';

export function loadWorkspace(): Workspace {
  const value = localStorage.getItem('workspace');
  return value === 'reading' || value === 'search' || value === 'source' || value === 'settings' || value === 'notes' ? value : 'vault';
}

export function loadSourceTab(): SourceTab {
  const value = localStorage.getItem('sourceTab');
  return value === 'table' || value === 'visual' || value === 'metrics' || value === 'export' ? value : 'overview';
}

export function rows(v: unknown): Record<string, unknown>[] {
  return Array.isArray(v) ? v : [];
}

export function first(v: unknown): Overview {
  return (Array.isArray(v) ? v[0] : v || {}) as Overview;
}

export function fmt(v: unknown) {
  return typeof v === 'number' ? Math.round(v).toLocaleString() : v == null || v === '' ? '—' : String(v);
}

export function short(p: string) {
  return !p ? '—' : p.length > 82 ? `…${p.slice(-79)}` : p;
}

export function formatSessionTime(value: string, lang: Lang) {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString(lang === 'zh' ? 'zh-CN' : 'en-US');
}

export function parseNoteConflict(error: unknown): { noteId: string; message: string } | null {
  const raw = typeof error === 'string' ? error : (error as any)?.message;
  const message = String(raw ?? '');
  if (message.startsWith('NOTE_CONFLICT|')) {
    const noteId = message.slice('NOTE_CONFLICT|'.length);
    return { noteId, message: `note revision conflict: ${noteId}` };
  }
  return null;
}

export function baseName(p: string) {
  const s = p.replace(/\\/g, '/');
  return s.slice(s.lastIndexOf('/') + 1) || p;
}

export function detectIdentifierType(raw: string): 'doi' | 'pmid' | 'pmcid' | null {
  const s = raw.trim().toLowerCase();
  if (!s) return null;
  if (s.startsWith('doi:') || s.startsWith('https://doi.org/') || s.startsWith('http://doi.org/') || s.includes('/')) return 'doi';
  if (s.startsWith('pmid:')) return 'pmid';
  if (s.startsWith('pmc:') || s.startsWith('pmcid:')) return 'pmcid';
  if (/^\d+$/.test(s)) return 'pmid';
  return null;
}

export function loadRecent(): string[] {
  try {
    return JSON.parse(localStorage.getItem('recentVaults') || '[]');
  } catch {
    return [];
  }
}

export function remember(path: string) {
  const next = [path, ...loadRecent().filter((p) => p !== path)].slice(0, 6);
  localStorage.setItem('recentVaults', JSON.stringify(next));
  return next;
}

export async function fetchRecentVaults(): Promise<RecentVaultDto[]> {
  try {
    return await invoke<RecentVaultDto[]>('recent_libraries');
  } catch {
    return [];
  }
}

export async function persistRecentVaults(vaults: RecentVaultDto[]) {
  try {
    await invoke('update_recent_libraries', { vaults });
  } catch {
    /* keep local state even if backend persist fails */
  }
}

export async function rememberBackend(path: string, existing: RecentVaultDto[]): Promise<RecentVaultDto[]> {
  const name = baseName(path);
  const next = [{ path, name, openedAt: new Date().toISOString() }, ...existing.filter((v) => v.path !== path)].slice(0, 6);
  await persistRecentVaults(next);
  return next;
}

export function loadRecentQueries(): QuerySnapshot[] {
  try {
    return JSON.parse(localStorage.getItem('recentSourceQueries') || '[]');
  } catch {
    return [];
  }
}

export function rememberQuery(snapshot: QuerySnapshot) {
  const next = [snapshot, ...loadRecentQueries().filter((q) => JSON.stringify(q) !== JSON.stringify(snapshot))].slice(0, 5);
  localStorage.setItem('recentSourceQueries', JSON.stringify(next));
  return next;
}

export function queryLabel(q: QuerySnapshot, lang: Lang) {
  const query = q.text.trim() ? q.text.trim() : lang === 'zh' ? '空关键词' : 'empty';
  const country = q.country.trim() ? q.country.trim().toUpperCase() : lang === 'zh' ? '不限国家' : 'any country';
  const oa = q.oaFilter === 'oa' ? 'OA' : q.oaFilter === 'non_oa' ? 'non-OA' : lang === 'zh' ? '全部OA状态' : 'all OA states';
  return `${q.sourceType} · ${q.metric} · ${query} · ${country} · ${oa}`;
}

export function summarizeManifest(v: unknown): VaultSummary {
  const m = v as any;
  return {
    vault_id: typeof m?.vault_id === 'string' ? m.vault_id : undefined,
    created_at: typeof m?.created_at === 'string' ? m.created_at : undefined,
    source:
      m?.source && typeof m.source === 'object'
        ? {
            name: typeof m.source.name === 'string' ? m.source.name : undefined,
            entity: typeof m.source.entity === 'string' ? m.source.entity : undefined,
            snapshot_date: typeof m.source.snapshot_date === 'string' ? m.source.snapshot_date : null,
            input_path: typeof m.source.input_path === 'string' ? m.source.input_path : undefined,
          }
        : undefined,
  };
}

export async function dir() {
  const v = await open({ directory: true, multiple: false });
  return typeof v === 'string' ? v : '';
}

export async function file() {
  const v = await open({ multiple: false, filters: [{ name: 'OpenAlex data', extensions: ['arrow', 'ipc', 'parquet'] }] });
  return typeof v === 'string' ? v : '';
}

export async function pdfFile() {
  const v = await open({ multiple: false, filters: [{ name: 'PDF', extensions: ['pdf'] }] });
  return typeof v === 'string' ? v : '';
}

export async function literatureFile(format: LiteratureImportFormat) {
  const extensions = format === 'bibtex' ? ['bib'] : ['ris'];
  const name = format === 'bibtex' ? 'BibTeX' : 'RIS';
  const v = await open({ multiple: false, filters: [{ name, extensions }] });
  return typeof v === 'string' ? v : '';
}

export function emptyLiteratureDraft(): LiteratureDraft {
  return {
    title: '',
    authors: [],
    publishedYear: null,
    itemType: 'article',
    favorite: false,
    readingStatus: 'inbox',
    tags: [],
    externalIdentifiers: [],
  };
}

export function formatFileSize(size: number | null) {
  if (size == null) return '—';
  if (size < 1024) return `${size} B`;
  if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} KB`;
  return `${(size / (1024 * 1024)).toFixed(1)} MB`;
}

export function annotationStatusLabel(resolution: string | undefined, t: Dict) {
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
