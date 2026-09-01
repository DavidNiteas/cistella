import { useEffect, useState } from 'react';
import { CheckCircle2, X, AlertCircle, AlertTriangle, Info } from 'lucide-react';
import styles from './Toast.module.css';
import { Icon } from '../Icon/Icon';

export type ToastType = 'success' | 'error' | 'warning' | 'info';

export interface ToastItem {
  id: string;
  message: React.ReactNode;
  type?: ToastType;
  duration?: number;
}

export interface ToastContainerProps {
  toasts: ToastItem[];
  onDismiss: (id: string) => void;
}

const icons: Record<ToastType, typeof CheckCircle2> = {
  success: CheckCircle2,
  error: AlertCircle,
  warning: AlertTriangle,
  info: Info,
};

function Toast({ item, onDismiss }: { item: ToastItem; onDismiss: (id: string) => void }) {
  useEffect(() => {
    if (item.duration === 0) return;
    const duration = item.duration ?? 4000;
    const timer = setTimeout(() => onDismiss(item.id), duration);
    return () => clearTimeout(timer);
  }, [item, onDismiss]);

  const type = item.type ?? 'info';
  return (
    <div className={[styles.toast, styles[type]].join(' ')} role="status" aria-live="polite">
      <span className={styles.icon}>
        <Icon icon={icons[type]} size={18} />
      </span>
      <div className={styles.content}>{item.message}</div>
      <button className={styles.close} onClick={() => onDismiss(item.id)} aria-label="Dismiss">
        <Icon icon={X} size={16} />
      </button>
    </div>
  );
}

export function ToastContainer({ toasts, onDismiss }: ToastContainerProps) {
  if (!toasts.length) return null;
  return (
    <div className={styles.container} role="region" aria-label="Notifications">
      {toasts.map((toast) => (
        <Toast key={toast.id} item={toast} onDismiss={onDismiss} />
      ))}
    </div>
  );
}

export function useToastState() {
  const [toasts, setToasts] = useState<ToastItem[]>([]);

  const push = (message: React.ReactNode, type: ToastType = 'info', duration = 4000) => {
    const id = `${Date.now()}-${Math.random().toString(36).slice(2, 9)}`;
    setToasts((prev) => [...prev, { id, message, type, duration }]);
    return id;
  };

  const dismiss = (id: string) => setToasts((prev) => prev.filter((t) => t.id !== id));

  return { toasts, push, dismiss };
}
