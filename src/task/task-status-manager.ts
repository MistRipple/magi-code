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
  progress: number; // 0-100
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
export class TaskStatusManager extends EventEmitter {
  private taskProgress: Map<string, TaskProgress> = new Map();
  private subTaskProgress: Map<string, SubTaskProgress> = new Map();
  private updateInterval: NodeJS.Timeout | null = null;

  constructor() {
    super();
  }

  /** 开始跟踪任务 */
  startTask(taskId: string, totalSubTasks: number): void {
    const progress: TaskProgress = {
      taskId,
      status: 'running',
      progress: 0,
      completedSubTasks: 0,
      totalSubTasks,
      startTime: Date.now(),
      elapsedTime: 0,
    };
    this.taskProgress.set(taskId, progress);
    this.emitUpdate({ type: 'task', taskId, status: 'running', progress: 0, timestamp: Date.now() });
    this.startProgressUpdates();
  }

  /** 开始跟踪子任务 */
  startSubTask(taskId: string, subTaskId: string, cli: CLIType, description: string): void {
    const progress: SubTaskProgress = {
      subTaskId,
      taskId,
      status: 'running',
      cli,
      description,
      startTime: Date.now(),
      output: [],
    };
    this.subTaskProgress.set(subTaskId, progress);

    const taskProg = this.taskProgress.get(taskId);
    if (taskProg) {
      taskProg.currentSubTask = subTaskId;
    }

    this.emitUpdate({ type: 'subtask', taskId, subTaskId, status: 'running', timestamp: Date.now() });
  }

  /** 添加子任务输出 */
  addSubTaskOutput(subTaskId: string, output: string): void {
    const progress = this.subTaskProgress.get(subTaskId);
    if (progress) {
      progress.output.push(output);
      this.emit('output', { subTaskId, output, timestamp: Date.now() });
    }
  }

  /** 完成子任务 */
  completeSubTask(subTaskId: string, success: boolean): void {
    const progress = this.subTaskProgress.get(subTaskId);
    if (progress) {
      progress.status = success ? 'completed' : 'failed';
      progress.endTime = Date.now();

      const taskProg = this.taskProgress.get(progress.taskId);
      if (taskProg) {
        taskProg.completedSubTasks++;
        taskProg.progress = Math.round((taskProg.completedSubTasks / taskProg.totalSubTasks) * 100);
        if (taskProg.currentSubTask === subTaskId) {
          taskProg.currentSubTask = undefined;
        }
      }

      this.emitUpdate({
        type: 'subtask',
        taskId: progress.taskId,
        subTaskId,
        status: progress.status,
        timestamp: Date.now(),
      });
    }
  }

  /** 完成任务 */
  completeTask(taskId: string, status: TaskStatus): void {
    const progress = this.taskProgress.get(taskId);
    if (progress) {
      progress.status = status;
      progress.progress = status === 'completed' ? 100 : progress.progress;
      progress.elapsedTime = Date.now() - progress.startTime;
    }
    this.emitUpdate({ type: 'task', taskId, status, progress: progress?.progress, timestamp: Date.now() });
    this.stopProgressUpdates();
  }

  /** 获取任务进度 */
  getTaskProgress(taskId: string): TaskProgress | undefined {
    return this.taskProgress.get(taskId);
  }

  /** 获取子任务进度 */
  getSubTaskProgress(subTaskId: string): SubTaskProgress | undefined {
    return this.subTaskProgress.get(subTaskId);
  }

  /** 获取所有活动任务 */
  getActiveTasks(): TaskProgress[] {
    return Array.from(this.taskProgress.values()).filter(p => p.status === 'running');
  }

  /** 清理任务数据 */
  clearTask(taskId: string): void {
    this.taskProgress.delete(taskId);
    for (const [id, prog] of this.subTaskProgress) {
      if (prog.taskId === taskId) this.subTaskProgress.delete(id);
    }
  }

  private emitUpdate(update: StatusUpdate): void {
    this.emit('statusUpdate', update);
  }

  private startProgressUpdates(): void {
    if (this.updateInterval) return;
    this.updateInterval = setInterval(() => {
      for (const progress of this.taskProgress.values()) {
        if (progress.status === 'running') {
          progress.elapsedTime = Date.now() - progress.startTime;
          this.emit('progressTick', { taskId: progress.taskId, elapsed: progress.elapsedTime });
        }
      }
    }, 1000);
  }

  private stopProgressUpdates(): void {
    if (this.updateInterval) {
      clearInterval(this.updateInterval);
      this.updateInterval = null;
    }
  }

  dispose(): void {
    this.stopProgressUpdates();
    this.taskProgress.clear();
    this.subTaskProgress.clear();
    this.removeAllListeners();
  }
}

