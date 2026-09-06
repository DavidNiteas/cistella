import { ExternalLink, Trash2, Upload } from 'lucide-react';
import {
  Button,
  Card,
  CardHeader,
  Checkbox,
  ConfirmDialog,
  ErrorBanner,
  Icon,
  PageHeader,
} from '../../components/ui';
import type { VaultConnection, DocumentAsset, DocumentAssetKind } from '../../types';
import type { VaultContextValue } from '../../hooks/useVaultContext';
import type { VaultImportState } from '../../hooks/useVaultImport';
import type { Dict } from '../../lib/i18n/dict';
import { short, dir, formatFileSize } from '../../lib/utils';
import styles from './VaultPage.module.css';

export interface VaultPageProps {
  vault: VaultConnection;
  context: VaultContextValue;
  importState: VaultImportState;
  literatureItems: import('../../types').LiteratureItem[];
  t: Dict;
  lang: 'zh' | 'en';
  onConnect: (path: string) => void;
}

export function VaultPage({ vault, context, importState, literatureItems, t, lang, onConnect }: VaultPageProps) {
  const { assets, removeTarget, requestRemoveAsset, confirmRemoveAsset, importAsset, linkExternalAsset, openAsset, setAssetKind, setAssetDefault, migrateAsset } = context;
  const { rawDir, setRawDir, importPreview, importError, lastImport, buildArrow, setBuildArrow, inspectSources, importVault, retryImport } = importState;

  return (
    <section className="page">
      <VaultHero t={t} busy={vault.busy} onChooseVault={async () => { const v = await dir(); if (v) vault.setVaultPath(v); }} onChooseSource={async () => { const v = await dir(); if (v) await inspectSources(v); }} onImport={() => void importVault()} importEnabled={Boolean(vault.vaultPath && rawDir && importPreview)} />

      <div className="page-cols">
        <VaultInfo t={t} vaultPath={vault.vaultPath} vaultSummary={context.summary} appDirs={vault.appDirs} busy={vault.busy} onConnect={() => onConnect(vault.vaultPath)} />
        <RecentVaultsPanel t={t} recentVaults={vault.recentVaults} busy={vault.busy} onConnect={onConnect} />
      </div>

      <ImportSection
        t={t}
        lang={lang}
        busy={vault.busy}
        rawDir={rawDir}
        onPickRawDir={async () => { const v = await dir(); if (v) setRawDir(v); }}
        vaultPath={vault.vaultPath}
        onPickOutput={async () => { const v = await dir(); if (v) vault.setVaultPath(v); }}
        importPreview={importPreview}
        importError={importError}
        lastImport={lastImport}
        buildArrow={buildArrow}
        onBuildArrowChange={(e) => setBuildArrow(e.target.checked)}
        onInspect={() => void inspectSources(rawDir)}
        onImport={() => void importVault()}
        onRetry={() => void retryImport()}
      />

      {vault.hasVault && (
        <AssetCenterPanel
          t={t}
          busy={vault.busy}
          assets={assets}
          items={literatureItems}
          onImportAsset={importAsset}
          onLinkExternalAsset={linkExternalAsset}
          onOpenAsset={openAsset}
          onSetKind={setAssetKind}
          onSetDefault={setAssetDefault}
          onMigrate={migrateAsset}
          onRemove={requestRemoveAsset}
        />
      )}

      <ConfirmDialog
        open={removeTarget != null}
        title={lang === 'zh' ? '移除资产记录' : 'Remove asset record'}
        onConfirm={() => void confirmRemoveAsset()}
        onCancel={() => requestRemoveAsset(null)}
        confirmLabel={t.removeAsset}
        cancelLabel={t.cancel}
      >
        {lang === 'zh'
          ? `移除“${removeTarget?.displayName ?? ''}”的资产记录？不会删除磁盘文件。`
          : `Remove the asset record for "${removeTarget?.displayName ?? ''}"? The disk file will not be deleted.`}
      </ConfirmDialog>
    </section>
  );
}

function VaultHero({ t, busy, onChooseVault, onChooseSource, onImport, importEnabled }: { t: Dict; busy: boolean; onChooseVault: () => void; onChooseSource: () => void; onImport: () => void; importEnabled: boolean }) {
  return (
    <Card>
      <PageHeader
        title={t.vaultTitle}
        description={t.vaultDesc}
        actions={
          <div className="actions">
            <Button variant="secondary" onClick={onChooseVault} disabled={busy}>
              {t.chooseVault}
            </Button>
            <Button variant="secondary" onClick={onChooseSource} disabled={busy}>
              {t.chooseDir}
            </Button>
            <Button onClick={onImport} disabled={busy || !importEnabled}>
              <Icon icon={Upload} size={14} /> {t.importFromOpenAlex}
            </Button>
          </div>
        }
      />
    </Card>
  );
}

