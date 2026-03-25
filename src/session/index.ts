/**
 * Session 模块导出
 */

export {
  UnifiedSessionManager,
  type UnifiedSession,
  type SessionMessage,
  type FileSnapshotMeta,
  type SessionMeta,
  type SessionStatus,
} from './unified-session-manager';

export {
  type TimelineRecord,
  type TimelineRecordKind,
  type SessionNotificationRecord,
  type SessionRuntimeTimelineState,
  type SessionRuntimeNotificationState,
} from './timeline-record';

export {
  buildSessionTimelineProjection,
  isSessionTimelineProjection,
  type SessionTimelineProjection,
  type SessionTimelineProjectionArtifact,
  type SessionTimelineProjectionExecutionItem,
  type SessionTimelineProjectionMessage,
} from './session-timeline-projection';
