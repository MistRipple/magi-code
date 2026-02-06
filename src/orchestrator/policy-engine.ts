/**
 * PolicyEngine - 统一策略引擎
 * 集中管理所有策略决策：Worker选择、风险评估、验证策略、冲突检测等
 *
 * v0.7.0: 集成 ProfileLoader，移除硬编码的任务映射
 */

import { EventEmitter } from 'events';
import { WorkerSlot, SubTask } from '../types';
import { RiskPolicy, RiskAssessment, RiskLevel, VerificationLevel } from './risk-policy';
import { ExecutionPlan } from './protocols/types';
import { VerificationConfig } from './verification-runner';
import { ProfileLoader } from './profile/profile-loader';
import { CategoryResolver } from './profile/category-resolver';
import { AssignmentResolver } from './profile/assignment-resolver';
import { WorkerAssignmentLoader } from './profile/worker-assignments';

/** Worker 选择策略 */
export interface WorkerSelectionPolicy {
  /** 推荐的 Worker */
  recommendedWorker: WorkerSlot;
  /** 候选 Worker 列表 */
  alternativeWorkers: WorkerSlot[];
  /** 选择原因 */
  reason: string;
  /** 置信度 (0-1) */
  confidence: number;
}


/** 冲突检测结果 */
export interface ConflictDetectionResult {
  /** 是否存在冲突 */
  hasConflict: boolean;
  /** 冲突的文件列表 */
  conflictingFiles: string[];
  /** 冲突的任务对 */
  conflictingTasks: Array<{ task1: string; task2: string; files: string[] }>;
  /** 建议的执行顺序 */
  suggestedOrder?: string[];
}

/** 验证策略决策 */
export interface VerificationDecision {
  /** 是否需要验证 */
  shouldVerify: boolean;
  /** 验证配置 */
  config: Partial<VerificationConfig>;
  /** 决策原因 */
  reason: string;
}

/** Worker 健康状态 */
export interface WorkerHealthStatus {
  worker: WorkerSlot;
  available: boolean;
  successRate: number;
  avgResponseTime: number;
  lastError?: string;
  lastSuccessAt?: number;
}


/**
 * 统一策略引擎
 * 集成 ProfileLoader，从配置读取任务分类和 Worker 映射
 */
export class PolicyEngine extends EventEmitter {
  private riskPolicy: RiskPolicy;
  private workerHealthStatus: Map<WorkerSlot, WorkerHealthStatus> = new Map();
  private executionHistory: Array<{ worker: WorkerSlot; success: boolean; duration: number }> = [];

  /** 画像加载器 */
  private profileLoader?: ProfileLoader;
  private categoryResolver = new CategoryResolver();
  private assignmentResolver: AssignmentResolver;

  constructor(profileLoader?: ProfileLoader) {
    super();
    this.riskPolicy = new RiskPolicy();
    this.profileLoader = profileLoader;
    const assignmentLoader = profileLoader
      ? profileLoader.getAssignmentLoader()
      : new WorkerAssignmentLoader();
    this.assignmentResolver = new AssignmentResolver(assignmentLoader);
    this.initializeWorkerHealth();
  }

  /**
   * 设置画像加载器（支持延迟注入）
   */
  setProfileLoader(loader: ProfileLoader): void {
    this.profileLoader = loader;
    this.assignmentResolver = new AssignmentResolver(loader.getAssignmentLoader());
  }

  /** 初始化 Worker 健康状态 */
  private initializeWorkerHealth(): void {
    const workers: WorkerSlot[] = ['claude', 'codex', 'gemini'];
    for (const worker of workers) {
      this.workerHealthStatus.set(worker, {
        worker,
        available: true,
        successRate: 1.0,
        avgResponseTime: 0,
      });
    }
  }

  // ========== Worker 选择策略 ==========

