export type TurnStage = 'pending' | 'preparing' | 'streaming' | 'finalizing' | 'done';

export interface ProcessingStateSnapshot {
  isProcessing: boolean;
  source: string | null;
  agent: string | null;
  startedAt: number | null;
  pendingRequestIds: string[];
  stage?: TurnStage | null;
}
