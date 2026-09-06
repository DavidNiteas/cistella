import { Button, Card, CardHeader, EmptyState, ErrorBanner, Field, Input, PageHeader, Select } from '../../components/ui';
import styles from './SearchPage.module.css';
import type { Dict } from '../../lib/i18n/dict';
import type { LiteratureItem, VaultConnection } from '../../types';
import type { LocalSearchState } from '../../hooks/useLocalSearch';

export interface SearchPageProps {
  vault: VaultConnection;
  search: LocalSearchState;
  items: LiteratureItem[];
  t: Dict;
  lang: 'zh' | 'en';
}

export function SearchPage({ vault, search, items, t, lang }: SearchPageProps) {
  return (
    <section className="page">
      <Card>
        <PageHeader
          title={t.search}
          description={lang === 'zh' ? '检索当前库的标题、作者、标签和已建立索引的 PDF 正文。结果只返回文献和资产身份。' : 'Search titles, authors, tags, and indexed PDF text in the current library. Results contain identities only.'}
        />
        {!vault.hasVault && <p>{t.connectForAnalysis}</p>}
      </Card>

      {vault.hasVault && (
        <>
          <div className={`card controls ${styles.localSearchControls}`}>
            <div className={styles.searchInput} style={{ flex: 1 }}>
              <Field label={lang === 'zh' ? '查询' : 'Query'}>
                <Input value={search.text} onChange={(e) => search.setText(e.target.value)} onKeyDown={(e) => { if (e.key === 'Enter') void search.runSearch(); }} placeholder={lang === 'zh' ? '输入关键词，回车搜索' : 'Enter keywords and press Enter'} />
              </Field>
            </div>
            <Field label={lang === 'zh' ? '范围' : 'Scope'}>
              <Select value={search.scope} onChange={(e) => search.setScope(e.target.value as 'all' | 'title' | 'authors' | 'tags' | 'content')} options={[
                { value: 'all', label: lang === 'zh' ? '全部字段' : 'All fields' },
                { value: 'title', label: lang === 'zh' ? '标题' : 'Title' },
                { value: 'authors', label: lang === 'zh' ? '作者' : 'Authors' },
                { value: 'tags', label: lang === 'zh' ? '标签' : 'Tags' },
                { value: 'content', label: lang === 'zh' ? '正文' : 'Content' },
              ]} />
            </Field>
            <Button onClick={() => void search.runSearch()} disabled={search.loading} loading={search.loading}>
              {lang === 'zh' ? '搜索' : 'Search'}
            </Button>
          </div>

          <Card className="compact">
            <CardHeader
              title={lang === 'zh' ? '索引健康' : 'Index health'}
              action={<span className={`${styles.searchStatus} ${search.indexState?.status ? (styles as Record<string, string>)[search.indexState.status] ?? '' : ''}`}>{search.indexState?.status ?? 'missing'}</span>}
            />
            <p>{search.indexState?.detail || (lang === 'zh' ? '索引操作需要由你明确发起；打开当前库不会隐式重建。' : 'Index work is explicit; opening a library never rebuilds implicitly.')}</p>
            <div className="actions">
              <Button onClick={() => void search.runTask('synchronize_local_search_index')} disabled={search.task?.status === 'building'} loading={search.task?.status === 'building'}>
                {lang === 'zh' ? '同步' : 'Sync'}
              </Button>
              <Button variant="secondary" onClick={() => void search.runTask('rebuild_local_search_index')} disabled={search.task?.status === 'building'}>
                {lang === 'zh' ? '重建' : 'Rebuild'}
              </Button>
              <Button variant="secondary" onClick={() => void search.cancelTask()} disabled={search.task?.status !== 'building'}>
                {lang === 'zh' ? '取消' : 'Cancel'}
              </Button>
              <Button variant="secondary" onClick={() => void search.loadIssues()}>
                {lang === 'zh' ? '查看问题' : 'Issues'}
              </Button>
            </div>
            {search.task && <small>{lang === 'zh' ? '任务' : 'Task'}: {search.task.status}{search.task.detail ? ` · ${search.task.detail}` : ''}</small>}
          </Card>

          {search.error && (
            <Card className="">
              <CardHeader title={lang === 'zh' ? '搜索错误' : 'Search error'} />
              <ErrorBanner>{search.error}</ErrorBanner>
            </Card>
          )}

          {search.outcome?.outcome === 'unavailable' && (
            <Card className="">
              <CardHeader title={lang === 'zh' ? '索引不可用' : 'Index unavailable'} />
              <p>{search.outcome.indexState.status}{search.outcome.indexState.detail ? ` · ${search.outcome.indexState.detail}` : ''}</p>
              <p>{lang === 'zh' ? '这不是“无命中”。请显式同步或重建索引。' : 'This is not an empty result. Explicitly sync or rebuild the index.'}</p>
            </Card>
          )}

          {search.outcome?.outcome === 'ready' && (
            <Card className={`${styles.localSearchResults}`}>
              <CardHeader
                title={lang === 'zh' ? `结果 ${search.outcome.page.totalHits}` : `${search.outcome.page.totalHits} results`}
                action={<span>{lang === 'zh' ? '每页 50；元数据优先，item_id 稳定排序' : '50/page; metadata first, stable item_id order'}</span>}
              />
              <div className={`actions ${styles.searchPagination}`}>
                <Button variant="secondary" onClick={() => void search.runSearch(Math.max(0, search.offset - 50))} disabled={search.offset === 0 || search.loading}>
                  {lang === 'zh' ? '上一页' : 'Previous'}
                </Button>
                <small>{search.offset + 1}–{Math.min(search.offset + 50, search.outcome.page.totalHits)} / {search.outcome.page.totalHits}</small>
                <Button variant="secondary" onClick={() => void search.runSearch(search.offset + 50)} disabled={search.loading || search.offset + 50 >= search.outcome.page.totalHits}>
                  {lang === 'zh' ? '下一页' : 'Next'}
                </Button>
              </div>
              {search.outcome.page.hits.length ? (
                search.outcome.page.hits.map((hit) => {
                  const item = items.find((x) => x.itemId === hit.itemId);
                  return (
                    <article className={styles.literatureItem} key={hit.itemId}>
                      <div className={styles.literatureItemDetail}>
                        <h3>{item?.title || hit.itemId}</h3>
                        <p>{item?.authors.join(', ') || '—'}</p>
                        {hit.fieldMatches.map((match, index) => {
                          const assetId = match.assetId;
                          return (
                            <div className={styles.searchMatch} key={`${match.field}-${assetId ?? index}`}>
                              <small>
                                {match.field}
                                {match.matchedTerms.length ? ` · ${match.matchedTerms.join(', ')}` : ''}
                                {match.assetState ? ` · ${match.assetState}` : ''}
                              </small>
                              {match.excerpt && <p>{match.excerpt}</p>}
                              {assetId && match.assetState === 'indexed' && (
                                <Button variant="secondary" onClick={() => search.openHitAsset(hit.itemId, assetId)}>
                                  {lang === 'zh' ? '受控打开命中资产' : 'Open matched asset'}
                                </Button>
                              )}
                            </div>
                          );
                        })}
                      </div>
                    </article>
                  );
                })
              ) : (
                <EmptyState title={lang === 'zh' ? '没有匹配文献。' : 'No matching literature.'} />
              )}
            </Card>
          )}

          {search.issues?.outcome === 'ready' && (
            <Card className="">
              <CardHeader title={lang === 'zh' ? '资产级索引问题' : 'Asset index issues'} action={<span>{search.issues.issues.length}</span>} />
              {search.issues.issues.length ? (
                search.issues.issues.map((issue) => (
                  <p key={`${issue.itemId}-${issue.assetId}`}>{issue.kind}{issue.detail ? ` · ${issue.detail}` : ''}</p>
                ))
              ) : (
                <p>{lang === 'zh' ? '没有资产级问题。' : 'No asset-level issues.'}</p>
              )}
            </Card>
          )}

          {search.issues?.outcome === 'unavailable' && (
            <Card className="">
              <p>{lang === 'zh' ? '索引不可用，暂不能读取问题列表。' : 'The index is unavailable, so issues cannot be read yet.'}</p>
            </Card>
          )}
        </>
      )}
    </section>
  );
}
