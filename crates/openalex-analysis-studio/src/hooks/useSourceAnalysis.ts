import { useEffect, useMemo, useState } from 'react';
import { save } from '@tauri-apps/plugin-dialog';
import type { Dict } from '../lib/i18n/dict';
import type { Adapter, Lang, Overview, QuerySnapshot, Row, VaultConnection, VaultRequestContext } from '../types';
import { invoke } from '../lib/invoke';
import { first, loadRecentQueries, queryLabel, rememberQuery, rows, short } from '../lib/utils';
import { useToast } from '../components/ui/Toast/ToastProvider';

export interface SourceAnalysisState {
  adapters: Adapter[];
  overview: Overview | null;
  analysisError: string;
  ranking: Row[];
  results: Row[];
  recentQueries: QuerySnapshot[];
  sourceType: string;
  setSourceType: (value: string) => void;
  metric: string;
  setMetric: (value: string) => void;
  text: string;
  setText: (value: string) => void;
  country: string;
  setCountry: (value: string) => void;
  oaFilter: 'all' | 'oa' | 'non_oa';
  setOaFilter: (value: 'all' | 'oa' | 'non_oa') => void;
  appliedQuery: QuerySnapshot;
  currentQuery: QuerySnapshot;
  queryDirty: boolean;
  currentQueryText: string;
  resultCount: number;
  rankingCount: number;
  refresh: (snapshot?: QuerySnapshot, vaultReady?: boolean, request?: VaultRequestContext) => Promise<boolean>;
  reset: () => Promise<void>;
  exportRows: (kind: 'top' | 'search') => Promise<void>;
  rankingChartData: { name: string; value: number }[];
}

