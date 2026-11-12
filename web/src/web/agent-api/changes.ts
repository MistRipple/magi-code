import { getTransport } from '../../shared/transport';
import { i18n } from '../../stores/i18n.svelte';
import type { WorkspaceAgentBindingOverride } from '../agent-binding-context';
import type { AgentChangeDiffPayload, AgentFilePreviewPayload, AgentPendingChangesPayload } from './types';
import { agentUrl, buildBoundQueryWithOverride, buildFilePreviewQuery, findCachedWorkspaceSummary, parseAgentJson, postBoundJson } from './core';

export async function getAgentPendingChanges(
  options: WorkspaceAgentBindingOverride & {
    forceRefresh?: boolean;
  },
): Promise<AgentPendingChangesPayload> {
  try {
    const project = findCachedWorkspaceSummary(options.workspaceId);
    if (!options.sourceFolderId && project.sourceFolders.length > 1) {
      const projections = await Promise.all(project.sourceFolders.map((folder) => getAgentPendingChanges({ ...options, sourceFolderId: folder.sourceFolderId })));
      const count = projections.reduce((total, projection) => total + projection.pendingChanges.length, 0);
      const states = projections.map((projection) => projection.pendingChangesState as Record<string, unknown>);
      const state = states.find((state) => state?.status === 'error') || states.find((state) => state?.status !== 'ready') || states[0];
      return {
        ...projections[0], workspacePath: project.rootPath,
        generatedAt: Math.min(...projections.map((projection) => projection.generatedAt)),
        pendingChanges: projections.flatMap((projection) => projection.pendingChanges),
        pendingChangesState: { ...state, pendingCount: count },
      };
    }
    const query = buildBoundQueryWithOverride(
      options.forceRefresh ? { forceRefresh: 'true' } : {},
      options,
      { includeScope: false },
    );
    const response = await getTransport().request(agentUrl('/api/changes', query));
    return await parseAgentJson<AgentPendingChangesPayload>(response, 'load pending changes');
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error(i18n.t('bridge.agentUnreachable'));
    }
    throw error;
  }
}

export async function getAgentChangeDiff(
  filePath: string,
  options: WorkspaceAgentBindingOverride,
): Promise<AgentChangeDiffPayload> {
  try {
    const query = buildBoundQueryWithOverride({ filePath, ...(options.sourceFolderId ? { sourceFolderId: options.sourceFolderId } : {}) }, options, { includeScope: false });
    const response = await getTransport().request(agentUrl(`/api/changes/diff`, query));
    return await parseAgentJson<AgentChangeDiffPayload>(response, 'load change diff');
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error(i18n.t('bridge.agentUnreachable'));
    }
    throw error;
  }
}

export async function getAgentFilePreview(
  filePath: string,
  options: { includeSession?: boolean; sessionId?: string; workspaceId?: string; workspacePath?: string; sourceFolderId?: string } = {},
): Promise<AgentFilePreviewPayload> {
  try {
    const query = buildFilePreviewQuery(filePath, options);
    const response = await getTransport().request(agentUrl(`/api/files/content`, query));
    return await parseAgentJson<AgentFilePreviewPayload>(response, 'load file preview');
  } catch (error) {
    if (error instanceof TypeError) {
      throw new Error(i18n.t('bridge.agentUnreachable'));
    }
    throw error;
  }
}

export async function approveAgentChange(
  filePath: string,
  options: WorkspaceAgentBindingOverride,
): Promise<void> {
  await postBoundJson('/api/changes/approve', { filePath }, 'approve change', options);
}

export async function revertAgentChange(
  filePath: string,
  options: WorkspaceAgentBindingOverride,
): Promise<void> {
  await postBoundJson('/api/changes/revert', { filePath }, 'revert change', options);
}

async function mutateProjectFolders(
  options: WorkspaceAgentBindingOverride,
  action: (binding: WorkspaceAgentBindingOverride) => Promise<unknown>,
): Promise<void> {
  const folders = options.sourceFolderId ? [] : findCachedWorkspaceSummary(options.workspaceId).sourceFolders;
  const bindings = folders.length > 1 ? folders.map((folder) => ({ ...options, sourceFolderId: folder.sourceFolderId })) : [options];
  for (const binding of bindings) await action(binding);
}

export async function approveAllAgentChanges(
  options: WorkspaceAgentBindingOverride,
): Promise<void> {
  await mutateProjectFolders(options, (binding) => postBoundJson('/api/changes/approve-all', {}, 'approve all changes', binding));
}

export async function revertAllAgentChanges(
  options: WorkspaceAgentBindingOverride,
): Promise<void> {
  await mutateProjectFolders(options, (binding) => postBoundJson('/api/changes/revert-all', {}, 'revert all changes', binding));
}

export async function revertAgentExecutionGroupChanges(
  executionGroupId: string,
  options: WorkspaceAgentBindingOverride,
): Promise<void> {
  await mutateProjectFolders(options, (binding) => postBoundJson(
    '/api/changes/revert-execution-group',
    { executionGroupId },
    'revert execution group changes',
    binding,
  ));
}
