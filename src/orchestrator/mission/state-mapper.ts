/**
 * MissionStateMapper - Mission 状态映射器
 *
 * 设计原则（来自 orchestration-unified-design.md 第 7.3 节）：
 * - Mission 是唯一真实状态源
 * - UI 状态由 Mission 派生，不独立维护
 * - 禁止冗余状态系统
 *
 * 状态映射：
 * Mission -> TaskView (UI 展示)
 * Assignment -> SubTaskView (子任务卡片)
 * WorkerTodo -> TodoView (Todo 列表)
 */

import type { WorkerSlot } from '../../types';
import type {
  Mission,
  MissionStatus,
  MissionPhase,
  Assignment,
  AssignmentStatus,
  WorkerTodo,
  TodoStatus,
} from './types';

/**
 * UI 任务状态
 */
export type TaskViewStatus = 'pending' | 'running' | 'completed' | 'failed' | 'cancelled' | 'paused';

/**
 * UI 子任务状态
 */
export type SubTaskViewStatus = 'pending' | 'planning' | 'running' | 'blocked' | 'completed' | 'failed';

/**
 * UI Todo 状态
 */
export type TodoViewStatus = 'pending' | 'blocked' | 'ready' | 'running' | 'completed' | 'failed' | 'skipped';

/**
 * TaskView - UI 任务视图
 * 由 Mission 派生
 */
export interface TaskView {
  id: string;
  title: string;
  description: string;
  status: TaskViewStatus;
  phase: string;
  progress: number;
  subTasks: SubTaskView[];
  createdAt: number;
  startedAt?: number;
  completedAt?: number;
  error?: string;
}

/**
 * SubTaskView - UI 子任务视图
 * 由 Assignment 派生
 */
export interface SubTaskView {
  id: string;
  title: string;
  worker: WorkerSlot;
  status: SubTaskViewStatus;
  progress: number;
  todos: TodoView[];
  summary?: string;
  modifiedFiles?: string[];
  createdFiles?: string[];
  duration?: number;
}

/**
 * TodoView - UI Todo 视图
 * 由 WorkerTodo 派生
 */
export interface TodoView {
  id: string;
  content: string;
  status: TodoViewStatus;
  type: string;
  priority: number;
  output?: string;
  error?: string;
}

/**
 * 状态变化回调
 */
export type StateChangeCallback = (taskView: TaskView) => void;

/**
 * MissionStateMapper - 状态映射器
 *
 * 负责将 Mission 数据模型映射为 UI 可用的 TaskView
 * 确保 UI 状态始终与 Mission 保持同步
 */
export class MissionStateMapper {
  private callbacks: Set<StateChangeCallback> = new Set();

  /**
   * 订阅状态变化
   */
  subscribe(callback: StateChangeCallback): () => void {
    this.callbacks.add(callback);
    return () => {
      this.callbacks.delete(callback);
    };
  }

  /**
   * 通知状态变化
   */
  private notify(taskView: TaskView): void {
    this.callbacks.forEach(cb => cb(taskView));
  }

  /**
   * 将 Mission 映射为 TaskView
   */
  mapMissionToTaskView(mission: Mission): TaskView {
    const subTasks = mission.assignments.map(a => this.mapAssignmentToSubTaskView(a));
    const progress = this.calculateMissionProgress(mission);

    const taskView: TaskView = {
      id: mission.id,
      title: mission.goal,
      description: mission.analysis || mission.userPrompt,
      status: this.mapMissionStatus(mission.status),
      phase: this.formatPhase(mission.phase),
      progress,
      subTasks,
      createdAt: mission.createdAt,
      startedAt: mission.startedAt,
      completedAt: mission.completedAt,
    };

    return taskView;
  }

  /**
   * 将 Assignment 映射为 SubTaskView
   */
  mapAssignmentToSubTaskView(assignment: Assignment): SubTaskView {
    const todos = assignment.todos.map(t => this.mapTodoToTodoView(t));

    // 计算摘要（基于完成的 Todo）
    const completedTodos = assignment.todos.filter(t => t.status === 'completed');
    const modifiedFiles = new Set<string>();
    const createdFiles = new Set<string>();

    completedTodos.forEach(todo => {
      if (todo.output) {
        todo.output.modifiedFiles?.forEach(f => {
          if (f.includes('created') || f.includes('new')) {
            createdFiles.add(f);
          } else {
            modifiedFiles.add(f);
          }
        });
      }
    });

    // 计算持续时间
    let duration: number | undefined;
    if (assignment.startedAt && assignment.completedAt) {
      duration = assignment.completedAt - assignment.startedAt;
    }

    const subTaskView: SubTaskView = {
      id: assignment.id,
      title: assignment.shortTitle || assignment.responsibility,
      worker: assignment.workerId,
      status: this.mapAssignmentStatus(assignment.status),
      progress: assignment.progress,
      todos,
      summary: this.generateAssignmentSummary(assignment),
      modifiedFiles: Array.from(modifiedFiles),
      createdFiles: Array.from(createdFiles),
      duration,
    };

    return subTaskView;
  }

