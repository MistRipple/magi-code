import { messagesState } from '../stores/messages.svelte';
import { getClientBridge } from './bridges/bridge-runtime';
import type { ClientBridgeMessage } from './bridges/client-bridge';
import { copyOrchestratorSessionConfig } from './orchestrator-session-config';

export type SessionNavigationTarget =
  | { kind: 'draft'; workspaceId: string; workspacePath: string }
  | { kind: 'session'; workspaceId: string; workspacePath: string; sessionId: string };

export interface SessionNavigationTransaction {
  requestId: string;
  target: SessionNavigationTarget;
  startedAt: number;
}

export const sessionNavigationState = $state({
  pending: null as SessionNavigationTransaction | null,
});

function normalizeTarget(target: SessionNavigationTarget): SessionNavigationTarget | null {
  const workspaceId = target.workspaceId.trim();
  const workspacePath = target.workspacePath.trim();
  if (!workspaceId || !workspacePath) return null;
  if (target.kind === 'draft') return { kind: 'draft', workspaceId, workspacePath };
  const sessionId = target.sessionId.trim();
  return sessionId ? { kind: 'session', workspaceId, workspacePath, sessionId } : null;
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
      workspaceId: target.workspaceId,
      workspacePath: target.workspacePath,
      orchestratorSessionConfig: inheritedDraftConfig(),
    };
  }
  return {
    type: 'navigateSession',
    requestId,
    target: 'session',
    workspaceId: target.workspaceId,
    workspacePath: target.workspacePath,
    sessionId: target.sessionId,
  };
}

export function navigateSession(target: SessionNavigationTarget): SessionNavigationTransaction | null {
  if (sessionNavigationState.pending) return null;
  const normalized = normalizeTarget(target);
  if (!normalized) return null;
  const transaction: SessionNavigationTransaction = {
    requestId: `session-navigation-${Date.now()}-${Math.random().toString(36).slice(2, 10)}`,
    target: normalized,
    startedAt: Date.now(),
  };
  sessionNavigationState.pending = transaction;
  getClientBridge().postMessage(navigationMessage(transaction));
  return transaction;
}

export function settleSessionNavigation(
  requestId: unknown,
  target: Pick<SessionNavigationTarget, 'workspaceId'> & { kind: SessionNavigationTarget['kind']; sessionId?: string },
): boolean {
  const pending = sessionNavigationState.pending;
  if (!pending || typeof requestId !== 'string' || pending.requestId !== requestId.trim()) return false;
  const pendingSessionId = pending.target.kind === 'session' ? pending.target.sessionId : '';
  const targetSessionId = typeof target.sessionId === 'string' ? target.sessionId.trim() : '';
  if (
    pending.target.kind !== target.kind
    || pending.target.workspaceId !== target.workspaceId.trim()
    || pendingSessionId !== targetSessionId
  ) return false;
  sessionNavigationState.pending = null;
  return true;
}

export function failSessionNavigation(requestId: unknown): boolean {
  const pending = sessionNavigationState.pending;
  if (!pending || typeof requestId !== 'string' || pending.requestId !== requestId.trim()) return false;
  sessionNavigationState.pending = null;
  return true;
}
