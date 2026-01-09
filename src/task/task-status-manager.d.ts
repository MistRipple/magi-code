/**
 * 任务状态管理器
 * 管理任务状态的实时更新和通知
 */
import { EventEmitter } from 'events';
import { CLIType, TaskStatus, SubTaskStatus } from '../types';
/** 任务进度信息 */
export interface TaskProgress {
    taskId: string;
    status: TaskStatus;
    progress: number;
    currentSubTask?: string;
    completedSubTasks: number;
    totalSubTasks: number;
    startTime: number;
    elapsedTime: number;
    estimatedRemaining?: number;
}
/** 子任务进度信息 */
export interface SubTaskProgress {
    subTaskId: string;
    taskId: string;
    status: SubTaskStatus;
    cli: CLIType;
    description: string;
    startTime?: number;
    endTime?: number;
    output: string[];
}
/** 状态更新事件 */
export interface StatusUpdate {
    type: 'task' | 'subtask';
    taskId: string;
    subTaskId?: string;
    status: TaskStatus | SubTaskStatus;
    progress?: number;
    message?: string;
    timestamp: number;
}
/**
 * 任务状态管理器类
 */
export declare class TaskStatusManager extends EventEmitter {
    private taskProgress;
    private subTaskProgress;
    private updateInterval;
    constructor();
    /** 开始跟踪任务 */
    startTask(taskId: string, totalSubTasks: number): void;
    /** 开始跟踪子任务 */
    startSubTask(taskId: string, subTaskId: string, cli: CLIType, description: string): void;
    /** 添加子任务输出 */
    addSubTaskOutput(subTaskId: string, output: string): void;
    /** 完成子任务 */
    completeSubTask(subTaskId: string, success: boolean): void;
    /** 完成任务 */
    completeTask(taskId: string, status: TaskStatus): void;
    /** 获取任务进度 */
    getTaskProgress(taskId: string): TaskProgress | undefined;
    /** 获取子任务进度 */
    getSubTaskProgress(subTaskId: string): SubTaskProgress | undefined;
    /** 获取所有活动任务 */
    getActiveTasks(): TaskProgress[];
    /** 清理任务数据 */
    clearTask(taskId: string): void;
    private emitUpdate;
    private startProgressUpdates;
    private stopProgressUpdates;
    dispose(): void;
}
//# sourceMappingURL=task-status-manager.d.ts.map