import { useState } from 'react';
import { open, save } from '@tauri-apps/plugin-dialog';
import type { Dict } from '../lib/i18n/dict';
import type { Adapter, MigrationMode, RecentVaultDto, VaultConnection } from '../types';
import { invoke } from '../lib/invoke';
import { baseName, dir, persistRecentVaults, short } from '../lib/utils';
import { useToast } from '../components/ui/Toast/ToastProvider';

export interface SettingsState {
  migrationOpen: boolean;
  migrationStep: number;
  migrationSource: string;
  migrationName: string;
  migrationMode: MigrationMode;
  migrationTargetPreview: string;
  migrationError: string;
  setMigrationError: (value: string) => void;
  backupRestoreError: string;
  openMigrationWizard: () => void;
  closeMigrationWizard: () => void;
  pickMigrationSource: () => Promise<void>;
  computeMigrationTarget: () => Promise<void>;
  confirmMigration: () => Promise<void>;
  setMigrationStep: (value: number) => void;
  setMigrationMode: (value: MigrationMode) => void;
  backupCurrentVault: () => Promise<void>;
  restoreFromBackup: (connect: (path: string) => Promise<unknown>) => Promise<void>;
}

export function useSettings(vault: VaultConnection, adapters: Adapter[], t: Dict): SettingsState {
  const [migrationOpen, setMigrationOpen] = useState(false);
  const [migrationStep, setMigrationStep] = useState(1);
  const [migrationSource, setMigrationSource] = useState('');
  const [migrationName, setMigrationName] = useState('');
  const [migrationMode, setMigrationMode] = useState<MigrationMode>('portable');
  const [migrationTargetPreview, setMigrationTargetPreview] = useState('');
  const [migrationError, setMigrationError] = useState('');
  const [backupRestoreError, setBackupRestoreError] = useState('');
  const toast = useToast();

  const openMigrationWizard = () => {
    setMigrationSource(vault.vaultPath || '');
    setMigrationName(vault.vaultPath ? baseName(vault.vaultPath) : '');
    setMigrationMode(vault.appDirs?.isPortableMode ? 'installed' : 'portable');
    setMigrationStep(1);
    setMigrationError('');
    setMigrationTargetPreview('');
    setMigrationOpen(true);
  };

  const closeMigrationWizard = () => {
    setMigrationOpen(false);
    setMigrationError('');
  };

  const pickMigrationSource = async () => {
    const picked = await dir();
    if (!picked) return;
    setMigrationSource(picked);
    if (!migrationName) setMigrationName(baseName(picked));
  };

  const computeMigrationTarget = async () => {
    if (!migrationSource) return;
    const dirs = await invoke<import('../types').AppDirectoriesDto>('app_directories');
    const root = migrationMode === 'portable' ? (dirs.portableRoot || dirs.configDir) : dirs.configDir;
    const parent = root.replace(/\\/g, '/').replace(/\/config\/?$/, '').replace(/\/$/, '');
    setMigrationTargetPreview(`${parent}/vaults/${migrationName || baseName(migrationSource)}`);
  };

  const confirmMigration = async () => {
    if (!migrationSource) {
      setMigrationError(t.chooseFirst);
      return;
    }
    const recent: RecentVaultDto = {
      path: migrationSource,
      name: migrationName || baseName(migrationSource),
      openedAt: new Date().toISOString(),
    };
    const command = migrationMode === 'portable' ? 'migrate_vaults_to_portable' : 'migrate_vaults_to_installed';
    try {
      const migrated = await invoke<RecentVaultDto[]>(command, { vaults: [recent] });
      vault.setRecentVaults(migrated);
      await persistRecentVaults(migrated);
      const dirs = await invoke<import('../types').AppDirectoriesDto>('app_directories');
      vault.setAppDirs(dirs);
      const message = migrationMode === 'portable' ? t.migratedToPortable : t.migratedToInstalled;
      vault.setStatus(message);
      toast.push(message, 'success');
      closeMigrationWizard();
    } catch (e: any) {
      const message = String(e?.message ?? e);
      setMigrationError(message);
      toast.push(`${t.failed}: ${message}`, 'error');
    }
  };

  const backupCurrentVault = async () => {
    setBackupRestoreError('');
    if (!vault.vaultPath) {
      setBackupRestoreError(t.chooseFirst);
      return;
    }
    const defaultName = `${baseName(vault.vaultPath)}-backup.zip`;
    const picked = await save({ defaultPath: defaultName, filters: [{ name: 'ZIP', extensions: ['zip'] }] });
    if (typeof picked !== 'string') return;
    try {
      await invoke('backup_library', { vaultPath: vault.vaultPath, backupPath: picked });
      const message = `${t.backupComplete}: ${short(picked)}`;
      vault.setStatus(message);
      toast.push(message, 'success');
      setBackupRestoreError('');
    } catch (e: any) {
      const message = String(e?.message ?? e);
      setBackupRestoreError(message);
      toast.push(`${t.failed}: ${message}`, 'error');
    }
  };

  const restoreFromBackup = async (connect: (path: string) => Promise<unknown>) => {
    setBackupRestoreError('');
    const backup = await open({ multiple: false, filters: [{ name: 'ZIP', extensions: ['zip'] }] });
    if (typeof backup !== 'string') return;
    const target = await dir();
    if (!target) return;
    const restoredDir = `${target}/${baseName(backup).replace(/\.zip$/i, '')}`;
    try {
      await invoke('restore_library', { backupPath: backup, targetPath: restoredDir });
      await connect(restoredDir);
      const message = `${t.restoreComplete}: ${short(restoredDir)}`;
      vault.setStatus(message);
      toast.push(message, 'success');
      setBackupRestoreError('');
    } catch (e: any) {
      const message = String(e?.message ?? e);
      setBackupRestoreError(message);
      toast.push(`${t.failed}: ${message}`, 'error');
    }
  };

  return {
    migrationOpen,
    migrationStep,
    migrationSource,
    migrationName,
    migrationMode,
    migrationTargetPreview,
    migrationError,
    setMigrationError,
    backupRestoreError,
    openMigrationWizard,
    closeMigrationWizard,
    pickMigrationSource,
    computeMigrationTarget,
    confirmMigration,
    setMigrationStep,
    setMigrationMode,
    backupCurrentVault,
    restoreFromBackup,
  };
}
