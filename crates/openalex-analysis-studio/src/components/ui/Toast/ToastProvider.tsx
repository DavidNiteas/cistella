import { createContext, useContext } from 'react';
import type { ReactNode } from 'react';
import { ToastContainer, useToastState } from './Toast';
import type { ToastItem, ToastType } from './Toast';

export interface ToastContextValue {
  toasts: ToastItem[];
  push: (message: ReactNode, type?: ToastType, duration?: number) => string;
  dismiss: (id: string) => void;
}

const ToastContext = createContext<ToastContextValue | null>(null);

export function ToastProvider({ children }: { children: ReactNode }) {
  const { toasts, push, dismiss } = useToastState();
  return (
    <ToastContext.Provider value={{ toasts, push, dismiss }}>
      {children}
      <ToastContainer toasts={toasts} onDismiss={dismiss} />
    </ToastContext.Provider>
  );
}

export function useToast(): ToastContextValue {
  const ctx = useContext(ToastContext);
  if (!ctx) {
    throw new Error('useToast must be used within a ToastProvider');
  }
  return ctx;
}
