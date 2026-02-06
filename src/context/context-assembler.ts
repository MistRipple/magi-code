/**
 * ContextAssembler - 上下文组装器
 *
 * 按预算分配组装上下文，支持：
 * - 项目知识库 (L0): 10%
 * - 共享任务上下文 (L1): 25%
 * - 任务契约与关键变更 (L1): 15%
 * - 本地最近 N 轮对话 (L2): 40%
 * - 长期记忆召回 (L3): 10%
 *
 * @see docs/context/unified-memory-plan.md 5.3 节和 7.1-7.3 节
 */

import { logger, LogCategory } from '../logging';
import { MemoryDocument } from './memory-document';
import { ProjectKnowledgeBase } from '../knowledge/project-knowledge-base';
import { ContextSource, FileSummary } from './file-summary-cache';
import {
  SharedContextEntry,
  SharedContextEntryType,
  ImportanceLevel,
  FileReference,
  QueryOptions,
} from './shared-context-pool';

// 重新导出类型以便使用者从此模块导入
export { ContextSource, FileSummary } from './file-summary-cache';
export {
  SharedContextEntry,
  SharedContextEntryType,
  ImportanceLevel,
  FileReference,
} from './shared-context-pool';

// ============================================================================
// 类型定义
// ============================================================================

/**
 * Agent 上下文订阅配置
 */
export interface AgentContextSubscription {
  /** Agent 标识 */
  agentId: string;

  /** 订阅的标签（例如 ['architecture', 'api-design']） */
  subscribedTags: string[];

  /** 排除的来源（可选） */
  excludedSources?: ContextSource[];
}

/**
 * Token 预算配置
 */
export interface TokenBudgetConfig {
  /** 总预算 */
  total: number;

  /** 项目知识库占比 (默认 10%) */
  projectKnowledgeRatio: number;

  /** 共享上下文占比 (默认 25%) */
  sharedContextRatio: number;

  /** 契约与变更占比 (默认 15%) */
  contractsRatio: number;

  /** 本地对话占比 (默认 40%) */
  localWindowRatio: number;

  /** 长期记忆占比 (默认 10%) */
  longTermMemoryRatio: number;
}

/**
 * 上下文组装选项
 */
export interface ContextAssemblyOptions {
  /** Mission ID */
  missionId: string;

  /** Agent 订阅配置 */
  subscription: AgentContextSubscription;

  /** Token 预算配置 */
  budget: TokenBudgetConfig;

  /** 最小重要性级别 */
  minImportance?: ImportanceLevel;

  /** 本地对话轮数范围 */
  localTurns?: {
    min: number;
    max: number;
  };
}

/**
 * 上下文部分类型
 */
export type ContextPartType =
  | 'project_knowledge'
  | 'shared_context'
  | 'contracts'
  | 'recent_turns'
  | 'long_term_memory';

/**
 * 上下文部分
 */
export interface ContextPart {
  /** 部分类型 */
  type: ContextPartType;

  /** 内容 */
  content: string;

  /** Token 数量 */
  tokens: number;
}

/**
 * 组装后的上下文
 */
export interface AssembledContext {
  /** 上下文各部分 */
  parts: ContextPart[];

  /** 总 Token 数 */
  totalTokens: number;

  /** 预算使用率 (0-1) */
  budgetUsage: number;
}

/**
 * 共享上下文查询选项
 * 该类型是 QueryOptions 的别名
 */
export type SharedContextQueryOptions = QueryOptions;

/**
 * SharedContextPool 接口定义
 */
export interface ISharedContextPool {
  /**
   * 按 Mission 获取条目
   */
  getByMission(missionId: string, options?: QueryOptions): SharedContextEntry[];

  /**
   * 按类型获取条目
   */
  getByType(missionId: string, type: SharedContextEntryType, maxTokens?: number): SharedContextEntry[];

  /**
   * 添加条目
   */
  add(entry: SharedContextEntry): { action: 'added' | 'merged'; id?: string; existingId?: string };
}

/**
 * FileSummaryCache 接口定义
 * 文件摘要缓存，以 filePath + fileHash 为 key
 */
export interface IFileSummaryCache {
  /**
   * 获取文件摘要
   */
  get(filePath: string, currentHash: string): FileSummary | null;

  /**
   * 设置文件摘要
   */
  set(filePath: string, fileHash: string, summary: FileSummary, source: ContextSource): void;

