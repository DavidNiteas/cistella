import type { InputHTMLAttributes, ReactNode } from 'react';
import styles from './Checkbox.module.css';

export interface CheckboxProps extends Omit<InputHTMLAttributes<HTMLInputElement>, 'type'> {
  label?: ReactNode;
}

export function Checkbox({ label, className = '', ...rest }: CheckboxProps) {
  return (
    <label className={[styles.checkbox, className].filter(Boolean).join(' ')}>
      <input type="checkbox" {...rest} />
      {label && <span>{label}</span>}
    </label>
  );
}
