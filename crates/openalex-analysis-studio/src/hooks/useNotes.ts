import { useEffect, useState } from 'react';
import type { Dict } from '../lib/i18n/dict';
import type { Annotation, AnnotationResolution, Lang, LiteratureItem, Note, VaultConnection, VaultRequestContext } from '../types';
import { invoke } from '../lib/invoke';
import { parseNoteConflict } from '../lib/utils';
import { useToast } from '../components/ui/Toast/ToastProvider';

export interface NotesState {
  items: Note[];
  filterItemId: string;
  setFilterItemId: (value: string) => void;
  selectedId: string | null;
  setSelectedId: (value: string | null) => void;
  editorTitle: string;
  setEditorTitle: (value: string) => void;
  editorBody: string;
  setEditorBody: (value: string) => void;
  loading: boolean;
  saving: boolean;
  error: string;
  conflict: { revision: string; message: string } | null;
  annotations: Annotation[];
  annotationLoading: boolean;
  annotationError: string;
  load: (request?: VaultRequestContext) => Promise<void>;
  beginNew: () => void;
  save: () => Promise<void>;
  refreshSelected: () => Promise<void>;
  overwrite: () => Promise<void>;
  archive: (note: Note) => Promise<void>;
  restore: (note: Note) => Promise<void>;
  openAnnotationAsset: (annotation: Annotation) => void;
}

