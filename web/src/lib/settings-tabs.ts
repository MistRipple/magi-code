import type { IconName } from './icons';

export type SettingsTabId =
  | 'model'
  | 'tools'
  | 'browser'
  | 'agents'
  | 'rules'
  | 'stats'
  | 'appearance'
  | 'project';

export interface SettingsTabDefinition {
  id: SettingsTabId;
  icon: IconName;
  titleKey: string;
  descKey: string;
}

/** 设置分类的唯一清单：导航与页头标题都从这里生成，顺序即导航顺序。 */
export const SETTINGS_TABS: readonly SettingsTabDefinition[] = [
  { id: 'model', icon: 'model', titleKey: 'settings.zone.quickStart', descKey: 'settings.zone.quickStartDesc' },
  { id: 'tools', icon: 'tools', titleKey: 'settings.zone.capabilities', descKey: 'settings.zone.capabilitiesDesc' },
  { id: 'browser', icon: 'globe', titleKey: 'settings.zone.browser', descKey: 'settings.zone.browserDesc' },
  { id: 'agents', icon: 'bot', titleKey: 'settings.zone.roles', descKey: 'settings.zone.rolesDesc' },
  { id: 'rules', icon: 'shield', titleKey: 'settings.zone.preferences', descKey: 'settings.zone.preferencesDesc' },
  { id: 'stats', icon: 'stats', titleKey: 'settings.zone.usage', descKey: 'settings.zone.usageDesc' },
  { id: 'appearance', icon: 'sparkles', titleKey: 'settings.zone.appearance', descKey: 'settings.zone.appearanceDesc' },
  { id: 'project', icon: 'git-branch', titleKey: 'settings.zone.project', descKey: 'settings.zone.projectDesc' },
];