  /**
   * 将 WorkerTodo 映射为 TodoView
   */
  mapTodoToTodoView(todo: WorkerTodo): TodoView {
    return {
      id: todo.id,
      content: todo.content,
      status: this.mapTodoStatus(todo.status),
      type: todo.type,
      priority: todo.priority,
      output: todo.output?.summary,
      error: todo.output?.error,
    };
  }

  /**
   * 映射 Mission 状态到 UI 状态
   */
  mapMissionStatus(status: MissionStatus): TaskViewStatus {
    const statusMap: Record<MissionStatus, TaskViewStatus> = {
      'draft': 'pending',
      'planning': 'running',
      'pending_review': 'running',
      'pending_approval': 'pending',
      'executing': 'running',
      'paused': 'paused',
      'reviewing': 'running',
      'completed': 'completed',
      'failed': 'failed',
      'cancelled': 'cancelled',
    };
    return statusMap[status] || 'pending';
  }

  /**
   * 映射 Assignment 状态到 UI 状态
   */
  mapAssignmentStatus(status: AssignmentStatus): SubTaskViewStatus {
    const statusMap: Record<AssignmentStatus, SubTaskViewStatus> = {
      'pending': 'pending',
      'planning': 'planning',
      'ready': 'pending',
      'executing': 'running',
      'blocked': 'blocked',
      'completed': 'completed',
      'failed': 'failed',
    };
    return statusMap[status] || 'pending';
  }

  /**
   * 映射 Todo 状态到 UI 状态
   */
  mapTodoStatus(status: TodoStatus): TodoViewStatus {
    const statusMap: Record<TodoStatus, TodoViewStatus> = {
      'pending': 'pending',
      'blocked': 'blocked',
      'ready': 'ready',
      'running': 'running',
      'completed': 'completed',
      'failed': 'failed',
      'skipped': 'skipped',
    };
    return statusMap[status] || 'pending';
  }

  /**
   * 格式化阶段名称为用户可读格式
   */
  formatPhase(phase: MissionPhase): string {
    const phaseMap: Record<MissionPhase, string> = {
      'goal_understanding': '理解目标',
      'participant_selection': '选择参与者',
      'contract_definition': '定义契约',
      'responsibility_assignment': '分配职责',
      'worker_planning': 'Worker 规划',
      'plan_review': '规划审查',
      'execution': '执行中',
      'verification': '验收中',
      'summary': '生成总结',
    };
    return phaseMap[phase] || phase;
  }

  /**
   * 计算 Mission 进度（0-100）
   */
  calculateMissionProgress(mission: Mission): number {
    if (mission.status === 'completed') return 100;
    if (mission.status === 'failed' || mission.status === 'cancelled') return 0;
    if (mission.assignments.length === 0) return 0;

    // 基于 Assignment 进度加权计算
    const totalProgress = mission.assignments.reduce((sum, a) => sum + a.progress, 0);
    return Math.round(totalProgress / mission.assignments.length);
  }

  /**
   * 生成 Assignment 摘要
   */
  private generateAssignmentSummary(assignment: Assignment): string {
    const completedCount = assignment.todos.filter(t => t.status === 'completed').length;
    const totalCount = assignment.todos.length;

    if (assignment.status === 'completed') {
      const outputs = assignment.todos
        .filter(t => t.output?.summary)
        .map(t => t.output!.summary)
        .slice(0, 3);

      if (outputs.length > 0) {
        return outputs.join('; ');
      }
      return `完成 ${completedCount}/${totalCount} 个任务`;
    }

    if (assignment.status === 'failed') {
      const failedTodo = assignment.todos.find(t => t.status === 'failed');
      return failedTodo?.output?.error || '执行失败';
    }

    if (assignment.status === 'executing') {
      const runningTodo = assignment.todos.find(t => t.status === 'running');
      return runningTodo?.content || `进行中 (${completedCount}/${totalCount})`;
    }

    return `${completedCount}/${totalCount} 个任务`;
  }

  /**
   * 处理 Mission 更新，触发状态变化通知
   */
  handleMissionUpdate(mission: Mission): TaskView {
    const taskView = this.mapMissionToTaskView(mission);
    this.notify(taskView);
    return taskView;
  }

  /**
   * 批量映射多个 Mission
   */
  mapMissions(missions: Mission[]): TaskView[] {
    return missions.map(m => this.mapMissionToTaskView(m));
  }

  /**
   * 销毁映射器
   */
  dispose(): void {
    this.callbacks.clear();
  }
}

/**
 * 全局状态映射器实例
 */
export const globalMissionStateMapper = new MissionStateMapper();
