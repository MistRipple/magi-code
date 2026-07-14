export type SessionActivityIndicator = 'running' | 'unread' | 'none';

export interface SessionActivityState {
  isRunning: boolean;
  hasUnreadCompletion: boolean;
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
