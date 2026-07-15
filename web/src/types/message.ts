/**
 * 消息类型定义
 */

import type { AgentId, AnyAgentId } from '../shared/types/agent-types';
import type { LocaleCode } from '../i18n/types';

// --- 从 orchestrator/runtime/orchestration-runtime-state-types 内联前端所需子集 ---

export type OrchestrationRuntimeStateStatus =
  | 'idle'
  | 'running'
  | 'waiting'
  | 'paused'
  | 'blocked'
  | 'completed'
  | 'failed'
  | 'cancelled';

export type ExecutionChainStatus = string;
export type InterruptedReason = string;

export interface OrchestrationRuntimeAssignmentSummary {
  assignmentId: string;
  workerId?: string;
  title: string;
  status: string;
  progress: number;
  taskTotal: number;
  awaitingApprovalTaskCount: number;
  reviewRequiredTaskCount: number;
  blockedTaskCount: number;
  completedTaskCount: number;
  failedTaskCount: number;
  runningTaskCount: number;
  trace?: unknown;
}

export interface OrchestrationRuntimeChainSummary {
  chainId: string;
  status: ExecutionChainStatus;
  recoverable: boolean;
  attempt: number;
  createdAt: number;
  updatedAt: number;
  interruptedReason?: InterruptedReason;
}

export interface OrchestrationRuntimeScopeSummary {
  sessionId: string;
  requestId?: string;
  executionGroupId?: string;
  planId?: string;
  batchId?: string;
}

export interface OrchestrationRuntimeExecutionGroupSummary {
  executionGroupId: string;
  title: string;
  status: string;
  phase: string;
  deliveryStatus: string;
  updatedAt: number;
  failureReason?: string;
}

export interface OrchestrationRuntimePlanSummary {
  planId: string;
  status: string;
  mode: string;
  revision: number;
  version: number;
  updatedAt: number;
  acceptanceSummary: unknown;
  waitState: string;
  replanState: string;
  terminationReason?: string;
}

export interface OrchestrationRuntimeTimelineEntry {
  eventId: string;
  seq: number;
  timestamp: number;
  type: string;
  summary: string;
  diffCount: number;
  trace?: unknown;
}

export interface OrchestrationRuntimeStateDiffEntry {
  eventId: string;
  timestamp: number;
  entityType: string;
  entityId: string;
  changedKeys: string[];
  beforeSummary?: string;
  afterSummary?: string;
}

export interface OrchestrationRuntimeFailureRootCause {
  summary: string;
  eventType?: string;
  eventId?: string;
  occurredAt: number;
  assignmentId?: string;
  taskId?: string;
  verificationId?: string;
  error?: string;
}

export interface OrchestrationRuntimeRecoverySummary {
  continuationPolicy?: string;
  continuationReason?: string;
  waitState?: string;
  waitReasonCode?: string;
  replanState?: string;
  replanReason?: string;
  terminationReason?: string;
  acceptanceSummary?: unknown;
  reviewState?: string;
  latestSnapshotId?: string;
  latestSnapshotCreatedAt?: number;
  snapshotStorage?: 'head_commit' | 'ghost_commit';
  snapshotRef?: string;
  snapshotBaseRef?: string;
  snapshotDirtyFileCount?: number;
  snapshotPendingChangeCount?: number;
  restoredWorkerBranchCount?: number;
  restoredWorkerSessionCount?: number;
  pendingTaskCount?: number;
  runningTaskCount?: number;
  completedTaskCount?: number;
  cancelledTaskCount?: number;
}

export interface OrchestrationRuntimeGovernanceView {
  mode: string;
  phase: string;
  dispatchAllowed: boolean;
  phaseReason: string;
  reviewState: string;
  verificationSummary: string;
  waitState: string;
  replanState: string;
  approvalAction: string;
  approvalReason: string;
  blockedAssignments: number;
  awaitingApprovalAssignments: number;
  reviewRequiredAssignments: number;
}

export interface OrchestrationRuntimeOpsView {
  scope: OrchestrationRuntimeScopeSummary;
  timelinePath: string;
  eventCount: number;
  diffCount: number;
  executionGroup?: OrchestrationRuntimeExecutionGroupSummary;
  plan?: OrchestrationRuntimePlanSummary;
  recentTimeline: OrchestrationRuntimeTimelineEntry[];
  recentStateDiffs: OrchestrationRuntimeStateDiffEntry[];
  failureRootCause?: OrchestrationRuntimeFailureRootCause;
  recovery?: OrchestrationRuntimeRecoverySummary;
  governance?: OrchestrationRuntimeGovernanceView;
  projectInstructions?: unknown;
  knowledgeSnapshot?: unknown;
  knowledgeAudit?: OrchestrationRuntimeKnowledgeAuditView;
  evidenceLedger?: unknown;
}

