export interface SessionBootstrapSnapshot {
  workspace?: {
    workspaceId?: string;
    rootPath?: string;
  };
  sessionId: string;
  sessions: unknown[];
  state: unknown;
  notifications?: {
    sessionId: string;
    notifications: unknown;
  };
  orchestratorRuntimeState?: unknown;
}
