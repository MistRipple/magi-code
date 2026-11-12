import { getTransport } from '../../shared/transport';
import type { AgentBinding, ModelEngine } from '../../shared/types/registry-types';
import type { RoleTemplate } from '../../shared/types/role-templates';
import type { MainModelCatalog, MainModelSourcesConfig } from '../../shared/main-model-sources';
import type { FetchModelsResponseDto } from '../../shared/rust-backend-types';
import { i18n } from '../../stores/i18n.svelte';
import { isSupportedLocale } from '../../i18n/locales';
import type { AgentBindingOverride } from '../agent-binding-context';
import type { AgentRuntimeSettings } from './types';
import { agentUrl, parseAgentJson, postBoundJson, postGlobalJson, resolveBindingWithOverride, safeWriteLocalStorage } from './core';

export async function updateAgentRuntimeSetting(key: string, value: unknown): Promise<AgentRuntimeSettings> {
  const payload = await postGlobalJson<AgentRuntimeSettings>('/api/settings/update', { key, value }, 'update runtime setting');
  if (key === 'locale' && isSupportedLocale(payload?.locale)) {
    safeWriteLocalStorage('magi-locale', payload.locale);
    i18n.setLocale(payload.locale);
  }
  return payload;
}

export async function saveAgentWorkerConfig(worker: string, config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/worker/save', { worker, config }, 'save worker config');
}

export async function saveAgentUserRules(data: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/user-rules/save', data, 'save user rules');
}

export async function saveAgentOrchestratorConfig(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/orchestrator/save', config, 'save orchestrator config');
}

export async function getAgentMainModelCatalog(): Promise<MainModelCatalog> {
  const response = await getTransport().request(agentUrl('/api/settings/models/catalog'), { cache: 'no-store' });
  return await parseAgentJson<MainModelCatalog>(response, 'load main model catalog');
}

export async function saveAgentMainModelSources(
  config: MainModelSourcesConfig,
): Promise<MainModelSourcesConfig> {
  const response = await saveAgentOrchestratorConfig({
    revision: config.revision,
    expectedRevision: config.revision,
    sources: config.sources,
  });
  return response.config as MainModelSourcesConfig;
}

export async function saveAgentOrchestratorSessionConfig(
  config: Record<string, unknown>,
  bindingOverride?: AgentBindingOverride,
): Promise<Record<string, unknown>> {
  const binding = resolveBindingWithOverride(bindingOverride);
  return await postBoundJson<Record<string, unknown>>(
    '/api/settings/orchestrator/session/save',
    {
      scope: binding.scope,
      config,
    },
    'save session orchestrator config',
    bindingOverride,
  );
}

export async function saveAgentModelContextWindow(
  model: string,
  contextWindowTokens: number,
  sourceId?: string,
): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>(
    '/api/settings/model-context-window/save',
    { model, contextWindowTokens, ...(sourceId?.trim() ? { sourceId: sourceId.trim() } : {}) },
    'save model context window',
  );
}

export async function saveAgentAuxiliaryConfig(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/auxiliary/save', config, 'save auxiliary config');
}

export async function saveAgentVisionConfig(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/vision/save', config, 'save vision config');
}

export async function saveAgentImageGenerationConfig(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/image-generation/save', config, 'save image generation config');
}

export async function removeAgentWorkerConfig(worker: string): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/worker/remove', { worker }, 'remove worker config');
}

export async function testAgentWorkerConnection(worker: string, config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/worker/test', { worker, config }, 'test worker connection');
}

export async function testAgentOrchestratorConnection(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/orchestrator/test', config, 'test orchestrator connection');
}

export async function testAgentAuxiliaryConnection(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/auxiliary/test', config, 'test auxiliary connection');
}

export async function testAgentVisionConnection(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/vision/test', config, 'test vision connection');
}

export async function testAgentImageGenerationConnection(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/image-generation/test', config, 'test image generation connection');
}

export async function listAgentRoleTemplates(): Promise<RoleTemplate[]> {
  const response = await getTransport().request(agentUrl('/api/settings/registry/role-templates'));
  const payload = await parseAgentJson<{ templates?: RoleTemplate[] }>(response, 'load role templates');
  return Array.isArray(payload.templates) ? payload.templates : [];
}

