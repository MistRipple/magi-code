/**
 * Mission Executor - 任务执行器
 *
 * 核心职责：
 * - 执行 Mission 中的所有 Assignment
 * - 协调 Worker 之间的执行顺序
 * - 管理契约验证
 * - 处理执行过程中的错误
 */

import { EventEmitter } from 'events';
import { WorkerSlot } from '../../types';
import { IAdapterFactory } from '../../adapters/adapter-factory-interface';
import { TokenUsage } from '../../types/agent-types';
import { ProfileLoader } from '../profile/profile-loader';
import { GuidanceInjector } from '../profile/guidance-injector';
import { ProfileAwareReviewer } from '../review/profile-aware-reviewer';
import { AutonomousWorker, AutonomousExecutionResult } from '../worker';
import {
  Mission,
  Assignment,
  WorkerTodo,
} from '../mission';
import { MissionOrchestrator } from './mission-orchestrator';
import { SnapshotManager } from '../../snapshot-manager';
import { UnifiedTaskManager } from '../../task/unified-task-manager';
import { PlanTodoManager } from '../plan-todo';
import { logger, LogCategory } from '../../logging';

/**
 * 阻塞项类型
 */
export type BlockedItemType = 'assignment' | 'todo';

/**
 * 阻塞原因
 */
export interface BlockingReason {
  /** 阻塞类型 */
  type: 'contract_pending' | 'dependency_incomplete' | 'resource_conflict' | 'approval_required';
  /** 依赖的契约 ID */
  contractId?: string;
  /** 依赖的 Todo ID */
  dependencyId?: string;
  /** 描述 */
  description: string;
}

/**
 * 阻塞项
 */
export interface BlockedItem {
  /** 唯一 ID */
  id: string;
  /** 阻塞项类型 */
  type: BlockedItemType;
  /** Mission ID */
  missionId: string;
  /** Assignment ID */
  assignmentId: string;
  /** Todo ID（如果是 Todo 级别阻塞） */
  todoId?: string;
  /** 阻塞原因 */
  reason: BlockingReason;
  /** 阻塞开始时间 */
  blockedAt: number;
  /** 解除时间 */
  unblockedAt?: number;
  /** 是否已解除 */
  resolved: boolean;
}

/**
 * 执行选项
 */
export interface ExecutionOptions {
  /** 工作目录 */
  workingDirectory: string;
  /** 超时时间（毫秒） */
  timeout?: number;
  /** 项目上下文 */
  projectContext?: string;
  /** 并行执行 */
  parallel?: boolean;
  /** 并行规划（默认 true） */
  parallelPlanning?: boolean;
  /** 阻塞超时时间（毫秒），超时后跳过阻塞项 */
  blockingTimeout?: number;
  /** 阻塞检查间隔（毫秒） */
  blockingCheckInterval?: number;
  /** 外部 Task ID（用于同步 SubTask） */
  taskId?: string;
  /** 输出回调 */
  onOutput?: (workerId: WorkerSlot, output: string) => void;
  /** 进度回调 */
  onProgress?: (progress: ExecutionProgress) => void;
  /** 阻塞回调 */
  onBlocked?: (blockedItem: BlockedItem) => void;
  /** 解除阻塞回调 */
  onUnblocked?: (blockedItem: BlockedItem) => void;
  /** 执行后端选择（已弃用，仅支持 adapter 模式） */
  executionMode?: 'adapter';
}

/**
 * 执行进度
 */
export interface ExecutionProgress {
  missionId: string;
  phase: 'planning' | 'executing' | 'reviewing' | 'completed';
  totalAssignments: number;
  completedAssignments: number;
  blockedAssignments: number;
  currentAssignment?: {
    id: string;
    workerId: WorkerSlot;
    progress: number;
  };
  blockedItems: BlockedItem[];
  overallProgress: number;
}

/**
 * 执行结果
 */
export interface ExecutionResult {
  mission: Mission;
  success: boolean;
  assignmentResults: Map<string, AutonomousExecutionResult>;
  contractVerifications: Map<string, boolean>;
  blockedItems: BlockedItem[];
  resolvedBlockings: BlockedItem[];
  errors: string[];
  duration: number;
  /** 聚合的 Token 使用统计 */
  tokenUsage?: TokenUsage;
}

/**
 * MissionExecutor - 任务执行器
 */
export class MissionExecutor extends EventEmitter {
  private workers: Map<WorkerSlot, AutonomousWorker> = new Map();
  private reviewer: ProfileAwareReviewer;
  private blockedItems: Map<string, BlockedItem> = new Map();
  private resolvedBlockings: BlockedItem[] = [];
  private blockingIdCounter = 0;
  private snapshotManager: SnapshotManager | null = null;
  private taskManager: UnifiedTaskManager | null = null;
  private todoManager: PlanTodoManager | null = null;
  private workspaceRoot: string = '';
  private adapterFactory: IAdapterFactory | null = null;
  private contextManager: import('../../context/context-manager').ContextManager | null = null;
  private currentMissionId: string | null = null;

