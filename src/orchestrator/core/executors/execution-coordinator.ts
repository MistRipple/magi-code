/**
 * Execution Coordinator - 执行协调器
 *
 * 职责：
 * - 协调各个执行器
 * - 管理执行流程
 * - 处理并行和顺序执行
 */

import { EventEmitter } from 'events';
import { WorkerSlot } from '../../../types';
import { IAdapterFactory } from '../../../adapters/adapter-factory-interface';
import { TokenUsage } from '../../../types/agent-types';
import { ProfileLoader } from '../../profile/profile-loader';
import { ProfileAwareReviewer } from '../../review/profile-aware-reviewer';
import { AutonomousWorker, AutonomousExecutionResult } from '../../worker';
import { Mission, Assignment } from '../../mission';
import { SnapshotManager } from '../../../snapshot-manager';
import { logger, LogCategory } from '../../../logging';

import { PlanningExecutor, PlanningOptions } from './planning-executor';
import { AssignmentExecutor, AssignmentExecutionOptions, AssignmentExecutionResult } from './assignment-executor';
import { ReviewExecutor, ReviewOptions } from './review-executor';
import { ContractVerifier } from './contract-verifier';
import { ProgressReporter, ExecutionProgress } from './progress-reporter';
import { TodoManager } from '../../../todo';
import { BlockingManager, BlockedItem } from './blocking-manager';
import type { ReportCallback } from '../../protocols/worker-report';
import { TaskDependencyGraph, ExecutionBatch } from '../../task-dependency-graph';

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
  /** 输出回调 */
  onOutput?: (workerId: WorkerSlot, output: string) => void;
  /** 进度回调 */
  onProgress?: (progress: ExecutionProgress) => void;
  /** 阻塞回调 */
  onBlocked?: (blockedItem: BlockedItem) => void;
  /** 解除阻塞回调 */
  onUnblocked?: (blockedItem: BlockedItem) => void;
  /** Worker 汇报回调 */
  onReport?: ReportCallback;
  /** 汇报超时(ms) */
  reportTimeout?: number;
  /** 获取补充指令（在决策点注入） */
  getSupplementaryInstructions?: (workerId: WorkerSlot) => string[];

  // ============ 智能执行策略选项 ============
  /** 是否需要规划阶段（由 TaskPreAnalyzer 决定） */
  needsPlanning: boolean;
  /** 是否需要评审阶段（由 TaskPreAnalyzer 决定） */
  needsReview: boolean;
  /** 是否需要验证阶段（由 TaskPreAnalyzer 决定） */
  needsVerification: boolean;

  // ============ SubTask 同步选项 ============
  /** 关联的 Task ID，用于同步 Assignments 到 SubTasks */
  taskId?: string;

  // ============ Wave 执行选项（提案 4.6） ============
  /** 使用 Wave 并行分组执行 */
  useWaveExecution?: boolean;
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
  /** 每个 Assignment 的详细执行结果 */
  assignmentResults: Map<string, AutonomousExecutionResult>;
  /** Wave 执行信息（提案 4.6） */
  waveInfo?: WaveExecutionInfo;
  /** 是否有等待审批的任务 */
  hasPendingApprovals?: boolean;
}

/**
 * Wave 执行信息（提案 4.6）
 */
export interface WaveExecutionInfo {
  /** 总 Wave 数 */
  totalWaves: number;
  /** 已完成的 Wave 数 */
  completedWaves: number;
  /** 每个 Wave 的任务 ID */
  waves: string[][];
  /** 关键路径 */
  criticalPath: string[];
}

export class ExecutionCoordinator extends EventEmitter {
  private planningExecutor: PlanningExecutor;
  private assignmentExecutor: AssignmentExecutor;
  private reviewExecutor: ReviewExecutor;
  private contractVerifier: ContractVerifier;
  private progressReporter: ProgressReporter;
  private blockingManager: BlockingManager;

  private contextManager: import('../../../context/context-manager').ContextManager | null = null;

