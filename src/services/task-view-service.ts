/**
 * TaskViewService - 任务视图 CRUD 服务
 *
 * 从 MDE 提取的任务生命周期管理逻辑。
 * 职责：Mission 的创建、查询、状态更新、删除。
 */

import { globalEventBus } from '../events';
import type { MissionStorageManager } from '../orchestrator/mission';
import type { TaskView } from '../task/task-view-adapter';

export class TaskViewService {
  constructor(
    private missionStorage: MissionStorageManager,
    private workspaceRoot: string,
  ) {}

  /**
   * 获取会话的所有任务视图
   */
  async listTaskViews(sessionId: string): Promise<TaskView[]> {
    const { missionToTaskView } = await import('../task/task-view-adapter');
    const { TodoManager } = await import('../todo');

    const missions = await this.missionStorage.listBySession(sessionId);
    const taskViews: TaskView[] = [];

    const todosByMission = new Map<string, import('../todo').UnifiedTodo[]>();

    try {
      const todoManager = new TodoManager(this.workspaceRoot);
      await todoManager.initialize();

      for (const mission of missions) {
        const todos = await todoManager.getByMission(mission.id);
        todosByMission.set(mission.id, todos);
      }
    } catch {
      // TodoManager 不可用时，使用空映射
    }

    for (const mission of missions) {
      const todos = todosByMission.get(mission.id) || [];
      taskViews.push(missionToTaskView(mission, todos));
    }

    return taskViews;
  }

  /**
   * 创建任务（Mission）
   */
  async createTaskFromPrompt(sessionId: string, prompt: string): Promise<TaskView> {
    const { missionToTaskView } = await import('../task/task-view-adapter');

    const mission = await this.missionStorage.createMission({
      sessionId,
      userPrompt: prompt,
      context: '',
    });

    return missionToTaskView(mission, []);
  }

  /**
   * 取消任务
   */
  async cancelTaskById(taskId: string): Promise<void> {
    const mission = await this.missionStorage.load(taskId);
    if (mission) {
      mission.status = 'cancelled';
      mission.updatedAt = Date.now();
      await this.missionStorage.update(mission);
    }
  }

  /**
   * 删除任务
   */
  async deleteTaskById(taskId: string): Promise<void> {
    await this.missionStorage.delete(taskId);
  }

  /**
   * 标记任务失败
   */
  async failTaskById(taskId: string, error: string): Promise<void> {
    const mission = await this.missionStorage.load(taskId);
    if (mission) {
      mission.status = 'failed';
      mission.updatedAt = Date.now();
      await this.missionStorage.update(mission);
    }
    globalEventBus.emitEvent('task:failed', { data: { taskId, error } });
  }

  /**
   * 标记任务完成
   */
  async completeTaskById(taskId: string): Promise<void> {
    const mission = await this.missionStorage.load(taskId);
    if (mission) {
      mission.status = 'completed';
      mission.completedAt = Date.now();
      mission.updatedAt = Date.now();
      await this.missionStorage.update(mission);
    }
    globalEventBus.emitEvent('task:completed', { data: { taskId } });
  }

  /**
   * 标记任务为执行中
   */
  async markTaskExecuting(taskId: string): Promise<void> {
    const mission = await this.missionStorage.load(taskId);
    if (mission) {
      mission.status = 'executing';
      mission.startedAt = Date.now();
      mission.updatedAt = Date.now();
      await this.missionStorage.update(mission);
    }
  }
}