  /**
   * 根据任务特征选择最佳 Worker
   * 从 ProfileLoader 读取任务分类配置
   */
  selectWorker(task: SubTask, availableWorkers?: WorkerSlot[]): WorkerSelectionPolicy {
    const available = availableWorkers || this.getAvailableWorkers();
    const taskType = this.resolveTaskType(task);
    const assignedWorker = this.assignmentResolver.resolveWorker(taskType);

    if (available.length === 0) {
      throw new Error(`无可用 Worker（分类: ${taskType}）`);
    }
    if (!available.includes(assignedWorker)) {
      throw new Error(`分类 "${taskType}" 归属 ${assignedWorker}，但当前不可用`);
    }

    return {
      recommendedWorker: assignedWorker,
      alternativeWorkers: [],
      reason: `任务类型 "${taskType}" 归属 ${assignedWorker}`,
      confidence: this.calculateConfidence(assignedWorker),
    };
  }

  /** 推断任务类型
   * 使用 CategoryResolver 的唯一规则
   */
  private resolveTaskType(task: SubTask): string {
    const description = (task.description || '').toLowerCase();
    const title = (task.title || '').toLowerCase();
    const combinedText = `${description} ${title}`;

    return this.categoryResolver.resolveFromText(combinedText);
  }

  /** 根据健康状态排序 Worker */
  private sortByHealth(workers: WorkerSlot[]): WorkerSlot[] {
    return [...workers].sort((a, b) => {
      const healthA = this.workerHealthStatus.get(a);
      const healthB = this.workerHealthStatus.get(b);
      if (!healthA || !healthB) return 0;

      // 优先考虑可用性
      if (healthA.available !== healthB.available) {
        return healthA.available ? -1 : 1;
      }

      // 然后考虑成功率
      return healthB.successRate - healthA.successRate;
    });
  }

  /** 计算置信度
   * 基于 ProfileLoader 配置和健康状态
   */
  private calculateConfidence(worker: WorkerSlot): number {
    const health = this.workerHealthStatus.get(worker);
    const healthFactor = health?.successRate || 0.5;
    const baseConfidence = 0.9;
    return Math.min(1, baseConfidence * healthFactor);
  }

  /** 获取可用的 Worker 列表 */
  getAvailableWorkers(): WorkerSlot[] {
    const available: WorkerSlot[] = [];
    for (const [worker, status] of this.workerHealthStatus) {
      if (status.available && status.successRate > 0.3) {
        available.push(worker);
      }
    }
    return available;
  }

  /** 更新 Worker 健康状态 */
  updateWorkerHealth(worker: WorkerSlot, success: boolean, responseTime: number, error?: string): void {
    const status = this.workerHealthStatus.get(worker);
    if (!status) return;

    // 记录执行历史
    this.executionHistory.push({ worker, success, duration: responseTime });
    if (this.executionHistory.length > 100) {
      this.executionHistory.shift();
    }

    // 计算最近的成功率
    const recentHistory = this.executionHistory.filter(h => h.worker === worker).slice(-20);
    const successCount = recentHistory.filter(h => h.success).length;
    status.successRate = recentHistory.length > 0 ? successCount / recentHistory.length : 1;

    // 更新平均响应时间
    const avgTime = recentHistory.reduce((sum, h) => sum + h.duration, 0) / recentHistory.length;
    status.avgResponseTime = avgTime;

    // 更新错误信息
    if (!success && error) {
      status.lastError = error;
    }
    if (success) {
      status.lastSuccessAt = Date.now();
    }

    // 如果连续失败太多，标记为不可用
    const recentFailures = recentHistory.slice(-5).filter(h => !h.success).length;
    status.available = recentFailures < 5;

    this.emit('workerHealthUpdated', { worker, status });
  }



  // ========== 冲突检测策略 ==========

