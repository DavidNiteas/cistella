import type { LucideIcon } from 'lucide-react';
import { BarChart3, BookOpen, Database, NotebookPen, Search, Settings, Sparkles } from 'lucide-react';
import type { Workspace } from '../../types';

export type Lang = 'zh' | 'en';
export type WorkspaceCategoryId = 'reading' | 'writing' | 'management' | 'analysis' | 'tools';

export interface WorkspaceNavigationEntry {
  id: string;
  workspace?: Workspace;
  icon: LucideIcon;
  label: Record<Lang, string>;
  description: Record<Lang, string>;
  kind: 'workspace' | 'placeholder';
  shortcut?: string;
}

export interface WorkspaceNavigationCategory {
  id: WorkspaceCategoryId;
  icon: LucideIcon;
  label: Record<Lang, string>;
  description: Record<Lang, string>;
  entries: WorkspaceNavigationEntry[];
}

export const workspaceNavigationCategories: WorkspaceNavigationCategory[] = [
  {
    id: 'reading',
    icon: BookOpen,
    label: { zh: '阅读', en: 'Reading' },
    description: { zh: '阅读与文献处理入口', en: 'Reading and literature entry points' },
    entries: [
      { id: 'reading-workspace', workspace: 'reading', icon: BookOpen, label: { zh: '阅读', en: 'Reading' }, description: { zh: '浏览和管理当前库中的文献', en: 'Browse and manage literature in the current library' }, kind: 'workspace', shortcut: 'Ctrl/Cmd+2' },
      { id: 'notes-workspace', workspace: 'notes', icon: NotebookPen, label: { zh: '笔记', en: 'Notes' }, description: { zh: '管理 Markdown 笔记和标注', en: 'Manage Markdown notes and annotations' }, kind: 'workspace', shortcut: 'Ctrl/Cmd+3' },
      { id: 'search-reading', workspace: 'search', icon: Search, label: { zh: '检索', en: 'Search' }, description: { zh: '在阅读过程中查找相关内容', en: 'Find relevant content while reading' }, kind: 'workspace', shortcut: 'Ctrl/Cmd+4' },
    ],
  },
  {
    id: 'writing',
    icon: Sparkles,
    label: { zh: '写作', en: 'Writing' },
    description: { zh: '组织写作和输出内容', en: 'Organize writing and outputs' },
    entries: [
      { id: 'writing-placeholder', icon: Sparkles, label: { zh: '写作', en: 'Writing' }, description: { zh: '即将推出，当前暂不可用', en: 'Coming soon and currently unavailable' }, kind: 'placeholder' },
    ],
  },
  {
    id: 'management',
    icon: Database,
    label: { zh: '管理', en: 'Management' },
    description: { zh: '管理库、资产和数据流转', en: 'Manage libraries, assets, and data flows' },
    entries: [
      { id: 'vault-workspace', workspace: 'vault', icon: Database, label: { zh: '库管理', en: 'Library' }, description: { zh: '打开、导入和管理当前 cistella 库', en: 'Open, import, and manage the current cistella library' }, kind: 'workspace', shortcut: 'Ctrl/Cmd+1' },
    ],
  },
  {
    id: 'analysis',
    icon: BarChart3,
    label: { zh: '分析', en: 'Analysis' },
    description: { zh: '分析数据并进行全文检索', en: 'Analyze data and search full text' },
    entries: [
      { id: 'source-workspace', workspace: 'source', icon: BarChart3, label: { zh: '来源分析', en: 'Source analysis' }, description: { zh: '浏览、筛选和导出来源数据', en: 'Browse, filter, and export source data' }, kind: 'workspace', shortcut: 'Ctrl/Cmd+5' },
      { id: 'search-analysis', workspace: 'search', icon: Search, label: { zh: '全文检索', en: 'Full-text search' }, description: { zh: '在本地文献内容中进行全文检索', en: 'Search the contents of local literature' }, kind: 'workspace', shortcut: 'Ctrl/Cmd+4' },
    ],
  },
  {
    id: 'tools',
    icon: Settings,
    label: { zh: '工具', en: 'Tools' },
    description: { zh: '设置与诊断工具', en: 'Settings and diagnostic tools' },
    entries: [
      { id: 'settings-workspace', workspace: 'settings', icon: Settings, label: { zh: '设置', en: 'Settings' }, description: { zh: '配置界面、语言和更新选项', en: 'Configure interface, language, and updates' }, kind: 'workspace', shortcut: 'Ctrl/Cmd+6' },
    ],
  },
];

export function getWorkspaceCategoryIds(workspace: Workspace): WorkspaceCategoryId[] {
  return workspaceNavigationCategories
    .filter((category) => category.entries.some((entry) => entry.workspace === workspace))
    .map((category) => category.id);
}

export function getWorkspaceCategoryTitle(category: WorkspaceNavigationCategory, lang: Lang): string {
  return category.label[lang];
}

export function getWorkspaceCategoryDescription(category: WorkspaceNavigationCategory, lang: Lang): string {
  return category.description[lang];
}

export function getWorkspaceEntryTitle(entry: WorkspaceNavigationEntry, lang: Lang): string {
  return entry.label[lang];
}
