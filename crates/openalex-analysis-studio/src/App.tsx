import { invoke } from '@tauri-apps/api/core';
import { open, save } from '@tauri-apps/plugin-dialog';
import ReactECharts from 'echarts-for-react';
import { useMemo, useState } from 'react';
import './style.css';

type JsonRow = Record<string, unknown>;
type View = 'build' | 'search' | 'settings';
type Lang = 'zh' | 'en';
type Overview = { source_count?: number; journal_count?: number; conference_count?: number; oa_count?: number; works_count?: number; cited_by_count?: number };
type Dict = typeof dict.zh;

const dict = {
  zh: {
    brand: 'OpenAlex Studio', eyebrow: '学术影响力分析', workspace: '工作区', buildNav: '建库', searchNav: '搜索分析', settings: '设置',
    ready: '就绪', running: '处理中', failed: '操作失败', chooseFirst: '请先选择路径。',
    buildTitle: '一页完成建库', buildDesc: '选择原始数据和输出位置，生成 Parquet 压缩库与 Arrow 快速读取库。',
    raw: '原始 OpenAlex Sources', output: '输出分析库', chooseDir: '选择目录', buildArrow: '生成 Arrow 快速读取布局', startBuild: '开始建库', openAfterBuild: '建库完成后打开', imported: '建库完成',
    buildChecklist: '建库检查', checkRaw: '原始目录应包含 OpenAlex Sources parquet 分片', checkOutput: '输出目录会写入 manifest.json、parquet/、arrow/', checkArrow: '大库推荐生成 Arrow，搜索体验更好',
    storageTitle: '数据布局', parquet: 'Parquet：压缩、打包、传输', arrow: 'Arrow：本地快速读取、面向 mmap', manifest: 'Manifest：记录 schema 与物理文件',
    searchTitle: '一页完成连接、检索与导出', searchDesc: '连接外部库目录或单个 Arrow / Parquet 文件，进行指标排行、检索和结果导出。',
    dataSource: '数据源', chooseLibrary: '选择库目录', chooseFile: '选择 Arrow/Parquet', connect: '连接', connected: '已连接', noData: '未连接数据源',
    sources: 'Sources', journals: '期刊', conferences: '会议', oa: 'OA 来源', works: 'Works', cited: '总被引',
    type: '类型', journal: '期刊', conference: '会议', metric: '排行指标', keyword: '名称关键词', country: '国家代码', oaFilter: 'OA 筛选', all: '全部', onlyOa: '仅 OA', onlyNonOa: '非 OA', query: '查询',
    ranking: '影响力排行', results: '检索结果', exportRanking: '导出排行', exportSearch: '导出检索',
    insight: '学术使用建议', tip1: 'H-index 更稳定，适合综合影响力初筛。', tip2: '两年平均被引更适合观察近期热度。', tip3: '会议与期刊应分开比较，避免指标语境混淆。',
    recent: '最近连接', emptyRecent: '暂无最近路径', language: '界面语言', chinese: '中文', english: 'English',
    metrics: [['h_index','H-index'],['cited_by_count','总被引'],['works_count','作品数'],['i10_index','i10-index'],['mean_citedness_2yr','两年平均被引']]
  },
  en: {
    brand: 'OpenAlex Studio', eyebrow: 'Scholarly impact analytics', workspace: 'Workspace', buildNav: 'Build', searchNav: 'Search', settings: 'Settings',
    ready: 'Ready', running: 'Working', failed: 'Failed', chooseFirst: 'Choose a path first.',
    buildTitle: 'Build in one page', buildDesc: 'Choose raw data and output location, then create compressed Parquet and fast Arrow layouts.',
    raw: 'Raw OpenAlex Sources', output: 'Output analysis library', chooseDir: 'Choose folder', buildArrow: 'Create Arrow fast-read layout', startBuild: 'Build library', openAfterBuild: 'Open after build', imported: 'Library built',
    buildChecklist: 'Build checklist', checkRaw: 'Raw folder should contain OpenAlex Sources parquet parts', checkOutput: 'Output folder will contain manifest.json, parquet/, arrow/', checkArrow: 'Arrow is recommended for large local libraries',
    storageTitle: 'Data layout', parquet: 'Parquet: compressed, package, transfer', arrow: 'Arrow: fast local reads, mmap-oriented', manifest: 'Manifest: schema and physical file map',
    searchTitle: 'Connect, search and export in one page', searchDesc: 'Connect an external library folder or a single Arrow / Parquet file for ranking, search and export.',
    dataSource: 'Data source', chooseLibrary: 'Choose library', chooseFile: 'Choose Arrow/Parquet', connect: 'Connect', connected: 'Connected', noData: 'No data source',
    sources: 'Sources', journals: 'Journals', conferences: 'Conferences', oa: 'OA sources', works: 'Works', cited: 'Citations',
    type: 'Type', journal: 'Journal', conference: 'Conference', metric: 'Ranking metric', keyword: 'Name keyword', country: 'Country code', oaFilter: 'OA filter', all: 'All', onlyOa: 'OA only', onlyNonOa: 'Non-OA', query: 'Run',
    ranking: 'Impact ranking', results: 'Search results', exportRanking: 'Export ranking', exportSearch: 'Export search',
    insight: 'Research workflow tips', tip1: 'H-index is stable for first-pass impact screening.', tip2: '2-year mean citedness is better for recent momentum.', tip3: 'Compare journals and conferences separately.',
    recent: 'Recent sources', emptyRecent: 'No recent paths', language: 'Language', chinese: '中文', english: 'English',
    metrics: [['h_index','H-index'],['cited_by_count','Cited by count'],['works_count','Works count'],['i10_index','i10-index'],['mean_citedness_2yr','2-year mean citedness']]
  }
};

