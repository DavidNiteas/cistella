import { useEffect, useState } from 'react';
import { useTheme } from './useTheme';

export interface ChartTheme {
  backgroundColor: string;
  textColor: string;
  axisColor: string;
  splitLineColor: string;
}

function readChartTheme(): ChartTheme {
  if (typeof window === 'undefined') {
    return {
      backgroundColor: 'transparent',
      textColor: '#5c607a',
      axisColor: '#8589a2',
      splitLineColor: '#e0e2ec',
    };
  }
  const style = getComputedStyle(document.documentElement);
  return {
    backgroundColor: 'transparent',
    textColor: style.getPropertyValue('--color-text-secondary').trim() || '#5c607a',
    axisColor: style.getPropertyValue('--color-text-tertiary').trim() || '#8589a2',
    splitLineColor: style.getPropertyValue('--color-border').trim() || '#e0e2ec',
  };
}

export function useChartTheme(): ChartTheme {
  const { resolvedTheme } = useTheme();
  const [theme, setTheme] = useState<ChartTheme>(readChartTheme);

  useEffect(() => {
    setTheme(readChartTheme());
  }, [resolvedTheme]);

  return theme;
}