export async function upsertAgentRole(role: Record<string, unknown>, expectedRoleRevision?: number): Promise<RoleTemplate> {
  const payload = await postGlobalJson<{ role?: RoleTemplate }>(
    '/api/settings/registry/roles/upsert',
    {
      role,
      ...(expectedRoleRevision === undefined ? {} : { expectedRoleRevision }),
    },
    'upsert agent role',
  );
  if (!payload.role) throw new Error(i18n.t('settings.agents.error.saveResponse'));
  return payload.role;
}

export async function deleteAgentRole(templateId: string, expectedRoleRevision: number): Promise<void> {
  await postGlobalJson<Record<string, unknown>>(
    '/api/settings/registry/roles/delete',
    { templateId, expectedRoleRevision },
    'delete agent role',
  );
}

export async function importAgentRole(content: string, conflict: 'reject' | 'overwrite' | 'rename' = 'reject', newId?: string): Promise<RoleTemplate> {
  const payload = await postGlobalJson<{ role?: RoleTemplate }>(
    '/api/settings/registry/roles/import',
    { content, conflict, ...(newId ? { newId } : {}) },
    'import agent role',
  );
  if (!payload.role) throw new Error(i18n.t('settings.agents.error.importResponse'));
  return payload.role;
}

export async function exportAgentRole(templateId: string): Promise<{ fileName: string; content: string }> {
  const response = await getTransport().request(agentUrl(`/api/settings/registry/roles/export?templateId=${encodeURIComponent(templateId)}`));
  const payload = await parseAgentJson<{ fileName?: string; content?: string }>(response, 'export agent role');
  if (!payload.fileName || payload.content === undefined) {
    throw new Error(i18n.t('settings.agents.error.exportResponse'));
  }
  return { fileName: payload.fileName, content: payload.content };
}

export async function listAgentRegistryEngines(): Promise<ModelEngine[]> {
  const response = await getTransport().request(agentUrl('/api/settings/registry/engines'));
  const payload = await parseAgentJson<{ engines?: ModelEngine[] }>(response, 'load registry engines');
  return Array.isArray(payload.engines) ? payload.engines : [];
}

export async function listAgentRegistryAgents(): Promise<AgentBinding[]> {
  const response = await getTransport().request(agentUrl('/api/settings/registry/agents'));
  const payload = await parseAgentJson<{ agents?: AgentBinding[] }>(response, 'load registry agents');
  return Array.isArray(payload.agents) ? payload.agents : [];
}

export async function upsertAgentRegistryEngine(engine: ModelEngine): Promise<ModelEngine[]> {
  const payload = await postGlobalJson<{ engines?: ModelEngine[] }>('/api/settings/registry/engines/upsert', engine as unknown as Record<string, unknown>, 'upsert registry engine');
  return Array.isArray(payload.engines) ? payload.engines : [];
}

export async function removeAgentRegistryEngine(engineId: string): Promise<ModelEngine[]> {
  const payload = await postGlobalJson<{ engines?: ModelEngine[] }>('/api/settings/registry/engines/remove', { engineId }, 'remove registry engine');
  return Array.isArray(payload.engines) ? payload.engines : [];
}

export async function upsertAgentRegistryBinding(agent: AgentBinding): Promise<AgentBinding[]> {
  const payload = await postGlobalJson<{ agents?: AgentBinding[] }>('/api/settings/registry/agents/upsert', agent as unknown as Record<string, unknown>, 'upsert registry agent');
  return Array.isArray(payload.agents) ? payload.agents : [];
}

export async function resolveAgentModelReasoning(config: Record<string, unknown>, model: string): Promise<import('../../shared/rust-backend-types').ModelReasoningCapability> {
  return await postGlobalJson('/api/settings/models/reasoning', { config, model }, 'resolve model reasoning');
}

export async function fetchAgentModelList(
  config: Record<string, unknown>,
  target: string,
  workspaceId?: string | null,
): Promise<FetchModelsResponseDto> {
  return await postGlobalJson<FetchModelsResponseDto>(
    '/api/settings/models/fetch',
    { config, target, ...(workspaceId?.trim() ? { workspaceId: workspaceId.trim() } : {}) },
    'fetch model list',
  );
}
