import type { LucideIcon } from 'lucide-react';
import type { ReactNode } from 'react';
import styles from './EmptyState.module.css';
import { Icon } from '../Icon/Icon';

export interface EmptyStateProps {
  icon?: LucideIcon;
  title: ReactNode;
  description?: ReactNode;
  action?: ReactNode;
}

export function EmptyState({ icon, title, description, action }: EmptyStateProps) {
  return (
    <div className={styles.empty}>
      {icon && (
        <div className={styles.icon}>
          <Icon icon={icon} size={40} />
        </div>
      )}
      <h3 className={styles.title}>{title}</h3>
      {description && <p className={styles.description}>{description}</p>}
      {action && <div className={styles.action}>{action}</div>}
    </div>
  );
}
