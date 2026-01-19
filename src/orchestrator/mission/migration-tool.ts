/**
 * Mission Migration Tool
 *
 * 将旧的 PlanRecord + ExecutionPlan + SubTask 数据结构
 * 迁移到新的 Mission + Assignment + WorkerTodo 数据结构
 *
 * 设计原则：
 * - 只进行一次性迁移，不保留兼容层
 * - 迁移失败不应阻塞新系统运行
 * - 迁移后旧数据保留备份但不再使用
 */

import * as fs from 'fs';
import * as path from 'path';
import { logger, LogCategory } from '../../logging';
import { CLIType, SubTask, SubTaskStatus } from '../../types';
import { PlanRecord, PlanStorage } from '../plan-storage';
import { ExecutionPlan } from '../protocols/types';
import {
  Mission,
  MissionStatus,
  MissionPhase,
  Assignment,
  AssignmentStatus,
  WorkerTodo,
  TodoStatus,
  RiskLevel,
  ExecutionPath,
  Constraint,
  AcceptanceCriterion,
  AssignmentScope,
  AssignmentReason,
} from './types';
import { MissionStorageManager, createFileBasedMissionStorage } from './mission-storage';

/**
 * 迁移统计
 */
export interface MigrationStats {
  totalPlans: number;
  successfulMigrations: number;
  failedMigrations: number;
  skippedMigrations: number;
  errors: string[];
}

/**
 * 迁移选项
 */
export interface MigrationOptions {
  /** 是否进行试运行（不实际写入） */
  dryRun?: boolean;
  /** 是否备份旧数据 */
  backupOld?: boolean;
  /** 是否覆盖已存在的 Mission */
  overwrite?: boolean;
  /** 进度回调 */
  onProgress?: (current: number, total: number, planId: string) => void;
}

/**
 * 迁移工具
 */
export class MissionMigrationTool {
  private planStorage: PlanStorage;
  private missionStorage: MissionStorageManager;

  constructor(workspaceRoot: string) {
    this.planStorage = new PlanStorage(workspaceRoot);
    const sessionsDir = path.join(workspaceRoot, '.multicli', 'sessions');
    this.missionStorage = createFileBasedMissionStorage(sessionsDir);
  }

  /**
   * 迁移指定会话的所有 Plan
   */
  async migrateSession(sessionId: string, options: MigrationOptions = {}): Promise<MigrationStats> {
    const stats: MigrationStats = {
      totalPlans: 0,
      successfulMigrations: 0,
      failedMigrations: 0,
      skippedMigrations: 0,
      errors: [],
    };

    const plans = this.planStorage.listPlansForSession(sessionId);
    stats.totalPlans = plans.length;

    for (let i = 0; i < plans.length; i++) {
      const plan = plans[i];

      if (options.onProgress) {
        options.onProgress(i + 1, plans.length, plan.id);
      }

      try {
        // 检查是否已存在
        const existingMission = await this.missionStorage.load(plan.id);
        if (existingMission && !options.overwrite) {
          stats.skippedMigrations++;
          continue;
        }

        // 执行迁移
        const mission = this.convertPlanToMission(plan);

        if (!options.dryRun) {
          await this.missionStorage.save(mission);
        }

        stats.successfulMigrations++;
      } catch (error) {
        const errorMessage = error instanceof Error ? error.message : String(error);
        stats.failedMigrations++;
        stats.errors.push(`Plan ${plan.id}: ${errorMessage}`);
        logger.warn('编排器.迁移.失败', { planId: plan.id, error: errorMessage }, LogCategory.ORCHESTRATOR);
      }
    }

    return stats;
  }

