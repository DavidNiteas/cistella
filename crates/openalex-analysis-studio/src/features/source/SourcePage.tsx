import { useEffect, useMemo, useState } from 'react';
import { Download, RefreshCw } from 'lucide-react';
import ReactECharts from 'echarts-for-react';
import { Button, Card, CardHeader, DataTable, EmptyState, ErrorBanner, Field, Icon, Input, PageHeader, Select } from '../../components/ui';
import styles from './SourcePage.module.css';
import { useChartTheme } from '../../hooks/useChartTheme';
import { dir, queryLabel, short } from '../../lib/utils';
import type { Dict } from '../../lib/i18n/dict';
import type { Row, VaultConnection } from '../../types';
import type { SourceAnalysisState } from '../../hooks/useSourceAnalysis';
import type { VaultContextValue } from '../../hooks/useVaultContext';
import { fmt } from '../../lib/utils';

export interface SourcePageProps {
  vault: VaultConnection;
  source: SourceAnalysisState;
  context: VaultContextValue;
  t: Dict;
  lang: 'zh' | 'en';
  onConnect: (path: string) => void;
}

export function SourcePage({ vault, source, context, t, lang, onConnect }: SourcePageProps) {
  const chartTheme = useChartTheme();
  const [sourceTab, setSourceTab] = useState<import('../../types').SourceTab>(() => {
    const value = localStorage.getItem('sourceTab');
    return value === 'table' || value === 'visual' || value === 'metrics' || value === 'export' ? value : 'overview';
  });

  useEffect(() => {
    localStorage.setItem('sourceTab', sourceTab);
  }, [sourceTab]);

  const tabs: import('../../types').SourceTab[] = ['overview', 'table', 'visual', 'metrics', 'export'];

  return (
    <section className="page">
      <Card>
        <PageHeader
          title={t.sourceTitle}
          description={t.sourceDesc}
          actions={
            !vault.hasVault ? (
              <Button onClick={async () => { const path = await dir(); if (path) await onConnect(path); }} disabled={vault.busy}>
                {t.openForAnalysis}
              </Button>
            ) : undefined
          }
        />
        {vault.hasVault ? (
          <p>{context.summary?.source?.name ? `${context.summary.source.name} · ${context.summary.source.entity ?? 'source'} · ${context.summary.vault_id ?? 'library'}` : t.noData}</p>
        ) : (
          <p>{t.connectForAnalysis}</p>
        )}
      </Card>

      <Card className="compact">
        <CardHeader title={t.currentVault} />
        <div className="sourceRow">
          <div className="path">{short(vault.vaultPath)}</div>
          <Button variant="secondary" onClick={() => vault.connect(vault.vaultPath)} disabled={vault.busy || !vault.vaultPath}>
            {t.connect}
          </Button>
          <Button variant="secondary" onClick={() => context.refresh()} disabled={vault.busy || !vault.vaultPath}>
            <Icon icon={RefreshCw} size={14} /> {t.refreshVault}
          </Button>
        </div>
        <small>{context.summary?.vault_id ? `${t.vaultId}: ${context.summary.vault_id}` : t.noData}</small>
        <small>{context.tableCount != null ? `tables: ${context.tableCount}` : t.noData}</small>
        <small>{context.summary?.source?.input_path ? short(context.summary.source.input_path) : t.noData}</small>
      </Card>

      {source.analysisError && (
        <Card className="">
          <CardHeader title={t.analysisFailed} />
          <ErrorBanner>{source.analysisError}</ErrorBanner>
          <div className="actions">
            <Button variant="secondary" onClick={() => void source.refresh(source.appliedQuery)} disabled={vault.busy || !vault.hasVault}>
              {lang === 'zh' ? '重试分析' : 'Retry analysis'}
            </Button>
          </div>
        </Card>
      )}

      <div className={`${styles.tabs} `}>
        {tabs.map((k) => (
          <button key={k} className={sourceTab === k ? styles.active : ''} onClick={() => setSourceTab(k)}>
            {t[k]}
          </button>
        ))}
      </div>

      {sourceTab === 'overview' && (
        <SourceOverview t={t} lang={lang} hasVault={vault.hasVault} overview={source.overview} adapters={source.adapters} currentQueryText={source.currentQueryText} queryDirty={source.queryDirty} recentQueries={source.recentQueries} onReset={() => void source.reset()} onRefresh={() => void source.refresh(source.currentQuery)} onReplay={(q) => { source.setSourceType(q.sourceType); source.setMetric(q.metric); source.setText(q.text); source.setCountry(q.country); source.setOaFilter(q.oaFilter); void source.refresh(q); }} />
      )}

      {sourceTab === 'table' && (
        <>
          {!vault.hasVault ? (
            <Card className="">
              <CardHeader title={t.connectForAnalysis} />
              <EmptyState title={t.noData} />
            </Card>
          ) : (
            <>
              <SourceControls t={t} source={source} busy={vault.busy} />
              <Card className="">
                <CardHeader title={lang === 'zh' ? '当前筛选摘要' : 'Current query summary'} />
                <p>{source.currentQueryText}</p>
                {source.queryDirty && <small>{t.queryPending}</small>}
                <p>{lang === 'zh' ? `检索结果 ${source.resultCount} 条` : `${source.resultCount} results`}</p>
                <div className="actions">
                  <Button variant="secondary" onClick={() => void source.reset()} disabled={vault.busy}>
                    {lang === 'zh' ? '重置筛选' : 'Reset filters'}
                  </Button>
                  <Button variant="secondary" onClick={() => void source.refresh(source.currentQuery)} disabled={vault.busy || !source.overview}>
                    {lang === 'zh' ? '重新查询' : 'Rerun query'}
                  </Button>
                </div>
              </Card>
              <Card className="">
                <CardHeader title={t.recentQueries} />
                <div className={styles.recent}>
                  {source.recentQueries.length ? (
                    source.recentQueries.map((q, i) => (
                      <button key={`${q.sourceType}-${i}`} onClick={() => { source.setSourceType(q.sourceType); source.setMetric(q.metric); source.setText(q.text); source.setCountry(q.country); source.setOaFilter(q.oaFilter); void source.refresh(q); }}>
                        {queryLabel(q, lang)}
                      </button>
                    ))
                  ) : (
                    <EmptyState title={t.noRecentQueries} />
                  )}
                </div>
              </Card>
              <SourceTable rows={source.results} lang={lang} t={t} />
            </>
          )}
        </>
      )}

      {sourceTab === 'visual' && (
        <>
          <Card className="">
            <CardHeader
              title={t.ranking}
              action={<Button variant="secondary" onClick={() => void source.exportRows('top')} disabled={!vault.hasVault || !source.overview || Boolean(source.analysisError) || vault.busy}><Icon icon={Download} size={14} /> {t.exportRank}</Button>}
            />
            <p>{lang === 'zh' ? `排行结果 ${source.rankingCount} 条` : `${source.rankingCount} ranked items`}</p>
            <p>{source.currentQueryText}</p>
            {source.queryDirty && <small>{t.queryPending}</small>}
          </Card>
          <Card className="">
            <CardHeader title={t.chartRanking} />
            <ReactECharts
              option={useMemo(() => ({
                backgroundColor: chartTheme.backgroundColor,
                xAxis: {
                  type: 'category',
                  data: source.rankingChartData.map((r) => r.name),
                  axisLine: { lineStyle: { color: chartTheme.axisColor } },
                  axisLabel: { color: chartTheme.textColor, rotate: 45, interval: 0 },
                },
                yAxis: {
                  type: 'value',
                  axisLine: { lineStyle: { color: chartTheme.axisColor } },
                  axisLabel: { color: chartTheme.textColor },
                  splitLine: { lineStyle: { color: chartTheme.splitLineColor } },
                },
                series: [{ type: 'bar', data: source.rankingChartData.map((r) => r.value) }],
                grid: { left: 50, right: 20, top: 20, bottom: 100 },
              }), [source.rankingChartData, chartTheme])}
              style={{ height: 360 }}
            />
          </Card>
        </>
      )}

      {sourceTab === 'metrics' && (
        <Card className={`${styles.metricGrid}`}>
          <CardHeader title={t.metricGuide} />
          {[t.hTip, t.citedTip, t.worksTip, t.i10Tip, t.meanTip].map((x, i) => <p key={i}>{x}</p>)}
        </Card>
      )}

      {sourceTab === 'export' && (
        <Card className="">
          <CardHeader title={t.export} />
          <p>{lang === 'zh' ? '导出当前已执行的筛选或排行结果。' : 'Export the currently executed ranking or search result.'}</p>
          <p>{source.currentQueryText}</p>
          {source.queryDirty && <small>{t.queryPending}</small>}
          <p>{lang === 'zh' ? `检索结果 ${source.resultCount} 条，排行结果 ${source.rankingCount} 条` : `${source.resultCount} search results, ${source.rankingCount} ranked items`}</p>
          <div className="actions">
            <Button onClick={() => void source.exportRows('top')} disabled={vault.busy || !source.overview || Boolean(source.analysisError)}>
              <Icon icon={Download} size={14} /> {t.exportRank}
            </Button>
            <Button variant="secondary" onClick={() => void source.exportRows('search')} disabled={vault.busy || !source.overview || Boolean(source.analysisError)}>
              <Icon icon={Download} size={14} /> {t.exportSearch}
            </Button>
          </div>
        </Card>
      )}
    </section>
  );
}

