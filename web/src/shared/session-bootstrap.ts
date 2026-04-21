/**
 * Session Bootstrap 类型定义
 *
 * 前端自包含版本 — 仅保留 web-client-bridge.ts 所需的类型。
 * 移除了原版中的 buildSessionBootstrapTimelineProjection 等
 * 服务端专用逻辑和重量级依赖。
 */

export interface BootstrapQueuedMessage {
  id: string;
  content: string;
  createdAt: number;
}

/**
 * 执行链前端摘要（browser-safe，纯基础类型）
 */
export interface ExecutionChainBootstrapSummary {
  hasRecoverableChain: boolean;
  recoverableChainId?: string;
  recoverableChainTitle?: string;
  lastChainStatus?: string;
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
  /** 当前 session 的执行链摘要（停止/继续/放弃按钮状态） */
  executionChainSummary?: ExecutionChainBootstrapSummary;
}
