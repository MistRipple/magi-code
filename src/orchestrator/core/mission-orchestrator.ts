/**
 * Mission Orchestrator - 任务编排核心
 *
 * 核心职责：
 * - Worker 注册与管理（ensureWorker / getWorker）
 * - Mission 事件中枢（EventEmitter：转发 Worker + Storage 事件）
 * - Todo 审批
 * - Mission ID 生命周期
 *
 * 注意：原始的 mission pipeline（processRequest → createMission → understandGoal
 * → selectParticipants → ... → verifyMission → summarizeMission）已被
 * MissionDrivenEngine 的统一执行流完全替代，相关方法已于 Wave 4.1 清理。
 */

import { EventEmitter } from 'events';
import { WorkerSlot } from '../../types';
import { ProfileLoader } from '../profile/profile-loader';
import { GuidanceInjector } from '../profile/guidance-injector';
import { MissionStorageManager, Mission } from '../mission';
import { AutonomousWorker } from '../worker';
import type { IAdapterFactory } from '../../adapters/adapter-factory-interface';
import { ContextManager } from '../../context/context-manager';
import { logger, LogCategory } from '../../logging';
import { TodoManager } from '../../todo';
import type { UnifiedTodo } from '../../todo/types';

// ============================================================================
// MissionOrchestrator 类型安全事件合约
// ============================================================================

/**
 * MissionOrchestrator 可发射的全部事件及参数类型
 *
 * 消费方：
 * - WebviewProvider.bindMissionEvents() — 驱动前端 UI 状态
 * - DispatchManager.setupMissionEventListeners() — 驱动 SubTaskCard 进度
 */
export interface MissionOrchestratorEventMap {
  // ---- Mission 生命周期 (storage 转发) ----
  missionCreated: (data: { mission: Mission }) => void;
  missionStatusChanged: (data: { mission: Mission; oldStatus: string; newStatus: string }) => void;
  missionPhaseChanged: (data: { mission: Mission; oldPhase: string; newPhase: string }) => void;
  // ---- Worker Session ----
  workerSessionCreated: (data: { sessionId: string; assignmentId: string; workerId: WorkerSlot }) => void;
  workerSessionResumed: (data: { sessionId: string; assignmentId: string; workerId: WorkerSlot; completedTodos: number }) => void;
  // ---- Todo 进度 (Worker 转发) ----
  todoStarted: (data: { assignmentId: string; todoId: string; content: string; missionId: string | null }) => void;
  todoCompleted: (data: { assignmentId: string; todoId: string; content: string; output: any; missionId: string | null }) => void;
  todoFailed: (data: { assignmentId: string; todoId: string; content: string; error: string; missionId: string | null }) => void;
  dynamicTodoAdded: (data: { assignmentId: string; todo: UnifiedTodo; missionId: string | null }) => void;
  insightGenerated: (data: { workerId: string; type: string; content: string; importance: string; missionId: string | null }) => void;
  // ---- Assignment (公开方法触发，禁止外部直接 emit) ----
  assignmentPlanned: (data: { missionId: string; assignmentId: string; todos: UnifiedTodo[]; warnings?: string[] }) => void;
  // ---- Assignment 生命周期 (Worker 转发) ----
  assignmentStarted: (data: { assignmentId: string; missionId: string | null; workerId?: WorkerSlot }) => void;
  assignmentCompleted: (data: { assignmentId: string; missionId: string | null; success: boolean; summary?: string }) => void;
  // ---- 审批 (Worker 转发) ----
  approvalRequested: (data: { todoId: string; content: string; reason: string; missionId: string | null }) => void;
}

/** 类型安全的 on/emit 声明合并 */
export interface MissionOrchestrator {
  on<K extends keyof MissionOrchestratorEventMap>(event: K, listener: MissionOrchestratorEventMap[K]): this;
  emit<K extends keyof MissionOrchestratorEventMap>(event: K, ...args: Parameters<MissionOrchestratorEventMap[K]>): boolean;
}

/**
 * MissionOrchestrator - 任务编排核心
 */
export class MissionOrchestrator extends EventEmitter {
  private storage: MissionStorageManager;
  private contextManager: ContextManager;

  // Worker 管理
  private workers: Map<WorkerSlot, AutonomousWorker> = new Map();
  private todoManager?: TodoManager;
  private currentMissionId: string | null = null;

  constructor(
    private profileLoader: ProfileLoader,
    private guidanceInjector: GuidanceInjector,
    _adapterFactory: IAdapterFactory,
    contextManager: ContextManager,
    storage?: MissionStorageManager,
    private workspaceRoot?: string,
    _snapshotManager?: unknown,
  ) {
    super();
    this.contextManager = contextManager;
    this.storage = storage || new MissionStorageManager();
    this.setupStorageListeners();
  }

  /**
   * 通知 assignmentPlanned 事件（公开方法，替代外部直接 emit）
   *
   * 由 DispatchManager 在任务规划完成后调用。
   * WVP.bindMissionEvents() 监听此事件驱动前端 Todo 面板更新。
   */
  notifyAssignmentPlanned(data: Parameters<MissionOrchestratorEventMap['assignmentPlanned']>[0]): void {
    this.emit('assignmentPlanned', data);
  }

