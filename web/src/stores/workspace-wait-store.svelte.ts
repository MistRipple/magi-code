/**
 * 会话的这一轮正在等别的会话用完工作区（同一仓库同一时刻只有一个主会话在执行）。
 * 事实在 daemon，由 `session.workspace.waiting` / `session.workspace.ready` 事件驱动，这里只是投影。
 */
export const workspaceWaitState = $state({
  bySession: {} as Record<string, { blockingSessionIds: string[] }>,
});

export function markWorkspaceWaiting(sessionId: string, blockingSessionIds: string[]): void {
  const id = sessionId.trim();
  if (!id) return;
  workspaceWaitState.bySession = {
    ...workspaceWaitState.bySession,
    [id]: { blockingSessionIds },
  };
}

export function clearWorkspaceWaiting(sessionId: string): void {
  const id = sessionId.trim();
  if (!id || !(id in workspaceWaitState.bySession)) return;
  const next = { ...workspaceWaitState.bySession };
  delete next[id];
  workspaceWaitState.bySession = next;
}

export function getWorkspaceWait(sessionId: string): { blockingSessionIds: string[] } | null {
  return workspaceWaitState.bySession[sessionId.trim()] ?? null;
}
