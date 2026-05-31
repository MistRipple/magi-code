export interface EmptyWorkspaceAppStatePayload {
  sessions: [];
  isProcessing: false;
  processingState: null;
  pendingChanges: [];
  pendingChangesState: null;
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
    pendingChangesState: null,
    edits: [],
    pendingChangesStateVersion: 0,
    stateUpdatedAt: now,
    recovered: false,
  };
}
