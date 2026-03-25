import type { ContentBlock, InteractionRequest, MessageVisibility, NotifyPresentation } from '../protocol/message-protocol';
import type { AgentType, WorkerSlot } from '../types/agent-types';

export type TimelineRecordKind =
  | 'user_input'
  | 'assistant_text'
  | 'thinking'
  | 'tool_card'
  | 'worker_lifecycle'
  | 'worker_result'
  | 'progress'
  | 'system_notice';

export interface TimelineMessageLike {
  id: string;
  role: 'user' | 'assistant' | 'system';
  content: string;
  agent?: AgentType;
  source?: string;
  timestamp: number;
  updatedAt?: number;
  attachments?: { name: string; path: string; mimeType?: string }[];
  images?: Array<{ dataUrl: string }>;
  blocks?: ContentBlock[];
  type?: string;
  category?: string;
  visibility?: MessageVisibility;
  noticeType?: string;
  isStreaming?: boolean;
  isComplete?: boolean;
  interaction?: InteractionRequest;
  metadata?: Record<string, unknown>;
}

export interface TimelineRecord {
  recordId: string;
  nodeId: string;
  stableKey: string;
  messageId: string;
  kind: TimelineRecordKind;
  role: 'user' | 'assistant' | 'system';
  source?: string;
  agent?: AgentType;
  messageType?: string;
  category?: string;
  visibility?: MessageVisibility;
  requestId?: string;
  turnId?: string;
  missionId?: string;
  dispatchWaveId?: string;
  assignmentId?: string;
  laneId?: string;
  workerCardId?: string;
  worker?: WorkerSlot;
  threadVisible: boolean;
  workerViews: WorkerSlot[];
  cardId?: string;
  lifecycleKey?: string;
  anchorEventSeq: number;
  anchorTimestamp: number;
  cardStreamSeq: number;
  messageTimestamp: number;
  createdAt: number;
  updatedAt: number;
  version: number;
  content: string;
  attachments?: { name: string; path: string; mimeType?: string }[];
  images?: Array<{ dataUrl: string }>;
  blocks?: ContentBlock[];
  noticeType?: string;
  isStreaming?: boolean;
  isComplete?: boolean;
  interaction?: InteractionRequest;
  metadata?: Record<string, unknown>;
}

export interface SessionNotificationRecord {
  notificationId: string;
  kind: 'toast' | 'center' | 'incident' | 'audit';
  level: string;
  title?: string;
  message: string;
  source?: string;
  createdAt: number;
  read: boolean;
  persistToCenter: boolean;
  actionRequired: boolean;
  countUnread: boolean;
  displayMode?: NotifyPresentation['displayMode'];
  duration?: number;
}

export interface SessionRuntimeTimelineState {
  lastEventSeq: number;
  records: TimelineRecord[];
}

export interface SessionRuntimeNotificationState {
  lastUpdatedAt: number;
  records: SessionNotificationRecord[];
}
