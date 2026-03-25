/**
 * 统一会话管理器
 * 将所有会话相关数据按会话ID组织存储
 * 
 * 目录结构：
 * .magi/sessions/{sessionId}/
 * ├── session.json          # 会话唯一权威快照
 * ├── plans/                # 计划文件
 * ├── tasks.json            # 子任务状态
 * ├── snapshots/            # 快照文件
 * └── execution-state.json  # 执行状态
 */

import { logger, LogCategory } from '../logging';
import * as fs from 'fs';
import * as path from 'path';
import { FileSnapshot } from '../types';
import { AgentType, WorkerSlot } from '../types/agent-types';
import type {
  ContentBlock as StandardContentBlock,
  InteractionRequest,
  MessageCategory,
  MessageVisibility,
  StandardMessage,
} from '../protocol/message-protocol';
import { globalEventBus } from '../events';
import { estimateTokenCount } from '../utils/token-estimator';
import { atomicWriteFile } from '../utils/atomic-write';
import { CoalescedAsyncTaskQueue } from '../utils/async-task-queue';
import {
  buildSessionTimelineProjection,
  isSessionTimelineProjection,
  type SessionTimelineProjection,
  type SessionTimelineProjectionMessage,
} from './session-timeline-projection';
import type {
  SessionRuntimeNotificationState,
  SessionRuntimeTimelineState,
  TimelineRecord,
} from './timeline-record';
import {
  buildNotificationRecordFromStandardMessage,
  resolveSessionPersistenceTarget,
  buildTimelineRecordsFromMessageLike,
  mergeTimelineRecord,
} from './timeline-classifier';
import {
  materializeProjectionSourceMessagesFromTimelineRecords,
  materializeSessionMessagesFromTimelineRecords,
  sortTimelineRecordsBySemanticOrder,
} from './timeline-record-adapter';
import {
  buildPersistedStandardMessagePayload,
  sanitizePersistedMessageMetadata,
} from './standard-message-session-persistence';

/** 会话消息 */
export interface SessionMessage {
  id: string;
  role: 'user' | 'assistant' | 'system';
  content: string;
  agent?: AgentType;
  source?: 'orchestrator' | 'worker' | 'system' | WorkerSlot;
  timestamp: number;
  updatedAt?: number;
  attachments?: { name: string; path: string; mimeType?: string }[];
  /** 用户上传的图片（base64 Data URL 格式） */
  images?: Array<{ dataUrl: string }>;
  /** 结构化内容块（tool_call/file_change/thinking 等） */
  blocks?: StandardContentBlock[];
  /** UI 消息类型（text/tool_call/...） */
  type?: string;
  /** 协议层消息类别 */
  category?: MessageCategory;
  /** 用户可见性 */
  visibility?: MessageVisibility;
  /** 系统通知级别 */
  noticeType?: string;
  /** 消息流式状态（恢复时会在前端归一为历史完成态） */
  isStreaming?: boolean;
  isComplete?: boolean;
  interaction?: InteractionRequest;
  /** 扩展元数据（cardId/eventSeq/standardized 等） */
  metadata?: Record<string, unknown>;
}

type SessionMessageSource = 'orchestrator' | 'worker' | 'system' | WorkerSlot;

/** 文件快照元数据 */
export interface FileSnapshotMeta {
  id: string;
  filePath: string;
  timestamp: number;

  // Mission 架构字段
  missionId: string;
  assignmentId: string;
  todoId: string;
  workerId: string;  // Worker 标识（claude/codex/gemini）
  contributors?: string[];

  agentType?: AgentType;
  reason?: string;
}

/** 任务状态 */
export type SessionStatus = 'active' | 'completed';

/** 会话总结（用于会话恢复） */
export interface SessionSummary {
  sessionId: string;
  title: string;
  objective: string;              // 会话目标/主题
  completedTasks: string[];       // 已完成任务摘要
  inProgressTasks: string[];      // 进行中任务摘要
  keyDecisions: string[];         // 关键决策
  codeChanges: string[];          // 代码变更摘要
  pendingIssues: string[];        // 待解决问题
  messageCount: number;           // 消息数量
  lastUpdated: number;            // 最后更新时间
}

/** 统一会话数据结构
 *
 * 注意：任务管理已迁移到 Mission 系统
 * 使用 MissionDrivenEngine.listTaskViews() 获取任务列表
 */
export interface UnifiedSession {
  schemaVersion?: 'session-runtime.v2';
  id: string;
  name?: string;
  status: SessionStatus;
  createdAt: number;
  updatedAt: number;
  /** 由 timeline.records 派生的消息缓存 */
  messages: SessionMessage[];
  /** 会话唯一持久化语义快照 */
  timeline: SessionRuntimeTimelineState;
  /** 会话级通知快照 */
  notifications: SessionRuntimeNotificationState;
  /** 快照元数据 */
  snapshots: FileSnapshotMeta[];
  /** 主线与 Worker 面板统一恢复投影 */
  timelineProjection: SessionTimelineProjection;
  /** 执行链持久化数据（由 MDE 注入，session 层透传） */
  executionChains?: unknown;
  /** 恢复快照持久化数据（由 MDE 注入，session 层透传） */
  resumeSnapshots?: unknown;
}

interface PersistedUnifiedSessionRecord {
  schemaVersion: 'session-runtime.v2';
  id: string;
  name?: string;
  status: SessionStatus;
  createdAt: number;
  updatedAt: number;
  timeline: SessionRuntimeTimelineState;
  notifications: SessionRuntimeNotificationState;
  snapshots: FileSnapshotMeta[];
  /** 执行链持久化数据（由 MDE 写入，session 层透传） */
  executionChains?: unknown;
  /** 恢复快照持久化数据（由 MDE 写入，session 层透传） */
  resumeSnapshots?: unknown;
}

/** 会话元数据（用于列表显示） */
export interface SessionMeta {
  id: string;
  name?: string;
  messageCount: number;
  createdAt: number;
  updatedAt: number;
  preview: string;
}

interface SessionMessageAppendOptions {
  id?: string;
  type?: string;
  category?: MessageCategory;
  visibility?: MessageVisibility;
  updatedAt?: number;
  interaction?: InteractionRequest;
  metadata?: Record<string, unknown>;
  /** 结构化内容块（tool_call/file_change/thinking 等），用于会话恢复时还原工具卡片 */
  blocks?: StandardContentBlock[];
}

interface SessionMessageUpsertOptions extends SessionMessageAppendOptions {
  timestamp?: number;
}

/** 生成唯一 ID */
function generateId(): string {
  return `session-${Date.now()}-${Math.random().toString(36).substring(2, 9)}`;
}

/** 生成消息 ID */
function generateMessageId(): string {
  return `msg-${Date.now()}-${Math.random().toString(36).substring(2, 6)}`;
}

function createEmptyTimelineState(): SessionRuntimeTimelineState {
  return {
    lastEventSeq: 0,
    records: [],
  };
}

function createEmptyNotificationState(updatedAt: number): SessionRuntimeNotificationState {
  return {
    lastUpdatedAt: updatedAt,
    records: [],
  };
}

function isPersistedSessionRecord(value: unknown): value is PersistedUnifiedSessionRecord {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    return false;
  }
  const record = value as Record<string, unknown>;
  if (record.schemaVersion !== 'session-runtime.v2') {
    return false;
  }
  if (typeof record.id !== 'string' || !record.id.trim()) {
    return false;
  }
  if (record.status !== 'active' && record.status !== 'completed') {
    return false;
  }
  if (typeof record.createdAt !== 'number' || !Number.isFinite(record.createdAt)) {
    return false;
  }
  if (typeof record.updatedAt !== 'number' || !Number.isFinite(record.updatedAt)) {
    return false;
  }
  if (!Array.isArray(record.snapshots)) {
    return false;
  }
  if (!record.timeline || typeof record.timeline !== 'object' || Array.isArray(record.timeline)) {
    return false;
  }
  if (!Array.isArray((record.timeline as { records?: unknown[] }).records)) {
    return false;
  }
  if (!record.notifications || typeof record.notifications !== 'object' || Array.isArray(record.notifications)) {
    return false;
  }
  if (!Array.isArray((record.notifications as { records?: unknown[] }).records)) {
    return false;
  }
  return true;
}

/**
 * 统一会话管理器
 */
export class UnifiedSessionManager {
  private sessions: Map<string, UnifiedSession> = new Map();
  private sessionMetas: Map<string, SessionMeta> = new Map();
  private currentSessionId: string | null = null;
  private workspaceRoot: string;
  private baseDir: string;
  private readonly sessionPersistQueue = new CoalescedAsyncTaskQueue((sessionId, error) => {
    logger.error('会话.保存.异步落盘失败', { sessionId, error }, LogCategory.SESSION);
  });

