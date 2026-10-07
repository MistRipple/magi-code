import type {
  IsolationMergePlanDto,
  SessionIsolationSummaryDto,
} from '../shared/rust-backend-types';
import {
  applyAgentIsolationMerge,
  discardAgentSessionIsolation,
  enableAgentSessionIsolation,
  listAgentSessionIsolations,
  planAgentIsolationMerge,
} from '../web/agent-api';
import { refreshPendingChangesProjection } from '../lib/pending-changes-refresh';

export interface IsolationBinding {
  workspaceId: string;
  workspacePath: string;
}

/**
 * 运行在隔离副本里的会话。权威事实在 daemon（会话 → 隔离副本的登记），这里只是投影，
 * 由 `session.isolation.*` 事件和用户操作刷新。
 */
export const sessionIsolationState = $state({
  bySession: {} as Record<string, SessionIsolationSummaryDto>,
  hydrated: false,
});

/** 合并面板：由横幅、输入区菜单打开，App 里挂载一个全局实例。 */
export const isolationMergeDialogState = $state({
  open: false,
  sessionId: '',
  binding: { workspaceId: '', workspacePath: '' } as IsolationBinding,
});

let syncRevision = 0;

export async function syncSessionIsolations(): Promise<void> {
  const revision = ++syncRevision;
  const response = await listAgentSessionIsolations();
  if (revision !== syncRevision) return;
  const next: Record<string, SessionIsolationSummaryDto> = {};
  for (const isolation of response.isolations) {
    next[isolation.sessionId] = isolation;
  }
  sessionIsolationState.bySession = next;
  sessionIsolationState.hydrated = true;
}

export function getSessionIsolation(sessionId: string): SessionIsolationSummaryDto | null {
  return sessionIsolationState.bySession[sessionId.trim()] ?? null;
}

export function isSessionIsolated(sessionId: string): boolean {
  return getSessionIsolation(sessionId) !== null;
}

function refreshChanges(sessionId: string, binding: IsolationBinding): void {
  void refreshPendingChangesProjection({
    scope: 'workspace',
    sessionId,
    workspaceId: binding.workspaceId,
    workspacePath: binding.workspacePath,
    forceRefresh: true,
  }).catch((error) => {
    console.warn('[session-isolation] 刷新变更列表失败:', error);
  });
}

function errorMessage(error: unknown, fallback: string): string {
  return error instanceof Error && error.message.trim() ? error.message : fallback;
}

export type IsolationActionResult = { ok: true } | { ok: false; error: string };

export async function enableSessionIsolation(
  sessionId: string,
  binding: IsolationBinding,
): Promise<IsolationActionResult> {
  try {
    await enableAgentSessionIsolation(sessionId, binding);
    await syncSessionIsolations();
    refreshChanges(sessionId, binding);
    return { ok: true };
  } catch (error) {
    return { ok: false, error: errorMessage(error, 'enable isolation failed') };
  }
}

export async function discardSessionIsolation(
  sessionId: string,
  binding: IsolationBinding,
  discardChanges: boolean,
): Promise<IsolationActionResult> {
  try {
    await discardAgentSessionIsolation(sessionId, discardChanges, binding);
    await syncSessionIsolations();
    refreshChanges(sessionId, binding);
    return { ok: true };
  } catch (error) {
    return { ok: false, error: errorMessage(error, 'discard isolation failed') };
  }
}

export async function loadIsolationMergePlan(
  sessionId: string,
  binding: IsolationBinding,
): Promise<IsolationMergePlanDto> {
  return await planAgentIsolationMerge(sessionId, binding);
}

export async function applyIsolationMerge(
  sessionId: string,
  binding: IsolationBinding,
  request: Parameters<typeof applyAgentIsolationMerge>[1],
) {
  const outcome = await applyAgentIsolationMerge(sessionId, request, binding);
  refreshChanges(sessionId, binding);
  return outcome;
}

export function openIsolationMergeDialog(sessionId: string, binding: IsolationBinding): void {
  isolationMergeDialogState.sessionId = sessionId.trim();
  isolationMergeDialogState.binding = { ...binding };
  isolationMergeDialogState.open = true;
}

export function closeIsolationMergeDialog(): void {
  isolationMergeDialogState.open = false;
}
