import { useEffect } from 'react';
import type { Workspace } from '../types';
import { useToast } from '../components/ui/Toast/ToastProvider';

const workspaceOrder: Workspace[] = ['vault', 'reading', 'notes', 'search', 'source', 'settings'];

export interface KeyboardShortcutsOptions {
  workspace: Workspace;
  setWorkspace: (workspace: Workspace) => void;
  commandPaletteOpen: boolean;
  setCommandPaletteOpen: (open: boolean) => void;
  literature: {
    editorOpen: boolean;
    beginNew: () => void;
    closeEditor: () => void;
    save: () => Promise<void>;
  };
  notes: {
    selectedId: string | null;
    setSelectedId: (id: string | null) => void;
    save: () => Promise<void>;
  };
  newItemHint?: string;
  saveHint?: string;
}

function isTypingTarget(event: KeyboardEvent) {
  const target = event.target as HTMLElement | null;
  if (!target) return false;
  const tag = target.tagName;
  return tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || target.isContentEditable;
}

export function useKeyboardShortcuts({
  workspace,
  setWorkspace,
  commandPaletteOpen,
  setCommandPaletteOpen,
  literature,
  notes,
  newItemHint,
  saveHint,
}: KeyboardShortcutsOptions) {
  const toast = useToast();

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      const mod = event.metaKey || event.ctrlKey;

      if (mod && event.key.toLowerCase() === 'k') {
        event.preventDefault();
        setCommandPaletteOpen(true);
        return;
      }

      if (mod && event.key.toLowerCase() === 'n') {
        event.preventDefault();
        if (workspace === 'reading' && !literature.editorOpen) {
          literature.beginNew();
        } else {
          toast.push(newItemHint ?? 'Switch to Reading workspace to create a new item.', 'info');
        }
        return;
      }

      if (mod && event.key.toLowerCase() === 's') {
        event.preventDefault();
        if (workspace === 'reading' && literature.editorOpen) {
          void literature.save();
        } else if (workspace === 'notes' && notes.selectedId) {
          void notes.save();
        } else {
          toast.push(saveHint ?? 'Nothing to save in the current workspace.', 'info');
        }
        return;
      }

      if (mod && /^[1-6]$/.test(event.key)) {
        event.preventDefault();
        const index = Number(event.key) - 1;
        const next = workspaceOrder[index];
        if (next && next !== workspace) {
          setWorkspace(next);
        }
        return;
      }

      if (event.key === 'Escape') {
        if (commandPaletteOpen) {
          event.preventDefault();
          setCommandPaletteOpen(false);
          return;
        }
        if (workspace === 'reading' && literature.editorOpen) {
          event.preventDefault();
          literature.closeEditor();
          return;
        }
        if (workspace === 'notes' && notes.selectedId) {
          event.preventDefault();
          notes.setSelectedId(null);
          return;
        }
        return;
      }

      // Ignore other shortcuts while typing, except the handled ones above.
      if (isTypingTarget(event)) return;
    };

    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [workspace, setWorkspace, commandPaletteOpen, setCommandPaletteOpen, literature, notes, newItemHint, saveHint, toast]);
}
