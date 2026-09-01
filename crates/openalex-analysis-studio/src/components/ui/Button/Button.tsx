import type { ButtonHTMLAttributes, ReactNode } from 'react';
import { Loader2 } from 'lucide-react';
import styles from './Button.module.css';
import { Icon } from '../Icon/Icon';

export type ButtonVariant = 'primary' | 'secondary' | 'ghost' | 'danger' | 'icon';

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant;
  loading?: boolean;
  children: ReactNode;
}

export function Button({
  variant = 'primary',
  loading = false,
  children,
  className = '',
  disabled,
  ...rest
}: ButtonProps) {
  const classes = [styles.button, styles[variant], loading ? styles.loading : '', className]
    .filter(Boolean)
    .join(' ');

  return (
    <button className={classes} disabled={disabled || loading} {...rest}>
      {children}
      {loading && (
        <span className={styles.spinner}>
          <Icon icon={Loader2} size={16} />
        </span>
      )}
    </button>
  );
}
