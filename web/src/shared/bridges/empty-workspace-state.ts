export interface EmptyWorkspaceAppStatePayload {
  sessions: [];
  isProcessing: false;
  processingState: null;
  pendingChanges: [];
  edits: [];
  pendingChangesStateVersion: number;
  stateUpdatedAt: number;
  recovered: false;
  currentSessionId?: string;
}

export function buildEmptyWorkspaceAppState(now: number): EmptyWorkspaceAppStatePayload {
  return {
    sessions: [],
    currentSessionId: '',
    isProcessing: false,
    processingState: null,
    pendingChanges: [],
    edits: [],
    pendingChangesStateVersion: 0,
    stateUpdatedAt: now,
    recovered: false,
  };
}