  /** 保存前回调：允许外部模块（如 MDE）在持久化前注入数据到 session */
  private beforeSaveHook: ((session: UnifiedSession) => void) | null = null;
  /** 加载后回调：允许外部模块（如 MDE）在恢复时提取数据 */
  private afterLoadHook: ((session: UnifiedSession) => void) | null = null;

  // 内存管理配置
  private readonly MAX_SESSIONS_IN_MEMORY = 50;  // 最大内存中会话数
  private readonly MAX_MESSAGES_PER_SESSION = 1000;  // 每个会话最大消息数
  private readonly MESSAGE_CLEANUP_THRESHOLD = 800;  // 消息清理阈值

  constructor(workspaceRoot: string) {
    this.workspaceRoot = workspaceRoot;
    this.baseDir = path.join(workspaceRoot, '.magi', 'sessions');
    this.ensureBaseDir();
    this.loadAllSessions();
  }

  /** 确保基础目录存在 */
  private ensureBaseDir(): void {
    if (!fs.existsSync(this.baseDir)) {
      fs.mkdirSync(this.baseDir, { recursive: true });
    }
  }

  /**
   * 注册保存前回调
   *
   * 在每次 saveSession 执行序列化之前调用，允许外部模块（如 MDE）
   * 将执行链数据注入到 session 对象上。
   */
  setBeforeSaveHook(hook: (session: UnifiedSession) => void): void {
    this.beforeSaveHook = hook;
  }

  /**
   * 注册加载后回调
   *
   * 在 session 从磁盘恢复并 hydrate 后调用，允许外部模块（如 MDE）
   * 从 session 对象中提取执行链数据并恢复到内存态存储。
   */
  setAfterLoadHook(hook: (session: UnifiedSession) => void): void {
    this.afterLoadHook = hook;
  }

  /** 获取会话目录路径 */
  getSessionDir(sessionId: string): string {
    return path.join(this.baseDir, sessionId);
  }

  /** 确保会话目录结构存在 */
  private ensureSessionDir(sessionId: string): void {
    const sessionDir = this.getSessionDir(sessionId);
    const dirs = [
      sessionDir,
      path.join(sessionDir, 'plans'),
      path.join(sessionDir, 'snapshots'),
    ];
    for (const dir of dirs) {
      if (!fs.existsSync(dir)) {
        fs.mkdirSync(dir, { recursive: true });
      }
    }
  }

  /** 获取会话文件路径 */
  private getSessionFilePath(sessionId: string): string {
    return path.join(this.getSessionDir(sessionId), 'session.json');
  }

  /** 创建新会话 */
  createSession(name?: string, sessionId?: string): UnifiedSession {
    if (sessionId) {
      const existing = this.getSession(sessionId);
      if (existing) {
        this.currentSessionId = sessionId;
        return existing;
      }
    }

    const now = Date.now();
    const id = sessionId ?? generateId();

    const session: UnifiedSession = {
      schemaVersion: 'session-runtime.v2',
      id,
      name: name || undefined,
      status: 'active',
      createdAt: now,
      updatedAt: now,
      messages: [],
      timeline: createEmptyTimelineState(),
      notifications: createEmptyNotificationState(now),
      snapshots: [],
      timelineProjection: buildSessionTimelineProjection({
        id,
        updatedAt: now,
        messages: [],
      }),
    };

    this.ensureSessionDir(id);

    // 内存管理：如果会话数超过限制，驱逐最早的非当前会话
    this.evictOldSessionsIfNeeded();

    this.sessions.set(id, session);
    this.currentSessionId = id;
    this.saveSession(session);
    this.refreshSessionMeta(session);

    globalEventBus.emitEvent('session:created', { sessionId: id });
    return session;
  }

  /** 获取当前会话 */
  getCurrentSession(): UnifiedSession | null {
    if (!this.currentSessionId) return null;
    return this.getSession(this.currentSessionId);
  }

  /** 获取或创建当前会话 */
  getOrCreateCurrentSession(): UnifiedSession {
    const current = this.getCurrentSession();
    if (current) return current;
    return this.createSession();
  }

  /** 切换会话 */
  switchSession(sessionId: string): UnifiedSession | null {
    const session = this.getSession(sessionId);
    if (session) {
      this.currentSessionId = sessionId;
      return session;
    }
    return null;
  }

  /** 获取会话 */
  getSession(sessionId: string): UnifiedSession | null {
    const existing = this.sessions.get(sessionId);
    if (existing) return existing;
    if (this.sessionMetas.has(sessionId)) {
      return this.loadSession(sessionId);
    }
    return null;
  }

  /** 获取所有会话（按更新时间倒序） */
  getAllSessions(): UnifiedSession[] {
    return Array.from(this.sessions.values())
      .sort((a, b) => b.updatedAt - a.updatedAt);
  }

  getSessionTimelineProjection(sessionId: string): SessionTimelineProjection | null {
    return this.getSession(sessionId)?.timelineProjection ?? null;
  }

  private ensureSessionRuntimeState(session: UnifiedSession): void {
    if (!session.timeline || typeof session.timeline !== 'object') {
      session.timeline = createEmptyTimelineState();
    }
    if (!Array.isArray(session.timeline.records)) {
      session.timeline.records = [];
    }
    if (typeof session.timeline.lastEventSeq !== 'number' || !Number.isFinite(session.timeline.lastEventSeq)) {
      session.timeline.lastEventSeq = 0;
    }
    if (!session.notifications || typeof session.notifications !== 'object') {
      session.notifications = createEmptyNotificationState(session.updatedAt);
    }
    if (!Array.isArray(session.notifications.records)) {
      session.notifications.records = [];
    }
    if (typeof session.notifications.lastUpdatedAt !== 'number' || !Number.isFinite(session.notifications.lastUpdatedAt)) {
      session.notifications.lastUpdatedAt = session.updatedAt;
    }
  }

  private normalizeTimelineRecordRole(record: TimelineRecord): TimelineRecord {
    if (record.messageType === 'user_input' && record.role !== 'user') {
      return { ...record, role: 'user' };
    }
    if (record.messageType === 'system-notice' && record.role !== 'system') {
      return { ...record, role: 'system' };
    }
    return record;
  }

  private normalizeTimelineSnapshot(session: UnifiedSession): boolean {
    this.ensureSessionRuntimeState(session);
    const currentRecords = Array.isArray(session.timeline.records) ? session.timeline.records : [];
    const normalizedByStableKey = new Map<string, TimelineRecord>();
    const orderedStableKeys: string[] = [];
    for (const record of currentRecords.map((item) => this.normalizeTimelineRecordRole(item))) {
      const stableKey = typeof record.stableKey === 'string' ? record.stableKey.trim() : '';
      if (!stableKey) {
        continue;
      }
      const existing = normalizedByStableKey.get(stableKey);
      if (!existing) {
        orderedStableKeys.push(stableKey);
        normalizedByStableKey.set(stableKey, record);
        continue;
      }
      normalizedByStableKey.set(stableKey, mergeTimelineRecord(existing, record));
    }
    const normalizedRecords = sortTimelineRecordsBySemanticOrder(
      orderedStableKeys
        .map((stableKey) => normalizedByStableKey.get(stableKey))
        .filter((record): record is TimelineRecord => Boolean(record)),
    );
    const nextLastEventSeq = normalizedRecords.reduce(
      (maxSeq, record) => Math.max(maxSeq, record.anchorEventSeq),
      0,
    );
    const timelineChanged = !this.areTimelineRecordsEquivalent(currentRecords, normalizedRecords)
      || session.timeline.lastEventSeq !== nextLastEventSeq;
    session.timeline = {
      lastEventSeq: nextLastEventSeq,
      records: normalizedRecords,
    };
    return timelineChanged;
  }

  private syncSessionMessagesFromTimeline(session: UnifiedSession): boolean {
    this.ensureSessionRuntimeState(session);
    const currentMessages = Array.isArray(session.messages) ? session.messages : [];
    const nextMessages = this.deduplicateSessionMessages(
      materializeSessionMessagesFromTimelineRecords(session.timeline.records),
    );
    const messagesChanged = !this.areSessionMessagesEquivalent(currentMessages, nextMessages);
    session.messages = nextMessages;
    return messagesChanged;
  }

  private reconcileSessionMessagesAndTimeline(session: UnifiedSession): {
    messagesChanged: boolean;
    timelineChanged: boolean;
  } {
    this.ensureSessionRuntimeState(session);
    const timelineChanged = this.normalizeTimelineSnapshot(session);
    const messagesChanged = this.syncSessionMessagesFromTimeline(session);
    return {
      messagesChanged,
      timelineChanged,
    };
  }

  private resolveProjectionSourceMessages(session: UnifiedSession): SessionTimelineProjectionMessage[] {
    this.ensureSessionRuntimeState(session);
    return materializeProjectionSourceMessagesFromTimelineRecords(session.timeline.records);
  }