export type OrchestrationRuntimeKnowledgeDecision =
  | 'not_needed'
  | 'missing_workspace'
  | 'queried_no_match'
  | 'matched_not_injected'
  | 'injected';

export interface OrchestrationRuntimeKnowledgeAuditEntry {
  timestamp: number;
  consumer: string;
  decision?: OrchestrationRuntimeKnowledgeDecision;
  status?: string;
  failureReason?: string;
  candidateCount?: number;
  insertedCount?: number;
  knowledgeIds: string[];
  resultKinds: string[];
  matchedCount: number;
  injectedCount: number;
  injectedChars: number;
  truncated: boolean;
  purpose: string;
  resultKind: string;
  referenceCount: number;
}

export interface OrchestrationRuntimeKnowledgeAuditView {
  eventCount: number;
  recentEntries: OrchestrationRuntimeKnowledgeAuditEntry[];
}

// --- 从 types.ts 内联 UIProcessingState ---

export type MessageSource = 'orchestrator' | 'system' | string;

export interface UIProcessingState {
  isProcessing: boolean;
  source: MessageSource | null;
  agent: string | null;
  startedAt: number | null;
  pendingRequestIds: string[];
}

// 消息角色
export type MessageRole = 'user' | 'assistant' | 'system';

// 占位消息状态（符合 message-response-flow-design.md 规范）
export type PlaceholderState =
  | 'pending'    // 正在准备...（发送后立即）
  | 'received'   // 已接收...（后端确认接收）
  | 'thinking'   // 正在思考...（编排进入分析）
  ;

// 请求-响应绑定
export interface RequestResponseBinding {
  /** 用户请求 ID（前端生成） */
  requestId: string;
  /** 用户消息 ID */
  userMessageId: string;
  /** 本地响应占位消息 ID */
  placeholderMessageId?: string;
  /** 真实响应消息 ID（后端生成） */
  realMessageId?: string;
  /** 发送意图创建时分配的稳定渲染轮次序号，保证后端 accepted 前也按轮次排序 */
  turnOrderSeq?: number;
  /** 后端 accepted 后的 canonical turn 序号 */
  turnSeq?: number;
  /** 创建时间戳 */
  createdAt: number;
  /** 首 token 超时定时器 ID */
  timeoutId?: ReturnType<typeof setTimeout>;
}

export type RetryRuntimePhase = 'attempt_started' | 'scheduled';

export interface RetryRuntimeState {
  phase: RetryRuntimePhase;
  attempt: number;
  maxAttempts: number;
  delayMs?: number;
  nextRetryAt?: number;
}

// 消息类型 - 与协议层 MessageType 完全对齐
export type MessageType =
  // 协议层核心类型
  | 'text'
  | 'plan'
  | 'progress'
  | 'result'
  | 'error'
  | 'interaction'
  | 'system-notice'
  | 'tool_call'
  | 'thinking'
  // 方案 B 扩展类型
  | 'user_input'
  | 'task_card'
  | 'instruction';

// 通知类型
export type NoticeType = 'info' | 'success' | 'warning' | 'error';

// 工具调用状态
export type ToolCallStatus = 'pending' | 'running' | 'success' | 'error';

// 工具结果标准化状态（与协议层保持一致）
export type StandardizedToolStatus =
  | 'success'
  | 'error'
  | 'timeout'
  | 'killed'
  | 'blocked'
  | 'rejected'
  | 'aborted';

// 工具结果标准化结构（机器可读）
export interface StandardizedToolResult {
  schemaVersion: 'tool-result.v1';
  source: 'builtin' | 'mcp' | 'skill';
  toolName: string;
  toolCallId: string;
  status: StandardizedToolStatus;
  message: string;
  data?: unknown;
  errorCode?: string;
  sourceId?: string;
}

export interface MessageImage {
  name?: string;
  dataUrl: string;
}

export interface MessageContextReference {
  kind: 'file' | 'directory';
  path: string;
  name: string;
}

export interface ToolPolicyPayload {
  schemaVersion: 'tool-policy.v1';
  source: 'request' | 'mode' | 'skill' | 'composed';
  allowedToolNames?: string[];
  forbiddenToolNames?: string[];
  readOnly?: boolean;
  allowedFilePatternGroups?: string[][];
  forbiddenFilePatterns?: string[];
  restrictUnknownExternalTools?: boolean;
  activeInstructionSkillName?: string;
}