  constructor(
    private orchestrator: MissionOrchestrator,
    private profileLoader: ProfileLoader,
    private guidanceInjector: GuidanceInjector
  ) {
    super();
    this.reviewer = new ProfileAwareReviewer(profileLoader);
  }

  /**
   * 设置快照管理器
   */
  setSnapshotManager(snapshotManager: SnapshotManager): void {
    this.snapshotManager = snapshotManager;
  }

  /**
   * 设置任务管理器
   */
  setTaskManager(taskManager: UnifiedTaskManager): void {
    this.taskManager = taskManager;
  }

  /**
   * 设置 TODO 管理器
   */
  setTodoManager(todoManager: PlanTodoManager): void {
    this.todoManager = todoManager;
  }

  /**
   * 设置工作目录
   */
  setWorkspaceRoot(workspaceRoot: string): void {
    this.workspaceRoot = workspaceRoot;
  }

  /**
   * 设置适配器工厂（用于真实执行）
   */
  setAdapterFactory(adapterFactory: IAdapterFactory): void {
    this.adapterFactory = adapterFactory;
  }

  /**
   * 设置上下文管理器
   */
  setContextManager(contextManager: import('../../context/context-manager').ContextManager): void {
    this.contextManager = contextManager;
  }

  /**
   * 确保 Worker 存在（懒加载创建）
   */
  private ensureWorker(workerSlot: WorkerSlot): AutonomousWorker {
    let worker = this.workers.get(workerSlot);
    if (!worker) {
      worker = new AutonomousWorker(
        workerSlot,
        this.profileLoader,
        this.guidanceInjector
      );
      this.setupWorkerListeners(worker);
      this.workers.set(workerSlot, worker);
    }
    return worker;
  }

  /**
   * 设置 Worker 事件监听
   */
  private setupWorkerListeners(worker: AutonomousWorker): void {
    worker.on('todoStarted', (data) => {
      this.emit('todoStarted', data);
    });

    worker.on('todoCompleted', (data) => {
      this.emit('todoCompleted', data);

      // 更新 TODO 文件状态
      this.updateTodoFileStatus(data.todoId, 'completed');
    });

    worker.on('todoFailed', (data) => {
      this.emit('todoFailed', data);

      // 更新 TODO 文件状态
      this.updateTodoFileStatus(data.todoId, 'failed');
    });

    worker.on('dynamicTodoAdded', (data) => {
      this.emit('dynamicTodoAdded', data);
    });

    worker.on('approvalRequested', (data) => {
      this.emit('approvalRequested', data);
    });
  }

  /**
   * 更新 TODO 文件中的状态
   */
  private updateTodoFileStatus(todoId: string, status: 'completed' | 'failed'): void {
    if (!this.todoManager || !this.snapshotManager) {
      return;
    }

    const session = (this.snapshotManager as any).sessionManager?.getCurrentSession();
    if (!session) {
      return;
    }

    // 从当前执行的 mission 中获取 missionId
    // 注意：这里需要在 execute() 方法中存储当前 missionId
    if (this.currentMissionId) {
      this.todoManager.updateMissionTodoStatus(
        session.id,
        this.currentMissionId,
        todoId,
        status
      );
    }
  }

