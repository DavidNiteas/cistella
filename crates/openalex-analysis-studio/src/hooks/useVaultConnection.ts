import { useEffect, useRef, useState } from 'react';
import type { AppDirectoriesDto, RecentVaultDto, UpdateCheck, VaultConnection, VaultContext, VaultRequestContext } from '../types';
import type { Dict } from '../lib/i18n/dict';
import { invoke } from '../lib/invoke';
import { fetchRecentVaults, persistRecentVaults, rememberBackend, short } from '../lib/utils';
import { useToast } from '../components/ui/Toast/ToastProvider';

export function useVaultConnection(t: Dict): VaultConnection {
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState(t.ready);
  const [restored, setRestored] = useState(false);
  const [vaultError, setVaultError] = useState('');
  const [vaultPath, setVaultPath] = useState('');
  const vaultPathRef = useRef('');
  const [recentVaults, setRecentVaults] = useState<RecentVaultDto[]>([]);
  const [appDirs, setAppDirs] = useState<AppDirectoriesDto | null>(null);
  const [appVersion, setAppVersion] = useState<string | null>(null);
  const [updateCheck, setUpdateCheck] = useState<UpdateCheck | null>(null);
  const toast = useToast();
  const [migratePortableOpen, setMigratePortableOpen] = useState(false);
  const [migrateInstalledOpen, setMigrateInstalledOpen] = useState(false);
  const vaultConnectionGenerationRef = useRef(0);

  useEffect(() => {
    vaultPathRef.current = vaultPath;
  }, [vaultPath]);

  const captureVaultRequest = (): VaultRequestContext => ({
    expectedGeneration: vaultConnectionGenerationRef.current,
    expectedVaultPath: vaultPathRef.current,
  });

  const isCurrentVaultRequest = (request: VaultRequestContext) =>
    vaultConnectionGenerationRef.current === request.expectedGeneration && vaultPathRef.current === request.expectedVaultPath;

  const beginVaultConnection = () => {
    const generation = vaultConnectionGenerationRef.current + 1;
    vaultConnectionGenerationRef.current = generation;
    clearVaultContext(false);
    return generation;
  };

  const isCurrentVaultConnection = (generation: number) => vaultConnectionGenerationRef.current === generation;

  const clearVaultContext = (invalidatePending = true) => {
    if (invalidatePending) vaultConnectionGenerationRef.current += 1;
    vaultPathRef.current = '';
    setVaultPath('');
    setVaultError('');
  };

  const run = async (fn: () => Promise<unknown>) => {
    setBusy(true);
    try {
      await fn();
    } catch (e: any) {
      const message = `${t.failed}: ${e?.message ?? e}`;
      setStatus(message);
      toast.push(message, 'error');
    } finally {
      setBusy(false);
    }
  };

  const connect = async (path: string): Promise<VaultContext | null> => {
    if (!path) {
      setStatus(t.chooseFirst);
      return null;
    }
    const generation = beginVaultConnection();
    setVaultError('');
    setStatus(t.connecting);
    try {
      const ctx = await invoke<VaultContext>('connect_library', { path, generation });
      if (!isCurrentVaultConnection(generation)) return null;
      const nextPath = typeof ctx?.root === 'string' ? ctx.root : path;
      vaultPathRef.current = nextPath;
      setVaultPath(nextPath);
      const next = await rememberBackend(nextPath, recentVaults);
      setRecentVaults(next);
      setStatus(`${t.connected}: ${nextPath}`);
      toast.push(`${t.connected}: ${short(nextPath)}`, 'success');
      setRestored(true);
      return ctx;
    } catch (e: any) {
      if (!isCurrentVaultConnection(generation)) return null;
      const message = String(e?.message ?? e);
      clearVaultContext(false);
      setVaultError(message);
      const status = `${t.failed}: ${message}`;
      setStatus(status);
      toast.push(status, 'error');
      setRestored(true);
      return null;
    }
  };

  const refreshVaultContext = async () => {
    const request = captureVaultRequest();
    if (!request.expectedVaultPath) return false;
    try {
      const ctx = await invoke<VaultContext>('library_context');
      if (!isCurrentVaultRequest(request)) return false;
      const nextPath = typeof ctx?.root === 'string' ? ctx.root : request.expectedVaultPath;
      if (!nextPath) return false;
      vaultPathRef.current = nextPath;
      setVaultPath(nextPath);
      setVaultError('');
      return true;
    } catch (e: any) {
      if (!isCurrentVaultRequest(request)) return false;
      const message = String(e?.message ?? e);
      clearVaultContext(false);
      setVaultError(message);
      const status = `${t.failed}: ${message}`;
      setStatus(status);
      toast.push(status, 'error');
      return false;
    }
  };

  const confirmMigrateToPortable = async () => {
    try {
      const migrated = await invoke<RecentVaultDto[]>('migrate_vaults_to_portable', { vaults: recentVaults });
      setRecentVaults(migrated);
      await persistRecentVaults(migrated);
      const dirs = await invoke<AppDirectoriesDto>('app_directories');
      setAppDirs(dirs);
      setStatus(t.migratedToPortable);
      toast.push(t.migratedToPortable, 'success');
    } catch (e: any) {
      const message = `${t.failed}: ${e?.message ?? e}`;
      setStatus(message);
      toast.push(message, 'error');
    }
  };

  const confirmMigrateToInstalled = async () => {
    try {
      const migrated = await invoke<RecentVaultDto[]>('migrate_vaults_to_installed', { vaults: recentVaults });
      setRecentVaults(migrated);
      await persistRecentVaults(migrated);
      const dirs = await invoke<AppDirectoriesDto>('app_directories');
      setAppDirs(dirs);
      setStatus(t.migratedToInstalled);
      toast.push(t.migratedToInstalled, 'success');
    } catch (e: any) {
      const message = `${t.failed}: ${e?.message ?? e}`;
      setStatus(message);
      toast.push(message, 'error');
    }
  };

  const migrateToPortable = () => setMigratePortableOpen(true);
  const migrateToInstalled = () => setMigrateInstalledOpen(true);

  // Restore the most recent vault on startup.
  useEffect(() => {
    let cancelled = false;
    const restoreRecentVault = async () => {
      setStatus(t.restoring);
      const [dirs, candidates] = await Promise.all([
        invoke<AppDirectoriesDto | null>('app_directories').catch(() => null),
        fetchRecentVaults(),
      ]);
      if (cancelled) return;
      if (dirs) setAppDirs(dirs);
      setRecentVaults(candidates);
      for (const vault of candidates) {
        const generation = beginVaultConnection();
        try {
          const ctx = await invoke<VaultContext>('connect_library', { path: vault.path, generation });
          if (cancelled || !isCurrentVaultConnection(generation)) return;
          const nextPath = typeof ctx?.root === 'string' ? ctx.root : vault.path;
          vaultPathRef.current = nextPath;
          setVaultPath(nextPath);
          const next = await rememberBackend(nextPath, candidates);
          if (!cancelled) setRecentVaults(next);
          setVaultError('');
          setStatus(`${t.connected}: ${nextPath}`);
          toast.push(`${t.connected}: ${short(nextPath)}`, 'success');
          setRestored(true);
          return;
        } catch {
          if (cancelled || !isCurrentVaultConnection(generation)) return;
        }
      }
      if (!cancelled) {
        clearVaultContext();
        setStatus(t.ready);
        setRestored(true);
      }
    };
    void restoreRecentVault();
    return () => {
      cancelled = true;
    };
  }, []);

  // Load version and update info once.
  useEffect(() => {
    let cancelled = false;
    const loadVersionInfo = async () => {
      try {
        const version = await invoke<string>('get_app_version');
        if (!cancelled) setAppVersion(version);
      } catch {
        /* ignore */
      }
      try {
        const check = await invoke<UpdateCheck>('check_update');
        if (!cancelled) setUpdateCheck(check);
      } catch {
        /* ignore */
      }
    };
    void loadVersionInfo();
    return () => {
      cancelled = true;
    };
  }, []);

  return {
    busy,
    setBusy,
    status,
    setStatus,
    restored,
    vaultError,
    setVaultError,
    vaultPath,
    setVaultPath,
    hasVault: Boolean(vaultPath),
    recentVaults,
    setRecentVaults,
    appDirs,
    setAppDirs,
    appVersion,
    updateCheck,
    migratePortableOpen,
    setMigratePortableOpen,
    migrateInstalledOpen,
    setMigrateInstalledOpen,
    generationRef: vaultConnectionGenerationRef,
    run,
    captureVaultRequest,
    isCurrentVaultRequest,
    beginVaultConnection,
    isCurrentVaultConnection,
    clearVaultContext,
    connect,
    refreshVaultContext,
    confirmMigrateToPortable,
    confirmMigrateToInstalled,
    migrateToPortable,
    migrateToInstalled,
  };
}
