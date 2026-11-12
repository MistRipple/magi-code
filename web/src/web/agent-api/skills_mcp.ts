import { getTransport } from '../../shared/transport';
import type { SkillDetailDto, SkillsLibraryResponseDto } from '../../shared/rust-backend-types';
import { AgentApiError, agentUrl, normalizeMcpServerConfig, parseAgentJson, postGlobalJson } from './core';
import type { AgentLocalSkillInstallRequest, AgentSkillSource } from './types';

export async function addAgentMcpServer(server: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/mcp/add', normalizeMcpServerConfig(server), 'add mcp server');
}

export async function updateAgentMcpServer(serverId: string, updates: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>(
    '/api/settings/mcp/update',
    normalizeMcpServerConfig({ ...updates, id: serverId }),
    'update mcp server',
  );
}

export async function deleteAgentMcpServer(serverId: string): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/mcp/delete', { serverId }, 'delete mcp server');
}

export async function getAgentMcpServerTools(serverId: string): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/mcp/tools', { serverId }, 'get mcp server tools');
}

export async function refreshAgentMcpTools(serverId: string): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/mcp/tools/refresh', { serverId }, 'refresh mcp tools');
}

export async function connectAgentMcpServer(serverId: string): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/mcp/connect', { serverId }, 'connect mcp server');
}

export async function disconnectAgentMcpServer(serverId: string): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/mcp/disconnect', { serverId }, 'disconnect mcp server');
}

export async function addAgentRepository(url: string): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/repositories/add', { url }, 'add repository');
}

export async function updateAgentRepository(repositoryId: string, updates: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/repositories/update', { repositoryId, updates }, 'update repository');
}

export async function deleteAgentRepository(repositoryId: string): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/repositories/delete', { repositoryId }, 'delete repository');
}

export async function refreshAgentRepository(repositoryId: string): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/repositories/refresh', { repositoryId }, 'refresh repository');
}

export async function loadAgentSkillLibrary(): Promise<SkillsLibraryResponseDto> {
  const response = await getTransport().request(agentUrl('/api/settings/skills/library'));
  return await parseAgentJson<SkillsLibraryResponseDto>(response, 'load skill library');
}

export async function getAgentSkillDetail(skillId: string): Promise<SkillDetailDto> {
  const normalized = skillId.trim();
  if (!normalized) {
    throw new AgentApiError(400, 'skillId 不能为空', 'load skill detail');
  }
  const response = await getTransport().request(
    agentUrl('/api/settings/skills/detail', new URLSearchParams({ skillId: normalized }).toString()),
    { cache: 'no-store' },
  );
  return await parseAgentJson<SkillDetailDto>(response, 'load skill detail');
}

export async function installAgentSkill(skillId: string): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/skills/install', { skillId }, 'install skill');
}

export async function installAgentLocalSkill(
  request?: string | AgentLocalSkillInstallRequest,
): Promise<Record<string, unknown>> {
  const payload: Record<string, unknown> = {};
  if (typeof request === 'string') {
    payload.directoryPath = request;
  } else if (request) {
    if (request.directoryPath) {
      payload.directoryPath = request.directoryPath;
    }
    if (request.skillId) {
      payload.skillId = request.skillId;
    }
  }
  return await postGlobalJson<Record<string, unknown>>('/api/settings/skills/install-local', payload, 'install local skill');
}

export async function scanAgentLocalSkillDirectory(directoryPath: string): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/skills/scan-local', { directoryPath }, 'scan local skill directory');
}

export async function saveAgentSkillsConfig(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/skills/config/save', config, 'save skills config');
}

export async function toggleAgentSkill(skillId: string, enabled: boolean): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>(
    '/api/settings/skills/toggle',
    { skillId, enabled },
    'toggle skill',
  );
}

export async function saveAgentSafeguardConfig(config: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/safeguard/save', config, 'save safeguard config');
}

export async function addAgentCustomTool(tool: Record<string, unknown>): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/skills/custom-tool/add', tool, 'add custom tool');
}

export async function removeAgentInstalledSkill(skillId: string, source: AgentSkillSource): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/skills/remove', { skillId, source }, 'remove installed skill');
}

export async function updateAgentSkill(skillId: string): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/skills/update', { skillId }, 'update skill');
}

export async function updateAllAgentSkills(): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/skills/update-all', {}, 'update all skills');
}

export async function checkAgentSkillUpdates(): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/skills/check-updates', {}, 'check skill updates');
}

export async function rollbackAgentSkill(skillId: string): Promise<Record<string, unknown>> {
  return await postGlobalJson<Record<string, unknown>>('/api/settings/skills/rollback', { skillId }, 'rollback skill');
}
