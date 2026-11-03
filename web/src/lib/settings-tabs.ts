import type { IconName } from './icons';

export type SettingsTabId =
  | 'model'
  | 'tools'
  | 'browser'
  | 'agents'
  | 'rules'
  | 'safeguard'
  | 'stats'
  | 'appearance'
  | 'project'
  | 'plugins'
  | 'mcpServer';

export interface SettingsTabDefinition {
  id: SettingsTabId;
  icon: IconName;
  titleKey: string;
  descKey: string;
}

/** 设置分类的唯一清单：核心配置 → 能力与扩展 → 使用偏好 → 系统管理与项目信息。 */
export const SETTINGS_TABS: readonly SettingsTabDefinition[] = [
  { id: 'model', icon: 'model', titleKey: 'settings.zone.quickStart', descKey: 'settings.zone.quickStartDesc' },
  { id: 'agents', icon: 'bot', titleKey: 'settings.zone.roles', descKey: 'settings.zone.rolesDesc' },
  { id: 'tools', icon: 'tools', titleKey: 'settings.zone.capabilities', descKey: 'settings.zone.capabilitiesDesc' },
  { id: 'browser', icon: 'globe', titleKey: 'settings.zone.browser', descKey: 'settings.zone.browserDesc' },
  { id: 'mcpServer', icon: 'plug', titleKey: 'settings.zone.mcpServer', descKey: 'settings.zone.mcpServerDesc' },
  { id: 'plugins', icon: 'settings', titleKey: 'settings.zone.plugins', descKey: 'settings.zone.pluginsDesc' },
  { id: 'rules', icon: 'settings', titleKey: 'settings.zone.preferences', descKey: 'settings.zone.preferencesDesc' },
  { id: 'appearance', icon: 'sparkles', titleKey: 'settings.zone.appearance', descKey: 'settings.zone.appearanceDesc' },
  { id: 'safeguard', icon: 'shield', titleKey: 'settings.zone.safeguard', descKey: 'settings.zone.safeguardDesc' },
  { id: 'stats', icon: 'stats', titleKey: 'settings.zone.usage', descKey: 'settings.zone.usageDesc' },
  { id: 'project', icon: 'git-branch', titleKey: 'settings.zone.project', descKey: 'settings.zone.projectDesc' },
];