  /**
   * 检查是否存在有效摘要
   */
  has(filePath: string, fileHash: string): boolean;
}

/**
 * 本地对话轮次获取选项
 */
interface LocalTurnsOptions {
  maxTokens: number;
  minTurns: number;
  maxTurns: number;
  prioritizeDecisionPoints: boolean;
}

// ============================================================================
// 默认配置
// ============================================================================

/**
 * 默认 Token 预算配置
 */
export const DEFAULT_TOKEN_BUDGET: TokenBudgetConfig = {
  total: 8000,
  projectKnowledgeRatio: 0.10,  // 10%
  sharedContextRatio: 0.25,     // 25%
  contractsRatio: 0.15,         // 15%
  localWindowRatio: 0.40,       // 40%
  longTermMemoryRatio: 0.10,    // 10%
};

/**
 * 默认本地对话轮数配置
 */
export const DEFAULT_LOCAL_TURNS = {
  min: 1,
  max: 10,
};

// ============================================================================
// ContextAssembler 类
// ============================================================================

/**
 * 上下文组装器
 * 按预算分配组装最终上下文
 */
export class ContextAssembler {
  constructor(
    private projectKnowledgeBase: ProjectKnowledgeBase | null,
    private sharedContextPool: ISharedContextPool | null,
    private fileSummaryCache: IFileSummaryCache | null,
    private memoryDocument: MemoryDocument | null,
    private recentTurnsProvider?: (agentId: string, options: LocalTurnsOptions) => Promise<string | null>
  ) {}

  /**
   * 组装上下文
   * 按预算分配从各层收集上下文
   *
   * @param options 组装选项
   * @returns 组装后的上下文
   */
  async assemble(options: ContextAssemblyOptions): Promise<AssembledContext> {
    const { missionId, subscription, budget } = options;
    const parts: ContextPart[] = [];
    let usedTokens = 0;

    logger.info('上下文组装.开始', {
      missionId,
      agentId: subscription.agentId,
      totalBudget: budget.total,
    }, LogCategory.SESSION);

    // 1. 项目知识库 (L0) - 10%
    const pkbBudget = Math.floor(budget.total * budget.projectKnowledgeRatio);
    const pkbPart = await this.assembleProjectKnowledge(pkbBudget);
    if (pkbPart) {
      parts.push(pkbPart);
      usedTokens += pkbPart.tokens;
    }

    // 2. 共享任务上下文 (L1) - 高优先级 25%
    const sharedBudget = Math.floor(budget.total * budget.sharedContextRatio);
    const sharedPart = await this.assembleSharedContext(
      missionId,
      subscription,
      options.minImportance || 'medium',
      sharedBudget
    );
    if (sharedPart) {
      parts.push(sharedPart);
      usedTokens += sharedPart.tokens;
    }

    // 3. 任务契约与变更 (L1) - 15%
    const contractBudget = Math.floor(budget.total * budget.contractsRatio);
    const contractPart = await this.assembleContracts(missionId, contractBudget);
    if (contractPart) {
      parts.push(contractPart);
      usedTokens += contractPart.tokens;
    }

    // 4. 本地最近 N 轮对话 (L2) - 40%
    const localBudget = Math.floor(budget.total * budget.localWindowRatio);
    const localTurnsConfig = options.localTurns || DEFAULT_LOCAL_TURNS;
    const localPart = await this.assembleRecentTurns(
      subscription.agentId,
      {
        maxTokens: localBudget,
        minTurns: localTurnsConfig.min,
        maxTurns: localTurnsConfig.max,
        prioritizeDecisionPoints: true,
      }
    );
    if (localPart) {
      parts.push(localPart);
      usedTokens += localPart.tokens;
    }

    // 5. 长期记忆召回 (L3) - 10%
    const memoryBudget = Math.floor(budget.total * budget.longTermMemoryRatio);
    const memoryPart = await this.assembleLongTermMemory(memoryBudget);
    if (memoryPart) {
      parts.push(memoryPart);
      usedTokens += memoryPart.tokens;
    }

    const result: AssembledContext = {
      parts,
      totalTokens: usedTokens,
      budgetUsage: budget.total > 0 ? usedTokens / budget.total : 0,
    };

    logger.info('上下文组装.完成', {
      missionId,
      partsCount: parts.length,
      totalTokens: usedTokens,
      budgetUsage: `${(result.budgetUsage * 100).toFixed(1)}%`,
    }, LogCategory.SESSION);

    return result;
  }

