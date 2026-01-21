/**
 * Execution Coordinator - 执行协调器
 *
 * 职责：
 * - 协调各个执行器
 * - 管理执行流程
 * - 处理并行和顺序执行
 */

import { EventEmitter } from 'events';
import { CLIType } from '../../../types';
import { IAdapterFactory } from '../../../adapters/adapter-factory-interface';
import { TokenUsage } from '../../../types/agent-types';
import { ProfileLoader } from '../../profile/profile-loader';
import { ProfileAwareReviewer } from '../../review/profile-aware-reviewer';
import { AutonomousWorker } from '../../worker';
import { Mission, Assignment } from '../../mission';
import { SnapshotManager } from '../../../snapshot-manager';
import { UnifiedTaskManager } from '../../../task/unified-task-manager';
import { logger, LogCategory } from '../../../logging';

import { PlanningExecutor, PlanningOptions } from './planning-executor';
import { AssignmentExecutor, AssignmentExecutionOptions } from './assignment-executor';
import { ReviewExecutor, ReviewOptions } from './review-executor';
import { ContractVerifier } from './contract-verifier';
import { ProgressReporter, ExecutionProgress } from './progress-reporter';
import { BlockingManager, BlockedItem } from './blocking-manager';

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
  onOutput?: (workerId: CLIType, output: string) => void;
  /** 进度回调 */
  onProgress?: (progress: ExecutionProgress) => void;
  /** 阻塞回调 */
  onBlocked?: (blockedItem: BlockedItem) => void;
  /** 解除阻塞回调 */
  onUnblocked?: (blockedItem: BlockedItem) => void;
}

/**
 * 执行结果
 */
export interface ExecutionResult {
  success: boolean;
  completedAssignments: number;
  totalAssignments: number;
  errors: string[];
  tokenUsage?: TokenUsage;
}

export class ExecutionCoordinator extends EventEmitter {
  private planningExecutor: PlanningExecutor;
  private assignmentExecutor: AssignmentExecutor;
  private reviewExecutor: ReviewExecutor;
  private contractVerifier: ContractVerifier;
  private progressReporter: ProgressReporter;
  private blockingManager: BlockingManager;

  private contextManager: import('../../../context/context-manager').ContextManager | null = null;

  constructor(
    private workers: Map<CLIType, AutonomousWorker>,
    private adapterFactory: IAdapterFactory,
    private profileLoader: ProfileLoader,
    private reviewer: ProfileAwareReviewer,
    private snapshotManager: SnapshotManager | null,
    private taskManager: UnifiedTaskManager | null,
    private mission: Mission
  ) {
    super();

    // 初始化各个执行器
    this.planningExecutor = new PlanningExecutor(workers);
    this.assignmentExecutor = new AssignmentExecutor(
      workers,
      adapterFactory,
      snapshotManager,
      taskManager
    );
    this.reviewExecutor = new ReviewExecutor(workers, profileLoader, reviewer);
    this.contractVerifier = new ContractVerifier();
    this.progressReporter = new ProgressReporter(mission);
    this.blockingManager = new BlockingManager();

    // 设置事件监听
    this.setupEventListeners();
  }

  /**
   * 设置 ContextManager
   */
  setContextManager(contextManager: import('../../../context/context-manager').ContextManager): void {
    this.contextManager = contextManager;
  }

  /**
   * 执行 Mission
   */
  async execute(options: ExecutionOptions): Promise<ExecutionResult> {
    logger.info(LogCategory.ORCHESTRATOR, `开始执行 Mission: ${this.mission.goal}`);

    // 添加 Assignments 到 ContextManager
    await this.initializeContextManager();

    // 同步 Assignments 到 SubTasks
    if (options.taskId) {
      await this.syncAssignmentsToSubTasks(options.taskId);
    }

    const errors: string[] = [];

    try {
      // 阶段 1: 规划
      this.progressReporter.setPhase('planning');
      const planningResult = await this.planningExecutor.execute(this.mission, {
        projectContext: options.projectContext,
        parallel: options.parallelPlanning,
        contextManager: this.contextManager,
      });

      if (!planningResult.success) {
        errors.push(...planningResult.errors);
        return this.buildResult(false, errors);
      }

      // 阶段 2: 执行
      this.progressReporter.setPhase('execution');
      const executionResult = options.parallel
        ? await this.executeParallel(options)
        : await this.executeSequential(options);

      if (!executionResult.success) {
        errors.push(...executionResult.errors);
      }

      // 阶段 3: 评审（可选）
      // Note: Mission doesn't have reviewRequired property, skip for now
      // if (this.mission.reviewRequired) {
      //   this.progressReporter.setPhase('review');
      //   const reviewResult = await this.reviewExecutor.execute(this.mission, {
      //     workingDirectory: options.workingDirectory,
      //     projectContext: options.projectContext,
      //   });
      //
      //   if (!reviewResult.success) {
      //     errors.push(...reviewResult.errors);
      //   }
      // }

      // 阶段 4: 验证契约
      this.progressReporter.setPhase('verification');
      const verificationResult = await this.contractVerifier.verify(this.mission);

      if (!verificationResult.success) {
        errors.push(...verificationResult.errors);
      }

      // 完成
      this.progressReporter.setPhase('completed');

      const success = errors.length === 0;
      logger.info(
        LogCategory.ORCHESTRATOR,
        `Mission 执行${success ? '成功' : '失败'}: ${this.mission.goal}`
      );

      return this.buildResult(success, errors);
    } catch (error: any) {
      logger.error(LogCategory.ORCHESTRATOR, `Mission 执行异常: ${error.message}`);
      errors.push(error.message);
      return this.buildResult(false, errors);
    }
  }