  /**
   * 执行 Mission
   */
  async execute(
    mission: Mission,
    options: ExecutionOptions
  ): Promise<ExecutionResult> {
    // 仅支持 adapter 模式
    if (!this.adapterFactory) {
      throw new Error('未配置 AdapterFactory，无法执行任务');
    }
    const effectiveOptions: ExecutionOptions = { ...options, executionMode: 'adapter' };
    const startTime = Date.now();
    const assignmentResults = new Map<string, AutonomousExecutionResult>();
    const contractVerifications = new Map<string, boolean>();
    const errors: string[] = [];

    // 设置当前 missionId（用于 TODO 状态更新）
    this.currentMissionId = mission.id;

    // 清除上一次执行的阻塞状态
    this.clearBlockingState();

    this.emit('executionStarted', { missionId: mission.id });

    // 报告初始进度
    this.reportProgress(mission, 'planning', assignmentResults, effectiveOptions.onProgress);

    // 生成 TODO 文件
    if (this.todoManager && this.snapshotManager) {
      const session = (this.snapshotManager as any).sessionManager?.getCurrentSession();
      if (session) {
        this.todoManager.ensureMissionTodoFile(mission, session.id);
      }
    }

    // ✅ 将 Assignments 添加到 ContextManager
    if (this.contextManager) {
      for (const assignment of mission.assignments) {
        this.contextManager.addTask({
          id: assignment.id,
          description: assignment.responsibility,
          status: 'pending',
          assignedWorker: assignment.workerId,
        });
      }
      await this.contextManager.saveMemory();
    }

    // 同步 Assignment 到 TaskManager 作为 SubTask
    await this.syncAssignmentsToSubTasks(mission, effectiveOptions.taskId);

    try {
      // 1. Worker 规划阶段
      await this.planningPhase(mission, effectiveOptions);

      // 2. 执行阶段
      this.reportProgress(mission, 'executing', assignmentResults, effectiveOptions.onProgress);

      if (effectiveOptions.parallel) {
        await this.executeParallel(mission, effectiveOptions, assignmentResults, errors);
      } else {
        await this.executeSequential(mission, effectiveOptions, assignmentResults, errors);
      }

      // 3. 评审阶段
      this.reportProgress(mission, 'reviewing', assignmentResults, effectiveOptions.onProgress);
      await this.reviewPhase(mission, assignmentResults);

      // 4. 契约验证
      await this.verifyContracts(mission, contractVerifications, errors);

      // 5. 完成
      const success = errors.length === 0 &&
        Array.from(assignmentResults.values()).every(r => r.success);

      if (success) {
        await this.orchestrator.completeMission(mission.id);
      } else {
        await this.orchestrator.failMission(mission.id, errors.join('; '));
      }

      this.reportProgress(mission, 'completed', assignmentResults, options.onProgress);

      // 聚合所有 Assignment 的 Token 统计
      const totalTokenUsage: TokenUsage = { inputTokens: 0, outputTokens: 0 };
      for (const assignmentResult of assignmentResults.values()) {
        if (assignmentResult.tokenUsage) {
          totalTokenUsage.inputTokens += assignmentResult.tokenUsage.inputTokens || 0;
          totalTokenUsage.outputTokens += assignmentResult.tokenUsage.outputTokens || 0;
          if (assignmentResult.tokenUsage.cacheReadTokens) {
            totalTokenUsage.cacheReadTokens = (totalTokenUsage.cacheReadTokens || 0) +
              assignmentResult.tokenUsage.cacheReadTokens;
          }
        }
      }

      const result: ExecutionResult = {
        mission,
        success,
        assignmentResults,
        contractVerifications,
        blockedItems: this.getBlockedItems(),
        resolvedBlockings: this.getResolvedBlockings(),
        errors,
        duration: Date.now() - startTime,
        tokenUsage: totalTokenUsage,
      };

      this.emit('executionCompleted', result);

      // 清除当前 missionId
      this.currentMissionId = null;

      return result;
    } catch (error) {
      const errorMessage = error instanceof Error ? error.message : String(error);
      errors.push(errorMessage);

      await this.orchestrator.failMission(mission.id, errorMessage);

      this.emit('executionFailed', { missionId: mission.id, error: errorMessage });

      // 清除当前 missionId
      this.currentMissionId = null;

      return {
        mission,
        success: false,
        assignmentResults,
        contractVerifications,
        blockedItems: this.getBlockedItems(),
        resolvedBlockings: this.getResolvedBlockings(),
        errors,
        duration: Date.now() - startTime,
      };
    }
  }

  /**
   * Worker 规划阶段（支持并行规划）
   */
  private async planningPhase(
    mission: Mission,
    options: ExecutionOptions
  ): Promise<void> {
    // 默认启用并行规划
    const parallelPlanning = options.parallelPlanning !== false;

    if (parallelPlanning && mission.assignments.length > 1) {
      // 并行规划：所有 Worker 同时规划
      await this.planningPhaseParallel(mission, options);
    } else {
      // 顺序规划
      await this.planningPhaseSequential(mission, options);
    }
  }

  /**
   * 并行规划阶段
   */
  private async planningPhaseParallel(
    mission: Mission,
    options: ExecutionOptions
  ): Promise<void> {
    // 生成 contextSnapshot（如果有 ContextManager）
    const contextSnapshot = this.contextManager?.getContextSlice({
      maxTokens: 4000,
      memoryRatio: 0.4,
      memorySummary: {
        includeCurrentTasks: true,
        includeKeyDecisions: 3,
        includeCodeChanges: 5,
      },
    });

    const planningPromises = mission.assignments.map(async (assignment) => {
      const worker = this.ensureWorker(assignment.workerId);

      // Worker 自主规划（传递动态上下文）
      const planResult = await worker.planAssignment(assignment, {
        projectContext: options.projectContext,
        contextSnapshot,
      });

      // 更新 Assignment 的 todos
      assignment.todos = planResult.todos;
      assignment.planningStatus = 'planned';

      await this.orchestrator.updateAssignment(mission.id, assignment);

      this.emit('assignmentPlanned', {
        missionId: mission.id,
        assignmentId: assignment.id,
        todos: planResult.todos,
        warnings: planResult.warnings,
      });

      return { assignmentId: assignment.id, planResult };
    });

    await Promise.all(planningPromises);
  }

