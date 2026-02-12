/**
 * Magi 核心类型定义
 * 版本: 0.3.0
 */

// ============================================
// Agent 类型与角色系统
// ============================================

// ✅ 导入并重新导出新的 AgentType 系统
import type { AgentType, WorkerSlot, AgentRole } from './types/agent-types';
import type { StandardMessage, StreamUpdate } from './protocol/message-protocol';
export type { AgentType, WorkerSlot, AgentRole };


// 任务类型（用于任务分类和 Worker 分配）
export type TaskCategory =
  | 'architecture'  // 架构设计
  | 'implement'     // 功能实现
  | 'refactor'      // 代码重构
  | 'bugfix'        // Bug 修复
  | 'debug'         // 问题排查
  | 'data_analysis' // 数据处理/分析
  | 'frontend'      // 前端开发
  | 'backend'       // 后端开发
  | 'test'          // 测试编写
  | 'document'      // 文档生成
  | 'review'        // 代码审查
  | 'simple'        // 简单任务
  | 'general';      // 通用任务

// ============================================
// 统一任务类型（从新架构导出）
// ============================================

/**
 * 导入并重新导出统一的 Task 和 SubTask 类型
 * 任务系统使用 task/types.ts 中的完整定义
 *
 * 注意：UI 层使用 TaskView/TodoItemView（从 task-view-adapter.ts）
 * 内部逻辑仍使用完整的 Task/SubTask 类型
 */
import type {
  Task,
  SubTask,
  TaskStatus,
  SubTaskStatus,
  CreateTaskParams,
  CreateSubTaskParams,
  WorkerResult,
} from './task/types';

export type {
  Task,
  SubTask,
  TaskStatus,
  SubTaskStatus,
  CreateTaskParams,
  CreateSubTaskParams,
  WorkerResult,
};

// 导出视图类型（供 UI 层使用）
import type {
  TaskView,
  TodoItemView,
  TaskViewStatus,
  SubTaskViewStatus,
} from './task/task-view-adapter';

export type {
  TaskView,
  TodoItemView,
  TaskViewStatus,
  SubTaskViewStatus,
};

// ============================================
// Session 和 Task 管理
// ============================================

/**
 * Session - 插件窗口会话
 * 打开插件窗口时创建，关闭时结束
 *
 * 注意：任务管理已迁移到 Mission 系统
 * 使用 MissionDrivenEngine.listTaskViews() 获取任务列表
 */
export interface Session {
  id: string;
  createdAt: number;
  status: SessionStatus;
  snapshots: FileSnapshot[];
  /** 对话消息历史 */
  messages?: SessionMessage[];
}

/** 会话消息 */
export interface SessionMessage {
  role: 'user' | 'assistant' | 'system';
  content: string;
  agent?: AgentType;  // ✅ 使用 AgentType
  source?: MessageSource;
  timestamp: number;
}

export type SessionStatus = 'active' | 'completed';

/**
 * FileSnapshot - 文件快照
 * 用于还原文件到修改前的状态
 */
export interface FileSnapshot {
  id: string;
  sessionId: string;
  filePath: string;
  originalContent: string;
  timestamp: number;

  // Mission 架构字段
  missionId: string;
  assignmentId: string;
  todoId: string;
  workerId: string;
  contributors?: string[];
  agentType?: AgentType;
  reason?: string;
}

/**
 * PendingChange - 待处理变更
 * 用于 UI 展示待审查的文件修改
 */
export interface PendingChange {
  filePath: string;
  snapshotId: string;
  additions: number;
  deletions: number;
  status: 'pending' | 'approved' | 'reverted';

  // Mission 架构字段
  missionId: string;
  assignmentId: string;
  todoId: string;
  workerId: string;
  contributors?: string[];
}

// ============================================
// Worker 角色系统
// ============================================

// Worker 角色定义
export interface WorkerRole {
  type: WorkerSlot;
  name: string;
  strengths: string[];
  taskAffinity: TaskCategory[];
  keywords: string[];
  priority: number;  // 1 最高，用于替代选择
}


