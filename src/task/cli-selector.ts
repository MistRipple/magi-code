/**
 * CLI 选择器
 * 根据任务类型、用户配置、CLI 可用性、执行统计和 Worker 画像选择最佳 CLI
 *
 * 🆕 集成 Worker Profile System：
 * - 基于 Worker 画像的能力匹配
 * - 基于任务分类的智能选择
 * - 支持成本/速度/质量优化目标
 */

import { CLIType, TaskCategory } from '../types';
import { TaskAnalysis } from './task-analyzer';
import { ExecutionStats } from '../orchestrator/execution-stats';
import {
  ProfileLoader,
  WorkerProfile,
  WorkerSelectionOptions,
  WorkerSelectionResult,
} from '../orchestrator/profile';
import {
  ConflictResolver,
  ConflictResolutionConfig,
  ConflictResolutionInput,
  ConflictResolutionResult,
} from './conflict-resolver';

/** CLI 能力配置 */
export interface CLISkillsConfig {
  architecture: CLIType;
  implement: CLIType;
  refactor: CLIType;
  bugfix: CLIType;
  debug: CLIType;
  frontend: CLIType;
  backend: CLIType;
  test: CLIType;
  document: CLIType;
  review: CLIType;
  general: CLIType;
}

/** 默认 CLI 技能配置 */
const DEFAULT_SKILLS: CLISkillsConfig = {
  architecture: 'claude',
  implement: 'claude',
  refactor: 'claude',
  bugfix: 'codex',
  debug: 'claude',
  frontend: 'gemini',
  backend: 'codex',
  test: 'codex',
  document: 'claude',
  review: 'claude',
  general: 'claude',
};

/** CLI 优先级降级顺序 */
const CLI_FALLBACK_ORDER: Record<CLIType, CLIType[]> = {
  claude: ['codex', 'gemini'],
  codex: ['claude', 'gemini'],
  gemini: ['claude', 'codex'],
};

/** CLI 选择结果 */
export interface CLISelection {
  /** 选中的 CLI */
  cli: CLIType;
  /** 是否为降级选择 */
  degraded: boolean;
  /** 原始首选 CLI */
  preferred: CLIType;
  /** 选择原因 */
  reason: string;
  /** 基于统计的置信度 (0-1) */
  confidence?: number;
  /** 🆕 任务分类 */
  category?: string;
  /** 🆕 匹配分数 */
  score?: number;
}

/**
 * CLI 选择器类
 * 🆕 支持基于 Worker 画像的智能选择
 * 🆕 集成 ConflictResolver 统一冲突解决
 */
export class CLISelector {
  private skills: CLISkillsConfig;
  private availableCLIs: Set<CLIType> = new Set();
  private executionStats?: ExecutionStats;
  /** 是否启用基于统计的智能选择 */
  private useStatsBasedSelection: boolean = true;
  /** 健康阈值：低于此成功率的 CLI 会被降级 */
  private healthThreshold: number = 0.6;
  /** 🆕 画像加载器 */
  private profileLoader?: ProfileLoader;
  /** 🆕 是否启用画像选择 */
  private useProfileBasedSelection: boolean = true;
  /** 🆕 冲突解决器 */
  private conflictResolver: ConflictResolver;

  constructor(skills?: Partial<CLISkillsConfig>) {
    this.skills = { ...DEFAULT_SKILLS, ...skills };
    this.conflictResolver = new ConflictResolver();
  }

  /**
   * 🆕 设置画像加载器
   */
  setProfileLoader(loader: ProfileLoader): void {
    this.profileLoader = loader;
    this.conflictResolver.setProfileLoader(loader);
  }

  /**
   * 🆕 配置画像选择
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
   * 🆕 配置冲突解决策略
   */
  configureConflictResolution(config: Partial<ConflictResolutionConfig>): void {
    this.conflictResolver.updateConfig(config);
  }

  /**
   * 更新可用 CLI 列表
   */
  setAvailableCLIs(clis: CLIType[]): void {
    this.availableCLIs = new Set(clis);
  }

  /**
   * 更新技能配置
   */
  updateSkills(skills: Partial<CLISkillsConfig>): void {
    this.skills = { ...this.skills, ...skills };
  }

