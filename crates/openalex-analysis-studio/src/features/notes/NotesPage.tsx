import { Archive, ExternalLink } from 'lucide-react';
import { Button, Card, CardHeader, EmptyState, ErrorBanner, Field, Icon, Input, PageHeader, TextArea } from '../../components/ui';
import styles from './NotesPage.module.css';
import type { Dict } from '../../lib/i18n/dict';
import type { Annotation, LiteratureItem, Note, VaultConnection } from '../../types';
import type { NotesState } from '../../hooks/useNotes';
import { annotationStatusLabel } from '../../lib/utils';

export interface NotesPageProps {
  vault: VaultConnection;
  notes: NotesState;
  items: LiteratureItem[];
  t: Dict;
  lang: 'zh' | 'en';
}

export function NotesPage({ vault, notes, items, t, lang }: NotesPageProps) {
  return (
    <section className="page">
      <Card className="span3 hero">
        <PageHeader title={t.notesTitle} description={t.notesDesc} />
        {!vault.hasVault ? <p>{t.noVaultReading}</p> : <p>{vault.vaultPath}</p>}
      </Card>

      {vault.hasVault && (
        <Card className={`span3 ${styles.notesToolbar}`}>
          <Field label={t.noteItemFilter}>
            <select value={notes.filterItemId} onChange={(e) => { notes.setFilterItemId(e.target.value); notes.setSelectedId(null); }} disabled={notes.loading}>
              <option value="all">{t.allItems}</option>
              {items.map((item) => (
                <option key={item.itemId} value={item.itemId}>{item.title || '—'}</option>
              ))}
            </select>
          </Field>
          <Button onClick={notes.beginNew} disabled={notes.saving || notes.loading}>
            {t.newNote}
          </Button>
        </Card>
      )}

      {vault.hasVault && notes.error && (
        <Card className="span3">
          <CardHeader title={t.failed} />
          <ErrorBanner>{notes.error}</ErrorBanner>
        </Card>
      )}

      {vault.hasVault && notes.conflict && (
        <Card className="span3">
          <CardHeader title={t.noteConflict} />
          <p>{t.noteConflictMessage}</p>
          <p>{notes.conflict.message}</p>
          <div className="actions">
            <Button onClick={() => void notes.refreshSelected()} disabled={notes.saving}>
              {t.refreshNote}
            </Button>
            <Button onClick={() => void notes.overwrite()} disabled={notes.saving}>
              {t.overwriteNote}
            </Button>
          </div>
        </Card>
      )}

      {vault.hasVault && (
        <Card className={`span3 ${styles.notesEditor}`}>
          <Field label={t.noteTitle}>
            <Input value={notes.editorTitle} onChange={(e) => notes.setEditorTitle(e.target.value)} disabled={notes.saving} />
          </Field>
          <Field label={t.noteBody}>
            <TextArea value={notes.editorBody} onChange={(e) => notes.setEditorBody(e.target.value)} disabled={notes.saving} rows={12} />
          </Field>
          <div className="actions">
            <Button onClick={() => void notes.save()} disabled={notes.saving || !notes.editorTitle.trim()} loading={notes.saving}>
              {notes.saving ? t.savingNote : t.saveNote}
            </Button>
          </div>
        </Card>
      )}

      {vault.hasVault && (
        <NoteList t={t} lang={lang} notes={notes.items} saving={notes.saving} loading={notes.loading} onSelect={notes.setSelectedId} onArchive={notes.archive} onRestore={notes.restore} />
      )}

      {vault.hasVault && notes.selectedId && (
        <AnnotationPanel t={t} lang={lang} annotations={notes.annotations} loading={notes.annotationLoading} error={notes.annotationError} onOpenAsset={notes.openAnnotationAsset} saving={notes.saving} />
      )}
    </section>
  );
}

function NoteList({ t, lang, notes, saving, loading, onSelect, onArchive, onRestore }: {
  t: Dict;
  lang: 'zh' | 'en';
  notes: Note[];
  saving: boolean;
  loading: boolean;
  onSelect: (id: string) => void;
  onArchive: (note: Note) => Promise<void>;
  onRestore: (note: Note) => Promise<void>;
}) {
  return (
    <Card className={`span3 ${styles.notesList}`}>
      <CardHeader title={t.notes} />
      {loading ? (
        <p>{lang === 'zh' ? '加载中…' : 'Loading…'}</p>
      ) : notes.length ? (
        notes.map((note) => (
          <article className={`${styles.literatureItem} ${note.archivedAt ? styles.unavailable : ''}`} key={note.noteId}>
            <button className={`${styles.literatureItemDetail} ${styles.noteSelectButton}`} onClick={() => onSelect(note.noteId)} aria-label={`${t.openFile} ${note.title || '—'}`}>
              <h3>{note.title || '—'}{note.archivedAt && <small> · {t.noteArchived}</small>}</h3>
              <p>{new Date(note.updatedAt).toLocaleString(lang === 'zh' ? 'zh-CN' : 'en-US')}</p>
            </button>
            <div className={styles.itemActions}>
              {note.archivedAt ? (
                <Button variant="secondary" onClick={(e) => { e.stopPropagation(); void onRestore(note); }} disabled={saving}>
                  <Icon icon={Archive} size={14} /> {t.unarchiveNote}
                </Button>
              ) : (
                <Button variant="danger" onClick={(e) => { e.stopPropagation(); void onArchive(note); }} disabled={saving}>
                  <Icon icon={Archive} size={14} /> {t.archiveNote}
                </Button>
              )}
            </div>
          </article>
        ))
      ) : (
        <EmptyState title={t.noNotes} />
      )}
    </Card>
  );
}

function AnnotationPanel({ t, lang, annotations, loading, error, onOpenAsset, saving }: {
  t: Dict;
  lang: 'zh' | 'en';
  annotations: Annotation[];
  loading: boolean;
  error: string;
  onOpenAsset: (annotation: Annotation) => void;
  saving: boolean;
}) {
  return (
    <Card className={`span3 ${styles.annotationsPanel}`}>
      <CardHeader title={t.annotations} />
      {loading ? (
        <p>{lang === 'zh' ? '加载中…' : 'Loading…'}</p>
      ) : error ? (
        <ErrorBanner>{t.annotationLoadError}: {error}</ErrorBanner>
      ) : annotations.length ? (
        annotations.map((a) => (
          <article className={styles.literatureFile} key={a.annotationId}>
            <div className={styles.assetMeta}>
              <strong>{t.annotationPage} {a.anchor.pageNumber}</strong>
              <small>{a.anchor.selectedText}</small>
              <small>{t.annotationResolution}: {annotationStatusLabel(a.resolution, t)}</small>
            </div>
            <div className={styles.fileActions}>
              <Button variant="secondary" onClick={() => onOpenAsset(a)} disabled={a.resolution !== 'resolved_exact' || saving}>
                <Icon icon={ExternalLink} size={14} /> {t.openAssociatedAsset}
              </Button>
            </div>
          </article>
        ))
      ) : (
        <EmptyState title={t.noAnnotations} />
      )}
    </Card>
  );
}
