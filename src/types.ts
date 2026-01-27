/**
 * MultiCLI 核心类型定义
 * 版本: 0.3.0
 */

// ============================================
// Agent 类型与角色系统
// ============================================

// ✅ 导入并重新导出新的 AgentType 系统
import type { AgentType, WorkerSlot, AgentRole } from './types/agent-types';
export type { AgentType, WorkerSlot, AgentRole };


// 任务类型（用于任务分类和 Worker 分配）
export type TaskCategory =
  | 'architecture'  // 架构设计
  | 'implement'     // 功能实现
  | 'refactor'      // 代码重构
  | 'bugfix'        // Bug 修复
  | 'debug'         // 问题排查
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
 * 这些类型来自新的任务系统架构
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
  priority: number;  // 1 最高，用于降级时选择
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


// 降级等级
export enum DegradationLevel {
  FULL = 3,           // 全功能：Claude + Codex + Gemini
  DUAL = 2,           // 双 Agent：Claude + 任一
  SINGLE_CLAUDE = 1,  // 单 Agent：仅 Claude (智能模式)
  SINGLE_OTHER = 0.5, // 单 Agent：仅 Codex 或 Gemini (简单模式)
  NONE = 0            // 无可用模型
}

// 降级策略结果
export interface DegradationStrategy {
  level: DegradationLevel;
  availableWorkers: WorkerSlot[];
  missingWorkers: WorkerSlot[];
  hasOrchestrator: boolean;  // Claude 是否可用作编排者
  recommendation: string;
  canProceed: boolean;
  fallbackMap: Partial<Record<WorkerSlot, WorkerSlot>>;  // 降级映射
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
export interface UIChatSession extends Session {
  workerOutputs?: Record<string, any[]>;
}

export interface UIState {
  currentSessionId?: string;
  sessions?: UIChatSession[];
  currentTask?: Task;
  tasks?: Task[];
  activePlan?: { planId: string; formattedPlan: string; updatedAt: number; review?: { status: 'approved' | 'rejected' | 'skipped'; summary: string } };
  workerStatuses: WorkerStatus[];  // ✅ 使用 WorkerStatus 替代 cliStatuses
  pendingChanges: PendingChange[];
  isRunning: boolean;
  logs: LogEntry[];
  /** 当前交互模式 */
  interactionMode: InteractionMode;
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
  | { type: 'executeTask'; prompt: string; worker?: WorkerSlot }
  | { type: 'interruptTask'; taskId: string }
  | { type: 'continueTask'; taskId: string; prompt: string }
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
  | { type: 'selectWorker'; worker: WorkerSlot | null }
  | { type: 'updateSetting'; key: string; value: unknown }
  | { type: 'viewDiff'; filePath: string }
  | { type: 'openFile'; filepath: string }
  | { type: 'confirmPlan'; confirmed: boolean }
  | { type: 'answerQuestions'; answer: string | null }
  | { type: 'getState' }
  | { type: 'requestState' }
  // 新增：交互模式相关
  | { type: 'setInteractionMode'; mode: InteractionMode }
  | { type: 'confirmRecovery'; decision: 'retry' | 'rollback' | 'continue' }

  | { type: 'requestExecutionStats' }
  | { type: 'resetExecutionStats' }

  | { type: 'checkWorkerStatus' }

  | { type: 'clearAllTasks' }

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
  | { type: 'saveProfileConfig'; data: { workers: Record<string, any>; categories: Record<string, string> } }
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
  | { type: 'installSkill'; skillId: string }
  | { type: 'applyInstructionSkill'; skillName: string; args?: string; images?: Array<{ dataUrl: string }>; agent?: WorkerSlot | null }
  // 新增：Skills 仓库相关
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
  // 新增：工具授权相关
  | { type: 'toolAuthorizationResponse'; allowed: boolean };

// Extension 发送到 Webview 的消息
// source 字段用于区分消息来源：'orchestrator' = 编排者, 'worker' = 执行代理
export type MessageSource = 'orchestrator' | 'worker' | 'system';

export type ExtensionToWebviewMessage =
  | { type: 'stateUpdate'; state: UIState }
  | { type: 'taskUpdate'; task: Task }
  | { type: 'workerStatusUpdate'; statuses: Record<string, { status: string; model?: string }> }
  | { type: 'workerStatusChanged'; worker: WorkerSlot; available: boolean; model?: string }
  | { type: 'workerError'; worker: string; error: string; source?: MessageSource }
  | { type: 'streamEvent'; phase: 'chunk' | 'complete'; content?: string; error?: string; sessionId?: string | null; source?: MessageSource; worker?: WorkerSlot; append?: boolean; sentAt?: number; target?: 'thread' | 'worker' }
  | { type: 'loginSuccess' }
  | { type: 'loginError'; message: string }
  | { type: 'authStatus'; loggedIn: boolean }
  | { type: 'toast'; message: string; toastType?: 'success' | 'error' | 'warning' | 'info'; duration?: number }
  | { type: 'sessionLoaded'; session: Session }
  | { type: 'sessionCreated'; session: Session }
  | { type: 'sessionSwitched'; sessionId: string; session?: Session }
  | { type: 'sessionSummaryLoaded'; sessionId: string; summary: any }
  | { type: 'sessionsUpdated'; sessions: Session[] }
  | { type: 'showDiff'; filePath: string; diff: string }
  | { type: 'confirmationRequest'; plan: unknown; formattedPlan: string }
  | { type: 'error'; message: string }
  // 新增：编排者专用消息类型
  | { type: 'orchestratorMessage'; content: string; phase: string; taskId?: string; messageType?: string; metadata?: Record<string, unknown>; sessionId?: string | null; timestamp?: number }
  // 新增：Worker 专用消息类型
  | { type: 'workerOutput'; workerId: string; workerType: WorkerSlot; content: string; subTaskId: string }
  // 交互模式和验证相关
  | { type: 'interactionModeChanged'; mode: InteractionMode }
  | { type: 'phaseChanged'; phase: string; taskId: string; isRunning?: boolean }
  | { type: 'verificationResult'; success: boolean; summary: string; details?: string }
  | { type: 'recoveryRequest'; taskId: string; error: string; canRetry: boolean; canRollback: boolean }
  | { type: 'recoveryResult'; success: boolean; strategy: string; message: string }
  | { type: 'taskPaused'; taskId: string }
  | { type: 'taskResumed'; taskId: string }

