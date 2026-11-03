export type MainModelProtocol = 'openai_responses' | 'openai_chat' | 'anthropic_messages';

export interface MainModelCatalogEntry {
  id: string;
  displayName: string;
}

export interface MainModelSource {
  id: string;
  name: string;
  baseUrl: string;
  apiKey: string;
  apiProtocol: MainModelProtocol;
  models: MainModelCatalogEntry[];
}

export interface MainModelSourcesConfig {
  revision: number;
  sources: MainModelSource[];
}

export interface MainModelCatalogGroup {
  sourceId: string;
  name: string;
  kind: 'custom' | 'plugin';
  /** 插件来源所属的插件；用户来源没有。 */
  pluginId?: string;
  models: Array<MainModelCatalogEntry & {
    reasoningCapability: import('./rust-backend-types').ModelReasoningCapability;
  }>;
}

export interface MainModelCatalog {
  groups: MainModelCatalogGroup[];
}

export function mainModelContextWindowKey(sourceId: string, model: string): string {
  const normalizedModel = model.trim().replace(/[A-Z]/g, (letter) => letter.toLowerCase());
  return `${sourceId.trim()}::${normalizedModel}`;
}
