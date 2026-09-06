import { Star, StarOff, Trash2, Upload } from 'lucide-react';
import { Button, Card, CardHeader, DangerDialog, ErrorBanner, EmptyState, Field, Icon, Input, PageHeader, Select, TextArea } from '../../components/ui';
import styles from './ReadingPage.module.css';
import type { Dict } from '../../lib/i18n/dict';
import type { DocumentAsset, LiteratureItem, ReadingSession, VaultConnection } from '../../types';
import type { LiteratureState } from '../../hooks/useLiterature';
import type { ReadingSessionsState } from '../../hooks/useReadingSessions';
import type { VaultContextValue } from '../../hooks/useVaultContext';
import { detectIdentifierType, dir, formatFileSize, formatSessionTime, short } from '../../lib/utils';

export interface ReadingPageProps {
  vault: VaultConnection;
  literature: LiteratureState;
  reading: ReadingSessionsState;
  context: VaultContextValue;
  t: Dict;
  lang: 'zh' | 'en';
  onConnect: (path: string) => void;
}

export function ReadingPage({ vault, literature, reading, context, t, lang, onConnect }: ReadingPageProps) {
  return (
    <section className={`page ${styles.readingPage}`}>
      <ReadingHero t={t} hasVault={vault.hasVault} vaultSummary={context.summary} vaultPath={vault.vaultPath} busy={vault.busy} onConnect={onConnect} />

      {vault.hasVault && (
        <SessionList t={t} lang={lang} busy={vault.busy} sessions={reading.sessions} items={literature.items} assets={context.assets} onContinue={reading.continueReading} onResume={reading.resume} onPause={reading.pause} onEnd={reading.end} />
      )}

      {vault.hasVault && (
        <LiteratureToolbar
          t={t}
          busy={vault.busy}
          lang={lang}
          vaultPath={vault.vaultPath}
          literature={literature}
        />
      )}

      {vault.hasVault && literature.importFormat === 'openalex_works' && (
        <OpenAlexWorksPanel t={t} lang={lang} busy={vault.busy} literature={literature} />
      )}

      {vault.hasVault && literature.importPreview && (
        <ImportPreviewPanel t={t} busy={vault.busy} preview={literature.importPreview} onPolicyChange={literature.updateImportPolicy} onCommit={literature.commitImport} onDismiss={literature.dismissImportPreview} />
      )}

      {vault.hasVault && literature.importResult && (
        <Card className={`${styles.literatureImportResult}`}>
          <CardHeader title={t.importResultTitle} />
          <p>
            {lang === 'zh'
              ? `新建 ${literature.importResult.created} · 合并 ${literature.importResult.merged} · 跳过 ${literature.importResult.skipped} · 错误 ${literature.importResult.errors}`
              : `Created ${literature.importResult.created} · Merged ${literature.importResult.merged} · Skipped ${literature.importResult.skipped} · Errors ${literature.importResult.errors}`}
          </p>
          <Button variant="secondary" onClick={literature.clearImportResult}>
            {t.cancel}
          </Button>
        </Card>
      )}

      {vault.hasVault && literature.importError && (
        <Card className="">
          <CardHeader title={t.failed} />
          <ErrorBanner>{literature.importError}</ErrorBanner>
        </Card>
      )}

      {vault.hasVault && (literature.remoteResolveLoading || literature.remoteResolveError || literature.remoteResolvePreview) && (
        <RemoteResolvePanel
          t={t}
          busy={vault.busy}
          loading={literature.remoteResolveLoading}
          error={literature.remoteResolveError}
          preview={literature.remoteResolvePreview}
          strategy={literature.remoteImportStrategy}
          onStrategyChange={literature.updateRemoteImportStrategy}
          onCommit={literature.commitRemoteImport}
          onDismiss={() => { literature.setRemoteResolvePreview(null); literature.setRemoteResolveError(''); }}
        />
      )}

      {vault.hasVault && literature.editorOpen && (
        <LiteratureEditor t={t} busy={vault.busy} literature={literature} />
      )}

      {vault.hasVault && (
        <LiteratureList t={t} busy={vault.busy} items={literature.filteredItems} assets={context.assets} onToggleFavorite={literature.toggleFavorite} onEdit={literature.beginEdit} onDelete={literature.requestDelete} onStartReading={reading.start} />
      )}

      <DangerDialog
        open={literature.deleteId != null}
        title={lang === 'zh' ? '删除条目' : 'Delete item'}
        onConfirm={() => void literature.confirmDelete()}
        onCancel={() => literature.requestDelete(null)}
        dangerLabel={t.deleteLiterature}
        cancelLabel={t.cancel}
      >
        {lang === 'zh' ? '确定删除这个条目吗？此操作不可撤销。' : 'Delete this item? This action cannot be undone.'}
      </DangerDialog>
    </section>
  );
}

