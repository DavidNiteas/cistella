import { Loader2 } from 'lucide-react';
import styles from './Spinner.module.css';
import { Icon } from '../Icon/Icon';

export type SpinnerSize = 'small' | 'medium' | 'large';

export interface SpinnerProps {
  size?: SpinnerSize;
  className?: string;
}

export function Spinner({ size = 'medium', className = '' }: SpinnerProps) {
  const classes = [styles.spinner, styles[size], className].filter(Boolean).join(' ');
  return (
    <span className={classes} aria-label="Loading">
      <Icon icon={Loader2} size={size === 'small' ? 16 : size === 'large' ? 40 : 24} />
    </span>
  );
}