  private resolveMessageRevisionTimestamp(message: SessionMessage): number {
    if (typeof message.updatedAt === 'number' && Number.isFinite(message.updatedAt)) {
      return Math.floor(message.updatedAt);
    }
    if (typeof message.timestamp === 'number' && Number.isFinite(message.timestamp)) {
      return Math.floor(message.timestamp);
    }
    return 0;
  }

  private resolveMessageCompletenessScore(message: SessionMessage): number {
    let score = 0;
    const content = typeof message.content === 'string' ? message.content.trim() : '';
    if (content) {
      score += content.length;
    }
    if (Array.isArray(message.blocks) && message.blocks.length > 0) {
      score += message.blocks.length * 100;
    }
    if (Array.isArray(message.attachments) && message.attachments.length > 0) {
      score += message.attachments.length * 10;
    }
    if (Array.isArray(message.images) && message.images.length > 0) {
      score += message.images.length * 10;
    }
    if (message.metadata && typeof message.metadata === 'object' && !Array.isArray(message.metadata)) {
      score += Object.keys(message.metadata).length * 5;
    }
    return score;
  }

  private shouldPreferIncomingSessionMessage(existing: SessionMessage, incoming: SessionMessage): boolean {
    const existingRevision = this.resolveMessageRevisionTimestamp(existing);
    const incomingRevision = this.resolveMessageRevisionTimestamp(incoming);
    if (incomingRevision !== existingRevision) {
      return incomingRevision > existingRevision;
    }

    const existingScore = this.resolveMessageCompletenessScore(existing);
    const incomingScore = this.resolveMessageCompletenessScore(incoming);
    if (incomingScore !== existingScore) {
      return incomingScore > existingScore;
    }

    return incoming.timestamp >= existing.timestamp;
  }

  private mergeDuplicateSessionMessage(existing: SessionMessage, incoming: SessionMessage): SessionMessage {
    const existingMetadata = existing.metadata && typeof existing.metadata === 'object' && !Array.isArray(existing.metadata)
      ? existing.metadata
      : undefined;
    const incomingMetadata = incoming.metadata && typeof incoming.metadata === 'object' && !Array.isArray(incoming.metadata)
      ? incoming.metadata
      : undefined;
    const preferIncoming = this.shouldPreferIncomingSessionMessage(existing, incoming);
    const primary = preferIncoming ? incoming : existing;
    const secondary = preferIncoming ? existing : incoming;
    const primaryMetadata = preferIncoming ? incomingMetadata : existingMetadata;
    const secondaryMetadata = preferIncoming ? existingMetadata : incomingMetadata;
    const merged = this.normalizeMessageRole({
      ...secondary,
      ...primary,
      id: existing.id,
      role: primary.role || secondary.role,
      content: primary.content?.trim() ? primary.content : secondary.content,
      timestamp: Math.min(existing.timestamp, incoming.timestamp),
      updatedAt: Math.max(
        this.resolveMessageRevisionTimestamp(existing),
        this.resolveMessageRevisionTimestamp(incoming),
      ),
      attachments: Array.isArray(primary.attachments) && primary.attachments.length > 0
        ? primary.attachments
        : secondary.attachments,
      images: Array.isArray(primary.images) && primary.images.length > 0
        ? primary.images
        : secondary.images,
      blocks: Array.isArray(primary.blocks) && primary.blocks.length > 0
        ? primary.blocks
        : secondary.blocks,
      metadata: sanitizePersistedMessageMetadata({
        role: primary.role || secondary.role,
        type: primary.type || secondary.type,
        content: primary.content?.trim() ? primary.content : secondary.content,
        blocks: Array.isArray(primary.blocks) && primary.blocks.length > 0
          ? primary.blocks
          : secondary.blocks,
        metadata: {
          ...(secondaryMetadata || {}),
          ...(primaryMetadata || {}),
        },
      }),
    });
    return merged;
  }

  private deduplicateSessionMessages(messages: SessionMessage[]): SessionMessage[] {
    const orderedIds: string[] = [];
    const dedupedById = new Map<string, SessionMessage>();

    for (const message of messages) {
      const messageId = typeof message?.id === 'string' ? message.id.trim() : '';
      if (!messageId) {
        continue;
      }
      const normalizedMessage = this.normalizeMessageRole({
        ...message,
        id: messageId,
      });
      const sanitizedMessage = this.sanitizeSessionMessageMetadata(normalizedMessage);
      const existing = dedupedById.get(messageId);
      if (!existing) {
        orderedIds.push(messageId);
        dedupedById.set(messageId, sanitizedMessage);
        continue;
      }
      dedupedById.set(messageId, this.mergeDuplicateSessionMessage(existing, sanitizedMessage));
    }

    return orderedIds.map((messageId) => dedupedById.get(messageId)!);
  }

  private upsertTimelineRecordInSession(session: UnifiedSession, record: TimelineRecord): void {
    this.ensureSessionRuntimeState(session);
    const existingIndex = session.timeline.records.findIndex((item) => item.stableKey === record.stableKey);
    if (existingIndex < 0) {
      session.timeline.records.push(record);
    } else {
      session.timeline.records[existingIndex] = mergeTimelineRecord(session.timeline.records[existingIndex], record);
    }
    session.timeline.lastEventSeq = Math.max(session.timeline.lastEventSeq, record.anchorEventSeq);
  }

  private resolveTimelineSourceMessageId(record: TimelineRecord): string {
    const metadata = record.metadata && typeof record.metadata === 'object' && !Array.isArray(record.metadata)
      ? record.metadata
      : undefined;
    const originMessageId = typeof metadata?.originMessageId === 'string' ? metadata.originMessageId.trim() : '';
    return originMessageId || record.messageId;
  }

  private syncTimelineSnapshotFromMessage(
    session: UnifiedSession,
    message: SessionMessage,
  ): void {
    const records = buildTimelineRecordsFromMessageLike(message);
    const sourceMessageId = typeof message.id === 'string' ? message.id.trim() : '';
    const nextStableKeys = new Set(records.map((record) => record.stableKey));
    if (sourceMessageId) {
      const existingSourceRecords = session.timeline.records.filter((record) => (
        this.resolveTimelineSourceMessageId(record) === sourceMessageId
      ));
      session.timeline.records = session.timeline.records.filter((record) => (
        this.resolveTimelineSourceMessageId(record) !== sourceMessageId
        || nextStableKeys.has(record.stableKey)
      ));
      session.timeline.lastEventSeq = session.timeline.records.reduce(
        (maxSeq, record) => Math.max(maxSeq, record.anchorEventSeq),
        0,
      );
    }
    if (records.length === 0) {
      return;
    }
    for (const record of records) {
      this.upsertTimelineRecordInSession(session, record);
    }
  }

  private rebuildTimelineSnapshotFromMessages(messages: SessionMessage[]): SessionRuntimeTimelineState {
    const state = createEmptyTimelineState();
    for (const message of messages) {
      for (const record of buildTimelineRecordsFromMessageLike(message)) {
        const existingIndex = state.records.findIndex((item) => item.stableKey === record.stableKey);
        if (existingIndex < 0) {
          state.records.push(record);
        } else {
          state.records[existingIndex] = mergeTimelineRecord(state.records[existingIndex], record);
        }
        state.lastEventSeq = Math.max(state.lastEventSeq, record.anchorEventSeq);
      }
    }
    state.records.sort((left, right) => (
      (left.anchorEventSeq - right.anchorEventSeq)
      || (left.anchorTimestamp - right.anchorTimestamp)
      || left.stableKey.localeCompare(right.stableKey)
    ));
    return state;
  }

  appendNotificationToSession(sessionId: string, record: SessionRuntimeNotificationState['records'][number]): void {
    const session = this.getSession(sessionId);
    if (!session) {
      throw new Error(`Session not found: ${sessionId}`);
    }
    this.ensureSessionRuntimeState(session);
    const existingIndex = session.notifications.records.findIndex((item) => item.notificationId === record.notificationId);
    if (existingIndex < 0) {
      session.notifications.records.push(record);
    } else {
      session.notifications.records[existingIndex] = {
        ...session.notifications.records[existingIndex],
        ...record,
      };
    }
    session.notifications.lastUpdatedAt = Math.max(session.notifications.lastUpdatedAt, record.createdAt);
    session.updatedAt = Date.now();
    this.saveSession(session);
  }

  getSessionNotifications(sessionId: string): SessionRuntimeNotificationState | null {
    const session = this.getSession(sessionId);
    if (!session) {
      return null;
    }
    this.ensureSessionRuntimeState(session);
    return structuredClone(session.notifications);
  }

  markAllSessionNotificationsRead(sessionId: string): SessionRuntimeNotificationState | null {
    const session = this.getSession(sessionId);
    if (!session) {
      return null;
    }
    this.ensureSessionRuntimeState(session);
    const updatedAt = Date.now();
    session.notifications.records = session.notifications.records.map((record) => ({
      ...record,
      read: true,
    }));
    session.notifications.lastUpdatedAt = updatedAt;
    session.updatedAt = updatedAt;
    this.saveSession(session);
    return structuredClone(session.notifications);
  }

