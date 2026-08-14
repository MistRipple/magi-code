export interface SessionBootstrapSnapshot {
  scope?: 'personal' | 'workspace';
  agent?: {
    runtimeEpoch?: string;
  };
  eventStreamNextSequence?: number;
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
  hasMoreBefore?: boolean;
  beforeCursor?: string | null;
  canonicalHasMoreBefore?: boolean;
  canonicalBeforeCursor?: string | null;
}
