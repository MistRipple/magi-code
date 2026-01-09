/**
 * CLI Arranger 核心类型定义
 * 版本: 0.3.0
 */
export type CLIType = 'claude' | 'codex' | 'gemini';
export type TaskCategory = 'architecture' | 'implement' | 'refactor' | 'bugfix' | 'debug' | 'frontend' | 'test' | 'document' | 'review' | 'general';
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
    /** 各 CLI 的会话 ID，用于继续对话 */
    cliSessionIds?: {
        claude?: string;
        codex?: string;
        gemini?: string;
    };
    /** 对话消息历史 */
    messages?: SessionMessage[];
}
/** 会话消息 */
export interface SessionMessage {
    role: 'user' | 'assistant' | 'system';
    content: string;
    cli?: CLIType;
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
}
export type TaskStatus = 'pending' | 'running' | 'interrupted' | 'completed' | 'failed' | 'cancelled';
/**
 * SubTask - 子任务
 * Task 分解后的执行单元，每个 SubTask 由一个 CLI 执行
 */
export interface SubTask {
    id: string;
    taskId: string;
    description: string;
    category: TaskCategory;
    assignedCli: CLIType;
    targetFiles: string[];
    status: SubTaskStatus;
    output: string[];
    result?: WorkerResult;
    startedAt?: number;
    completedAt?: number;
}
export type SubTaskStatus = 'pending' | 'running' | 'completed' | 'failed' | 'skipped';
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
export interface CLIRole {
    type: CLIType;
    name: string;
    strengths: string[];
    taskAffinity: TaskCategory[];
    keywords: string[];
    priority: number;
}
export declare const CLI_ROLES: Record<CLIType, CLIRole>;
export declare enum CLIStatusCode {
    AVAILABLE = "AVAILABLE",// 可用
    NOT_INSTALLED = "NOT_INSTALLED",// 未安装
    AUTH_FAILED = "AUTH_FAILED",// 认证失败
    QUOTA_EXCEEDED = "QUOTA_EXCEEDED",// 配额耗尽
    TIMEOUT = "TIMEOUT",// 响应超时
    RUNTIME_ERROR = "RUNTIME_ERROR",// 运行时错误
    NETWORK_ERROR = "NETWORK_ERROR"
}
export interface CLIStatus {
    type: CLIType;
    code: CLIStatusCode;
    available: boolean;
    version?: string;
    path: string;
    error?: string;
    lastChecked?: Date;
}
export declare enum DegradationLevel {
    FULL = 3,// 全功能：Claude + Codex + Gemini
    DUAL = 2,// 双 Agent：Claude + 任一
    SINGLE_CLAUDE = 1,// 单 Agent：仅 Claude (智能模式)
    SINGLE_OTHER = 0.5,// 单 Agent：仅 Codex 或 Gemini (简单模式)
    NONE = 0
}
export interface DegradationStrategy {
    level: DegradationLevel;
    availableCLIs: CLIType[];
    missingCLIs: CLIType[];
    hasOrchestrator: boolean;
    recommendation: string;
    canProceed: boolean;
    fallbackMap: Partial<Record<CLIType, CLIType>>;
}
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
export interface DiffHunk {
    filePath: string;
    oldStart: number;
    oldLines: number;
    newStart: number;
    newLines: number;
    content: string;
    source: CLIType;
}
export interface ConflictInfo {
    filePath: string;
    hunks: DiffHunk[];
    sources: CLIType[];
    description: string;
}
export type ExecutionMode = 'auto' | 'parallel' | 'sequential';
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
export declare const INTERACTION_MODE_CONFIGS: Record<InteractionMode, InteractionModeConfig>;
export interface WorkerConfig {
    cliType: CLIType;
    cliPath: string;
    timeout: number;
    workingDirectory: string;
    sandbox?: 'read-only' | 'workspace-write' | 'danger-full-access';
}
export interface OrchestratorConfig {
    mode: ExecutionMode;
    workers: WorkerConfig[];
    maxParallel: number;
    conflictResolution: 'ask' | 'auto-merge' | 'first-wins';
}
export type EventType = 'session:created' | 'session:ended' | 'task:created' | 'task:started' | 'task:completed' | 'task:failed' | 'task:interrupted' | 'task:interrupt' | 'task:state_changed' | 'subtask:started' | 'subtask:output' | 'subtask:completed' | 'subtask:failed' | 'snapshot:created' | 'snapshot:reverted' | 'snapshot:accepted' | 'change:approved' | 'change:reverted' | 'cli:statusChanged' | 'cli:healthCheck' | 'cli:error' | 'cli:output' | 'orchestrator:waiting_confirmation' | 'orchestrator:phase_changed' | 'orchestrator:mode_changed' | 'orchestrator:plan_ready' | 'orchestrator:ui_message' | 'verification:started' | 'verification:completed' | 'recovery:started' | 'recovery:completed';
export interface AppEvent {
    type: EventType;
    sessionId?: string;
    taskId?: string;
    subTaskId?: string;
    data?: unknown;
    timestamp: number;
}
export type EventListener = (event: AppEvent) => void;
export interface UIState {
    currentSession?: Session;
    currentSessionId?: string;
    sessions?: Session[];
    /** 聊天会话元数据列表 */
    chatSessions?: ChatSessionMeta[];
    /** 当前聊天会话 */
    currentChatSession?: ChatSession;
    currentTask?: Task;
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
    cliSessionIds?: Partial<Record<CLIType, string>>;
}
/** 聊天消息 */
export interface ChatMessage {
    id: string;
    role: 'user' | 'assistant';
    content: string;
    cli?: CLIType;
    timestamp: number;
}
export interface LogEntry {
    level: 'info' | 'warn' | 'error' | 'debug';
    message: string;
    source?: CLIType | 'orchestrator' | 'system';
    timestamp: number;
}
export type WebviewToExtensionMessage = {
    type: 'executeTask';
    prompt: string;
    cli?: CLIType;
} | {
    type: 'interruptTask';
    taskId: string;
} | {
    type: 'continueTask';
    taskId: string;
    prompt: string;
} | {
    type: 'pauseTask';
    taskId: string;
} | {
    type: 'resumeTask';
    taskId: string;
} | {
    type: 'appendMessage';
    taskId: string;
    content: string;
} | {
    type: 'approveChange';
    filePath: string;
} | {
    type: 'revertChange';
    filePath: string;
} | {
    type: 'approveAllChanges';
} | {
    type: 'revertAllChanges';
} | {
    type: 'newSession';
} | {
    type: 'saveCurrentSession';
    messages: any[];
    cliOutputs: Record<string, any[]>;
} | {
    type: 'switchSession';
    sessionId: string;
} | {
    type: 'renameSession';
    sessionId: string;
    name: string;
} | {
    type: 'closeSession';
    sessionId: string;
} | {
    type: 'selectCli';
    cli: CLIType | null;
} | {
    type: 'updateSetting';
    key: string;
    value: unknown;
} | {
    type: 'viewDiff';
    filePath: string;
} | {
    type: 'confirmPlan';
    confirmed: boolean;
} | {
    type: 'getState';
} | {
    type: 'setInteractionMode';
    mode: InteractionMode;
} | {
    type: 'confirmRecovery';
    decision: 'retry' | 'rollback' | 'continue';
};
export type ExtensionToWebviewMessage = {
    type: 'stateUpdate';
    state: UIState;
} | {
    type: 'taskUpdate';
    task: Task;
} | {
    type: 'subTaskOutput';
    subTaskId: string;
    output: string;
    cliType?: CLIType;
    sessionId?: string | null;
} | {
    type: 'cliStatusUpdate';
    statuses: CLIStatus[];
} | {
    type: 'cliStatusChanged';
    cli: string;
    available: boolean;
    version?: string;
} | {
    type: 'cliError';
    cli: string;
    error: string;
} | {
    type: 'cliResponse';
    cli: CLIType;
    content: string;
    error?: string;
    sessionId?: string | null;
} | {
    type: 'streamingUpdate';
    content: string;
    sessionId?: string | null;
} | {
    type: 'toast';
    message: string;
    toastType?: 'success' | 'error' | 'warning' | 'info';
    duration?: number;
} | {
    type: 'sessionCreated';
    session: Session;
} | {
    type: 'sessionSwitched';
    sessionId: string;
} | {
    type: 'sessionsUpdated';
    sessions: Session[];
} | {
    type: 'showDiff';
    filePath: string;
    diff: string;
} | {
    type: 'confirmationRequest';
    plan: unknown;
    formattedPlan: string;
} | {
    type: 'error';
    message: string;
} | {
    type: 'interactionModeChanged';
    mode: InteractionMode;
} | {
    type: 'phaseChanged';
    phase: string;
    taskId: string;
    isRunning?: boolean;
} | {
    type: 'verificationResult';
    success: boolean;
    summary: string;
    details?: string;
} | {
    type: 'recoveryRequest';
    taskId: string;
    error: string;
    canRetry: boolean;
    canRollback: boolean;
} | {
    type: 'recoveryResult';
    success: boolean;
    strategy: string;
    message: string;
} | {
    type: 'taskPaused';
    taskId: string;
} | {
    type: 'taskResumed';
    taskId: string;
};
//# sourceMappingURL=types.d.ts.map