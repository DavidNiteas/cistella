import { useEffect, useMemo, useState } from 'react';
import { ChevronDown, ChevronRight, PanelLeft } from 'lucide-react';
import styles from './Sidebar.module.css';
import { Icon } from '../../ui/Icon/Icon';
import { getWorkspaceCategoryDescription, getWorkspaceCategoryIds, getWorkspaceCategoryTitle, getWorkspaceEntryTitle, workspaceNavigationCategories, type WorkspaceCategoryId } from '../workspaceNavigation';
import type { Workspace, WorkspaceContract } from '../../../types';

export interface SidebarLabels {
  sub: string;
  navigationSection: string;
  version: string;
}

export interface SidebarProps {
  active: Workspace;
  onChange: (workspace: Workspace) => void;
  labels: SidebarLabels;
  status?: string;
  version?: string | null;
  collapsed?: boolean;
  onToggleCollapse?: () => void;
  lang: 'zh' | 'en';
  workspaceContract?: WorkspaceContract | null;
}

const collapsedStorageKey = 'sidebar.collapsedCategories';

function loadCollapsedCategories(): string[] {
  try {
    const raw = localStorage.getItem(collapsedStorageKey);
    const parsed = raw ? JSON.parse(raw) : [];
    return Array.isArray(parsed) ? parsed.filter((value): value is string => typeof value === 'string') : [];
  } catch {
    return [];
  }
}

export function Sidebar({ active, onChange, labels, status, version, collapsed = false, onToggleCollapse, lang, workspaceContract }: SidebarProps) {
  const forcedOpenCategoryIds = useMemo(() => new Set(getWorkspaceCategoryIds(active)), [active]);
  const [collapsedCategoryIds, setCollapsedCategoryIds] = useState<string[]>(loadCollapsedCategories);

  useEffect(() => {
    try {
      localStorage.setItem(collapsedStorageKey, JSON.stringify(collapsedCategoryIds));
    } catch {
      /* keep UI state only */
    }
  }, [collapsedCategoryIds]);

  const isCategoryOpen = (categoryId: WorkspaceCategoryId) => forcedOpenCategoryIds.has(categoryId) || !collapsedCategoryIds.includes(categoryId);

  const toggleCategory = (categoryId: WorkspaceCategoryId) => {
    if (forcedOpenCategoryIds.has(categoryId)) return;
    setCollapsedCategoryIds((current) =>
      current.includes(categoryId) ? current.filter((value) => value !== categoryId) : [...current, categoryId],
    );
  };

  return (
    <aside className={[styles.sidebar, collapsed ? styles.collapsed : ''].filter(Boolean).join(' ')}>
      <div className={styles.brand}>
        <span className={styles.logo}>C</span>
        {!collapsed && (
          <div className={styles.title}>
            <b>cistella</b>
            <small>{labels.sub}</small>
          </div>
        )}
        <button className={styles.collapseToggle} onClick={onToggleCollapse} aria-label={collapsed ? 'Expand sidebar' : 'Collapse sidebar'}>
          <Icon icon={PanelLeft} size={16} />
        </button>
      </div>
      {!collapsed && <em className={styles.sectionLabel}>{labels.navigationSection}</em>}
      <nav className={styles.nav} aria-label={workspaceContract ? `${workspaceContract.schemaVersion} Library navigation` : 'Library navigation'}>
        {workspaceNavigationCategories.map((category) => {
          const categoryId = category.id as WorkspaceCategoryId;
          const open = isCategoryOpen(categoryId);
          const forcedOpen = forcedOpenCategoryIds.has(categoryId);
          const categoryTitle = getWorkspaceCategoryTitle(category, lang);
          const categoryDescription = getWorkspaceCategoryDescription(category, lang);
          return (
            <section key={categoryId} className={[styles.category, open ? styles.categoryOpen : '', forcedOpen ? styles.categoryForcedOpen : ''].filter(Boolean).join(' ')}>
              <button
                className={styles.categoryButton}
                onClick={() => toggleCategory(categoryId)}
                aria-expanded={open}
                aria-disabled={forcedOpen}
                disabled={forcedOpen}
                title={`${categoryTitle}
${categoryDescription}`}
              >
                <Icon icon={category.icon} size={16} />
                {!collapsed && <span className={styles.categoryLabel}>{categoryTitle}</span>}
                {!collapsed && <Icon icon={open ? ChevronDown : ChevronRight} size={14} className={styles.chevron} />}
              </button>

              {open && (
                <div className={styles.entries}>
                  {category.entries.map((entry) => {
                    const isActive = entry.workspace ? entry.workspace === active : false;
                    const entryTitle = getWorkspaceEntryTitle(entry, lang);
                    const description = entry.description[lang];
                    const disabled = entry.kind !== 'workspace' || !entry.workspace;
                    const shortcut = entry.shortcut ? ` (${entry.shortcut})` : '';
                    return (
                      <button
                        key={entry.id}
                        className={[styles.entryButton, isActive ? styles.entryActive : '', disabled ? styles.entryDisabled : ''].filter(Boolean).join(' ')}
                        onClick={() => entry.workspace && onChange(entry.workspace)}
                        aria-current={isActive ? 'page' : undefined}
                        aria-disabled={disabled}
                        disabled={disabled}
                        title={`${entryTitle}
${description}${shortcut}`}
                      >
                        <Icon icon={entry.icon} size={16} />
                        {!collapsed && <span className={styles.entryLabel}>{entryTitle}</span>}
                      </button>
                    );
                  })}
                </div>
              )}
            </section>
          );
        })}
      </nav>
      <div className={styles.bottom}>
        {!collapsed && status && <small>{status}</small>}
        {!collapsed && version && <small>{labels.version}: {version}</small>}
      </div>
    </aside>
  );
}