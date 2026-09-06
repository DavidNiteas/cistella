import { useState } from 'react';
import type { Dict } from '../lib/i18n/dict';
import type { ImportPreview, ImportRequest, VaultConnection, VaultContext } from '../types';
import type { VaultContextValue } from './useVaultContext';
import { invoke } from '../lib/invoke';
import { summarizeManifest, short } from '../lib/utils';
import { useToast } from '../components/ui/Toast/ToastProvider';

export interface VaultImportState {
  rawDir: string;
  setRawDir: (value: string) => void;
  importPreview: ImportPreview | null;
  importError: string;
  lastImport: ImportRequest | null;
  buildArrow: boolean;
  setBuildArrow: (value: boolean) => void;
  inspectSources: (path: string) => Promise<void>;
  importVault: () => Promise<void>;
  retryImport: () => void;
}

export function useVaultImport(vault: VaultConnection, context: VaultContextValue, t: Dict): VaultImportState {
  const [rawDir, setRawDir] = useState('');
  const [importPreview, setImportPreview] = useState<ImportPreview | null>(null);
  const [lastImport, setLastImport] = useState<ImportRequest | null>(null);
  const [importError, setImportError] = useState('');
  const [buildArrow, setBuildArrow] = useState(true);
  const toast = useToast();

  const inspectSources = async (path: string) => {
    if (!path) {
      vault.setStatus(t.chooseFirst);
      return;
    }
    try {
      const preview = await invoke<ImportPreview>('inspect_sources', { rawSourcesDir: path });
      setImportPreview(preview);
      setRawDir(path);
      vault.setStatus(`${t.inspectSources}: ${path}`);
      toast.push(`${t.inspectSources}: ${short(path)}`, 'info');
    } catch (e: any) {
      setImportPreview(null);
      const message = `${t.failed}: ${e?.message ?? e}`;
      vault.setStatus(message);
      toast.push(message, 'error');
    }
  };

  const performImport = async (request: ImportRequest) => {
    setLastImport(request);
    setImportError('');
    try {
      const ctx = await invoke<VaultContext>('import_to_library', { req: request });
      const outputDir = typeof ctx?.root === 'string' ? ctx.root : request.outputDir;
      vault.setVaultPath(outputDir);
      context.applyContext(ctx);
      const summaryId = summarizeManifest(ctx?.manifest ?? {}).vault_id;
      const status = `${t.imported}: ${short(request.outputDir)}${summaryId ? ` · ${summaryId}` : ''}`;
      vault.setStatus(status);
      toast.push(status, 'success');
      await vault.connect(outputDir);
    } catch (e: any) {
      const message = String(e?.message ?? e);
      setImportError(message);
      toast.push(`${t.importFailed}: ${message}`, 'error');
      throw e;
    }
  };

  const importVault = async () => {
    await vault.run(async () => {
      if (!rawDir || !vault.vaultPath) {
        vault.setStatus(t.chooseFirst);
        return;
      }
      if (!importPreview) {
        await inspectSources(rawDir);
        return;
      }
      await performImport({ rawSourcesDir: rawDir, outputDir: vault.vaultPath, buildArrowCache: buildArrow });
    });
  };

  const retryImport = () => {
    if (!lastImport) return;
    void vault.run(() => performImport(lastImport));
  };

  return {
    rawDir,
    setRawDir,
    importPreview,
    importError,
    lastImport,
    buildArrow,
    setBuildArrow,
    inspectSources,
    importVault,
    retryImport,
  };
}
