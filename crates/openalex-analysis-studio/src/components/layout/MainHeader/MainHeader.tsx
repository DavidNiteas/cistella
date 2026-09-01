import type { ReactNode } from 'react';
import styles from './MainHeader.module.css';

export interface MainHeaderProps {
  kicker?: ReactNode;
  title: ReactNode;
  description?: ReactNode;
  actions?: ReactNode;
}

export function MainHeader({ kicker, title, description, actions }: MainHeaderProps) {
  return (
    <header className={styles.header}>
      <div>
        {kicker && <div className={styles.kicker}>{kicker}</div>}
        <h1 className={styles.title}>{title}</h1>
        {description && <p className={styles.description}>{description}</p>}
      </div>
      {actions && <div className={styles.actions}>{actions}</div>}
    </header>
  );
}
