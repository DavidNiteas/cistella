import { Database, BookOpen, NotebookPen, Search, BarChart3, Settings, PanelLeft } from 'lucide-react';
import styles from './Sidebar.module.css';
import { Icon } from '../../ui/Icon/Icon';

export type Workspace = 'vault' | 'reading' | 'notes' | 'search' | 'source' | 'settings';

export interface SidebarLabels extends Record<Workspace, string> {
  sub: string;
  workspaceSection: string;
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
}

const workspaceIcons: Record<Workspace, typeof Database> = {
  vault: Database,
  reading: BookOpen,
  notes: NotebookPen,
  search: Search,
  source: BarChart3,
  settings: Settings,
};

const workspaceOrder: Workspace[] = ['vault', 'reading', 'notes', 'search', 'source', 'settings'];

export function Sidebar({ active, onChange, labels, status, version, collapsed = false, onToggleCollapse }: SidebarProps) {
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
      {!collapsed && <em className={styles.sectionLabel}>{labels.workspaceSection}</em>}
      <nav className={styles.nav} aria-label="Workspace navigation">
        {workspaceOrder.map((key) => (
          <button
            key={key}
            className={[styles.navButton, active === key ? styles.active : ''].filter(Boolean).join(' ')}
            onClick={() => onChange(key)}
            aria-current={active === key ? 'page' : undefined}
            title={labels[key]}
          >
            <Icon icon={workspaceIcons[key]} size={18} />
            {!collapsed && <span className={styles.label}>{labels[key]}</span>}
          </button>
        ))}
      </nav>
      <div className={styles.bottom}>
        {!collapsed && status && <small>{status}</small>}
        {!collapsed && version && <small>{labels.version}: {version}</small>}
      </div>
    </aside>
  );
}
