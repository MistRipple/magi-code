/**
 * 独立编排者架构 - 核心类型定义
 *
 * 架构理念：
 * - Orchestrator Claude：专职编排，不执行任何编码任务
 * - Worker Agents：专职执行，向编排者汇报进度和结果
 */
import { CLIType } from '../../types';
/** Worker 状态 */
export type WorkerState = 'idle' | 'executing' | 'completed' | 'failed' | 'cancelled';
/** Worker 类型（包括 Worker Claude） */
export type WorkerType = CLIType;
/** Worker 信息 */
export interface WorkerInfo {
    id: string;
    type: WorkerType;
    state: WorkerState;
    currentTaskId?: string;
    lastActivity?: number;
}
/** 子任务定义 */
export interface SubTask {
    id: string;
    description: string;
    assignedWorker: WorkerType;
    reason: string;
    targetFiles?: string[];
    dependencies: string[];
    prompt: string;
    priority?: number;
}
/** 执行计划 */
export interface ExecutionPlan {
    id: string;
    analysis: string;
    isSimpleTask?: boolean;
    skipReason?: string;
    needsCollaboration: boolean;
    subTasks: SubTask[];
    executionMode: 'parallel' | 'sequential';
    summary: string;
    createdAt: number;
}
/** 执行结果 */
export interface ExecutionResult {
    workerId: string;
    workerType: WorkerType;
    taskId: string;
    subTaskId: string;
    result: string;
    success: boolean;
    duration: number;
    modifiedFiles?: string[];
    error?: string;
}
/** 消息类型 */
export type MessageType = 'task_dispatch' | 'task_cancel' | 'progress_report' | 'task_completed' | 'task_failed' | 'worker_ready' | 'orchestrator_command';
/** 基础消息结构 */
export interface BaseMessage {
    id: string;
    type: MessageType;
    timestamp: number;
    source: string;
    target?: string;
}
/** 任务分发消息 */
export interface TaskDispatchMessage extends BaseMessage {
    type: 'task_dispatch';
    payload: {
        taskId: string;
        subTask: SubTask;
        context?: string;
    };
}
/** 任务取消消息 */
export interface TaskCancelMessage extends BaseMessage {
    type: 'task_cancel';
    payload: {
        taskId: string;
        subTaskId?: string;
        reason?: string;
    };
}
/** 进度汇报消息 */
export interface ProgressReportMessage extends BaseMessage {
    type: 'progress_report';
    payload: {
        taskId: string;
        subTaskId: string;
        status: 'started' | 'in_progress' | 'completed' | 'failed';
        progress?: number;
        message?: string;
        output?: string;
    };
}
/** 任务完成消息 */
export interface TaskCompletedMessage extends BaseMessage {
    type: 'task_completed';
    payload: {
        result: ExecutionResult;
    };
}
/** 任务失败消息 */
export interface TaskFailedMessage extends BaseMessage {
    type: 'task_failed';
    payload: {
        taskId: string;
        subTaskId: string;
        error: string;
        canRetry: boolean;
    };
}
/** Worker 就绪消息 */
export interface WorkerReadyMessage extends BaseMessage {
    type: 'worker_ready';
    payload: {
        workerInfo: WorkerInfo;
    };
}
/** 编排者命令消息 */
export interface OrchestratorCommandMessage extends BaseMessage {
    type: 'orchestrator_command';
    payload: {
        command: 'pause_all' | 'resume_all' | 'cancel_all' | 'status_check';
    };
}
/** 所有消息类型联合 */
export type BusMessage = TaskDispatchMessage | TaskCancelMessage | ProgressReportMessage | TaskCompletedMessage | TaskFailedMessage | WorkerReadyMessage | OrchestratorCommandMessage;
/** 编排者状态 */
export type OrchestratorState = 'idle' | 'analyzing' | 'waiting_confirmation' | 'dispatching' | 'monitoring' | 'verifying' | 'recovering' | 'summarizing' | 'completed' | 'failed';
/** 编排者配置 */
export interface OrchestratorConfig {
    /** 超时时间（毫秒） */
    timeout: number;
    /** 最大重试次数 */
    maxRetries: number;
    /** 验证配置 */
    verification?: {
        compileCheck?: boolean;
        compileCommand?: string;
        ideCheck?: boolean;
        lintCheck?: boolean;
        lintCommand?: string;
        testCheck?: boolean;
        testCommand?: string;
        timeout?: number;
    };
}
/** 任务上下文 */
export interface TaskContext {
    taskId: string;
    userPrompt: string;
    plan?: ExecutionPlan;
    results: ExecutionResult[];
    startTime: number;
    endTime?: number;
}
/** 编排者向用户发送的消息类型 */
export type OrchestratorMessageType = 'plan_ready' | 'progress_update' | 'worker_output' | 'verification_result' | 'summary' | 'error';
/** 编排者消息（发送给前端） */
export interface OrchestratorUIMessage {
    type: OrchestratorMessageType;
    taskId: string;
    timestamp: number;
    content: string;
    metadata?: {
        phase?: OrchestratorState;
        workerId?: string;
        workerType?: WorkerType;
        subTaskId?: string;
        progress?: number;
        plan?: ExecutionPlan;
        result?: ExecutionResult;
    };
}
/** 消息总线事件 */
export interface MessageBusEvents {
    message: (message: BusMessage) => void;
    error: (error: Error) => void;
}
/** Worker 事件 */
export interface WorkerEvents {
    stateChange: (state: WorkerState) => void;
    output: (chunk: string) => void;
    progress: (progress: number, message?: string) => void;
    completed: (result: ExecutionResult) => void;
    failed: (error: string) => void;
}
/** 编排者事件 */
export interface OrchestratorEvents {
    stateChange: (state: OrchestratorState) => void;
    planReady: (plan: ExecutionPlan) => void;
    workerProgress: (workerId: string, progress: ProgressReportMessage['payload']) => void;
    taskCompleted: (result: ExecutionResult) => void;
    allCompleted: (results: ExecutionResult[]) => void;
    error: (error: Error) => void;
}
//# sourceMappingURL=types.d.ts.map