  /**
   * 将 PlanRecord 转换为 Mission
   */
  convertPlanToMission(record: PlanRecord): Mission {
    const plan = record.plan;

    // 1. 转换风险等级
    const riskLevel = this.convertRiskLevel(plan.riskLevel);

    // 2. 转换约束条件
    const constraints = this.extractConstraints(plan);

    // 3. 转换验收标准
    const acceptanceCriteria = this.convertAcceptanceCriteria(plan.acceptanceCriteria);

    // 4. 转换 SubTask 到 Assignment
    const assignments = plan.subTasks.map(subTask =>
      this.convertSubTaskToAssignment(subTask, record.id)
    );

    // 5. 确定状态和阶段
    const { status, phase } = this.determineStatusAndPhase(record, plan);

    // 6. 构建 Mission
    const mission: Mission = {
      id: record.id,
      sessionId: record.sessionId,

      // 目标定义
      userPrompt: record.prompt,
      goal: plan.summary || record.prompt.substring(0, 100),
      analysis: plan.analysis || '',
      context: '',

      // 约束与验收
      constraints,
      acceptanceCriteria,

      // 协作定义（契约在新系统中动态生成）
      contracts: [],
      assignments,

      // 风险评估
      riskLevel,
      riskFactors: this.extractRiskFactors(plan),
      executionPath: this.convertExecutionPath(riskLevel),

      // 状态
      status,
      phase,

      // 时间戳
      createdAt: record.createdAt,
      updatedAt: record.updatedAt,
      startedAt: record.createdAt,
      completedAt: status === 'completed' ? record.updatedAt : undefined,
    };

    return mission;
  }

  /**
   * 将 SubTask 转换为 Assignment
   */
  private convertSubTaskToAssignment(subTask: SubTask, missionId: string): Assignment {
    // 转换 SubTask 的 status 到 AssignmentStatus
    const status = this.convertSubTaskStatus(subTask.status);

    // 创建职责分配原因
    const assignmentReason: AssignmentReason = {
      profileMatch: {
        category: 'legacy_migration',
        score: 0.5,
        matchedKeywords: [],
      },
      contractRole: 'none',
      explanation: '从旧系统迁移的任务分配',
      alternatives: [],
    };

    // 创建职责范围
    const scope: AssignmentScope = {
      includes: [subTask.description],
      excludes: [],
      targetPaths: subTask.targetFiles || [],
    };

    // 创建 Todo（每个 SubTask 对应一个 Todo）
    const todo = this.convertSubTaskToTodo(subTask, `${missionId}-${subTask.id}`);

    const assignment: Assignment = {
      id: subTask.id,
      missionId,

      // Worker 分配（SubTask 使用 assignedWorker）
      workerId: subTask.assignedWorker,
      assignmentReason,

      // 职责定义
      responsibility: subTask.description,
      scope,
      guidancePrompt: '',

      // 契约关联（新系统中动态生成）
      producerContracts: [],
      consumerContracts: [],

      // Worker 规划
      todos: [todo],
      planningStatus: 'approved', // 旧数据已经执行过，视为已批准

      // 状态
      status,
      progress: status === 'completed' ? 100 : 0,

      // 时间戳
      createdAt: Date.now(),
      startedAt: subTask.startedAt,
      completedAt: subTask.completedAt,
    };

    return assignment;
  }

  /**
   * 将 SubTask 转换为 WorkerTodo
   */
  private convertSubTaskToTodo(subTask: SubTask, assignmentId: string): WorkerTodo {
    const status = this.convertSubTaskStatusToTodoStatus(subTask.status);

    const todo: WorkerTodo = {
      id: `todo-${subTask.id}`,
      assignmentId,

      // 内容
      content: subTask.description,
      reasoning: '从旧系统迁移',
      expectedOutput: '完成任务',

      // 分类
      type: 'implementation',
      priority: subTask.priority || 3,

      // 范围检查
      outOfScope: false,

      // 依赖（SubTask 使用 dependencies 而非 dependsOn）
      dependsOn: subTask.dependencies || [],
      requiredContracts: [],
      producesContracts: [],

      // 状态
      status,

      // 执行结果（SubTask.result 是 WorkerResult 类型）
      output: subTask.result ? {
        success: subTask.status === 'completed',
        summary: subTask.result.output || '执行完成',
        modifiedFiles: subTask.modifiedFiles || [],
        duration: (subTask.completedAt || Date.now()) - (subTask.startedAt || Date.now()),
      } : undefined,

      // 时间戳
      createdAt: Date.now(),
      startedAt: subTask.startedAt,
      completedAt: subTask.completedAt,
    };

    return todo;
  }

  /**
   * 转换风险等级
   */
  private convertRiskLevel(level?: string): RiskLevel {
    switch (level) {
      case 'critical':
      case 'high':
        return 'high';
      case 'medium':
        return 'medium';
      default:
        return 'low';
    }
  }

  /**
   * 转换执行路径
   */
  private convertExecutionPath(riskLevel: RiskLevel): ExecutionPath {
    switch (riskLevel) {
      case 'high':
        return 'full';
      case 'medium':
        return 'standard';
      default:
        return 'light';
    }
  }

