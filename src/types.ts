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
import type { LocaleCode } from './i18n/types';
export type { AgentType, WorkerSlot, AgentRole };


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

// Session / SessionMessage / SessionStatus 已统一到 src/session/unified-session-manager.ts
// 请使用 UnifiedSessionManager 导出的类型

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
  diff?: string;
  originalContent?: string;
  previewContent?: string;
  previewAbsolutePath?: string;
  previewCanOpenWorkspaceFile?: boolean;

  // Mission 架构字段
  missionId: string;
  assignmentId: string;
  todoId: string;
  workerId: string;
  contributors?: string[];
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



// ============================================
// 安全防护
// ============================================

/**
 * 安全防护规则类别
 */
export type SafeguardCategory =
  | 'git_history'       // Git 历史变更
  | 'git_discard'       // Git 丢弃修改
  | 'package_publish'   // 包发布
  | 'bulk_delete'       // 批量删除
  | 'custom';           // 自定义

export interface SafeguardRule {
  /** 匹配关键词（子串匹配） */
  pattern: string;
  /** 是否启用 */
  enabled: boolean;
  /** 所属类别 */
  category: SafeguardCategory;
}

export interface SafeguardConfig {
  rules: SafeguardRule[];
}

/**
 * 预置安全防护规则（默认全部启用）
 */
export const DEFAULT_SAFEGUARD_RULES: SafeguardRule[] = [
  // Git 历史变更
  { pattern: 'git commit', enabled: true, category: 'git_history' },
  { pattern: 'git push', enabled: true, category: 'git_history' },
  { pattern: 'git push --force', enabled: true, category: 'git_history' },
  // Git 丢弃修改
  { pattern: 'git restore', enabled: true, category: 'git_discard' },
  { pattern: 'git reset --hard', enabled: true, category: 'git_discard' },
  { pattern: 'git clean', enabled: true, category: 'git_discard' },
  // 包发布
  { pattern: 'npm publish', enabled: true, category: 'package_publish' },
  { pattern: 'yarn publish', enabled: true, category: 'package_publish' },
  { pattern: 'pnpm publish', enabled: true, category: 'package_publish' },
  { pattern: 'cargo publish', enabled: true, category: 'package_publish' },
  // 批量删除
  { pattern: 'rm -rf', enabled: true, category: 'bulk_delete' },
  { pattern: 'rm -r', enabled: true, category: 'bulk_delete' },
];

export const DEFAULT_SAFEGUARD_CONFIG: SafeguardConfig = {
  rules: [...DEFAULT_SAFEGUARD_RULES],
};

export interface StrategyConfig {
  enableVerification: boolean;
  enableRecovery: boolean;
  autoRollbackOnFailure: boolean;
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
  | 'task:paused'
  | 'task:cancelled'
  | 'task:state_changed'
  | 'subtask:started'
  | 'subtask:completed'
  | 'subtask:failed'
  | 'snapshot:created'
  | 'snapshot:changed'
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
  locale?: LocaleCode;
  activePlan?: { planId: string; formattedPlan: string; updatedAt: number; review?: { status: 'approved' | 'rejected' | 'skipped'; summary: string } };
  planHistory?: Array<{
    planId: string;
    sessionId: string;
    missionId?: string;
    turnId: string;
    version: number;
    mode: 'standard' | 'deep';
    status: 'draft' | 'awaiting_confirmation' | 'approved' | 'rejected' | 'executing' | 'partially_completed' | 'completed' | 'failed' | 'cancelled' | 'superseded';
    summary: string;
    createdAt: number;
    updatedAt: number;
    items: Array<{
      itemId: string;
      title: string;
      owner: string;
      status: 'pending' | 'running' | 'completed' | 'failed' | 'skipped' | 'cancelled';
      progress: number;
    }>;
  }>;
  workerStatuses: WorkerStatus[];
  pendingChanges: PendingChange[];
  isRunning: boolean;
  logs: LogEntry[];
  /** 当前编排器阶段 */
  orchestratorPhase?: string;
  /** 当前会话处理态快照（刷新/切会话恢复的唯一运行态来源） */
  processingState?: UIProcessingState | null;
  /** 状态更新时间戳（用于前端时序防护） */
  stateUpdatedAt?: number;
  /** 是否为恢复收敛后的状态更新 */
  recovered?: boolean;
}

