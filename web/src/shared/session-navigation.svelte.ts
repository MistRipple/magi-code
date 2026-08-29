import { messagesState } from '../stores/messages.svelte';
import { getClientBridge } from './bridges/bridge-runtime';
import type { ClientBridgeMessage } from './bridges/client-bridge';
import { copyOrchestratorSessionConfig } from './orchestrator-session-config';

export type SessionNavigationTarget =
  | { kind: 'draft'; scope: 'personal' }
  | { kind: 'draft'; scope: 'workspace'; workspaceId: string; workspacePath: string }
  | { kind: 'session'; scope: 'personal'; sessionId: string }
  | { kind: 'session'; scope: 'workspace'; workspaceId: string; workspacePath: string; sessionId: string };

export interface SessionNavigationTransaction {
  requestId: string;
  target: SessionNavigationTarget;
  startedAt: number;
  /** 导航的唯一完成信号；成功 resolve，失败/超时 reject。 */
  completion: Promise<void>;
}

export const SESSION_NAVIGATION_TIMEOUT_MS = 10_000;

export class SessionNavigationSupersededError extends Error {
  constructor() {
    super('会话导航已被新的导航请求替代');
    this.name = 'SessionNavigationSupersededError';
  }
}

export const sessionNavigationState = $state({
  pending: null as SessionNavigationTransaction | null,
});

type SessionNavigationAttempt = {
  transaction: SessionNavigationTransaction;
  resolve: () => void;
  reject: (reason?: unknown) => void;
  timeoutId: number;
};

const sessionNavigationAttempts = new Map<string, SessionNavigationAttempt>();

export type SessionNavigationCommitTarget = {
  kind: SessionNavigationTarget['kind'];
  scope: 'personal' | 'workspace';
  workspaceId?: string;
  workspacePath?: string;
  sessionId?: string;
};

function normalizeTarget(target: SessionNavigationTarget): SessionNavigationTarget | null {
  if (target.scope === 'personal') {
    if (target.kind === 'draft') return { kind: 'draft', scope: 'personal' };
    const sessionId = target.sessionId.trim();
    return sessionId ? { kind: 'session', scope: 'personal', sessionId } : null;
  }
  const workspaceId = target.workspaceId.trim();
  const workspacePath = target.workspacePath.trim();
  if (!workspaceId || !workspacePath) return null;
  if (target.kind === 'draft') return { kind: 'draft', scope: 'workspace', workspaceId, workspacePath };
  const sessionId = target.sessionId.trim();
  return sessionId ? { kind: 'session', scope: 'workspace', workspaceId, workspacePath, sessionId } : null;
}

function inheritedDraftConfig(): Record<string, unknown> {
  if (!messagesState.currentSessionId?.trim()) {
    const draftConfig = copyOrchestratorSessionConfig(
      messagesState.draftOrchestratorSessionConfig,
      undefined,
    );
    if (typeof draftConfig.model === 'string' && draftConfig.model.trim()) {
      return draftConfig;
    }
  }
  const snapshot = messagesState.settingsBootstrapSnapshot;
  return copyOrchestratorSessionConfig(
    snapshot?.orchestratorSessionDefaults,
    snapshot?.effectiveOrchestratorConfig,
  );
}

function navigationMessage(transaction: SessionNavigationTransaction): ClientBridgeMessage {
  const { target, requestId } = transaction;
  if (target.kind === 'draft') {
    return {
      type: 'navigateSession',
      requestId,
      target: 'draft',
      scope: target.scope,
      ...(target.scope === 'workspace'
        ? { workspaceId: target.workspaceId, workspacePath: target.workspacePath }
        : {}),
      orchestratorSessionConfig: inheritedDraftConfig(),
    };
  }
  return {
    type: 'navigateSession',
    requestId,
    target: 'session',
    scope: target.scope,
    ...(target.scope === 'workspace'
      ? { workspaceId: target.workspaceId, workspacePath: target.workspacePath }
      : {}),
    sessionId: target.sessionId,
  };
}

function navigationTargetsEqual(
  left: SessionNavigationTarget,
  right: SessionNavigationTarget,
): boolean {
  if (left.kind !== right.kind || left.scope !== right.scope) {
    return false;
  }
  if (left.scope === 'personal') {
    return !('sessionId' in left) || left.sessionId === (('sessionId' in right) ? right.sessionId : '');
  }
  const leftWorkspaceId = 'workspaceId' in left ? left.workspaceId : '';
  const rightWorkspaceId = 'workspaceId' in right ? right.workspaceId : '';
  const leftWorkspacePath = 'workspacePath' in left ? left.workspacePath : '';
  const rightWorkspacePath = 'workspacePath' in right ? right.workspacePath : '';
  if (leftWorkspaceId !== rightWorkspaceId || leftWorkspacePath !== rightWorkspacePath) {
    return false;
  }
  return !('sessionId' in left) || left.sessionId === (('sessionId' in right) ? right.sessionId : '');
}

