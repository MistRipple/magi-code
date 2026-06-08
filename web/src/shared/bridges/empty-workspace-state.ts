export interface EmptyWorkspaceAppStatePayload {
  sessions: [];
  isProcessing: false;
  processingState: null;
  pendingChanges: [];
  pendingChangesState: null;
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
    pendingChangesState: null,
    pendingChangesStateVersion: 0,
    stateUpdatedAt: now,
    recovered: false,
  };
}