  clearSessionNotifications(sessionId: string): SessionRuntimeNotificationState | null {
    const session = this.getSession(sessionId);
    if (!session) {
      return null;
    }
    this.ensureSessionRuntimeState(session);
    const updatedAt = Date.now();
    session.notifications.records = [];
    session.notifications.lastUpdatedAt = updatedAt;
    session.updatedAt = updatedAt;
    this.saveSession(session);
    return structuredClone(session.notifications);
  }

  removeSessionNotification(sessionId: string, notificationId: string): SessionRuntimeNotificationState | null {
    const session = this.getSession(sessionId);
    if (!session) {
      return null;
    }
    this.ensureSessionRuntimeState(session);
    const updatedAt = Date.now();
    session.notifications.records = session.notifications.records.filter((record) => record.notificationId !== notificationId);
    session.notifications.lastUpdatedAt = updatedAt;
    session.updatedAt = updatedAt;
    this.saveSession(session);
    return structuredClone(session.notifications);
  }

  persistStandardMessageToSession(sessionId: string, message: StandardMessage): void {
    const target = resolveSessionPersistenceTarget(message);
    if (target === 'ignore') {
      return;
    }
    if (target === 'notification') {
      const notification = buildNotificationRecordFromStandardMessage(message);
      if (notification) {
        this.appendNotificationToSession(sessionId, notification);
      }
      return;
    }
    const persistedPayload = buildPersistedStandardMessagePayload(message);
    this.upsertMessageToSession(
      sessionId,
      persistedPayload.role,
      persistedPayload.content,
      persistedPayload.agent,
      persistedPayload.source,
      undefined,
      {
        id: message.id,
        type: persistedPayload.type,
        category: persistedPayload.category,
        visibility: persistedPayload.visibility,
        timestamp: persistedPayload.timestamp,
        updatedAt: persistedPayload.updatedAt,
        interaction: persistedPayload.interaction,
        metadata: persistedPayload.metadata,
        blocks: persistedPayload.blocks,
      },
    );
  }

  private buildSessionMeta(session: UnifiedSession): SessionMeta {
    return {
      id: session.id,
      name: session.name,
      messageCount: session.messages.filter(m => this.isUserMessage(m)).length,
      createdAt: session.createdAt,
      updatedAt: session.updatedAt,
      preview: this.getSessionPreview(session),
    };
  }

  private refreshSessionMeta(session: UnifiedSession): void {
    this.sessionMetas.set(session.id, this.buildSessionMeta(session));
  }

  /** 统一 role/type 语义：type=user_input 必须是 user，type=system-notice 必须是 system */
  private normalizeMessageRole(message: SessionMessage): SessionMessage {
    if (message.type === 'user_input' && message.role !== 'user') {
      return { ...message, role: 'user' };
    }
    if (message.type === 'system-notice' && message.role !== 'system') {
      return { ...message, role: 'system' };
    }
    return message;
  }

  private sanitizeSessionMessageMetadata(message: SessionMessage): SessionMessage {
    return {
      ...message,
      metadata: sanitizePersistedMessageMetadata({
        role: message.role,
        type: message.type,
        content: message.content,
        blocks: Array.isArray(message.blocks) ? message.blocks : undefined,
        metadata: message.metadata,
      }),
    };
  }

  /** 判断是否为用户消息（统一以语义为准：role 或 type） */
  private isUserMessage(message: SessionMessage): boolean {
    return message.role === 'user' || message.type === 'user_input';
  }

  /** 获取会话元数据列表 */
  getSessionMetas(): SessionMeta[] {
    return Array.from(this.sessionMetas.values())
      .sort((a, b) => b.updatedAt - a.updatedAt);
  }

  /** 获取会话预览 */
  private getSessionPreview(session: UnifiedSession): string {
    const firstUserMsg = session.messages.find(m => this.isUserMessage(m));
    if (!firstUserMsg) return '新对话';
    const content = firstUserMsg.content.trim();
    return content.length > 50 ? content.substring(0, 50) + '...' : content;
  }

  /** 获取当前会话 ID */
  getCurrentSessionId(): string | null {
    return this.currentSessionId;
  }

  // ============================================================================
  // 消息管理
  // ============================================================================

  /** 添加消息到当前会话 */
  addMessage(
    role: 'user' | 'assistant' | 'system',
    content: string,
    agent?: AgentType,  // ✅ 使用 AgentType
    source?: SessionMessageSource,
    images?: Array<{ dataUrl: string }>  // 🔧 新增：用户上传的图片
  ): SessionMessage {
    const session = this.getOrCreateCurrentSession();
    return this.appendMessageToSession(session, role, content, agent, source, images);
  }

  /** 添加消息到指定会话（强一致会话写入，避免跨会话污染） */
  addMessageToSession(
    sessionId: string,
    role: 'user' | 'assistant' | 'system',
    content: string,
    agent?: AgentType,
    source?: SessionMessageSource,
    images?: Array<{ dataUrl: string }>,
    options?: SessionMessageAppendOptions
  ): SessionMessage {
    const session = this.getSession(sessionId);
    if (!session) {
      throw new Error(`Session not found: ${sessionId}`);
    }
    return this.appendMessageToSession(session, role, content, agent, source, images, options);
  }

  upsertMessageToSession(
    sessionId: string,
    role: 'user' | 'assistant' | 'system',
    content: string,
    agent?: AgentType,
    source?: SessionMessageSource,
    images?: Array<{ dataUrl: string }>,
    options?: SessionMessageUpsertOptions,
  ): SessionMessage {
    const session = this.getSession(sessionId);
    if (!session) {
      throw new Error(`Session not found: ${sessionId}`);
    }

    const messageId = options?.id?.trim();
    if (!messageId) {
      return this.appendMessageToSession(session, role, content, agent, source, images, options);
    }

    const existingIndex = session.messages.findIndex((message) => message.id === messageId);
    if (existingIndex < 0) {
      return this.appendMessageToSession(session, role, content, agent, source, images, options);
    }

    const metadata = options?.metadata && typeof options.metadata === 'object' && !Array.isArray(options.metadata)
      ? { ...options.metadata }
      : undefined;
    const existingMetadata = session.messages[existingIndex].metadata && typeof session.messages[existingIndex].metadata === 'object'
      && !Array.isArray(session.messages[existingIndex].metadata)
      ? session.messages[existingIndex].metadata
      : undefined;
    const nextBlocks = (Array.isArray(options?.blocks) && options!.blocks.length > 0)
      ? options!.blocks
      : session.messages[existingIndex].blocks;
    const nextMessage: SessionMessage = {
      ...session.messages[existingIndex],
      id: messageId,
      role,
      content,
      agent,
      source,
      timestamp: typeof options?.timestamp === 'number' && Number.isFinite(options.timestamp)
        ? Math.min(session.messages[existingIndex].timestamp, options.timestamp)
        : session.messages[existingIndex].timestamp,
      updatedAt: typeof options?.updatedAt === 'number' && Number.isFinite(options.updatedAt)
        ? Math.max(session.messages[existingIndex].updatedAt || session.messages[existingIndex].timestamp, options.updatedAt)
        : Date.now(),
      images: images && images.length > 0 ? images : undefined,
      // blocks：优先使用新传入的，否则保留已有的（避免后续 upsert 覆盖丢失）
      blocks: nextBlocks,
      type: options?.type,
      category: options?.category,
      visibility: options?.visibility,
      interaction: options?.interaction,
      metadata: sanitizePersistedMessageMetadata({
        role,
        type: options?.type,
        content,
        blocks: nextBlocks,
        metadata: {
          ...(existingMetadata || {}),
          ...(metadata || {}),
        },
      }),
    };

    session.messages[existingIndex] = nextMessage;
    this.syncTimelineSnapshotFromMessage(session, nextMessage);
    session.updatedAt = Date.now();
    this.saveSession(session);
    return nextMessage;
  }

