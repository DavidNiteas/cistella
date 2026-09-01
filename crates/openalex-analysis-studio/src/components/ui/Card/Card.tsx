import type { ReactNode } from 'react';
import styles from './Card.module.css';

export interface CardProps {
  children: ReactNode;
  compact?: boolean;
  className?: string;
}

export interface CardHeaderProps {
  title: ReactNode;
  action?: ReactNode;
}

export function Card({ children, compact = false, className = '' }: CardProps) {
  const classes = [styles.card, compact ? styles.compact : '', className].filter(Boolean).join(' ');
  return <div className={classes}>{children}</div>;
}

export function CardHeader({ title, action }: CardHeaderProps) {
  return (
    <div className={styles.header}>
      <h2 className={styles.title}>{title}</h2>
      {action && <div className={styles.action}>{action}</div>}
    </div>
  );
}
