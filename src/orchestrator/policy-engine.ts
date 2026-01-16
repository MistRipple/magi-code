/**
 * PolicyEngine - 统一策略引擎
 * 集中管理所有策略决策：CLI选择、风险评估、验证策略、冲突检测等
 */

import { EventEmitter } from 'events';
import { CLIType, SubTask } from '../types';
import { RiskPolicy, RiskAssessment, RiskLevel, VerificationLevel } from './risk-policy';
import { ExecutionPlan } from './protocols/types';
import { VerificationConfig } from './verification-runner';

/** CLI 选择策略 */
export interface CLISelectionPolicy {
  /** 推荐的 CLI */
  recommendedCli: CLIType;
  /** 备选 CLI 列表 */
  fallbackClis: CLIType[];
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

/** CLI 健康状态 */
export interface CLIHealthStatus {
  cli: CLIType;
  available: boolean;
  successRate: number;
  avgResponseTime: number;
  lastError?: string;
  lastSuccessAt?: number;
}

/** CLI 任务类型映射 */
const CLI_TASK_MAPPING: Record<string, CLIType[]> = {
  // 架构设计、复杂重构
  architecture: ['claude'],
  refactoring: ['claude', 'codex'],
  // 后端开发、API、数据库
  backend: ['codex', 'claude'],
  api: ['codex', 'claude'],
  database: ['codex'],
  // 前端开发、UI/UX
  frontend: ['gemini', 'claude'],
  ui: ['gemini'],
  styling: ['gemini'],
  // Bug 修复、调试
  bugfix: ['codex', 'claude'],
  debug: ['codex', 'claude'],
  // 测试、文档
  testing: ['codex', 'claude'],
  documentation: ['claude', 'gemini'],
  // 默认
  general: ['claude', 'codex', 'gemini'],
};

/** 文件类型到任务类型的映射 */
const FILE_TYPE_MAPPING: Record<string, string> = {
  '.tsx': 'frontend',
  '.jsx': 'frontend',
  '.vue': 'frontend',
  '.svelte': 'frontend',
  '.css': 'styling',
  '.scss': 'styling',
  '.less': 'styling',
  '.ts': 'backend',
  '.js': 'backend',
  '.py': 'backend',
  '.go': 'backend',
  '.rs': 'backend',
  '.java': 'backend',
  '.sql': 'database',
  '.prisma': 'database',
  '.md': 'documentation',
  '.test.ts': 'testing',
  '.spec.ts': 'testing',
  '.test.js': 'testing',
  '.spec.js': 'testing',
};

/**
 * 统一策略引擎
 */
export class PolicyEngine extends EventEmitter {
  private riskPolicy: RiskPolicy;
  private cliHealthStatus: Map<CLIType, CLIHealthStatus> = new Map();
  private executionHistory: Array<{ cli: CLIType; success: boolean; duration: number }> = [];

  constructor() {
    super();
    this.riskPolicy = new RiskPolicy();
    this.initializeCLIHealth();
  }

  /** 初始化 CLI 健康状态 */
  private initializeCLIHealth(): void {
    const clis: CLIType[] = ['claude', 'codex', 'gemini'];
    for (const cli of clis) {
      this.cliHealthStatus.set(cli, {
        cli,
        available: true,
        successRate: 1.0,
        avgResponseTime: 0,
      });
    }
  }

  // ========== CLI 选择策略 ==========

  /**
   * 根据任务特征选择最佳 CLI
   */
  selectCLI(task: SubTask, availableClis?: CLIType[]): CLISelectionPolicy {
    const available = availableClis || this.getAvailableCLIs();
    const taskType = this.inferTaskType(task);
    const preferredClis = CLI_TASK_MAPPING[taskType] || CLI_TASK_MAPPING.general;

    // 过滤出可用的 CLI
    const candidates = preferredClis.filter(cli => available.includes(cli));
    if (candidates.length === 0) {
      // 回退到任何可用的 CLI
      return {
        recommendedCli: available[0] || 'claude',
        fallbackClis: available.slice(1),
        reason: `无首选 CLI 可用，回退到 ${available[0] || 'claude'}`,
        confidence: 0.5,
      };
    }

    // 根据健康状态和成功率排序
    const sorted = this.sortByHealth(candidates);
    const recommended = sorted[0];
    const fallbacks = sorted.slice(1);

    return {
      recommendedCli: recommended,
      fallbackClis: fallbacks,
      reason: `任务类型 "${taskType}" 推荐使用 ${recommended}`,
      confidence: this.calculateConfidence(recommended, taskType),
    };
  }