  /**
   * 顺序规划阶段
   */
  private async planningPhaseSequential(
    mission: Mission,
    options: ExecutionOptions
  ): Promise<void> {
    // 生成 contextSnapshot（如果有 ContextManager）
    const contextSnapshot = this.contextManager?.getContextSlice({
      maxTokens: 4000,
      memoryRatio: 0.4,
      memorySummary: {
        includeCurrentTasks: true,
        includeKeyDecisions: 3,
        includeCodeChanges: 5,
      },
    });

    for (const assignment of mission.assignments) {
      const worker = this.ensureWorker(assignment.workerId);

      // Worker 自主规划（传递动态上下文）
      const planResult = await worker.planAssignment(assignment, {
        projectContext: options.projectContext,
        contextSnapshot,
      });

      // 更新 Assignment 的 todos
      assignment.todos = planResult.todos;
      assignment.planningStatus = 'planned';

      await this.orchestrator.updateAssignment(mission.id, assignment);

      this.emit('assignmentPlanned', {
        missionId: mission.id,
        assignmentId: assignment.id,
        todos: planResult.todos,
        warnings: planResult.warnings,
      });
    }
  }

  /**
   * 顺序执行
   */
  private async executeSequential(
    mission: Mission,
    options: ExecutionOptions,
    results: Map<string, AutonomousExecutionResult>,
    errors: string[]
  ): Promise<void> {
    for (const assignment of mission.assignments) {
      const result = await this.executeAssignment(assignment, mission, options);
      results.set(assignment.id, result);

      if (!result.success) {
        errors.push(...result.errors);
      }

      await this.orchestrator.updateAssignment(mission.id, assignment);
    }
  }

  /**
   * 并行执行
   */
  private async executeParallel(
    mission: Mission,
    options: ExecutionOptions,
    results: Map<string, AutonomousExecutionResult>,
    errors: string[]
  ): Promise<void> {
    // 分析依赖关系，确定可并行执行的 Assignment
    const executionGroups = this.groupByDependencies(mission);

    for (const group of executionGroups) {
      const groupPromises = group.map(async (assignment) => {
        const result = await this.executeAssignment(assignment, mission, options);
        results.set(assignment.id, result);

        if (!result.success) {
          errors.push(...result.errors);
        }

        await this.orchestrator.updateAssignment(mission.id, assignment);
        return result;
      });

      await Promise.all(groupPromises);
    }
  }

  /**
   * 执行单个 Assignment
   */
  private async executeAssignment(
    assignment: Assignment,
    mission: Mission,
    options: ExecutionOptions
  ): Promise<AutonomousExecutionResult> {
    const worker = this.ensureWorker(assignment.workerId);

    // 检查阻塞
    const blockingReason = this.checkAssignmentBlocking(assignment, mission);
    if (blockingReason) {
      const blockedItem = this.recordBlocking(
        'assignment',
        mission.id,
        assignment.id,
        blockingReason
      );

      // 等待阻塞解除
      const unblocked = await this.waitForUnblocking(blockedItem, mission, options);
      if (!unblocked) {
        // 超时未解除，返回失败结果
        assignment.status = 'blocked';
        return {
          assignment,
          success: false,
          completedTodos: [],
          failedTodos: [],
          skippedTodos: assignment.todos,
          dynamicTodos: [],
          recoveredTodos: [],
          totalDuration: 0,
          errors: [`Assignment blocked: ${blockingReason.description}`],
          recoveryAttempts: 0,
        };
      }
    }

    assignment.status = 'executing';
    assignment.startedAt = Date.now();

    // 创建快照 - 在执行前为目标文件创建快照
    await this.createSnapshotsForAssignment(assignment, mission);

    // 同步 SubTask 状态为 running
    await this.updateSubTaskStatus(mission, assignment, 'running');

    this.emit('assignmentStarted', {
      missionId: mission.id,
      assignmentId: assignment.id,
      workerId: assignment.workerId,
    });

    // 生成 contextSnapshot（如果有 ContextManager）
    const contextSnapshot = this.contextManager
      ? this.contextManager.getContext(6000)
      : undefined;

    const result = await worker.executeAssignment(assignment, {
      workingDirectory: options.workingDirectory,
      timeout: options.timeout,
      projectContext: options.projectContext,
      adapterFactory: options.executionMode === 'adapter' ? this.adapterFactory || undefined : undefined,
      adapterScope: options.executionMode === 'adapter' ? {
        source: 'worker',
        streamToUI: true,
        adapterRole: 'worker',
        messageMeta: {
          taskId: mission.id,
          subTaskId: assignment.id,
          contextSnapshot,
          taskContext: {
            goal: assignment.responsibility,
            targetFiles: assignment.scope?.targetPaths,
            dependencies: assignment.consumerContracts, // 使用 consumerContracts 作为依赖
            constraints: assignment.scope?.excludes, // 使用 excludes 作为约束
          },
        },
      } : undefined,
      onOutput: (output) => {
        options.onOutput?.(assignment.workerId, output);
      },
    });

    // 更新契约状态（执行期解锁消费者任务）
    this.updateContractsFromAssignment(mission, assignment);

    // 执行完成后，检查并解除可能因此满足的阻塞
    this.checkAndResolveBlockings(mission);

    assignment.status = result.success ? 'completed' : 'failed';
    assignment.completedAt = Date.now();
    assignment.progress = 100;

    // ✅ 自动更新到 ContextManager
    if (this.contextManager) {
      if (result.success) {
        // 更新任务状态为完成
        this.contextManager.updateTaskStatus(
          assignment.id,
          'completed',
          `完成 ${result.completedTodos.length} 个 Todo`
        );

        // 添加代码变更记录
        const modifiedFiles = new Set<string>();
        for (const todo of result.completedTodos) {
          if (todo.output?.modifiedFiles) {
            for (const file of todo.output.modifiedFiles) {
              modifiedFiles.add(file);
            }
          }
        }

        // 为每个修改的文件添加变更记录
        for (const file of modifiedFiles) {
          this.contextManager.addCodeChange(
            file,
            'modify',
            `${assignment.workerId} 完成: ${assignment.responsibility}`
          );
        }

        // 如果有动态添加的 Todo，记录为重要上下文
        if (result.dynamicTodos.length > 0) {
          this.contextManager.addImportantContext(
            `${assignment.workerId} 动态添加了 ${result.dynamicTodos.length} 个 Todo`
          );
        }
      } else {
        // 更新任务状态为失败
        this.contextManager.updateTaskStatus(
          assignment.id,
          'failed',
          result.errors.join('; ')
        );

        // 添加待解决问题
        if (result.errors.length > 0) {
          this.contextManager.addPendingIssue(
            `${assignment.workerId} 执行失败: ${result.errors[0]}`
          );
        }
      }

      // 保存 Memory
      await this.contextManager.saveMemory();
    }

    // 同步 SubTask 状态
    await this.updateSubTaskStatus(
      mission,
      assignment,
      result.success ? 'completed' : 'failed',
      result.success ? undefined : result.errors.join('; ')
    );

    this.emit('assignmentCompleted', {
      missionId: mission.id,
      assignmentId: assignment.id,
      success: result.success,
    });

    return result;
  }

