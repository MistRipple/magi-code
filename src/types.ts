/**
 * MultiCLI 核心类型定义
 * 版本: 0.3.0
 */

// ============================================
// CLI 类型与角色系统
// ============================================

// CLI 类型枚举
export type CLIType = 'claude' | 'codex' | 'gemini';

// 任务类型（用于任务分类和 CLI 分配）
export type TaskCategory =
  | 'architecture'  // 架构设计
  | 'implement'     // 功能实现
  | 'refactor'      // 代码重构
  | 'bugfix'        // Bug 修复
  | 'debug'         // 问题排查
  | 'frontend'      // 前端开发
  | 'test'          // 测试编写
  | 'document'      // 文档生成
  | 'review'        // 代码审查
  | 'general';      // 通用任务

// ============================================
// Session 和 Task 管理
// ============================================

/**
 * Session - 插件窗口会话
 * 打开插件窗口时创建，关闭时结束
 */
export interface Session {
  id: string;
  createdAt: number;
  status: SessionStatus;
  tasks: Task[];
  snapshots: FileSnapshot[];
  /** 对话消息历史 */
  messages?: SessionMessage[];
}

/** 会话消息 */
export interface SessionMessage {
  role: 'user' | 'assistant' | 'system';
  content: string;
  cli?: CLIType;
  source?: MessageSource;
  timestamp: number;
}

export type SessionStatus = 'active' | 'completed';

/**
 * Task - 用户任务
 * 用户每次输入 Prompt 时由 Orchestrator 创建
 */
export interface Task {
  id: string;
  sessionId: string;
  prompt: string;
  status: TaskStatus;
  subTasks: SubTask[];
  createdAt: number;
  startedAt?: number;
  completedAt?: number;
  interruptedAt?: number;
  /** 功能契约（统一前后端约束） */
  featureContract?: string;
  /** 验收清单 */
  acceptanceCriteria?: string[];
}

export type TaskStatus =
  | 'pending'      // 等待执行
  | 'running'      // 执行中
  | 'interrupted'  // 已打断
  | 'completed'    // 已完成
  | 'failed'       // 失败
  | 'cancelled';   // 已取消

/**
 * SubTask - 子任务（统一类型定义）
 * Task 分解后的执行单元，每个 SubTask 由一个 Worker 执行
 *
 * 合并了旧架构（状态管理）和新架构（编排功能）的优点
 */
export interface SubTask {
  id: string;
  taskId: string;
  description: string;

  // Worker 分配（新架构命名）
  assignedWorker: CLIType;

  // 向后兼容别名（旧架构命名）
  assignedCli?: CLIType;

  // 任务标题（用于依赖图显示）
  title?: string;

  // 分配原因（新架构，用于解释为什么选择该 Worker）
  reason?: string;

  // 执行提示词（新架构，Worker 执行时使用的具体指令）
  prompt?: string;

  // 目标文件列表
  targetFiles: string[];

  // 依赖关系（新架构，子任务间的依赖）
  dependencies: string[];

  // 优先级（新架构，1 最高）
  priority?: number;
  /** 子任务类型（实现/集成/修复/架构） */
  kind?: 'implementation' | 'integration' | 'repair' | 'architecture';
  /** 功能分组 ID（用于跨子任务联调） */
  featureId?: string;

  // 状态管理（旧架构）
  status: SubTaskStatus;
  output: string[];
  result?: WorkerResult;
  startedAt?: number;
  completedAt?: number;
}

export type SubTaskStatus =
  | 'pending'    // 等待执行
  | 'running'    // 执行中
  | 'completed'  // 已完成
  | 'failed'     // 失败
  | 'skipped';   // 跳过

/**
 * @deprecated 使用 assignedWorker 替代
 * 为了向后兼容，保留 assignedCli 的类型别名
 */
export type WorkerType = CLIType;

/**
 * FileSnapshot - 文件快照
 * 用于还原文件到修改前的状态
 */