  private appendMessageToSession(
    session: UnifiedSession,
    role: 'user' | 'assistant' | 'system',
    content: string,
    agent?: AgentType,
    source?: SessionMessageSource,
    images?: Array<{ dataUrl: string }>,
    options?: SessionMessageAppendOptions
  ): SessionMessage {
    const metadata = options?.metadata && typeof options.metadata === 'object' && !Array.isArray(options.metadata)
      ? { ...options.metadata }
      : undefined;
    const blocks = Array.isArray(options?.blocks) && options!.blocks.length > 0 ? options!.blocks : undefined;

    const message: SessionMessage = {
      id: options?.id || generateMessageId(),
      role,
      content,
      agent,
      source,
      timestamp: typeof (options as SessionMessageUpsertOptions | undefined)?.timestamp === 'number'
        && Number.isFinite((options as SessionMessageUpsertOptions | undefined)?.timestamp)
        ? Math.floor((options as SessionMessageUpsertOptions).timestamp!)
        : Date.now(),
      updatedAt: typeof options?.updatedAt === 'number' && Number.isFinite(options.updatedAt)
        ? options.updatedAt
        : Date.now(),
      images: images && images.length > 0 ? images : undefined,
      blocks,
      type: options?.type,
      category: options?.category,
      visibility: options?.visibility,
      interaction: options?.interaction,
      metadata: sanitizePersistedMessageMetadata({
        role,
        type: options?.type,
        content,
        blocks,
        metadata,
      }),
    };

    session.messages.push(message);
    this.syncTimelineSnapshotFromMessage(session, message);
    session.updatedAt = Date.now();

    // 自动生成会话标题
    if (!session.name && this.isUserMessage(message) && session.messages.filter(m => this.isUserMessage(m)).length === 1) {
      session.name = this.generateSessionTitle(content);
    }

    // 消息数量管理：如果超过阈值，清理历史消息
    this.cleanupOldMessagesIfNeeded(session);

    this.saveSession(session);
    return message;
  }

  /** 生成会话标题 */
  private generateSessionTitle(firstMessage: string): string {
    let text = firstMessage.trim().replace(/\n+/g, ' ').replace(/\s+/g, ' ');

    // 移除冗余前缀
    const prefixes = [/^(请|帮我|帮忙|能不能|可以|麻烦|我想|我要|我需要)/, /^(please|can you|could you|help me)/i];
    for (const p of prefixes) text = text.replace(p, '').trim();

    // 移除末尾语气词
    const suffixes = [/(吗|呢|吧|啊|谢谢|thanks)[\s。？?！!]*$/i];
    for (const s of suffixes) text = text.replace(s, '').trim();

    return text.length <= 100 ? text : text.substring(0, 100) + '...';
  }

  /** 更新会话数据 */
  updateSessionData(sessionId: string, messages: SessionMessage[]): boolean {  // ✅ 移除 cliOutputs 参数
    const session = this.sessions.get(sessionId);
    if (session) {
      // 最后防线：禁止用空消息列表覆盖已有消息，防止因前端时序问题导致数据丢失
      if (messages.length === 0 && session.messages.length > 0) {
        logger.warn('会话.更新.拒绝_空覆写', {
          sessionId,
          existingCount: session.messages.length,
        }, LogCategory.SESSION);
        return false;
      }

      const normalizedMessages: SessionMessage[] = [];
      let normalizedRoleCount = 0;
      for (const msg of messages) {
        const normalized = this.normalizeMessageRole(msg);
        if (normalized.role !== msg.role) {
          normalizedRoleCount++;
        }
        normalizedMessages.push(normalized);
      }
      const dedupedMessages = this.deduplicateSessionMessages(normalizedMessages);

      if (normalizedRoleCount > 0) {
        logger.warn('会话.更新.role归一化', {
          sessionId,
          normalizedRoleCount,
        }, LogCategory.SESSION);
      }

      for (const msg of dedupedMessages) {
        if (!msg.id || typeof msg.id !== 'string' || !msg.id.trim()) {
          throw new Error('Session message missing id');
        }
        if (!msg.role || !['user', 'assistant', 'system'].includes(msg.role)) {
          throw new Error('Session message role invalid');
        }
        if (typeof msg.content !== 'string') {
          throw new Error('Session message content invalid');
        }
        if (typeof msg.timestamp !== 'number') {
          throw new Error('Session message timestamp invalid');
        }
        if (msg.blocks !== undefined) {
          if (!Array.isArray(msg.blocks)) {
            throw new Error('Session message blocks invalid');
          }
          const hasInvalidBlock = msg.blocks.some((block) => {
            if (!block || typeof block !== 'object' || Array.isArray(block)) {
              return true;
            }
            return typeof (block as { type?: unknown }).type !== 'string';
          });
          if (hasInvalidBlock) {
            throw new Error('Session message block item invalid');
          }
        }
        if (msg.metadata !== undefined && (!msg.metadata || typeof msg.metadata !== 'object' || Array.isArray(msg.metadata))) {
          throw new Error('Session message metadata invalid');
        }
      }

      session.messages = dedupedMessages;
      session.timeline = this.rebuildTimelineSnapshotFromMessages(dedupedMessages);
      // ✅ 移除 cliOutputs 更新逻辑
      session.updatedAt = Date.now();
      this.saveSession(session);
      return true;
    }
    return false;
  }

  /** 重命名会话 */
  renameSession(sessionId: string, name: string): boolean {
    const session = this.getSession(sessionId);
    if (session) {
      session.name = name;
      session.updatedAt = Date.now();
      this.saveSession(session);
      this.refreshSessionMeta(session);
      return true;
    }
    return false;
  }

  /** 清空当前会话消息 */
  clearCurrentSessionMessages(): void {
    const session = this.getCurrentSession();
    if (session) {
      session.messages = [];
      session.timeline = createEmptyTimelineState();
      session.notifications = createEmptyNotificationState(Date.now());
      session.updatedAt = Date.now();
      this.saveSession(session);
    }
  }

  /** 获取最近消息 */
  getRecentMessages(count: number = 10): SessionMessage[] {
    const session = this.getCurrentSession();
    if (!session) return [];
    return session.messages.slice(-count);
  }

  /** 估算消息的 token 数量（统一口径：1 token ≈ 4 字符） */
  private estimateTokenCount(text: string): number {
    return estimateTokenCount(text);
  }

  /** 获取消息的总 token 数 */
  private getMessageTokenCount(message: SessionMessage): number {
    let total = this.estimateTokenCount(message.content);

    // 添加元数据的 token 开销（role, timestamp 等）
    total += 20; // 固定开销

    return total;
  }

  /** 获取在 token 预算内的最近消息 */
  getRecentMessagesWithinTokenBudget(maxTokens: number = 8000): SessionMessage[] {
    const session = this.getCurrentSession();
    if (!session || session.messages.length === 0) return [];

    const messages: SessionMessage[] = [];
    let totalTokens = 0;

    // 从最新消息开始，向前累加
    for (let i = session.messages.length - 1; i >= 0; i--) {
      const message = session.messages[i];
      const messageTokens = this.getMessageTokenCount(message);

      if (totalTokens + messageTokens > maxTokens) {
        // 超出预算，停止添加
        break;
      }

      messages.unshift(message); // 添加到开头以保持顺序
      totalTokens += messageTokens;
    }

    return messages;
  }

  /** 获取上下文窗口统计信息 */
  getContextWindowStats(): {
    totalMessages: number;
    estimatedTokens: number;
    oldestMessageAge: number;
    newestMessageAge: number;
  } {
    const session = this.getCurrentSession();
    if (!session || session.messages.length === 0) {
      return {
        totalMessages: 0,
        estimatedTokens: 0,
        oldestMessageAge: 0,
        newestMessageAge: 0,
      };
    }

    const now = Date.now();
    let totalTokens = 0;

    for (const message of session.messages) {
      totalTokens += this.getMessageTokenCount(message);
    }

    return {
      totalMessages: session.messages.length,
      estimatedTokens: totalTokens,
      oldestMessageAge: now - session.messages[0].timestamp,
      newestMessageAge: now - session.messages[session.messages.length - 1].timestamp,
    };
  }

  // ============================================================================
  // 快照管理
  // ============================================================================

  /** 添加快照元数据 */
  addSnapshot(sessionId: string, snapshot: FileSnapshotMeta): void {
    const session = this.sessions.get(sessionId);
    if (session) {
      if (!this.isValidSnapshotMeta(snapshot)) {
        logger.error('会话.快照.非法_元数据', { sessionId, snapshot }, LogCategory.SESSION);
        throw new Error('Invalid snapshot metadata');
      }

      // 以 snapshot.id 为唯一主键，避免按 filePath 覆盖导致跨轮快照丢失
      const existingIndex = session.snapshots.findIndex(s => s.id === snapshot.id);
      if (existingIndex !== -1) {
        const previous = session.snapshots[existingIndex];
        const previousContributors = previous.contributors ?? [previous.workerId];
        const nextContributors = snapshot.contributors ?? [snapshot.workerId];
        snapshot.contributors = Array.from(new Set([...previousContributors, ...nextContributors]));
        session.snapshots[existingIndex] = snapshot;
      } else {
        session.snapshots.push(snapshot);
      }
      session.snapshots.sort((a, b) => (a.timestamp - b.timestamp) || a.id.localeCompare(b.id));

      try {
        this.saveSession(session);
      } catch (error) {
        logger.error('会话.快照.保存_失败', error, LogCategory.SESSION);
        throw error;
      }
    }
  }

  /** 获取快照元数据 */
  getSnapshot(sessionId: string, filePath: string): FileSnapshotMeta | null {
    return this.getEarliestSnapshotByFile(sessionId, filePath);
  }

