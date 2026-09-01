import { useMemo, useRef, useState } from 'react';
import type { Dict } from '../lib/i18n/dict';
import type {
  LiteratureDraft,
  LiteratureImportFormat,
  LiteratureImportPreview,
  LiteratureImportResult,
  LiteratureItem,
  OpenAlexWorkCandidate,
  RemoteSourceRecord,
  VaultConnection,
  VaultRequestContext,
} from '../types';
import type { VaultContextValue } from './useVaultContext';
import { invoke } from '../lib/invoke';
import { dir, emptyLiteratureDraft } from '../lib/utils';
import { useToast } from '../components/ui/Toast/ToastProvider';

export interface LiteratureState {
  items: LiteratureItem[];
  keyword: string;
  setKeyword: (value: string) => void;
  filteredItems: LiteratureItem[];

  // Editor
  editingId: string | null;
  editorOpen: boolean;
  draft: LiteratureDraft;
  editingItem: LiteratureItem | undefined;
  beginNew: () => void;
  beginEdit: (item: LiteratureItem) => void;
  closeEditor: () => void;
  updateDraft: (patch: Partial<LiteratureDraft>) => void;
  save: () => Promise<void>;
  deleteId: string | null;
  requestDelete: (itemId: string | null) => void;
  confirmDelete: () => Promise<void>;
  toggleFavorite: (item: LiteratureItem) => Promise<void>;

  // File import
  fileInputRef: { current: HTMLInputElement | null };
  importFormat: LiteratureImportFormat;
  setImportFormat: (value: LiteratureImportFormat) => void;
  importPreview: LiteratureImportPreview | null;
  importError: string;
  importResult: LiteratureImportResult | null;
  inspectImport: () => void;
  handleFileSelected: (file: File) => Promise<void>;
  updateImportPolicy: (recordId: string, policy: 'merge' | 'skip' | 'create') => void;
  commitImport: () => Promise<void>;
  clearImportResult: () => void;
  dismissImportPreview: () => void;

  // Remote resolve
  identifierInput: string;
  setIdentifierInput: (value: string) => void;
  remoteResolveLoading: boolean;
  remoteResolveError: string;
  setRemoteResolveError: (value: string) => void;
  remoteResolvePreview: LiteratureImportPreview | null;
  setRemoteResolvePreview: (value: LiteratureImportPreview | null) => void;
  remoteImportStrategy: 'merge' | 'skip' | 'create';
  updateRemoteImportStrategy: (policy: 'merge' | 'skip' | 'create') => void;
  resolveRemoteMetadata: () => Promise<void>;
  commitRemoteImport: () => Promise<void>;
  clearRemoteCache: () => Promise<void>;

  // OpenAlex works
  openAlexWorksDir: string;
  setOpenAlexWorksDir: (value: string) => void;
  openAlexQuery: string;
  setOpenAlexQuery: (value: string) => void;
  openAlexCandidates: OpenAlexWorkCandidate[];
  openAlexLoading: boolean;
  openAlexError: string;
  pickOpenAlexWorksDir: () => Promise<void>;
  searchOpenAlexWorks: () => Promise<void>;
  previewOpenAlexWork: (candidate: OpenAlexWorkCandidate) => Promise<void>;

  loadItems: (request: VaultRequestContext) => Promise<void>;
  load: (request?: VaultRequestContext) => Promise<void>;
  reloadAssets: () => Promise<void>;
}