export interface UIProcessingState {
  isProcessing: boolean;
  source: import('./protocol/message-protocol').MessageSource | null;
  agent: string | null;
  startedAt: number | null;
  pendingRequestIds: string[];
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
  | { type: 'pauseTask'; taskId: string }
  | { type: 'resumeTask'; taskId: string }
  | { type: 'appendMessage'; taskId: string; content: string }
  | { type: 'updateQueuedMessage'; queueId: string; content: string }
  | { type: 'deleteQueuedMessage'; queueId: string }
  | { type: 'approveChange'; filePath: string }
  | { type: 'revertChange'; filePath: string }
  | { type: 'approveAllChanges' }
  | { type: 'revertAllChanges' }
  | { type: 'revertMission'; missionId: string }
  | { type: 'newSession' }
  | { type: 'saveCurrentSession' }
  | { type: 'switchSession'; sessionId: string }
  | { type: 'renameSession'; sessionId: string; name: string }
  | { type: 'closeSession'; sessionId: string }
  | { type: 'deleteSession'; sessionId: string; requireConfirm?: boolean }
  | { type: 'markAllNotificationsRead' }
  | { type: 'clearAllNotifications' }
  | { type: 'removeNotification'; notificationId: string }
  | { type: 'selectWorker'; worker: WorkerSlot | null }
  | { type: 'updateSetting'; key: string; value: unknown }
  | {
      type: 'viewDiff';
      filePath: string;
      sessionId?: string;
      diff?: string;
      originalContent?: string;
      previewContent?: string;
      previewAbsolutePath?: string;
      previewCanOpenWorkspaceFile?: boolean;
    }
  | {
      type: 'openFile';
      filepath?: string;
      filePath?: string;
      sessionId?: string;
      previewContent?: string;
      previewAbsolutePath?: string;
      previewCanOpenWorkspaceFile?: boolean;
    }
  | { type: 'openLink'; url: string }
  | { type: 'getState' }
  | { type: 'requestState' }
  | { type: 'webviewReady' }
  | { type: 'confirmRecovery'; decision: 'retry' | 'rollback' | 'continue' }

  | { type: 'requestExecutionStats' }
  | { type: 'resetExecutionStats' }

  | { type: 'loadSettingsBootstrap'; force?: boolean }

  | { type: 'clearAllTasks' }

  // UI 错误上报
  | { type: 'uiError'; component: string; detail: string; stack?: string }
  // 交互响应（动态审批等）
  | { type: 'interactionResponse'; requestId: string; response: string }
  // Mermaid 图表面板
  | { type: 'openMermaidPanel'; code: string; title?: string }
  // 局域网访问信息
  | { type: 'getLanAccessInfo' }
  | { type: 'getTunnelStatus' }
  | { type: 'startTunnel' }
  | { type: 'stopTunnel' }

