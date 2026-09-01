import { useEffect, useState } from 'react';
import { Shell, Sidebar, StatusBar } from './components/layout';
import { VaultPage, ReadingPage, SearchPage, SourcePage, NotesPage, SettingsPage } from './features';
import { useI18n, useVaultConnection, useVaultContext, useVaultImport, useLiterature, useReadingSessions, useSourceAnalysis, useLocalSearch, useNotes, useSettings } from './hooks';
import { useKeyboardShortcuts } from './hooks/useKeyboardShortcuts';
import { CommandPalette } from './components/CommandPalette/CommandPalette';
import { loadWorkspace } from './lib/utils';
import './style.css';

export default function App() {
  const { lang, setLang, t } = useI18n();
  const vault = useVaultConnection(t);
  const context = useVaultContext(vault, t);
  const vaultImport = useVaultImport(vault, context, t);
  const literature = useLiterature(vault, context, t);
  const reading = useReadingSessions(vault, t);
  const source = useSourceAnalysis(vault, t, lang);
  const [workspace, setWorkspace] = useState(loadWorkspace);
  const [commandPaletteOpen, setCommandPaletteOpen] = useState(false);
  const [sidebarCollapsed, setSidebarCollapsed] = useState(() => window.innerWidth < 1024);
  const activeWorkspace = vault.restored ? workspace : 'vault';
  const search = useLocalSearch(vault, literature.items, t, lang, activeWorkspace === 'search');

  useEffect(() => {
    const onResize = () => setSidebarCollapsed(window.innerWidth < 1024);
    window.addEventListener('resize', onResize);
    return () => window.removeEventListener('resize', onResize);
  }, []);
  const notes = useNotes(vault, literature.items, t, lang);
  const settings = useSettings(vault, source.adapters, t);

  useEffect(() => {
    localStorage.setItem('workspace', workspace);
  }, [workspace]);

  const handleConnect = async (path: string) => {
    const ctx = await vault.connect(path);
    if (!ctx) return;
    await context.applyContext(ctx);
    await Promise.all([literature.load(), source.refresh(undefined, true)]);
    setWorkspace('source');
  };

  // Load core data whenever the vault connection changes.
  useEffect(() => {
    const request = vault.captureVaultRequest();
    if (!vault.hasVault || !vault.restored) {
      void literature.loadItems(request);
      void reading.load(request);
      void search.refreshHealth(request);
      void notes.load(request);
      void source.refresh(undefined, false, request);
      return;
    }
    void Promise.all([literature.load(request), source.refresh(undefined, true, request), context.refresh(request)]);
  }, [vault.vaultPath, vault.restored]);

  // Load workspace-specific data when the active workspace changes.
  useEffect(() => {
    if (!vault.hasVault || !vault.restored) return;
    const request = vault.captureVaultRequest();
    if (activeWorkspace === 'reading') {
      void literature.load(request);
      void reading.load(request);
    } else if (activeWorkspace === 'notes') {
      void notes.load(request);
    } else if (activeWorkspace === 'vault') {
      void literature.load(request);
    } else if (activeWorkspace === 'search') {
      void search.refreshHealth(request);
    }
  }, [activeWorkspace, vault.hasVault, vault.restored, vault.vaultPath]);

  const sidebarLabels = {
    vault: t.vault,
    reading: t.reading,
    notes: t.notes,
    search: t.search,
    source: t.source,
    settings: t.settings,
    sub: t.sub,
    workspaceSection: lang === 'zh' ? '工作区' : 'Workspace',
    version: t.version,
  };

  useKeyboardShortcuts({
    workspace: activeWorkspace,
    setWorkspace,
    commandPaletteOpen,
    setCommandPaletteOpen,
    literature: {
      editorOpen: literature.editorOpen,
      beginNew: literature.beginNew,
      closeEditor: literature.closeEditor,
      save: literature.save,
    },
    notes: {
      selectedId: notes.selectedId,
      setSelectedId: notes.setSelectedId,
      save: notes.save,
    },
    newItemHint: lang === 'zh' ? '请先切换到 Reading 工作区再新建条目。' : 'Switch to the Reading workspace to create a new item.',
    saveHint: lang === 'zh' ? '当前工作区没有可保存的编辑器。' : 'There is nothing to save in the current workspace.',
  });

  return (
    <>
      <CommandPalette
        open={commandPaletteOpen}
        onClose={() => setCommandPaletteOpen(false)}
        workspace={activeWorkspace}
        setWorkspace={setWorkspace}
        recentVaults={vault.recentVaults}
        onConnect={handleConnect}
        literature={literature}
        notes={notes}
        lang={lang}
        t={t}
      />
      <Shell
        sidebar={
          <Sidebar
            active={activeWorkspace}
            onChange={setWorkspace}
            labels={sidebarLabels}
            status={vault.status}
            version={vault.appVersion}
            collapsed={sidebarCollapsed}
            onToggleCollapse={() => setSidebarCollapsed((v) => !v)}
          />
        }
        statusBar={<StatusBar vaultPath={vault.vaultPath} status={vault.status} busy={vault.busy} error={Boolean(vault.vaultError || source.analysisError)} />}
      >
        {activeWorkspace === 'vault' && (
          <VaultPage vault={vault} context={context} importState={vaultImport} literatureItems={literature.items} t={t} lang={lang} onConnect={handleConnect} />
        )}
        {activeWorkspace === 'reading' && (
          <ReadingPage vault={vault} literature={literature} reading={reading} context={context} t={t} lang={lang} onConnect={handleConnect} />
        )}
        {activeWorkspace === 'search' && (
          <SearchPage vault={vault} search={search} items={literature.items} t={t} lang={lang} />
        )}
        {activeWorkspace === 'source' && (
          <SourcePage vault={vault} source={source} context={context} t={t} lang={lang} onConnect={handleConnect} />
        )}
        {activeWorkspace === 'notes' && (
          <NotesPage vault={vault} notes={notes} items={literature.items} t={t} lang={lang} />
        )}
        {activeWorkspace === 'settings' && (
          <SettingsPage vault={vault} settings={settings} adapters={source.adapters} t={t} lang={lang} onLangChange={setLang} />
        )}
      </Shell>
    </>
  );
}