// 工具调用
export interface ToolCall {
  id: string;
  name: string;
  arguments: Record<string, unknown>;
  status: ToolCallStatus;
  result?: string;
  error?: string;
  standardized?: StandardizedToolResult;
  durationMs?: number;
  startTime?: number;
  endTime?: number;
}

export type TerminalOperation = 'shell_exec';

export interface TerminalSessionBlock {
  terminalId: number;
  operation: TerminalOperation;
  status: string;
  phase?: string;
  runMode?: 'task' | 'service';
  terminalName?: string;
  cwd?: string;
  command?: string;
  output: string;
  outputCursor?: number;
  outputStartCursor?: number;
  fromCursor?: number;
  nextCursor?: number;
  delta?: boolean;
  truncated?: boolean;
  startupStatus?: 'pending' | 'confirmed' | 'timeout' | 'failed' | 'skipped';
  startupMessage?: string;
  locked?: boolean;
  returnCode?: number | null;
  accepted?: boolean;
  killed?: boolean;
  releasedLock?: boolean;
  error?: string;
  updatedAt: number;
}

// 思考块
export interface ThinkingBlock {
  content: string;
  isComplete: boolean;
  summary?: string;
  blockId?: string;
}

// 消息内容块
export interface ContentBlock {
  id: string;                // 唯一标识符，用于 #each 循环的 key（构造时必须确定性赋值）
  type: 'text' | 'code' | 'thinking' | 'tool_call' | 'tool_result' | 'file_change' | 'plan';
  content: string;
  language?: string;        // 代码块语言
  toolCall?: ToolCall;      // 工具调用信息
  thinking?: ThinkingBlock; // 思考块信息
  fileChange?: {
    sessionId?: string;
    workspaceId?: string;
    workspacePath?: string;
    filePath: string;
    oldPath?: string;
    changeType: 'create' | 'modify' | 'delete' | 'rename';
    additions?: number;
    deletions?: number;
    diff?: string;
    contentKind?: EditContentKind;
    size?: number;
    mime?: string;
    error?: string;
    symlinkTarget?: string;
    headSummary?: string;
    tailSummary?: string;
    toolCallId?: string;
  };
  plan?: {
    goal: string;
    analysis?: string;
    constraints?: string[];
    acceptanceCriteria?: string[];
    riskLevel?: 'low' | 'medium' | 'high';
    riskFactors?: string[];
    rawJson?: string;
  };
  blockId?: string;
  status?: 'pending' | 'running' | 'completed' | 'blocked' | 'failed' | 'cancelled';
  summaryText?: string;
}

// 模型连接状态类型（供设置面板和任务执行状态共用）
export type ModelStatusType =
  | 'available'       // 可用（已连接）
  | 'connected'       // 已连接
  | 'configured'      // 已配置（未测试连接）
  | 'disabled'        // 已禁用
  | 'not_configured'  // 未配置
  | 'checking'        // 检测中
  | 'error'           // 错误
  | 'unavailable'     // 不可用
  | 'invalid_model'   // 无效模型
  | 'auth_failed'     // 认证失败
  | 'network_error'   // 网络错误
  | 'timeout'         // 超时
  | 'orchestrator';  // 使用编排者模型

export interface ModelStatus {
  status: ModelStatusType;
  model?: string;
  version?: string;
  tokens?: number;
  error?: string;
}

// 模型状态映射
export type ModelStatusMap = Record<string, ModelStatus>;

// Wave 执行状态（提案 4.6）
export interface WaveState {
  /** 当前 Wave 索引 */
  currentWave: number;
  /** 总 Wave 数 */
  totalWaves: number;
  /** 每个 Wave 的任务 ID */
  waves: string[][];
  /** 关键路径 */
  criticalPath: string[];
  /** Wave 执行状态 */
  status: 'idle' | 'executing' | 'completed';
}

export interface OrchestratorRuntimeDecisionGateState {
  noProgressStreak: number;
  budgetBreachStreak: number;
  externalWaitBreachStreak: number;
  consecutiveUpstreamModelErrors: number;
}

export interface OrchestratorRuntimeDecisionTraceEntry {
  round: number;
  phase: 'no_tool' | 'tool' | 'handoff' | 'finalize';
  action: 'continue' | 'continue_with_prompt' | 'terminate' | 'handoff' | 'fallback';
  requiredTotal: number;
  reason?: string;
  candidates?: string[];
  gateState?: OrchestratorRuntimeDecisionGateState;
  note?: string;
  timestamp: number;
}

