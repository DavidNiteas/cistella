import type { ReactNode } from 'react';
import styles from './Shell.module.css';

export interface ShellProps {
  sidebar: ReactNode;
  header?: ReactNode;
  children: ReactNode;
  statusBar?: ReactNode;
}

export function Shell({ sidebar, header, children, statusBar }: ShellProps) {
  return (
    <div className={styles.shell}>
      {sidebar}
      <div className={styles.main}>
        {header}
        <main className={styles.content}>{children}</main>
        {statusBar}
      </div>
    </div>
  );
}
