import { applyPendingChangesProjection } from '../stores/messages.svelte';
import { synchronizeChangeDiffTabs } from '../stores/right-pane.svelte';
import { getAgentPendingChanges } from '../web/agent-api';
import type { Edit } from '../types/message';
import { createSingleFlight } from './single-flight';

export interface PendingChangesRefreshScope {
  scope: 'workspace';
  sessionId?: string;
  workspaceId: string;
  workspacePath: string;
  forceRefresh?: boolean;
}

// 变更列表需要在服务端做一次工作区差异计算，单次可达秒级。切换会话时 bridge 的
// bootstrap 后刷新与 App 的会话变化刷新会几乎同时对同一个作用域各发一次，白白让
// daemon 算两遍。同一作用域已有请求在途时直接共用它；只有“要强制刷新”而在途的不是
// 强制刷新时才另起一个，强制刷新的语义不变。
const runSingleFlight = createSingleFlight<boolean>();
const invalidations = new Map<string, { pending: boolean; promise: Promise<boolean> }>();

function scopeKey(scope: PendingChangesRefreshScope): string {
  return `${scope.workspaceId}\u0000${scope.workspacePath}\u0000${scope.sessionId ?? ''}`;
}

async function loadAndApply(scope: PendingChangesRefreshScope): Promise<boolean> {
  const payload = await getAgentPendingChanges(scope);
  const applied = applyPendingChangesProjection(payload);
  if (applied) {
    synchronizeChangeDiffTabs(
      payload.workspaceId,
      payload.sessionId,
      payload.pendingChanges as Edit[],
    );
  }
  return applied;
}

export function refreshPendingChangesProjection(
  scope: PendingChangesRefreshScope,
): Promise<boolean> {
  return runSingleFlight(scopeKey(scope), scope.forceRefresh === true, () => loadAndApply(scope));
}

// 工具结算可能发生在上一次变更查询期间；该查询不能覆盖后来发生的修改。
// 合并同作用域的失效通知，并在在途查询结束后补读一次最新的 daemon 事实。
export function invalidatePendingChangesProjection(
  scope: PendingChangesRefreshScope,
): Promise<boolean> {
  const key = scopeKey(scope);
  const existing = invalidations.get(key);
  if (existing) {
    existing.pending = true;
    return existing.promise;
  }
  const entry = { pending: true, promise: Promise.resolve(false) };
  entry.promise = Promise.resolve().then(async () => {
    try {
      let applied = false;
      do {
        entry.pending = false;
        applied = await refreshPendingChangesProjection({ ...scope, forceRefresh: true }) || applied;
      } while (entry.pending);
      return applied;
    } finally {
      invalidations.delete(key);
    }
  });
  invalidations.set(key, entry);
  return entry.promise;
}
