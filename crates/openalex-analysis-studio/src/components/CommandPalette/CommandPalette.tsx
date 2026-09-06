import { useEffect, useRef } from 'react';
import { FilePlus, FolderOpen, Save } from 'lucide-react';
import { Dialog, Icon } from '../ui';
import type { Dict } from '../../lib/i18n/dict';
import type { LiteratureState } from '../../hooks/useLiterature';
import type { NotesState } from '../../hooks/useNotes';
import type { RecentVaultDto, Workspace } from '../../types';
import { getWorkspaceCategoryTitle, getWorkspaceEntryTitle, workspaceNavigationCategories } from '../layout/workspaceNavigation';
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

export function CommandPalette({ open, onClose, workspace, setWorkspace, recentVaults, onConnect, literature, notes, lang, t }: CommandPaletteProps) {
  const listRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const first = listRef.current?.querySelector('[data-command="true"]') as HTMLElement | null;
    first?.focus();
  }, [open]);

  if (!open) return null;

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
      {workspaceNavigationCategories.map((category) => {
        const categoryEntries = category.entries.filter((entry) => entry.kind === 'workspace' && entry.workspace);
        if (categoryEntries.length === 0 && category.id !== 'writing') return null;
        return (
          <div key={category.id} className={styles.section}>
            <div className={styles.sectionHeader}>
              <h3 className={styles.sectionTitle}>{getWorkspaceCategoryTitle(category, lang)}</h3>
              <span className={styles.sectionMeta}>{category.description[lang]}</span>
            </div>
            <div ref={category.id === 'reading' ? listRef : undefined} className={styles.list} role="listbox" aria-label={getWorkspaceCategoryTitle(category, lang)}>
              {category.entries.map((entry) => {
                const isActive = entry.workspace ? workspace === entry.workspace : false;
                const disabled = entry.kind !== 'workspace' || !entry.workspace;
                return (
                  <button
                    key={entry.id}
                    data-command={category.id === 'reading' ? 'true' : undefined}
                    className={[styles.item, isActive ? styles.active : '', disabled ? styles.disabled : ''].filter(Boolean).join(' ')}
                    onClick={disabled ? undefined : () => handleWorkspace(entry.workspace as Workspace)}
                    aria-label={entry.label[lang]}
                    role="option"
                    aria-selected={isActive}
                    aria-disabled={disabled}
                    disabled={disabled}
                    title={entry.description[lang]}
                  >
                    <Icon icon={entry.icon} size={16} />
                    <span className={styles.itemText}>
                      <span>{getWorkspaceEntryTitle(entry, lang)}</span>
                      <small>{entry.description[lang]}</small>
                    </span>
                    <small className={styles.itemMeta}>{category.label[lang]}</small>
                    {isActive && <small>{lang === 'zh' ? '??' : 'current'}</small>}
                  </button>
                );
              })}
            </div>
          </div>
        );
      })}

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
            <span className={styles.itemText}>
              <span>{t.addLiterature}</span>
              <small>{lang === 'zh' ? '在阅读页面中新建条目' : 'Create a new item in Reading'}</small>
            </span>
            <small className={styles.itemMeta}>Ctrl/Cmd+N</small>
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
            <span className={styles.itemText}>
              <span>{lang === 'zh' ? '保存' : 'Save'}</span>
              <small>{lang === 'zh' ? '保存当前可编辑内容' : 'Save the current editable content'}</small>
            </span>
            <small className={styles.itemMeta}>Ctrl/Cmd+S</small>
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
                <span className={styles.itemText}>
                  <span className={styles.truncate}>{r.name || r.path}</span>
                  <small>{t.openVault}</small>
                </span>
              </button>
            ))}
          </div>
        </div>
      )}
    </Dialog>
  );
}