export function useNotes(vault: VaultConnection, literatureItems: LiteratureItem[], t: Dict, _lang: Lang): NotesState {
  const [items, setItems] = useState<Note[]>([]);
  const [filterItemId, setFilterItemId] = useState('all');
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [editorTitle, setEditorTitle] = useState('');
  const [editorBody, setEditorBody] = useState('');
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const toast = useToast();
  const [conflict, setConflict] = useState<{ revision: string; message: string } | null>(null);
  const [annotations, setAnnotations] = useState<Annotation[]>([]);
  const [annotationLoading, setAnnotationLoading] = useState(false);
  const [annotationError, setAnnotationError] = useState('');

  const runNoteAction = async (action: (request: VaultRequestContext) => Promise<void>) => {
    const request = vault.captureVaultRequest();
    if (!vault.isCurrentVaultRequest(request)) return;
    setSaving(true);
    setError('');
    try {
      await action(request);
    } catch (e: any) {
      if (!vault.isCurrentVaultRequest(request)) return;
      const conflictInfo = parseNoteConflict(e?.message ?? e);
      if (conflictInfo) {
        setConflict({ revision: '', message: conflictInfo.message });
      } else {
        setError(String(e?.message ?? e));
      }
    } finally {
      if (vault.isCurrentVaultRequest(request)) setSaving(false);
    }
  };

  const load = async (request?: VaultRequestContext) => {
    const req = request ?? vault.captureVaultRequest();
    if (!req.expectedVaultPath || !vault.isCurrentVaultRequest(req)) return;
    setLoading(true);
    setError('');
    try {
      const value = await invoke<Note[]>('list_notes', {
        itemId: filterItemId === 'all' ? null : filterItemId,
        includeArchived: true,
      });
      if (!vault.isCurrentVaultRequest(req)) return;
      setItems(
        Array.isArray(value)
          ? value.sort((a, b) => new Date(b.updatedAt).getTime() - new Date(a.updatedAt).getTime())
          : []
      );
    } catch (e: any) {
      if (vault.isCurrentVaultRequest(req)) setError(String(e?.message ?? e));
    } finally {
      if (vault.isCurrentVaultRequest(req)) setLoading(false);
    }
  };

  const loadAnnotations = async (request: VaultRequestContext, itemId: string) => {
    if (!request.expectedVaultPath || !vault.isCurrentVaultRequest(request)) return;
    setAnnotationLoading(true);
    setAnnotationError('');
    try {
      const value = await invoke<Annotation[]>('list_annotations', { itemId });
      if (!vault.isCurrentVaultRequest(request)) return;
      const list = Array.isArray(value) ? value : [];
      const resolved = await Promise.all(
        list.map(async (annotation) => {
          try {
            const resolution = await invoke<AnnotationResolution>('annotation_resolution', {
              annotationId: annotation.annotationId,
              req: request,
            });
            if (!vault.isCurrentVaultRequest(request)) return annotation;
            return { ...annotation, resolution };
          } catch {
            return annotation;
          }
        })
      );
      if (vault.isCurrentVaultRequest(request)) {
        setAnnotations(resolved.sort((a, b) => new Date(b.updatedAt).getTime() - new Date(a.updatedAt).getTime()));
      }
    } catch (e: any) {
      if (vault.isCurrentVaultRequest(request)) setAnnotationError(String(e?.message ?? e));
    } finally {
      if (vault.isCurrentVaultRequest(request)) setAnnotationLoading(false);
    }
  };

  // Reload notes when the item filter changes while the workspace is active.
  useEffect(() => {
    if (!vault.hasVault) return;
    void load();
  }, [filterItemId, vault.hasVault, vault.vaultPath]);

  const beginNew = () => {
    setSelectedId(null);
    setEditorTitle('');
    setEditorBody('');
    setConflict(null);
    setError('');
  };

  const save = async () => {
    await runNoteAction(async (request) => {
      const title = editorTitle.trim();
      if (!title) return;
      if (!selectedId && filterItemId === 'all') {
        setError(t.chooseItemForNote);
        return;
      }
      const isConflict = Boolean(conflict);
      setConflict(null);
      const body = editorBody;
      if (selectedId) {
        if (isConflict) {
          const refreshed = await invoke<Note>('get_note', { noteId: selectedId });
          if (!vault.isCurrentVaultRequest(request)) return;
          const note = await invoke<Note>('update_note', {
            noteId: refreshed.noteId,
            expectedRevision: refreshed.revision,
            title,
            markdownBody: body,
            req: request,
          });
          if (!vault.isCurrentVaultRequest(request)) return;
          setSelectedId(note.noteId);
        } else {
          const current = items.find((n) => n.noteId === selectedId);
          const note = await invoke<Note>('update_note', {
            noteId: selectedId,
            expectedRevision: current?.revision ?? '',
            title,
            markdownBody: body,
            req: request,
          });
          if (!vault.isCurrentVaultRequest(request)) return;
          setSelectedId(note.noteId);
        }
      } else {
        const note = await invoke<Note>('create_note', {
          itemId: filterItemId,
          title,
          markdownBody: body,
          req: request,
        });
        if (!vault.isCurrentVaultRequest(request)) return;
        setSelectedId(note.noteId);
      }
      toast.push(t.noteSaved, 'success');
      await load(request);
    });
  };

  const refreshSelected = async () => {
    await runNoteAction(async (request) => {
      if (!selectedId) return;
      const refreshed = await invoke<Note>('get_note', { noteId: selectedId });
      if (!vault.isCurrentVaultRequest(request)) return;
      setEditorTitle(refreshed.title);
      setEditorBody(refreshed.markdownBody);
      setConflict(null);
    });
  };

  const overwrite = async () => {
    await runNoteAction(async (request) => {
      if (!selectedId) return;
      const refreshed = await invoke<Note>('get_note', { noteId: selectedId });
      if (!vault.isCurrentVaultRequest(request)) return;
      const title = editorTitle.trim();
      if (!title) return;
      const note = await invoke<Note>('update_note', {
        noteId: refreshed.noteId,
        expectedRevision: refreshed.revision,
        title,
        markdownBody: editorBody,
        req: request,
      });
      if (!vault.isCurrentVaultRequest(request)) return;
      setConflict(null);
      setSelectedId(note.noteId);
      toast.push(t.noteSaved, 'success');
      await load(request);
    });
  };

  const archive = async (note: Note) => {
    await runNoteAction(async (request) => {
      await invoke('archive_note', { noteId: note.noteId, expectedRevision: note.revision, req: request });
      if (!vault.isCurrentVaultRequest(request)) return;
      toast.push(t.archived, 'success');
      if (selectedId === note.noteId) setSelectedId(null);
      await load(request);
    });
  };

  const restore = async (note: Note) => {
    await runNoteAction(async (request) => {
      await invoke('unarchive_note', { noteId: note.noteId, expectedRevision: note.revision, req: request });
      if (!vault.isCurrentVaultRequest(request)) return;
      toast.push(t.unarchiveNote, 'success');
      await load(request);
    });
  };

  const openAnnotationAsset = (annotation: Annotation) => {
    void runNoteAction(async (request) => {
      if (annotation.resolution !== 'resolved_exact') return;
      await invoke('open_annotation_asset', { annotationId: annotation.annotationId, req: request });
      if (!vault.isCurrentVaultRequest(request)) return;
      toast.push(t.fileRequestAccepted, 'info');
    });
  };

  useEffect(() => {
    if (!selectedId) {
      setEditorTitle('');
      setEditorBody('');
      setConflict(null);
      setAnnotations([]);
      return;
    }
    const note = items.find((n) => n.noteId === selectedId);
    if (!note) return;
    setEditorTitle(note.title);
    setEditorBody(note.markdownBody);
    setConflict(null);
    const request = vault.captureVaultRequest();
    void loadAnnotations(request, note.itemId);
  }, [selectedId, items]);

  return {
    items,
    filterItemId,
    setFilterItemId,
    selectedId,
    setSelectedId,
    editorTitle,
    setEditorTitle,
    editorBody,
    setEditorBody,
    loading,
    saving,
    error,
    conflict,
    annotations,
    annotationLoading,
    annotationError,
    load,
    beginNew,
    save,
    refreshSelected,
    overwrite,
    archive,
    restore,
    openAnnotationAsset,
  };
}