  private updateContractsFromAssignment(mission: Mission, assignment: Assignment): void {
    const producedContracts = new Set<string>();
    for (const todo of assignment.todos) {
      if (todo.status !== 'completed') continue;
      for (const contractId of todo.producesContracts || []) {
        producedContracts.add(contractId);
      }
    }

    if (producedContracts.size === 0) return;

    for (const contractId of producedContracts) {
      const contract = mission.contracts.find(c => c.id === contractId);
      if (!contract) continue;
      if (contract.status === 'implemented' || contract.status === 'verified') {
        continue;
      }
      contract.status = 'implemented';
    }
  }

  /**
   * 为 Assignment 创建快照
   * 注意：此方法在执行前调用，只能使用 scope.includes 作为目标文件来源
   */
  private async createSnapshotsForAssignment(
    assignment: Assignment,
    mission: Mission
  ): Promise<void> {
    if (!this.snapshotManager) {
      logger.warn('执行器.快照.未配置', { assignmentId: assignment.id }, LogCategory.ORCHESTRATOR);
      return;
    }

    // 收集所有目标文件
    const targetFiles = new Set(this.collectTargetFiles(assignment));
    if (targetFiles.size > 0) {
      // 使用新的清理方法：按 Assignment 清理
      this.snapshotManager.clearSnapshotsForAssignment(assignment.id);
    }

    if (targetFiles.size === 0) {
      logger.debug('执行器.快照.无目标文件', { assignmentId: assignment.id }, LogCategory.ORCHESTRATOR);
      return;
    }

    // 为每个目标文件创建快照
    // 注意：这里为整个 Assignment 创建快照，todoId 使用 'assignment-init'
    for (const filePath of targetFiles) {
      try {
        const snapshot = this.snapshotManager.createSnapshotForMission(
          filePath,
          mission.id,
          assignment.id,
          'assignment-init', // 初始快照，不属于特定 todo
          assignment.workerId,
          `Assignment 执行前快照: ${assignment.responsibility}`
        );

        if (snapshot) {
          // 将快照 ID 添加到 Mission 的快照列表
          if (!mission.snapshots) {
            mission.snapshots = [];
          }
          mission.snapshots.push(snapshot.id);

          logger.debug('执行器.快照.创建', {
            filePath,
            missionId: mission.id,
            assignmentId: assignment.id,
            workerId: assignment.workerId,
            snapshotId: snapshot.id,
          }, LogCategory.ORCHESTRATOR);
        }
      } catch (error) {
        // 快照创建失败不阻止执行，但记录警告
        logger.warn('执行器.快照.创建_失败', {
          filePath,
          assignmentId: assignment.id,
          error: error instanceof Error ? error.message : String(error),
        }, LogCategory.ORCHESTRATOR);
      }
    }
  }

