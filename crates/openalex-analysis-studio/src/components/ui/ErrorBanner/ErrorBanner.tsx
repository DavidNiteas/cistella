import type { ReactNode } from 'react';
import styles from './ErrorBanner.module.css';
import { AlertCircle } from 'lucide-react';
import { Icon } from '../Icon/Icon';

export interface ErrorBannerProps {
  children?: ReactNode;
  onDismiss?: () => void;
}

export function ErrorBanner({ children, onDismiss }: ErrorBannerProps) {
  return (
    <div className={styles.banner} role="alert">
      <Icon icon={AlertCircle} size={18} />
      <span className={styles.content}>{children}</span>
      {onDismiss && (
        <button className={styles.dismiss} onClick={onDismiss} aria-label="Dismiss">
          ×
        </button>
      )}
    </div>
  );
}
