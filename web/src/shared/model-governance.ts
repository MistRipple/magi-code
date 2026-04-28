/**
 * Model Governance 工具函数
 *
 * 从 magi 原始 src/orchestrator/registry/model-governance.ts 提取前端所需子集。
 * 前端自包含版本 — 不引入 orchestrator 重量级依赖。
 */

import type { AgentBinding, ModelEngine } from './types/registry-types';
import type { RoleTemplate } from './types/role-templates';

export interface ModelEngineLLM {
  enabled?: boolean;
  [key: string]: unknown;
}

export interface ModelEngineLike {
  llm?: ModelEngineLLM;
}

export interface WorkerConfigLike {
  enabled?: boolean;
}

export interface RoleUsageSummary {
  templateId: string;
  displayName: string;
}

export interface ModelStatusLike {
  status?: string;
}

export type ModelListFetchBlockReason =
  | 'full_url_mode'
  | 'missing_base_url_or_api_key';

export interface ModelListFetchConfigLike {
  baseUrl?: unknown;
  apiKey?: unknown;
  urlMode?: unknown;
}

function trimmedConfigString(value: unknown): string {
  return typeof value === 'string' ? value.trim() : '';
}

export function resolveModelListFetchBlockReason(
  config: ModelListFetchConfigLike | null | undefined,
): ModelListFetchBlockReason | null {
  if (!config) {
    return 'missing_base_url_or_api_key';
  }

  const urlMode = trimmedConfigString(config.urlMode).toLowerCase();
  if (urlMode === 'full') {
    return 'full_url_mode';
  }

  if (!trimmedConfigString(config.baseUrl) || !trimmedConfigString(config.apiKey)) {
    return 'missing_base_url_or_api_key';
  }

  return null;
}

export function canFetchModelList(
  config: ModelListFetchConfigLike | null | undefined,
): boolean {
  return resolveModelListFetchBlockReason(config) === null;
}

export function isRegistryEngineEnabled(engine: Pick<ModelEngineLike, 'llm'> | null | undefined): boolean {
  if (!engine?.llm || typeof engine.llm !== 'object') {
    return false;
  }
  return engine.llm.enabled !== false;
}

export function isEngineEnabled(
  engineId: string,
  registryEngines: ReadonlyArray<ModelEngine>,
  workerConfigs: Readonly<Record<string, WorkerConfigLike>> = {},
): boolean {
  if (!engineId) {
    return false;
  }

  const localConfig = workerConfigs[engineId];
  if (localConfig && typeof localConfig === 'object') {
    return localConfig.enabled !== false;
  }

  const engine = registryEngines.find((item) => item.id === engineId);
  return isRegistryEngineEnabled(engine);
}

export function resolveSelectableRegistryEngines(
  registryEngines: ReadonlyArray<ModelEngine>,
  workerConfigs: Readonly<Record<string, WorkerConfigLike>> = {},
): ModelEngine[] {
  return registryEngines.filter((engine) => isEngineEnabled(engine.id, registryEngines, workerConfigs));
}

export function resolveEnabledRoleUsagesForEngine(
  engineId: string,
  registryAgents: ReadonlyArray<AgentBinding>,
  roleTemplates: ReadonlyArray<RoleTemplate>,
): RoleUsageSummary[] {
  if (!engineId) {
    return [];
  }

  const templateMap = new Map(roleTemplates.map((template) => [template.templateId, template]));
  return registryAgents
    .filter(
      (agent) =>
        agent.enabled !== false
        && agent.modelSource === 'engine'
        && agent.engineId === engineId,
    )
    .map((agent) => ({
      templateId: agent.templateId,
      displayName: (() => {
        const template = templateMap.get(agent.templateId);
        return template ? template.displayName : agent.templateId;
      })(),
    }));
}

export function isAgentBindingOperational(
  agent: Pick<AgentBinding, 'enabled' | 'modelSource' | 'engineId'>,
  registryEngines: ReadonlyArray<ModelEngine>,
  workerConfigs: Readonly<Record<string, WorkerConfigLike>> = {},
): boolean {
  if (agent.enabled === false) {
    return false;
  }
  if (agent.modelSource !== 'engine') {
    return true;
  }
  return isEngineEnabled(agent.engineId, registryEngines, workerConfigs);
}

export function resolveModelConfigTabStatus(
  target: 'orch' | 'comp' | string,
  modelStatuses: Readonly<Record<string, ModelStatusLike>>,
  workerConfigs: Readonly<Record<string, WorkerConfigLike>> = {},
): string {
  if (target === 'orch') {
    return modelStatuses.orchestrator?.status || 'not_configured';
  }
  if (target === 'comp') {
    return modelStatuses.auxiliary?.status || 'not_configured';
  }

  if (workerConfigs[target] && workerConfigs[target].enabled === false) {
    return 'disabled';
  }

  return modelStatuses[target]?.status || 'not_configured';
}
