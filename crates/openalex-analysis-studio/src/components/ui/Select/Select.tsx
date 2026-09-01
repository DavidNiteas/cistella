import type { SelectHTMLAttributes, ReactNode } from 'react';
import styles from './Select.module.css';

export interface SelectOption {
  value: string;
  label: ReactNode;
}

export interface SelectProps extends Omit<SelectHTMLAttributes<HTMLSelectElement>, 'children'> {
  options: SelectOption[];
}

export function Select({ options, className = '', ...rest }: SelectProps) {
  return (
    <select className={[styles.select, className].filter(Boolean).join(' ')} {...rest}>
      {options.map((opt) => (
        <option key={opt.value} value={opt.value}>
          {opt.label}
        </option>
      ))}
    </select>
  );
}
