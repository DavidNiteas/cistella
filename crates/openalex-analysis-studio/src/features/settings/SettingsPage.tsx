import { Button, Card, CardHeader, DangerDialog, Dialog, ErrorBanner, Field, Select } from '../../components/ui';
import type { Dict } from '../../lib/i18n/dict';
import type { Adapter, VaultConnection } from '../../types';
import type { SettingsState } from '../../hooks/useSettings';
import { useTheme } from '../../hooks/useTheme';
import { short } from '../../lib/utils';
import styles from './SettingsPage.module.css';

export interface SettingsPageProps {
  vault: VaultConnection;
  settings: SettingsState;
  adapters: Adapter[];
  t: Dict;
  lang: 'zh' | 'en';
  onLangChange: (lang: 'zh' | 'en') => void;
}

export function SettingsPage({ vault, settings, adapters, t, lang, onLangChange }: SettingsPageProps) {
  const { theme, setTheme } = useTheme();

  return (
    <section className="page">
      <div className="page-cols">
      <Card>
        <CardHeader title={t.language} />
        <Button variant={lang === 'zh' ? 'primary' : 'secondary'} onClick={() => onLangChange('zh')}>
          {t.chinese}
        </Button>
        <Button variant={lang === 'en' ? 'primary' : 'secondary'} onClick={() => onLangChange('en')}>
          {t.english}
        </Button>
      </Card>

      <Card>
        <CardHeader title={t.theme} />
        <Field label={t.theme}>
          <Select
            value={theme}
            onChange={(e) => setTheme(e.target.value as 'light' | 'dark' | 'system')}
            options={[
              { value: 'light', label: t.themeLight },
              { value: 'dark', label: t.themeDark },
              { value: 'system', label: t.themeSystem },
            ]}
          />
        </Field>
      </Card>

      <Card>
        <CardHeader title={t.keyboardShortcuts} />
        <ul className={styles.shortcutList}>
          <li>{t.shortcutSwitchWorkspace}</li>
          <li>{t.shortcutCommandPalette}</li>
          <li>{t.shortcutNewItem}</li>
          <li>{t.shortcutSave}</li>
          <li>{t.shortcutEsc}</li>
        </ul>
      </Card>

      <Card>
        <CardHeader title={t.runMode} />
        <p><strong>{vault.appDirs ? (vault.appDirs.isPortableMode ? t.portableMode : t.installedMode) : '—'}</strong></p>
        {vault.appDirs && (
          <>
            <p><small>{t.configDir}: {short(vault.appDirs.configDir)}</small></p>
            <p><small>{t.recentVaultsPath}: {short(vault.appDirs.recentVaultsPath)}</small></p>
          </>
        )}
        <div className="actions">
          <Button variant="secondary" onClick={vault.migrateToPortable} disabled={vault.busy || !vault.appDirs || vault.appDirs.isPortableMode}>
            {t.migrateToPortable}
          </Button>
          <Button variant="secondary" onClick={vault.migrateToInstalled} disabled={vault.busy || !vault.appDirs || !vault.appDirs.isPortableMode}>
            {t.migrateToInstalled}
          </Button>
          <Button variant="secondary" onClick={settings.openMigrationWizard} disabled={vault.busy || !vault.appDirs}>
            {t.migrationWizard}
          </Button>
        </div>
        <div className="actions" style={{ marginTop: 10 }}>
          <Button variant="secondary" onClick={() => void settings.backupCurrentVault()} disabled={vault.busy || !vault.hasVault}>
            {t.backupVault}
          </Button>
          <Button variant="secondary" onClick={() => void settings.restoreFromBackup(vault.connect)} disabled={vault.busy}>
            {t.restoreVault}
          </Button>
        </div>
        {settings.backupRestoreError && <ErrorBanner>{settings.backupRestoreError}</ErrorBanner>}
      </Card>

      <Card>
        <CardHeader title={t.brand} />
        <p>{lang === 'zh' ? '桌面优先、免安装、库即一切。' : 'Desktop-first, portable, vault-first.'}</p>
        <p>{lang === 'zh' ? `当前支持来源：${adapters.map((a) => a.name).join('、')}` : `Supported sources: ${adapters.map((a) => a.name).join(', ')}`}</p>
        <p>{vault.vaultPath ? `${t.vaultId}: ${vault.vaultPath}` : (lang === 'zh' ? '尚未连接库。' : 'No vault connected yet.')}</p>
        <p>{vault.vaultPath ? `tables: —` : (lang === 'zh' ? '尚无表信息。' : 'No table info yet.')}</p>
        <p><strong>{t.version}</strong>: {vault.appVersion ?? '—'}</p>
        {vault.updateCheck && (
          <p>
            <strong>{t.updateCheck}</strong>:{' '}
            {vault.updateCheck.hasUpdate
              ? `${t.updateAvailable} ${vault.updateCheck.latestVersion}`
              : `${t.upToDate} (${vault.updateCheck.currentVersion})`}
          </p>
        )}
        <small>{adapters.map((a) => `${a.name}${a.is_default ? ' · 默认' : ''}`).join(' | ')}</small>
      </Card>
      </div>

      {settings.migrationOpen && <MigrationWizardDialog t={t} lang={lang} vault={vault} settings={settings} />}

      <DangerDialog
        open={vault.migratePortableOpen}
        title={lang === 'zh' ? '迁移到便携目录' : 'Migrate to portable'}
        onConfirm={() => { vault.setMigratePortableOpen(false); void vault.run(vault.confirmMigrateToPortable); }}
        onCancel={() => vault.setMigratePortableOpen(false)}
        dangerLabel={t.migrationConfirm}
        cancelLabel={t.cancel}
      >
        {t.migrateToPortableConfirm}
      </DangerDialog>

      <DangerDialog
        open={vault.migrateInstalledOpen}
        title={lang === 'zh' ? '迁移到安装目录' : 'Migrate to installed'}
        onConfirm={() => { vault.setMigrateInstalledOpen(false); void vault.run(vault.confirmMigrateToInstalled); }}
        onCancel={() => vault.setMigrateInstalledOpen(false)}
        dangerLabel={t.migrationConfirm}
        cancelLabel={t.cancel}
      >
        {t.migrateToInstalledConfirm}
      </DangerDialog>
    </section>
  );
}

