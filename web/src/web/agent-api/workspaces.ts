import { getTransport } from '../../shared/transport';
import { i18n } from '../../stores/i18n.svelte';
import type { AgentBindingOverride } from '../agent-binding-context';
import type { AgentFileRevealTarget, AgentWorkspaceRegistrationResult, AgentWorkspaceSummary, DirectoryListResult, RawAgentWorkspaceSummary, ResolvedAgentPath, WorkspaceDirectoryListResult } from './types';
import { AgentApiError, agentUrl, cacheWorkspaceSummaries, cachedWorkspaceSummaries, normalizeWorkspaceSummary, parseAgentJson, postJsonWithBinding } from './core';

export async function listAgentWorkspaces(): Promise<AgentWorkspaceSummary[]> {
  try {
    const response = await getTransport().request(agentUrl('/api/workspaces'));
    const payload = await parseAgentJson<{ workspaces?: RawAgentWorkspaceSummary[] }>(response, 'list workspaces');
    return cacheWorkspaceSummaries(
      Array.isArray(payload.workspaces)
        ? payload.workspaces.map((workspace) => normalizeWorkspaceSummary(workspace))
        : [],
    );
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error(i18n.t('bridge.agentUnreachable'));
    }
    throw error;
  }
}

export async function registerAgentWorkspace(rootPath: string): Promise<AgentWorkspaceRegistrationResult> {
  try {
    const response = await getTransport().request(agentUrl('/api/workspaces/register'), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        path: rootPath,
      }),
    });
    const payload = await parseAgentJson<{ workspaceId?: string }>(response, 'register workspace');
    const workspaceId = payload.workspaceId?.trim() || '';
    if (!workspaceId) {
      throw new Error('工作区注册响应缺少 workspaceId');
    }
    return {
      workspaceId,
      workspaces: await listAgentWorkspaces(),
    };
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error(i18n.t('bridge.agentUnreachable'));
    }
    throw error;
  }
}

export async function createAgentProject(
  name: string,
  sourceFolders: Array<{ sourceFolderId?: string; path: string; name?: string }>,
  primarySourceFolderId?: string,
): Promise<AgentWorkspaceSummary> {
  const response = await getTransport().request(agentUrl('/api/workspaces/create'), {
    method: 'POST', headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ name, sourceFolders, ...(primarySourceFolderId ? { primarySourceFolderId } : {}) }),
  });
  const payload = await parseAgentJson<{ workspace?: RawAgentWorkspaceSummary }>(response, 'create project');
  if (!payload.workspace) throw new Error('项目创建响应缺少 workspace');
  const project = normalizeWorkspaceSummary(payload.workspace);
  cacheWorkspaceSummaries([...cachedWorkspaceSummaries.filter((item) => item.workspaceId !== project.workspaceId), project]);
  return project;
}

export async function updateAgentProject(
  workspace: AgentWorkspaceSummary,
  name: string,
  sourceFolders: Array<{ sourceFolderId: string; path: string; name?: string }>,
  primarySourceFolderId: string,
): Promise<AgentWorkspaceSummary> {
  const response = await getTransport().request(agentUrl('/api/workspaces/update'), {
    method: 'POST', headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      workspaceId: workspace.workspaceId,
      expectedRevision: workspace.revision,
      name,
      sourceFolders,
      primarySourceFolderId,
    }),
  });
  const payload = await parseAgentJson<{ workspace?: RawAgentWorkspaceSummary }>(response, 'update project');
  if (!payload.workspace) throw new Error('项目更新响应缺少 workspace');
  const project = normalizeWorkspaceSummary(payload.workspace);
  cacheWorkspaceSummaries(cachedWorkspaceSummaries.map((item) => item.workspaceId === project.workspaceId ? project : item));
  return project;
}

