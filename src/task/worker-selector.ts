/**
 * Worker 选择器
 * 根据任务类型、用户配置、Worker 可用性、执行统计和 Worker 画像选择最佳 Worker
 *
 * 集成 Worker Profile System：
 * - 基于 Worker 画像的能力匹配
 * - 基于任务分类的智能选择
 * - 支持成本/速度/质量优化目标
 */

import { WorkerSlot, TaskCategory } from '../types';
import { TaskAnalysis } from './task-analyzer';
import { ExecutionStats } from '../orchestrator/execution-stats';
import {
  ProfileLoader,
  WorkerProfile,
  WorkerSelectionOptions,
  WorkerSelectionResult,
} from '../orchestrator/profile';
import {
  DEFAULT_CLAUDE_PROFILE,
  DEFAULT_CODEX_PROFILE,
  DEFAULT_GEMINI_PROFILE,
  DEFAULT_CATEGORIES_CONFIG,
} from '../orchestrator/profile/defaults';
import {
  ConflictResolver,
  ConflictResolutionConfig,
  ConflictResolutionInput,
  ConflictResolutionResult,
} from './conflict-resolver';

/** Worker 选择结果 */
export interface WorkerSelection {
  /** 选中的 Worker */
  worker: WorkerSlot;
  /** 是否为降级选择 */
  degraded: boolean;
  /** 原始首选 Worker */
  preferred: WorkerSlot;
  /** 选择原因 */
  reason: string;
  /** 基于统计的置信度 (0-1) */
  confidence?: number;
  /** 任务分类 */
  category?: string;
  /** 匹配分数 */
  score?: number;
}


/**
 * Worker 选择器类
 * 支持基于 Worker 画像的智能选择
 * 集成 ConflictResolver 统一冲突解决
 */
export class WorkerSelector {
  private availableWorkers: Set<WorkerSlot> = new Set();
  private executionStats?: ExecutionStats;
  /** 是否启用基于统计的智能选择 */
  private useStatsBasedSelection: boolean = true;
  /** 健康阈值：低于此成功率的 Worker 会被降级 */
  private healthThreshold: number = 0.6;
  /** 画像加载器 */
  private profileLoader?: ProfileLoader;
  /** 是否启用画像选择 */
  private useProfileBasedSelection: boolean = true;
  /** 冲突解决器 */
  private conflictResolver: ConflictResolver;

  constructor() {
    this.conflictResolver = new ConflictResolver();
  }

  /**
   * 设置画像加载器
   */
  setProfileLoader(loader: ProfileLoader): void {
    this.profileLoader = loader;
    this.conflictResolver.setProfileLoader(loader);
  }

  /**
   * 配置画像选择
   */
  configureProfileSelection(enabled: boolean): void {
    this.useProfileBasedSelection = enabled;
  }

  /**
   * 设置执行统计实例
   */
  setExecutionStats(stats: ExecutionStats): void {
    this.executionStats = stats;
    this.conflictResolver.setExecutionStats(stats);
  }

  /**
   * 配置智能选择参数
   */
  configureSmartSelection(options: {
    enabled?: boolean;
    healthThreshold?: number;
  }): void {
    if (options.enabled !== undefined) {
      this.useStatsBasedSelection = options.enabled;
    }
    if (options.healthThreshold !== undefined) {
      this.healthThreshold = options.healthThreshold;
      // 同步更新 ConflictResolver 配置
      this.conflictResolver.updateConfig({ healthThreshold: options.healthThreshold });
    }
  }

  /**
   * 配置冲突解决策略
   */
  configureConflictResolution(config: Partial<ConflictResolutionConfig>): void {
    this.conflictResolver.updateConfig(config);
  }

  /**
   * 更新可用 Worker 列表
   */
  setAvailableWorkers(workers: WorkerSlot[]): void {
    this.availableWorkers = new Set(workers);
  }


  private getCategoryConfig(category: TaskCategory) {
    if (this.useProfileBasedSelection && this.profileLoader) {
      const config = this.profileLoader.getCategory(category);
      if (config) return config;
    }
    return DEFAULT_CATEGORIES_CONFIG.categories[category];
  }

  private getCategoryRules() {
    if (this.useProfileBasedSelection && this.profileLoader) {
      return this.profileLoader.getCategoryRules();
    }
    return DEFAULT_CATEGORIES_CONFIG.rules;
  }