export interface OrchestratorRuntimeSnapshot {
  progressVector?: {
    terminalRequiredTaskCount?: number;
    acceptedCriteria?: number;
    criticalPathResolved?: number;
    unresolvedBlockers?: number;
  };
  reviewState?: {
    accepted?: number;
    total?: number;
  };
  blockerState?: {
    open?: number;
    score?: number;
    externalWaitOpen?: number;
    maxExternalWaitAgeMs?: number;
  };
  budgetState?: {
    elapsedMs?: number;
    tokenUsed?: number;
    remainingTokens?: number;
    tokenLimit?: number;
    usageRatio?: number;
    warningLevel?: 'normal' | 'notice' | 'warning' | 'danger';
    errorRate?: number;
    lastCompactionAt?: number;
    lastCompactionReason?: string;
    originalTokenEstimate?: number;
    compactedTokenEstimate?: number;
    originalMessageCount?: number;
    compactedMessageCount?: number;
  };
  cacheState?: {
    mode?: 'disabled' | 'unsupported' | 'cache_control' | 'cache_editing';
    health?: 'unknown' | 'healthy' | 'cooling' | 'cold' | 'degraded';
    cacheReadTokens?: number;
    cacheWriteTokens?: number;
    cacheReadRatio?: number;
    baselineCacheReadTokens?: number;
    suspectedBreak?: boolean;
    lastBreakReason?: 'cache_read_miss' | 'cache_read_drop' | 'idle_expired';
    lastResetAt?: number;
    lastResetReason?: 'micro_compaction' | 'idle_micro_compaction' | 'manual_compaction' | 'session_reset';
    lastObservedAt?: number;
  };
  requiredTotal?: number;
  failedRequired?: number;
  runningOrPendingRequired?: number;
  sourceEventIds?: string[];
}

export interface OrchestratorRuntimeState {
  sessionId?: string;
  requestId?: string;
  chain?: OrchestrationRuntimeChainSummary;
  status: OrchestrationRuntimeStateStatus;
  phase: string;
  statusReason?: string;
  canResume?: boolean;
  runtimeReason?: string;
  failureReason?: string;
  errors: string[];
  startedAt?: number;
  statusChangedAt: number;
  lastEventAt: number;
  endedAt?: number;
  runtimeSnapshot?: OrchestratorRuntimeSnapshot | null;
  runtimeDecisionTrace?: OrchestratorRuntimeDecisionTraceEntry[];
  assignments: OrchestrationRuntimeAssignmentSummary[];
  opsView?: OrchestrationRuntimeOpsView | null;
}

// 单条消息
export interface Message {
  id: string;
  role: MessageRole;
  source: MessageSource;
  content: string;            // 完整内容（用于 Markdown 渲染）
  blocks?: ContentBlock[];    // 结构化内容块
  timestamp: number;
  updatedAt?: number;
  isStreaming: boolean;       // 是否正在流式输出
  isComplete: boolean;        // 是否已完成
  type?: MessageType;         // 消息类型（notice = 系统通知）
  noticeType?: NoticeType;    // 通知类型（info/success/warning/error）
  /** 用户上传的图片（base64 Data URL 格式） */
  images?: MessageImage[];
  /** 用户显式添加的文件或目录上下文引用。 */
  contextReferences?: MessageContextReference[];
  metadata?: {
    model?: string;
    tokens?: number;
    duration?: number;
    worker?: string;        // Worker 类型（orchestrator, coder, reviewer 等）
    filePath?: string;      // 相关文件路径
    // 占位消息相关字段
    isPlaceholder?: boolean;          // 是否为占位消息
    placeholderState?: PlaceholderState; // 占位消息状态
    requestId?: string;               // 关联的请求 ID
    turnId?: string;                  // 对话轮次 ID（用于计划账本回溯定位）
    wasPlaceholder?: boolean;         // 是否从占位消息转换而来（用于过渡动画）
    justCompleted?: boolean;          // 是否刚完成（用于完成动画）
    sendingAnimation?: boolean;       // 用户消息发送动画
    eventId?: string;                 // 事件 ID（后端下发）
    eventSeq?: number;                // 事件序号（会话内单调递增）
    cardId?: string;                  // 卡片实体 ID
    cardStreamSeq?: number;           // 卡片流式序号
    parentCardId?: string;            // 父卡片 ID（补遗卡片）
    finalStreamSeq?: number;          // 封口流式序号
    lateArrival?: boolean;            // 是否为晚到补遗
    lateFromCardId?: string;          // 晚到来源 cardId
    [key: string]: unknown;
  };
}

