/**
 * ConflictResolver - CLI 选择冲突解决器
 *
 * 职责：
 * - 解决用户偏好 vs 画像推荐 vs 执行统计之间的冲突
 * - 提供明确的优先级策略
 * - 支持配置化的冲突解决规则
 *
 * 优先级层级（从高到低）：
 * Level 1: 用户手动选择（explicit user preference）
 * Level 2: 执行统计推荐（健康度降级）
 * Level 3: 画像配置推荐
 * Level 4: 硬编码默认值
 */

import { logger, LogCategory } from '../logging';
import { CLIType } from '../types';
import { ExecutionStats } from '../orchestrator/execution-stats';
import { ProfileLoader } from '../orchestrator/profile/profile-loader';

/** 冲突解决配置 */
export interface ConflictResolutionConfig {
  /** 用户偏好策略: 'always-respect' | 'health-aware' */
  userPreference: 'always-respect' | 'health-aware';
  /** 统计 vs 画像策略: 'prefer-stats-if-healthy' | 'prefer-profile' | 'balanced' */
  statsVsProfile: 'prefer-stats-if-healthy' | 'prefer-profile' | 'balanced';
  /** 健康度阈值（低于此值时降级） */
  healthThreshold: number;
}

/** 冲突解决输入 */
export interface ConflictResolutionInput {
  /** 用户手动选择的 CLI */
  userPreference?: CLIType;
  /** 画像推荐的 CLI */
  profileRecommendation?: CLIType;
  /** 执行统计推荐的 CLI */
  statsRecommendation?: CLIType;
  /** 任务分类 */
  category?: string;
  /** 可用的 CLI 列表 */
  availableClis: CLIType[];
}

/** 冲突解决结果 */
export interface ConflictResolutionResult {
  /** 最终选择的 CLI */
  cli: CLIType;
  /** 选择原因 */
  reason: string;
  /** 使用的决策层级 */
  level: 'user' | 'stats' | 'profile' | 'default';
  /** 是否发生了降级 */
  degraded: boolean;
  /** 置信度 (0-1) */
  confidence: number;
}

/** 默认配置 */
const DEFAULT_CONFIG: ConflictResolutionConfig = {
  userPreference: 'health-aware',
  statsVsProfile: 'prefer-stats-if-healthy',
  healthThreshold: 0.6,
};

/**
 * CLI 选择冲突解决器
 */
export class ConflictResolver {
  private config: ConflictResolutionConfig;
  private executionStats?: ExecutionStats;
  private profileLoader?: ProfileLoader;

  constructor(
    config?: Partial<ConflictResolutionConfig>,
    executionStats?: ExecutionStats,
    profileLoader?: ProfileLoader
  ) {
    this.config = { ...DEFAULT_CONFIG, ...config };
    this.executionStats = executionStats;
    this.profileLoader = profileLoader;
  }

  /**
   * 解决 CLI 选择冲突
   */
  resolve(input: ConflictResolutionInput): ConflictResolutionResult {
    const { userPreference, profileRecommendation, statsRecommendation, availableClis } = input;

    // Level 1: 用户手动选择
    if (userPreference && availableClis.includes(userPreference)) {
      const userResult = this.handleUserPreference(userPreference, input);
      if (userResult) {
        return userResult;
      }
    }

    // Level 2: 执行统计推荐（健康度降级）
    if (statsRecommendation && availableClis.includes(statsRecommendation)) {
      const statsResult = this.handleStatsRecommendation(statsRecommendation, profileRecommendation, input);
      if (statsResult) {
        return statsResult;
      }
    }

    // Level 3: 画像配置推荐
    if (profileRecommendation && availableClis.includes(profileRecommendation)) {
      return {
        cli: profileRecommendation,
        reason: `画像推荐: ${profileRecommendation}`,
        level: 'profile',
        degraded: false,
        confidence: 0.8,
      };
    }

    // Level 4: 硬编码默认值
    const defaultCli = availableClis[0] || 'claude';
    return {
      cli: defaultCli,
      reason: `回退到默认值: ${defaultCli}`,
      level: 'default',
      degraded: true,
      confidence: 0.5,
    };
  }

