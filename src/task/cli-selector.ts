/**
 * CLI 选择器
 * 根据任务类型、用户配置、CLI 可用性和执行统计选择最佳 CLI
 */

import { CLIType, TaskCategory } from '../types';
import { TaskAnalysis } from './task-analyzer';
import { ExecutionStats, CLIStats } from '../orchestrator/execution-stats';

/** CLI 能力配置 */
export interface CLISkillsConfig {
  architecture: CLIType;
  implement: CLIType;
  refactor: CLIType;
  bugfix: CLIType;
  debug: CLIType;
  frontend: CLIType;
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
  /** 🆕 基于统计的置信度 (0-1) */
  confidence?: number;
}

/**
 * CLI 选择器类
 * 🆕 支持基于执行统计的智能选择
 */
export class CLISelector {
  private skills: CLISkillsConfig;
  private availableCLIs: Set<CLIType> = new Set();
  private executionStats?: ExecutionStats;
  /** 🆕 是否启用基于统计的智能选择 */
  private useStatsBasedSelection: boolean = true;
  /** 🆕 健康阈值：低于此成功率的 CLI 会被降级 */
  private healthThreshold: number = 0.6;

  constructor(skills?: Partial<CLISkillsConfig>) {
    this.skills = { ...DEFAULT_SKILLS, ...skills };
  }

  /**
   * 🆕 设置执行统计实例
   */
  setExecutionStats(stats: ExecutionStats): void {
    this.executionStats = stats;
  }

  /**
   * 🆕 配置智能选择参数
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
    }
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
   * 🆕 集成基于统计的智能选择
   */
  select(analysis: TaskAnalysis): CLISelection {
    const preferred = this.skills[analysis.category] || this.skills.general;

    // 🆕 如果启用统计选择，检查首选 CLI 的健康状态
    if (this.useStatsBasedSelection && this.executionStats) {
      const smartSelection = this.selectWithStats(preferred, analysis.category);
      if (smartSelection) {
        return smartSelection;
      }
    }

    // 检查首选 CLI 是否可用
    if (this.availableCLIs.has(preferred)) {
      return {
        cli: preferred,
        degraded: false,
        preferred,
        reason: `任务类型 "${analysis.category}" 的首选 CLI`,
      };
    }

    // 降级到备选 CLI
    const fallbacks = CLI_FALLBACK_ORDER[preferred] || [];
    for (const fallback of fallbacks) {
      if (this.availableCLIs.has(fallback)) {
        return {
          cli: fallback,
          degraded: true,
          preferred,
          reason: `首选 ${preferred} 不可用，降级到 ${fallback}`,
        };
      }
    }

    // 如果没有可用的 CLI，返回首选（让调用者处理错误）
    return {
      cli: preferred,
      degraded: false,
      preferred,
      reason: '没有可用的 CLI，使用默认首选',
    };
  }

  /**
   * 🆕 基于统计数据的智能选择
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
        reason: `任务类型 "${category}" 的首选 CLI (成功率: ${(preferredStats.successRate * 100).toFixed(0)}%)`,
        confidence: preferredStats.successRate,
      };
    }

    // 如果首选 CLI 不健康，寻找更好的替代
    if (!preferredStats.isHealthy || preferredStats.successRate < this.healthThreshold) {
      const availableList = Array.from(this.availableCLIs);
      const betterCli = this.executionStats.recommendCLI(category, availableList);

      if (betterCli !== preferred && this.availableCLIs.has(betterCli)) {
        const betterStats = this.executionStats.getStats(betterCli);
        return {
          cli: betterCli,
          degraded: true,
          preferred,
          reason: `${preferred} 近期表现不佳 (${(preferredStats.successRate * 100).toFixed(0)}%)，` +
                  `智能选择 ${betterCli} (${(betterStats.successRate * 100).toFixed(0)}%)`,
          confidence: betterStats.successRate,
        };
      }
    }

    return null; // 使用默认逻辑
  }

  /**
   * 根据任务类型直接选择 CLI
   * 🆕 集成基于统计的智能选择
   */
  selectByCategory(category: TaskCategory): CLISelection {
    const preferred = this.skills[category] || this.skills.general;

    // 🆕 如果启用统计选择，检查首选 CLI 的健康状态
    if (this.useStatsBasedSelection && this.executionStats) {
      const smartSelection = this.selectWithStats(preferred, category);
      if (smartSelection) {
        return smartSelection;
      }
    }

    if (this.availableCLIs.has(preferred)) {
      return {
        cli: preferred,
        degraded: false,
        preferred,
        reason: `任务类型 "${category}" 的首选 CLI`,
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
        };
      }
    }

    return {
      cli: preferred,
      degraded: false,
      preferred,
      reason: '没有可用的 CLI，使用默认首选',
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
}