function ReadingHero({ t, hasVault, vaultSummary, vaultPath, busy, onConnect }: { t: Dict; hasVault: boolean; vaultSummary: import('../../types').VaultSummary | null; vaultPath: string; busy: boolean; onConnect: (path: string) => void }) {
  return (
    <Card>
      <PageHeader
        title={t.readingTitle}
        description={t.readingDesc}
        actions={
          !hasVault ? (
            <Button onClick={async () => { const path = await dir(); if (path) await onConnect(path); }} disabled={busy}>
              {t.connect}
            </Button>
          ) : undefined
        }
      />
      {!hasVault ? <p>{t.noVaultReading}</p> : <p>{vaultSummary?.vault_id ?? vaultPath}</p>}
    </Card>
  );
}

function SessionList({ t, lang, busy, sessions, items, assets, onContinue, onResume, onPause, onEnd }: {
  t: Dict;
  lang: 'zh' | 'en';
  busy: boolean;
  sessions: import('../../types').ReadingSessionSummary[];
  items: LiteratureItem[];
  assets: DocumentAsset[];
  onContinue: () => void;
  onResume: (session: ReadingSession) => void;
  onPause: (session: ReadingSession) => void;
  onEnd: (session: ReadingSession) => void;
}) {
  return (
    <Card className={`${styles.readingSessions}`}>
      <CardHeader title={t.recentReading} action={<Button onClick={onContinue} disabled={busy || !sessions.length}>{t.continueReading}</Button>} />
      <p>{t.recentReadingDesc}</p>
      {sessions.length ? (
        <div className={styles.readingSessionList}>
          {sessions.map((summary) => {
            const session = summary.session;
            const item = items.find((entry) => entry.itemId === session.itemId);
            const asset = assets.find((entry) => entry.assetId === session.assetId && entry.itemId === session.itemId);
            const isAvailable = summary.assetStatus === 'available';
            return (
              <article className={`${styles.readingSession} ${isAvailable ? '' : styles.unavailable}`} key={session.sessionId}>
                <div className={styles.readingSessionMeta}>
                  <strong>{item?.title || '—'}</strong>
                  <small>{asset?.displayName || session.assetId}</small>
                  <small>{t.readingSessionState}: {t.readingSessionStates[session.state]} · {t.assetHealth}: {t.assetStatuses[summary.assetStatus]}</small>
                  <small>{t.lastOpenedAt}: {formatSessionTime(session.lastOpenedAt, lang)} · {t.startedAt}: {formatSessionTime(session.startedAt, lang)}</small>
                  {!isAvailable && <small className={styles.sessionWarning}>{t.sessionUnavailable}</small>}
                </div>
                <div className={styles.fileActions}>
                  {isAvailable && (
                    <Button variant="secondary" onClick={() => onResume(session)} disabled={busy}>
                      {t.resumeReading}
                    </Button>
                  )}
                  {session.state === 'active' && (
                    <Button variant="secondary" onClick={() => onPause(session)} disabled={busy}>
                      {t.pauseReading}
                    </Button>
                  )}
                  {session.state !== 'closed' && (
                    <Button variant="danger" onClick={() => onEnd(session)} disabled={busy}>
                      {t.endReading}
                    </Button>
                  )}
                </div>
              </article>
            );
          })}
        </div>
      ) : (
        <EmptyState title={t.noRecentReading} />
      )}
    </Card>
  );
}