  private getProfile(worker: WorkerSlot): WorkerProfile {
    if (this.useProfileBasedSelection && this.profileLoader) {
      return this.profileLoader.getProfile(worker);
    }
    switch (worker) {
      case 'codex':
        return DEFAULT_CODEX_PROFILE;
      case 'gemini':
        return DEFAULT_GEMINI_PROFILE;
      default:
        return DEFAULT_CLAUDE_PROFILE;
    }
  }

  /**
   * 根据任务分析选择最佳 Worker
   * 使用 ConflictResolver 统一冲突解决
   */
  select(analysis: TaskAnalysis, userPreference?: WorkerSlot): WorkerSelection {
    const category = analysis.category;

    // 获取画像推荐
    const categoryConfig = this.getCategoryConfig(category);
    const defaultGeneral = DEFAULT_CATEGORIES_CONFIG.categories.general?.defaultWorker || 'claude';
    const profileRecommendation = (categoryConfig?.defaultWorker || defaultGeneral) as WorkerSlot;

    const availableWorkers = this.availableWorkers.size > 0
      ? Array.from(this.availableWorkers)
      : (['claude', 'codex', 'gemini'] as WorkerSlot[]);

    // 获取执行统计推荐
    let statsRecommendation: WorkerSlot | undefined;
    if (this.useStatsBasedSelection && this.executionStats) {
      statsRecommendation = this.executionStats.recommendWorker(category, availableWorkers);
    }

    // 使用 ConflictResolver 解决冲突
    const resolution = this.conflictResolver.resolve({
      userPreference,
      profileRecommendation,
      statsRecommendation,
      category,
      availableWorkers,
    });

    return {
      worker: resolution.worker,
      degraded: resolution.degraded,
      preferred: profileRecommendation || resolution.worker,
      reason: resolution.reason,
      confidence: resolution.confidence,
      category,
    };
  }

  /**
   * 基于统计数据的智能选择
   */
  private selectWithStats(preferred: WorkerSlot, category: TaskCategory): WorkerSelection | null {
    if (!this.executionStats) return null;

    const preferredStats = this.executionStats.getStats(preferred);

    // 如果首选 Worker 健康且可用，直接使用
    if (preferredStats.isHealthy && this.availableWorkers.has(preferred)) {
      return {
        worker: preferred,
        degraded: false,
        preferred,
        reason: `任务类型 "${category}" 的首选 Worker (健康度: ${(preferredStats.healthScore * 100).toFixed(0)}%)`,
        confidence: preferredStats.healthScore,
      };
    }

    // 如果首选 Worker 不健康，寻找更好的替代
    if (!preferredStats.isHealthy || preferredStats.healthScore < this.healthThreshold) {
      const availableList = Array.from(this.availableWorkers);
      const betterWorker = this.executionStats.recommendWorker(category, availableList);

      if (betterWorker !== preferred && this.availableWorkers.has(betterWorker)) {
        const betterStats = this.executionStats.getStats(betterWorker);
        return {
          worker: betterWorker,
          degraded: true,
          preferred,
          reason: `${preferred} 近期表现不佳 (${(preferredStats.healthScore * 100).toFixed(0)}%)，` +
                  `智能选择 ${betterWorker} (${(betterStats.healthScore * 100).toFixed(0)}%)`,
          confidence: betterStats.healthScore,
        };
      }
    }

    return null; // 使用默认逻辑
  }