export type { AgentId, AnyAgentId };

export type TimelineProjectionArtifactKind = 'message' | 'tool';

export interface TimelineProjectionArtifact {
  artifactId: string;
  kind: TimelineProjectionArtifactKind;
  displayOrder: number;
  artifactVersion?: number;
  anchorEventSeq: number;
  latestEventSeq: number;
  cardStreamSeq: number;
  timestamp: number;
  cardId?: string;
  lifecycleKey?: string;
  /**
   * 代理 transcript 路由信号。
   *
   * - `undefined` 表示 artifact 归属 root agent 主线（thread）；
   * - 非空字符串表示 artifact 归属对应代理 Task，RightPane 以 `agent:${taskId}` 去重。
   *
   * 由 sidechain `CanonicalTurnItem.worker.taskId` 派生；roleId / workerId 只作为展示元信息，
   * 不再参与 tab 聚合，避免同一 role 的多个代理被合并。
   */
  taskId?: string;
  messageIds: string[];
  message: Message;
}

export interface TimelineProjectionRenderEntry {
  entryId: string;
  artifactId: string;
}

export interface TimelineRenderItem {
  key: string;
  message: Message;
  /** 当前渲染项所属 session。文件预览等动作必须使用该 scope，不能从全局当前会话猜测。 */
  sessionId?: string;
  /** 当前渲染项所属 workspace。 */
  workspaceId?: string;
  /** 当前渲染项所属 workspace 路径。 */
  workspacePath?: string;
}

export interface SessionTimelineProjection {
  schemaVersion: 'session-timeline-projection.v2';
  sessionId: string;
  updatedAt: number;
  lastAppliedEventSeq: number;
  artifacts: TimelineProjectionArtifact[];
  threadRenderEntries: TimelineProjectionRenderEntry[];
}

// 会话信息
export interface Session {
  id: string;
  workspaceId?: string;
  name?: string;  // 可选，未命名会话可能没有 name
  createdAt: number;
  updatedAt: number;
  messageCount?: number;
  isRunning?: boolean;
  runningTaskCount?: number;
  hasUnreadCompletion?: boolean;
  preview?: string;  // 会话预览
  messages?: { id: string; role: string; content: string }[];
  notifications?: {
    lastUpdatedAt: number;
    records: IncidentNotificationRecord[];
  };
}

export interface QueuedMessageImage {
  name: string;
  dataUrl: string;
}

export interface QueuedMessageContextReference {
  kind: 'file' | 'directory';
  path: string;
  name: string;
}

export interface QueuedMessage {
  id: string;
  requestId?: string;
  localMessageId?: string;
  blockedByUserMessageId?: string;
  blockedByUserContent?: string;
  workspaceId?: string;
  workspacePath?: string;
  sessionId?: string;
  content: string;
  text?: string | null;
  createdAt: number;
  skillName?: string | null;
  goalMode?: boolean;
  accessProfile?: 'read_only' | 'restricted' | 'full_access' | null;
  images?: QueuedMessageImage[];
  contextReferences?: QueuedMessageContextReference[];
}

// 处理中的 Actor
export interface ProcessingActor {
  source: MessageSource;
  agent: AnyAgentId;
}

// Tab 类型（动态架构下扩展 tab 为任意 string）
export type TabType = 'thread' | string | 'settings' | 'knowledge' | 'edits';

// 滚动位置映射（动态 key：thread + agent:${taskId} / code:${filepath}）
export interface ScrollPositions {
  thread: number;
  [panelKey: string]: number;
}

// 自动滚动配置（动态 key）
export interface AutoScrollConfig {
  thread: boolean;
  [panelKey: string]: boolean;
}

export interface ScrollAnchor {
  messageId: string | null;
  offsetTop: number;
}

export interface ScrollAnchors {
  thread: ScrollAnchor;
  [workerId: string]: ScrollAnchor;
}

// 任务状态
export type TaskStatus = 'pending' | 'paused' | 'running' | 'completed' | 'failed' | 'cancelled';
export type DeliveryStatus = 'pending' | 'passed' | 'failed' | 'blocked' | 'skipped';