  /**
   * 组装项目知识库上下文 (L0)
   */
  private async assembleProjectKnowledge(maxTokens: number): Promise<ContextPart | null> {
    if (!this.projectKnowledgeBase || maxTokens <= 0) {
      return null;
    }

    try {
      const content = this.projectKnowledgeBase.getProjectContext(maxTokens);
      if (!content) {
        return null;
      }

      const tokens = this.estimateTokens(content);
      return {
        type: 'project_knowledge',
        content,
        tokens,
      };
    } catch (error) {
      logger.warn('上下文组装.项目知识库.失败', { error }, LogCategory.SESSION);
      return null;
    }
  }

  /**
   * 组装共享上下文 (L1)
   */
  private async assembleSharedContext(
    missionId: string,
    subscription: AgentContextSubscription,
    minImportance: ImportanceLevel,
    maxTokens: number
  ): Promise<ContextPart | null> {
    if (!this.sharedContextPool || maxTokens <= 0) {
      return null;
    }

    try {
      const entries = this.sharedContextPool.getByMission(missionId, {
        minImportance,
        subscribedTags: subscription.subscribedTags,
        excludeSources: subscription.excludedSources,
        maxTokens,
      });

      if (entries.length === 0) {
        return null;
      }

      // 格式化共享上下文条目
      const content = this.formatSharedEntries(entries);
      const tokens = this.estimateTokens(content);

      return {
        type: 'shared_context',
        content,
        tokens,
      };
    } catch (error) {
      logger.warn('上下文组装.共享上下文.失败', { error }, LogCategory.SESSION);
      return null;
    }
  }

  /**
   * 组装任务契约 (L1)
   */
  private async assembleContracts(missionId: string, maxTokens: number): Promise<ContextPart | null> {
    if (!this.sharedContextPool || maxTokens <= 0) {
      return null;
    }

    try {
      const contracts = this.sharedContextPool.getByType(missionId, 'contract', maxTokens);

      if (contracts.length === 0) {
        return null;
      }

      // 格式化契约条目
      const content = this.formatContracts(contracts);
      const tokens = this.estimateTokens(content);

      return {
        type: 'contracts',
        content,
        tokens,
      };
    } catch (error) {
      logger.warn('上下文组装.契约.失败', { error }, LogCategory.SESSION);
      return null;
    }
  }

  /**
   * 组装本地最近对话 (L2)
   */
  private async assembleRecentTurns(
    agentId: string,
    options: LocalTurnsOptions
  ): Promise<ContextPart | null> {
    if (!this.recentTurnsProvider || options.maxTokens <= 0) {
      return null;
    }

    try {
      const content = await this.recentTurnsProvider(agentId, options);
      if (!content) {
        return null;
      }

      const tokens = this.estimateTokens(content);
      return {
        type: 'recent_turns',
        content,
        tokens,
      };
    } catch (error) {
      logger.warn('上下文组装.本地对话.失败', { error }, LogCategory.SESSION);
      return null;
    }
  }

  /**
   * 组装长期记忆 (L3)
   */
  private async assembleLongTermMemory(maxTokens: number): Promise<ContextPart | null> {
    if (!this.memoryDocument || maxTokens <= 0) {
      return null;
    }

    try {
      const content = this.getRelevantMemorySummary(maxTokens);
      if (!content) {
        return null;
      }

      const tokens = this.estimateTokens(content);
      return {
        type: 'long_term_memory',
        content,
        tokens,
      };
    } catch (error) {
      logger.warn('上下文组装.长期记忆.失败', { error }, LogCategory.SESSION);
      return null;
    }
  }