export async function removeAgentWorkspace(workspaceId: string): Promise<AgentWorkspaceSummary[]> {
  try {
    const response = await getTransport().request(agentUrl('/api/workspaces/remove'), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        workspaceId,
      }),
    });
    const payload = await parseAgentJson<{ workspaces?: RawAgentWorkspaceSummary[]; removed?: boolean }>(response, 'remove workspace');
    if (Array.isArray(payload.workspaces)) {
      return cacheWorkspaceSummaries(payload.workspaces.map((workspace) => normalizeWorkspaceSummary(workspace)));
    }
    if (payload.removed) {
      return await listAgentWorkspaces();
    }
    return [];
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error(i18n.t('bridge.agentUnreachable'));
    }
    throw error;
  }
}

function throwNormalizedDirectoryError(error: unknown): never {
  if (error instanceof TypeError) {
    throw new Error(i18n.t('bridge.agentUnreachable'));
  }
  if (error instanceof AgentApiError) {
    const message = error.message.trim();
    if (message.includes('ENOENT')) {
      throw new Error(i18n.t('bridge.dirNotFound'));
    }
    if (message.includes('EACCES') || message.includes('EPERM')) {
      throw new Error(i18n.t('bridge.dirNoAccess'));
    }
  }
  throw error;
}

export async function listAgentDirectory(
  dirPath: string,
  showHidden: boolean,
  workspaceId: string,
  sessionId?: string,
  sourceFolderId?: string,
): Promise<WorkspaceDirectoryListResult> {
  try {
    const query = new URLSearchParams();
    if (dirPath) {
      query.set('path', dirPath);
    }
    if (workspaceId) query.set('workspaceId', workspaceId);
    if (sourceFolderId) query.set('sourceFolderId', sourceFolderId);
    if (sessionId) query.set('sessionId', sessionId);
    if (showHidden) {
      query.set('showHidden', '1');
    }
    const response = await getTransport().request(agentUrl('/api/filesystem/list', query.toString()));
    return await parseAgentJson<WorkspaceDirectoryListResult>(response, 'list directory');
  } catch (error) {
    throwNormalizedDirectoryError(error);
  }
}

export async function browseAgentDirectory(
  options: {
    pathRef?: string;
    input?: string;
    basePathRef?: string;
    showHidden?: boolean;
    includeFiles?: boolean;
  } = {},
): Promise<DirectoryListResult> {
  try {
    const query = new URLSearchParams();
    if (options.pathRef) {
      query.set('pathRef', options.pathRef);
    }
    if (options.input) {
      query.set('path', options.input);
    }
    if (options.basePathRef) {
      query.set('basePathRef', options.basePathRef);
    }
    if (options.showHidden) {
      query.set('showHidden', '1');
    }
    if (options.includeFiles) {
      query.set('includeFiles', '1');
    }
    const response = await getTransport().request(agentUrl('/api/filesystem/browse', query.toString()));
    return await parseAgentJson<DirectoryListResult>(response, 'browse directory');
  } catch (error) {
    throwNormalizedDirectoryError(error);
  }
}

export async function resolveAgentPath(
  input: string,
  basePathRef?: string,
): Promise<ResolvedAgentPath> {
  return await postJsonWithBinding<ResolvedAgentPath>(
    '/api/filesystem/resolve',
    { input, ...(basePathRef ? { basePathRef } : {}) },
    'resolve filesystem path',
    undefined,
    false,
    undefined,
    { includeWorkspaceBinding: false },
  );
}

export async function resolveAgentFileRevealTarget(
  filePath: string,
  scope: AgentBindingOverride,
  signal?: AbortSignal,
): Promise<AgentFileRevealTarget> {
  return await postJsonWithBinding<AgentFileRevealTarget>(
    '/api/files/reveal-target',
    { filePath, ...(scope.scope === 'workspace' && scope.sourceFolderId ? { sourceFolderId: scope.sourceFolderId } : {}) },
    'resolve workspace file reveal target',
    scope,
    true,
    signal,
  );
}