// 子任务状态（对齐后端 SubTaskViewStatus）
export type SubTaskStatus =
  | 'pending'
  | 'waiting_deps'
  | 'review_required'
  | 'awaiting_approval'
  | 'running'
  | 'paused'
  | 'completed'
  | 'failed'
  | 'skipped'
  | 'blocked'
  | 'cancelled'
  | 'in_progress'; // 增量事件可能发送此值

// 子任务（对齐后端 TaskItemView）
export interface SubTaskItem {
  id: string;
  description: string;
  title?: string;
  assignedWorker: string;
  assignmentId: string;
  source?: string;
  status: SubTaskStatus;
  progress: number;
  priority: number;
  approvalStatus?: 'pending' | 'approved' | 'rejected';
  approvalSeverity?: 'medium' | 'high' | 'critical';
  approvalNote?: string;
  reviewStatus?: 'approved' | 'needs_revision' | 'rejected';
  reviewFeedback?: string;
  targetFiles: string[];
  modifiedFiles?: string[];
  error?: string;
  startedAt?: number;
  completedAt?: number;
}

// 任务（对齐后端 TaskView）
export interface Task {
  id: string;
  name: string;
  prompt?: string;
  description?: string;
  status: TaskStatus;
  deliveryStatus?: DeliveryStatus;
  deliverySummary?: string;
  deliveryDetails?: string;
  deliveryWarnings?: string[];
  subTasks: SubTaskItem[];
  progress: number;
  executionGroupId: string;
  failureReason?: string;
}

// 编辑/变更记录
export type EditType = 'add' | 'modify' | 'delete' | 'rename';
export type EditContentKind = 'text' | 'large_text' | 'binary' | 'symlink' | 'special';
export type EditSourceKind = 'tool' | 'watcher' | 'external' | 'baseline';

export interface Edit {
  sessionId?: string;
  workspaceId?: string;
  workspacePath?: string;
  filePath: string;
  oldPath?: string;
  snapshotId?: string;
  updatedAt?: number;
  type?: EditType;
  additions?: number;
  deletions?: number;
  diff?: string;
  originalContent?: string | null;
  previewContent?: string | null;
  previewAbsolutePath?: string;
  previewCanOpenWorkspaceFile?: boolean;
  contentKind?: EditContentKind;
  size?: number;
  mime?: string;
  sourceKind?: EditSourceKind;
  hasError?: boolean;
  revertible?: boolean;
  symlinkTarget?: string;
  headSummary?: string;
  tailSummary?: string;
  toolCallId?: string;
  workerId?: string;
  contributors?: string[];
  executionGroupId?: string;
}

export interface ChangeMutationStatus {
  isMutating: boolean;
  sessionId?: string | null;
  workspaceId?: string | null;
  workspacePath?: string | null;
  updatedAt?: number;
}

// Toast 通知
export type ToastType = 'info' | 'success' | 'warning' | 'error';

export interface Toast {
  id: string;
  type: ToastType;
  title?: string;
  message: string;
  duration?: number;
}

export interface IncidentNotificationRecord {
  notificationId: string;
  kind: 'incident';
  scope: 'app' | 'workspace' | 'session';
  level: string;
  title?: string;
  message: string;
  source?: string;
  workspaceId?: string;
  sessionId?: string;
  createdAt: number;
  read: boolean;
  handled: boolean;
  resolved: boolean;
  actionRequired: boolean;
  countUnread: boolean;
  occurrenceCount: number;
}

// 应用状态（后端下发的完整状态）
export interface AppState {
  sessions?: Session[];
  currentSession?: Session;
  isProcessing?: boolean;
  processingState?: UIProcessingState | null;
  pendingChanges?: unknown[];
  pendingChangesState?: unknown;
  locale?: LocaleCode;
  pendingChangesStateVersion?: number;
  toasts?: Toast[];
  stateUpdatedAt?: number;
  recovered?: boolean;
  [key: string]: unknown;
}

// Webview 持久化状态
export interface WebviewPersistedState {
  currentTopTab: TabType;
  scrollPositions?: ScrollPositions;
  scrollAnchors?: ScrollAnchors;
  autoScrollEnabled?: AutoScrollConfig;
  sessionViewStateByScope?: Record<string, PersistedSessionViewState>;
  sessionQueuedMessagesByScope?: Record<string, QueuedMessage[]>;
}

export interface PersistedSessionViewState {
  workspaceId: string | null;
  sessionId: string;
  scrollPositions?: ScrollPositions;
  scrollAnchors?: ScrollAnchors;
  autoScrollEnabled?: AutoScrollConfig;
}
