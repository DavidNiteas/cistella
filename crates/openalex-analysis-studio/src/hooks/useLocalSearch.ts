import { useEffect, useState } from 'react';
import type { Dict } from '../lib/i18n/dict';
import type { Lang, LiteratureItem, LocalSearchIssues, LocalSearchOutcome, LocalSearchTask, VaultConnection, VaultRequestContext } from '../types';
import { invoke } from '../lib/invoke';
import { createLocalSearchTaskPoller } from '../localSearchPolling';
import { useToast } from '../components/ui/Toast/ToastProvider';

export interface LocalSearchState {
  text: string;
  setText: (value: string) => void;
  scope: 'all' | 'title' | 'authors' | 'tags' | 'content';
  setScope: (value: 'all' | 'title' | 'authors' | 'tags' | 'content') => void;
  offset: number;
  outcome: LocalSearchOutcome | null;
  indexState: import('../types').LocalSearchIndexState | null;
  task: LocalSearchTask | null;
  issues: LocalSearchIssues | null;
  loading: boolean;
  error: string;
  refreshHealth: (request?: VaultRequestContext) => Promise<LocalSearchTask | null>;
  runSearch: (offset?: number, request?: VaultRequestContext) => Promise<void>;
  loadIssues: (request?: VaultRequestContext) => Promise<void>;
  runTask: (command: 'synchronize_local_search_index' | 'rebuild_local_search_index') => Promise<void>;
  cancelTask: () => Promise<void>;
  openHitAsset: (itemId: string, assetId: string) => void;
}