function LiteratureToolbar({ t, busy, lang: _lang, vaultPath, literature }: { t: Dict; busy: boolean; lang: 'zh' | 'en'; vaultPath: string; literature: LiteratureState }) {
  return (
    <Card className={`${styles.literatureToolbar}`}>
      <Input placeholder={t.literatureKeyword} value={literature.keyword} onChange={(e) => literature.setKeyword(e.target.value)} />
      <span>{literature.items.length} {t.literatureCount}</span>
      <input ref={literature.fileInputRef} type="file" accept=".bib,.ris" style={{ display: 'none' }} onChange={(e) => { const file = e.target.files?.[0]; if (file) void literature.handleFileSelected(file); e.target.value = ''; }} />
      <Field label={t.importLiteratureFormat}>
        <Select value={literature.importFormat} onChange={(e) => literature.setImportFormat(e.target.value as 'bibtex' | 'ris' | 'openalex_works')} options={[
          { value: 'bibtex', label: 'BibTeX' },
          { value: 'ris', label: 'RIS' },
          { value: 'openalex_works', label: t.openAlexWorksFormat },
        ]} />
      </Field>
      {literature.importFormat === 'openalex_works' ? (
        <>
          <Button variant="secondary" onClick={() => void literature.pickOpenAlexWorksDir()} disabled={busy}>
            {t.chooseDir}
          </Button>
          <small title={literature.openAlexWorksDir}>{short(literature.openAlexWorksDir)}</small>
          <Input placeholder={t.openAlexWorksQuery} value={literature.openAlexQuery} onChange={(e) => literature.setOpenAlexQuery(e.target.value)} onKeyDown={(e) => { if (e.key === 'Enter') void literature.searchOpenAlexWorks(); }} />
          <Button variant="secondary" onClick={() => void literature.searchOpenAlexWorks()} disabled={busy || literature.openAlexLoading || !literature.openAlexWorksDir || !literature.openAlexQuery.trim()}>
            {t.searchOpenAlexWorks}
          </Button>
        </>
      ) : (
        <Button variant="secondary" onClick={literature.inspectImport} disabled={busy}>
          <Icon icon={Upload} size={14} /> {t.importLiterature}
        </Button>
      )}
      <span style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
        <Input placeholder={t.identifierInputPlaceholder} value={literature.identifierInput} onChange={(e) => literature.setIdentifierInput(e.target.value)} onKeyDown={(e) => { if (e.key === 'Enter') void literature.resolveRemoteMetadata(); }} />
        {(() => {
          const type = detectIdentifierType(literature.identifierInput);
          return type ? <small>{type.toUpperCase()}</small> : null;
        })()}
      </span>
      <Button variant="secondary" onClick={() => void literature.resolveRemoteMetadata()} disabled={busy || literature.remoteResolveLoading || !literature.identifierInput.trim()}>
        {t.resolveIdentifier}
      </Button>
      <Button variant="secondary" onClick={() => void literature.clearRemoteCache()} disabled={busy || !vaultPath}>
        {t.clearRemoteCache}
      </Button>
      <Button onClick={literature.beginNew} disabled={busy}>
        {t.addLiterature}
      </Button>
    </Card>
  );
}

function OpenAlexWorksPanel({ t, lang, busy, literature }: { t: Dict; lang: 'zh' | 'en'; busy: boolean; literature: LiteratureState }) {
  return (
    <Card className="">
      <CardHeader title={t.openAlexWorksTitle} />
      {literature.openAlexLoading && <p>{lang === 'zh' ? '搜索中…' : 'Searching…'}</p>}
      {literature.openAlexError && <ErrorBanner>{literature.openAlexError}</ErrorBanner>}
      {literature.openAlexCandidates.length > 0 && (
        <div className={styles.importPreviewList}>
          {literature.openAlexCandidates.map((candidate) => {
            const ids = candidate.externalIdentifiers.map((id) => `${id.namespace}: ${id.value}`).join(' · ');
            return (
              <article key={candidate.recordId} className={styles.importPreviewItem}>
                <div>
                  <strong>{candidate.title || '—'}</strong>
                  <small>{candidate.authors.join(', ') || '—'}{candidate.publishedYear ? ` · ${candidate.publishedYear}` : ''}</small>
                  {ids && <small>{ids}</small>}
                </div>
                <Button variant="secondary" onClick={() => void literature.previewOpenAlexWork(candidate)} disabled={busy}>
                  {t.importPreview}
                </Button>
              </article>
            );
          })}
        </div>
      )}
    </Card>
  );
}