export interface FileSnapshot {
  id: string;
  sessionId: string;
  filePath: string;
  originalContent: string;
  lastModifiedBy: CLIType;
  lastModifiedAt: number;
  subTaskId: string;
}

/**
 * PendingChange - 待处理变更
 * 用于 UI 展示待审查的文件修改
 */
export interface PendingChange {
  filePath: string;
  snapshotId: string;
  lastModifiedBy: CLIType;
  additions: number;
  deletions: number;
  status: 'pending' | 'approved' | 'reverted';
}

// ============================================
// CLI 角色系统
// ============================================

// CLI 角色定义
export interface CLIRole {
  type: CLIType;
  name: string;
  strengths: string[];
  taskAffinity: TaskCategory[];
  keywords: string[];
  priority: number;  // 1 最高，用于降级时选择
}

// 预设角色配置
export const CLI_ROLES: Record<CLIType, CLIRole> = {
  claude: {
    type: 'claude',
    name: '架构师/编排者',
    strengths: ['整体架构搭建', '系统设计', '任务分解', '代码审查', '重构规划'],
    taskAffinity: ['architecture', 'refactor', 'review', 'general'],
    keywords: ['架构', '设计', '重构', '模块', '结构', 'refactor', 'design', 'architecture'],
    priority: 1
  },
  codex: {
    type: 'codex',
    name: '修复专家',
    strengths: ['Bug 修复', '问题排查', '性能调优', '错误处理', '代码调试'],
    taskAffinity: ['bugfix', 'debug', 'implement'],
    keywords: ['修复', 'bug', '报错', 'error', 'fix', '调试', 'debug', '性能', 'performance'],
    priority: 2
  },
  gemini: {
    type: 'gemini',
    name: '前端专家',
    strengths: ['前端 UI/UX', '组件开发', '样式处理', '交互逻辑', '响应式设计'],
    taskAffinity: ['frontend', 'implement', 'test'],
    keywords: ['前端', 'UI', '组件', '样式', 'CSS', 'React', 'Vue', 'component', 'frontend'],
    priority: 3
  }
};

// ============================================
// CLI 状态系统 (更细粒度)
// ============================================

// CLI 详细状态枚举
export enum CLIStatusCode {
  AVAILABLE = 'AVAILABLE',           // 可用
  NOT_INSTALLED = 'NOT_INSTALLED',   // 未安装
  AUTH_FAILED = 'AUTH_FAILED',       // 认证失败
  QUOTA_EXCEEDED = 'QUOTA_EXCEEDED', // 配额耗尽
  TIMEOUT = 'TIMEOUT',               // 响应超时
  RUNTIME_ERROR = 'RUNTIME_ERROR',   // 运行时错误
  NETWORK_ERROR = 'NETWORK_ERROR'    // 网络问题
}

// CLI 状态
export interface CLIStatus {
  type: CLIType;
  code: CLIStatusCode;
  available: boolean;
  version?: string;
  path: string;
  error?: string;
  lastChecked?: Date;
}

// 降级等级
export enum DegradationLevel {
  FULL = 3,           // 全功能：Claude + Codex + Gemini
  DUAL = 2,           // 双 Agent：Claude + 任一
  SINGLE_CLAUDE = 1,  // 单 Agent：仅 Claude (智能模式)
  SINGLE_OTHER = 0.5, // 单 Agent：仅 Codex 或 Gemini (简单模式)
  NONE = 0            // 无可用 CLI
}

// 降级策略结果
export interface DegradationStrategy {
  level: DegradationLevel;
  availableCLIs: CLIType[];
  missingCLIs: CLIType[];
  hasOrchestrator: boolean;  // Claude 是否可用作编排者
  recommendation: string;
  canProceed: boolean;
  fallbackMap: Partial<Record<CLIType, CLIType>>;  // 降级映射
}

// Worker 执行结果
export interface WorkerResult {
  workerId: string;
  cliType: CLIType;
  success: boolean;
  output?: string;
  diff?: string;
  error?: string;
  duration: number;
  timestamp: Date;
}

