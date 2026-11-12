import { getTransport } from '../../shared/transport';
import type { SessionIsolationListResponseDto, WorkspaceWaitListResponseDto, SessionIsolationResponseDto, IsolationMergePlanDto, IsolationMergeOutcomeDto, IsolationConflictResolutionDto } from '../../shared/rust-backend-types';
import type { AgentBindingOverride } from '../agent-binding-context';
import { AgentApiError, agentUrl, parseAgentJson, postBoundJson } from './core';

function isolationBinding(
  sessionId: string,
  binding: { workspaceId?: string; workspacePath?: string; sourceFolderId?: string } | undefined,
): AgentBindingOverride {
  const workspaceId = binding?.workspaceId?.trim() || '';
  const workspacePath = binding?.workspacePath?.trim() || '';
  return workspaceId || workspacePath
    ? { scope: 'workspace', workspaceId, workspacePath, sessionId, sourceFolderId: binding?.sourceFolderId }
    : { scope: 'personal', sessionId };
}

function requireIsolationSessionId(sessionId: string, action: string): string {
  const normalized = sessionId.trim();
  if (!normalized) {
    throw new AgentApiError(400, 'sessionId 不能为空', action);
  }
  return normalized;
}

/** 当前运行在隔离副本里的全部会话（侧栏、输入区据此标记）。 */
export async function listAgentSessionIsolations(): Promise<SessionIsolationListResponseDto> {
  const response = await getTransport().request(
    agentUrl('/api/session/isolations'),
    { cache: 'no-store' },
  );
  return parseAgentJson<SessionIsolationListResponseDto>(response, 'list session isolations');
}

/** 正在等别的会话用完工作区的会话；页面刷新、重连后据此还原「正在等谁」。 */
export async function listAgentSessionWorkspaceWaits(): Promise<WorkspaceWaitListResponseDto> {
  const response = await getTransport().request(
    agentUrl('/api/session/workspace-waits'),
    { cache: 'no-store' },
  );
  return parseAgentJson<WorkspaceWaitListResponseDto>(response, 'list session workspace waits');
}

export async function enableAgentSessionIsolation(
  sessionId: string,
  binding?: { workspaceId?: string; workspacePath?: string; sourceFolderId?: string },
): Promise<SessionIsolationResponseDto> {
  const normalized = requireIsolationSessionId(sessionId, 'enable session isolation');
  return await postBoundJson<SessionIsolationResponseDto>(
    '/api/session/isolation/enable',
    {},
    'enable session isolation',
    isolationBinding(normalized, binding),
  );
}

export async function discardAgentSessionIsolation(
  sessionId: string,
  discardChanges: boolean,
  binding?: { workspaceId?: string; workspacePath?: string; sourceFolderId?: string },
): Promise<SessionIsolationResponseDto> {
  const normalized = requireIsolationSessionId(sessionId, 'discard session isolation');
  return await postBoundJson<SessionIsolationResponseDto>(
    '/api/session/isolation/discard',
    { discardChanges },
    'discard session isolation',
    isolationBinding(normalized, binding),
  );
}

export async function planAgentIsolationMerge(
  sessionId: string,
  binding?: { workspaceId?: string; workspacePath?: string; sourceFolderId?: string },
): Promise<IsolationMergePlanDto> {
  const normalized = requireIsolationSessionId(sessionId, 'plan isolation merge');
  return await postBoundJson<IsolationMergePlanDto>(
    '/api/session/isolation/merge-plan',
    {},
    'plan isolation merge',
    isolationBinding(normalized, binding),
  );
}

export async function applyAgentIsolationMerge(
  sessionId: string,
  selection: {
    paths?: string[];
    resolutions?: Record<string, IsolationConflictResolutionDto>;
  },
  binding?: { workspaceId?: string; workspacePath?: string; sourceFolderId?: string },
): Promise<IsolationMergeOutcomeDto> {
  const normalized = requireIsolationSessionId(sessionId, 'apply isolation merge');
  return await postBoundJson<IsolationMergeOutcomeDto>(
    '/api/session/isolation/merge',
    {
      ...(selection.paths ? { paths: selection.paths } : {}),
      resolutions: selection.resolutions ?? {},
    },
    'apply isolation merge',
    isolationBinding(normalized, binding),
  );
}