// 预设角色配置
export const WORKER_ROLES: Record<WorkerSlot, WorkerRole> = {
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
// Worker 状态系统 (更细粒度)
// ============================================

// Worker 详细状态枚举
export enum WorkerStatusCode {
  AVAILABLE = 'AVAILABLE',           // 可用
  NOT_CONFIGURED = 'NOT_CONFIGURED', // 未配置
  AUTH_FAILED = 'AUTH_FAILED',       // 认证失败
  QUOTA_EXCEEDED = 'QUOTA_EXCEEDED', // 配额耗尽
  TIMEOUT = 'TIMEOUT',               // 响应超时
  RUNTIME_ERROR = 'RUNTIME_ERROR',   // 运行时错误
  NETWORK_ERROR = 'NETWORK_ERROR'    // 网络问题
}


// Worker 详细状态
export interface WorkerDetailedStatus {
  type: WorkerSlot;
  code: WorkerStatusCode;
  available: boolean;
  model?: string;
  provider?: string;
  error?: string;
  lastChecked?: Date;
}


// 运行模式等级
export enum OperationModeLevel {
  FULL = 3,           // 全功能：Claude + Codex + Gemini
  DUAL = 2,           // 双 Agent：Claude + 任一
  SINGLE_CLAUDE = 1,  // 单 Agent：仅 Claude (智能模式)
  SINGLE_OTHER = 0.5, // 单 Agent：仅 Codex 或 Gemini (简单模式)
  NONE = 0            // 无可用模型
}

// 韧性策略结果
export interface ResilienceStrategy {
  level: OperationModeLevel;
  availableWorkers: WorkerSlot[];
  missingWorkers: WorkerSlot[];
  hasOrchestrator: boolean;  // Claude 是否可用作编排者
  recommendation: string;
  canProceed: boolean;
  alternativeMap: Partial<Record<WorkerSlot, WorkerSlot>>;  // 替代映射
}

// Diff 块
export interface DiffHunk {
  filePath: string;
  oldStart: number;
  oldLines: number;
  newStart: number;
  newLines: number;
  content: string;
  source: AgentType;  // ✅ 使用 AgentType
}

// 冲突信息
export interface ConflictInfo {
  filePath: string;
  hunks: DiffHunk[];
  sources: AgentType[];  // ✅ 使用 AgentType
  description: string;
}

// 执行模式（Worker 层）
export type ExecutionMode = 'auto' | 'parallel' | 'sequential';

// ============================================
// 用户交互模式（Orchestrator 层）
// ============================================

/**
 * 用户交互模式
 * - ask: 对话模式，可以调用工具，但每次都需要用户授权
 * - auto: 自动模式，完全自动执行，不需要确认
 */
export type InteractionMode = 'ask' | 'auto';

export interface PermissionMatrix {
  allowEdit: boolean;
  allowBash: boolean;
  allowWeb: boolean;
}

export interface StrategyConfig {
  enableVerification: boolean;
  enableRecovery: boolean;
  autoRollbackOnFailure: boolean;
}

/**
 * 交互模式配置
 */
export interface InteractionModeConfig {
  mode: InteractionMode;
  /** 是否允许文件修改 */
  allowFileModification: boolean;
  /** 是否允许命令执行 */
  allowCommandExecution: boolean;
  /** 是否需要工具授权 */
  requireToolAuthorization: boolean;
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
    allowFileModification: true,
    allowCommandExecution: true,
    requireToolAuthorization: true,   // ✅ 需要工具授权
    requirePlanConfirmation: false,
    requireRecoveryConfirmation: false,
    autoRollbackOnFailure: true,
    maxFilesToModify: 0,
  },
  auto: {
    mode: 'auto',
    allowFileModification: true,
    allowCommandExecution: true,
    requireToolAuthorization: false,  // ❌ 不需要工具授权
    requirePlanConfirmation: false,
    requireRecoveryConfirmation: false,
    autoRollbackOnFailure: true,
    maxFilesToModify: 0,  // 可由用户配置
  },
};

// Worker 配置
export interface WorkerConfig {
  workerSlot: WorkerSlot;
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
  permissions?: PermissionMatrix;
  strategy?: StrategyConfig;
  workerSelection?: {
    enabled?: boolean;
    healthThreshold?: number;
  };
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
  | 'task:cancelled'
  | 'task:state_changed'
  | 'subtask:started'
  | 'subtask:completed'
  | 'subtask:failed'
  | 'snapshot:created'
  | 'snapshot:reverted'
  | 'snapshot:accepted'
  | 'change:approved'
  | 'change:reverted'
  | 'worker:statusChanged'
  | 'worker:healthCheck'
  | 'worker:error'
  | 'worker:session_event'
  | 'orchestrator:waiting_confirmation'
  | 'orchestrator:phase_changed'
  | 'orchestrator:mode_changed'
  | 'orchestrator:plan_ready'
  | 'orchestrator:dependency_analysis'
  | 'orchestrator:ui_message'
  | 'tool:authorization_request'
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
  currentSessionId?: string;
  sessions?: import('./session').SessionMeta[];
  currentTask?: Task;
  tasks?: Task[];
  activePlan?: { planId: string; formattedPlan: string; updatedAt: number; review?: { status: 'approved' | 'rejected' | 'skipped'; summary: string } };
  workerStatuses: WorkerStatus[];
  pendingChanges: PendingChange[];
  isRunning: boolean;
  logs: LogEntry[];
  /** 当前交互模式 */
  interactionMode: InteractionMode;
  /** 交互模式更新时间戳（用于前端时序防护） */
  interactionModeUpdatedAt?: number;
  /** 当前编排器阶段 */
  orchestratorPhase?: string;
}