  /**
   * 根据任务类型直接选择 Worker
   * 集成画像系统和基于统计的智能选择
   */
  selectByCategory(category: TaskCategory): WorkerSelection {
    // 如果有画像系统，使用画像配置的默认 Worker
    const categoryConfig = this.getCategoryConfig(category);
    const defaultGeneral = DEFAULT_CATEGORIES_CONFIG.categories.general?.defaultWorker || 'claude';
    let preferred = (categoryConfig?.defaultWorker || defaultGeneral) as WorkerSlot;

    // 基于统计的智能选择
    if (this.useStatsBasedSelection && this.executionStats) {
      const smartSelection = this.selectWithStats(preferred, category);
      if (smartSelection) {
        smartSelection.category = category;
        return smartSelection;
      }
    }

    const availableList = this.availableWorkers.size > 0
      ? Array.from(this.availableWorkers)
      : (['claude', 'codex', 'gemini'] as WorkerSlot[]);

    if (availableList.includes(preferred)) {
      return {
        worker: preferred,
        degraded: false,
        preferred,
        reason: `任务类型 "${category}" 的首选 Worker`,
        category,
      };
    }

    const description = [
      categoryConfig?.displayName,
      categoryConfig?.description,
      category,
    ].filter(Boolean).join(' ');
    const scores = this.calculateProfileScores(description, category, {
      preferredWorker: preferred,
    });
    if (this.executionStats) {
      this.adjustScoresWithStats(scores, category);
    }
    if (availableList.length === 0) {
      return {
        worker: preferred,
        degraded: false,
        preferred,
        reason: '没有可用的 Worker，保持首选',
        category,
      };
    }

    let bestWorker = availableList[0] as WorkerSlot;
    let bestScore = scores.get(bestWorker) || 0;
    for (const worker of availableList) {
      const score = scores.get(worker as WorkerSlot);
      if (score !== undefined && score > bestScore) {
        bestScore = score;
        bestWorker = worker as WorkerSlot;
      }
    }

    const degraded = bestWorker !== preferred;
    return {
      worker: bestWorker,
      degraded,
      preferred,
      reason: degraded
        ? `首选 ${preferred} 不可用，基于画像选择 ${bestWorker}`
        : '没有可用的 Worker，保持首选',
      category,
    };
  }

  /**
   * 获取可用 Worker 列表
   */
  getAvailableWorkers(): WorkerSlot[] {
    return Array.from(this.availableWorkers);
  }

  // ============================================================================
  // 基于 Worker 画像的选择方法
  // ============================================================================

  /**
   * 基于任务描述智能选择 Worker
   * 综合考虑：画像匹配 + 执行统计 + 成本/速度/质量因子
   *
   * 注意：分类逻辑已统一到 TaskAnalyzer，此方法直接使用分类结果
   */
  selectByDescription(
    taskDescription: string,
    options: WorkerSelectionOptions = {}
  ): WorkerSelectionResult {
    // 1. 使用画像配置识别任务分类
    const { category, defaultWorker } = this.classifyWithProfile(taskDescription);

    // 2. 计算各 Worker 的匹配分数
    const scores = this.calculateProfileScores(taskDescription, category, options);

    // 3. 结合执行统计调整分数
    if (this.executionStats) {
      this.adjustScoresWithStats(scores, category);
    }

    // 4. 选择最高分的 Worker
    const availableList = this.availableWorkers.size > 0
      ? Array.from(this.availableWorkers)
      : (['claude', 'codex', 'gemini'] as WorkerSlot[]);
    let bestWorker = defaultWorker;
    let bestScore = scores.get(defaultWorker) || 0;

    if (availableList.length > 0 && !this.availableWorkers.has(defaultWorker)) {
      bestWorker = availableList[0] as WorkerSlot;
      bestScore = scores.get(bestWorker) || 0;
    }

    for (const [workerType, score] of scores) {
      if (score > bestScore && (availableList.length === 0 || this.availableWorkers.has(workerType))) {
        bestScore = score;
        bestWorker = workerType;
      }
    }

    // 构建选择原因
    const profile = this.getProfile(bestWorker);
    const reason = this.buildSelectionReasonSimple(bestWorker, category, profile);

    return {
      worker: bestWorker,
      category,
      score: bestScore,
      reason,
    };
  }

  /**
   * 使用画像配置分类任务
   */
  private classifyWithProfile(taskDescription: string): { category: string; defaultWorker: WorkerSlot } {
    const rules = this.getCategoryRules();
    const categories = this.profileLoader?.getAllCategories();
    const lowerDesc = taskDescription.toLowerCase();

    let bestMatch: { category: string; score: number; defaultWorker: WorkerSlot } | null = null;

    for (const categoryName of rules.categoryPriority) {
      const config = categories?.get(categoryName) || DEFAULT_CATEGORIES_CONFIG.categories[categoryName];
      if (!config) continue;

      let score = 0;
      for (const pattern of config.keywords) {
        try {
          const regex = new RegExp(pattern, 'i');
          if (regex.test(lowerDesc)) {
            score += 10;
          }
        } catch {
          if (lowerDesc.includes(pattern.toLowerCase())) {
            score += 5;
          }
        }
      }

      if (score > 0 && (!bestMatch || score > bestMatch.score)) {
        bestMatch = {
          category: categoryName,
          score,
          defaultWorker: config.defaultWorker as WorkerSlot,
        };
      }
    }

    if (bestMatch) {
      return { category: bestMatch.category, defaultWorker: bestMatch.defaultWorker };
    }

    // 回退到默认分类
    const defaultCategory = rules.defaultCategory;
    const defaultConfig = categories?.get(defaultCategory) || DEFAULT_CATEGORIES_CONFIG.categories[defaultCategory];
    return {
      category: defaultCategory,
      defaultWorker: (defaultConfig?.defaultWorker || 'claude') as WorkerSlot,
    };
  }

