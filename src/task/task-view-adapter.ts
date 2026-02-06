/**
 * Task View Adapter - 任务视图适配器
 *
 * 职责：
 * - 将 Mission + UnifiedTodo 转换为 UI 需要的 TaskView + SubTaskView 格式
 * - 纯函数，无状态，不持久化
 *
 * 设计目标：
 * - 统一使用 Todo 系统作为数据源
 * - 统一使用 Todo 系统替代 UnifiedTaskManager + Task/SubTask 持久化
 * - UI 层通过视图适配器获取展示数据
 */

import { Mission, Assignment, MissionStatus } from '../orchestrator/mission/types';
import { UnifiedTodo, TodoStatus } from '../todo/types';
import type { WorkerSlot } from '../types/agent-types';

// ============================================================================
// 视图类型定义（UI 层使用，不持久化）
// ============================================================================

/**
 * TaskStatus - 任务状态（视图层）
 * 映射自 MissionStatus
 */
export type TaskViewStatus =
  | 'pending'      // 等待执行
  | 'running'      // 执行中
  | 'paused'       // 已暂停
  | 'completed'    // 已完成
  | 'failed'       // 失败
  | 'cancelled';   // 已取消

/**
 * SubTaskStatus - 子任务状态（视图层）
 * 映射自 TodoStatus
 */
export type SubTaskViewStatus =
  | 'pending'      // 等待执行
  | 'running'      // 执行中
  | 'paused'       // 已暂停
  | 'completed'    // 已完成
  | 'failed'       // 失败
  | 'skipped'      // 跳过
  | 'blocked'      // 被阻塞
  | 'cancelled';   // 已取消

/**
 * TaskView - 任务视图
 * 从 Mission 转换而来，供 UI 层使用
 */
export interface TaskView {
  // 基础信息
  id: string;               // = Mission.id
  sessionId: string;        // = Mission.sessionId
  prompt: string;           // = Mission.userPrompt
  goal: string;             // = Mission.goal

  // 状态
  status: TaskViewStatus;   // 映射自 MissionStatus
  priority: number;         // 默认 5

  // 子任务
  subTasks: SubTaskView[];  // 从 UnifiedTodo 聚合

  // 时间戳
  createdAt: number;
  startedAt?: number;
  completedAt?: number;
  cancelledAt?: number;

  // 进度（从子任务计算）
  progress: number;         // 0-100

  // 关联
  missionId: string;        // 原始 Mission ID
}

/**
 * SubTaskView - 子任务视图
 * 从 UnifiedTodo 转换而来，供 UI 层使用
 */
export interface SubTaskView {
  // 基础信息
  id: string;               // = UnifiedTodo.id
  taskId: string;           // = Mission.id
  description: string;      // = UnifiedTodo.content
  title?: string;           // = UnifiedTodo.content 截取

  // Worker 分配
  assignedWorker: WorkerSlot; // = UnifiedTodo.workerId
  assignmentId: string;     // = UnifiedTodo.assignmentId

  // 状态
  status: SubTaskViewStatus; // 映射自 TodoStatus
  progress: number;         // = UnifiedTodo.progress
  priority: number;         // = UnifiedTodo.priority

  // 文件跟踪
  targetFiles: string[];    // = UnifiedTodo.targetFiles
  modifiedFiles?: string[]; // = UnifiedTodo.modifiedFiles

  // 执行结果
  output: string[];         // 从 UnifiedTodo.output 提取
  error?: string;           // = UnifiedTodo.error

  // 时间戳
  startedAt?: number;
  completedAt?: number;

  // 重试
  retryCount: number;
  maxRetries: number;
}

// ============================================================================
// 状态映射函数
// ============================================================================

/**
 * 将 MissionStatus 映射为 TaskViewStatus
 */
export function mapMissionStatusToTaskViewStatus(status: MissionStatus): TaskViewStatus {
  const mapping: Record<MissionStatus, TaskViewStatus> = {
    draft: 'pending',
    planning: 'running',
    pending_review: 'paused',
    pending_approval: 'paused',
    executing: 'running',
    paused: 'paused',
    reviewing: 'running',
    completed: 'completed',
    failed: 'failed',
    cancelled: 'cancelled',
  };
  return mapping[status] || 'pending';
}

/**
 * 将 TodoStatus 映射为 SubTaskViewStatus
 */
export function mapTodoStatusToSubTaskViewStatus(status: TodoStatus): SubTaskViewStatus {
  const mapping: Record<TodoStatus, SubTaskViewStatus> = {
    pending: 'pending',
    blocked: 'blocked',
    ready: 'pending',
    running: 'running',
    completed: 'completed',
    failed: 'failed',
    skipped: 'skipped',
  };
  return mapping[status] || 'pending';
}

// ============================================================================
// 转换函数
// ============================================================================

/**
 * 将 UnifiedTodo 转换为 SubTaskView
 */
export function todoToSubTaskView(todo: UnifiedTodo, missionId: string): SubTaskView {
  return {
    id: todo.id,
    taskId: missionId,
    description: todo.content,
    title: todo.content.length > 50 ? todo.content.substring(0, 50) + '...' : todo.content,
    assignedWorker: todo.workerId,
    assignmentId: todo.assignmentId,
    status: mapTodoStatusToSubTaskViewStatus(todo.status),
    progress: todo.progress,
    priority: todo.priority,
    targetFiles: todo.targetFiles || [],
    modifiedFiles: todo.modifiedFiles,
    output: todo.output ? [todo.output.summary] : [],
    error: todo.error,
    startedAt: todo.startedAt,
    completedAt: todo.completedAt,
    retryCount: todo.retryCount,
    maxRetries: todo.maxRetries,
  };
}

/**
 * 将 Mission + UnifiedTodo[] 转换为 TaskView
 */
export function missionToTaskView(mission: Mission, todos: UnifiedTodo[]): TaskView {
  const subTasks = todos.map(todo => todoToSubTaskView(todo, mission.id));

  // 计算进度
  const completedCount = subTasks.filter(
    st => st.status === 'completed' || st.status === 'skipped'
  ).length;
  const progress = subTasks.length > 0
    ? Math.round((completedCount / subTasks.length) * 100)
    : 0;

  return {
    id: mission.id,
    sessionId: mission.sessionId,
    prompt: mission.userPrompt,
    goal: mission.goal,
    status: mapMissionStatusToTaskViewStatus(mission.status),
    priority: 5, // 默认优先级
    subTasks,
    createdAt: mission.createdAt,
    startedAt: mission.startedAt,
    completedAt: mission.completedAt,
    cancelledAt: mission.status === 'cancelled' ? mission.updatedAt : undefined,
    progress,
    missionId: mission.id,
  };
}

/**
 * 批量转换 Mission 列表为 TaskView 列表
 *
 * @param missions Mission 列表
 * @param todosByMission 按 missionId 分组的 Todo 映射
 */
export function missionsToTaskViews(
  missions: Mission[],
  todosByMission: Map<string, UnifiedTodo[]>
): TaskView[] {
  return missions.map(mission => {
    const todos = todosByMission.get(mission.id) || [];
    return missionToTaskView(mission, todos);
  });
}