function VaultInfo({ t, vaultPath, vaultSummary, appDirs, busy, onConnect }: { t: Dict; vaultPath: string; vaultSummary: import('../../types').VaultSummary | null; appDirs: import('../../types').AppDirectoriesDto | null; busy: boolean; onConnect: () => void }) {
  return (
    <Card>
      <CardHeader title={t.currentVault} />
      <div className="sourceRow">
        <div className="path">{short(vaultPath)}</div>
        <Button variant="secondary" onClick={onConnect} disabled={busy || !vaultPath}>
          {t.openVault}
        </Button>
      </div>
      <small>{vaultSummary?.vault_id ? `vault_id: ${vaultSummary.vault_id}` : t.noData}</small>
      <small>{appDirs ? `${t.runMode}: ${appDirs.isPortableMode ? t.portableMode : t.installedMode}` : ''}</small>
    </Card>
  );
}

function RecentVaultsPanel({ t, recentVaults, busy, onConnect }: { t: Dict; recentVaults: import('../../types').RecentVaultDto[]; busy: boolean; onConnect: (path: string) => void }) {
  return (
    <Card>
      <CardHeader title={t.recentVaults} />
      <div className={styles.recent}>
        {recentVaults.length ? (
          recentVaults.map((r) => (
            <button key={r.path} title={r.path} onClick={() => onConnect(r.path)} disabled={busy}>
              {short(r.name || r.path)}
            </button>
          ))
        ) : (
          <small>{t.noData}</small>
        )}
      </div>
    </Card>
  );
}

function ImportSection({ t, lang, busy, rawDir, onPickRawDir, vaultPath, onPickOutput, importPreview, importError, lastImport, buildArrow, onBuildArrowChange, onInspect, onImport, onRetry }: {
  t: Dict;
  lang: 'zh' | 'en';
  busy: boolean;
  rawDir: string;
  onPickRawDir: () => void;
  vaultPath: string;
  onPickOutput: () => void;
  importPreview: VaultImportState['importPreview'];
  importError: string | null;
  lastImport: VaultImportState['lastImport'];
  buildArrow: boolean;
  onBuildArrowChange: (e: React.ChangeEvent<HTMLInputElement>) => void;
  onInspect: () => void;
  onImport: () => void;
  onRetry: () => void;
}) {
  const previewText = importPreview
    ? (lang === 'zh'
      ? `分区数 ${importPreview.partitionCount ?? 0} · 清单 ${importPreview.hasManifest ? '存在' : '缺失'} · 快照 ${importPreview.snapshotDate ?? '未知'}`
      : `${importPreview.partitionCount ?? 0} partitions · manifest ${importPreview.hasManifest ? 'present' : 'missing'} · snapshot ${importPreview.snapshotDate ?? 'unknown'}`)
    : (lang === 'zh' ? '请选择来源目录并执行预检。' : 'Choose a source folder and run inspection.');
  return (
    <Card>
      <CardHeader title={t.buildVault} />
      <PathField label={t.raw} value={rawDir} button={t.chooseDir} onPick={onPickRawDir} disabled={busy} />
      <PathField label={t.output} value={vaultPath} button={t.chooseVault} onPick={onPickOutput} disabled={busy} />
      <div className="actions">
        <Button variant="secondary" onClick={onInspect} disabled={busy || !rawDir}>
          {t.inspectSources}
        </Button>
        <Button onClick={onImport} disabled={busy || !vaultPath || !rawDir || !importPreview}>
          <Icon icon={Upload} size={14} /> {t.importFromOpenAlex}
        </Button>
      </div>
      <Checkbox label={t.buildCache} checked={buildArrow} onChange={onBuildArrowChange} />
      <p className={styles.previewLine}>{previewText}</p>
      {importError && (
        <>
          <ErrorBanner>{importError}</ErrorBanner>
          <div className="actions">
            <Button variant="secondary" onClick={onRetry} disabled={busy || !lastImport}>
              {t.retryImport}
            </Button>
          </div>
        </>
      )}
      <details className={styles.helpDetails}>
        <summary>{lang === 'zh' ? '数据布局与工作流说明' : 'Data layout and workflow'}</summary>
        <ul>
          <li>{t.rawTip}</li>
          <li>{t.outTip}</li>
          <li>{t.parquet}</li>
          <li>{t.arrow}</li>
          <li>{t.manifest}</li>
        </ul>
      </details>
    </Card>
  );
}