  /**
   * 根据任务分析选择最佳 CLI
   * 🆕 使用 ConflictResolver 统一冲突解决
   */
  select(analysis: TaskAnalysis, userPreference?: CLIType): CLISelection {
    const category = analysis.category;

    // 获取画像推荐
    let profileRecommendation: CLIType | undefined;
    if (this.useProfileBasedSelection && this.profileLoader) {
      const categoryConfig = this.profileLoader.getCategory(category);
      profileRecommendation = categoryConfig?.defaultWorker as CLIType;
    }
    if (!profileRecommendation) {
      profileRecommendation = this.skills[category] || this.skills.general;
    }

    // 获取执行统计推荐
    let statsRecommendation: CLIType | undefined;
    if (this.useStatsBasedSelection && this.executionStats) {
      const availableList = Array.from(this.availableCLIs);
      statsRecommendation = this.executionStats.recommendCLI(category, availableList);
    }

    // 使用 ConflictResolver 解决冲突
    const resolution = this.conflictResolver.resolve({
      userPreference,
      profileRecommendation,
      statsRecommendation,
      category,
      availableClis: Array.from(this.availableCLIs),
    });

    return {
      cli: resolution.cli,
      degraded: resolution.degraded,
      preferred: profileRecommendation || resolution.cli,
      reason: resolution.reason,
      confidence: resolution.confidence,
      category,
    };
  }

  /**
   * 基于统计数据的智能选择
   */
  private selectWithStats(preferred: CLIType, category: TaskCategory): CLISelection | null {
    if (!this.executionStats) return null;

    const preferredStats = this.executionStats.getStats(preferred);

    // 如果首选 CLI 健康且可用，直接使用
    if (preferredStats.isHealthy && this.availableCLIs.has(preferred)) {
      return {
        cli: preferred,
        degraded: false,
        preferred,
        reason: `任务类型 "${category}" 的首选 CLI (健康度: ${(preferredStats.healthScore * 100).toFixed(0)}%)`,
        confidence: preferredStats.healthScore,
      };
    }

    // 如果首选 CLI 不健康，寻找更好的替代
    if (!preferredStats.isHealthy || preferredStats.healthScore < this.healthThreshold) {
      const availableList = Array.from(this.availableCLIs);
      const betterCli = this.executionStats.recommendCLI(category, availableList);

      if (betterCli !== preferred && this.availableCLIs.has(betterCli)) {
        const betterStats = this.executionStats.getStats(betterCli);
        return {
          cli: betterCli,
          degraded: true,
          preferred,
          reason: `${preferred} 近期表现不佳 (${(preferredStats.healthScore * 100).toFixed(0)}%)，` +
                  `智能选择 ${betterCli} (${(betterStats.healthScore * 100).toFixed(0)}%)`,
          confidence: betterStats.healthScore,
        };
      }
    }

    return null; // 使用默认逻辑
  }

  /**
   * 根据任务类型直接选择 CLI
   * 🆕 集成画像系统和基于统计的智能选择
   */
  selectByCategory(category: TaskCategory): CLISelection {
    // 🆕 如果有画像系统，使用画像配置的默认 Worker
    let preferred = this.skills[category] || this.skills.general;

    if (this.useProfileBasedSelection && this.profileLoader) {
      const categoryConfig = this.profileLoader.getCategory(category);
      if (categoryConfig?.defaultWorker) {
        preferred = categoryConfig.defaultWorker as CLIType;
      }
    }

    // 基于统计的智能选择
    if (this.useStatsBasedSelection && this.executionStats) {
      const smartSelection = this.selectWithStats(preferred, category);
      if (smartSelection) {
        smartSelection.category = category;
        return smartSelection;
      }
    }

    if (this.availableCLIs.has(preferred)) {
      return {
        cli: preferred,
        degraded: false,
        preferred,
        reason: `任务类型 "${category}" 的首选 CLI`,
        category,
      };
    }

    const fallbacks = CLI_FALLBACK_ORDER[preferred] || [];
    for (const fallback of fallbacks) {
      if (this.availableCLIs.has(fallback)) {
        return {
          cli: fallback,
          degraded: true,
          preferred,
          reason: `首选 ${preferred} 不可用，降级到 ${fallback}`,
          category,
        };
      }
    }

    return {
      cli: preferred,
      degraded: false,
      preferred,
      reason: '没有可用的 CLI，使用默认首选',
      category,
    };
  }

