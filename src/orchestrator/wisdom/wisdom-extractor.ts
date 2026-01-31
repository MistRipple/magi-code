/**
 * Wisdom 提取流水线 - 提案 4.5
 *
 * 从 Worker 执行结果中提取知识，存入现有知识库系统：
 * - MemoryDocument: 会话级知识（decisions, pendingIssues, context）
 * - ProjectKnowledgeBase: 项目级知识（FAQs, ADRs）
 */

import {
  WorkerReport,
  WisdomExtraction,
} from '../protocols/worker-report';
import { logger, LogCategory } from '../../logging';

// ============================================================================
// Wisdom 提取器
// ============================================================================

/**
 * Wisdom 提取结果
 */
export interface WisdomExtractionResult {
  /** 学习到的信息 */
  learnings: string[];
  /** 做出的决策 */
  decisions: string[];
  /** 需要注意的问题 */
  warnings: string[];
  /** 值得持久化的重要经验 */
  significantLearning?: string;
}

/**
 * Wisdom 提取器配置
 */
export interface WisdomExtractorConfig {
  /** 是否启用自动提取 */
  enabled?: boolean;
  /** 最大 learnings 数量 */
  maxLearnings?: number;
  /** 最大 decisions 数量 */
  maxDecisions?: number;
}

/**
 * Wisdom 提取器
 *
 * 从 Worker 输出中提取结构化知识
 */
export class WisdomExtractor {
  private config: Required<WisdomExtractorConfig>;

  constructor(config?: WisdomExtractorConfig) {
    this.config = {
      enabled: config?.enabled ?? true,
      maxLearnings: config?.maxLearnings ?? 5,
      maxDecisions: config?.maxDecisions ?? 5,
    };
  }

  /**
   * 从 WorkerReport 提取 Wisdom
   */
  extractFromReport(report: WorkerReport): WisdomExtractionResult {
    if (!this.config.enabled) {
      return { learnings: [], decisions: [], warnings: [] };
    }

    const summary = report.result?.summary || '';
    const error = report.error;

    // 如果 report 已有结构化 wisdom，直接使用
    if (report.result?.wisdomExtraction) {
      return {
        learnings: report.result.wisdomExtraction.learnings || [],
        decisions: report.result.wisdomExtraction.decisions || [],
        warnings: report.result.wisdomExtraction.warnings || [],
        significantLearning: report.result.wisdomExtraction.significantLearning,
      };
    }

    // 否则从文本中提取
    const learnings = this.extractLearnings(summary);
    const decisions = this.extractDecisions(summary);
    const warnings = error ? [error] : [];

    // 检测重要经验
    const significantLearning = this.detectSignificantLearning(summary, learnings);

    return {
      learnings: learnings.slice(0, this.config.maxLearnings),
      decisions: decisions.slice(0, this.config.maxDecisions),
      warnings,
      significantLearning,
    };
  }

  /**
   * 从文本中提取 learnings
   */
  extractLearnings(text: string): string[] {
    if (!text) return [];

    const patterns = [
      /发现[：:]?\s*(.+?)(?:[。\n]|$)/g,
      /注意[：:]?\s*(.+?)(?:[。\n]|$)/g,
      /了解到[：:]?\s*(.+?)(?:[。\n]|$)/g,
      /学习到[：:]?\s*(.+?)(?:[。\n]|$)/g,
      /观察到[：:]?\s*(.+?)(?:[。\n]|$)/g,
      /Found[：:]?\s*(.+?)(?:[.\n]|$)/gi,
      /Noticed[：:]?\s*(.+?)(?:[.\n]|$)/gi,
      /Learned[：:]?\s*(.+?)(?:[.\n]|$)/gi,
    ];

    const learnings: string[] = [];
    for (const pattern of patterns) {
      let match;
      // Reset lastIndex for each pattern
      pattern.lastIndex = 0;
      while ((match = pattern.exec(text)) !== null) {
        const learning = match[1].trim();
        if (learning && learning.length > 5 && !learnings.includes(learning)) {
          learnings.push(learning);
        }
      }
    }

    return learnings;
  }

  /**
   * 从文本中提取 decisions
   */
  extractDecisions(text: string): string[] {
    if (!text) return [];

    const patterns = [
      /决定[：:]?\s*(.+?)(?:[。\n]|$)/g,
      /选择[：:]?\s*(.+?)(?:[。\n]|$)/g,
      /采用[：:]?\s*(.+?)(?:[。\n]|$)/g,
      /使用[：:]?\s*(.+?)(?:[。\n]|$)/g,
      /Decided[：:]?\s*(.+?)(?:[.\n]|$)/gi,
      /Chose[：:]?\s*(.+?)(?:[.\n]|$)/gi,
      /Using[：:]?\s*(.+?)(?:[.\n]|$)/gi,
    ];

    const decisions: string[] = [];
    for (const pattern of patterns) {
      let match;
      pattern.lastIndex = 0;
      while ((match = pattern.exec(text)) !== null) {
        const decision = match[1].trim();
        if (decision && decision.length > 5 && !decisions.includes(decision)) {
          decisions.push(decision);
        }
      }
    }

    return decisions;
  }