function AssetCenterPanel({ t, busy, assets, items, onImportAsset, onLinkExternalAsset, onOpenAsset, onSetKind, onSetDefault, onMigrate, onRemove }: {
  t: Dict;
  busy: boolean;
  assets: DocumentAsset[];
  items: import('../../types').LiteratureItem[];
  onImportAsset: (itemId: string) => void;
  onLinkExternalAsset: (itemId: string) => void;
  onOpenAsset: (asset: DocumentAsset) => void;
  onSetKind: (asset: DocumentAsset, kind: DocumentAssetKind) => void;
  onSetDefault: (asset: DocumentAsset) => void;
  onMigrate: (asset: DocumentAsset) => void;
  onRemove: (asset: DocumentAsset) => void;
}) {
  const assetsForItem = (itemId: string) => assets.filter((asset) => asset.itemId === itemId);
  return (
    <Card className={styles.assetCenter}>
      <CardHeader title={t.assetCenter} action={<span>{assets.length} PDF</span>} />
      <p>{t.assetsInVault}</p>
      {items.length ? (
        items.map((item) => (
          <div className={styles.vaultAssetGroup} key={item.itemId}>
            <div className={styles.cardHead}>
              <div>
                <strong>{item.title || '—'}</strong>
                <small>{item.authors.join(', ') || '—'}</small>
              </div>
              <div className={styles.fileActions}>
                <Button variant="secondary" onClick={() => onImportAsset(item.itemId)} disabled={busy}>
                  <Icon icon={Upload} size={14} /> {t.importAsset}
                </Button>
                <Button variant="secondary" onClick={() => onLinkExternalAsset(item.itemId)} disabled={busy}>
                  <Icon icon={ExternalLink} size={14} /> {t.linkExternalAsset}
                </Button>
              </div>
            </div>
            <AssetList assets={assetsForItem(item.itemId)} t={t} busy={busy} onOpen={onOpenAsset} onSetKind={onSetKind} onSetDefault={onSetDefault} onMigrate={onMigrate} onRemove={onRemove} />
          </div>
        ))
      ) : (
        <p>{t.noLiterature}</p>
      )}
    </Card>
  );
}

function AssetList({ assets, t, busy, onOpen, onSetKind, onSetDefault, onMigrate, onRemove }: {
  assets: DocumentAsset[];
  t: Dict;
  busy: boolean;
  onOpen: (asset: DocumentAsset) => void;
  onSetKind: (asset: DocumentAsset, kind: DocumentAssetKind) => void;
  onSetDefault: (asset: DocumentAsset) => void;
  onMigrate: (asset: DocumentAsset) => void;
  onRemove: (asset: DocumentAsset) => void;
}) {
  if (!assets.length) return <small>{t.noFiles}</small>;
  return (
    <div className={`${styles.literatureFiles} ${styles.assetList}`}>
      {assets.map((asset) => (
        <div className={`${styles.literatureFile} ${styles.assetFile}`} key={asset.assetId}>
          <div className={styles.assetMeta}>
            <strong>{asset.displayName}</strong>
            <small>
              {t.assetStorage}: {asset.storageKind === 'vault' ? t.vaultFile : t.externalFile}
              {asset.isDefault ? ` · ${t.defaultFile}` : ''}
              {' · '}{t.assetKind}: {t.assetKinds[asset.assetKind]}
              {' · '}{t.assetHealth}: {t.assetStatuses[asset.status]}
              {' · '}{t.assetSize}: {formatFileSize(asset.fileSize)}
            </small>
            <small>
              {t.assetHash}: {asset.contentHash ? `${asset.contentHash.slice(0, 12)}…` : '—'} · {t.assetPath}: {short(asset.path)}
            </small>
          </div>
          <div className={styles.fileActions}>
            <Button variant="secondary" onClick={() => onOpen(asset)} disabled={busy}>
              {t.openFile}
            </Button>
            {!asset.isDefault && (
              <Button variant="secondary" onClick={() => onSetDefault(asset)} disabled={busy}>
                {t.setDefaultFile}
              </Button>
            )}
            <select value={asset.assetKind} aria-label={t.assetKind} onChange={(e) => onSetKind(asset, e.target.value as DocumentAssetKind)} disabled={busy}>
              {(['primary', 'supplement', 'version', 'appendix', 'other'] as DocumentAssetKind[]).map((kind) => (
                <option key={kind} value={kind}>{t.assetKinds[kind]}</option>
              ))}
            </select>
            {asset.storageKind === 'external' && (
              <Button variant="secondary" onClick={() => onMigrate(asset)} disabled={busy}>
                {t.migrateToVault}
              </Button>
            )}
            <Button variant="danger" onClick={() => onRemove(asset)} disabled={busy}>
              <Icon icon={Trash2} size={14} /> {t.removeAsset}
            </Button>
          </div>
        </div>
      ))}
    </div>
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