/** Worker 状态（基于 LLM 适配器） */
export interface WorkerStatus {
  worker: WorkerSlot;
  available: boolean;
  enabled: boolean;
  model?: string;      // 配置的模型名称
  provider?: string;   // openai 或 anthropic
}

// 日志条目
export interface LogEntry {
  level: 'info' | 'warn' | 'error' | 'debug';
  message: string;
  source?: AgentType | 'orchestrator' | 'system';  // ✅ 使用 AgentType
  timestamp: number;
}

// ============================================
// Webview 消息通信
// ============================================

// Webview 发送到 Extension 的消息
export type WebviewToExtensionMessage =
  | { type: 'executeTask'; prompt: string; images?: Array<{ dataUrl: string }>; mode?: string; agent?: WorkerSlot | null; worker?: WorkerSlot; requestId?: string }
  | { type: 'interruptTask'; taskId?: string; silent?: boolean; reason?: string }
  | { type: 'continueTask'; taskId: string; prompt: string }
  | { type: 'startTask'; taskId: string }
  | { type: 'deleteTask'; taskId: string }
  | { type: 'login'; apiKey: string; provider?: string; remember?: boolean }
  | { type: 'logout' }
  | { type: 'getStatus' }
  | { type: 'pauseTask'; taskId: string }
  | { type: 'resumeTask'; taskId: string }
  | { type: 'appendMessage'; taskId: string; content: string }
  | { type: 'approveChange'; filePath: string }
  | { type: 'revertChange'; filePath: string }
  | { type: 'approveAllChanges' }
  | { type: 'revertAllChanges' }
  | { type: 'newSession' }
  | { type: 'saveCurrentSession'; messages: any[] }
  | { type: 'switchSession'; sessionId: string }
  | { type: 'renameSession'; sessionId: string; name: string }
  | { type: 'closeSession'; sessionId: string }
  | { type: 'deleteSession'; sessionId: string; requireConfirm?: boolean }
  | { type: 'selectWorker'; worker: WorkerSlot | null }
  | { type: 'updateSetting'; key: string; value: unknown }
  | { type: 'viewDiff'; filePath: string }
  | { type: 'openFile'; filepath: string }
  | { type: 'openLink'; url: string }
  | { type: 'confirmPlan'; confirmed: boolean }
  | { type: 'answerQuestions'; answer: string | null }
  | { type: 'getState' }
  | { type: 'requestState' }
  | { type: 'webviewReady' }
  // 新增：交互模式相关
  | { type: 'setInteractionMode'; mode: InteractionMode }
  | { type: 'confirmRecovery'; decision: 'retry' | 'rollback' | 'continue' }

  | { type: 'requestExecutionStats' }
  | { type: 'resetExecutionStats' }

  | { type: 'checkWorkerStatus'; force?: boolean }

  | { type: 'clearAllTasks' }

  // UI 错误上报
  | { type: 'uiError'; component: string; detail: string; stack?: string }
  // 工具授权响应
  | { type: 'toolAuthorizationResponse'; requestId?: string; allowed: boolean }
  // 交互响应（动态审批等）
  | { type: 'interactionResponse'; requestId: string; response: string }
  // Mermaid 图表面板
  | { type: 'openMermaidPanel'; code: string; title?: string }

  | { type: 'getPromptEnhanceConfig' }
  | { type: 'updatePromptEnhance'; config: { baseUrl: string; apiKey: string }; source?: 'auto' | 'manual' }
  | { type: 'testPromptEnhance'; baseUrl: string; apiKey: string }
  | { type: 'enhancePrompt'; prompt: string }
  // 新增：需求澄清回答
  | { type: 'answerClarification'; answers: Record<string, string> | null; additionalInfo?: string }
  // 新增：Worker 问题回答
  | { type: 'answerWorkerQuestion'; answer: string | null }
  // 新增：画像配置
  | { type: 'getProfileConfig' }
  | { type: 'saveProfileConfig'; data: { assignments: Record<string, string>; userRules?: string } }
  | { type: 'resetProfileConfig' }
  // 新增：LLM 配置相关
  | { type: 'loadAllWorkerConfigs' }
  | { type: 'saveWorkerConfig'; worker: WorkerSlot; config: any }
  | { type: 'testWorkerConnection'; worker: WorkerSlot; config: any }
  | { type: 'loadOrchestratorConfig' }
  | { type: 'saveOrchestratorConfig'; config: any }
  | { type: 'testOrchestratorConnection'; config: any }
  | { type: 'loadCompressorConfig' }
  | { type: 'saveCompressorConfig'; config: any }
  | { type: 'testCompressorConnection'; config: any }
  | { type: 'fetchModelList'; config: any; target: string }
  // 新增：MCP 配置相关
  | { type: 'loadMCPServers' }
  | { type: 'addMCPServer'; server: any }
  | { type: 'updateMCPServer'; serverId: string; updates: any }
  | { type: 'deleteMCPServer'; serverId: string }
  | { type: 'connectMCPServer'; serverId: string }
  | { type: 'disconnectMCPServer'; serverId: string }
  | { type: 'refreshMCPTools'; serverId: string }
  | { type: 'getMCPServerTools'; serverId: string }
  // 新增：Skills 配置相关
  | { type: 'loadSkillsConfig' }
  | { type: 'saveSkillsConfig'; config: any }
  | { type: 'toggleBuiltInTool'; tool: string; enabled: boolean }
  | { type: 'addCustomTool'; tool: any }
  | { type: 'removeCustomTool'; toolName: string }
  | { type: 'removeInstructionSkill'; skillName: string }
  | { type: 'installSkill'; skillId: string }
  // Skills 仓库相关
  | { type: 'loadRepositories' }
  | { type: 'addRepository'; url: string }
  | { type: 'updateRepository'; repositoryId: string; updates: any }
  | { type: 'deleteRepository'; repositoryId: string }
  | { type: 'refreshRepository'; repositoryId: string }
  | { type: 'loadSkillLibrary' }
  // 新增：项目知识相关
  | { type: 'getProjectKnowledge' }
  | { type: 'getADRs'; filter?: { status?: string } }
  | { type: 'getFAQs'; filter?: { category?: string } }
  | { type: 'searchFAQs'; keyword: string }
  | { type: 'addADR'; adr: any }
  | { type: 'updateADR'; id: string; updates: any }
  | { type: 'deleteADR'; id: string }
  | { type: 'addFAQ'; faq: any }
  | { type: 'updateFAQ'; id: string; updates: any }
  | { type: 'deleteFAQ'; id: string }
  | { type: 'clearProjectKnowledge' }
  // 新增：前端错误上报
  | { type: 'uiError'; component: string; detail?: unknown; stack?: string }
  | { type: 'toolAuthorizationResponse'; requestId: string; allowed: boolean }
  | { type: 'interactionResponse'; requestId: string; response: any }
  // 新增：Mermaid 图表
  | { type: 'openMermaidPanel'; code: string; title?: string };

// Extension 发送到 Webview 的消息
// source 字段用于区分消息来源：'orchestrator' = 编排者, 'worker' = 执行代理
export type MessageSource = 'orchestrator' | 'worker' | 'system';

export type ExtensionToWebviewMessage =
  | { type: 'unifiedMessage'; message: StandardMessage; sessionId?: string | null }
  | { type: 'unifiedUpdate'; update: StreamUpdate; sessionId?: string | null }
  | { type: 'unifiedComplete'; message: StandardMessage; sessionId?: string | null };

/** Worker 执行统计数据（用于 UI 显示） */
export interface WorkerExecutionStats {
  /** 模型标识 */
  worker: string;
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
  /** 健康评分 (0-1) */
  healthScore?: number;
  /** 总输入 token */
  totalInputTokens?: number;
  /** 总输出 token */
  totalOutputTokens?: number;
}

/** 模型目录（用于动态渲染统计卡片） */
export interface ModelCatalogEntry {
  id: string;
  label: string;
  model?: string;
  provider?: string;
  enabled?: boolean;
  role?: 'worker' | 'orchestrator' | 'compressor' | 'unknown';
}
