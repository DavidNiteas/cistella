import styles from './StatusBar.module.css';

export interface StatusBarProps {
  vaultPath?: string;
  status?: string;
  busy?: boolean;
  error?: boolean;
}

export function StatusBar({ vaultPath, status, busy = false, error = false }: StatusBarProps) {
  const dotClass = error ? styles.error : busy ? styles.busy : '';
  return (
    <footer className={styles.statusBar}>
      <span className={styles.path} title={vaultPath}>
        {vaultPath || '—'}
      </span>
      <span className={styles.status} aria-live="polite" aria-atomic="true">
        <span className={[styles.dot, dotClass].filter(Boolean).join(' ')} aria-hidden="true" />
        {status || ''}
      </span>
    </footer>
  );
}