  /**
   * 检测任务之间的文件冲突
   */
  detectConflicts(tasks: SubTask[]): ConflictDetectionResult {
    const result: ConflictDetectionResult = {
      hasConflict: false,
      conflictingFiles: [],
      conflictingTasks: [],
    };

    // 构建文件到任务的映射
    const fileToTasks = new Map<string, string[]>();
    for (const task of tasks) {
      const files = task.targetFiles || [];
      // 同时考虑 conflictDomain
      const domain = task.conflictDomain;
      const allFiles = domain ? [...files, `__domain__:${domain}`] : files;

      for (const file of allFiles) {
        const normalized = this.normalizeFilePath(file);
        const existing = fileToTasks.get(normalized) || [];
        existing.push(task.id);
        fileToTasks.set(normalized, existing);
      }
    }

    // 检测冲突
    const conflictingFilesSet = new Set<string>();
    for (const [file, taskIds] of fileToTasks) {
      if (taskIds.length > 1) {
        result.hasConflict = true;
        if (!file.startsWith('__domain__:')) {
          conflictingFilesSet.add(file);
        }

        // 记录冲突的任务对
        for (let i = 0; i < taskIds.length; i++) {
          for (let j = i + 1; j < taskIds.length; j++) {
            const existingPair = result.conflictingTasks.find(
              p => (p.task1 === taskIds[i] && p.task2 === taskIds[j]) ||
                   (p.task1 === taskIds[j] && p.task2 === taskIds[i])
            );
            if (existingPair) {
              if (!file.startsWith('__domain__:')) {
                existingPair.files.push(file);
              }
            } else {
              result.conflictingTasks.push({
                task1: taskIds[i],
                task2: taskIds[j],
                files: file.startsWith('__domain__:') ? [] : [file],
              });
            }
          }
        }
      }
    }

    result.conflictingFiles = Array.from(conflictingFilesSet);

    // 如果有冲突，建议执行顺序
    if (result.hasConflict) {
      result.suggestedOrder = this.suggestExecutionOrder(tasks, result.conflictingTasks);
    }

    return result;
  }

