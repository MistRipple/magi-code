/**
 * ContextManager - 三层上下文管理器
 * Layer 1: 即时上下文（最近几轮对话）
 * Layer 2: 会话 Memory（结构化任务记录）
 * Layer 3: 项目知识库（跨会话知识）
 *
 * 混合策略：
 * - Augment 风格预防性截断（即时上下文）
 * - LLM 智能压缩（Memory 文档）
 */

import { logger, LogCategory } from '../logging';
import * as path from 'path';
import { MemoryDocument } from './memory-document';
import { TruncationUtils } from './truncation-utils';
import {
  ContextMessage,
  ContextManagerConfig,
  DEFAULT_CONTEXT_CONFIG,
  MemoryContent
} from './types';
import { UnifiedSessionManager, SessionSummary } from '../session/unified-session-manager';
import { ProjectKnowledgeBase } from '../knowledge/project-knowledge-base';

type MemorySummaryOptions = {
  includeCurrentTasks?: boolean;
  includeKeyDecisions?: number;
  includeImportantContext?: boolean;
  includePendingIssues?: boolean;
  includeCompletedTasks?: number;
  includeCodeChanges?: number;
};

type ContextSliceOptions = {
  maxTokens: number;
  memoryRatio?: number;
  includeMemory?: boolean;
  includeRecent?: boolean;
  memorySummary?: MemorySummaryOptions;
};

export class ContextManager {
  private config: ContextManagerConfig;
  private immediateContext: ContextMessage[] = [];
  private sessionMemory: MemoryDocument | null = null;
  private initialized: boolean = false;
  private truncationUtils: TruncationUtils;
  private sessionManager: UnifiedSessionManager | null = null;
  private currentSessionId: string | null = null;
  private projectKnowledgeBase: ProjectKnowledgeBase | null = null;

  constructor(
    private workspacePath: string,
    config: Partial<ContextManagerConfig> = {},
    sessionManager?: UnifiedSessionManager
  ) {
    this.config = { ...DEFAULT_CONTEXT_CONFIG, ...config };
    this.truncationUtils = new TruncationUtils(this.config.compression.truncation);
    this.sessionManager = sessionManager || null;
  }

  /**
   * 设置 SessionManager（用于获取会话总结）
   */
  setSessionManager(sessionManager: UnifiedSessionManager): void {
    this.sessionManager = sessionManager;
  }

  /**
   * 设置当前会话 ID（用于获取会话总结）
   */
  setCurrentSessionId(sessionId: string): void {
    this.currentSessionId = sessionId;
  }

  /**
   * 设置 ProjectKnowledgeBase（用于获取项目知识）
   */
  setProjectKnowledgeBase(knowledgeBase: ProjectKnowledgeBase): void {
    this.projectKnowledgeBase = knowledgeBase;
    logger.info('上下文.项目知识库.已设置', undefined, LogCategory.SESSION);
  }

  /**
   * 初始化上下文管理器
   */
  async initialize(sessionId: string, sessionName: string): Promise<void> {
    const storagePath = path.join(this.workspacePath, this.config.storagePath);
    this.sessionMemory = new MemoryDocument(sessionId, sessionName, storagePath);
    await this.sessionMemory.load();
    this.initialized = true;
    logger.info('上下文.初始化.完成', { sessionId }, LogCategory.SESSION);
  }

  /**
   * 添加消息到即时上下文
   * 自动应用 Augment 风格截断
   */
  addMessage(message: Omit<ContextMessage, 'timestamp'>, applyTruncation: boolean = true): void {
    let content = message.content;

    // Augment 风格：对长消息进行预防性截断
    if (applyTruncation && this.config.compression.truncation.enabled) {
      const truncated = this.truncationUtils.truncateMessage(content);
      if (truncated.wasTruncated) {
        logger.info(
          '上下文.截断.已应用',
          { originalLength: truncated.originalLength, truncatedLength: truncated.truncatedLength },
          LogCategory.SESSION
        );
        content = truncated.content;
      }
    }

    const msg: ContextMessage = {
      ...message,
      content,
      timestamp: new Date().toISOString(),
      tokenCount: this.estimateTokens(content)
    };

    this.immediateContext.push(msg);

    // 保持即时上下文在限定轮数内（每轮包含 user + assistant）
    const maxMessages = this.config.immediateContextRounds * 2;
    if (this.immediateContext.length > maxMessages) {
      // ✅ P1修复: 将被移除的消息转存到 Memory
      const systemMessages = this.immediateContext.filter(m => m.role === 'system');
      const otherMessages = this.immediateContext.filter(m => m.role !== 'system');

      // 提取将被移除的消息
      const toRemove = otherMessages.slice(0, -maxMessages);

      // 转存到 Memory
      for (const removedMsg of toRemove) {
        this.migrateToMemory(removedMsg);
      }

      // 清理即时上下文
      this.immediateContext = [
        ...systemMessages,
        ...otherMessages.slice(-maxMessages)
      ];

      logger.info('上下文.即时上下文.已裁剪', { removedCount: toRemove.length }, LogCategory.SESSION);
    }
  }

