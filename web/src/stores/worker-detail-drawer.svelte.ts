/**
 * Worker 详情 Drawer 状态。
 *
 * 承载全局单例 "当前展开的 worker 详情"：
 * - `activeWorkerTabId` 为 null 时 drawer 关闭
 * - 非 null 时展开，值为 `WorkerTabId`（role 聚合键，与 canonical projection 对齐）
 *
 * Drawer 内容由 `buildTimelinePanelView(projection, 'worker', workerTabId)` 投影出，
 * 不在此 store 内缓存消息，保证单真源、避免二次状态漂移。
 */

export interface WorkerDetailDrawerState {
  activeWorkerTabId: string | null;
}

export const workerDetailDrawerState = $state<WorkerDetailDrawerState>({
  activeWorkerTabId: null,
});

function normalize(id: string | null | undefined): string | null {
  if (typeof id !== 'string') {
    return null;
  }
  const trimmed = id.trim();
  return trimmed.length > 0 ? trimmed : null;
}

export function openWorkerDetailDrawer(workerTabId: string | null | undefined): void {
  const next = normalize(workerTabId);
  if (!next) {
    return;
  }
  workerDetailDrawerState.activeWorkerTabId = next;
}

export function closeWorkerDetailDrawer(): void {
  workerDetailDrawerState.activeWorkerTabId = null;
}

export function isWorkerDetailDrawerOpen(): boolean {
  return workerDetailDrawerState.activeWorkerTabId !== null;
}
