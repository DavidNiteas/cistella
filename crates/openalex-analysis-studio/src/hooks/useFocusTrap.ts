import { useEffect, useRef } from 'react';

const FOCUSABLE_SELECTOR = [
  'button:not([disabled])',
  'a[href]',
  'input:not([disabled])',
  'select:not([disabled])',
  'textarea:not([disabled])',
  '[tabindex]:not([tabindex="-1"])',
].join(', ');

function getFocusable(container: HTMLElement): HTMLElement[] {
  return Array.from(container.querySelectorAll(FOCUSABLE_SELECTOR)).filter(
    (el): el is HTMLElement => el instanceof HTMLElement && el.offsetParent !== null
  );
}

function findInitialFocus(container: HTMLElement): HTMLElement | null {
  const focusable = getFocusable(container);
  if (!focusable.length) return null;
  // Prefer the first non-disabled primary/action button in the footer.
  const footer = container.querySelector('[data-dialog-footer]');
  if (footer) {
    const footerButtons = Array.from(footer.querySelectorAll('button:not([disabled])')).filter(
      (el): el is HTMLElement => el instanceof HTMLElement
    );
    if (footerButtons.length) return footerButtons[0];
  }
  // Then try the close button.
  const close = container.querySelector('[data-dialog-close]');
  if (close instanceof HTMLElement) return close;
  return focusable[0];
}

export function useFocusTrap(open: boolean, onClose?: () => void, containerRef?: React.RefObject<HTMLElement | null>) {
  const previousActiveRef = useRef<HTMLElement | null>(null);
  const previousOverflowRef = useRef<string>('');

  useEffect(() => {
    if (!open) return;
    previousActiveRef.current = document.activeElement as HTMLElement | null;
    previousOverflowRef.current = document.body.style.overflow;
    document.body.style.overflow = 'hidden';

    const container = containerRef?.current;
    if (container) {
      const initial = findInitialFocus(container);
      initial?.focus();
    }

    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        event.stopPropagation();
        onClose?.();
        return;
      }
      if (event.key !== 'Tab' || !container) return;
      const focusable = getFocusable(container);
      if (!focusable.length) {
        event.preventDefault();
        return;
      }
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    };

    document.addEventListener('keydown', handleKeyDown, true);
    return () => {
      document.removeEventListener('keydown', handleKeyDown, true);
      document.body.style.overflow = previousOverflowRef.current;
      const previous = previousActiveRef.current;
      if (previous && document.body.contains(previous)) {
        previous.focus();
      }
    };
  }, [open, onClose, containerRef]);
}
