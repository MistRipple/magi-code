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
