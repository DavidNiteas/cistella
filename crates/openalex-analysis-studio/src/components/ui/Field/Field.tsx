import type { InputHTMLAttributes, TextareaHTMLAttributes, ReactNode } from 'react';
import styles from './Field.module.css';

export interface FieldProps {
  label?: ReactNode;
  hint?: ReactNode;
  error?: ReactNode;
  children: ReactNode;
}

export function Field({ label, hint, error, children }: FieldProps) {
  return (
    <label className={styles.field}>
      {label && <span className={styles.label}>{label}</span>}
      {children}
      {error && <span className={styles.error}>{error}</span>}
      {hint && !error && <span className={styles.hint}>{hint}</span>}
    </label>
  );
}

export function Input({ className = '', ...rest }: InputHTMLAttributes<HTMLInputElement>) {
  return <input className={[styles.input, className].filter(Boolean).join(' ')} {...rest} />;
}

export function TextArea({ className = '', ...rest }: TextareaHTMLAttributes<HTMLTextAreaElement>) {
  return <textarea className={[styles.textarea, className].filter(Boolean).join(' ')} {...rest} />;
}