  /** 收集每个 Assignment 的执行结果 */
  private collectedAssignmentResults: Map<string, AutonomousExecutionResult> = new Map();

  constructor(
    private workers: Map<WorkerSlot, AutonomousWorker>,
    private adapterFactory: IAdapterFactory,
    private profileLoader: ProfileLoader,
    private reviewer: ProfileAwareReviewer,
    private snapshotManager: SnapshotManager | null,
    private workspaceRoot: string,
    private mission: Mission,
    todoManager: TodoManager
  ) {
    super();

    // 初始化各个执行器
    this.planningExecutor = new PlanningExecutor(workers, todoManager);
    this.assignmentExecutor = new AssignmentExecutor(
      workers,
      adapterFactory,
      snapshotManager,
      this.workspaceRoot
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
   *
   * 支持动态阶段选择：
   * - needsPlanning: 是否执行规划阶段（简单任务可跳过）
   * - needsReview: 是否执行评审阶段（复杂任务需要）
   * - needsVerification: 是否执行验证阶段（有契约时需要）
   */
  async execute(options: ExecutionOptions): Promise<ExecutionResult> {
    if (
      options.needsPlanning === undefined
      || options.needsReview === undefined
      || options.needsVerification === undefined
    ) {
      throw new Error('[ExecutionCoordinator] 必须显式指定 needsPlanning/needsReview/needsVerification');
    }
    const needsPlanning = options.needsPlanning;
    const needsReview = options.needsReview;
    const needsVerification = options.needsVerification;

    logger.info(
      `开始执行 Mission: ${this.mission.goal}`,
      {
        needsPlanning,
        needsReview,
        needsVerification,
        parallel: options.parallel,
        taskId: options.taskId,
      },
      LogCategory.ORCHESTRATOR
    );

    // 添加 Assignments 到 ContextManager
    await this.initializeContextManager();

    // 统一 Todo 系统：不再需要同步到 SubTasks
    // Assignments 信息通过 Todo 系统传递给 UI

    const errors: string[] = [];

    try {
      // ========== 阶段 1: 规划（动态） ==========
      if (needsPlanning) {
        this.progressReporter.setPhase('planning');
        logger.info(LogCategory.ORCHESTRATOR, '执行规划阶段');

        const planningResult = await this.planningExecutor.execute(this.mission, {
          projectContext: options.projectContext,
          parallel: options.parallelPlanning,
          contextManager: this.contextManager,
        });

        if (!planningResult.success) {
          errors.push(...planningResult.errors);
          return this.buildResult(false, errors);
        }

        // 通知规划完成（同步到 UI）
        for (const assignment of this.mission.assignments) {
          this.emit('assignmentPlanned', {
            missionId: this.mission.id,
            assignmentId: assignment.id,
            todos: assignment.todos || [],
          });
        }
      } else {
        logger.info(LogCategory.ORCHESTRATOR, '跳过规划阶段（简单任务）');
        // 默认 Todo 由 MissionOrchestrator 通过 TodoManager 创建
        // 此处仅记录日志，确保 assignment.todos 已被填充
      }

      // ========== 阶段 2: 执行（总是执行） ==========
      this.progressReporter.setPhase('execution');

      // 选择执行策略：Wave 并行 > 普通并行 > 顺序
      let executionResult: ExecutionResult;
      if (options.useWaveExecution || (options.parallel && this.hasContractDependencies())) {
        // 使用 Wave 分组执行（提案 4.6）
        executionResult = await this.executeWithWaves(options);
      } else if (options.parallel) {
        executionResult = await this.executeParallel(options);
      } else {
        executionResult = await this.executeSequential(options);
      }

      if (!executionResult.success) {
        errors.push(...executionResult.errors);
      }

      // ========== 阶段 3: 评审（动态） ==========
      if (needsReview) {
        this.progressReporter.setPhase('review');
        logger.info(LogCategory.ORCHESTRATOR, '执行评审阶段');

        const reviewResult = await this.reviewExecutor.execute(this.mission, {
          workingDirectory: options.workingDirectory,
          projectContext: options.projectContext,
        });

        if (!reviewResult.success) {
          errors.push(...reviewResult.errors);
        }
      } else {
        logger.info(LogCategory.ORCHESTRATOR, '跳过评审阶段');
      }

      // ========== 阶段 4: 验证契约（动态） ==========
      if (needsVerification) {
        this.progressReporter.setPhase('verification');
        logger.info(LogCategory.ORCHESTRATOR, '执行验证阶段');

        const verificationResult = await this.contractVerifier.verify(this.mission);

        if (!verificationResult.success) {
          errors.push(...verificationResult.errors);
        }
      } else {
        logger.info(LogCategory.ORCHESTRATOR, '跳过验证阶段');
      }

      // ========== 完成 ==========
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
    let anyPendingApprovals = false;

    // 按依赖关系分组
    const groups = this.groupByDependencies();

    for (const group of groups) {
      for (const assignment of group) {
        this.progressReporter.reportAssignmentStart(assignment);
        this.markAssignmentStart(assignment);

        const result = await this.assignmentExecutor.execute(this.mission, assignment, {
          workingDirectory: options.workingDirectory,
          projectContext: options.projectContext,
          timeout: options.timeout,
          contextManager: this.contextManager,
          onOutput: options.onOutput,
          onReport: options.onReport,
          reportTimeout: options.reportTimeout,
          getSupplementaryInstructions: options.getSupplementaryInstructions
            ? () => options.getSupplementaryInstructions!(assignment.workerId)
            : undefined,
        });

        // 收集详细执行结果
        if (result.fullResult) {
          this.collectedAssignmentResults.set(assignment.id, result.fullResult);
        }

        if (result.success) {
          if (result.hasPendingApprovals) {
            anyPendingApprovals = true;
          } else {
            completedCount++;
          }
          this.progressReporter.reportAssignmentComplete(
            assignment,
            result.completedTodos,
            result.tokenUsage
          );
        } else {
          errors.push(...result.errors);
        }

        this.markAssignmentComplete(assignment, result);
      }
    }

    return this.buildResult(errors.length === 0, errors, completedCount, anyPendingApprovals);
  }

  /**
   * 并行执行
   */
  private async executeParallel(options: ExecutionOptions): Promise<ExecutionResult> {
    const errors: string[] = [];
    let completedCount = 0;
    let anyPendingApprovals = false;

    // 按依赖关系分组
    const groups = this.groupByDependencies();

    for (const group of groups) {
      const promises = group.map(async (assignment) => {
        this.progressReporter.reportAssignmentStart(assignment);
        this.markAssignmentStart(assignment);

        const result = await this.assignmentExecutor.execute(this.mission, assignment, {
          workingDirectory: options.workingDirectory,
          projectContext: options.projectContext,
          timeout: options.timeout,
          contextManager: this.contextManager,
          onOutput: options.onOutput,
          onReport: options.onReport,
          reportTimeout: options.reportTimeout,
          getSupplementaryInstructions: options.getSupplementaryInstructions
            ? () => options.getSupplementaryInstructions!(assignment.workerId)
            : undefined,
        });

        // 收集详细执行结果
        if (result.fullResult) {
          this.collectedAssignmentResults.set(assignment.id, result.fullResult);
        }

        if (result.success) {
          this.progressReporter.reportAssignmentComplete(
            assignment,
            result.completedTodos,
            result.tokenUsage
          );
          this.markAssignmentComplete(assignment, result);
          return { success: true, errors: [] as string[], hasPendingApprovals: result.hasPendingApprovals };
        } else {
          this.markAssignmentComplete(assignment, result);
          return { success: false, errors: result.errors };
        }
      });

      const results = await Promise.all(promises);
      results.forEach(result => {
        if (result.success) {
          if (result.hasPendingApprovals) {
            anyPendingApprovals = true;
          } else {
            completedCount++;
          }
        } else {
          errors.push(...result.errors);
        }
      });
    }

    return this.buildResult(errors.length === 0, errors, completedCount, anyPendingApprovals);
  }

  private markAssignmentStart(assignment: Assignment): void {
    assignment.status = 'executing';
    assignment.startedAt = assignment.startedAt || Date.now();
    assignment.progress = this.calculateAssignmentProgress(assignment);
    this.emit('assignmentStarted', { assignmentId: assignment.id, missionId: this.mission.id, workerId: assignment.workerId });
  }

  private markAssignmentComplete(
    assignment: Assignment,
    result: AssignmentExecutionResult
  ): void {
    assignment.status = result.success ? 'completed' : 'failed';
    assignment.completedAt = Date.now();
    assignment.progress = this.calculateAssignmentProgress(assignment);
    this.emit('assignmentCompleted', {
      assignmentId: assignment.id,
      missionId: this.mission.id,
      workerId: assignment.workerId,
      success: result.success,
    });
  }

  private calculateAssignmentProgress(assignment: Assignment): number {
    const total = assignment.todos?.length || 0;
    if (!total) {
      return assignment.status === 'completed' ? 100 : 0;
    }
    const done = assignment.todos.filter(t => t.status === 'completed' || t.status === 'skipped').length;
    return Math.min(100, Math.round((done / total) * 100));
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
   * 检查是否存在契约依赖
   */
  private hasContractDependencies(): boolean {
    return this.mission.assignments.some(
      (a) => a.consumerContracts.length > 0 || a.producerContracts.length > 0
    );
  }

  /**
   * 使用 Wave 并行分组执行（提案 4.6）
   *
   * Wave 执行策略：
   * 1. 使用 TaskDependencyGraph 分析 Assignment 依赖
   * 2. 基于 Contract 依赖和文件冲突自动分组
   * 3. 每个 Wave 内部并行执行，Wave 之间顺序执行
   * 4. 发射 waveStarted/waveCompleted 事件供 UI 显示
   */
  private async executeWithWaves(options: ExecutionOptions): Promise<ExecutionResult> {
    const errors: string[] = [];
    let completedCount = 0;

    // 构建依赖图
    const graph = this.buildDependencyGraph();

    // 分析得到执行批次（Waves）
    const analysis = graph.analyze();

    if (analysis.hasCycle) {
      logger.warn(
        LogCategory.ORCHESTRATOR,
        `检测到循环依赖，切换为普通并行执行: ${analysis.cycleNodes?.join(', ')}`
      );
      return this.executeParallel(options);
    }

    const waves = analysis.executionBatches;
    const totalWaves = waves.length;

    logger.info(
      LogCategory.ORCHESTRATOR,
      `Wave 执行: ${totalWaves} 个 Wave，关键路径长度 ${analysis.criticalPath.length}`
    );

    // 发射 Wave 执行开始事件
    this.emit('waveExecutionStarted', {
      missionId: this.mission.id,
      totalWaves,
      waves: waves.map((w) => w.taskIds),
      criticalPath: analysis.criticalPath,
    });

    let completedWaves = 0;

    for (const wave of waves) {
      const waveIndex = wave.batchIndex;
      const assignmentIds = wave.taskIds;
      const waveAssignments = this.mission.assignments.filter((a) =>
        assignmentIds.includes(a.id)
      );

      logger.info(
        LogCategory.ORCHESTRATOR,
        `开始 Wave ${waveIndex + 1}/${totalWaves}: ${waveAssignments.length} 个任务`
      );

      // 发射 Wave 开始事件
      this.emit('waveStarted', {
        missionId: this.mission.id,
        waveIndex,
        totalWaves,
        assignmentIds,
      });

      // Wave 内部并行执行
      const promises = waveAssignments.map(async (assignment) => {
        this.progressReporter.reportAssignmentStart(assignment);
        this.markAssignmentStart(assignment);

        const result = await this.assignmentExecutor.execute(this.mission, assignment, {
          workingDirectory: options.workingDirectory,
          projectContext: options.projectContext,
          timeout: options.timeout,
          contextManager: this.contextManager,
          onOutput: options.onOutput,
          onReport: options.onReport,
          reportTimeout: options.reportTimeout,
          getSupplementaryInstructions: options.getSupplementaryInstructions
            ? () => options.getSupplementaryInstructions!(assignment.workerId)
            : undefined,
        });

        // 收集详细执行结果
        if (result.fullResult) {
          this.collectedAssignmentResults.set(assignment.id, result.fullResult);
        }

        if (result.success) {
          this.progressReporter.reportAssignmentComplete(
            assignment,
            result.completedTodos,
            result.tokenUsage
          );
          this.markAssignmentComplete(assignment, result);
          return { success: true, errors: [] as string[], assignmentId: assignment.id };
        } else {
          this.markAssignmentComplete(assignment, result);
          return { success: false, errors: result.errors, assignmentId: assignment.id };
        }
      });

      const results = await Promise.all(promises);

      const waveSuccessCount = results.filter((r) => r.success).length;
      completedCount += waveSuccessCount;

      results.forEach((result) => {
        if (!result.success) {
          errors.push(...result.errors);
        }
      });

      completedWaves++;

      // 发射 Wave 完成事件
      this.emit('waveCompleted', {
        missionId: this.mission.id,
        waveIndex,
        totalWaves,
        completedCount: waveSuccessCount,
        failedCount: results.length - waveSuccessCount,
      });

      logger.info(
        LogCategory.ORCHESTRATOR,
        `完成 Wave ${waveIndex + 1}/${totalWaves}: 成功 ${waveSuccessCount}/${waveAssignments.length}`
      );
    }

    // 构建带 Wave 信息的结果
    const waveInfo: WaveExecutionInfo = {
      totalWaves,
      completedWaves,
      waves: waves.map((w) => w.taskIds),
      criticalPath: analysis.criticalPath,
    };

    return this.buildResultWithWave(errors.length === 0, errors, completedCount, waveInfo);
  }

  /**
   * 构建依赖图
   */
  private buildDependencyGraph(): TaskDependencyGraph {
    const graph = new TaskDependencyGraph();

    // 添加所有 Assignment 作为节点
    for (const assignment of this.mission.assignments) {
      graph.addTask(
        assignment.id,
        assignment.responsibility,
        assignment,
        assignment.scope?.targetPaths || []
      );
    }

    // 基于 Contract 添加依赖关系
    for (const assignment of this.mission.assignments) {
      for (const contractId of assignment.consumerContracts) {
        // 找到产出该 Contract 的 Assignment
        const producer = this.mission.assignments.find((a) =>
          a.producerContracts.includes(contractId)
        );
        if (producer) {
          graph.addDependency(assignment.id, producer.id);
        }
      }
    }

    // 基于文件冲突自动添加依赖
    graph.addFileDependencies('sequential');

    return graph;
  }

  /**
   * 构建带 Wave 信息的执行结果
   */
  private buildResultWithWave(
    success: boolean,
    errors: string[],
    completedCount: number,
    waveInfo: WaveExecutionInfo
  ): ExecutionResult {
    const progress = this.progressReporter.getProgress();

    return {
      success,
      completedAssignments: completedCount,
      totalAssignments: progress.totalAssignments,
      errors,
      tokenUsage: progress.tokenUsage,
      assignmentResults: new Map(this.collectedAssignmentResults),
      waveInfo,
    };
  }

  /**
   * 构建执行结果
   */
  private buildResult(
    success: boolean,
    errors: string[],
    completedCount?: number,
    hasPendingApprovals?: boolean
  ): ExecutionResult {
    const progress = this.progressReporter.getProgress();

    return {
      success,
      completedAssignments: completedCount ?? progress.completedAssignments,
      totalAssignments: progress.totalAssignments,
      errors,
      tokenUsage: progress.tokenUsage,
      assignmentResults: new Map(this.collectedAssignmentResults),
      hasPendingApprovals,
    };
  }

  /**
   * 销毁协调器（清理资源）
   */
  dispose(): void {
    this.collectedAssignmentResults.clear();
    this.removeAllListeners();
    logger.debug('执行协调器.销毁', undefined, LogCategory.ORCHESTRATOR);
  }
}
