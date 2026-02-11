/**
 * MissionStateMapper - Mission 状态映射器
 *
 * 职责：
 * - Assignment -> AssignmentView (子任务卡片)
 * - UnifiedTodo -> TodoView (Todo 列表)
 *
 * 设计原则：
 * - Mission 是唯一真实状态源
 * - UI 状态由 Mission 派生，不独立维护
 * - TaskView 由 task-view-adapter.ts 统一定义（本文件不再重复）
 */

import type { WorkerSlot } from '../../types';
import type { UnifiedTodo } from '../../todo/types';
import type {
  Assignment,
  AssignmentStatus,
  TodoStatus,
} from './types';

/**
 * UI 子任务状态
 */
type SubTaskViewStatus = 'pending' | 'planning' | 'running' | 'blocked' | 'completed' | 'failed';

/**
 * UI Todo 状态
 */
type TodoViewStatus = 'pending' | 'blocked' | 'ready' | 'running' | 'completed' | 'failed' | 'skipped';

/**
 * AssignmentView - UI 子任务视图
 * 由 Assignment 派生
 */
export interface AssignmentView {
  id: string;
  title: string;
  worker: WorkerSlot;
  status: SubTaskViewStatus;
  progress: number;
  todos: TodoView[];
  summary?: string;
  error?: string;
  modifiedFiles?: string[];
  createdFiles?: string[];
  duration?: number;
}

/**
 * TodoView - UI Todo 视图
 * 由 UnifiedTodo 派生
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
 * MissionStateMapper - 状态映射器
 *
 * 负责将 Assignment/Todo 映射为 UI 可用的视图
 */
export class MissionStateMapper {
  /**
   * 将 Assignment 映射为 AssignmentView
   */
  mapAssignmentToAssignmentView(assignment: Assignment): AssignmentView {
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

    const subTaskView: AssignmentView = {
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
   * 将 UnifiedTodo 映射为 TodoView
   */
  mapTodoToTodoView(todo: UnifiedTodo): TodoView {
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
   * 生成 Assignment 摘要
   */
  generateAssignmentSummary(assignment: Assignment): string {
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
}
