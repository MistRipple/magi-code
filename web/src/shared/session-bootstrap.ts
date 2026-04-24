/**
 * Session Bootstrap 类型定义
 *
 * 前端自包含版本 — 仅保留 web-client-bridge.ts 所需的类型。
 * 移除了原版中的 buildSessionBootstrapTimelineProjection 等
 * 服务端专用逻辑和重量级依赖。
 */

export interface BootstrapQueuedMessage {
  id: string;
  requestId?: string;
  content: string;
  text?: string | null;
  createdAt: number;
  mode?: 'queue' | 'guide';
  deepTask?: boolean;
  skillName?: string | null;
  images?: Array<{
    name: string;
    dataUrl: string;
  }>;
}

/**
 * Session Timeline Projection 的前端表示
 * 原版定义在 session/session-timeline-projection.ts，
 * 此处用 unknown 保持类型安全，不引入 session 子系统重量级依赖。
 */
export interface SessionTimelineProjection {
  schemaVersion: 'session-timeline-projection.v2';
  sessionId: string;
  updatedAt: number;
  lastAppliedEventSeq: number;
  artifacts: unknown[];
  threadRenderEntries: unknown[];
  workerRenderEntries: Record<string, unknown[]>;
}

export interface SessionBootstrapSnapshot {
  sessionId: string;
  sessions: unknown[];
  state: unknown;
  timelineProjection: SessionTimelineProjection;
  notifications?: {
    sessionId: string;
    notifications: unknown;
  };
  queuedMessages: BootstrapQueuedMessage[];
  orchestratorRuntimeState?: unknown;
}
