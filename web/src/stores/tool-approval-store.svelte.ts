import type {
  PendingToolApprovalDto,
  ToolApprovalDecision,
} from '../shared/rust-backend-types';
import {
  getAgentSessionToolApprovals,
  resolveAgentToolApproval,
} from '../web/agent-api';
import type { AgentBindingOverride } from '../web/agent-binding-context';

type ToolApprovalBinding =
  | { scope: 'personal' }
  | { scope: 'workspace'; workspaceId: string; workspacePath: string };

export const toolApprovalState = $state({
  sessionId: '',
  binding: { scope: 'personal' } as ToolApprovalBinding,
  pending: [] as PendingToolApprovalDto[],
  hydrated: false,
  syncing: false,
  error: '',
});

let syncRevision = 0;

export async function syncToolApprovals(
  sessionId: string,
  binding: ToolApprovalBinding = { scope: 'personal' },
): Promise<void> {
  const normalizedSessionId = sessionId.trim();
  const revision = ++syncRevision;
  if (!normalizedSessionId) {
    toolApprovalState.sessionId = '';
    toolApprovalState.binding = { scope: 'personal' };
    toolApprovalState.pending = [];
    toolApprovalState.hydrated = true;
    toolApprovalState.syncing = false;
    toolApprovalState.error = '';
    return;
  }
  toolApprovalState.sessionId = normalizedSessionId;
  toolApprovalState.binding = binding.scope === 'workspace'
    ? {
        scope: 'workspace',
        workspaceId: binding.workspaceId?.trim() || '',
        workspacePath: binding.workspacePath?.trim() || '',
      }
    : { scope: 'personal' };
  toolApprovalState.syncing = true;
  toolApprovalState.hydrated = false;
  toolApprovalState.error = '';
  try {
    const bindingOverride: AgentBindingOverride = toolApprovalState.binding.scope === 'workspace'
      ? {
          scope: 'workspace',
          workspaceId: toolApprovalState.binding.workspaceId,
          workspacePath: toolApprovalState.binding.workspacePath,
          sessionId: normalizedSessionId,
        }
      : { scope: 'personal', sessionId: normalizedSessionId };
    const response = await getAgentSessionToolApprovals(normalizedSessionId, bindingOverride);
    if (revision !== syncRevision || toolApprovalState.sessionId !== normalizedSessionId) return;
    toolApprovalState.pending = response.pendingApprovals;
    toolApprovalState.hydrated = true;
  } catch (error) {
    if (revision !== syncRevision || toolApprovalState.sessionId !== normalizedSessionId) return;
    toolApprovalState.error = error instanceof Error ? error.message : 'load tool approvals failed';
  } finally {
    if (revision === syncRevision && toolApprovalState.sessionId === normalizedSessionId) {
      toolApprovalState.syncing = false;
    }
  }
}

export function hasPendingToolApproval(sessionId: string): boolean {
  const normalizedSessionId = sessionId.trim();
  return toolApprovalState.sessionId === normalizedSessionId
    && toolApprovalState.pending.length > 0;
}

export function isToolApprovalPending(sessionId: string, approvalId: string): boolean {
  const normalizedSessionId = sessionId.trim();
  const normalizedApprovalId = approvalId.trim();
  return toolApprovalState.sessionId === normalizedSessionId
    && toolApprovalState.pending.some((approval) => approval.approvalId === normalizedApprovalId);
}

export async function resolvePendingToolApproval(
  sessionId: string,
  approvalId: string,
  decision: ToolApprovalDecision,
): Promise<void> {
  const bindingOverride: AgentBindingOverride = toolApprovalState.binding.scope === 'workspace'
    ? {
        scope: 'workspace',
        workspaceId: toolApprovalState.binding.workspaceId,
        workspacePath: toolApprovalState.binding.workspacePath,
        sessionId: sessionId.trim(),
      }
    : { scope: 'personal', sessionId: sessionId.trim() };
  await resolveAgentToolApproval(sessionId, approvalId, decision, bindingOverride);
  if (toolApprovalState.sessionId === sessionId.trim()) {
    toolApprovalState.pending = toolApprovalState.pending.filter(
      (approval) => approval.approvalId !== approvalId.trim(),
    );
    toolApprovalState.hydrated = true;
  }
}