  /**
   * 同步 Assignment 到 TaskManager 作为 SubTask
   */
  private async syncAssignmentsToSubTasks(mission: Mission, externalTaskId?: string): Promise<void> {
    if (!this.taskManager) {
      logger.debug('执行器.任务管理器.未配置', { missionId: mission.id }, LogCategory.ORCHESTRATOR);
      return;
    }

    // 优先使用外部传入的 taskId，否则使用 missionId
    const taskId = externalTaskId || mission.id;

    // 保存 taskId 到 mission 以供后续使用
    mission.externalTaskId = taskId;

    for (const assignment of mission.assignments) {
      try {
        // 收集目标文件（从 scope 中获取）
        const targetFiles = this.collectTargetFiles(assignment);

        // 创建 SubTask（包含 assignmentId 用于稳定匹配）
        await this.taskManager.createSubTask(taskId, {
          description: assignment.responsibility,
          assignedWorker: assignment.workerId,
          assignmentId: assignment.id,
          targetFiles,
          priority: 5,
          reason: assignment.assignmentReason?.explanation,
          prompt: assignment.guidancePrompt,
        });

        logger.debug('执行器.子任务.创建', {
          taskId,
          missionId: mission.id,
          assignmentId: assignment.id,
          workerId: assignment.workerId,
        }, LogCategory.ORCHESTRATOR);
      } catch (error) {
        // 创建 SubTask 失败不阻止执行，但记录警告
        logger.warn('执行器.子任务.创建_失败', {
          taskId,
          missionId: mission.id,
          assignmentId: assignment.id,
          error: error instanceof Error ? error.message : String(error),
        }, LogCategory.ORCHESTRATOR);
      }
    }
  }

  private collectTargetFiles(assignment: Assignment): string[] {
    const filePattern = /[\w\-./]+\.(ts|js|tsx|jsx|py|java|go|rs|cpp|c|css|scss|html|json|md|yaml|yml|txt)/i;
    const candidates = [
      ...(assignment.scope?.targetPaths || []),
      ...(assignment.scope?.includes || []),
    ];
    const filtered = candidates.filter((item) => filePattern.test(item));
    return [...new Set(filtered)];
  }

  /**
   * 更新 SubTask 状态
   * 使用 assignmentId 进行稳定匹配（替代不稳定的 description 匹配）
   */
  private async updateSubTaskStatus(
    mission: Mission,
    assignment: Assignment,
    status: 'running' | 'completed' | 'failed',
    error?: string
  ): Promise<void> {
    if (!this.taskManager) return;

    // 使用 Mission 的 externalTaskId
    const taskId = mission.externalTaskId || mission.id;

    try {
      // 使用 assignmentId 进行稳定匹配（替代不稳定的 description 匹配）
      const subTask = await this.taskManager.getSubTaskByAssignmentId(taskId, assignment.id);
      if (!subTask) {
        logger.warn('执行器.子任务状态.未找到', {
          taskId,
          assignmentId: assignment.id,
          status,
        }, LogCategory.ORCHESTRATOR);
        return;
      }

      switch (status) {
        case 'running':
          await this.taskManager.startSubTask(taskId, subTask.id);
          break;
        case 'completed':
          await this.taskManager.completeSubTask(taskId, subTask.id, {
            agentType: assignment.workerId,  // ✅ 使用 agentType
            success: true,
            output: 'Assignment completed',
            duration: assignment.completedAt && assignment.startedAt
              ? assignment.completedAt - assignment.startedAt
              : 0,
            timestamp: new Date(),
          });
          break;
        case 'failed':
          await this.taskManager.failSubTask(taskId, subTask.id, error || 'Unknown error');
          break;
      }
    } catch (err) {
      logger.warn('执行器.子任务状态.更新_失败', {
        assignmentId: assignment.id,
        status,
        error: err instanceof Error ? err.message : String(err),
      }, LogCategory.ORCHESTRATOR);
    }
  }

  /**
   * 评审阶段
   */
  private async reviewPhase(
    mission: Mission,
    results: Map<string, AutonomousExecutionResult>
  ): Promise<void> {
    for (const assignment of mission.assignments) {
      const result = results.get(assignment.id);
      if (!result) continue;

      // 对每个完成的 Todo 进行评审
      for (const todo of result.completedTodos) {
        const reviewResult = await this.reviewer.reviewTodoOutput(todo, assignment);

        if (reviewResult.status !== 'approved') {
          this.emit('todoReviewFailed', {
            missionId: mission.id,
            assignmentId: assignment.id,
            todoId: todo.id,
            feedback: reviewResult.feedback,
            issues: reviewResult.issues,
          });
        }
      }

      // 互检评审
      if (mission.assignments.length > 1) {
        const reviewer = this.reviewer.selectPeerReviewer(assignment, assignment.workerId);
        const reviewerProfile = this.profileLoader.getProfile(reviewer);
        const executorProfile = this.profileLoader.getProfile(assignment.workerId);

        const peerReviewGuidance = this.guidanceInjector.buildPeerReviewGuidance(
          reviewerProfile,
          executorProfile,
          assignment.responsibility
        );

        this.emit('peerReviewScheduled', {
          missionId: mission.id,
          assignmentId: assignment.id,
          executor: assignment.workerId,
          reviewer,
          guidance: peerReviewGuidance,
        });
      }
    }
  }