function MigrationWizardDialog({ t, lang: _lang, vault, settings }: { t: Dict; lang: 'zh' | 'en'; vault: VaultConnection; settings: SettingsState }) {
  return (
    <Dialog
      title={t.migrationWizard}
      onClose={settings.closeMigrationWizard}
      footer={
        <div className="actions">
          {settings.migrationStep > 1 && (
            <Button variant="secondary" onClick={() => settings.setMigrationStep(settings.migrationStep - 1)}>
              {t.migrationBack}
            </Button>
          )}
          {settings.migrationStep < 3 && (
            <Button onClick={() => {
              if (settings.migrationStep === 1) {
                if (!settings.migrationSource) { settings.setMigrationError(t.chooseFirst); return; }
                settings.setMigrationError('');
              }
              settings.setMigrationStep(settings.migrationStep + 1);
              if (settings.migrationStep === 2) void settings.computeMigrationTarget();
            }}>
              {t.migrationNext}
            </Button>
          )}
          {settings.migrationStep === 3 && (
            <Button onClick={() => void vault.run(settings.confirmMigration)} disabled={!settings.migrationTargetPreview}>
              {t.migrationConfirm}
            </Button>
          )}
        </div>
      }
    >
      {settings.migrationStep === 1 && (
        <div className={styles.migrationStep}>
          <p>{t.migrationSource}</p>
          <PathField label={t.chooseDir} value={settings.migrationSource} button={t.chooseDir} onPick={() => void settings.pickMigrationSource()} disabled={vault.busy} />
        </div>
      )}
      {settings.migrationStep === 2 && (
        <div className={styles.migrationStep}>
          <p>{t.migrationMode}</p>
          <label className={styles.radioLabel}>
            <input type="radio" name="migrationMode" checked={settings.migrationMode === 'portable'} onChange={() => settings.setMigrationMode('portable')} disabled={vault.appDirs?.isPortableMode === true} />
            {t.migrationModePortable}
          </label>
          <label className={styles.radioLabel}>
            <input type="radio" name="migrationMode" checked={settings.migrationMode === 'installed'} onChange={() => settings.setMigrationMode('installed')} disabled={vault.appDirs?.isPortableMode === false} />
            {t.migrationModeInstalled}
          </label>
          {vault.appDirs?.isPortableMode === true && <p className={styles.hintText}>{t.migrationAlreadyPortable}</p>}
          {vault.appDirs?.isPortableMode === false && <p className={styles.hintText}>{t.migrationAlreadyInstalled}</p>}
        </div>
      )}
      {settings.migrationStep === 3 && (
        <div className={styles.migrationStep}>
          <p>{t.migrationTarget}: <code>{settings.migrationTargetPreview || '—'}</code></p>
        </div>
      )}
      {settings.migrationError && <ErrorBanner>{settings.migrationError}</ErrorBanner>}
    </Dialog>
  );
}

function PathField({ label, value, button, onPick, disabled }: { label: string; value: string; button: string; onPick: () => void; disabled: boolean }) {
  return (
    <div className="pathPick">
      <label>{label}</label>
      <div>
        <span>{short(value)}</span>
        <Button variant="secondary" onClick={onPick} disabled={disabled}>
          {button}
        </Button>
      </div>
    </div>
  );
}