  /**
   * 截断工具输出（Augment 风格）
   * 用于处理工具返回的大量数据
   */
  truncateToolOutput(output: string): { content: string; wasTruncated: boolean } {
    const result = this.truncationUtils.truncateToolOutput(output);
    return {
      content: result.content,
      wasTruncated: result.wasTruncated
    };
  }

  /**
   * 截断代码块
   */
  truncateCodeBlock(code: string, maxLines?: number): { content: string; wasTruncated: boolean } {
    const result = this.truncationUtils.truncateCodeBlock(code, maxLines);
    return {
      content: result.content,
      wasTruncated: result.wasTruncated
    };
  }

  /**
   * 获取组装后的上下文（用于发送给 LLM）
   */
  getContext(maxTokens: number = 8000): string {
    return this.getContextSlice({ maxTokens });
  }

  /**
   * 获取受限上下文切片（用于 Worker 精简上下文）
   */
  getContextSlice(options: ContextSliceOptions): string {
    const {
      maxTokens,
      memoryRatio = 0.3,
      includeMemory = true,
      includeRecent = true,
      memorySummary = {},
    } = options;
    const parts: string[] = [];
    let currentTokens = 0;

    // 0. 添加项目知识（如果有 ProjectKnowledgeBase）- 最高优先级
    if (this.projectKnowledgeBase) {
      const projectBudget = Math.floor(maxTokens * 0.1); // 最多占用 10% 的 token 预算
      const projectContext = this.projectKnowledgeBase.getProjectContext(projectBudget);

      if (projectContext) {
        const projectTokens = this.estimateTokens(projectContext);
        parts.push('## 项目知识\n' + projectContext);
        currentTokens += projectTokens;
        logger.info('上下文.项目知识.已注入', {
          tokens: projectTokens,
          budget: projectBudget
        }, LogCategory.SESSION);
      }
    }

    // 1. 添加会话总结（如果有 SessionManager 和当前会话 ID）
    if (this.sessionManager && this.currentSessionId) {
      const sessionSummary = this.sessionManager.getSessionSummary(this.currentSessionId);
      if (sessionSummary) {
        const summaryText = this.formatSessionSummaryForContext(sessionSummary);
        const summaryTokens = this.estimateTokens(summaryText);
        const summaryBudget = Math.floor(maxTokens * 0.2); // 最多占用 20% 的 token 预算

        if (summaryTokens <= summaryBudget) {
          parts.push(summaryText);
          currentTokens += summaryTokens;
          logger.info('上下文.会话总结.已注入', {
            sessionId: this.currentSessionId,
            tokens: summaryTokens
          }, LogCategory.SESSION);
        } else {
          // 如果总结太长，进行截断
          const truncated = this.truncationUtils.truncateMessage(summaryText, summaryBudget * 4);
          parts.push(truncated.content);
          currentTokens += this.estimateTokens(truncated.content);
          logger.info('上下文.会话总结.已截断', {
            sessionId: this.currentSessionId,
            originalTokens: summaryTokens,
            truncatedTokens: this.estimateTokens(truncated.content)
          }, LogCategory.SESSION);
        }
      }
    }

    // 2. 添加会话 Memory 摘要（高优先级）
    if (includeMemory && this.sessionMemory) {
      const summary = this.buildMemorySummary(memorySummary);
      if (summary) {
        const memoryTokens = this.estimateTokens(summary);
        const memoryBudget = Math.max(0, Math.floor((maxTokens - currentTokens) * memoryRatio));
        if (memoryBudget > 0) {
          if (memoryTokens <= memoryBudget) {
            parts.push('## 会话上下文\n' + summary);
            currentTokens += memoryTokens;
          } else {
            const truncated = this.truncationUtils.truncateMessage(summary, memoryBudget * 4);
            parts.push('## 会话上下文\n' + truncated.content);
            currentTokens += this.estimateTokens(truncated.content);
          }
        }
      }
    }

    // 3. 添加即时上下文（最近对话）
    if (includeRecent) {
      const remainingTokens = maxTokens - currentTokens;
      const recentMessages = this.getRecentMessages(remainingTokens);
      if (recentMessages.length > 0) {
        parts.push('## 最近对话\n' + recentMessages.map(m =>
          `[${m.role}]: ${m.content}`
        ).join('\n\n'));
      }
    }

    return parts.join('\n\n---\n\n');
  }