// Diff 块
export interface DiffHunk {
  filePath: string;
  oldStart: number;
  oldLines: number;
  newStart: number;
  newLines: number;
  content: string;
  source: CLIType;
}

// 冲突信息
export interface ConflictInfo {
  filePath: string;
  hunks: DiffHunk[];
  sources: CLIType[];
  description: string;
}

// 执行模式（Worker 层）
export type ExecutionMode = 'auto' | 'parallel' | 'sequential';

// ============================================
// 用户交互模式（Orchestrator 层）
// ============================================

/**
 * 用户交互模式
 * - ask: 对话模式，仅对话交流，不执行代码编辑
 * - agent: 代理模式，关键节点需要用户确认（Hard Stop）
 * - auto: 自动模式，无需确认，自动执行并回滚保护
 */
export type InteractionMode = 'ask' | 'agent' | 'auto';

/**
 * 交互模式配置
 */
export interface InteractionModeConfig {
  mode: InteractionMode;
  /** 是否允许文件修改 */
  allowFileModification: boolean;
  /** 是否允许命令执行 */
  allowCommandExecution: boolean;
  /** 是否需要 Phase 2 确认 */
  requirePlanConfirmation: boolean;
  /** 是否需要 Phase 5 恢复确认 */
  requireRecoveryConfirmation: boolean;
  /** 验证失败时是否自动回滚 */
  autoRollbackOnFailure: boolean;
  /** 最大修改文件数限制（0 表示无限制） */
  maxFilesToModify: number;
}

/**
 * 预设交互模式配置
 */
export const INTERACTION_MODE_CONFIGS: Record<InteractionMode, InteractionModeConfig> = {
  ask: {
    mode: 'ask',
    allowFileModification: false,
    allowCommandExecution: false,
    requirePlanConfirmation: false,
    requireRecoveryConfirmation: false,
    autoRollbackOnFailure: false,
    maxFilesToModify: 0,
  },
  agent: {
    mode: 'agent',
    allowFileModification: true,
    allowCommandExecution: true,
    requirePlanConfirmation: true,
    requireRecoveryConfirmation: true,
    autoRollbackOnFailure: false,
    maxFilesToModify: 0,
  },
  auto: {
    mode: 'auto',
    allowFileModification: true,
    allowCommandExecution: true,
    requirePlanConfirmation: false,
    requireRecoveryConfirmation: false,
    autoRollbackOnFailure: true,
    maxFilesToModify: 0,  // 可由用户配置
  },
};

// Worker 配置
export interface WorkerConfig {
  cliType: CLIType;
  cliPath: string;
  timeout: number;
  workingDirectory: string;
  sandbox?: 'read-only' | 'workspace-write' | 'danger-full-access';
}

// Orchestrator 配置
export interface OrchestratorConfig {
  mode: ExecutionMode;
  workers: WorkerConfig[];
  maxParallel: number;
  conflictResolution: 'ask' | 'auto-merge' | 'first-wins';
}

// ============================================
// 事件系统
// ============================================

// 事件类型
export type EventType =
  | 'session:created'
  | 'session:ended'
  | 'task:created'
  | 'task:started'
  | 'task:completed'
  | 'task:failed'
  | 'task:interrupted'
  | 'task:interrupt'
  | 'task:state_changed'
  | 'subtask:started'
  | 'subtask:output'
  | 'subtask:completed'
  | 'subtask:failed'
  | 'snapshot:created'
  | 'snapshot:reverted'
  | 'snapshot:accepted'
  | 'change:approved'
  | 'change:reverted'
  | 'cli:statusChanged'
  | 'cli:healthCheck'
  | 'cli:error'
  | 'cli:output'
  | 'cli:session_event'
  | 'orchestrator:waiting_confirmation'
  | 'orchestrator:phase_changed'
  | 'orchestrator:mode_changed'
  | 'orchestrator:plan_ready'
  | 'orchestrator:ui_message'
  | 'verification:started'
  | 'verification:completed'
  | 'recovery:started'
  | 'recovery:completed'
  | 'execution:stats_updated';

