import styles from './ProgressBar.module.css';

export interface ProgressBarProps {
  value?: number;
  max?: number;
  label?: string;
  indeterminate?: boolean;
}

export function ProgressBar({ value = 0, max = 100, label, indeterminate = false }: ProgressBarProps) {
  const percentage = indeterminate ? 0 : Math.min(100, Math.max(0, (value / max) * 100));
  return (
    <div>
      {label && (
        <div className={styles.label}>
          <span>{label}</span>
          {!indeterminate && <span>{Math.round(percentage)}%</span>}
        </div>
      )}
      <div className={styles.track} role="progressbar" aria-valuemin={0} aria-valuemax={max} aria-valuenow={indeterminate ? undefined : value} aria-label={label}>
        <div className={[styles.fill, indeterminate ? styles.indeterminate : ''].filter(Boolean).join(' ')} style={{ width: indeterminate ? undefined : `${percentage}%` }} />
      </div>
    </div>
  );
}
