/**
 * i18n 类型定义
 */

export type LocaleCode = 'zh-CN' | 'en-US';

export const DEFAULT_LOCALE: LocaleCode = 'zh-CN';

export const SUPPORTED_LOCALES: readonly LocaleCode[] = ['zh-CN', 'en-US'] as const;