  /**
   * 契约验证
   */
  private async verifyContracts(
    mission: Mission,
    verifications: Map<string, boolean>,
    errors: string[]
  ): Promise<void> {
    for (const contract of mission.contracts) {
      // 检查契约是否被实现
      const producerAssignment = mission.assignments.find(
        a => a.workerId === contract.producer
      );

      if (!producerAssignment || producerAssignment.status !== 'completed') {
        verifications.set(contract.id, false);
        errors.push(`契约 "${contract.name}" 未被提供方 ${contract.producer} 实现`);
        continue;
      }

      // 基于提供方完成状态验证契约（更复杂的验证可以扩展 ContractManager）
      verifications.set(contract.id, true);

      this.emit('contractVerified', {
        missionId: mission.id,
        contractId: contract.id,
        passed: true,
      });
    }
  }

  /**
   * 按依赖关系分组
   */
  private groupByDependencies(mission: Mission): Assignment[][] {
    // 简单实现：检查契约依赖
    const groups: Assignment[][] = [];
    const remaining = [...mission.assignments];
    const completed = new Set<string>();

    while (remaining.length > 0) {
      const currentGroup: Assignment[] = [];

      for (let i = remaining.length - 1; i >= 0; i--) {
        const assignment = remaining[i];

        // 检查是否所有依赖的契约都已完成
        const consumerContracts = assignment.consumerContracts;
        const dependenciesMet = consumerContracts.every(contractId => {
          const contract = mission.contracts.find(c => c.id === contractId);
          if (!contract) return true;

          // 检查提供方是否已完成
          return completed.has(contract.producer);
        });

        if (dependenciesMet) {
          currentGroup.push(assignment);
          remaining.splice(i, 1);
        }
      }

      if (currentGroup.length === 0 && remaining.length > 0) {
        // 有循环依赖，强制执行剩余的
        currentGroup.push(...remaining);
        remaining.length = 0;
      }

      if (currentGroup.length > 0) {
        groups.push(currentGroup);
        for (const assignment of currentGroup) {
          completed.add(assignment.workerId);
        }
      }
    }

    return groups;
  }

  /**
   * 报告进度
   */
  private reportProgress(
    mission: Mission,
    phase: ExecutionProgress['phase'],
    results: Map<string, AutonomousExecutionResult>,
    onProgress?: (progress: ExecutionProgress) => void
  ): void {
    const completedCount = Array.from(results.values()).filter(r => r.success).length;
    const totalCount = mission.assignments.length;
    const activeBlockedItems = Array.from(this.blockedItems.values());
    const blockedAssignmentIds = new Set(
      activeBlockedItems
        .filter(b => b.type === 'assignment')
        .map(b => b.assignmentId)
    );

    const progress: ExecutionProgress = {
      missionId: mission.id,
      phase,
      totalAssignments: totalCount,
      completedAssignments: completedCount,
      blockedAssignments: blockedAssignmentIds.size,
      blockedItems: activeBlockedItems,
      overallProgress: totalCount > 0 ? Math.round((completedCount / totalCount) * 100) : 0,
    };

    onProgress?.(progress);
    this.emit('progressUpdated', progress);
  }

  /**
   * 获取 Worker
   */
  getWorker(workerType: WorkerSlot): AutonomousWorker | undefined {
    return this.workers.get(workerType);
  }

  /**
   * 获取所有 Worker
   */
  getAllWorkers(): Map<WorkerSlot, AutonomousWorker> {
    return new Map(this.workers);
  }

  // ============= 阻塞处理方法 =============

  /**
   * 检查 Assignment 是否被阻塞
   */
  checkAssignmentBlocking(
    assignment: Assignment,
    mission: Mission
  ): BlockingReason | null {
    // 检查契约依赖
    for (const contractId of assignment.consumerContracts) {
      const contract = mission.contracts.find(c => c.id === contractId);
      if (!contract) continue;

      // 检查契约是否已实现
      if (contract.status !== 'implemented' && contract.status !== 'verified') {
        return {
          type: 'contract_pending',
          contractId: contract.id,
          description: `等待契约 "${contract.name}" 由 ${contract.producer} 实现`,
        };
      }
    }

    return null;
  }