// 事件数据
export interface AppEvent {
  type: EventType;
  sessionId?: string;
  taskId?: string;
  subTaskId?: string;
  data?: unknown;
  timestamp: number;
}

// 事件监听器
export type EventListener = (event: AppEvent) => void;

// ============================================
// UI 状态
// ============================================

// UI 状态
export interface UIState {
  currentSession?: Session;
  currentSessionId?: string;
  sessions?: Session[];
  /** 聊天会话元数据列表 */
  chatSessions?: ChatSessionMeta[];
  /** 当前聊天会话 */
  currentChatSession?: ChatSession;
  currentTask?: Task;
  tasks?: Task[];
  cliStatuses: CLIStatus[];
  degradationStrategy: DegradationStrategy;
  pendingChanges: PendingChange[];
  isRunning: boolean;
  logs: LogEntry[];
  /** 当前交互模式 */
  interactionMode: InteractionMode;
  /** 当前编排器阶段 */
  orchestratorPhase?: string;
}

/** 聊天会话元数据（用于列表显示） */
export interface ChatSessionMeta {
  id: string;
  name?: string;
  messageCount: number;
  createdAt: number;
  updatedAt: number;
  preview?: string;
}

/** 聊天会话 */
export interface ChatSession {
  id: string;
  name?: string;
  messages: ChatMessage[];
  createdAt: number;
  updatedAt: number;
}

/** 聊天消息 */
export interface ChatMessage {
  id: string;
  role: 'user' | 'assistant';
  content: string;
  cli?: CLIType;
  timestamp: number;
}

// 日志条目
export interface LogEntry {
  level: 'info' | 'warn' | 'error' | 'debug';
  message: string;
  source?: CLIType | 'orchestrator' | 'system';
  timestamp: number;
}

// ============================================
// Webview 消息通信
// ============================================

// Webview 发送到 Extension 的消息
export type WebviewToExtensionMessage =
  | { type: 'executeTask'; prompt: string; cli?: CLIType }
  | { type: 'interruptTask'; taskId: string }
  | { type: 'continueTask'; taskId: string; prompt: string }
  | { type: 'login'; apiKey: string; provider?: string; remember?: boolean }
  | { type: 'logout' }
  | { type: 'getStatus' }
  | { type: 'pauseTask'; taskId: string }  // 🆕 暂停任务
  | { type: 'resumeTask'; taskId: string }  // 🆕 恢复任务
  | { type: 'appendMessage'; taskId: string; content: string }  // 🆕 补充内容
  | { type: 'approveChange'; filePath: string }
  | { type: 'revertChange'; filePath: string }
  | { type: 'approveAllChanges' }
  | { type: 'revertAllChanges' }
  | { type: 'newSession' }
  | { type: 'saveCurrentSession'; messages: any[]; cliOutputs: Record<string, any[]> }
  | { type: 'switchSession'; sessionId: string }
  | { type: 'renameSession'; sessionId: string; name: string }
  | { type: 'closeSession'; sessionId: string }
  | { type: 'selectCli'; cli: CLIType | null }
  | { type: 'updateSetting'; key: string; value: unknown }
  | { type: 'viewDiff'; filePath: string }
  | { type: 'confirmPlan'; confirmed: boolean }
  | { type: 'answerQuestions'; answer: string | null }
  | { type: 'getState' }
  // 新增：交互模式相关
  | { type: 'setInteractionMode'; mode: InteractionMode }
  | { type: 'confirmRecovery'; decision: 'retry' | 'rollback' | 'continue' }
  // 🆕 执行统计相关
  | { type: 'requestExecutionStats' }
  | { type: 'resetExecutionStats' }
  // 🆕 CLI 连接状态检测
  | { type: 'checkCliStatus' }
  // 🆕 清理所有任务
  | { type: 'clearAllTasks' }
  // 🆕 Prompt 增强配置
  | { type: 'getPromptEnhanceConfig' }
  | { type: 'updatePromptEnhance'; config: { baseUrl: string; apiKey: string } }
  | { type: 'testPromptEnhance'; baseUrl: string; apiKey: string }
  | { type: 'enhancePrompt'; prompt: string };

