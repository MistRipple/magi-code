/**
 * i18n 状态管理 - Svelte 5 Runes
 * 使用 class 实例模式确保 $state/$derived 跨模块响应式正确追踪
 */

import zhCN from '../i18n/zh-CN.json';
import enUS from '../i18n/en-US.json';
import type { LocaleCode } from '../i18n/types';

const dictionaries: Record<LocaleCode, Record<string, string>> = {
  'zh-CN': zhCN,
  'en-US': enUS,
};

function resolveInitialLocale(): LocaleCode {
  if (typeof window !== 'undefined') {
    const locale = (window as unknown as { __INITIAL_LOCALE__?: string }).__INITIAL_LOCALE__;
    if (locale === 'zh-CN' || locale === 'en-US') {
      return locale;
    }
  }
  return 'zh-CN';
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

  setLocale(locale: LocaleCode): void {
    this.locale = locale;
  }
}

export const i18n = new I18nStore();
