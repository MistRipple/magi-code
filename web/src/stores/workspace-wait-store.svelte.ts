import { listAgentSessionWorkspaceWaits } from '../web/agent-api';

/**
 * 会话的这一轮正在等别的会话用完工作区（同一仓库同一时刻只有一个主会话在执行）。
 * 事实在 daemon，由 `session.workspace.waiting` / `session.workspace.ready` 事件驱动，这里只是投影。
 */
export const workspaceWaitState = $state({
  bySession: {} as Record<string, { blockingSessionIds: string[] }>,
});

let syncRevision = 0;
let eventRevision = 0;
const sessionEventRevisions = new Map<string, number>();

/** 以 daemon 的快照为准重建：事件只通知变化，页面刷新 / 重连期间错过的变化靠这里补上。 */
export async function syncWorkspaceWaits(): Promise<void> {
  const revision = ++syncRevision;
  const startingEventRevision = eventRevision;
  const response = await listAgentSessionWorkspaceWaits();
  if (revision !== syncRevision) return;
  const next: Record<string, { blockingSessionIds: string[] }> = {};
  for (const wait of response.waits) {
    next[wait.sessionId] = { blockingSessionIds: wait.blockingSessionIds };
  }
  // 读取期间到达的事件只替换相应会话，不能丢掉其他会话的首次快照。
  for (const [sessionId, changedAt] of sessionEventRevisions) {
    if (changedAt > startingEventRevision) {
      const current = workspaceWaitState.bySession[sessionId];
      if (current) next[sessionId] = current;
      else delete next[sessionId];
    } else {
      sessionEventRevisions.delete(sessionId);
    }
  }
  workspaceWaitState.bySession = next;
}

export function markWorkspaceWaiting(sessionId: string, blockingSessionIds: string[]): void {
  const id = sessionId.trim();
  if (!id) return;
  sessionEventRevisions.set(id, ++eventRevision);
  workspaceWaitState.bySession = {
    ...workspaceWaitState.bySession,
    [id]: { blockingSessionIds },
  };
}

export function clearWorkspaceWaiting(sessionId: string): void {
  const id = sessionId.trim();
  if (!id) return;
  sessionEventRevisions.set(id, ++eventRevision);
  if (!(id in workspaceWaitState.bySession)) return;
  const next = { ...workspaceWaitState.bySession };
  delete next[id];
  workspaceWaitState.bySession = next;
}

export function getWorkspaceWait(sessionId: string): { blockingSessionIds: string[] } | null {
  return workspaceWaitState.bySession[sessionId.trim()] ?? null;
}