export function useLocalSearch(vault: VaultConnection, items: LiteratureItem[], t: Dict, lang: Lang, active: boolean): LocalSearchState {
  const [text, setText] = useState('');
  const [scope, setScope] = useState<'all' | 'title' | 'authors' | 'tags' | 'content'>('all');
  const [offset, setOffset] = useState(0);
  const [outcome, setOutcome] = useState<LocalSearchOutcome | null>(null);
  const [indexState, setIndexState] = useState<import('../types').LocalSearchIndexState | null>(null);
  const [task, setTask] = useState<LocalSearchTask | null>(null);
  const [issues, setIssues] = useState<LocalSearchIssues | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState('');
  const toast = useToast();

  const clear = () => {
    setOutcome(null);
    setIndexState(null);
    setTask(null);
    setIssues(null);
    setOffset(0);
    setLoading(false);
    setError('');
  };

  useEffect(() => {
    if (!vault.hasVault) {
      clear();
    }
  }, [vault.hasVault, vault.vaultPath]);

  const refreshHealth = async (request?: VaultRequestContext): Promise<LocalSearchTask | null> => {
    const req = request ?? vault.captureVaultRequest();
    if (!req.expectedVaultPath || !vault.isCurrentVaultRequest(req)) return null;
    try {
      const [nextIndexState, nextTaskState] = await Promise.all([
        invoke<import('../types').LocalSearchIndexState>('local_search_index_state'),
        invoke<LocalSearchTask>('local_search_task_state'),
      ]);
      if (!vault.isCurrentVaultRequest(req)) return null;
      setIndexState(nextIndexState);
      setTask(nextTaskState);
      return nextTaskState;
    } catch (e: any) {
      if (vault.isCurrentVaultRequest(req)) {
        const message = String(e?.message ?? e);
        setError(message);
        toast.push(`${t.failed}: ${message}`, 'error');
      }
      return null;
    }
  };

  const runSearch = async (nextOffset = 0, request?: VaultRequestContext) => {
    const req = request ?? vault.captureVaultRequest();
    if (!req.expectedVaultPath || !vault.isCurrentVaultRequest(req)) return;
    setLoading(true);
    setError('');
    try {
      const result = await invoke<LocalSearchOutcome>('local_search', {
        req: { text, scopes: [scope], offset: nextOffset, limit: 50 },
      });
      if (!vault.isCurrentVaultRequest(req)) return;
      setOutcome(result);
      setOffset(result.outcome === 'ready' ? result.page.offset : 0);
      setIndexState(result.outcome === 'ready' ? result.page.indexState : result.indexState);
      if (result.outcome === 'ready') {
        toast.push(`${result.page.totalHits} ${t.results}`, 'info');
      }
    } catch (e: any) {
      if (vault.isCurrentVaultRequest(req)) {
        const message = String(e?.message ?? e);
        setError(message);
        toast.push(`${t.failed}: ${message}`, 'error');
      }
    } finally {
      if (vault.isCurrentVaultRequest(req)) setLoading(false);
    }
  };

  const loadIssues = async (request?: VaultRequestContext) => {
    const req = request ?? vault.captureVaultRequest();
    if (!req.expectedVaultPath || !vault.isCurrentVaultRequest(req)) return;
    try {
      const result = await invoke<LocalSearchIssues>('local_search_index_issues');
      if (vault.isCurrentVaultRequest(req)) setIssues(result);
    } catch (e: any) {
      if (vault.isCurrentVaultRequest(req)) setError(String(e?.message ?? e));
    }
  };

  const runTask = async (command: 'synchronize_local_search_index' | 'rebuild_local_search_index') => {
    const request = vault.captureVaultRequest();
    if (!request.expectedVaultPath || !vault.isCurrentVaultRequest(request)) return;
    setError('');
    try {
      const nextTask = await invoke<LocalSearchTask>(command, { req: request });
      if (!vault.isCurrentVaultRequest(request)) return;
      setTask(nextTask);
      setIndexState({ status: 'building', activeGeneration: indexState?.activeGeneration ?? null, detail: nextTask.detail ?? null });
      toast.push(t.busy, 'info');
    } catch (e: any) {
      if (vault.isCurrentVaultRequest(request)) {
        const message = String(e?.message ?? e);
        setError(message);
        toast.push(`${t.failed}: ${message}`, 'error');
      }
    }
  };

  const cancelTask = async () => {
    const request = vault.captureVaultRequest();
    if (!request.expectedVaultPath || !vault.isCurrentVaultRequest(request)) return;
    try {
      const nextTask = await invoke<LocalSearchTask>('cancel_local_search_index_task', { req: request });
      if (vault.isCurrentVaultRequest(request)) {
        setTask(nextTask);
        toast.push(t.cancel, 'info');
      }
    } catch (e: any) {
      if (vault.isCurrentVaultRequest(request)) {
        const message = String(e?.message ?? e);
        setError(message);
        toast.push(`${t.failed}: ${message}`, 'error');
      }
    }
  };

  const openHitAsset = (itemId: string, assetId: string) => {
    void (async () => {
      const request = vault.captureVaultRequest();
      if (!vault.isCurrentVaultRequest(request)) return;
      try {
        await invoke('open_document_asset', { itemId, assetId });
        setError('');
        toast.push(t.fileRequestAccepted, 'info');
      } catch (e: any) {
        if (vault.isCurrentVaultRequest(request)) {
          const message = String(e?.message ?? e);
          setError(message);
          toast.push(`${t.failed}: ${message}`, 'error');
        }
      }
    })();
  };

  // Poll task while building and search workspace is active.
  useEffect(() => {
    if (!active || !vault.hasVault || task?.status !== 'building') return;
    const request = vault.captureVaultRequest();
    return createLocalSearchTaskPoller({
      request,
      isCurrent: vault.isCurrentVaultRequest,
      refresh: refreshHealth,
      onTerminal: async (terminalRequest) => {
        if (!vault.isCurrentVaultRequest(terminalRequest)) return;
        const terminalTask = await refreshHealth(terminalRequest);
        await loadIssues(terminalRequest);
        if (terminalTask?.status === 'succeeded') {
          toast.push(t.ready, 'success');
        } else if (terminalTask?.status === 'failed') {
          toast.push(`${t.failed}: ${terminalTask.detail ?? t.failed}`, 'error');
        }
      },
    });
  }, [active, vault.hasVault, task?.status, vault.vaultPath]);

  return {
    text,
    setText,
    scope,
    setScope,
    offset,
    outcome,
    indexState,
    task,
    issues,
    loading,
    error,
    refreshHealth,
    runSearch,
    loadIssues,
    runTask,
    cancelTask,
    openHitAsset,
  };
}