  /**
   * 检查 Todo 是否被阻塞
   */
  checkTodoBlocking(
    todo: WorkerTodo,
    assignment: Assignment,
    mission: Mission
  ): BlockingReason | null {
    // 检查 Todo 依赖
    for (const depId of todo.dependsOn) {
      const depTodo = assignment.todos.find(t => t.id === depId);
      if (!depTodo) continue;

      if (depTodo.status !== 'completed') {
        return {
          type: 'dependency_incomplete',
          dependencyId: depId,
          description: `等待 Todo "${depTodo.content}" 完成`,
        };
      }
    }

    // 检查契约依赖
    for (const contractId of todo.requiredContracts) {
      const contract = mission.contracts.find(c => c.id === contractId);
      if (!contract) continue;

      if (contract.status !== 'implemented' && contract.status !== 'verified') {
        return {
          type: 'contract_pending',
          contractId: contract.id,
          description: `等待契约 "${contract.name}" 实现`,
        };
      }
    }

    // 检查审批状态
    if (todo.outOfScope && todo.approvalStatus !== 'approved') {
      return {
        type: 'approval_required',
        description: `超范围 Todo 需要审批`,
      };
    }

    return null;
  }

  /**
   * 记录阻塞
   */
  private recordBlocking(
    type: BlockedItemType,
    missionId: string,
    assignmentId: string,
    reason: BlockingReason,
    todoId?: string
  ): BlockedItem {
    const id = `blocking-${++this.blockingIdCounter}`;
    const blockedItem: BlockedItem = {
      id,
      type,
      missionId,
      assignmentId,
      todoId,
      reason,
      blockedAt: Date.now(),
      resolved: false,
    };

    this.blockedItems.set(id, blockedItem);
    this.emit('blocked', blockedItem);

    return blockedItem;
  }

  /**
   * 解除阻塞
   */
  private resolveBlocking(blockingId: string): void {
    const blockedItem = this.blockedItems.get(blockingId);
    if (!blockedItem) return;

    blockedItem.resolved = true;
    blockedItem.unblockedAt = Date.now();

    this.blockedItems.delete(blockingId);
    this.resolvedBlockings.push(blockedItem);

    this.emit('unblocked', blockedItem);
  }

  /**
   * 等待阻塞解除
   */
  async waitForUnblocking(
    blockedItem: BlockedItem,
    mission: Mission,
    options: ExecutionOptions
  ): Promise<boolean> {
    const timeout = options.blockingTimeout || 300000; // 默认 5 分钟
    const checkInterval = options.blockingCheckInterval || 1000; // 默认 1 秒
    const startTime = Date.now();

    options.onBlocked?.(blockedItem);

    while (Date.now() - startTime < timeout) {
      // 检查是否已解除
      const stillBlocked = this.isStillBlocked(blockedItem, mission);
      if (!stillBlocked) {
        this.resolveBlocking(blockedItem.id);
        options.onUnblocked?.(blockedItem);
        return true;
      }

      // 等待一段时间后重新检查
      await this.sleep(checkInterval);
    }

    // 超时，标记为解除但失败
    this.resolveBlocking(blockedItem.id);
    return false;
  }

  /**
   * 检查阻塞是否仍然存在
   */
  private isStillBlocked(blockedItem: BlockedItem, mission: Mission): boolean {
    const { reason } = blockedItem;

    switch (reason.type) {
      case 'contract_pending': {
        if (!reason.contractId) return false;
        const contract = mission.contracts.find(c => c.id === reason.contractId);
        if (!contract) return false;
        return contract.status !== 'implemented' && contract.status !== 'verified';
      }

      case 'dependency_incomplete': {
        if (!reason.dependencyId) return false;
        const assignment = mission.assignments.find(a => a.id === blockedItem.assignmentId);
        if (!assignment) return false;
        const depTodo = assignment.todos.find(t => t.id === reason.dependencyId);
        if (!depTodo) return false;
        return depTodo.status !== 'completed';
      }

      case 'approval_required': {
        const assignment = mission.assignments.find(a => a.id === blockedItem.assignmentId);
        if (!assignment || !blockedItem.todoId) return false;
        const todo = assignment.todos.find(t => t.id === blockedItem.todoId);
        if (!todo) return false;
        return todo.approvalStatus !== 'approved';
      }

      case 'resource_conflict':
        // 资源冲突需要外部解决
        return true;

      default:
        return false;
    }
  }

  /**
   * 批量检查并解除已满足条件的阻塞
   */
  checkAndResolveBlockings(mission: Mission): BlockedItem[] {
    const resolved: BlockedItem[] = [];

    for (const [id, blockedItem] of this.blockedItems) {
      if (!this.isStillBlocked(blockedItem, mission)) {
        this.resolveBlocking(id);
        resolved.push(blockedItem);
      }
    }

    return resolved;
  }

  /**
   * 获取当前所有阻塞项
   */
  getBlockedItems(): BlockedItem[] {
    return Array.from(this.blockedItems.values());
  }

  /**
   * 获取已解除的阻塞项
   */
  getResolvedBlockings(): BlockedItem[] {
    return [...this.resolvedBlockings];
  }

  /**
   * 清除阻塞状态
   */
  clearBlockingState(): void {
    this.blockedItems.clear();
    this.resolvedBlockings = [];
    this.blockingIdCounter = 0;
  }

  /**
   * 辅助方法：等待
   */
  private sleep(ms: number): Promise<void> {
    return new Promise(resolve => setTimeout(resolve, ms));
  }
}