export function useLiterature(vault: VaultConnection, vaultContext: VaultContextValue, t: Dict): LiteratureState {
  const [items, setItems] = useState<LiteratureItem[]>([]);
  const [keyword, setKeyword] = useState('');
  const toast = useToast();

  const [editingId, setEditingId] = useState<string | null>(null);
  const [editorOpen, setEditorOpen] = useState(false);
  const [draft, setDraft] = useState<LiteratureDraft>(emptyLiteratureDraft());

  const [deleteId, setDeleteId] = useState<string | null>(null);

  const [importFormat, setImportFormat] = useState<LiteratureImportFormat>('bibtex');
  const [importPreview, setImportPreview] = useState<LiteratureImportPreview | null>(null);
  const [importError, setImportError] = useState('');
  const [importResult, setImportResult] = useState<LiteratureImportResult | null>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);

  const [identifierInput, setIdentifierInput] = useState('');
  const [remoteResolveLoading, setRemoteResolveLoading] = useState(false);
  const [remoteResolveError, setRemoteResolveError] = useState('');
  const [remoteResolvePreview, setRemoteResolvePreview] = useState<LiteratureImportPreview | null>(null);
  const [remoteImportStrategy, setRemoteImportStrategy] = useState<'merge' | 'skip' | 'create'>('merge');

  const [openAlexWorksDir, setOpenAlexWorksDir] = useState('');
  const [openAlexQuery, setOpenAlexQuery] = useState('');
  const [openAlexCandidates, setOpenAlexCandidates] = useState<OpenAlexWorkCandidate[]>([]);
  const [openAlexLoading, setOpenAlexLoading] = useState(false);
  const [openAlexError, setOpenAlexError] = useState('');

  const editingItem = useMemo(() => (editingId ? items.find((item) => item.itemId === editingId) : undefined), [editingId, items]);

  const filteredItems = useMemo(() => {
    const q = keyword.trim().toLowerCase();
    if (!q) return items;
    return items.filter((item) => [item.title, ...item.authors, ...item.tags].join(' ').toLowerCase().includes(q));
  }, [items, keyword]);

  const loadItems = async (request: VaultRequestContext) => {
    if (!request.expectedVaultPath) {
      if (vault.isCurrentVaultRequest(request)) setItems([]);
      return;
    }
    try {
      const value = await invoke<LiteratureItem[]>('list_literature_items');
      if (vault.isCurrentVaultRequest(request)) {
        setItems(Array.isArray(value) ? value : []);
      }
    } catch (e: any) {
      if (vault.isCurrentVaultRequest(request)) toast.push(`${t.failed}: ${e?.message ?? e}`, 'error');
    }
  };

  const load = async (request?: VaultRequestContext) => {
    const req = request ?? vault.captureVaultRequest();
    await loadItems(req);
    await vaultContext.refresh(req);
  };

  const reloadAssets = async () => {
    await vaultContext.refresh();
  };

  const beginNew = () => {
    setEditingId(null);
    setDraft(emptyLiteratureDraft());
    setEditorOpen(true);
  };

  const beginEdit = (item: LiteratureItem) => {
    setEditingId(item.itemId);
    setDraft({
      title: item.title,
      authors: item.authors,
      publishedYear: item.publishedYear,
      itemType: item.itemType,
      favorite: item.favorite,
      readingStatus: item.readingStatus,
      tags: item.tags,
      externalIdentifiers: item.externalIdentifiers,
    });
    setEditorOpen(true);
  };

  const closeEditor = () => {
    setEditingId(null);
    setEditorOpen(false);
    setDraft(emptyLiteratureDraft());
  };

  const updateDraft = (patch: Partial<LiteratureDraft>) => {
    setDraft((prev) => ({ ...prev, ...patch }));
  };

  const literatureRequest = () => ({
    ...draft,
    authors: draft.authors.filter(Boolean),
    tags: draft.tags.filter(Boolean),
  });

  const save = async () => {
    await vault.run(async () => {
      const request = vault.captureVaultRequest();
      if (!vault.hasVault || !draft.title.trim() || !vault.isCurrentVaultRequest(request)) return;
      if (editingId) {
        await invoke('update_literature_item', { itemId: editingId, req: literatureRequest() });
      } else {
        await invoke('create_literature_item', { req: literatureRequest() });
      }
      await load(request);
      if (!vault.isCurrentVaultRequest(request)) return;
      setEditingId(null);
      setEditorOpen(false);
      setDraft(emptyLiteratureDraft());
      toast.push(editingId ? t.saveLiterature : t.addLiterature, 'success');
    });
  };

  const requestDelete = (itemId: string | null) => setDeleteId(itemId);

  const confirmDelete = async () => {
    const itemId = deleteId;
    if (!itemId) return;
    setDeleteId(null);
    await vault.run(async () => {
      const request = vault.captureVaultRequest();
      if (!vault.isCurrentVaultRequest(request)) return;
      await invoke('delete_literature_item', { itemId });
      await load(request);
      toast.push(t.deleteLiterature, 'success');
    });
  };

  const toggleFavorite = async (item: LiteratureItem) => {
    await vault.run(async () => {
      const request = vault.captureVaultRequest();
      if (!vault.isCurrentVaultRequest(request)) return;
      await invoke('set_literature_item_favorite', { itemId: item.itemId, favorite: !item.favorite });
      await loadItems(request);
      toast.push(item.favorite ? t.favorite : t.favorite, 'success');
    });
  };

  const inspectImport = () => {
    if (importFormat === 'openalex_works') return;
    fileInputRef.current?.click();
  };

  const handleFileSelected = async (file: File) => {
    setImportError('');
    setImportResult(null);
    vault.setBusy(true);
    try {
      const bytes = new Uint8Array(await file.arrayBuffer());
      const preview = await invoke<LiteratureImportPreview>('inspect_literature_import', {
        format: importFormat,
        bytes: Array.from(bytes),
      });
      setImportPreview(preview);
      toast.push(t.importPreview, 'info');
    } catch (e: any) {
      const message = String(e?.message ?? e);
      setImportError(message);
      setImportPreview(null);
      toast.push(`${t.importFailed}: ${message}`, 'error');
    } finally {
      vault.setBusy(false);
    }
  };

  const updateImportPolicy = (recordId: string, policy: 'merge' | 'skip' | 'create') => {
    setImportPreview((prev) => {
      if (!prev) return prev;
      return {
        ...prev,
        items: prev.items.map((item) => (item.recordId === recordId ? { ...item, selectedPolicy: policy } : item)),
      };
    });
  };

  const commitImport = async () => {
    if (!importPreview) return;
    vault.setBusy(true);
    setImportError('');
    try {
      const result = await invoke<LiteratureImportResult>('import_literature_file', {
        format: importFormat,
        req: { preview: importPreview },
      });
      setImportResult(result);
      setImportPreview(null);
      const request = vault.captureVaultRequest();
      await load(request);
      toast.push(t.importResultTitle, 'success');
    } catch (e: any) {
      const message = String(e?.message ?? e);
      setImportError(message);
      toast.push(`${t.importFailed}: ${message}`, 'error');
    } finally {
      vault.setBusy(false);
    }
  };

  const findMatchingRemoteItem = (record: RemoteSourceRecord): string | null => {
    const namespaces = new Set(['doi', 'isbn', 'issn', 'pmid', 'openalex']);
    for (const item of items) {
      for (const recordId of record.externalIdentifiers) {
        if (!namespaces.has(recordId.namespace)) continue;
        for (const itemId of item.externalIdentifiers) {
          if (itemId.namespace === recordId.namespace && itemId.value.toLowerCase() === recordId.value.toLowerCase()) {
            return item.itemId;
          }
        }
      }
    }
    return null;
  };

  const resolveRemoteMetadata = async () => {
    const raw = identifierInput.trim();
    if (!raw) return;
    setRemoteResolveLoading(true);
    setRemoteResolveError('');
    setRemoteResolvePreview(null);
    try {
      const record = await invoke<RemoteSourceRecord>('resolve_remote_metadata', { identifier: raw, forceRefresh: false });
      const matchedItemId = findMatchingRemoteItem(record);
      const preview: LiteratureImportPreview = {
        batchId: 'remote',
        sourceName: record.sourceName,
        items: [
          {
            recordId: record.recordId,
            sourceRecord: record,
            matchedItemId,
            defaultPolicy: 'merge',
            selectedPolicy: remoteImportStrategy,
          },
        ],
      };
      setRemoteResolvePreview(preview);
    } catch (e: any) {
      const message = String(e?.message ?? e);
      setRemoteResolveError(message);
      setRemoteResolvePreview(null);
      toast.push(`${t.remoteResolveTitle}: ${message}`, 'error');
    } finally {
      setRemoteResolveLoading(false);
    }
  };

  const updateRemoteImportStrategy = (policy: 'merge' | 'skip' | 'create') => {
    setRemoteImportStrategy(policy);
    setRemoteResolvePreview((prev) => {
      if (!prev) return prev;
      return { ...prev, items: prev.items.map((item) => ({ ...item, selectedPolicy: policy })) };
    });
  };

  const commitRemoteImport = async () => {
    if (!remoteResolvePreview) return;
    vault.setBusy(true);
    setRemoteResolveError('');
    try {
      const result = await invoke<LiteratureImportResult>('import_by_identifier', {
        identifier: identifierInput.trim(),
        strategy: remoteImportStrategy,
      });
      setRemoteResolvePreview(null);
      setIdentifierInput('');
      setImportResult(result);
      const request = vault.captureVaultRequest();
      await load(request);
      toast.push(t.importResultTitle, 'success');
    } catch (e: any) {
      const message = String(e?.message ?? e);
      setRemoteResolveError(message);
      toast.push(`${t.importFailed}: ${message}`, 'error');
    } finally {
      vault.setBusy(false);
    }
  };

  const clearRemoteCache = async () => {
    if (!vault.vaultPath) return;
    try {
      await invoke('clear_remote_metadata_cache', { vaultPath: vault.vaultPath });
      toast.push(t.remoteCacheCleared, 'success');
    } catch (e: any) {
      toast.push(`${t.remoteCacheClearFailed}: ${e?.message ?? e}`, 'error');
    }
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
      const records = await invoke<OpenAlexWorkCandidate[]>('list_local_openalex_works', {
        rawSourcesDir: openAlexWorksDir,
        query: openAlexQuery.trim(),
        limit: 20,
      });
      setOpenAlexCandidates(Array.isArray(records) ? records : []);
    } catch (e: any) {
      const message = String(e?.message ?? e);
      setOpenAlexError(message);
      toast.push(`${t.openAlexWorksTitle}: ${message}`, 'error');
    } finally {
      setOpenAlexLoading(false);
    }
  };

  const previewOpenAlexWork = async (candidate: OpenAlexWorkCandidate) => {
    setImportError('');
    setImportResult(null);
    vault.setBusy(true);
    try {
      const preview = await invoke<LiteratureImportPreview>('preview_openalex_work', { record: candidate });
      setImportPreview(preview);
      toast.push(t.importPreview, 'info');
    } catch (e: any) {
      const message = String(e?.message ?? e);
      setImportError(message);
      setImportPreview(null);
      toast.push(`${t.importFailed}: ${message}`, 'error');
    } finally {
      vault.setBusy(false);
    }
  };

  return {
    items,
    keyword,
    setKeyword,
    filteredItems,
    editingId,
    editorOpen,
    draft,
    beginNew,
    beginEdit,
    closeEditor,
    updateDraft,
    save,
    deleteId,
    requestDelete,
    confirmDelete,
    toggleFavorite,
    fileInputRef,
    importFormat,
    setImportFormat,
    importPreview,
    importError,
    importResult,
    inspectImport,
    handleFileSelected,
    updateImportPolicy,
    commitImport,
    clearImportResult: () => setImportResult(null),
    dismissImportPreview: () => setImportPreview(null),
    identifierInput,
    setIdentifierInput,
    remoteResolveLoading,
    remoteResolveError,
    setRemoteResolveError,
    remoteResolvePreview,
    setRemoteResolvePreview,
    remoteImportStrategy,
    updateRemoteImportStrategy,
    resolveRemoteMetadata,
    commitRemoteImport,
    clearRemoteCache,
    openAlexWorksDir,
    setOpenAlexWorksDir,
    openAlexQuery,
    setOpenAlexQuery,
    openAlexCandidates,
    openAlexLoading,
    openAlexError,
    pickOpenAlexWorksDir,
    searchOpenAlexWorks,
    previewOpenAlexWork,
    loadItems,
    load,
    reloadAssets,
    editingItem,
  } as LiteratureState;
}
