import type { MouseEvent, ReactNode } from 'react';
import { useId, useRef } from 'react';
import { X } from 'lucide-react';
import styles from './Dialog.module.css';
import { Button } from '../Button/Button';
import { Icon } from '../Icon/Icon';
import { useFocusTrap } from '../../../hooks/useFocusTrap';

export interface ModalOverlayProps {
  children: ReactNode;
  onClose?: () => void;
}

export interface DialogProps {
  title: ReactNode;
  children: ReactNode;
  onClose?: () => void;
  footer?: ReactNode;
  className?: string;
}

export interface ConfirmDialogProps {
  open: boolean;
  title: ReactNode;
  children: ReactNode;
  confirmLabel: ReactNode;
  cancelLabel: ReactNode;
  onConfirm: () => void | Promise<void>;
  onCancel: () => void | Promise<void>;
}

export interface DangerDialogProps extends Omit<ConfirmDialogProps, 'confirmLabel'> {
  dangerLabel: ReactNode;
}

export function ModalOverlay({ children, onClose }: ModalOverlayProps) {
  const handleBackdropClick = (e: MouseEvent<HTMLDivElement>) => {
    if (e.target === e.currentTarget) onClose?.();
  };

  return (
    <div className={styles.overlay} onClick={handleBackdropClick} role="presentation">
      {children}
    </div>
  );
}

export function Dialog({ title, children, onClose, footer, className = '' }: DialogProps) {
  const cardRef = useRef<HTMLDivElement>(null);
  const titleId = useId();
  const bodyId = useId();
  useFocusTrap(true, onClose, cardRef);

  return (
    <ModalOverlay onClose={onClose}>
      <div
        ref={cardRef}
        className={[styles.card, className].filter(Boolean).join(' ')}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={bodyId}
      >
        <div className={styles.header}>
          <h2 id={titleId} className={styles.title}>
            {title}
          </h2>
          {onClose && (
            <button data-dialog-close className={styles.close} onClick={onClose} aria-label="Close">
              <Icon icon={X} size={20} />
            </button>
          )}
        </div>
        <div id={bodyId} className={styles.body}>
          {children}
        </div>
        {footer && (
          <div data-dialog-footer className={styles.footer}>
            {footer}
          </div>
        )}
      </div>
    </ModalOverlay>
  );
}

export function ConfirmDialog({
  open,
  title,
  children,
  confirmLabel,
  cancelLabel,
  onConfirm,
  onCancel,
}: ConfirmDialogProps) {
  if (!open) return null;
  return (
    <Dialog
      title={title}
      onClose={onCancel}
      footer={
        <>
          <Button variant="secondary" onClick={onCancel}>
            {cancelLabel}
          </Button>
          <Button onClick={onConfirm}>{confirmLabel}</Button>
        </>
      }
    >
      {children}
    </Dialog>
  );
}

export function DangerDialog({
  open,
  title,
  children,
  dangerLabel,
  cancelLabel,
  onConfirm,
  onCancel,
}: DangerDialogProps) {
  if (!open) return null;
  return (
    <Dialog
      title={<span className={styles.dangerTitle}>{title}</span>}
      onClose={onCancel}
      footer={
        <>
          <Button variant="secondary" onClick={onCancel}>
            {cancelLabel}
          </Button>
          <Button variant="danger" onClick={onConfirm}>
            {dangerLabel}
          </Button>
        </>
      }
    >
      {children}
    </Dialog>
  );
}