function SourceOverview({ t, lang, hasVault, overview, adapters, currentQueryText, queryDirty, recentQueries, onReset, onRefresh, onReplay }: {
  t: Dict;
  lang: 'zh' | 'en';
  hasVault: boolean;
  overview: import('../../types').Overview | null;
  adapters: import('../../types').Adapter[];
  currentQueryText: string;
  queryDirty: boolean;
  recentQueries: import('../../types').QuerySnapshot[];
  onReset: () => void;
  onRefresh: () => void;
  onReplay: (q: import('../../types').QuerySnapshot) => void;
}) {
  return (
    <>
      {!hasVault && (
        <Card className="">
          <CardHeader title={t.connectForAnalysis} />
          <EmptyState title={t.noData} />
        </Card>
      )}
      {hasVault && overview && (
        <Stats t={t} overview={overview} />
      )}
      <Card className="">
        <CardHeader title={t.layout} />
        <p>{t.parquet}</p>
        <p>{t.arrow}</p>
        <p>{t.manifest}</p>
      </Card>
      <Card className="">
        <CardHeader title={lang === 'zh' ? '来源适配器' : 'Source adapters'} />
        <p>{lang === 'zh' ? '当前可用的来源适配器列表。' : 'Available source adapters.'}</p>
        <small>{adapters.map((a) => `${a.name}${a.is_default ? ' · 默认' : ''}`).join(' | ')}</small>
      </Card>
      <Card className="">
        <CardHeader title={lang === 'zh' ? '当前筛选摘要' : 'Current query summary'} />
        <p>{currentQueryText}</p>
        {queryDirty && <small>{t.queryPending}</small>}
        <div className="actions">
          <Button variant="secondary" onClick={onReset}>
            {lang === 'zh' ? '重置筛选' : 'Reset filters'}
          </Button>
          <Button variant="secondary" onClick={onRefresh}>
            {lang === 'zh' ? '重新查询' : 'Rerun query'}
          </Button>
        </div>
      </Card>
      <Card className="">
        <CardHeader title={t.recentQueries} />
        <div className={styles.recent}>
          {recentQueries.length ? (
            recentQueries.map((q, i) => (
              <button key={`${q.sourceType}-${i}`} onClick={() => onReplay(q)}>
                {queryLabel(q, lang)}
              </button>
            ))
          ) : (
            <EmptyState title={t.noRecentQueries} />
          )}
        </div>
      </Card>
    </>
  );
}

