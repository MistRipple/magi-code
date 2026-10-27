import { i18n } from '../stores/i18n.svelte';
import type { ThemePack } from './contract';

/** 只翻译内置主题的展示名称，导出和自定义主题仍保留原始内容。 */
export function themeDisplayName(pack: Pick<ThemePack, 'id' | 'name'>): string {
  const key = `appearance.theme.${pack.id}.name`;
  const label = i18n.t(key);
  return label === key ? pack.name : label;
}

export function themeDisplayDescription(pack: Pick<ThemePack, 'id' | 'description'>): string {
  const key = `appearance.theme.${pack.id}.description`;
  const label = i18n.t(key);
  return label === key ? pack.description || '' : label;
}
