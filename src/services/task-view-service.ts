/**
 * TaskViewService - 任务视图 CRUD 服务
 *
 * 从 MDE 提取的任务生命周期管理逻辑。
 * 职责：Mission 的创建、查询、状态更新、删除。
 */

import { globalEventBus } from '../events';
import { logger, LogCategory } from '../logging';
import type { Mission, MissionStorageManager } from '../orchestrator/mission';
import type { TaskView } from '../task/task-view-adapter';
import type { UnifiedTodo } from '../todo';

export class TaskViewService {
  private static readonly MISSION_CONCURRENCY_WINDOW_MS = 10;

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

    const todosByMission = new Map<string, UnifiedTodo[]>();

    try {
      const todoManager = new TodoManager(this.workspaceRoot);
      await todoManager.initialize();

      for (const mission of missions) {
        const todos = await todoManager.getByMission(mission.id);
        todosByMission.set(mission.id, todos);
      }
    } catch (error) {
      logger.warn('任务视图.TodoManager.初始化失败', {
        error: error instanceof Error ? error.message : String(error),
      }, LogCategory.ORCHESTRATOR);
    }

    const duplicateArtifactMissionIds = this.collectDuplicateArtifactMissionIds(missions, todosByMission);

    for (const mission of missions) {
      if (duplicateArtifactMissionIds.has(mission.id)) {
        continue;
      }
      const todos = todosByMission.get(mission.id) || [];
      taskViews.push(missionToTaskView(mission, todos));
    }

    return taskViews;
  }

  private collectDuplicateArtifactMissionIds(
    missions: Mission[],
    todosByMission: Map<string, UnifiedTodo[]>,
  ): Set<string> {
    const artifacts = new Set<string>();
    const byPrompt = new Map<string, Mission[]>();

    for (const mission of missions) {
      const promptKey = (mission.userPrompt || '').trim();
      if (!promptKey) {
        continue;
      }
      const group = byPrompt.get(promptKey);
      if (group) {
        group.push(mission);
      } else {
        byPrompt.set(promptKey, [mission]);
      }
    }

    for (const promptMissions of byPrompt.values()) {
      if (promptMissions.length < 2) {
        continue;
      }
      const sorted = [...promptMissions].sort((a, b) => a.createdAt - b.createdAt);
      let bucket: Mission[] = [];

      const flushBucket = () => {
        if (bucket.length > 1) {
          const keeper = this.selectMissionKeeper(bucket, todosByMission);
          for (const mission of bucket) {
            if (mission.id === keeper.id) {
              continue;
            }
            if (this.isDuplicateArtifactMission(mission, todosByMission)) {
              artifacts.add(mission.id);
            }
          }
        }
        bucket = [];
      };

      for (const mission of sorted) {
        if (bucket.length === 0) {
          bucket.push(mission);
          continue;
        }
        const last = bucket[bucket.length - 1];
        if (mission.createdAt - last.createdAt <= TaskViewService.MISSION_CONCURRENCY_WINDOW_MS) {
          bucket.push(mission);
        } else {
          flushBucket();
          bucket.push(mission);
        }
      }
      flushBucket();
    }

    return artifacts;
  }

  private isDuplicateArtifactMission(
    mission: Mission,
    todosByMission: Map<string, UnifiedTodo[]>,
  ): boolean {
    const todoCount = todosByMission.get(mission.id)?.length || 0;
    const goal = (mission.goal || '').trim();
    // 空壳 mission：无 goal、无 todo，且处于无实质内容的状态（并发产物或被替代的 draft）
    const isEmptyShell = goal.length === 0 && todoCount === 0;
    return isEmptyShell && (mission.status === 'executing' || mission.status === 'cancelled');
  }

  private selectMissionKeeper(
    missions: Mission[],
    todosByMission: Map<string, UnifiedTodo[]>,
  ): Mission {
    const scoreMission = (mission: Mission): number => {
      const goalScore = (mission.goal || '').trim().length > 0 ? 3 : 0;
      const todoScore = (todosByMission.get(mission.id)?.length || 0) > 0 ? 3 : 0;
      const terminalScore = mission.status === 'completed' || mission.status === 'failed' || mission.status === 'cancelled' ? 2 : 0;
      const startedScore = mission.startedAt ? 1 : 0;
      return goalScore + todoScore + terminalScore + startedScore;
    };

    return [...missions].sort((a, b) => {
      const scoreDiff = scoreMission(b) - scoreMission(a);
      if (scoreDiff !== 0) {
        return scoreDiff;
      }
      return a.createdAt - b.createdAt;
    })[0];
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
      await this.missionStorage.transitionStatus(taskId, 'cancelled');
    }
  }

  /**
   * 暂停任务（治理门禁触发时使用）
   */
  async pauseTaskById(taskId: string, reason?: string): Promise<void> {
    const mission = await this.missionStorage.load(taskId);
    const sessionId = mission?.sessionId;
    if (mission) {
      await this.missionStorage.transitionStatus(taskId, 'paused');
    }
    globalEventBus.emitEvent('task:paused', {
      taskId,
      sessionId,
      data: { taskId, sessionId, reason },
    });
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
    const sessionId = mission?.sessionId;
    if (mission) {
      await this.missionStorage.transitionStatus(taskId, 'failed', { failureReason: error });
    }
    globalEventBus.emitEvent('task:failed', {
      taskId,
      sessionId,
      data: { taskId, sessionId, error },
    });
  }

  /**
   * 标记任务完成
   */
  async completeTaskById(taskId: string): Promise<void> {
    const mission = await this.missionStorage.load(taskId);
    const sessionId = mission?.sessionId;
    if (mission) {
      await this.missionStorage.transitionStatus(taskId, 'completed');
    }
    globalEventBus.emitEvent('task:completed', {
      taskId,
      sessionId,
      data: { taskId, sessionId },
    });
  }

  /**
   * 标记任务为执行中
   */
  async markTaskExecuting(taskId: string): Promise<void> {
    const mission = await this.missionStorage.load(taskId);
    if (mission) {
      await this.missionStorage.transitionStatus(taskId, 'executing');
    }
  }

  /**
   * 收敛执行中状态（用于启动恢复/中断后清理）
   */
  async recoverRunningState(input: {
    sessionId?: string;
    missionId?: string;
  }): Promise<{ recovered: boolean; cancelledMissionIds: string[]; cancelledTodoIds: string[] }>
  {
    const { TodoManager } = await import('../todo');
    const { sessionId, missionId } = input;
    const cancelledMissionIds: string[] = [];
    const cancelledTodoIds: string[] = [];

    const missions = sessionId ? await this.missionStorage.listBySession(sessionId) : [];
    for (const mission of missions) {
      if (missionId && mission.id !== missionId) continue;
      if (mission.status !== 'executing') continue;
      await this.missionStorage.transitionStatus(mission.id, 'cancelled');
      cancelledMissionIds.push(mission.id);
    }

    try {
      const todoManager = new TodoManager(this.workspaceRoot);
      await todoManager.initialize();
      const cancelledTodos = await todoManager.cancelByQuery({
        sessionId,
        missionId,
        status: ['running'],
      }, '任务中断，状态收敛');
      cancelledTodoIds.push(...cancelledTodos);
    } catch (error) {
      logger.warn('任务视图.收敛.TodoManager.失败', {
        error: error instanceof Error ? error.message : String(error),
        sessionId,
        missionId,
      }, LogCategory.ORCHESTRATOR);
    }

    const recovered = cancelledMissionIds.length > 0 || cancelledTodoIds.length > 0;
    return { recovered, cancelledMissionIds, cancelledTodoIds };
  }
}
