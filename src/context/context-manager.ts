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

import * as path from 'path';
import { MemoryDocument } from './memory-document';
import { TruncationUtils } from './truncation-utils';
import {
  ContextMessage,
  ContextManagerConfig,
  DEFAULT_CONTEXT_CONFIG,
  MemoryContent
} from './types';

export class ContextManager {
  private config: ContextManagerConfig;
  private immediateContext: ContextMessage[] = [];
  private sessionMemory: MemoryDocument | null = null;
  private initialized: boolean = false;
  private truncationUtils: TruncationUtils;

  constructor(
    private workspacePath: string,
    config: Partial<ContextManagerConfig> = {}
  ) {
    this.config = { ...DEFAULT_CONTEXT_CONFIG, ...config };
    this.truncationUtils = new TruncationUtils(this.config.compression.truncation);
  }

  /**
   * 初始化上下文管理器
   */
  async initialize(sessionId: string, sessionName: string): Promise<void> {
    const storagePath = path.join(this.workspacePath, this.config.storagePath);
    this.sessionMemory = new MemoryDocument(sessionId, sessionName, storagePath);
    await this.sessionMemory.load();
    this.initialized = true;
    console.log(`[ContextManager] 已初始化，会话: ${sessionId}`);
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
        console.log(`[ContextManager] 消息已截断: ${truncated.originalLength} -> ${truncated.truncatedLength} 字符`);
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
      // 移除最旧的消息，但保留 system 消息
      const systemMessages = this.immediateContext.filter(m => m.role === 'system');
      const otherMessages = this.immediateContext.filter(m => m.role !== 'system');
      this.immediateContext = [
        ...systemMessages,
        ...otherMessages.slice(-maxMessages)
      ];
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
    const parts: string[] = [];
    let currentTokens = 0;

    // 1. 添加会话 Memory 摘要（高优先级）
    if (this.sessionMemory) {
      const memorySummary = this.getMemorySummary();
      const memoryTokens = this.estimateTokens(memorySummary);
      if (currentTokens + memoryTokens < maxTokens * 0.3) { // Memory 最多占 30%
        parts.push('## 会话上下文\n' + memorySummary);
        currentTokens += memoryTokens;
      }
    }

    // 2. 添加即时上下文（最近对话）
    const remainingTokens = maxTokens - currentTokens;
    const recentMessages = this.getRecentMessages(remainingTokens);
    if (recentMessages.length > 0) {
      parts.push('## 最近对话\n' + recentMessages.map(m => 
        `[${m.role}]: ${m.content}`
      ).join('\n\n'));
    }

    return parts.join('\n\n---\n\n');
  }

  /**
   * 获取 Memory 摘要
   */
  private getMemorySummary(): string {
    if (!this.sessionMemory) return '';
    
    const content = this.sessionMemory.getContent();
    const lines: string[] = [];

    // 当前任务
    if (content.currentTasks.length > 0) {
      lines.push('**当前任务:**');
      content.currentTasks.forEach(t => {
        lines.push(`- ${t.description} (${t.status})`);
      });
    }

    // 关键决策（最近3个）
    if (content.keyDecisions.length > 0) {
      lines.push('**关键决策:**');
      content.keyDecisions.slice(-3).forEach(d => {
        lines.push(`- ${d.description}`);
      });
    }

    // 重要上下文
    if (content.importantContext.length > 0) {
      lines.push('**重要上下文:**');
      content.importantContext.forEach(ctx => {
        lines.push(`- ${ctx}`);
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
      
      if (tokens + msgTokens > maxTokens) break;
      
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
}

