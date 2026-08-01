export interface SessionBootstrapSnapshot {
  workspace?: {
    workspaceId?: string;
    rootPath?: string;
  };
  sessionId: string;
  sessions: unknown[];
  state: unknown;
  notifications?: {
    workspaceId: string;
    sessionId?: string;
    notifications: unknown;
  };
  orchestratorRuntimeState?: unknown;
  hasMoreBefore?: boolean;
  beforeCursor?: string | null;
  canonicalHasMoreBefore?: boolean;
  canonicalBeforeCursor?: string | null;
}