function ImportPreviewPanel({ t, busy, preview, onPolicyChange, onCommit, onDismiss }: { t: Dict; busy: boolean; preview: import('../../types').LiteratureImportPreview; onPolicyChange: (recordId: string, policy: 'merge' | 'skip' | 'create') => void; onCommit: () => Promise<void>; onDismiss: () => void }) {
  return (
    <Card className={`${styles.literatureImportPreview}`}>
      <CardHeader title={t.importLiteratureTitle} />
      <p>{t.importLiteratureDesc}</p>
      <p>{t.importPreviewTitle}: {preview.items.length}</p>
      <div className={styles.importPreviewList}>
        {preview.items.map((item) => {
          const matched = item.matchedItemId != null;
          const ids = item.sourceRecord.externalIdentifiers.map((id) => `${id.namespace}: ${id.value}`).join(' · ');
          return (
            <article key={item.recordId} className={`${styles.importPreviewItem} ${matched ? styles.matched : ''}`}>
              <div>
                <strong>{item.sourceRecord.title || '—'}</strong>
                <small>{item.sourceRecord.authors.join(', ') || '—'}{item.sourceRecord.publishedYear ? ` · ${item.sourceRecord.publishedYear}` : ''}</small>
                {ids && <small>{ids}</small>}
                <small className={styles.importConflict}>{matched ? t.importConflictMatched : t.importConflictNew}</small>
              </div>
              <select value={item.selectedPolicy} onChange={(e) => onPolicyChange(item.recordId, e.target.value as 'merge' | 'skip' | 'create')} disabled={busy}>
                <option value="merge">{t.importPolicyMerge}</option>
                <option value="skip">{t.importPolicySkip}</option>
                <option value="create">{t.importPolicyCreate}</option>
              </select>
            </article>
          );
        })}
      </div>
      <div className="actions">
        <Button onClick={() => void onCommit()} disabled={busy}>
          {t.importCommit}
        </Button>
        <Button variant="secondary" onClick={onDismiss} disabled={busy}>
          {t.cancel}
        </Button>
      </div>
    </Card>
  );
}

function RemoteResolvePanel({ t, busy, loading, error, preview, strategy, onStrategyChange, onCommit, onDismiss }: {
  t: Dict;
  busy: boolean;
  loading: boolean;
  error: string;
  preview: import('../../types').LiteratureImportPreview | null;
  strategy: 'merge' | 'skip' | 'create';
  onStrategyChange: (policy: 'merge' | 'skip' | 'create') => void;
  onCommit: () => Promise<void>;
  onDismiss: () => void;
}) {
  return (
    <Card className={`${styles.literatureImportPreview}`}>
      <CardHeader title={t.remoteResolveTitle} />
      {loading && <p>{t.busy}</p>}
      {error && <ErrorBanner>{error}</ErrorBanner>}
      {preview && (
        <>
          <p>{t.importPreviewTitle}: {preview.items.length}</p>
          <div className={styles.importPreviewList}>
            {preview.items.map((item) => {
              const matched = item.matchedItemId != null;
              const ids = item.sourceRecord.externalIdentifiers.map((id) => `${id.namespace}: ${id.value}`).join(' · ');
              return (
                <article key={item.recordId} className={`${styles.importPreviewItem} ${matched ? styles.matched : ''}`}>
                  <div>
                    <strong>{item.sourceRecord.title || '—'}</strong>
                    <small>{item.sourceRecord.authors.join(', ') || '—'}{item.sourceRecord.publishedYear ? ` · ${item.sourceRecord.publishedYear}` : ''}</small>
                    {ids && <small>{ids}</small>}
                    <small className={styles.importConflict}>{matched ? t.importConflictMatched : t.importConflictNew}</small>
                  </div>
                  <select value={strategy} onChange={(e) => onStrategyChange(e.target.value as 'merge' | 'skip' | 'create')} disabled={busy}>
                    <option value="merge">{t.importPolicyMerge}</option>
                    <option value="skip">{t.importPolicySkip}</option>
                    <option value="create">{t.importPolicyCreate}</option>
                  </select>
                </article>
              );
            })}
          </div>
          <div className="actions">
            <Button onClick={() => void onCommit()} disabled={busy}>
              {t.importCommit}
            </Button>
            <Button variant="secondary" onClick={onDismiss} disabled={busy}>
              {t.cancel}
            </Button>
          </div>
        </>
      )}
    </Card>
  );
}

