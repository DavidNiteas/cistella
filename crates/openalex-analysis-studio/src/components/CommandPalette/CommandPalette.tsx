import { useEffect, useRef } from 'react';
import { Database, BookOpen, NotebookPen, Search, BarChart3, Settings, FilePlus, Save, FolderOpen } from 'lucide-react';
import { Dialog, Icon } from '../ui';
import type { Dict } from '../../lib/i18n/dict';
import type { LiteratureState } from '../../hooks/useLiterature';
import type { NotesState } from '../../hooks/useNotes';
import type { RecentVaultDto, Workspace } from '../../types';
import styles from './CommandPalette.module.css';

export interface CommandPaletteProps {
  open: boolean;
  onClose: () => void;
  workspace: Workspace;
  setWorkspace: (workspace: Workspace) => void;
  recentVaults: RecentVaultDto[];
  onConnect: (path: string) => void;
  literature: LiteratureState;
  notes: NotesState;
  lang: 'zh' | 'en';
  t: Dict;
}

function workspaceLabel(t: Dict, id: Workspace, lang: 'zh' | 'en'): string {
  switch (id) {
    case 'vault':
      return t.vault;
    case 'reading':
      return t.reading;
    case 'notes':
      return t.notes;
    case 'search':
      return lang === 'zh' ? '搜索' : 'Search';
    case 'source':
      return t.source;
    case 'settings':
      return t.settings;
  }
}

const workspaceCommands: { id: Workspace; icon: typeof Database }[] = [
  { id: 'vault', icon: Database },
  { id: 'reading', icon: BookOpen },
  { id: 'notes', icon: NotebookPen },
  { id: 'search', icon: Search },
  { id: 'source', icon: BarChart3 },
  { id: 'settings', icon: Settings },
];

export function CommandPalette({ open, onClose, workspace, setWorkspace, recentVaults, onConnect, literature, notes, lang, t }: CommandPaletteProps) {
  const listRef = useRef<HTMLDivElement>(null);

  if (!open) return null;

  useEffect(() => {
    if (!open) return;
    const first = listRef.current?.querySelector('[data-command="true"]') as HTMLElement | null;
    first?.focus();
  }, [open]);

  const handleWorkspace = (id: Workspace) => {
    setWorkspace(id);
    onClose();
  };

  const handleNewLiterature = () => {
    if (workspace !== 'reading') setWorkspace('reading');
    if (!literature.editorOpen) literature.beginNew();
    onClose();
  };

  const handleSave = () => {
    if (workspace === 'reading' && literature.editorOpen) {
      void literature.save();
    } else if (workspace === 'notes' && notes.selectedId) {
      void notes.save();
    }
    onClose();
  };

  const handleConnect = (path: string) => {
    void onConnect(path);
    onClose();
  };

  return (
    <Dialog
      title={lang === 'zh' ? '命令面板' : 'Command Palette'}
      onClose={onClose}
      footer={
        <div className={styles.footerHint}>
          {lang === 'zh' ? 'Ctrl/Cmd+K 打开 · Esc 关闭 · ↑↓ 选择 · Enter 执行' : 'Ctrl/Cmd+K to open · Esc to close · ↑↓ to select · Enter to run'}
        </div>
      }
    >
      <div className={styles.section}>
        <h3 className={styles.sectionTitle}>{lang === 'zh' ? '切换工作区' : 'Switch workspace'}</h3>
        <div ref={listRef} className={styles.list} role="listbox" aria-label={lang === 'zh' ? '工作区命令' : 'Workspace commands'}>
          {workspaceCommands.map((cmd) => {
            const label = workspaceLabel(t, cmd.id, lang);
            return (
              <button
                key={cmd.id}
                data-command="true"
                className={[styles.item, workspace === cmd.id ? styles.active : ''].filter(Boolean).join(' ')}
                onClick={() => handleWorkspace(cmd.id)}
                aria-label={label}
                role="option"
                aria-selected={workspace === cmd.id}
              >
                <Icon icon={cmd.icon} size={16} />
                <span>{label}</span>
                {workspace === cmd.id && <small>{lang === 'zh' ? '当前' : 'current'}</small>}
              </button>
            );
          })}
        </div>
      </div>

      <div className={styles.section}>
        <h3 className={styles.sectionTitle}>{lang === 'zh' ? '操作' : 'Actions'}</h3>
        <div className={styles.list} role="listbox" aria-label={lang === 'zh' ? '操作命令' : 'Action commands'}>
          <button
            data-command="true"
            className={styles.item}
            onClick={handleNewLiterature}
            aria-label={t.addLiterature}
            role="option"
            aria-selected={false}
          >
            <Icon icon={FilePlus} size={16} />
            <span>{t.addLiterature}</span>
            <small>Ctrl/Cmd+N</small>
          </button>
          <button
            data-command="true"
            className={styles.item}
            onClick={handleSave}
            aria-label={t.saveLiterature}
            role="option"
            aria-selected={false}
          >
            <Icon icon={Save} size={16} />
            <span>{lang === 'zh' ? '保存' : 'Save'}</span>
            <small>Ctrl/Cmd+S</small>
          </button>
        </div>
      </div>

      {recentVaults.length > 0 && (
        <div className={styles.section}>
          <h3 className={styles.sectionTitle}>{t.recentVaults}</h3>
          <div className={styles.list} role="listbox" aria-label={t.recentVaults}>
            {recentVaults.map((r) => (
              <button
                key={r.path}
                data-command="true"
                className={styles.item}
                onClick={() => handleConnect(r.path)}
                aria-label={`${t.openVault} ${r.name || r.path}`}
                role="option"
                aria-selected={false}
                title={r.path}
              >
                <Icon icon={FolderOpen} size={16} />
                <span className={styles.truncate}>{r.name || r.path}</span>
              </button>
            ))}
          </div>
        </div>
      )}
    </Dialog>
  );
}