function SourceControls({ t, source, busy }: { t: Dict; source: SourceAnalysisState; busy: boolean }) {
  return (
    <Card className="controls">
      <Field label={t.type}>
        <Select value={source.sourceType} onChange={(e) => source.setSourceType(e.target.value)} options={[
          { value: 'journal', label: t.journal },
          { value: 'conference', label: t.conference },
        ]} />
      </Field>
      <Field label={t.metric}>
        <Select value={source.metric} onChange={(e) => source.setMetric(e.target.value)} options={t.metricOptions.map(([value, label]) => ({ value, label }))} />
      </Field>
      <Field label={t.keyword}>
        <Input value={source.text} onChange={(e) => source.setText(e.target.value)} onKeyDown={(e) => { if (e.key === 'Enter') void source.refresh(); }} />
      </Field>
      <Field label={t.country}>
        <Input value={source.country} onChange={(e) => source.setCountry(e.target.value.toUpperCase())} onKeyDown={(e) => { if (e.key === 'Enter') void source.refresh(); }} />
      </Field>
      <Field label={t.oaFilter}>
        <Select value={source.oaFilter} onChange={(e) => source.setOaFilter(e.target.value as 'all' | 'oa' | 'non_oa')} options={[
          { value: 'all', label: t.all },
          { value: 'oa', label: t.onlyOa },
          { value: 'non_oa', label: t.onlyNonOa },
        ]} />
      </Field>
      <Button onClick={() => void source.refresh()} disabled={busy || !source.overview} loading={busy}>
        {t.run}
      </Button>
    </Card>
  );
}

function SourceTable({ rows, lang, t }: { rows: Row[]; lang: 'zh' | 'en'; t: Dict }) {
  const columns = [
    { key: 'display_name', title: lang === 'zh' ? '名称' : 'Name', width: 240, sortable: true },
    { key: 'source_type', title: t.type, width: 120, sortable: true },
    { key: 'country_code', title: t.country, width: 100, sortable: true },
    { key: 'h_index', title: 'H-index', width: 100, sortable: true },
    { key: 'i10_index', title: 'i10-index', width: 100, sortable: true },
    { key: 'cited_by_count', title: t.cited, width: 120, sortable: true },
    { key: 'works_count', title: t.works, width: 120, sortable: true },
    { key: 'mean_citedness_2yr', title: t.meanTip, width: 140, sortable: true },
  ];
  return <DataTable columns={columns} rows={rows} emptyTitle={lang === 'zh' ? '无结果' : 'No results'} pageSize={15} />;
}

function Stats({ t, overview }: { t: Dict; overview: import('../../types').Overview | null }) {
  return (
    <div className={`${styles.stats} `}>
      {[
        [overview?.source_count, t.sources],
        [overview?.journal_count, t.journals],
        [overview?.conference_count, t.conferences],
        [overview?.oa_count, t.oa],
        [overview?.works_count, t.works],
        [overview?.cited_by_count, t.cited],
      ].map(([v, l]) => (
        <div className={styles.mini} key={String(l)}>
          <span>{l}</span>
          <b>{fmt(v)}</b>
        </div>
      ))}
    </div>
  );
}
