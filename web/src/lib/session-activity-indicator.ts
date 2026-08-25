export type SessionActivityIndicator = 'running' | 'unread' | 'none';

export interface SessionActivityState {
  isRunning: boolean;
  hasUnreadCompletion: boolean;
}

export interface SessionRunningStateInput {
  isRunning?: boolean;
  runningTaskCount?: number;
  isCurrentSession: boolean;
  isCurrentWorkspace: boolean;
  isPersonalScope: boolean;
  hasActiveWorkspace: boolean;
  isProcessing: boolean;
}

/**
 * 会话目录运行态的唯一前端解析规则：后端明确值优先，任务数量只作旧
 * 数据兼容，最后才允许当前页面的本地处理中状态参与草稿态显示。
 */
export function resolveSessionRunningState(
  state: SessionRunningStateInput,
): boolean {
  if (typeof state.isRunning === 'boolean') {
    return state.isRunning;
  }
  if (typeof state.runningTaskCount === 'number' && state.runningTaskCount > 0) {
    return true;
  }
  if (!state.isCurrentSession || !state.isProcessing) {
    return false;
  }
  return state.isPersonalScope
    ? !state.hasActiveWorkspace
    : state.isCurrentWorkspace;
}

export interface SessionViewedDecision extends SessionActivityState {
  bootstrapped: boolean;
  sessionHydrating: boolean;
  isCurrentSession: boolean;
}

export function resolveSessionActivityIndicator(
  state: SessionActivityState,
): SessionActivityIndicator {
  if (state.isRunning) {
    return 'running';
  }
  return state.hasUnreadCompletion ? 'unread' : 'none';
}

export function shouldMarkSessionCompletionViewed(
  state: SessionViewedDecision,
): boolean {
  return state.bootstrapped
    && !state.sessionHydrating
    && state.isCurrentSession
    && !state.isRunning
    && state.hasUnreadCompletion;
}

export function deriveHasUnreadCompletion(
  lastCompletedAt: number | undefined,
  lastViewedAt: number | undefined,
): boolean {
  return typeof lastCompletedAt === 'number'
    && Number.isFinite(lastCompletedAt)
    && (!(typeof lastViewedAt === 'number' && Number.isFinite(lastViewedAt))
      || lastCompletedAt > lastViewedAt);
}
