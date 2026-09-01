import { useEffect, useState } from 'react';
import type { Lang } from '../types';
import { dict } from '../lib/i18n/dict';

export function useI18n() {
  const [lang, setLang] = useState<Lang>(() => {
    const stored = localStorage.getItem('lang') as Lang;
    return stored === 'en' ? 'en' : 'zh';
  });
  const t = dict[lang];

  useEffect(() => {
    localStorage.setItem('lang', lang);
  }, [lang]);

  return { lang, setLang, t };
}