  /** 获取指定文件的最早快照元数据 */
  getEarliestSnapshotByFile(sessionId: string, filePath: string): FileSnapshotMeta | null {
    const session = this.sessions.get(sessionId);
    if (session) {
      const matched = session.snapshots.filter(s => s.filePath === filePath);
      if (matched.length === 0) {
        return null;
      }
      matched.sort((a, b) => (a.timestamp - b.timestamp) || a.id.localeCompare(b.id));
      return matched[0];
    }
    return null;
  }

  /** 获取指定文件的最新快照元数据 */
  getLatestSnapshotByFile(sessionId: string, filePath: string): FileSnapshotMeta | null {
    const session = this.sessions.get(sessionId);
    if (session) {
      const matched = session.snapshots.filter(s => s.filePath === filePath);
      if (matched.length === 0) {
        return null;
      }
      matched.sort((a, b) => (a.timestamp - b.timestamp) || a.id.localeCompare(b.id));
      return matched[matched.length - 1];
    }
    return null;
  }

  /** 获取指定文件的所有快照（按时间升序） */
  getSnapshotsByFile(sessionId: string, filePath: string): FileSnapshotMeta[] {
    const session = this.sessions.get(sessionId);
    if (!session) {
      return [];
    }
    return session.snapshots
      .filter(s => s.filePath === filePath)
      .sort((a, b) => (a.timestamp - b.timestamp) || a.id.localeCompare(b.id));
  }

  /** 通过快照 ID 获取快照元数据 */
  getSnapshotById(sessionId: string, snapshotId: string): FileSnapshotMeta | null {
    const session = this.sessions.get(sessionId);
    if (!session) {
      return null;
    }
    return session.snapshots.find(s => s.id === snapshotId) ?? null;
  }

  /** 按快照 ID 移除快照元数据 */
  removeSnapshotById(sessionId: string, snapshotId: string): boolean {
    const session = this.sessions.get(sessionId);
    if (!session) {
      return false;
    }
    const index = session.snapshots.findIndex(s => s.id === snapshotId);
    if (index === -1) {
      return false;
    }
    session.snapshots.splice(index, 1);
    this.saveSession(session);
    return true;
  }

  /** 按文件移除所有快照元数据 */
  removeSnapshotsByFile(sessionId: string, filePath: string): number {
    const session = this.sessions.get(sessionId);
    if (!session) {
      return 0;
    }
    const before = session.snapshots.length;
    session.snapshots = session.snapshots.filter(s => s.filePath !== filePath);
    const removed = before - session.snapshots.length;
    if (removed > 0) {
      this.saveSession(session);
    }
    return removed;
  }

  /** 移除快照元数据 */
  removeSnapshot(sessionId: string, filePath: string): boolean {
    const earliest = this.getEarliestSnapshotByFile(sessionId, filePath);
    if (!earliest) {
      return false;
    }
    return this.removeSnapshotById(sessionId, earliest.id);
  }

  /** 获取快照文件存储路径 */
  getSnapshotFilePath(sessionId: string, snapshotId: string): string {
    return path.join(this.getSessionDir(sessionId), 'snapshots', `${snapshotId}.snapshot`);
  }

  // ============================================================================
  // 会话删除（清理整个会话目录）
  // ============================================================================

  /** 删除会话（删除整个会话目录） */
  deleteSession(sessionId: string): boolean {
    const session = this.sessions.get(sessionId);
    const hasMeta = this.sessionMetas.has(sessionId);
    if (!session && !hasMeta) return false;

    // 从内存中移除
    if (session) {
      this.sessions.delete(sessionId);
    }
    this.sessionMetas.delete(sessionId);

    // 删除整个会话目录
    const sessionDir = this.getSessionDir(sessionId);
    if (fs.existsSync(sessionDir)) {
      try {
        fs.rmSync(sessionDir, { recursive: true, force: true });
        logger.info('会话.删除.成功', { sessionId }, LogCategory.SESSION);
      } catch (error) {
        logger.error('会话.删除.失败', { sessionId, error }, LogCategory.SESSION);
        // 即使删除失败，也从内存中移除了，返回 true
        // 用户可以手动清理文件系统
      }
    }

    // 如果删除的是当前会话，切换到最新的会话
    if (this.currentSessionId === sessionId) {
      const metas = this.getSessionMetas();
      this.currentSessionId = metas.length > 0 ? metas[0].id : null;
      if (this.currentSessionId) {
        this.getSession(this.currentSessionId);
      }
    }

    globalEventBus.emitEvent('session:ended', {
      sessionId,
      data: { sessionId, reason: 'deleted' },
    });
    return true;
  }

  /** 结束会话（标记为完成但不删除） */
  endSession(sessionId: string): void {
    const session = this.getSession(sessionId);
    if (session) {
      session.status = 'completed';
      this.saveSession(session);
      if (this.currentSessionId === sessionId) {
        this.currentSessionId = null;
      }
      globalEventBus.emitEvent('session:ended', {
        sessionId,
        data: { sessionId, reason: 'completed' },
      });
    }
  }

  // ============================================================================
  // 数据完整性验证
  // ============================================================================

  /** 验证会话数据完整性 */
  private validateSessionData(session: any): boolean {
    // 基础字段验证
    if (!session || typeof session !== 'object') {
      return false;
    }

    // 必需字段验证
    if (!session.id || typeof session.id !== 'string') {
      logger.error('会话.验证.缺失标识', undefined, LogCategory.SESSION);
      return false;
    }

    if (!session.status || !['active', 'completed'].includes(session.status)) {
      logger.error('会话.验证.非法_状态', { status: session.status }, LogCategory.SESSION);
      return false;
    }

    if (typeof session.createdAt !== 'number' || typeof session.updatedAt !== 'number') {
      logger.error('会话.验证.非法_时间戳', undefined, LogCategory.SESSION);
      return false;
    }

    // 数组字段验证
    if (!Array.isArray(session.messages)) {
      logger.error('会话.验证.消息_非数组', undefined, LogCategory.SESSION);
      return false;
    }

    if (!Array.isArray(session.snapshots)) {
      logger.error('会话.验证.快照_非数组', undefined, LogCategory.SESSION);
      return false;
    }

    if (!session.timeline || typeof session.timeline !== 'object' || !Array.isArray(session.timeline.records)) {
      logger.error('会话.验证.时间轴快照_非法', undefined, LogCategory.SESSION);
      return false;
    }

    if (!session.notifications || typeof session.notifications !== 'object' || !Array.isArray(session.notifications.records)) {
      logger.error('会话.验证.通知快照_非法', undefined, LogCategory.SESSION);
      return false;
    }

    if (!session.timelineProjection || !isSessionTimelineProjection(session.timelineProjection)) {
      logger.error('会话.验证.时间轴投影_非法', undefined, LogCategory.SESSION);
      return false;
    }

    // 消息数据验证
    for (const msg of session.messages) {
      if (!msg.id || typeof msg.id !== 'string' || !msg.id.trim()) {
        logger.error('会话.验证.消息_缺失_id', { message: msg }, LogCategory.SESSION);
        return false;
      }
      if (!msg.role || !['user', 'assistant', 'system'].includes(msg.role)) {
        logger.error('会话.验证.消息_非法_role', { message: msg }, LogCategory.SESSION);
        return false;
      }
      if (typeof msg.content !== 'string') {
        logger.error('会话.验证.消息_非法_content', { message: msg }, LogCategory.SESSION);
        return false;
      }
      if (typeof msg.timestamp !== 'number') {
        logger.error('会话.验证.消息_非法_timestamp', { message: msg }, LogCategory.SESSION);
        return false;
      }
      if (msg.blocks !== undefined) {
        if (!Array.isArray(msg.blocks)) {
          logger.error('会话.验证.消息_blocks_非数组', { message: msg }, LogCategory.SESSION);
          return false;
        }
        const invalidBlock = msg.blocks.some((block: any) => {
          if (!block || typeof block !== 'object' || Array.isArray(block)) {
            return true;
          }
          return typeof block.type !== 'string';
        });
        if (invalidBlock) {
          logger.error('会话.验证.消息_blocks_非法', { message: msg }, LogCategory.SESSION);
          return false;
        }
      }
    }

    return true;
  }

  private isValidSnapshotMeta(snapshot: FileSnapshotMeta | undefined | null): snapshot is FileSnapshotMeta {
    if (!snapshot || typeof snapshot !== 'object') return false;
    if (!snapshot.id || typeof snapshot.id !== 'string') return false;
    if (!snapshot.filePath || typeof snapshot.filePath !== 'string' || snapshot.filePath.trim().length === 0) {
      return false;
    }
    if (typeof snapshot.timestamp !== 'number') return false;
    if (!snapshot.workerId || typeof snapshot.workerId !== 'string') return false;
    if (!snapshot.missionId || typeof snapshot.missionId !== 'string') return false;
    if (!snapshot.assignmentId || typeof snapshot.assignmentId !== 'string') return false;
    if (!snapshot.todoId || typeof snapshot.todoId !== 'string') return false;
    return true;
  }