  /**
   * 顺序执行
   */
  private async executeSequential(options: ExecutionOptions): Promise<ExecutionResult> {
    const errors: string[] = [];
    let completedCount = 0;

    // 按依赖关系分组
    const groups = this.groupByDependencies();

    for (const group of groups) {
      for (const assignment of group) {
        this.progressReporter.reportAssignmentStart(assignment);

        const result = await this.assignmentExecutor.execute(this.mission, assignment, {
          workingDirectory: options.workingDirectory,
          projectContext: options.projectContext,
          timeout: options.timeout,
          taskId: options.taskId,
          contextManager: this.contextManager,
          onOutput: options.onOutput,
        });

        if (result.success) {
          completedCount++;
          this.progressReporter.reportAssignmentComplete(
            assignment,
            result.completedTodos,
            result.tokenUsage
          );
        } else {
          errors.push(...result.errors);
        }
      }
    }

    return this.buildResult(errors.length === 0, errors, completedCount);
  }

  /**
   * 并行执行
   */
  private async executeParallel(options: ExecutionOptions): Promise<ExecutionResult> {
    const errors: string[] = [];
    let completedCount = 0;

    // 按依赖关系分组
    const groups = this.groupByDependencies();

    for (const group of groups) {
      const promises = group.map(async (assignment) => {
        this.progressReporter.reportAssignmentStart(assignment);

        const result = await this.assignmentExecutor.execute(this.mission, assignment, {
          workingDirectory: options.workingDirectory,
          projectContext: options.projectContext,
          timeout: options.timeout,
          taskId: options.taskId,
          contextManager: this.contextManager,
          onOutput: options.onOutput,
        });

        if (result.success) {
          this.progressReporter.reportAssignmentComplete(
            assignment,
            result.completedTodos,
            result.tokenUsage
          );
          return { success: true, errors: [] };
        } else {
          return { success: false, errors: result.errors };
        }
      });

      const results = await Promise.all(promises);
      results.forEach(result => {
        if (result.success) {
          completedCount++;
        } else {
          errors.push(...result.errors);
        }
      });
    }

    return this.buildResult(errors.length === 0, errors, completedCount);
  }

  /**
   * 按依赖关系分组
   */
  private groupByDependencies(): Assignment[][] {
    const groups: Assignment[][] = [];
    const processed = new Set<string>();
    const assignments = [...this.mission.assignments];

    while (processed.size < assignments.length) {
      const group: Assignment[] = [];

      for (const assignment of assignments) {
        if (processed.has(assignment.id)) {
          continue;
        }

        // Check contract dependencies
        const depsProcessed = assignment.consumerContracts.every((contractId: string) => {
          // Find the producer of this contract
          const producer = this.mission.assignments.find(
            a => a.producerContracts.includes(contractId)
          );
          return !producer || processed.has(producer.id);
        });

        if (depsProcessed) {
          group.push(assignment);
          processed.add(assignment.id);
        }
      }

      if (group.length === 0) {
        // 存在循环依赖，将剩余的都加入
        for (const assignment of assignments) {
          if (!processed.has(assignment.id)) {
            group.push(assignment);
            processed.add(assignment.id);
          }
        }
      }

      groups.push(group);
    }

    return groups;
  }

  /**
   * 初始化 ContextManager
   */
  private async initializeContextManager(): Promise<void> {
    if (!this.contextManager) {
      return;
    }

    for (const assignment of this.mission.assignments) {
      this.contextManager.addTask({
        id: assignment.id,
        description: assignment.responsibility,
        status: 'pending',
        assignedWorker: assignment.workerId,
      });
    }

    await this.contextManager.saveMemory();
  }

  /**
   * 同步 Assignments 到 SubTasks
   */
  private async syncAssignmentsToSubTasks(taskId: string): Promise<void> {
    if (!this.taskManager) {
      return;
    }

    try {
      for (const assignment of this.mission.assignments) {
        const existingSubTask = await this.taskManager.getSubTaskByAssignmentId(taskId, assignment.id);

        if (!existingSubTask) {
          await this.taskManager.createSubTask(taskId, {
            description: assignment.responsibility,
            assignedWorker: assignment.workerId,
            assignmentId: assignment.id,
          });
        }
      }
    } catch (error: any) {
      logger.warn(
        LogCategory.ORCHESTRATOR,
        `同步 SubTasks 失败: ${error.message}`
      );
    }
  }

  /**
   * 设置事件监听
   */
  private setupEventListeners(): void {
    // 进度事件
    this.progressReporter.on('progress', (progress: ExecutionProgress) => {
      this.emit('progress', progress);
    });

    // 阻塞事件
    this.blockingManager.on('blocked', (blockedItem: BlockedItem) => {
      this.emit('blocked', blockedItem);
    });

    this.blockingManager.on('unblocked', (blockedItem: BlockedItem) => {
      this.emit('unblocked', blockedItem);
    });
  }

  /**
   * 构建执行结果
   */
  private buildResult(
    success: boolean,
    errors: string[],
    completedCount?: number
  ): ExecutionResult {
    const progress = this.progressReporter.getProgress();

    return {
      success,
      completedAssignments: completedCount ?? progress.completedAssignments,
      totalAssignments: progress.totalAssignments,
      errors,
      tokenUsage: progress.tokenUsage,
    };
  }
}
