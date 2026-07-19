/**
 * Model Governance 工具函数
 *
 * 从 magi 原始 src/orchestrator/registry/model-governance.ts 提取前端所需子集。
 * 前端自包含版本 — 不引入 orchestrator 重量级依赖。
 */

import type { AgentBinding, ModelEngine } from './types/registry-types';

export interface ModelStatusLike {
  status?: string;
}

export type ModelListFetchBlockReason =
  | 'full_url_mode'
  | 'missing_base_url_or_api_key';

export type ResolvedModelApiProtocol = 'openai_chat' | 'anthropic_messages';

export interface ModelListFetchConfigLike {
  baseUrl?: unknown;
  apiKey?: unknown;
  urlMode?: unknown;
  model?: unknown;
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

export function resolveModelApiProtocol(
  config: Pick<ModelListFetchConfigLike, 'baseUrl' | 'urlMode' | 'model'> | null | undefined,
): ResolvedModelApiProtocol {
  const urlMode = trimmedConfigString(config?.urlMode).toLowerCase();
  const baseUrl = trimmedConfigString(config?.baseUrl)
    .replace(/\/+$/, '')
    .toLowerCase();
  const usesAnthropicEndpoint = baseUrl.endsWith('/anthropic') || baseUrl.endsWith('/messages');
  if (urlMode === 'full') {
    return usesAnthropicEndpoint ? 'anthropic_messages' : 'openai_chat';
  }

  const model = trimmedConfigString(config?.model).toLowerCase();
  return usesAnthropicEndpoint || model.includes('claude')
    ? 'anthropic_messages'
    : 'openai_chat';
}

/**
 * 引擎是否存在于 Registry —— 用作角色绑定的可选性判定。
 *
 * 历史上这里还要校验 `llm.enabled !== false`，但启用开关已经收敛到「角色应用决定是否使用」，
 * 这里只判断引擎是否在配置列表里。
 */
export function isEngineRegistered(
  engineId: string,
  registryEngines: ReadonlyArray<ModelEngine>,
): boolean {
  if (!engineId) return false;
  return registryEngines.some((engine) => engine.id === engineId);
}

export function resolveSelectableRegistryEngines(
  registryEngines: ReadonlyArray<ModelEngine>,
): ModelEngine[] {
  return [...registryEngines];
}

export function isAgentBindingOperational(
  agent: Pick<AgentBinding, 'engineId'>,
  registryEngines: ReadonlyArray<ModelEngine>,
): boolean {
  // engineId 空串 = 继承编排模型，编排模型由 orchestrator 章节单独治理，
  // 角色 binding 自身视为 operational。
  if (!agent.engineId) {
    return true;
  }
  return isEngineRegistered(agent.engineId, registryEngines);
}

export function resolveModelConfigTabStatus(
  target: 'orch' | 'comp' | string,
  modelStatuses: Readonly<Record<string, ModelStatusLike>>,
): string {
  if (target === 'orch') {
    return modelStatuses.orchestrator?.status || 'not_configured';
  }
  if (target === 'comp') {
    return modelStatuses.auxiliary?.status || 'not_configured';
  }
  return modelStatuses[target]?.status || 'not_configured';
}