  /**
   * 构建简化的选择原因
   */
  private buildSelectionReasonSimple(
    worker: WorkerSlot,
    category: string,
    profile: WorkerProfile
  ): string {
    const parts: string[] = [];
    parts.push(`任务分类: ${category}`);
    parts.push(`选择 ${profile.name}`);

    if (profile.preferences.preferredCategories.includes(category)) {
      parts.push('(分类匹配)');
    }

    return parts.join(' - ');
  }

  /**
   * 计算基于画像的 Worker 匹配分数
   */
  private calculateProfileScores(
    taskDescription: string,
    category: string,
    options: WorkerSelectionOptions
  ): Map<WorkerSlot, number> {
    const scores = new Map<WorkerSlot, number>();

    for (const workerType of ['claude', 'codex', 'gemini'] as WorkerSlot[]) {
      // 排除指定的 Worker
      if (options.excludeWorkers?.includes(workerType)) {
        scores.set(workerType, -Infinity);
        continue;
      }

      const profile = this.getProfile(workerType);
      let score = 50; // 基础分

      // 1. 分类匹配 (+30)
      if (profile.preferences.preferredCategories.includes(category)) {
        score += 30;
      }

      // 2. 关键词匹配 (+5 each, max +20)
      let keywordScore = 0;
      for (const pattern of profile.preferences.preferredKeywords) {
        try {
          const regex = new RegExp(pattern, 'i');
          if (regex.test(taskDescription)) {
            keywordScore += 5;
          }
        } catch {
          // 忽略无效正则
        }
      }
      score += Math.min(keywordScore, 20);

      // 3. 偏好 Worker 加分
      if (options.preferredWorker === workerType) {
        score += 10;
      }

      scores.set(workerType, score);
    }

    return scores;
  }

  /**
   * 基于执行统计调整分数
   */
  private adjustScoresWithStats(
    scores: Map<WorkerSlot, number>,
    category: string
  ): void {
    if (!this.executionStats) return;

    for (const [workerType, score] of scores) {
      if (score === -Infinity) continue;

      const stats = this.executionStats.getStats(workerType);
      if (!stats || stats.totalExecutions < 3) continue; // 样本不足

      // 成功率调整 (+/- 10)
      const successRateBonus = (stats.successRate - 0.8) * 50;

      // 健康度调整
      const healthBonus = stats.isHealthy ? 5 : -5;

      scores.set(workerType, score + successRateBonus + healthBonus);
    }
  }

  /**
   * 回退选择（无画像时使用）
   */
  private fallbackSelection(
    taskDescription: string,
    options: WorkerSelectionOptions
  ): WorkerSelectionResult {
    // 简单关键词匹配
    const desc = taskDescription.toLowerCase();
    let worker: WorkerSlot = 'claude';
    let category = 'general';

    if (desc.includes('bug') || desc.includes('fix') || desc.includes('修复')) {
      worker = 'codex';
      category = 'bugfix';
    } else if (desc.includes('前端') || desc.includes('ui') || desc.includes('页面')) {
      worker = 'gemini';
      category = 'frontend';
    } else if (desc.includes('架构') || desc.includes('设计') || desc.includes('重构')) {
      worker = 'claude';
      category = 'architecture';
    }

    // 应用偏好
    if (options.preferredWorker && !options.excludeWorkers?.includes(options.preferredWorker)) {
      worker = options.preferredWorker;
    }

    return {
      worker,
      category,
      score: 50,
      reason: `基于关键词匹配选择 ${worker}`,
    };
  }

  /**
   * 获取 Worker 画像
   */
  getWorkerProfile(workerType: WorkerSlot): WorkerProfile | undefined {
    return this.getProfile(workerType);
  }
}