  /**
   * 格式化会话总结为上下文文本
   */
  private formatSessionSummaryForContext(summary: SessionSummary): string {
    const lines: string[] = [];

    lines.push('## 会话总结');
    lines.push('');
    lines.push(`**会话**: ${summary.title}`);
    lines.push(`**目标**: ${summary.objective}`);
    lines.push(`**消息数**: ${summary.messageCount} 条`);
    lines.push('');

    if (summary.completedTasks.length > 0) {
      lines.push('**已完成任务**:');
      summary.completedTasks.forEach((task, i) => {
        lines.push(`${i + 1}. ${task}`);
      });
      lines.push('');
    }

    if (summary.inProgressTasks.length > 0) {
      lines.push('**进行中任务**:');
      summary.inProgressTasks.forEach((task, i) => {
        lines.push(`${i + 1}. ${task}`);
      });
      lines.push('');
    }

    if (summary.keyDecisions.length > 0) {
      lines.push('**关键决策**:');
      summary.keyDecisions.forEach((decision, i) => {
        lines.push(`${i + 1}. ${decision}`);
      });
      lines.push('');
    }

    if (summary.codeChanges.length > 0) {
      lines.push('**代码变更**:');
      summary.codeChanges.slice(0, 10).forEach((change, i) => {
        lines.push(`${i + 1}. ${change}`);
      });
      if (summary.codeChanges.length > 10) {
        lines.push(`... 还有 ${summary.codeChanges.length - 10} 个文件`);
      }
      lines.push('');
    }

    if (summary.pendingIssues.length > 0) {
      lines.push('**待解决问题**:');
      summary.pendingIssues.forEach((issue, i) => {
        lines.push(`${i + 1}. ${issue}`);
      });
      lines.push('');
    }

    return lines.join('\n');
  }

  /**
   * 获取 Memory 摘要
   */
  private getMemorySummary(): string {
    return this.buildMemorySummary({
      includeCurrentTasks: true,
      includeKeyDecisions: 3,
      includeImportantContext: true,
    });
  }

  private buildMemorySummary(options: MemorySummaryOptions): string {
    if (!this.sessionMemory) return '';
    
    const content = this.sessionMemory.getContent();
    const lines: string[] = [];
    const {
      includeCurrentTasks = false,
      includeKeyDecisions = 0,
      includeImportantContext = false,
      includePendingIssues = false,
      includeCompletedTasks = 0,
      includeCodeChanges = 0,
    } = options;

    // 当前任务
    if (includeCurrentTasks && content.currentTasks.length > 0) {
      lines.push('**当前任务:**');
      content.currentTasks.forEach(t => {
        lines.push(`- ${t.description} (${t.status})`);
      });
    }

    // 关键决策（最近3个）
    if (includeKeyDecisions > 0 && content.keyDecisions.length > 0) {
      lines.push('**关键决策:**');
      content.keyDecisions.slice(-includeKeyDecisions).forEach(d => {
        lines.push(`- ${d.description}`);
      });
    }

    // 重要上下文
    if (includeImportantContext && content.importantContext.length > 0) {
      lines.push('**重要上下文:**');
      content.importantContext.forEach(ctx => {
        lines.push(`- ${ctx}`);
      });
    }

    if (includePendingIssues && content.pendingIssues.length > 0) {
      lines.push('**待解决问题:**');
      content.pendingIssues.slice(-5).forEach(issue => {
        lines.push(`- ${issue}`);
      });
    }

    if (includeCompletedTasks > 0 && content.completedTasks.length > 0) {
      lines.push('**近期完成:**');
      content.completedTasks.slice(-includeCompletedTasks).forEach(task => {
        lines.push(`- ${task.description}`);
      });
    }

    if (includeCodeChanges > 0 && content.codeChanges.length > 0) {
      lines.push('**近期代码变更:**');
      content.codeChanges.slice(-includeCodeChanges).forEach(change => {
        lines.push(`- ${change.file}: ${change.summary}`);
      });
    }

    return lines.join('\n');
  }