  /** 推断任务类型 */
  private inferTaskType(task: SubTask): string {
    const description = (task.description || '').toLowerCase();
    const title = (task.title || '').toLowerCase();
    const files = task.targetFiles || [];

    // 1. 从描述关键词推断
    const keywords: Record<string, string[]> = {
      architecture: ['架构', 'architecture', '重构', 'refactor', '设计'],
      frontend: ['前端', 'frontend', 'ui', '界面', '组件', 'component'],
      backend: ['后端', 'backend', 'api', '服务', 'service'],
      database: ['数据库', 'database', 'sql', 'migration', '迁移'],
      bugfix: ['修复', 'fix', 'bug', '问题', 'issue', '错误'],
      testing: ['测试', 'test', 'spec', '单元测试'],
      documentation: ['文档', 'doc', 'readme', '注释'],
    };

    for (const [type, words] of Object.entries(keywords)) {
      if (words.some(w => description.includes(w) || title.includes(w))) {
        return type;
      }
    }

    // 2. 从目标文件推断
    for (const file of files) {
      const ext = this.getFileExtension(file);
      if (FILE_TYPE_MAPPING[ext]) {
        return FILE_TYPE_MAPPING[ext];
      }
    }

    return 'general';
  }

  /** 获取文件扩展名 */
  private getFileExtension(file: string): string {
    // 处理 .test.ts, .spec.ts 等复合扩展名
    if (file.includes('.test.') || file.includes('.spec.')) {
      return file.includes('.test.') ? '.test.ts' : '.spec.ts';
    }
    const match = file.match(/\.[^.]+$/);
    return match ? match[0] : '';
  }

  /** 根据健康状态排序 CLI */
  private sortByHealth(clis: CLIType[]): CLIType[] {
    return [...clis].sort((a, b) => {
      const healthA = this.cliHealthStatus.get(a);
      const healthB = this.cliHealthStatus.get(b);
      if (!healthA || !healthB) return 0;

      // 优先考虑可用性
      if (healthA.available !== healthB.available) {
        return healthA.available ? -1 : 1;
      }

      // 然后考虑成功率
      return healthB.successRate - healthA.successRate;
    });
  }

  /** 计算置信度 */
  private calculateConfidence(cli: CLIType, taskType: string): number {
    const health = this.cliHealthStatus.get(cli);
    const baseConfidence = CLI_TASK_MAPPING[taskType]?.[0] === cli ? 0.9 : 0.7;
    const healthFactor = health?.successRate || 0.5;
    return Math.min(1, baseConfidence * healthFactor);
  }

  /** 获取可用的 CLI 列表 */
  getAvailableCLIs(): CLIType[] {
    const available: CLIType[] = [];
    for (const [cli, status] of this.cliHealthStatus) {
      if (status.available && status.successRate > 0.3) {
        available.push(cli);
      }
    }
    return available.length > 0 ? available : ['claude'];
  }

  /** 更新 CLI 健康状态 */
  updateCLIHealth(cli: CLIType, success: boolean, responseTime: number, error?: string): void {
    const status = this.cliHealthStatus.get(cli);
    if (!status) return;

    // 记录执行历史
    this.executionHistory.push({ cli, success, duration: responseTime });
    if (this.executionHistory.length > 100) {
      this.executionHistory.shift();
    }

    // 计算最近的成功率
    const recentHistory = this.executionHistory.filter(h => h.cli === cli).slice(-20);
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

    this.emit('cliHealthUpdated', { cli, status });
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
  shouldHardStop(riskAssessment: RiskAssessment, mode: 'ask' | 'agent' | 'auto'): boolean {
    // Auto 模式跳过 Hard Stop
    if (mode === 'auto') return false;

    // Ask 模式不执行任务，无需 Hard Stop
    if (mode === 'ask') return false;

    // Agent 模式根据风险等级决定
    return riskAssessment.hardStop;
  }

  // ========== 降级策略 ==========

  /**
   * 获取 CLI 降级顺序
   */
  getFallbackOrder(primaryCli: CLIType): CLIType[] {
    const allClis: CLIType[] = ['claude', 'codex', 'gemini'];
    const available = this.getAvailableCLIs();

    // 移除主 CLI，按健康状态排序剩余的
    const fallbacks = allClis
      .filter(cli => cli !== primaryCli && available.includes(cli));

    return this.sortByHealth(fallbacks);
  }

  /**
   * 处理 CLI 失败，返回下一个可用的 CLI
   */
  handleCLIFailure(failedCli: CLIType, error: string): CLIType | null {
    // 更新健康状态
    this.updateCLIHealth(failedCli, false, 0, error);

    // 获取降级选项
    const fallbacks = this.getFallbackOrder(failedCli);
    return fallbacks.length > 0 ? fallbacks[0] : null;
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
    cliHealth: CLIHealthStatus[];
    recentExecutions: number;
    overallSuccessRate: number;
  } {
    const cliHealth = Array.from(this.cliHealthStatus.values());
    const recentExecutions = this.executionHistory.length;
    const successCount = this.executionHistory.filter(h => h.success).length;
    const overallSuccessRate = recentExecutions > 0 ? successCount / recentExecutions : 1;

    return {
      cliHealth,
      recentExecutions,
      overallSuccessRate,
    };
  }

  /**
   * 重置策略引擎状态
   */
  reset(): void {
    this.initializeCLIHealth();
    this.executionHistory = [];
  }
}

// 导出单例
export const policyEngine = new PolicyEngine();