  /**
   * 提取约束条件
   */
  private extractConstraints(plan: ExecutionPlan): Constraint[] {
    const constraints: Constraint[] = [];

    // 从 featureContract 提取约束
    if (plan.featureContract) {
      constraints.push({
        id: `constraint-contract`,
        type: 'must',
        description: plan.featureContract,
        source: 'system',
      });
    }

    return constraints;
  }

  /**
   * 转换验收标准
   */
  private convertAcceptanceCriteria(criteria?: string[]): AcceptanceCriterion[] {
    if (!criteria || criteria.length === 0) {
      return [];
    }

    return criteria.map((criterion, index) => ({
      id: `criterion-${index}`,
      description: criterion,
      verifiable: true,
      verificationMethod: 'manual' as const,
      status: 'pending' as const,
    }));
  }

  /**
   * 提取风险因素
   */
  private extractRiskFactors(plan: ExecutionPlan): string[] {
    const factors: string[] = [];

    if (plan.needsCollaboration) {
      factors.push('需要多 Worker 协作');
    }

    if (plan.subTasks.length > 3) {
      factors.push(`任务较多 (${plan.subTasks.length} 个子任务)`);
    }

    return factors;
  }

  /**
   * 确定状态和阶段
   */
  private determineStatusAndPhase(record: PlanRecord, plan: ExecutionPlan): {
    status: MissionStatus;
    phase: MissionPhase;
  } {
    // 根据 review 状态判断
    if (record.review) {
      if (record.review.status === 'rejected') {
        return { status: 'failed', phase: 'plan_review' };
      }
    }

    // 检查所有子任务状态（SubTaskStatus 使用 'running' 而非 'in_progress'）
    const allCompleted = plan.subTasks.every(t => t.status === 'completed');
    const anyFailed = plan.subTasks.some(t => t.status === 'failed');
    const anyRunning = plan.subTasks.some(t => t.status === 'running');

    if (allCompleted) {
      return { status: 'completed', phase: 'summary' };
    }

    if (anyFailed) {
      return { status: 'failed', phase: 'execution' };
    }

    if (anyRunning) {
      return { status: 'executing', phase: 'execution' };
    }

    return { status: 'pending_approval', phase: 'plan_review' };
  }

  /**
   * 转换 SubTask 状态到 AssignmentStatus
   */
  private convertSubTaskStatus(status?: SubTaskStatus): AssignmentStatus {
    switch (status) {
      case 'completed':
        return 'completed';
      case 'failed':
        return 'failed';
      case 'running':
      case 'retrying':
        return 'executing';
      default:
        return 'pending';
    }
  }

  /**
   * 转换 SubTask 状态到 TodoStatus
   */
  private convertSubTaskStatusToTodoStatus(status?: SubTaskStatus): TodoStatus {
    switch (status) {
      case 'completed':
        return 'completed';
      case 'failed':
        return 'failed';
      case 'running':
      case 'retrying':
        return 'in_progress';
      case 'skipped':
        return 'skipped';
      case 'paused':
        return 'blocked';
      default:
        return 'pending';
    }
  }
}

/**
 * 快速迁移函数
 */
export async function migrateSessionPlans(
  workspaceRoot: string,
  sessionId: string,
  options?: MigrationOptions
): Promise<MigrationStats> {
  const tool = new MissionMigrationTool(workspaceRoot);
  return tool.migrateSession(sessionId, options);
}

/**
 * 迁移所有会话
 */
export async function migrateAllSessions(
  workspaceRoot: string,
  options?: MigrationOptions
): Promise<Map<string, MigrationStats>> {
  const results = new Map<string, MigrationStats>();
  const sessionsDir = path.join(workspaceRoot, '.multicli', 'sessions');

  if (!fs.existsSync(sessionsDir)) {
    return results;
  }

  const sessionIds = fs.readdirSync(sessionsDir).filter(name => {
    const sessionPath = path.join(sessionsDir, name);
    return fs.statSync(sessionPath).isDirectory();
  });

  for (const sessionId of sessionIds) {
    try {
      const stats = await migrateSessionPlans(workspaceRoot, sessionId, options);
      results.set(sessionId, stats);
    } catch (error) {
      const errorMessage = error instanceof Error ? error.message : String(error);
      results.set(sessionId, {
        totalPlans: 0,
        successfulMigrations: 0,
        failedMigrations: 0,
        skippedMigrations: 0,
        errors: [errorMessage],
      });
    }
  }

  return results;
}