function first(v: unknown): Overview { return (Array.isArray(v) ? v[0] : v || {}) as Overview; }
function rows(v: unknown): JsonRow[] { return Array.isArray(v) ? v as JsonRow[] : []; }
function fmt(v: unknown) { return typeof v === 'number' ? Math.round(v).toLocaleString() : v == null || v === '' ? '—' : String(v); }
function shortPath(p: string) { if (!p) return '—'; return p.length > 76 ? `…${p.slice(-73)}` : p; }
function loadRecent(): string[] { try { return JSON.parse(localStorage.getItem('recentSources') || '[]'); } catch { return []; } }
function pushRecent(path: string) { const next = [path, ...loadRecent().filter((p) => p !== path)].slice(0, 5); localStorage.setItem('recentSources', JSON.stringify(next)); return next; }
async function pickDir() { const v = await open({ directory: true, multiple: false }); return typeof v === 'string' ? v : ''; }
async function pickDataFile() { const v = await open({ multiple: false, filters: [{ name: 'OpenAlex data', extensions: ['arrow', 'ipc', 'parquet'] }] }); return typeof v === 'string' ? v : ''; }

export default function App() {
  const [lang, setLangState] = useState<Lang>((localStorage.getItem('lang') as Lang) === 'en' ? 'en' : 'zh');
  const t = dict[lang];
  const [view, setView] = useState<View>('build');
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState('');
  const [rawDir, setRawDir] = useState('');
  const [libraryDir, setLibraryDir] = useState('');
  const [buildArrow, setBuildArrow] = useState(true);
  const [dataPath, setDataPath] = useState('');
  const [recent, setRecent] = useState<string[]>(loadRecent());
  const [overview, setOverview] = useState<Overview | null>(null);
  const [ranking, setRanking] = useState<JsonRow[]>([]);
  const [results, setResults] = useState<JsonRow[]>([]);
  const [sourceType, setSourceType] = useState('journal');
  const [metric, setMetric] = useState('h_index');
  const [text, setText] = useState('');
  const [country, setCountry] = useState('');
  const [oaFilter, setOaFilter] = useState('all');

  const isOa = oaFilter === 'all' ? null : oaFilter === 'oa';
  const run = async (fn: () => Promise<void>) => { setBusy(true); try { await fn(); } catch (e) { setStatus(`${t.failed}: ${e}`); } finally { setBusy(false); } };
  const refresh = async () => {
    setOverview(first(await invoke('dataset_overview')));
    setRanking(rows(await invoke('top_sources', { metric, sourceType, limit: 24 })));
    setResults(rows(await invoke('search_sources', { req: { text: text || null, sourceType, countryCode: country || null, isOa, limit: 80, offset: 0 } })));
  };
  const connect = async (path: string) => { if (!path) { setStatus(t.chooseFirst); return; } await invoke('connect_dataset', { path }); setDataPath(path); setRecent(pushRecent(path)); await refresh(); setStatus(`${t.connected}: ${path}`); setView('search'); };
  const buildLibrary = () => run(async () => { if (!rawDir || !libraryDir) { setStatus(t.chooseFirst); return; } await invoke('import_sources', { req: { rawSourcesDir: rawDir, outputDir: libraryDir, buildArrowCache: buildArrow } }); setStatus(`${t.imported}: ${libraryDir}`); await connect(libraryDir); });
  const exportRows = (kind: 'top' | 'search') => run(async () => { const path = await save({ defaultPath: kind === 'top' ? 'openalex-ranking.xlsx' : 'openalex-search.xlsx', filters: [{ name: 'CSV / Excel', extensions: ['csv', 'xlsx'] }] }); if (!path) return; if (kind === 'top') await invoke('export_top_sources', { output: path, metric, sourceType, limit: 1000 }); else await invoke('export_search_sources', { output: path, req: { text: text || null, sourceType, countryCode: country || null, isOa, limit: 1000, offset: 0 } }); setStatus(`${kind === 'top' ? t.exportRanking : t.exportSearch}: ${path}`); });
  const changeLang = (next: Lang) => { localStorage.setItem('lang', next); setLangState(next); };

  const chart = useMemo(() => { const top = ranking.slice(0, 12).reverse(); return { grid: { left: 170, right: 20, top: 8, bottom: 22 }, tooltip: { trigger: 'axis' }, xAxis: { type: 'value', splitLine: { lineStyle: { color: '#edf0f7' } } }, yAxis: { type: 'category', data: top.map((r) => r.display_name), axisTick: { show: false }, axisLine: { show: false } }, series: [{ type: 'bar', barWidth: 16, data: top.map((r) => r.metric_value), itemStyle: { color: '#6557f5', borderRadius: [0, 6, 6, 0] } }] }; }, [ranking]);

  return <div className="shell">
    <aside className="side"><div className="brand"><span>OA</span><div><b>{t.brand}</b><small>{t.eyebrow}</small></div></div><em>{t.workspace}</em><Nav active={view === 'build'} onClick={() => setView('build')} icon="▦" label={t.buildNav} /><Nav active={view === 'search'} onClick={() => setView('search')} icon="⌕" label={t.searchNav} /><div className="sideBottom"><Nav active={view === 'settings'} onClick={() => setView('settings')} icon="⚙" label={t.settings} /><small>v0.1.0</small></div></aside>
    <main className="main"><header className="head"><div><span className="crumb">OPENALEX / {view.toUpperCase()}</span><h1>{view === 'build' ? t.buildTitle : view === 'search' ? t.searchTitle : t.settings}</h1><p>{view === 'build' ? t.buildDesc : view === 'search' ? t.searchDesc : t.storageTitle}</p></div><div className={`pill ${busy ? 'busy' : ''}`}><i />{busy ? t.running : t.ready}</div></header>{status && <div className="notice">{shortPath(status)}</div>}
      {view === 'build' && <BuildPage t={t} rawDir={rawDir} libraryDir={libraryDir} setRawDir={setRawDir} setLibraryDir={setLibraryDir} buildArrow={buildArrow} setBuildArrow={setBuildArrow} buildLibrary={buildLibrary} openBuilt={() => run(() => connect(libraryDir))} busy={busy} />}
      {view === 'search' && <SearchPage t={t} dataPath={dataPath} setDataPath={setDataPath} connect={() => run(() => connect(dataPath))} recent={recent} openRecent={(p: string) => run(() => connect(p))} overview={overview} sourceType={sourceType} setSourceType={setSourceType} metric={metric} setMetric={setMetric} text={text} setText={setText} country={country} setCountry={setCountry} oaFilter={oaFilter} setOaFilter={setOaFilter} refresh={() => run(refresh)} ranking={ranking} results={results} chart={chart} exportRows={exportRows} busy={busy} />}
      {view === 'settings' && <section className="settings onepage"><div className="card"><h2>{t.language}</h2><button className={lang === 'zh' ? 'selected' : ''} onClick={() => changeLang('zh')}>{t.chinese}</button><button className={lang === 'en' ? 'selected' : ''} onClick={() => changeLang('en')}>{t.english}</button></div><div className="card"><h2>{t.storageTitle}</h2><p>{t.parquet}</p><p>{t.arrow}</p><p>{t.manifest}</p></div></section>}
    </main>
  </div>;
}