export function useSourceAnalysis(vault: VaultConnection, t: Dict, lang: Lang): SourceAnalysisState {
  const [adapters, setAdapters] = useState<Adapter[]>([{ name: 'OpenAlex', kind: 'source-import', is_default: true }]);
  const [overview, setOverview] = useState<Overview | null>(null);
  const [analysisError, setAnalysisError] = useState('');
  const [ranking, setRanking] = useState<Row[]>([]);
  const [results, setResults] = useState<Row[]>([]);
  const [recentQueries, setRecentQueries] = useState<QuerySnapshot[]>(loadRecentQueries());
  const toast = useToast();

  const [sourceType, setSourceType] = useState('journal');
  const [metric, setMetric] = useState('h_index');
  const [text, setText] = useState('');
  const [country, setCountry] = useState('');
  const [oaFilter, setOaFilter] = useState<'all' | 'oa' | 'non_oa'>('all');
  const [appliedQuery, setAppliedQuery] = useState<QuerySnapshot>({
    sourceType: 'journal',
    metric: 'h_index',
    text: '',
    country: '',
    oaFilter: 'all',
  });

  const currentQuery = useMemo(
    () => ({ sourceType, metric, text, country, oaFilter }),
    [sourceType, metric, text, country, oaFilter]
  );
  const currentQueryText = useMemo(() => queryLabel(appliedQuery, lang), [appliedQuery, lang]);
  const queryDirty = useMemo(() => JSON.stringify(currentQuery) !== JSON.stringify(appliedQuery), [currentQuery, appliedQuery]);
  const resultCount = results.length;
  const rankingCount = ranking.length;

  const rankingChartData = useMemo(
    () =>
      ranking.slice(0, 10).map((r) => ({
        name: String(r.display_name ?? r.openalex_id ?? '—'),
        value: Number(r.metric_value ?? r[appliedQuery.metric] ?? 0),
      })),
    [ranking, appliedQuery.metric]
  );

  useEffect(() => {
    const loadAdapters = async () => {
      try {
        const value = await invoke<unknown[]>('source_adapters');
        setAdapters(
          rows(value).map((v) => ({
            name: String(v.name ?? 'Unknown'),
            kind: String(v.kind ?? 'source-import'),
            is_default: Boolean(v.is_default),
          }))
        );
      } catch {
        /* keep fallback */
      }
    };
    void loadAdapters();
  }, []);

  const refresh = async (
    snapshot?: QuerySnapshot,
    vaultReady = vault.hasVault,
    request: VaultRequestContext = vault.captureVaultRequest()
  ): Promise<boolean> => {
    if (!vaultReady || !request.expectedVaultPath) {
      if (!vault.isCurrentVaultRequest(request)) return false;
      setOverview(null);
      setRanking([]);
      setResults([]);
      setAnalysisError('');
      vault.setStatus(t.connectForAnalysis);
      return false;
    }
    const current = snapshot ?? currentQuery;
    try {
      const [nextOverview, nextRanking, nextResults] = await Promise.all([
        invoke<unknown>('library_overview'),
        invoke<unknown>('top_sources', {
          metric: current.metric,
          sourceType: current.sourceType,
          limit: 30,
        }),
        invoke<unknown>('search_sources', {
          req: {
            text: current.text || null,
            sourceType: current.sourceType,
            countryCode: current.country || null,
            isOa: current.oaFilter === 'all' ? null : current.oaFilter === 'oa',
            limit: 100,
            offset: 0,
          },
        }),
      ]);
      if (!vault.isCurrentVaultRequest(request)) return false;
      setOverview(first(nextOverview));
      setRanking(rows(nextRanking));
      setResults(rows(nextResults));
      setAppliedQuery(current);
      setRecentQueries(rememberQuery(current));
      setAnalysisError('');
      toast.push(t.run, 'success');
      return true;
    } catch (e: any) {
      if (!vault.isCurrentVaultRequest(request)) return false;
      const message = String(e?.message ?? e);
      setOverview(null);
      setRanking([]);
      setResults([]);
      setAnalysisError(message);
      const status = `${t.failed}: ${message}`;
      vault.setStatus(status);
      toast.push(status, 'error');
      return false;
    }
  };

  const reset = async () => {
    setSourceType('journal');
    setMetric('h_index');
    setText('');
    setCountry('');
    setOaFilter('all');
    await refresh({ sourceType: 'journal', metric: 'h_index', text: '', country: '', oaFilter: 'all' });
  };

  const exportRows = async (kind: 'top' | 'search') => {
    await vault.run(async () => {
      if (!vault.hasVault || !overview || analysisError) {
        vault.setStatus(analysisError ? `${t.failed}: ${analysisError}` : t.connectForAnalysis);
        return;
      }
      const p = await save({
        defaultPath: kind === 'top' ? 'cistella-ranking.csv' : 'cistella-results.csv',
        filters: [{ name: 'CSV / Excel', extensions: ['csv', 'xlsx'] }],
      });
      if (!p) return;
      if (kind === 'top') {
        await invoke('export_top_sources', {
          output: p,
          metric: appliedQuery.metric,
          sourceType: appliedQuery.sourceType,
          limit: ranking.length,
        });
      } else {
        await invoke('export_search_sources', {
          output: p,
          req: {
            text: appliedQuery.text || null,
            sourceType: appliedQuery.sourceType,
            countryCode: appliedQuery.country || null,
            isOa: appliedQuery.oaFilter === 'all' ? null : appliedQuery.oaFilter === 'oa',
            limit: results.length,
            offset: 0,
          },
        });
      }
      const status = `${kind === 'top' ? t.exportRank : t.exportSearch}: ${short(p)}`;
      vault.setStatus(status);
      toast.push(status, 'success');
    });
  };

  return {
    adapters,
    overview,
    analysisError,
    ranking,
    results,
    recentQueries,
    sourceType,
    setSourceType,
    metric,
    setMetric,
    text,
    setText,
    country,
    setCountry,
    oaFilter,
    setOaFilter,
    appliedQuery,
    currentQuery,
    queryDirty,
    currentQueryText,
    resultCount,
    rankingCount,
    refresh,
    reset,
    exportRows,
    rankingChartData,
  };
}