  /**
   * 获取相关记忆摘要
   * 从 MemoryDocument 中提取关键信息
   */
  private getRelevantMemorySummary(maxTokens: number): string | null {
    if (!this.memoryDocument) {
      return null;
    }

    const memory = this.memoryDocument.getContent();
    const lines: string[] = [];

    // 优先级 1: 核心意图
    if (memory.primaryIntent) {
      lines.push(`**核心意图**: ${memory.primaryIntent}`);
    }

    // 优先级 2: 用户约束
    if (memory.userConstraints.length > 0) {
      lines.push('**用户约束**:');
      memory.userConstraints.forEach(c => lines.push(`- ${c}`));
    }

    // 优先级 3: 关键决策
    if (memory.keyDecisions.length > 0) {
      lines.push('**关键决策**:');
      memory.keyDecisions.slice(-3).forEach(d => {
        lines.push(`- ${d.description}: ${d.reason}`);
      });
    }

    // 优先级 4: 待解决问题
    if (memory.pendingIssues.length > 0) {
      lines.push('**待解决问题**:');
      memory.pendingIssues.slice(-3).forEach(i => {
        lines.push(`- ${i.description}`);
      });
    }

    const content = lines.join('\n');

    // 检查是否超出预算
    const tokens = this.estimateTokens(content);
    if (tokens > maxTokens) {
      // 按比例截断
      const ratio = maxTokens / tokens;
      const maxChars = Math.floor(content.length * ratio);
      return content.substring(0, maxChars) + '\n[... 已裁剪]';
    }

    return content || null;
  }

  /**
   * 格式化共享上下文条目
   *
   * @param entries 共享上下文条目列表
   * @returns 格式化后的文本
   */
  formatSharedEntries(entries: SharedContextEntry[]): string {
    return entries.map(e => {
      const timestamp = new Date(e.createdAt).toISOString();
      const sourceLabel = e.sources ? e.sources.join('+') : e.source;
      return `[${sourceLabel}@${timestamp}] (${e.type}|${e.importance}): ${e.content}`;
    }).join('\n\n');
  }

  /**
   * 格式化契约条目
   */
  private formatContracts(contracts: SharedContextEntry[]): string {
    return contracts.map((c, index) => {
      const timestamp = new Date(c.createdAt).toISOString();
      return `## 契约 ${index + 1}\n- **来源**: ${c.source}\n- **时间**: ${timestamp}\n- **内容**: ${c.content}`;
    }).join('\n\n');
  }

  /**
   * Token 估算
   * 约 4 字符 = 1 token
   *
   * @param text 文本内容
   * @returns 估算的 Token 数量
   */
  estimateTokens(text: string): number {
    return Math.ceil(text.length / 4);
  }
}

// ============================================================================
// OverflowTrimmer 类
// ============================================================================

/**
 * 超限裁剪器
 * 当上下文超限时，按优先级裁剪
 */
export class OverflowTrimmer {
  /**
   * 裁剪优先级顺序（优先裁剪的类型在前）
   * - long_term_memory: 优先裁剪长期记忆
   * - shared_context: 然后裁剪共享上下文（低 importance 优先）
   * - contracts: 再裁剪契约
   * - project_knowledge: 最后裁剪项目知识
   * - recent_turns: 保护最后，最后才动
   */
  private static readonly TRIM_ORDER: ContextPartType[] = [
    'long_term_memory',
    'shared_context',
    'contracts',
    'project_knowledge',
    // recent_turns 不在此列表中，意味着它被保护
  ];