  /**
   * 获取最近消息（在 token 限制内）
   */
  private getRecentMessages(maxTokens: number): ContextMessage[] {
    const result: ContextMessage[] = [];
    let tokens = 0;

    // 从最新的消息开始添加
    for (let i = this.immediateContext.length - 1; i >= 0; i--) {
      const msg = this.immediateContext[i];
      const msgTokens = msg.tokenCount || this.estimateTokens(msg.content);

      if (tokens + msgTokens > maxTokens) {
        if (result.length === 0 && maxTokens > 0) {
          const truncated = this.truncationUtils.truncateMessage(msg.content, maxTokens * 4);
          result.unshift({
            ...msg,
            content: truncated.content,
            tokenCount: this.estimateTokens(truncated.content),
          });
        }
        break;
      }

      result.unshift(msg);
      tokens += msgTokens;
    }

    return result;
  }

  /**
   * 估算 token 数量
   */
  private estimateTokens(text: string): number {
    return Math.ceil(text.length / 4);
  }

  // ========== Memory 操作代理方法 ==========

  /**
   * 获取 Memory 文档
   */
  getMemoryDocument(): MemoryDocument | null {
    return this.sessionMemory;
  }

  /**
   * 添加任务到 Memory
   */
  addTask(task: { id: string; description: string; status: 'pending' | 'in_progress'; assignedWorker?: string }): void {
    this.sessionMemory?.addCurrentTask(task);
  }

  /**
   * 更新任务状态
   */
  updateTaskStatus(taskId: string, status: 'pending' | 'in_progress' | 'completed' | 'failed', result?: string): void {
    this.sessionMemory?.updateTaskStatus(taskId, status, result);
  }

  /**
   * 添加关键决策
   */
  addDecision(id: string, description: string, reason: string): void {
    this.sessionMemory?.addDecision({ id, description, reason });
  }

  /**
   * 添加代码变更
   */
  addCodeChange(file: string, action: 'add' | 'modify' | 'delete', summary: string): void {
    this.sessionMemory?.addCodeChange({ file, action, summary });
  }

  /**
   * 添加重要上下文
   */
  addImportantContext(context: string): void {
    this.sessionMemory?.addImportantContext(context);
  }

  /**
   * 添加待解决问题
   */
  addPendingIssue(issue: string): void {
    this.sessionMemory?.addPendingIssue(issue);
  }

  /**
   * 检查是否需要压缩
   */
  needsCompression(): boolean {
    if (!this.sessionMemory) return false;
    return this.sessionMemory.needsCompression(
      this.config.compression.tokenLimit,
      this.config.compression.lineLimit
    );
  }

  /**
   * 保存 Memory
   */
  async saveMemory(): Promise<void> {
    if (this.sessionMemory?.isDirty()) {
      await this.sessionMemory.save();
    }
  }

  /**
   * 清理即时上下文
   */
  clearImmediateContext(): void {
    this.immediateContext = [];
  }

  /**
   * 获取即时上下文消息数量
   */
  getImmediateContextSize(): number {
    return this.immediateContext.length;
  }

  /**
   * 获取总 token 估算
   */
  getTotalTokenEstimate(): number {
    let total = 0;

    // 即时上下文
    this.immediateContext.forEach(msg => {
      total += msg.tokenCount || this.estimateTokens(msg.content);
    });

    // Memory
    if (this.sessionMemory) {
      total += this.sessionMemory.getContent().tokenEstimate;
    }

    return total;
  }

  /**
   * 导出当前状态（用于调试）
   */
  exportState(): {
    immediateContextCount: number;
    memoryTokens: number;
    totalTokens: number;
    needsCompression: boolean;
  } {
    return {
      immediateContextCount: this.immediateContext.length,
      memoryTokens: this.sessionMemory?.getContent().tokenEstimate || 0,
      totalTokens: this.getTotalTokenEstimate(),
      needsCompression: this.needsCompression()
    };
  }

  // ============================================================================
  // 私有辅助方法 - 上下文自动转存
  // ============================================================================