  | { type: 'enhancePrompt'; prompt: string }
  // 新增：需求澄清回答
  | { type: 'answerClarification'; answers: Record<string, string> | null; additionalInfo?: string }
  // 新增：Worker 问题回答
  | { type: 'answerWorkerQuestion'; answer: string | null }
  // 新增：画像配置
  | { type: 'saveProfileConfig'; data: { assignments: Record<string, string>; userRules?: string } }
  | { type: 'resetProfileConfig' }
  // 新增：LLM 配置相关
  | { type: 'saveWorkerConfig'; worker: WorkerSlot; config: any }
  | { type: 'testWorkerConnection'; worker: WorkerSlot; config: any }
  | { type: 'saveOrchestratorConfig'; config: any }
  | { type: 'testOrchestratorConnection'; config: any }
  | { type: 'saveAuxiliaryConfig'; config: any }
  | { type: 'testAuxiliaryConnection'; config: any }
  | { type: 'fetchModelList'; config: any; target: string }
  // 新增：MCP 配置相关
  | { type: 'addMCPServer'; server: any }
  | { type: 'updateMCPServer'; serverId: string; updates: any }
  | { type: 'deleteMCPServer'; serverId: string }
  | { type: 'connectMCPServer'; serverId: string }
  | { type: 'disconnectMCPServer'; serverId: string }
  | { type: 'refreshMCPTools'; serverId: string }
  | { type: 'getMCPServerTools'; serverId: string }
  // 新增：Skills 配置相关
  | { type: 'saveSkillsConfig'; config: any }
  | { type: 'toggleBuiltInTool'; tool: string; enabled: boolean }
  | { type: 'addCustomTool'; tool: any }
  | { type: 'removeCustomTool'; toolName: string }
  | { type: 'removeInstructionSkill'; skillName: string }
  | { type: 'installSkill'; skillId: string }
  | { type: 'installLocalSkill'; directoryPath?: string }
  | { type: 'updateSkill'; skillName: string }
  | { type: 'updateAllSkills' }
  // Skills 仓库相关
  | { type: 'addRepository'; url: string }
  | { type: 'updateRepository'; repositoryId: string; updates: any }
  | { type: 'deleteRepository'; repositoryId: string }
  | { type: 'refreshRepository'; repositoryId: string }
  | { type: 'loadSkillLibrary' }
  // 新增：项目知识相关
  | { type: 'getProjectKnowledge' }
  | { type: 'addADR'; adr: any }
  | { type: 'updateADR'; id: string; updates: any }
  | { type: 'deleteADR'; id: string }
  | { type: 'addFAQ'; faq: any }
  | { type: 'updateFAQ'; id: string; updates: any }
  | { type: 'deleteFAQ'; id: string }
  | { type: 'deleteLearning'; id: string }
  | { type: 'clearProjectKnowledge' }
  // 新增：前端错误上报
  | { type: 'uiError'; component: string; detail?: unknown; stack?: string }
  // 安全防护配置
  | { type: 'saveSafeguardConfig'; config: import('./types').SafeguardConfig }
  | { type: 'interactionResponse'; requestId: string; response: any }
  // 新增：Mermaid 图表
  | { type: 'openMermaidPanel'; code: string; title?: string }
  | { type: 'getLanAccessInfo' }
  // 执行链操作
  | { type: 'abandonChain'; chainId: string }
  // 宿主 API 代理：WebView 无法直接访问 localhost，所有 Agent API 请求通过宿主代理转发
  | { type: 'agentApiProxy'; requestId: string; method: string; url: string; body?: string; headers?: Record<string, string> }
  // 宿主 SSE 事件流控制
  | { type: 'agentSseSubscribe'; queryString: string; subscriptionId: string }
  | { type: 'agentSseUnsubscribe'; subscriptionId: string };

// Extension 发送到 Webview 的消息
// source 字段用于区分消息来源：'orchestrator' = 编排者, 'worker' = 执行代理
export type MessageSource = 'orchestrator' | 'worker' | 'system';

export type ExtensionToWebviewMessage =
  | { type: 'unifiedMessage'; message: StandardMessage; sessionId?: string | null }
  | { type: 'unifiedUpdate'; update: StreamUpdate; sessionId?: string | null }
  | { type: 'unifiedComplete'; message: StandardMessage; sessionId?: string | null }
  // 宿主 API 代理响应
  | { type: 'agentApiProxyResponse'; requestId: string; status: number; body: string; headers: Record<string, string> }
  // 宿主 SSE 事件转发
  | { type: 'agentSseEvent'; data: string; subscriptionId: string }
  // 宿主 SSE 连接状态
  | { type: 'agentSseStatus'; status: 'open' | 'error'; subscriptionId: string };

/** Worker 执行统计数据（用于 UI 显示） */
export interface WorkerExecutionStats {
  /** 模型标识 */
  worker: string;
  /** Provider（openai/anthropic/unknown） */
  provider?: 'openai' | 'anthropic' | 'unknown';
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
  role?: 'worker' | 'orchestrator' | 'auxiliary' | 'unknown';
}
