export interface SessionBootstrapSnapshot {
  scope?: 'personal' | 'workspace';
  agent?: {
    runtimeEpoch?: string;
  };
  eventStreamNextSequence?: number;
  canonicalEventNextSequence?: number;
  workspace?: {
    workspaceId?: string;
    rootPath?: string;
  };
  sessionId: string;
  sessions: unknown[];
  state: unknown;
  notifications?: {
    workspaceId: string | null;
    sessionId?: string;
    notifications: unknown;
  };
  orchestratorRuntimeState?: unknown;
  canonicalHasMoreBefore?: boolean;
  canonicalBeforeCursor?: string | null;
  navigationRequestId?: string;
  navigationTarget?: 'draft' | 'session';
  navigationOrchestratorSessionConfig?: Record<string, unknown>;
}