  | { type: 'executionStatsUpdate'; stats: WorkerExecutionStats[]; orchestratorStats?: { totalTasks: number; totalSuccess: number; totalFailed: number; totalInputTokens: number; totalOutputTokens: number }; modelCatalog?: ModelCatalogEntry[] }
  | { type: 'workerFallbackNotice'; originalWorker: WorkerSlot; fallbackWorker: WorkerSlot; reason: string }

  | { type: 'workerTaskCard'; worker: WorkerSlot; taskId: string; subTaskId: string; description: string; targetFiles?: string[]; reason?: string; status: string; dispatchId?: string; sessionId?: string | null }

  | { type: 'promptEnhanceResult'; success: boolean; message: string }
  | { type: 'promptEnhanceSaved'; success: boolean; error?: string }

  | { type: 'promptEnhanced'; enhancedPrompt: string; error: string }
  // 新增：LLM 配置响应
  | { type: 'allWorkerConfigsLoaded'; configs: any }
  | { type: 'workerConfigSaved'; worker: WorkerSlot; success?: boolean; error?: string }
  | { type: 'workerConnectionTestResult'; worker: WorkerSlot; success: boolean; error?: string }
  | { type: 'orchestratorConfigLoaded'; config: any }
  | { type: 'orchestratorConfigSaved'; success?: boolean; error?: string }
  | { type: 'orchestratorConnectionTestResult'; success: boolean; error?: string }
  | { type: 'compressorConfigLoaded'; config: any }
  | { type: 'compressorConfigSaved'; success?: boolean; error?: string }
  | { type: 'compressorConnectionTestResult'; success: boolean; error?: string }
  // 新增：MCP 配置响应
  | { type: 'mcpServersLoaded'; servers: any[] }
  | { type: 'mcpServerAdded'; server: any }
  | { type: 'mcpServerUpdated'; serverId: string }
  | { type: 'mcpServerDeleted'; serverId: string }
  | { type: 'mcpServerConnected'; serverId: string; toolCount: number }
  | { type: 'mcpServerDisconnected'; serverId: string }
  | { type: 'mcpServerConnectionFailed'; serverId: string; error: string }
  | { type: 'mcpToolsRefreshed'; serverId: string; tools: any[] }
  | { type: 'mcpServerTools'; serverId: string; tools: any[] }
  // 新增：Skills 配置响应
  | { type: 'skillsConfigLoaded'; config: any }
  | { type: 'skillsConfigSaved' }
  | { type: 'profileConfigSaved'; success: boolean; error?: string }
  | { type: 'builtInToolToggled'; tool: string; enabled: boolean }
  | { type: 'customToolAdded'; tool: any }
  | { type: 'customToolRemoved'; toolName: string }
  | { type: 'skillInstalled'; skillId: string; skill: any }
  // 新增：Skills 仓库响应
  | { type: 'repositoriesLoaded'; repositories: any[] }
  | { type: 'repositoryAdded'; repository: any }
  | { type: 'repositoryUpdated'; repositoryId: string }
  | { type: 'repositoryDeleted'; repositoryId: string }
  | { type: 'repositoryRefreshed'; repositoryId: string }
  | { type: 'skillLibraryLoaded'; skills: any[] }
  // 新增：项目知识响应
  | { type: 'projectKnowledgeLoaded'; codeIndex: any; adrs: any[]; faqs: any[] }
  | { type: 'adrsLoaded'; adrs: any[] }
  | { type: 'faqsLoaded'; faqs: any[] }
  | { type: 'faqSearchResults'; results: any[] }
  | { type: 'adrAdded'; adr: any }
  | { type: 'adrUpdated'; id: string }
  | { type: 'adrDeleted'; id: string }
  | { type: 'faqAdded'; faq: any }
  | { type: 'faqUpdated'; id: string }
  | { type: 'faqDeleted'; id: string }
  // 新增：工具授权相关
  | { type: 'toolAuthorizationRequest'; toolName: string; toolArgs: any };

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