  /**
   * 将消息转存到 SessionMemory
   * 自动提取关键信息并结构化存储
   */
  private migrateToMemory(message: ContextMessage): void {
    if (!this.sessionMemory) {
      logger.warn('上下文.记忆.未_初始化', undefined, LogCategory.SESSION);
      return;
    }

    try {
      // 用户消息 -> 提取需求和决策
      if (message.role === 'user') {
        const content = message.content;

        // 检测是否包含技术决策
        if (this.containsTechnicalDecision(content)) {
          this.sessionMemory.addDecision({
            id: `decision-${Date.now()}-${Math.random().toString(36).substring(2, 9)}`,
            description: this.extractDecisionSummary(content),
            reason: '用户需求',
          });
        }

        // 保存重要的用户需求
        if (content.length > 50) {
          this.sessionMemory.addImportantContext(
            `用户需求: ${content.substring(0, 200)}${content.length > 200 ? '...' : ''}`
          );
        }
      }

      // 助手消息 -> 提取任务和变更
      if (message.role === 'assistant') {
        const content = message.content;

        // 提取已完成任务
        const taskPattern = /(?:完成|创建|实现|修改|添加|优化)(?:了)?[:：]\s*([^\n]{10,100})/g;
        const taskMatches = content.match(taskPattern);
        if (taskMatches && taskMatches.length > 0) {
          for (const match of taskMatches.slice(0, 3)) {  // 最多提取3个任务
            const description = match.replace(/[:：]/g, ': ').substring(0, 100);
            // 将任务添加为已完成状态
            const taskId = `task-${Date.now()}-${Math.random().toString(36).substring(2, 9)}`;
            this.sessionMemory.addCurrentTask({
              id: taskId,
              description,
              status: 'completed',
            });
          }
        }

        // 提取文件变更
        const filePattern = /(?:创建|修改|删除)(?:了)?\s+[`']?([a-zA-Z0-9\-_/\\.]+\.[a-z]+)[`']?/g;
        const fileMatches = content.match(filePattern);
        if (fileMatches && fileMatches.length > 0) {
          for (const match of fileMatches.slice(0, 5)) {  // 最多提取5个文件
            const actionMatch = match.match(/(创建|修改|删除)/);
            const fileMatch = match.match(/([a-zA-Z0-9\-_/\\.]+\.[a-z]+)/);
            if (actionMatch && fileMatch) {
              const action = this.mapActionType(actionMatch[1]);
              this.sessionMemory.addCodeChange({
                file: fileMatch[1],
                action,
                summary: match.substring(0, 100),
              });
            }
          }
        }

        // 提取关键结论
        const conclusionPattern = /(?:因此|所以|结论|总结)[:：]\s*([^\n]{10,200})/g;
        const conclusionMatches = content.match(conclusionPattern);
        if (conclusionMatches && conclusionMatches.length > 0) {
          for (const match of conclusionMatches.slice(0, 2)) {
            this.sessionMemory.addImportantContext(
              `关键结论: ${match.replace(/[:：]/g, ': ')}`
            );
          }
        }
      }
    } catch (error) {
      logger.error('上下文.记忆.转存.失败', error, LogCategory.SESSION);
    }
  }

  /**
   * 检测内容是否包含技术决策
   */
  private containsTechnicalDecision(content: string): boolean {
    const decisionKeywords = [
      '使用', '选择', '采用', '决定',
      'use', 'choose', 'adopt', 'decide',
      'React', 'TypeScript', 'Vue', 'Angular', 'Node',
      '数据库', 'PostgreSQL', 'MySQL', 'MongoDB',
      '架构', 'MVC', 'MVVM', 'Microservices'
    ];

    const lowerContent = content.toLowerCase();
    return decisionKeywords.some(keyword =>
      lowerContent.includes(keyword.toLowerCase())
    );
  }

  /**
   * 提取决策摘要
   */
  private extractDecisionSummary(content: string): string {
    // 简单实现:提取包含决策关键词的句子
    const sentences = content.split(/[。.!！\n]/);
    for (const sentence of sentences) {
      if (this.containsTechnicalDecision(sentence)) {
        return sentence.trim().substring(0, 200);
      }
    }
    return content.substring(0, 200);
  }

  /**
   * 映射操作类型
   */
  private mapActionType(action: string): 'add' | 'modify' | 'delete' {
    if (action.includes('创建') || action.includes('新增')) return 'add';
    if (action.includes('删除') || action.includes('移除')) return 'delete';
    return 'modify';
  }
}