function LiteratureEditor({ t, busy, literature }: { t: Dict; busy: boolean; literature: LiteratureState }) {
  return (
    <Card className={`${styles.literatureEditor}`}>
      <CardHeader title={literature.editingId ? t.editLiterature : t.addLiterature} />
      <Field label={t.title}>
        <Input value={literature.draft.title} onChange={(e) => literature.updateDraft({ title: e.target.value })} />
      </Field>
      <Field label={t.authors}>
        <TextArea value={literature.draft.authors.join('\n')} onChange={(e) => literature.updateDraft({ authors: e.target.value.split('\n') })} />
      </Field>
      <div className={styles.literatureFields}>
        <Field label={t.publishedYear}>
          <Input type="number" value={literature.draft.publishedYear ?? ''} onChange={(e) => literature.updateDraft({ publishedYear: e.target.value ? Number(e.target.value) : null })} />
        </Field>
        <Field label={t.itemType}>
          <Select value={literature.draft.itemType} onChange={(e) => literature.updateDraft({ itemType: e.target.value as 'article' | 'book' | 'chapter' | 'other' })} options={[
            { value: 'article', label: 'article' },
            { value: 'book', label: 'book' },
            { value: 'chapter', label: 'chapter' },
            { value: 'other', label: 'other' },
          ]} />
        </Field>
        <small>{t.readingStatus}: {t[literature.draft.readingStatus]}</small>
      </div>
      <Field label={t.tags}>
        <Input value={literature.draft.tags.join(', ')} onChange={(e) => literature.updateDraft({ tags: e.target.value.split(',').map((x) => x.trim()).filter(Boolean) })} />
      </Field>
      <label className={styles.toggle}>
        <input type="checkbox" checked={literature.draft.favorite} onChange={(e) => literature.updateDraft({ favorite: e.target.checked })} />
        {t.favorite}
      </label>
      <div className={styles.editorFiles}>
        <h3>{t.readingAssets}</h3>
        <small>{literature.editingItem ? t.manageAssetsInVault : t.saveBeforeFiles}</small>
      </div>
      <div className="actions">
        <Button onClick={() => void literature.save()} disabled={busy || !literature.draft.title.trim()}>
          {t.saveLiterature}
        </Button>
        <Button variant="secondary" onClick={literature.closeEditor}>
          {t.cancel}
        </Button>
      </div>
    </Card>
  );
}

function LiteratureList({ t, busy, items, assets, onToggleFavorite, onEdit, onDelete, onStartReading }: {
  t: Dict;
  busy: boolean;
  items: LiteratureItem[];
  assets: DocumentAsset[];
  onToggleFavorite: (item: LiteratureItem) => Promise<void>;
  onEdit: (item: LiteratureItem) => void;
  onDelete: (itemId: string) => void;
  onStartReading: (asset: DocumentAsset) => void;
}) {
  return (
    <Card className={`${styles.literatureList}`}>
      <CardHeader title={t.literatureCount} />
      {items.length ? (
        items.map((item) => (
          <article className={styles.literatureItem} key={item.itemId}>
            <div className={styles.literatureItemDetail}>
              <h3>{item.title || '—'}</h3>
              <p>{item.authors.join(', ') || '—'}{item.publishedYear ? ` · ${item.publishedYear}` : ''}</p>
              <small>{item.itemType} · {item.tags.join(', ') || '—'}</small>
              <ReadingAssetList t={t} assets={assets.filter((a) => a.itemId === item.itemId)} busy={busy} onStart={onStartReading} />
            </div>
            <div className={styles.itemActions}>
              <Button variant="secondary" onClick={() => onToggleFavorite(item)} disabled={busy} aria-label={t.favorite}>
                <Icon icon={item.favorite ? Star : StarOff} size={16} />
              </Button>
              <Button variant="secondary" onClick={() => onEdit(item)} disabled={busy}>
                {t.editLiterature}
              </Button>
              <Button variant="danger" onClick={() => onDelete(item.itemId)} disabled={busy}>
                <Icon icon={Trash2} size={14} /> {t.deleteLiterature}
              </Button>
            </div>
          </article>
        ))
      ) : (
        <EmptyState title={t.noLiterature} />
      )}
    </Card>
  );
}

function ReadingAssetList({ t, assets, busy, onStart }: { t: Dict; assets: DocumentAsset[]; busy: boolean; onStart: (asset: DocumentAsset) => void }) {
  if (!assets.length) return <small>{t.noFiles} {t.manageAssetsInVault}</small>;
  return (
    <div className={`${styles.literatureFiles} ${styles.readingAssetList}`}>
      {assets.map((asset) => {
        const available = asset.status === 'available';
        return (
          <div className={`${styles.literatureFile} ${styles.readingAsset} ${available ? '' : styles.unavailable}`} key={asset.assetId}>
            <div className={styles.assetMeta}>
              <strong>{asset.displayName}</strong>
              <small>{t.assetKind}: {t.assetKinds[asset.assetKind]} · {t.assetStorage}: {asset.storageKind === 'vault' ? t.vaultFile : t.externalFile}</small>
              <small>{t.assetHealth}: {t.assetStatuses[asset.status]} · {t.assetSize}: {formatFileSize(asset.fileSize)}</small>
              {!available && <small className={styles.sessionWarning}>{t.sessionUnavailable}</small>}
            </div>
            <div className={styles.fileActions}>
              <Button onClick={() => onStart(asset)} disabled={busy || !available}>
                {t.startReading}
              </Button>
            </div>
          </div>
        );
      })}
    </div>
  );
}