  /**
   * 处理用户偏好
   */
  private handleUserPreference(
    userPreference: CLIType,
    input: ConflictResolutionInput
  ): ConflictResolutionResult | null {
    // always-respect: 直接使用用户选择
    if (this.config.userPreference === 'always-respect') {
      return {
        cli: userPreference,
        reason: `用户指定: ${userPreference}`,
        level: 'user',
        degraded: false,
        confidence: 1.0,
      };
    }

    // health-aware: 检查健康度
    if (this.config.userPreference === 'health-aware' && this.executionStats) {
      const stats = this.executionStats.getStats(userPreference);
      if (stats.isHealthy && stats.successRate >= this.config.healthThreshold) {
        return {
          cli: userPreference,
          reason: `用户指定: ${userPreference} (健康度: ${(stats.successRate * 100).toFixed(0)}%)`,
          level: 'user',
          degraded: false,
          confidence: stats.successRate,
        };
      }

      // 用户选择的 CLI 不健康，提示降级
      logger.warn(
        '任务.冲突.用户_偏好.不健康',
        {
          cli: userPreference,
          successRate: stats.successRate,
          threshold: this.config.healthThreshold,
        },
        LogCategory.TASK
      );

      // 如果有更健康的备选，建议降级
      if (input.statsRecommendation && input.statsRecommendation !== userPreference) {
        const altStats = this.executionStats.getStats(input.statsRecommendation);
        if (altStats.successRate > stats.successRate) {
          return {
            cli: input.statsRecommendation,
            reason: `用户指定的 ${userPreference} 不健康，降级到 ${input.statsRecommendation}`,
            level: 'stats',
            degraded: true,
            confidence: altStats.successRate,
          };
        }
      }

      // 没有更好的备选，仍使用用户选择（带警告）
      return {
        cli: userPreference,
        reason: `用户指定: ${userPreference} (健康度低，但无更好备选)`,
        level: 'user',
        degraded: false,
        confidence: stats.successRate,
      };
    }

    // 默认使用用户偏好
    return {
      cli: userPreference,
      reason: `用户指定: ${userPreference}`,
      level: 'user',
      degraded: false,
      confidence: 0.9,
    };
  }

  /**
   * 处理执行统计推荐
   */
  private handleStatsRecommendation(
    statsRecommendation: CLIType,
    profileRecommendation: CLIType | undefined,
    input: ConflictResolutionInput
  ): ConflictResolutionResult | null {
    if (!this.executionStats) {
      return null;
    }

    const statsData = this.executionStats.getStats(statsRecommendation);

    // prefer-stats-if-healthy: 统计数据健康时优先
    if (this.config.statsVsProfile === 'prefer-stats-if-healthy') {
      if (statsData.isHealthy && statsData.successRate >= this.config.healthThreshold) {
        return {
          cli: statsRecommendation,
          reason: `执行统计推荐: ${statsRecommendation} (成功率: ${(statsData.successRate * 100).toFixed(0)}%)`,
          level: 'stats',
          degraded: false,
          confidence: statsData.successRate,
        };
      }
    }

    // prefer-profile: 画像优先，统计作为参考
    if (this.config.statsVsProfile === 'prefer-profile') {
      if (profileRecommendation) {
        return null; // 继续到 Level 3
      }
      // 没有画像推荐时才使用统计
      return {
        cli: statsRecommendation,
        reason: `执行统计推荐: ${statsRecommendation} (无画像推荐)`,
        level: 'stats',
        degraded: false,
        confidence: statsData.successRate,
      };
    }

    // balanced: 综合考虑
    if (this.config.statsVsProfile === 'balanced' && profileRecommendation) {
      const profileData = this.executionStats.getStats(profileRecommendation);

      // 统计数据明显更好时才覆盖画像
      if (statsData.successRate > profileData.successRate + 0.2) {
        return {
          cli: statsRecommendation,
          reason: `执行统计显著优于画像推荐 (${(statsData.successRate * 100).toFixed(0)}% vs ${(profileData.successRate * 100).toFixed(0)}%)`,
          level: 'stats',
          degraded: false,
          confidence: statsData.successRate,
        };
      }

      // 否则继续使用画像推荐
      return null;
    }

    // 默认使用统计推荐
    return {
      cli: statsRecommendation,
      reason: `执行统计推荐: ${statsRecommendation}`,
      level: 'stats',
      degraded: false,
      confidence: statsData.successRate,
    };
  }

  /**
   * 更新配置
   */
  updateConfig(config: Partial<ConflictResolutionConfig>): void {
    this.config = { ...this.config, ...config };
  }

  /**
   * 设置执行统计
   */
  setExecutionStats(stats: ExecutionStats): void {
    this.executionStats = stats;
  }

  /**
   * 设置画像加载器
   */
  setProfileLoader(loader: ProfileLoader): void {
    this.profileLoader = loader;
  }
}