  /**
   * 检测重要经验（值得跨会话保存）
   */
  private detectSignificantLearning(
    summary: string,
    learnings: string[]
  ): string | undefined {
    if (!summary || learnings.length === 0) return undefined;

    // 检测重要性标记
    const significantPatterns = [
      /重要[：:]?\s*(.+?)(?:[。\n]|$)/,
      /关键[：:]?\s*(.+?)(?:[。\n]|$)/,
      /注意[：:]?\s*(.+?)(?:[。\n]|$)/,
      /Important[：:]?\s*(.+?)(?:[.\n]|$)/i,
      /Critical[：:]?\s*(.+?)(?:[.\n]|$)/i,
      /Note[：:]?\s*(.+?)(?:[.\n]|$)/i,
    ];

    for (const pattern of significantPatterns) {
      const match = summary.match(pattern);
      if (match && match[1]) {
        return match[1].trim();
      }
    }

    // 如果有多个 learnings，选择最长的作为重要经验
    if (learnings.length >= 3) {
      return learnings.reduce((a, b) => a.length > b.length ? a : b);
    }

    return undefined;
  }
}

// ============================================================================
// Wisdom 存储接口（与现有知识库集成）
// ============================================================================

/**
 * Wisdom 存储接口
 * 用于将提取的知识存入现有系统
 */
export interface WisdomStorage {
  /**
   * 存储 learning 到 MemoryDocument.context
   */
  storeLearning(learning: string, sourceAssignmentId: string): void;

  /**
   * 存储 decision 到 MemoryDocument.decisions
   */
  storeDecision(decision: string, sourceAssignmentId: string): void;

  /**
   * 存储 warning 到 MemoryDocument.pendingIssues
   */
  storeWarning(warning: string, sourceAssignmentId: string): void;

  /**
   * 存储重要经验到 ProjectKnowledgeBase
   */
  storeSignificantLearning(learning: string, context: string): void;
}

/**
 * 空操作的 Wisdom 存储
 * 当没有提供存储实现时使用
 */
export class NoOpWisdomStorage implements WisdomStorage {
  storeLearning(): void {}
  storeDecision(): void {}
  storeWarning(): void {}
  storeSignificantLearning(): void {}
}

// ============================================================================
// Wisdom 管理器
// ============================================================================

/**
 * Wisdom 管理器
 * 组合提取和存储功能
 */
export class WisdomManager {
  private extractor: WisdomExtractor;
  private storage: WisdomStorage;

  constructor(
    storage?: WisdomStorage,
    extractorConfig?: WisdomExtractorConfig
  ) {
    this.extractor = new WisdomExtractor(extractorConfig);
    this.storage = storage || new NoOpWisdomStorage();
  }

  /**
   * 处理 WorkerReport，提取并存储 Wisdom
   */
  processReport(report: WorkerReport, assignmentId: string): WisdomExtractionResult {
    const result = this.extractor.extractFromReport(report);

    // 存储 learnings
    for (const learning of result.learnings) {
      this.storage.storeLearning(learning, assignmentId);
    }

    // 存储 decisions
    for (const decision of result.decisions) {
      this.storage.storeDecision(decision, assignmentId);
    }

    // 存储 warnings
    for (const warning of result.warnings) {
      this.storage.storeWarning(warning, assignmentId);
    }

    // 存储重要经验
    if (result.significantLearning) {
      this.storage.storeSignificantLearning(
        result.significantLearning,
        `Assignment: ${assignmentId}`
      );
    }

    logger.debug('Wisdom.提取完成', {
      assignmentId,
      learnings: result.learnings.length,
      decisions: result.decisions.length,
      warnings: result.warnings.length,
      hasSignificant: !!result.significantLearning,
    }, LogCategory.ORCHESTRATOR);

    return result;
  }

  /**
   * 设置存储实现
   */
  setStorage(storage: WisdomStorage): void {
    this.storage = storage;
  }

  /**
   * 获取提取器
   */
  getExtractor(): WisdomExtractor {
    return this.extractor;
  }
}

// ============================================================================
// 导出默认实例
// ============================================================================

let defaultWisdomManager: WisdomManager | null = null;

/**
 * 获取默认 Wisdom 管理器
 */
export function getDefaultWisdomManager(): WisdomManager {
  if (!defaultWisdomManager) {
    defaultWisdomManager = new WisdomManager();
  }
  return defaultWisdomManager;
}

/**
 * 重置默认 Wisdom 管理器（用于测试）
 */
export function resetDefaultWisdomManager(): void {
  defaultWisdomManager = null;
}
