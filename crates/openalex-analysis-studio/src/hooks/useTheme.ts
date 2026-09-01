import { useEffect, useState } from 'react';

export type Theme = 'light' | 'dark' | 'system';

const STORAGE_KEY = 'theme';

function getSystemTheme(): 'light' | 'dark' {
  if (typeof window === 'undefined') return 'light';
  return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
}

function resolveTheme(theme: Theme): 'light' | 'dark' {
  return theme === 'system' ? getSystemTheme() : theme;
}

export function initializeTheme() {
  if (typeof window === 'undefined') return;
  const stored = localStorage.getItem(STORAGE_KEY) as Theme | null;
  const theme: Theme = stored === 'light' || stored === 'dark' || stored === 'system' ? stored : 'system';
  document.documentElement.setAttribute('data-theme', resolveTheme(theme));
}

export function useTheme() {
  const [theme, setThemeState] = useState<Theme>(() => {
    if (typeof window === 'undefined') return 'system';
    const stored = localStorage.getItem(STORAGE_KEY) as Theme | null;
    return stored === 'light' || stored === 'dark' || stored === 'system' ? stored : 'system';
  });

  const resolvedTheme = resolveTheme(theme);

  const setTheme = (next: Theme) => {
    setThemeState(next);
    if (typeof window !== 'undefined') {
      localStorage.setItem(STORAGE_KEY, next);
      document.documentElement.setAttribute('data-theme', resolveTheme(next));
    }
  };

  useEffect(() => {
    if (typeof window === 'undefined') return;
    const media = window.matchMedia('(prefers-color-scheme: dark)');
    const handler = () => {
      document.documentElement.setAttribute('data-theme', resolveTheme(theme));
    };
    media.addEventListener('change', handler);
    return () => media.removeEventListener('change', handler);
  }, [theme]);

  return { theme, setTheme, resolvedTheme };
}