// Extension 发送到 Webview 的消息
// source 字段用于区分消息来源：'orchestrator' = 编排者, 'worker' = 执行代理
export type MessageSource = 'orchestrator' | 'worker' | 'system';

export type ExtensionToWebviewMessage =
  | { type: 'stateUpdate'; state: UIState }
  | { type: 'taskUpdate'; task: Task }
  | { type: 'subTaskOutput'; subTaskId: string; output: string; cliType?: CLIType; sessionId?: string | null; source?: MessageSource }
  | { type: 'cliStatusUpdate'; statuses: Record<string, { status: string; version?: string }> }
  | { type: 'cliStatusChanged'; cli: string; available: boolean; version?: string }
  | { type: 'cliError'; cli: string; error: string; source?: MessageSource }
  | { type: 'cliResponse'; cli: CLIType; content: string; error?: string; sessionId?: string | null; source?: MessageSource }
  | { type: 'streamingUpdate'; content: string; sessionId?: string | null; source?: MessageSource; cli?: CLIType }
  | { type: 'streamingComplete'; content?: string; error?: string; sessionId?: string | null; source?: MessageSource; cli?: CLIType }
  | { type: 'loginSuccess' }
  | { type: 'loginError'; message: string }
  | { type: 'authStatus'; loggedIn: boolean }
  | { type: 'toast'; message: string; toastType?: 'success' | 'error' | 'warning' | 'info'; duration?: number }
  | { type: 'sessionCreated'; session: Session }
  | { type: 'sessionSwitched'; sessionId: string }
  | { type: 'sessionsUpdated'; sessions: Session[] }
  | { type: 'showDiff'; filePath: string; diff: string }
  | { type: 'confirmationRequest'; plan: unknown; formattedPlan: string }
  | { type: 'error'; message: string }
  // 新增：编排者专用消息类型
  | { type: 'orchestratorMessage'; content: string; phase: string; taskId?: string; messageType?: string; metadata?: Record<string, unknown> }
  // 新增：Worker 专用消息类型
  | { type: 'workerOutput'; workerId: string; workerType: CLIType; content: string; subTaskId: string }
  // 交互模式和验证相关
  | { type: 'interactionModeChanged'; mode: InteractionMode }
  | { type: 'phaseChanged'; phase: string; taskId: string; isRunning?: boolean }
  | { type: 'verificationResult'; success: boolean; summary: string; details?: string }
  | { type: 'recoveryRequest'; taskId: string; error: string; canRetry: boolean; canRollback: boolean }
  | { type: 'recoveryResult'; success: boolean; strategy: string; message: string }
  | { type: 'taskPaused'; taskId: string }
  | { type: 'taskResumed'; taskId: string }
  // 🆕 执行统计相关消息
  | { type: 'executionStatsUpdate'; stats: CLIExecutionStats[]; orchestratorStats?: { totalTasks: number; totalSuccess: number; totalFailed: number; totalInputTokens: number; totalOutputTokens: number } }
  | { type: 'cliFallbackNotice'; originalCli: CLIType; fallbackCli: CLIType; reason: string }
  // 🆕 Prompt 增强测试结果
  | { type: 'promptEnhanceResult'; success: boolean; message: string }
  // 🆕 Prompt 增强结果
  | { type: 'promptEnhanced'; enhancedPrompt: string; error: string };

/** 🆕 CLI 执行统计数据（用于 UI 显示） */
export interface CLIExecutionStats {
  /** CLI 类型 */
  cli: CLIType;
  /** 总执行次数 */
  totalExecutions: number;
  /** 成功次数 */
  successCount: number;
  /** 失败次数 */
  failureCount: number;
  /** 成功率 (0-1) */
  successRate: number;
  /** 平均执行时间 (ms) */
  avgDuration: number;
  /** 是否健康 */
  isHealthy: boolean;
  /** 最近错误（如果有） */
  lastError?: string;
  /** 最后执行时间 */
  lastExecutionTime?: number;
}