  /**
   * 设置存储层事件监听
   */
  private setupStorageListeners(): void {
    this.storage.on('missionCreated', (data) => {
      this.emit('missionCreated', data);
    });

    this.storage.on('missionStatusChanged', (data) => {
      this.emit('missionStatusChanged', data);
    });

    this.storage.on('missionPhaseChanged', (data) => {
      this.emit('missionPhaseChanged', data);
    });
  }

  // ============================================================================
  // Worker 管理
  // ============================================================================

  /**
   * 确保 Worker 存在（懒加载创建）
   */
  private async ensureWorker(workerSlot: WorkerSlot): Promise<AutonomousWorker> {
    let worker = this.workers.get(workerSlot);
    if (!worker) {
      // 确保 TodoManager 存在
      if (!this.todoManager && this.workspaceRoot) {
        try {
          this.todoManager = new TodoManager(this.workspaceRoot);
          await this.todoManager.initialize();
          logger.info('编排器.TodoManager.已初始化', {
            workspaceRoot: this.workspaceRoot,
            forWorker: workerSlot,
          }, LogCategory.ORCHESTRATOR);
        } catch (error) {
          const errorMessage = error instanceof Error ? error.message : String(error);
          logger.error('编排器.TodoManager.初始化失败', {
            error: errorMessage,
            workspaceRoot: this.workspaceRoot,
            workerSlot,
          }, LogCategory.ORCHESTRATOR);
          throw new Error(`初始化 TodoManager 失败 (workspace: ${this.workspaceRoot}, worker: ${workerSlot}): ${errorMessage}`);
        }
      }
      if (!this.todoManager) {
        throw new Error('未配置 TodoManager，无法创建 Worker');
      }
      // 确保 ContextManager 存在以获取共享上下文依赖
      const sharedContextDeps = {
        contextAssembler: this.contextManager.getContextAssembler(),
        fileSummaryCache: this.contextManager.getFileSummaryCache(),
        sharedContextPool: this.contextManager.getSharedContextPool(),
      };
      worker = new AutonomousWorker(
        workerSlot,
        this.profileLoader,
        this.guidanceInjector,
        this.todoManager,
        sharedContextDeps
      );
      this.workers.set(workerSlot, worker);

      worker.on('sessionCreated', (data: { sessionId: string; assignmentId: string }) => {
        this.emit('workerSessionCreated', {
          ...data,
          workerId: workerSlot,
        });
      });

      worker.on('sessionResumed', (data: { sessionId: string; assignmentId: string; completedTodos: number }) => {
        this.emit('workerSessionResumed', {
          ...data,
          workerId: workerSlot,
        });
      });

      // 转发 Todo 事件，确保 UI 能实时更新子任务状态
      worker.on('todoStarted', (data) => this.emit('todoStarted', { ...data, missionId: this.currentMissionId }));
      worker.on('todoCompleted', (data) => this.emit('todoCompleted', { ...data, missionId: this.currentMissionId }));
      worker.on('todoFailed', (data) => this.emit('todoFailed', { ...data, missionId: this.currentMissionId }));
      worker.on('dynamicTodoAdded', (data) => this.emit('dynamicTodoAdded', { ...data, missionId: this.currentMissionId }));
      worker.on('insightGenerated', (data) => this.emit('insightGenerated', { ...data, missionId: this.currentMissionId }));

      // 转发 Assignment 生命周期 + 审批事件
      worker.on('assignmentStarted', (data) => this.emit('assignmentStarted', { ...data, missionId: this.currentMissionId, workerId: workerSlot }));
      worker.on('assignmentCompleted', (data) => this.emit('assignmentCompleted', { ...data, missionId: this.currentMissionId }));
      worker.on('approvalRequested', (data) => this.emit('approvalRequested', { ...data, missionId: this.currentMissionId }));

      logger.info('编排器.Worker.创建', { workerSlot }, LogCategory.ORCHESTRATOR);
    }
    return worker;
  }

  /**
   * 批准 Todo (用于动态任务审批)
   */
  async approveTodo(todoId: string): Promise<void> {
    if (!this.todoManager) {
      throw new Error('TodoManager not initialized');
    }
    await this.todoManager.approve(todoId);
  }

  /**
   * 获取 Worker
   */
  getWorker(workerType: WorkerSlot): AutonomousWorker | undefined {
    return this.workers.get(workerType);
  }

  /**
   * 确保 Worker 存在（公开接口，供 dispatch_task 使用）
   */
  async ensureWorkerForDispatch(workerSlot: WorkerSlot): Promise<AutonomousWorker> {
    return this.ensureWorker(workerSlot);
  }

  /**
   * 获取 TodoManager 实例
   */
  getTodoManager(): TodoManager | undefined {
    return this.todoManager;
  }

  /**
   * 设置当前 Mission ID（供 DispatchManager 编排模式使用）
   * 确保 Worker 转发的 Todo 事件能关联到正确的 Mission
   */
  setCurrentMissionId(missionId: string | null): void {
    this.currentMissionId = missionId;
  }

  /**
   * 销毁编排器（清理资源）
   */
  dispose(): void {
    for (const worker of this.workers.values()) {
      worker.dispose();
    }
    this.workers.clear();
    this.removeAllListeners();
    logger.info('任务编排器.销毁', undefined, LogCategory.ORCHESTRATOR);
  }
}