  /** 备份损坏的会话文件 */
  private backupCorruptedSession(sessionId: string, filePath: string): void {
    try {
      const backupPath = `${filePath}.corrupted.${Date.now()}.bak`;
      if (fs.existsSync(filePath)) {
        fs.copyFileSync(filePath, backupPath);
        logger.info('会话.备份.损坏.成功', { backupPath }, LogCategory.SESSION);
      }
    } catch (error) {
      logger.error('会话.备份.损坏.失败', { sessionId, error }, LogCategory.SESSION);
    }
  }

  // ============================================================================
  // 内存管理
  // ============================================================================

  /** 驱逐历史会话（如果超过内存限制） */
  private evictOldSessionsIfNeeded(): void {
    if (this.sessions.size <= this.MAX_SESSIONS_IN_MEMORY) {
      return;
    }

    // 获取所有会话，按更新时间排序（最早的在前）
    const allSessions = Array.from(this.sessions.values())
      .sort((a, b) => a.updatedAt - b.updatedAt);

    // 计算需要驱逐的会话数
    const toEvict = this.sessions.size - this.MAX_SESSIONS_IN_MEMORY;

    // 驱逐最早的非当前会话
    let evicted = 0;
    for (const session of allSessions) {
      if (evicted >= toEvict) break;
      if (session.id === this.currentSessionId) continue; // 不驱逐当前会话

      // 保存到磁盘后从内存中移除
      this.saveSession(session);
      this.sessions.delete(session.id);
      evicted++;
    }

    if (evicted > 0) {
      logger.info('会话.清理.完成', { count: evicted }, LogCategory.SESSION);
    }
  }

  /** 清理历史消息（如果超过阈值） */
  private cleanupOldMessagesIfNeeded(session: UnifiedSession): void {
    if (session.messages.length <= this.MESSAGE_CLEANUP_THRESHOLD) {
      return;
    }

    // 保留最近的消息，删除最早的消息
    const toKeep = Math.floor(this.MAX_MESSAGES_PER_SESSION * 0.8); // 保留 80%
    const removed = session.messages.length - toKeep;

    logger.info(
      '会话.消息.清理',
      { sessionId: session.id, total: session.messages.length, threshold: this.MESSAGE_CLEANUP_THRESHOLD, removed },
      LogCategory.SESSION
    );

    session.messages = session.messages.slice(-toKeep);
    session.timeline = this.rebuildTimelineSnapshotFromMessages(session.messages);
  }

  // ============================================================================
  // 持久化
  // ============================================================================

  /** 保存会话 */
  saveSession(session: UnifiedSession): void {
    const filePath = this.getSessionFilePath(session.id);
    try {
      this.ensureSessionRuntimeState(session);
      this.reconcileSessionMessagesAndTimeline(session);
      // 保存前回调：允许外部模块注入执行链等数据
      if (this.beforeSaveHook) {
        try {
          this.beforeSaveHook(session);
        } catch (hookError) {
          logger.warn('会话.保存.beforeSaveHook异常', {
            sessionId: session.id,
            error: hookError instanceof Error ? hookError.message : String(hookError),
          }, LogCategory.SESSION);
        }
      }
      const projectionSourceMessages = this.resolveProjectionSourceMessages(session);
      session.timelineProjection = buildSessionTimelineProjection({
        id: session.id,
        updatedAt: session.updatedAt,
        messages: projectionSourceMessages,
      });
      const sessionId = session.id;
      this.refreshSessionMeta(session);
      this.sessionPersistQueue.schedule(sessionId, async () => {
        if (!this.sessionMetas.has(sessionId)) {
          return;
        }
        const latestSession = this.sessions.get(sessionId) ?? session;
        const payload = JSON.stringify(this.serializeSessionForDisk(latestSession), null, 2);
        await atomicWriteFile(filePath, payload);
      });
    } catch (error) {
      logger.error('会话.保存.失败', { sessionId: session.id, error }, LogCategory.SESSION);
      throw new Error(`Failed to save session: ${error}`);
    }
  }

  async flushPendingPersistence(): Promise<void> {
    await this.sessionPersistQueue.flushAll();
  }

  /** 保存当前会话 */
  saveCurrentSession(): void {
    const session = this.getCurrentSession();
    if (session) {
      this.saveSession(session);
    }
  }

  /** 加载会话元数据（不进入内存会话缓存） */
  private loadSessionMeta(sessionId: string): SessionMeta | null {
    const filePath = this.getSessionFilePath(sessionId);
    if (!fs.existsSync(filePath)) {
      return null;
    }
    try {
      const data = fs.readFileSync(filePath, 'utf-8');
      const persisted = JSON.parse(data) as unknown;
      if (!isPersistedSessionRecord(persisted)) {
        logger.error('会话.元数据加载.持久化结构_非法', { sessionId }, LogCategory.SESSION);
        this.backupCorruptedSession(sessionId, filePath);
        return null;
      }
      const session = this.hydrateSessionRecord(persisted);
      if (!this.validateSessionData(session)) {
        logger.error('会话.元数据加载.校验_失败', { sessionId }, LogCategory.SESSION);
        this.backupCorruptedSession(sessionId, filePath);
        return null;
      }
      const meta = this.buildSessionMeta(session);
      this.sessionMetas.set(meta.id, meta);
      return meta;
    } catch (e) {
      logger.error('会话.元数据加载.失败', { sessionId, error: e }, LogCategory.SESSION);
      this.backupCorruptedSession(sessionId, filePath);
      return null;
    }
  }

  /** 加载会话 */
  private loadSession(sessionId: string): UnifiedSession | null {
    const filePath = this.getSessionFilePath(sessionId);
    if (fs.existsSync(filePath)) {
      try {
        const data = fs.readFileSync(filePath, 'utf-8');
        const persisted = JSON.parse(data) as unknown;
        if (!isPersistedSessionRecord(persisted)) {
          logger.error('会话.加载.持久化结构_非法', { sessionId }, LogCategory.SESSION);
          this.backupCorruptedSession(sessionId, filePath);
          return null;
        }
        const session = this.hydrateSessionRecord(persisted);

        // 数据完整性验证
        if (!this.validateSessionData(session)) {
          logger.error('会话.加载.校验_失败', { sessionId }, LogCategory.SESSION);
          this.backupCorruptedSession(sessionId, filePath);
          return null;
        }

        this.sessions.set(session.id, session);
        this.refreshSessionMeta(session);
        // 加载后回调：允许外部模块恢复执行链等数据
        if (this.afterLoadHook) {
          try {
            this.afterLoadHook(session);
          } catch (hookError) {
            logger.warn('会话.加载.afterLoadHook异常', {
              sessionId: session.id,
              error: hookError instanceof Error ? hookError.message : String(hookError),
            }, LogCategory.SESSION);
          }
        }
        return session;
      } catch (e) {
        logger.error('会话.加载.失败', { sessionId, error: e }, LogCategory.SESSION);
        // 尝试备份损坏的会话文件
        this.backupCorruptedSession(sessionId, filePath);
      }
    }
    return null;
  }

  private serializeSessionForDisk(session: UnifiedSession): PersistedUnifiedSessionRecord {
    return {
      schemaVersion: 'session-runtime.v2',
      id: session.id,
      ...(session.name ? { name: session.name } : {}),
      status: session.status,
      createdAt: session.createdAt,
      updatedAt: session.updatedAt,
      timeline: session.timeline,
      notifications: session.notifications,
      snapshots: session.snapshots,
      ...(session.executionChains ? { executionChains: session.executionChains } : {}),
      ...(session.resumeSnapshots ? { resumeSnapshots: session.resumeSnapshots } : {}),
    };
  }

  private hydrateSessionRecord(record: PersistedUnifiedSessionRecord): UnifiedSession {
    const timeline = {
      lastEventSeq: typeof record.timeline.lastEventSeq === 'number' && Number.isFinite(record.timeline.lastEventSeq)
        ? Math.floor(record.timeline.lastEventSeq)
        : 0,
      records: sortTimelineRecordsBySemanticOrder(record.timeline.records),
    };
    const notifications = record.notifications;
    const materializedMessages = this.deduplicateSessionMessages(
      materializeSessionMessagesFromTimelineRecords(timeline.records),
    );
    const rebuiltProjection = buildSessionTimelineProjection({
      id: record.id,
      updatedAt: record.updatedAt,
      messages: materializeProjectionSourceMessagesFromTimelineRecords(timeline.records),
    });
    return {
      schemaVersion: 'session-runtime.v2',
      id: record.id,
      name: record.name,
      status: record.status,
      createdAt: record.createdAt,
      updatedAt: record.updatedAt,
      messages: materializedMessages,
      timeline,
      notifications,
      snapshots: Array.isArray(record.snapshots) ? record.snapshots : [],
      timelineProjection: rebuiltProjection,
      ...(record.executionChains ? { executionChains: record.executionChains } : {}),
      ...(record.resumeSnapshots ? { resumeSnapshots: record.resumeSnapshots } : {}),
    };
  }

