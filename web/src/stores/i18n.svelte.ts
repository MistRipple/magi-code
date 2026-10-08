/**
 * i18n 状态管理 - Svelte 5 Runes
 * 使用 class 实例模式确保 $state/$derived 跨模块响应式正确追踪
 */

import { DEFAULT_LOCALE, LOCALES, isSupportedLocale, type LocaleCode } from '../i18n/locales';

const dictionaries = Object.fromEntries(
  LOCALES.map((locale) => [locale.code, locale.dictionary]),
) as Record<LocaleCode, Record<string, string>>;

function resolveInitialLocale(): LocaleCode {
  if (typeof window !== 'undefined') {
    const locale = (window as unknown as { __INITIAL_LOCALE__?: string }).__INITIAL_LOCALE__;
    if (isSupportedLocale(locale)) {
      return locale;
    }
  }
  return DEFAULT_LOCALE;
}

class I18nStore {
  locale = $state<LocaleCode>(resolveInitialLocale());
  private dict = $derived(dictionaries[this.locale]);

  /**
   * 翻译指定 key，支持变量插值。
   * 查找顺序：当前语言 → key 本身（兜底）。
   */
  t(key: string, vars?: Record<string, string | number>): string {
    let text = this.dict[key] ?? key;
    if (vars) {
      for (const [k, v] of Object.entries(vars)) {
        text = text.replaceAll(`{${k}}`, String(v));
      }
    }
    return text;
  }

  setLocale(locale: string): void {
    if (isSupportedLocale(locale)) {
      this.locale = locale;
    }
  }
}

export const i18n = new I18nStore();
