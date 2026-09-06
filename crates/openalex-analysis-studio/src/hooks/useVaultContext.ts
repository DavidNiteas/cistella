import { useState } from 'react';
import type { Dict } from '../lib/i18n/dict';
import type {
  DocumentAsset,
  DocumentAssetKind,
  VaultConnection,
  VaultContext,
  VaultRequestContext,
  VaultSummary,
} from '../types';
import { invoke } from '../lib/invoke';
import { pdfFile, summarizeManifest } from '../lib/utils';
import { useToast } from '../components/ui/Toast/ToastProvider';

export interface VaultContextValue {
  summary: VaultSummary | null;
  tableCount: number | null;
  assets: DocumentAsset[];
  removeTarget: DocumentAsset | null;
  requestRemoveAsset: (asset: DocumentAsset | null) => void;
  confirmRemoveAsset: () => Promise<void>;
  importAsset: (itemId: string, assetKind?: DocumentAssetKind) => void;
  linkExternalAsset: (itemId: string, assetKind?: DocumentAssetKind) => void;
  openAsset: (asset: DocumentAsset) => void;
  setAssetKind: (asset: DocumentAsset, assetKind: DocumentAssetKind) => void;
  setAssetDefault: (asset: DocumentAsset) => void;
  migrateAsset: (asset: DocumentAsset) => void;
  refresh: (request?: VaultRequestContext) => Promise<boolean>;
  applyContext: (ctx: VaultContext, request?: VaultRequestContext) => Promise<void>;
}

export function useVaultContext(vault: VaultConnection, t: Dict): VaultContextValue {
  const [summary, setSummary] = useState<VaultSummary | null>(null);
  const [tableCount, setTableCount] = useState<number | null>(null);
  const [assets, setAssets] = useState<DocumentAsset[]>([]);
  const [removeTarget, setRemoveTarget] = useState<DocumentAsset | null>(null);
  const toast = useToast();

  const runAssetAction = async (action: (request: VaultRequestContext) => Promise<string | void>) => {
    const request = vault.captureVaultRequest();
    if (!vault.isCurrentVaultRequest(request)) return;
    vault.setBusy(true);
    try {
      const message = await action(request);
      if (message && vault.isCurrentVaultRequest(request)) toast.push(message, 'success');
    } catch (e: any) {
      if (vault.isCurrentVaultRequest(request)) toast.push(`${t.fileOperationFailed}: ${e?.message ?? e}`, 'error');
    } finally {
      vault.setBusy(false);
    }
  };

  const loadAssets = async (request: VaultRequestContext) => {
    if (!request.expectedVaultPath) {
      if (vault.isCurrentVaultRequest(request)) setAssets([]);
      return;
    }
    try {
      const value = await invoke<DocumentAsset[]>('list_document_assets');
      if (vault.isCurrentVaultRequest(request)) {
        setAssets(Array.isArray(value) ? value : []);
      }
    } catch (e: any) {
      if (vault.isCurrentVaultRequest(request)) toast.push(`${t.fileOperationFailed}: ${e?.message ?? e}`, 'error');
    }
  };

  const applyContext = async (ctx: VaultContext, request?: VaultRequestContext) => {
    const req = request ?? vault.captureVaultRequest();
    if (!vault.isCurrentVaultRequest(req)) return;
    setSummary(summarizeManifest(ctx?.manifest ?? {}));
    setTableCount(typeof ctx?.tableCount === 'number' ? ctx.tableCount : null);
    await loadAssets(req);
  };

  const refresh = async (request?: VaultRequestContext): Promise<boolean> => {
    const req = request ?? vault.captureVaultRequest();
    if (!req.expectedVaultPath) return false;
    try {
      const ctx = await invoke<VaultContext>('library_context');
      if (!vault.isCurrentVaultRequest(req)) return false;
      await applyContext(ctx, req);
      return true;
    } catch (e: any) {
      if (vault.isCurrentVaultRequest(req)) {
        toast.push(`${t.fileOperationFailed}: ${e?.message ?? e}`, 'error');
      }
      return false;
    }
  };

  const importAsset = (itemId: string, assetKind: DocumentAssetKind = 'primary') => {
    void runAssetAction(async (request) => {
      const sourcePath = await pdfFile();
      if (!sourcePath || !vault.isCurrentVaultRequest(request)) return;
      const result = await invoke<{ outcome: string }>('import_document_asset', { itemId, sourcePath, assetKind });
      await loadAssets(request);
      return result.outcome === 'duplicate' ? t.assetDuplicate : t.imported;
    });
  };

  const linkExternalAsset = (itemId: string, assetKind: DocumentAssetKind = 'primary') => {
    void runAssetAction(async (request) => {
      const sourcePath = await pdfFile();
      if (!sourcePath || !vault.isCurrentVaultRequest(request)) return;
      await invoke('link_external_document_asset', { itemId, sourcePath, assetKind });
      await loadAssets(request);
      return t.assetLinked;
    });
  };

  const openAsset = (asset: DocumentAsset) => {
    void runAssetAction(async (_request) => {
      await invoke('open_document_asset', { itemId: asset.itemId, assetId: asset.assetId });
      return t.fileRequestAccepted;
    });
  };

  const setAssetKind = (asset: DocumentAsset, assetKind: DocumentAssetKind) => {
    void runAssetAction(async (request) => {
      await invoke('set_document_asset_kind', { itemId: asset.itemId, assetId: asset.assetId, assetKind });
      await loadAssets(request);
    });
  };

  const setAssetDefault = (asset: DocumentAsset) => {
    void runAssetAction(async (request) => {
      await invoke('set_document_asset_default', { itemId: asset.itemId, assetId: asset.assetId });
      await loadAssets(request);
    });
  };

  const migrateAsset = (asset: DocumentAsset) => {
    void runAssetAction(async (request) => {
      const result = await invoke<{ outcome: string }>('migrate_external_document_asset', { itemId: asset.itemId, assetId: asset.assetId });
      await loadAssets(request);
      return result.outcome === 'duplicate' ? t.assetDuplicate : t.assetMigrated;
    });
  };

  const requestRemoveAsset = (asset: DocumentAsset | null) => setRemoveTarget(asset);

  const confirmRemoveAsset = async () => {
    const asset = removeTarget;
    if (!asset) return;
    setRemoveTarget(null);
    await runAssetAction(async (request) => {
      if (!vault.isCurrentVaultRequest(request)) return;
      await invoke('remove_document_asset', { itemId: asset.itemId, assetId: asset.assetId });
      await loadAssets(request);
    });
  };

  return {
    summary,
    tableCount,
    assets,
    removeTarget,
    requestRemoveAsset,
    confirmRemoveAsset,
    importAsset,
    linkExternalAsset,
    openAsset,
    setAssetKind,
    setAssetDefault,
    migrateAsset,
    refresh,
    applyContext,
  };
}