  private areSessionMessagesEquivalent(left: SessionMessage[], right: SessionMessage[]): boolean {
    return JSON.stringify(left) === JSON.stringify(right);
  }

  private areTimelineRecordsEquivalent(left: TimelineRecord[], right: TimelineRecord[]): boolean {
    return JSON.stringify(left) === JSON.stringify(right);
  }

  /** 加载所有会话 */
  private loadAllSessions(): void {
    if (!fs.existsSync(this.baseDir)) return;

    // 遍历 sessions 目录下的所有子目录
    const entries = fs.readdirSync(this.baseDir, { withFileTypes: true });
    const metas: SessionMeta[] = [];
    for (const entry of entries) {
      if (entry.isDirectory()) {
        const sessionId = entry.name;
        const meta = this.loadSessionMeta(sessionId);
        if (meta) {
          metas.push(meta);
        }
      }
    }

    // 设置当前会话为最新的会话
    if (metas.length > 0) {
      metas.sort((a, b) => b.updatedAt - a.updatedAt);
      this.currentSessionId = metas[0].id;
    }
  }

  // ============================================================================
  // 辅助路径方法（供其他管理器使用）
  // ============================================================================

  /** 获取计划目录 */
  getPlansDir(sessionId: string): string {
    return path.join(this.getSessionDir(sessionId), 'plans');
  }

  /** 获取任务状态文件路径 */
  getTasksFilePath(sessionId: string): string {
    return path.join(this.getSessionDir(sessionId), 'tasks.json');
  }

  /** 获取执行状态文件路径 */
  getExecutionStateFilePath(sessionId: string): string {
    return path.join(this.getSessionDir(sessionId), 'execution-state.json');
  }

  /** 获取快照目录 */
  getSnapshotsDir(sessionId: string): string {
    return path.join(this.getSessionDir(sessionId), 'snapshots');
  }

  // ============================================================================
  // 会话总结生成（用于会话恢复）
  // ============================================================================

  /** 生成会话总结（用于会话切换时的上下文注入）
   * 注意：任务信息现在从 Mission 系统获取，此方法返回的任务信息可能为空
   * 调用方应使用 MissionDrivenEngine.listTaskViews() 获取完整任务列表
   */
  getSessionSummary(sessionId?: string): SessionSummary | null {
    const session = sessionId ? this.getSession(sessionId) : this.getCurrentSession();
    if (!session) return null;

    // 任务信息已迁移到 Mission 系统，这里返回空数组
    // 调用方应使用 MissionDrivenEngine.listTaskViews() 获取任务
    const completedTasks: string[] = [];
    const inProgressTasks: string[] = [];
    const pendingIssues: string[] = [];

    // 提取代码变更摘要
    const codeChanges = session.snapshots
      .map(s => `${s.filePath} (${s.workerId})`)
      .slice(0, 20); // 最多 20 个文件

    // 提取关键决策（从消息中提取）
    const keyDecisions = this.extractKeyDecisions(session.messages);

    // 生成会话目标（从第一条用户消息或会话名称）
    const objective = this.extractObjective(session);

    return {
      sessionId: session.id,
      title: session.name || '未命名会话',
      objective,
      completedTasks,
      inProgressTasks,
      keyDecisions,
      codeChanges,
      pendingIssues,
      messageCount: session.messages.filter(m => this.isUserMessage(m)).length,
      lastUpdated: session.updatedAt,
    };
  }

  /** 提取会话目标 */
  private extractObjective(session: UnifiedSession): string {
    // 优先使用会话名称
    if (session.name) {
      return session.name;
    }

    // 否则使用第一条用户消息
    const firstUserMsg = session.messages.find(m => this.isUserMessage(m));
    if (firstUserMsg) {
      const content = firstUserMsg.content.trim();
      return content.length > 100 ? content.substring(0, 100) + '...' : content;
    }

    return '新对话';
  }

  /** 提取关键决策（简单规则：包含关键词的消息） */
  private extractKeyDecisions(messages: SessionMessage[]): string[] {
    const decisionKeywords = [
      '决定', '选择', '采用', '使用', '方案', '架构',
      'decide', 'choose', 'use', 'adopt', 'approach', 'architecture'
    ];

    const decisions: string[] = [];

    for (const msg of messages) {
      if (msg.role !== 'assistant') continue;

      const content = msg.content.toLowerCase();
      const hasKeyword = decisionKeywords.some(kw => content.includes(kw.toLowerCase()));

      if (hasKeyword) {
        // 提取包含关键词的句子
        const sentences = msg.content.split(/[。！？.!?]/);
        for (const sentence of sentences) {
          const sentenceLower = sentence.toLowerCase();
          if (decisionKeywords.some(kw => sentenceLower.includes(kw.toLowerCase()))) {
            const trimmed = sentence.trim();
            if (trimmed.length > 10 && trimmed.length < 200) {
              decisions.push(trimmed);
              if (decisions.length >= 5) break; // 最多 5 个决策
            }
          }
        }
      }

      if (decisions.length >= 5) break;
    }

    return decisions;
  }

  /** 格式化会话总结为文本（用于注入到上下文） */
  formatSessionSummary(summary: SessionSummary): string {
    const parts: string[] = [];

    parts.push(`# 会话总结: ${summary.title}`);
    parts.push(`会话目标: ${summary.objective}`);
    parts.push(`消息数量: ${summary.messageCount} 条`);
    parts.push('');

    if (summary.completedTasks.length > 0) {
      parts.push('## 已完成任务:');
      summary.completedTasks.forEach((task, i) => {
        parts.push(`${i + 1}. ${task}`);
      });
      parts.push('');
    }

    if (summary.inProgressTasks.length > 0) {
      parts.push('## 进行中任务:');
      summary.inProgressTasks.forEach((task, i) => {
        parts.push(`${i + 1}. ${task}`);
      });
      parts.push('');
    }

    if (summary.keyDecisions.length > 0) {
      parts.push('## 关键决策:');
      summary.keyDecisions.forEach((decision, i) => {
        parts.push(`${i + 1}. ${decision}`);
      });
      parts.push('');
    }

    if (summary.codeChanges.length > 0) {
      parts.push('## 代码变更:');
      summary.codeChanges.forEach((change, i) => {
        parts.push(`${i + 1}. ${change}`);
      });
      parts.push('');
    }

    if (summary.pendingIssues.length > 0) {
      parts.push('## 待解决问题:');
      summary.pendingIssues.forEach((issue, i) => {
        parts.push(`${i + 1}. ${issue}`);
      });
      parts.push('');
    }

    return parts.join('\n');
  }

  // ============================================================================
  // 格式化和清理方法
  // ============================================================================

  /** 格式化对话历史为字符串（用于 Prompt 增强） */
  formatConversationHistory(count: number = 10): string {
    const messages = this.getRecentMessages(count);
    if (messages.length === 0) {
      return '';
    }
    return messages
      .map(m => `${this.isUserMessage(m) ? 'User' : 'Assistant'}: ${m.content}`)
      .join('\n\n');
  }

  /** 清理任务状态文件（删除会话时自动清理，因为在同一目录） */
  private cleanupTaskState(sessionId: string): void {
    const taskFilePath = this.getTasksFilePath(sessionId);
    if (fs.existsSync(taskFilePath)) {
      try {
        fs.unlinkSync(taskFilePath);
        logger.info('会话.清理.任务_状态.成功', { path: taskFilePath }, LogCategory.SESSION);
      } catch (e) {
        logger.error('会话.清理.任务_状态.失败', { path: taskFilePath, error: e }, LogCategory.SESSION);
      }
    }
  }

  /** 清理图片附件 */
  private cleanupAttachments(session: UnifiedSession): void {
    for (const message of session.messages) {
      if (message.attachments && message.attachments.length > 0) {
        for (const attachment of message.attachments) {
          if (attachment.path.includes('.magi/attachments') && fs.existsSync(attachment.path)) {
            try {
              fs.unlinkSync(attachment.path);
              logger.info('会话.清理.附件.成功', { path: attachment.path }, LogCategory.SESSION);
            } catch (e) {
              logger.error('会话.清理.附件.失败', { path: attachment.path, error: e }, LogCategory.SESSION);
            }
          }
        }
      }
    }
  }

  // ============================================================================
  // Mission Storage 支持（新架构）
  // ============================================================================

  /** 获取会话的 missions 目录路径 */
  getMissionsDir(sessionId: string): string {
    return path.join(this.getSessionDir(sessionId), 'missions');
  }

  /** 确保 missions 目录存在 */
  ensureMissionsDir(sessionId: string): void {
    const missionsDir = this.getMissionsDir(sessionId);
    if (!fs.existsSync(missionsDir)) {
      fs.mkdirSync(missionsDir, { recursive: true });
    }
  }
}