function Nav({ active, onClick, icon, label }: { active: boolean; onClick: () => void; icon: string; label: string }) { return <button className={`nav ${active ? 'active' : ''}`} onClick={onClick}><span>{icon}</span>{label}</button>; }
function BuildPage({ t, rawDir, libraryDir, setRawDir, setLibraryDir, buildArrow, setBuildArrow, buildLibrary, openBuilt, busy }: { t: Dict; rawDir: string; libraryDir: string; setRawDir: (v: string) => void; setLibraryDir: (v: string) => void; buildArrow: boolean; setBuildArrow: (v: boolean) => void; buildLibrary: () => void; openBuilt: () => void; busy: boolean }) {
  return <section className="buildOne onepage"><div className="card buildBox"><Kicker text="01 / INPUT" /><PathPick label={t.raw} value={rawDir} button={t.chooseDir} onPick={async () => { const p = await pickDir(); if (p) setRawDir(p); }} disabled={busy} /><PathPick label={t.output} value={libraryDir} button={t.chooseDir} onPick={async () => { const p = await pickDir(); if (p) setLibraryDir(p); }} disabled={busy} /><label className="toggle"><input type="checkbox" checked={buildArrow} onChange={(e) => setBuildArrow(e.target.checked)} />{t.buildArrow}</label><div className="actions"><button onClick={buildLibrary} disabled={busy}>{t.startBuild}</button><button className="secondary" onClick={openBuilt} disabled={busy || !libraryDir}>{t.openAfterBuild}</button></div></div><div className="card checklist"><Kicker text="02 / QA" /><h2>{t.buildChecklist}</h2><ul><li>{t.checkRaw}</li><li>{t.checkOutput}</li><li>{t.checkArrow}</li></ul></div><div className="card layout"><Kicker text="03 / LAYOUT" /><h2>{t.storageTitle}</h2><div><b>Parquet</b><span>{t.parquet}</span></div><div><b>Arrow</b><span>{t.arrow}</span></div><div><b>Manifest</b><span>{t.manifest}</span></div></div></section>;
}
function SearchPage(props: { t: Dict; dataPath: string; setDataPath: (v: string) => void; connect: () => void; recent: string[]; openRecent: (p: string) => void; overview: Overview | null; sourceType: string; setSourceType: (v: string) => void; metric: string; setMetric: (v: string) => void; text: string; setText: (v: string) => void; country: string; setCountry: (v: string) => void; oaFilter: string; setOaFilter: (v: string) => void; refresh: () => void; ranking: JsonRow[]; results: JsonRow[]; chart: object; exportRows: (k: 'top'|'search') => void; busy: boolean }) {
  const p = props, t = p.t;
  return <section className="searchOne onepage"><div className="card source"><Kicker text="01 / CONNECT" /><label>{t.dataSource}</label><div className="sourceRow"><div className="path">{shortPath(p.dataPath)}</div><button className="light" disabled={p.busy} onClick={async () => { const d = await pickDir(); if (d) p.setDataPath(d); }}>{t.chooseLibrary}</button><button className="light" disabled={p.busy} onClick={async () => { const f = await pickDataFile(); if (f) p.setDataPath(f); }}>{t.chooseFile}</button><button disabled={p.busy || !p.dataPath} onClick={p.connect}>{t.connect}</button></div></div><div className="statsLine">{[[p.overview?.source_count,t.sources],[p.overview?.journal_count,t.journals],[p.overview?.conference_count,t.conferences],[p.overview?.oa_count,t.oa],[p.overview?.works_count,t.works],[p.overview?.cited_by_count,t.cited]].map(([v,l]) => <div className="mini" key={String(l)}><span>{l}</span><b>{fmt(v)}</b></div>)}</div><div className="card filters"><Select label={t.type} value={p.sourceType} onChange={p.setSourceType} options={[[ 'journal', t.journal ],[ 'conference', t.conference ]]} /><Select label={t.metric} value={p.metric} onChange={p.setMetric} options={t.metrics} /><Field label={t.keyword} value={p.text} onChange={p.setText} onEnter={p.refresh} /><Field label={t.country} value={p.country} onChange={(v) => p.setCountry(v.toUpperCase())} onEnter={p.refresh} /><Select label={t.oaFilter} value={p.oaFilter} onChange={p.setOaFilter} options={[[ 'all', t.all ],[ 'oa', t.onlyOa ],[ 'non_oa', t.onlyNonOa ]]} /><button disabled={p.busy || !p.overview} onClick={p.refresh}>{t.query}</button></div>{p.overview ? <><div className="card chart"><div className="cardHead"><h2>{t.ranking}</h2><button className="textBtn" onClick={() => p.exportRows('top')}>{t.exportRanking}</button></div><ReactECharts option={p.chart} style={{ height: '100%' }} /></div><div className="card table"><div className="cardHead"><h2>{t.results}</h2><button className="textBtn" onClick={() => p.exportRows('search')}>{t.exportSearch}</button></div><DataTable rows={p.results.length ? p.results : p.ranking} /></div></> : <div className="card empty"><h2>{t.noData}</h2><p>{t.searchDesc}</p></div>}<div className="card insight"><h2>{t.insight}</h2><p>{t.tip1}</p><p>{t.tip2}</p><p>{t.tip3}</p><h3>{t.recent}</h3>{p.recent.length ? p.recent.map((r) => <button key={r} onClick={() => p.openRecent(r)}>{shortPath(r)}</button>) : <small>{t.emptyRecent}</small>}</div></section>;
}
function Kicker({ text }: { text: string }) { return <div className="kicker">{text}</div>; }
function PathPick({ label, value, button, onPick, disabled }: { label: string; value: string; button: string; onPick: () => void; disabled: boolean }) { return <div className="pathPick"><label>{label}</label><div><span>{shortPath(value)}</span><button className="light" onClick={onPick} disabled={disabled}>{button}</button></div></div>; }
function Select({ label, value, onChange, options }: { label: string; value: string; onChange: (v: string) => void; options: string[][] }) { return <label className="field"><span>{label}</span><select value={value} onChange={(e) => onChange(e.target.value)}>{options.map(([v, l]) => <option key={v} value={v}>{l}</option>)}</select></label>; }
function Field({ label, value, onChange, onEnter }: { label: string; value: string; onChange: (v: string) => void; onEnter: () => void }) { return <label className="field"><span>{label}</span><input value={value} onChange={(e) => onChange(e.target.value)} onKeyDown={(e) => { if (e.key === 'Enter') onEnter(); }} /></label>; }
function DataTable({ rows }: { rows: JsonRow[] }) { const cols = ['display_name','source_type','country_code','h_index','i10_index','cited_by_count','works_count','mean_citedness_2yr']; return <div className="tableWrap"><table><thead><tr>{cols.map((c) => <th key={c}>{c}</th>)}</tr></thead><tbody>{rows.map((r, i) => <tr key={String(r.openalex_id ?? i)}>{cols.map((c) => <td key={c}>{fmt(r[c])}</td>)}</tr>)}</tbody></table></div>; }
