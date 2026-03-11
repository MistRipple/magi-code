import type { AgentType, Message } from '../types/message';

export interface WorkerPanelState {
  latestRoundAnchorMessage: Message | null;
  latestInstructionMessage: Message | null;
  latestRoundRequestId: string | null;
  panelHasPendingRequest: boolean;
  hasBottomStreamingMessage: boolean;
  workerHasCurrentRequestActivity: boolean;
}

interface DeriveWorkerPanelStateParams {
  messages: Message[];
  workerName?: AgentType;
  pendingRequestIds: Iterable<string>;
  isProcessing: boolean;
  processingActorAgent?: AgentType;
}

export function getMessageRequestId(message: Message | null | undefined): string | null {
  const requestId = message?.metadata?.requestId;
  if (typeof requestId !== 'string') return null;
  const normalized = requestId.trim();
  return normalized.length > 0 ? normalized : null;
}

export function deriveWorkerPanelState({
  messages,
  workerName,
  pendingRequestIds,
  isProcessing,
  processingActorAgent,
}: DeriveWorkerPanelStateParams): WorkerPanelState {
  const safeMessages = (messages || []).filter((message): message is Message => Boolean(message?.id));
  let latestRoundAnchorMessage: Message | null = null;
  let latestInstructionMessage: Message | null = null;

  for (let idx = safeMessages.length - 1; idx >= 0; idx -= 1) {
    const message = safeMessages[idx];
    if (!latestInstructionMessage && message.type === 'instruction') {
      latestInstructionMessage = message;
    }
    if (!latestRoundAnchorMessage && (message.type === 'instruction' || message.type === 'user_input')) {
      latestRoundAnchorMessage = message;
    }
    if (latestInstructionMessage && latestRoundAnchorMessage) {
      break;
    }
  }

  const latestRoundRequestId = getMessageRequestId(latestRoundAnchorMessage);
  const pendingRequestIdSet = pendingRequestIds instanceof Set ? pendingRequestIds : new Set(pendingRequestIds);
  const panelHasPendingRequest = latestRoundRequestId ? pendingRequestIdSet.has(latestRoundRequestId) : false;
  const lastMessage = safeMessages.length > 0 ? safeMessages[safeMessages.length - 1] : null;
  const hasBottomStreamingMessage = Boolean(lastMessage?.isStreaming);
  const workerHasCurrentRequestActivity = hasBottomStreamingMessage
    || (isProcessing && ((workerName && processingActorAgent === workerName) || panelHasPendingRequest));

  return {
    latestRoundAnchorMessage,
    latestInstructionMessage,
    latestRoundRequestId,
    panelHasPendingRequest,
    hasBottomStreamingMessage,
    workerHasCurrentRequestActivity,
  };
}