export function navigateSession(target: SessionNavigationTarget): SessionNavigationTransaction | null {
  const normalized = normalizeTarget(target);
  if (!normalized) return null;
  const pending = sessionNavigationState.pending;
  if (pending) {
    if (navigationTargetsEqual(pending.target, normalized)) {
      return pending;
    }
    // 导航代表最新的用户意图。结束旧事务，避免旧请求长期锁住会话操作。
    failSessionNavigation(pending.requestId, new SessionNavigationSupersededError());
  }
  let resolveCompletion!: () => void;
  let rejectCompletion!: (reason?: unknown) => void;
  const completion = new Promise<void>((resolve, reject) => {
    resolveCompletion = resolve;
    rejectCompletion = reject;
  });
  // 调用方并不一定需要等待导航（例如侧栏切换），失败也不能制造
  // unhandled rejection；waitForSessionNavigation 仍会观察同一个 Promise。
  completion.catch(() => undefined);
  const transaction: SessionNavigationTransaction = {
    requestId: `session-navigation-${Date.now()}-${Math.random().toString(36).slice(2, 10)}`,
    target: normalized,
    startedAt: Date.now(),
    completion,
  };
  sessionNavigationState.pending = transaction;
  const timeoutId = window.setTimeout(() => {
    failSessionNavigation(transaction.requestId, new Error('会话导航超时'));
  }, SESSION_NAVIGATION_TIMEOUT_MS);
  sessionNavigationAttempts.set(transaction.requestId, {
    transaction,
    resolve: resolveCompletion,
    reject: rejectCompletion,
    timeoutId,
  });
  try {
    getClientBridge().postMessage(navigationMessage(transaction));
  } catch (error) {
    failSessionNavigation(transaction.requestId, error);
  }
  return transaction;
}

function normalizeNavigationRequestId(requestId: unknown): string {
  return typeof requestId === 'string' ? requestId.trim() : '';
}

function navigationFailure(error: unknown, fallback = '会话导航失败'): Error {
  if (error instanceof Error && error.message.trim()) {
    return error;
  }
  if (typeof error === 'string' && error.trim()) {
    return new Error(error.trim());
  }
  return new Error(fallback);
}

function commitTargetMatches(
  pending: SessionNavigationTransaction,
  target: SessionNavigationCommitTarget,
): boolean {
  if (pending.target.kind !== target.kind || pending.target.scope !== target.scope) {
    return false;
  }
  const actualWorkspaceId = target.workspaceId?.trim() || '';
  const actualWorkspacePath = target.workspacePath?.trim() || '';
  if (pending.target.scope === 'workspace') {
    if (
      pending.target.workspaceId !== actualWorkspaceId
      || pending.target.workspacePath !== actualWorkspacePath
    ) {
      return false;
    }
  } else if (actualWorkspaceId || actualWorkspacePath) {
    return false;
  }
  const expectedSessionId = pending.target.kind === 'session' ? pending.target.sessionId : '';
  const actualSessionId = target.sessionId?.trim() || '';
  return expectedSessionId === actualSessionId;
}

/**
 * 返回 null 表示 requestId 已不是当前导航，false 表示当前请求收到错误目标，
 * true 表示目标与请求完全一致。调用方据此区分“迟到响应”与“协议错误”。
 */
export function matchesSessionNavigationTarget(
  requestId: unknown,
  target: SessionNavigationCommitTarget,
): boolean | null {
  const normalizedRequestId = normalizeNavigationRequestId(requestId);
  const pending = sessionNavigationState.pending;
  if (!pending || !normalizedRequestId || pending.requestId !== normalizedRequestId) {
    return null;
  }
  return commitTargetMatches(pending, target);
}

function finalizeSessionNavigation(
  requestId: unknown,
  outcome: 'success' | 'failure',
  error?: unknown,
): boolean {
  const normalizedRequestId = normalizeNavigationRequestId(requestId);
  if (!normalizedRequestId) return false;
  const attempt = sessionNavigationAttempts.get(normalizedRequestId);
  const pending = sessionNavigationState.pending;
  if (!attempt) {
    // 处理热更新或异常恢复造成的“状态有 pending、事务表已丢失”情况，
    // 不能让 UI 永久锁死；没有 Promise 可完成时仅清理相同 requestId。
    if (pending?.requestId !== normalizedRequestId) return false;
    sessionNavigationState.pending = null;
    return true;
  }
  sessionNavigationAttempts.delete(normalizedRequestId);
  window.clearTimeout(attempt.timeoutId);
  if (pending?.requestId === normalizedRequestId) {
    sessionNavigationState.pending = null;
  }
  if (outcome === 'success') {
    attempt.resolve();
  } else {
    attempt.reject(navigationFailure(error));
  }
  return true;
}

export function settleSessionNavigation(
  requestId: unknown,
  target: SessionNavigationCommitTarget,
): boolean {
  const match = matchesSessionNavigationTarget(requestId, target);
  if (match === null) return false;
  if (!match) {
    failSessionNavigation(requestId, new Error('会话导航响应与请求目标不一致'));
    return false;
  }
  return finalizeSessionNavigation(requestId, 'success');
}

export function failSessionNavigation(requestId: unknown, error?: unknown): boolean {
  return finalizeSessionNavigation(requestId, 'failure', error);
}

export async function waitForSessionNavigation(
  transaction: SessionNavigationTransaction,
  timeoutMillis = 10_000,
): Promise<void> {
  let timeoutId: number | null = null;
  const timeout = new Promise<never>((_, reject) => {
    timeoutId = window.setTimeout(() => {
      const error = new Error('会话导航超时');
      failSessionNavigation(transaction.requestId, error);
      reject(error);
    }, Math.max(0, timeoutMillis));
  });
  try {
    await Promise.race([transaction.completion, timeout]);
  } finally {
    if (timeoutId !== null) {
      window.clearTimeout(timeoutId);
    }
  }
}