  /**
   * 获取当前技能配置
   */
  getSkills(): CLISkillsConfig {
    return { ...this.skills };
  }

  /**
   * 获取可用 CLI 列表
   */
  getAvailableCLIs(): CLIType[] {
    return Array.from(this.availableCLIs);
  }

  // ============================================================================
  // 🆕 基于 Worker 画像的选择方法
  // ============================================================================

  /**
   * 🆕 基于任务描述智能选择 Worker
   * 综合考虑：画像匹配 + 执行统计 + 成本/速度/质量因子
   *
   * 注意：分类逻辑已统一到 TaskAnalyzer，此方法直接使用分类结果
   */
  selectByDescription(
    taskDescription: string,
    options: WorkerSelectionOptions = {}
  ): WorkerSelectionResult {
    // 如果没有画像加载器，回退到传统选择
    if (!this.profileLoader || !this.useProfileBasedSelection) {
      return this.fallbackSelection(taskDescription, options);
    }

    // 1. 使用画像配置识别任务分类
    const { category, defaultWorker } = this.classifyWithProfile(taskDescription);

    // 2. 计算各 Worker 的匹配分数
    const scores = this.calculateProfileScores(taskDescription, category, options);

    // 3. 结合执行统计调整分数
    if (this.executionStats) {
      this.adjustScoresWithStats(scores, category);
    }

    // 4. 选择最高分的 Worker
    let bestWorker = defaultWorker;
    let bestScore = scores.get(defaultWorker) || 0;

    for (const [workerType, score] of scores) {
      if (score > bestScore && this.availableCLIs.has(workerType)) {
        bestScore = score;
        bestWorker = workerType;
      }
    }

    // 构建选择原因
    const profile = this.profileLoader.getProfile(bestWorker);
    const reason = this.buildSelectionReasonSimple(bestWorker, category, profile);

    return {
      worker: bestWorker,
      category,
      score: bestScore,
      reason,
    };
  }

  /**
   * 🆕 使用画像配置分类任务
   */
  private classifyWithProfile(taskDescription: string): { category: string; defaultWorker: CLIType } {
    const categories = this.profileLoader!.getAllCategories();
    const rules = this.profileLoader!.getCategoryRules();
    const lowerDesc = taskDescription.toLowerCase();

    let bestMatch: { category: string; score: number; defaultWorker: CLIType } | null = null;

    for (const categoryName of rules.categoryPriority) {
      const config = categories.get(categoryName);
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
          defaultWorker: config.defaultWorker as CLIType,
        };
      }
    }

    if (bestMatch) {
      return { category: bestMatch.category, defaultWorker: bestMatch.defaultWorker };
    }

    // 回退到默认分类
    const defaultCategory = rules.defaultCategory;
    const defaultConfig = categories.get(defaultCategory);
    return {
      category: defaultCategory,
      defaultWorker: (defaultConfig?.defaultWorker || 'claude') as CLIType,
    };
  }

  /**
   * 🆕 构建简化的选择原因
   */
  private buildSelectionReasonSimple(
    worker: CLIType,
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
   * 🆕 计算基于画像的 Worker 匹配分数
   */
  private calculateProfileScores(
    taskDescription: string,
    category: string,
    options: WorkerSelectionOptions
  ): Map<CLIType, number> {
    const scores = new Map<CLIType, number>();

    for (const workerType of ['claude', 'codex', 'gemini'] as CLIType[]) {
      // 排除指定的 Worker
      if (options.excludeWorkers?.includes(workerType)) {
        scores.set(workerType, -Infinity);
        continue;
      }

      const profile = this.profileLoader!.getProfile(workerType);
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
   * 🆕 基于执行统计调整分数
   */
  private adjustScoresWithStats(
    scores: Map<CLIType, number>,
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
   * 🆕 回退选择（无画像时使用）
   */
  private fallbackSelection(
    taskDescription: string,
    options: WorkerSelectionOptions
  ): WorkerSelectionResult {
    // 简单关键词匹配
    const desc = taskDescription.toLowerCase();
    let worker: CLIType = 'claude';
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
   * 🆕 获取 Worker 画像
   */
  getWorkerProfile(workerType: CLIType): WorkerProfile | undefined {
    return this.profileLoader?.getProfile(workerType);
  }
}
