/**
 * 界面语言注册表：前端支持的语言只在这里登记。
 * 新增语言 = 加一份字典 JSON + 在 LOCALES 里加一项（并同步 crates/magi-api/src/locales.rs），
 * 切换器、i18n 存储、设置校验都从这里派生，不要在别处再写语言代码字面量。
 */

import zhCN from './zh-CN.json';
import enUS from './en-US.json';

export const LOCALES = [
  { code: 'zh-CN', nativeName: '简体中文', dictionary: zhCN as Record<string, string> },
  { code: 'en-US', nativeName: 'English', dictionary: enUS as Record<string, string> },
] as const;

export type LocaleCode = (typeof LOCALES)[number]['code'];

export const DEFAULT_LOCALE: LocaleCode = 'zh-CN';

export const SUPPORTED_LOCALE_CODES: readonly LocaleCode[] = LOCALES.map((locale) => locale.code);

export function isSupportedLocale(value: unknown): value is LocaleCode {
  return typeof value === 'string' && (SUPPORTED_LOCALE_CODES as readonly string[]).includes(value);
}

/** 把任意输入归一为受支持的语言，不受支持时回落到默认语言。 */
export function normalizeLocale(value: unknown): LocaleCode {
  return isSupportedLocale(value) ? value : DEFAULT_LOCALE;
}