  /** 标准化文件路径 */
  private normalizeFilePath(file: string): string {
    return file.replace(/\\/g, '/').replace(/^\.\//, '').toLowerCase();
  }

  /** 建议执行顺序（基于依赖和冲突） */
  private suggestExecutionOrder(
    tasks: SubTask[],
    conflicts: Array<{ task1: string; task2: string; files: string[] }>
  ): string[] {
    // 简单策略：按优先级排序，冲突的任务串行执行
    const taskMap = new Map(tasks.map(t => [t.id, t]));
    const sorted = [...tasks].sort((a, b) => {
      // 优先级高的先执行
      const priorityA = a.priority || 5;
      const priorityB = b.priority || 5;
      if (priorityA !== priorityB) return priorityA - priorityB;

      // 依赖少的先执行
      return (a.dependencies?.length || 0) - (b.dependencies?.length || 0);
    });

    return sorted.map(t => t.id);
  }

  /**
   * 为任务分配冲突域
   */
  assignConflictDomain(task: SubTask): string | undefined {
    const files = task.targetFiles || [];
    if (files.length === 0) return undefined;

    // 根据文件路径推断冲突域
    const domains = new Set<string>();
    for (const file of files) {
      const normalized = this.normalizeFilePath(file);
      const parts = normalized.split('/');

      // 使用第一级目录作为冲突域
      if (parts.length > 1) {
        domains.add(parts[0]);
      }
    }

    // 如果所有文件在同一目录，使用该目录作为冲突域
    if (domains.size === 1) {
      return Array.from(domains)[0];
    }

    // 多个目录，使用组合域
    if (domains.size > 1) {
      return Array.from(domains).sort().join('+');
    }

    return undefined;
  }

  // ========== 风险评估策略 ==========

  /**
   * 评估执行计划的风险
   */
  assessRisk(prompt: string, plan: ExecutionPlan): RiskAssessment {
    return this.riskPolicy.evaluate(prompt, plan);
  }

  /**
   * 根据风险等级决定验证策略
   */
  decideVerification(riskAssessment: RiskAssessment, modifiedFiles?: string[]): VerificationDecision {
    const { level, verification } = riskAssessment;

    // 基础配置
    const config: Partial<VerificationConfig> = {
      compileCheck: true,
      ideCheck: true,
      lintCheck: false,
      testCheck: false,
    };

    let reason = '';

    switch (level) {
      case 'low':
        // 低风险：仅基础检查
        config.compileCheck = true;
        config.ideCheck = true;
        reason = '低风险任务，执行基础编译和 IDE 诊断检查';
        break;

      case 'medium':
        // 中风险：添加 Lint 检查
        config.compileCheck = true;
        config.ideCheck = true;
        config.lintCheck = true;
        reason = '中风险任务，追加 Lint 检查';
        break;

      case 'high':
        // 高风险：完整验证
        config.compileCheck = true;
        config.ideCheck = true;
        config.lintCheck = true;
        config.testCheck = true;
        reason = '高风险任务，执行完整验证（编译 + IDE + Lint + 测试）';
        break;
    }

    // 根据修改的文件类型调整
    if (modifiedFiles) {
      const hasTestFiles = modifiedFiles.some(f =>
        f.includes('.test.') || f.includes('.spec.') || f.includes('__tests__')
      );
      if (hasTestFiles && !config.testCheck) {
        config.testCheck = true;
        reason += '；检测到测试文件变更，追加测试检查';
      }
    }

    return {
      shouldVerify: verification !== 'none',
      config,
      reason,
    };
  }


  // ========== Hard Stop 决策 ==========

  /**
   * 决定是否需要 Hard Stop（用户确认）
   */
  shouldHardStop(riskAssessment: RiskAssessment, mode: 'ask' | 'auto'): boolean {
    // Auto 模式跳过 Hard Stop
    if (mode === 'auto') return false;

    // Ask 模式不执行任务，无需 Hard Stop
    if (mode === 'ask') return false;

    // 默认不需要 Hard Stop
    return false;
  }

  // ========== 替代策略 ==========

  /**
   * 获取 Worker 候选顺序
   */
  getAlternativeOrder(primaryWorker: WorkerSlot): WorkerSlot[] {
    const allWorkers: WorkerSlot[] = ['claude', 'codex', 'gemini'];
    const available = this.getAvailableWorkers();

    // 移除主 Worker，按健康状态排序剩余候选
    const alternatives = allWorkers
      .filter(w => w !== primaryWorker && available.includes(w));

    return this.sortByHealth(alternatives);
  }

  /**
   * 处理 Worker 失败，返回下一个可用的 Worker
   */
  handleWorkerFailure(failedWorker: WorkerSlot, error: string): WorkerSlot | null {
    // 更新健康状态
    this.updateWorkerHealth(failedWorker, false, 0, error);

    // 获取候选选项
    const alternatives = this.getAlternativeOrder(failedWorker);
    return alternatives.length > 0 ? alternatives[0] : null;
  }

  // ========== 执行策略 ==========

  /**
   * 决定任务的执行策略（并行/串行）
   */
  decideExecutionStrategy(tasks: SubTask[]): {
    parallel: string[][];  // 可并行执行的任务组
    serial: string[];      // 必须串行执行的任务
  } {
    const conflicts = this.detectConflicts(tasks);
    const parallel: string[][] = [];
    const serial: string[] = [];

    if (!conflicts.hasConflict) {
      // 无冲突，所有任务可并行
      parallel.push(tasks.map(t => t.id));
    } else {
      // 有冲突，冲突任务串行，其他并行
      const conflictingTaskIds = new Set<string>();
      for (const pair of conflicts.conflictingTasks) {
        conflictingTaskIds.add(pair.task1);
        conflictingTaskIds.add(pair.task2);
      }

      const parallelTasks = tasks.filter(t => !conflictingTaskIds.has(t.id));
      const serialTasks = tasks.filter(t => conflictingTaskIds.has(t.id));

      if (parallelTasks.length > 0) {
        parallel.push(parallelTasks.map(t => t.id));
      }

      // 串行任务按建议顺序排列
      if (conflicts.suggestedOrder) {
        serial.push(...conflicts.suggestedOrder.filter(id => conflictingTaskIds.has(id)));
      } else {
        serial.push(...serialTasks.map(t => t.id));
      }
    }

    return { parallel, serial };
  }

  // ========== 统计和报告 ==========

  /**
   * 获取策略引擎状态报告
   */
  getStatusReport(): {
    workerHealth: WorkerHealthStatus[];
    recentExecutions: number;
    overallSuccessRate: number;
  } {
    const workerHealth = Array.from(this.workerHealthStatus.values());
    const recentExecutions = this.executionHistory.length;
    const successCount = this.executionHistory.filter(h => h.success).length;
    const overallSuccessRate = recentExecutions > 0 ? successCount / recentExecutions : 1;

    return {
      workerHealth,
      recentExecutions,
      overallSuccessRate,
    };
  }

  /**
   * 重置策略引擎状态
   */
  reset(): void {
    this.initializeWorkerHealth();
    this.executionHistory = [];
  }
}
