import { getTransport } from '../../shared/transport';
import type { EnhancePromptRequestDto } from '../../shared/rust-backend-types';
import { i18n } from '../../stores/i18n.svelte';
import type { AccessProfile } from '../../shared/access-profile';
import { normalizeAccessProfile, readStoredAccessProfile } from '../../shared/access-profile';
import type { AgentExecutionStatsPayload, AgentSettingsBootstrapSnapshot, AgentToolCatalogDiagnosticsSnapshot, SafeguardAuditPage } from './types';
import { agentUrl, buildBoundQuery, normalizeBuiltinTools, normalizeCapabilityDependencies, normalizeCommandEnvironment, normalizeSettingsBootstrapPayload, parseAgentJson, postBoundJson, postGlobalJson } from './core';

export async function getAgentSettingsBootstrap(
  options: { bootstrapScope?: 'core' | 'full'; accessProfile?: AccessProfile | null } = {},
): Promise<AgentSettingsBootstrapSnapshot> {
  try {
    const query = buildBoundQuery({
      ...(options.bootstrapScope === 'core' ? { bootstrapScope: 'core' } : {}),
      accessProfile: normalizeAccessProfile(options.accessProfile ?? readStoredAccessProfile()),
    });
    const response = await getTransport().request(agentUrl('/api/settings/bootstrap', query));
    const payload = await parseAgentJson<Record<string, unknown>>(response, 'load settings bootstrap');
    return normalizeSettingsBootstrapPayload(payload);
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error(i18n.t('bridge.agentUnreachable'));
    }
    throw error;
  }
}

export async function loadAgentToolCatalogDiagnostics(
  options: { accessProfile?: AccessProfile | null; refreshEnvironment?: boolean } = {},
): Promise<AgentToolCatalogDiagnosticsSnapshot> {
  try {
    const query = buildBoundQuery({
      includeExternal: 'true',
      includeMcpServers: 'true',
      includeAgentRoles: 'true',
      accessProfile: normalizeAccessProfile(options.accessProfile ?? readStoredAccessProfile()),
      ...(options.refreshEnvironment ? { refreshEnvironment: 'true' } : {}),
    });
    const response = await getTransport().request(agentUrl('/api/tools/catalog', query));
    const payload = await parseAgentJson<Record<string, unknown>>(response, 'load tool catalog diagnostics');
    const rawBuiltinTools = Array.isArray(payload.tools)
      ? payload.tools.filter((tool) => {
          return Boolean(
            tool
              && typeof tool === 'object'
              && !Array.isArray(tool)
              && (tool as Record<string, unknown>).public === true,
          );
        })
      : [];
    return {
      builtinTools: normalizeBuiltinTools(rawBuiltinTools),
      capabilityDependencies: normalizeCapabilityDependencies(payload.runtimeDependencies),
      commandEnvironment: normalizeCommandEnvironment(payload.commandEnvironment),
    };
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error(i18n.t('bridge.agentUnreachable'));
    }
    throw error;
  }
}

export async function resetAgentExecutionStats(): Promise<Record<string, unknown>> {
  try {
    const response = await getTransport().request(agentUrl('/api/settings/stats/reset'), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({}),
    });
    return await parseAgentJson<Record<string, unknown>>(response, 'reset execution stats');
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error(i18n.t('bridge.agentUnreachable'));
    }
    throw error;
  }
}

export async function getSafeguardAuditPage(before?: number | null): Promise<SafeguardAuditPage> {
  const query = typeof before === 'number' ? `?before=${before}` : '';
  try {
    const response = await getTransport().request(agentUrl(`/api/settings/safeguard/audit${query}`));
    return await parseAgentJson<SafeguardAuditPage>(response, 'load safeguard audit');
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error(i18n.t('bridge.agentUnreachable'));
    }
    throw error;
  }
}

export async function deleteSafeguardAudit(
  selection: { eventIds: string[] } | { all: true },
): Promise<{ removed: number; total: number }> {
  return await postGlobalJson<{ removed: number; total: number }>(
    '/api/settings/safeguard/audit/delete',
    selection,
    'delete safeguard audit',
  );
}

export async function getAgentExecutionStats(sessionId?: string): Promise<AgentExecutionStatsPayload> {
  try {
    const query = sessionId ? new URLSearchParams({ sessionId }).toString() : undefined;
    const response = await getTransport().request(agentUrl('/api/settings/stats', query));
    return await parseAgentJson<AgentExecutionStatsPayload>(response, 'load execution stats');
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error(i18n.t('bridge.agentUnreachable'));
    }
    throw error;
  }
}

export async function enhanceAgentPrompt(
  request: EnhancePromptRequestDto,
  signal?: AbortSignal,
): Promise<{ enhancedPrompt: string; error?: string }> {
  return await postBoundJson<{ enhancedPrompt: string; error?: string }>(
    '/api/prompt/enhance',
    request,
    'enhance prompt',
    undefined,
    signal,
  );
}