  /**
   * 裁剪上下文以适应目标 Token 数
   *
   * @param context 原始组装上下文
   * @param targetTokens 目标 Token 数
   * @returns 裁剪后的上下文
   */
  trim(context: AssembledContext, targetTokens: number): AssembledContext {
    // 如果未超限，直接返回
    if (context.totalTokens <= targetTokens) {
      return context;
    }

    let remaining = context.totalTokens - targetTokens;
    const trimmedParts = context.parts.map(p => ({ ...p }));

    logger.info('上下文裁剪.开始', {
      originalTokens: context.totalTokens,
      targetTokens,
      toTrim: remaining,
    }, LogCategory.SESSION);

    // 按优先级裁剪
    for (const type of OverflowTrimmer.TRIM_ORDER) {
      if (remaining <= 0) break;

      const partIndex = trimmedParts.findIndex(p => p.type === type);
      if (partIndex === -1) continue;

      const part = trimmedParts[partIndex];
      const trimAmount = Math.min(remaining, part.tokens);

      if (trimAmount > 0) {
        // 计算保留比例
        const trimRatio = 1 - (trimAmount / part.tokens);

        if (trimRatio <= 0) {
          // 完全移除此部分
          trimmedParts.splice(partIndex, 1);
          logger.info('上下文裁剪.移除', { type, removedTokens: part.tokens }, LogCategory.SESSION);
        } else {
          // 按比例裁剪内容
          part.content = this.truncateContent(part.content, trimRatio);
          const newTokens = Math.ceil(part.content.length / 4);
          const actualTrimmed = part.tokens - newTokens;
          part.tokens = newTokens;
          remaining -= actualTrimmed;

          logger.info('上下文裁剪.截断', {
            type,
            originalTokens: part.tokens + actualTrimmed,
            newTokens,
            trimRatio: `${(trimRatio * 100).toFixed(1)}%`,
          }, LogCategory.SESSION);
        }

        remaining -= trimAmount;
      }
    }

    // 如果仍然超限，裁剪 recent_turns
    if (remaining > 0) {
      const recentIndex = trimmedParts.findIndex(p => p.type === 'recent_turns');
      if (recentIndex !== -1) {
        const part = trimmedParts[recentIndex];
        const trimAmount = Math.min(remaining, part.tokens);

        if (trimAmount > 0) {
          const trimRatio = 1 - (trimAmount / part.tokens);
          if (trimRatio <= 0) {
            trimmedParts.splice(recentIndex, 1);
          } else {
            part.content = this.truncateContent(part.content, trimRatio);
            part.tokens = Math.ceil(part.content.length / 4);
          }
        }
      }
    }

    // 计算最终 Token 数
    const finalTokens = trimmedParts.reduce((sum, p) => sum + p.tokens, 0);

    const result: AssembledContext = {
      parts: trimmedParts,
      totalTokens: finalTokens,
      budgetUsage: targetTokens > 0 ? Math.min(1, finalTokens / targetTokens) : 0,
    };

    logger.info('上下文裁剪.完成', {
      originalTokens: context.totalTokens,
      finalTokens,
      trimmedParts: context.parts.length - trimmedParts.length,
    }, LogCategory.SESSION);

    return result;
  }

  /**
   * 截断内容到指定比例
   *
   * @param content 原始内容
   * @param ratio 保留比例 (0-1)
   * @returns 截断后的内容
   */
  truncateContent(content: string, ratio: number): string {
    if (ratio >= 1) {
      return content;
    }

    if (ratio <= 0) {
      return '';
    }

    const targetLength = Math.floor(content.length * ratio);
    if (targetLength <= 0) {
      return '';
    }

    // 尝试在自然断点处截断（换行符）
    const truncated = content.substring(0, targetLength);
    const lastNewline = truncated.lastIndexOf('\n');

    if (lastNewline > targetLength * 0.5) {
      // 如果换行符在后半部分，使用它作为截断点
      return truncated.substring(0, lastNewline) + '\n[... 已裁剪]';
    }

    return truncated + '\n[... 已裁剪]';
  }
}

// ============================================================================
// 辅助函数
// ============================================================================

/**
 * 创建默认的上下文组装选项
 */
export function createDefaultAssemblyOptions(
  missionId: string,
  agentId: string,
  subscribedTags: string[] = []
): ContextAssemblyOptions {
  return {
    missionId,
    subscription: {
      agentId,
      subscribedTags,
    },
    budget: { ...DEFAULT_TOKEN_BUDGET },
    minImportance: 'medium',
    localTurns: { ...DEFAULT_LOCAL_TURNS },
  };
}

/**
 * 将组装后的上下文转换为字符串
 */
export function assembledContextToString(context: AssembledContext): string {
  const sections: string[] = [];

  for (const part of context.parts) {
    const header = getPartHeader(part.type);
    sections.push(`## ${header}\n\n${part.content}`);
  }

  return sections.join('\n\n---\n\n');
}

/**
 * 获取部分类型的中文标题
 */
function getPartHeader(type: ContextPartType): string {
  const headers: Record<ContextPartType, string> = {
    project_knowledge: '项目知识',
    shared_context: '共享上下文',
    contracts: '任务契约',
    recent_turns: '最近对话',
    long_term_memory: '长期记忆',
  };
  return headers[type] || type;
}

/**
 * 重要性级别转数值分数
 */
export function importanceToScore(importance: ImportanceLevel): number {
  const scores: Record<ImportanceLevel, number> = {
    critical: 4,
    high: 3,
    medium: 2,
    low: 1,
  };
  return scores[importance];
}

/**
 * 检查重要性是否满足最小要求
 */
export function meetsImportance(entry: SharedContextEntry, minLevel: ImportanceLevel): boolean {
  return importanceToScore(entry.importance) >= importanceToScore(minLevel);
}
