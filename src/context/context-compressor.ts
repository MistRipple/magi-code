/**
 * ContextCompressor - 智能上下文压缩器
 * 混合策略：Augment 风格截断 + LLM 智能压缩
 *
 * 策略优先级：
 * 1. 预防性截断（Augment 风格）- 从源头控制大小
 * 2. LLM 智能压缩 - 保留语义信息
 * 3. 简单压缩（备用方案）- 基于重要性评分
 */

import { logger, LogCategory } from '../logging';
import { MemoryDocument } from './memory-document';
import { CompressionConfig, MemoryContent, DEFAULT_TRUNCATION_CONFIG } from './types';
import { TruncationUtils, TruncationResult } from './truncation-utils';
import { extractEmbeddedJson } from '../utils/content-parser';

// 压缩提示词（升级版：对齐 Claude Code 压缩格式）
const COMPRESSION_PROMPT = `你是一个专业的上下文压缩助手。请对以下会话 Memory 进行压缩，保留关键信息。

## 压缩原则（按优先级排序）

### 🔴 必须保留（不可压缩）
1. **用户核心意图**：primaryIntent 必须完整保留，这是会话的核心目标
2. **用户约束条件**：userConstraints 中的所有约束必须保留
3. **当前任务**：所有进行中的任务必须完整保留
4. **当前工作状态**：currentWork 描述最后在做什么

### 🟡 重要保留（谨慎压缩）
5. **关键决策**：重要的技术决策和原因必须保留
6. **用户原话**：userMessages 中标记为 isKeyInstruction=true 的消息保留原文
7. **下一步建议**：nextSteps 保留，便于后续 session 继续
8. **被拒绝方案**：rejectedApproaches 保留，避免重复提出已否决的方案

### 🟢 可压缩
9. **已完成任务**：只保留任务名称和简要结果
10. **代码变更**：相同文件的多次变更合并为一条
11. **已解决问题**：压缩为问题+方案的简要描述
12. **重要上下文**：移除冗余信息，保留核心要点

## 输入 Memory
{MEMORY_CONTENT}

## 输出格式
请以 JSON 格式输出压缩后的内容，保持完整结构：
\`\`\`json
{
  "sessionId": "...",
  "sessionName": "...",
  "created": "...",
  "lastUpdated": "...",
  "tokenEstimate": 0,
  "primaryIntent": "...",           // 🔴 必须保留
  "userConstraints": [...],         // 🔴 必须保留
  "userMessages": [...],            // 保留关键指令
  "currentTasks": [...],            // 🔴 必须保留
  "completedTasks": [...],          // 压缩后的已完成任务
  "currentWork": "...",             // 🔴 必须保留
  "nextSteps": [...],               // 保留下一步建议
  "keyDecisions": [...],            // 保留关键决策
  "codeChanges": [...],             // 合并后的代码变更
  "importantContext": [...],        // 精简后的上下文
  "pendingIssues": [...],           // 待解决问题
  "resolvedIssues": [...],          // 压缩后的已解决问题
  "rejectedApproaches": [...]       // 保留被拒绝的方案
}
\`\`\``;

export interface CompressorAdapter {
  sendMessage(message: string): Promise<string>;
}

/**
 * 压缩统计信息
 */
export interface CompressionStats {
  originalTokens: number;
  compressedTokens: number;
  compressionRatio: number;
  method: 'truncation' | 'llm' | 'simple' | 'aggressive';
  truncationApplied: boolean;
}

export class ContextCompressor {
  private truncationUtils: TruncationUtils;
  private lastStats: CompressionStats | null = null;

  constructor(
    private adapter: CompressorAdapter | null = null,
    private config: CompressionConfig = {
      tokenLimit: 8000,
      lineLimit: 200,
      compressionRatio: 0.5,
      retentionPriority: ['currentTasks', 'keyDecisions', 'importantContext', 'codeChanges', 'completedTasks', 'pendingIssues'],
      truncation: DEFAULT_TRUNCATION_CONFIG
    }
  ) {
    this.truncationUtils = new TruncationUtils(config.truncation);
  }

  /**
   * 设置 LLM 适配器
   */
  setAdapter(adapter: CompressorAdapter): void {
    this.adapter = adapter;
  }

  /**
   * 获取最后一次压缩的统计信息
   */
  getLastStats(): CompressionStats | null {
    return this.lastStats;
  }

  /**
   * 截断单条消息（Augment 风格，不使用 LLM）
   */
  truncateMessage(content: string, maxChars?: number): TruncationResult {
    return this.truncationUtils.truncateMessage(content, maxChars);
  }

  /**
   * 截断工具输出
   */
  truncateToolOutput(output: string): TruncationResult {
    return this.truncationUtils.truncateToolOutput(output);
  }

  /**
   * 截断代码块
   */
  truncateCodeBlock(code: string, maxLines?: number): TruncationResult {
    return this.truncationUtils.truncateCodeBlock(code, maxLines);
  }

  /**
   * 压缩 Memory 文档
   * 混合策略：预防性截断 + LLM 智能压缩 + 简单压缩（备用方案）
   */
  async compress(memory: MemoryDocument): Promise<boolean> {
    const content = memory.getContent();
    const originalTokens = memory.estimateTokens();
    let method: CompressionStats['method'] = 'truncation';
    let truncationApplied = false;

    // 第一步：预防性截断（Augment 风格）
    // 对长文本字段进行截断，快速减少大小
    const { content: truncatedContent, changed: truncationChanged } = this.applyPreventiveTruncation(content);
    if (truncationChanged) {
      memory.replaceContent(truncatedContent);
      truncationApplied = true;
      logger.info('上下文压缩.预防_截断.已应用', undefined, LogCategory.SESSION);

      // 检查截断后是否还需要进一步压缩
      if (!memory.needsCompression(this.config.tokenLimit, this.config.lineLimit)) {
        this.updateStats(originalTokens, memory.estimateTokens(), 'truncation', true);
        return true;
      }
    }

    // 第二步：LLM 智能压缩（保留语义）
    if (this.adapter) {
      logger.info('上下文压缩.LLM.开始', undefined, LogCategory.SESSION);
      const success = await this.llmCompress(memory);
      if (success) {
        this.updateStats(originalTokens, memory.estimateTokens(), 'llm', truncationApplied);
        return true;
      }
      logger.info('上下文压缩.LLM.切换_为_简单', undefined, LogCategory.SESSION);
    }

    // 第三步：简单压缩（备用方案）
    logger.info('上下文压缩.简单.开始', undefined, LogCategory.SESSION);
    if (this.trySimpleCompression(memory)) {
      this.updateStats(originalTokens, memory.estimateTokens(), 'simple', truncationApplied);
      logger.info('上下文压缩.简单.完成', undefined, LogCategory.SESSION);
      return true;
    }

    // 第四步：激进压缩（最后手段）
    this.aggressiveSimpleCompression(memory);
    this.updateStats(originalTokens, memory.estimateTokens(), 'aggressive', truncationApplied);
    logger.info('上下文压缩.简单.激进.完成', undefined, LogCategory.SESSION);
    return true;
  }

  /**
   * 更新压缩统计
   */
  private updateStats(
    originalTokens: number,
    compressedTokens: number,
    method: CompressionStats['method'],
    truncationApplied: boolean
  ): void {
    this.lastStats = {
      originalTokens,
      compressedTokens,
      compressionRatio: originalTokens > 0 ? compressedTokens / originalTokens : 1,
      method,
      truncationApplied
    };
  }

  /**
   * 预防性截断（Augment 风格）
   * 对长文本字段进行截断，不使用 LLM
   */
  private applyPreventiveTruncation(content: MemoryContent): { content: MemoryContent; changed: boolean } {
    const result: MemoryContent = { ...content };
    const maxContextLength = 500;  // 单条上下文最大长度
    const maxResultLength = 200;   // 任务结果最大长度
    let changed = false;

    // 截断重要上下文中的长文本
    const safeImportant = Array.isArray(result.importantContext) ? result.importantContext : [];
    result.importantContext = safeImportant
      .filter((ctx) => typeof ctx === 'string')
      .map(ctx => {
        if (ctx.length > maxContextLength) {
          changed = true;
          return ctx.substring(0, maxContextLength) + '...';
        }
        return ctx;
      });

    // 截断任务结果
    const safeCompleted = Array.isArray(result.completedTasks) ? result.completedTasks : [];
    result.completedTasks = safeCompleted
      .filter((task) => task && typeof task === 'object')
      .map(task => ({
        ...task,
        result: typeof task.result === 'string' && task.result.length > maxResultLength
          ? task.result.substring(0, maxResultLength) + '...'
          : task.result
      }));
    if (!changed) {
      changed = safeCompleted.some(task => typeof task?.result === 'string' && task.result.length > maxResultLength);
    }

    // 截断代码变更摘要
    const safeChanges = Array.isArray(result.codeChanges) ? result.codeChanges : [];
    result.codeChanges = safeChanges
      .filter((change) => change && typeof change === 'object')
      .map(change => {
        const summary = typeof change.summary === 'string' ? change.summary : '';
        return {
          ...change,
          summary: summary.length > maxContextLength
            ? summary.substring(0, maxContextLength) + '...'
            : summary
        };
      });
    if (!changed) {
      changed = safeChanges.some(change => typeof change?.summary === 'string' && change.summary.length > maxContextLength);
    }

    return { content: result, changed };
  }

  /**
   * 简单压缩（不需要 LLM）- 基于重要性评分
   * 优先保留重要信息，而不是简单的时间截断
   */
  private trySimpleCompression(memory: MemoryDocument): boolean {
    const content = JSON.parse(JSON.stringify(memory.getContent())) as MemoryContent;
    let compressed = false;

    // 1. 对已完成任务进行重要性评分，保留高分任务
    if (content.completedTasks.length > 5) {
      const scoredTasks = content.completedTasks.map(task => ({
        task,
        score: this.scoreTaskImportance(task)
      }));
      // 按分数排序，保留最重要的 5 个
      scoredTasks.sort((a, b) => b.score - a.score);
      content.completedTasks = scoredTasks.slice(0, 5).map(s => s.task);
      compressed = true;
    }

    // 2. 合并相同文件的代码变更（保留语义）
    const mergedChanges = this.mergeCodeChanges(content.codeChanges);
    if (mergedChanges.length < content.codeChanges.length) {
      content.codeChanges = mergedChanges;
      compressed = true;
    }

    // 3. 对代码变更进行重要性评分，保留高分变更
    if (content.codeChanges.length > 10) {
      const scoredChanges = content.codeChanges.map(change => ({
        change,
        score: this.scoreChangeImportance(change)
      }));
      scoredChanges.sort((a, b) => b.score - a.score);
      content.codeChanges = scoredChanges.slice(0, 10).map(s => s.change);
      compressed = true;
    }

    // 4. 按保留优先级进行裁剪（配置驱动）
    if (this.applyPriorityTrimming(content)) {
      compressed = true;
    }

    if (compressed) {
      memory.replaceContent(content);
    }

    // 检查是否还需要进一步压缩
    return !memory.needsCompression(this.config.tokenLimit, this.config.lineLimit);
  }

  private applyPriorityTrimming(content: MemoryContent): boolean {
    const priorities = this.config.retentionPriority;
    if (!Array.isArray(priorities) || priorities.length === 0) {
      throw new Error('compression.retentionPriority must be a non-empty array');
    }

    // 定义各字段的压缩上限（按重要性设置）
    const arrayCaps: Partial<Record<keyof MemoryContent, number>> = {
      // 🔴 核心字段：不压缩或保留大部分
      primaryIntent: Number.MAX_SAFE_INTEGER,      // string，不压缩
      userConstraints: Number.MAX_SAFE_INTEGER,    // 全部保留
      currentTasks: Number.MAX_SAFE_INTEGER,       // 全部保留
      currentWork: Number.MAX_SAFE_INTEGER,        // string，不压缩

      // 🟡 重要字段：适度压缩
      keyDecisions: 5,
      userMessages: 10,
      nextSteps: 5,
      rejectedApproaches: 5,

      // 🟢 可压缩字段
      importantContext: 5,
      codeChanges: 10,
      pendingIssues: 5,
      resolvedIssues: 5,
      completedTasks: 5
    };

    let changed = false;
    // 从优先级最低的开始压缩
    for (const key of [...priorities].reverse()) {
      const cap = arrayCaps[key as keyof typeof arrayCaps];
      if (cap === undefined || cap === Number.MAX_SAFE_INTEGER) {
        continue; // 跳过不需要压缩的字段或非数组字段
      }

      const value = content[key as keyof MemoryContent];
      if (!Array.isArray(value)) {
        continue; // 跳过非数组字段（如 primaryIntent, currentWork）
      }

      if (value.length > cap) {
        // 特殊处理 userMessages：优先保留关键指令
        if (key === 'userMessages') {
          const messages = value as any[];
          const keyMsgs = messages.filter((m: any) => m.isKeyInstruction);
          const regularMsgs = messages.filter((m: any) => !m.isKeyInstruction);
          const keepRegular = Math.max(0, cap - keyMsgs.length);
          (content as any)[key] = [...keyMsgs, ...regularMsgs.slice(-keepRegular)];
        } else {
          (content as any)[key] = value.slice(-cap);
        }
        changed = true;
      }
    }
    return changed;
  }

  /**
   * 评估任务重要性（0-100分）
   */
  private scoreTaskImportance(task: MemoryContent['completedTasks'][0]): number {
    let score = 50; // 基础分

    const desc = task.description.toLowerCase();

    // 关键词加分
    const highPriorityKeywords = ['架构', 'architecture', '重构', 'refactor', '核心', 'core', '关键', 'critical', '安全', 'security'];
    const mediumPriorityKeywords = ['修复', 'fix', '实现', 'implement', '添加', 'add', '功能', 'feature'];
    const lowPriorityKeywords = ['格式', 'format', '注释', 'comment', '清理', 'cleanup', '微调', 'tweak'];

    highPriorityKeywords.forEach(kw => {
      if (desc.includes(kw)) score += 20;
    });
    mediumPriorityKeywords.forEach(kw => {
      if (desc.includes(kw)) score += 10;
    });
    lowPriorityKeywords.forEach(kw => {
      if (desc.includes(kw)) score -= 10;
    });

    // 有结果的任务更重要
    if (task.result && task.result.length > 20) score += 10;

    // 失败的任务需要保留（可能需要后续处理）
    if (task.status === 'failed') score += 15;

    return Math.max(0, Math.min(100, score));
  }

  /**
   * 评估代码变更重要性（0-100分）
   */
  private scoreChangeImportance(change: MemoryContent['codeChanges'][0]): number {
    let score = 50;

    const file = change.file.toLowerCase();
    const summary = change.summary.toLowerCase();

    // 核心文件加分
    const coreFiles = ['index', 'main', 'app', 'core', 'config', 'types'];
    coreFiles.forEach(cf => {
      if (file.includes(cf)) score += 15;
    });

    // 新增文件比修改更重要
    if (change.action === 'add') score += 10;
    if (change.action === 'delete') score += 5; // 删除也需要记录

    // 关键变更加分
    const importantChanges = ['架构', '接口', 'api', 'interface', '类型', 'type', '配置', 'config'];
    importantChanges.forEach(ic => {
      if (summary.includes(ic)) score += 10;
    });

    return Math.max(0, Math.min(100, score));
  }

  /**
   * 合并相同文件的代码变更
   */
  private mergeCodeChanges(changes: MemoryContent['codeChanges']): MemoryContent['codeChanges'] {
    const maxSummaryLength = 500;
    const fileMap = new Map<string, MemoryContent['codeChanges'][0]>();

    changes.forEach(change => {
      const existing = fileMap.get(change.file);
      if (existing) {
        // 合并摘要
        const merged = `${existing.summary}; ${change.summary}`;
        existing.summary = merged.length > maxSummaryLength ? merged.substring(0, maxSummaryLength) + '...' : merged;
        existing.timestamp = change.timestamp; // 使用最新时间
      } else {
        fileMap.set(change.file, { ...change });
      }
    });

    return Array.from(fileMap.values());
  }

  /**
   * 激进的简单压缩（不需要 LLM）
   * 升级版：支持所有新字段
   */
  private aggressiveSimpleCompression(memory: MemoryDocument): void {
    const content = memory.getContent();

    // ========== 核心字段：保留 ==========
    // primaryIntent: 不压缩
    // userConstraints: 不压缩
    // currentTasks: 不压缩
    // currentWork: 不压缩

    // ========== 重要字段：适度压缩 ==========
    // 1. 用户消息：只保留关键指令 + 最近2条普通消息
    const keyMessages = content.userMessages.filter(m => m.isKeyInstruction);
    const regularMessages = content.userMessages.filter(m => !m.isKeyInstruction);
    content.userMessages = [...keyMessages, ...regularMessages.slice(-2)];

    // 2. 下一步建议：只保留最近3个
    content.nextSteps = content.nextSteps.slice(-3);

    // 3. 被拒绝的方案：只保留最近3个
    content.rejectedApproaches = content.rejectedApproaches.slice(-3);

    // 4. 关键决策：只保留最近3个
    content.keyDecisions = content.keyDecisions.slice(-3);

    // ========== 可压缩字段 ==========
    // 5. 只保留最近2个已完成任务，并截断结果
    content.completedTasks = content.completedTasks.slice(-2).map(t => ({
      ...t,
      result: t.result ? t.result.substring(0, 50) + '...' : undefined
    }));

    // 6. 只保留最近5个代码变更（合并后）
    content.codeChanges = this.mergeCodeChanges(content.codeChanges).slice(-5);

    // 7. 限制重要上下文数量
    content.importantContext = content.importantContext.slice(-3);

    // 8. 限制待解决问题数量
    content.pendingIssues = content.pendingIssues.slice(-3);

    // 9. 限制已解决问题数量
    content.resolvedIssues = content.resolvedIssues.slice(-2);

    memory.replaceContent(content);
  }

  /**
   * 使用 LLM 进行智能压缩
   */
  private async llmCompress(memory: MemoryDocument): Promise<boolean> {
    if (!this.adapter) return false;

    try {
      const markdown = memory.toMarkdown();
      const prompt = COMPRESSION_PROMPT.replace('{MEMORY_CONTENT}', markdown);

      logger.info('上下文压缩.LLM.开始', undefined, LogCategory.SESSION);
      const response = await this.adapter.sendMessage(prompt);

      const candidates = this.extractCompressionCandidates(response);
      if (candidates.length === 0) {
        logger.error('上下文压缩.LLM.解析_失败', { reason: '未提取到 JSON 候选' }, LogCategory.SESSION);
        return false;
      }

      let lastValidationError = '无可用候选';
      for (const candidate of candidates) {
        const validated = this.validateMemoryContent(candidate);
        if (validated.valid) {
          memory.replaceContent(validated.content);
          logger.info('上下文压缩.LLM.完成', {
            candidates: candidates.length,
          }, LogCategory.SESSION);
          return true;
        }
        lastValidationError = validated.error;
      }

      logger.error('上下文压缩.LLM.解析_失败', {
        reason: lastValidationError,
        candidates: candidates.length,
      }, LogCategory.SESSION);
      return false;
    } catch (error) {
      logger.error('上下文压缩.LLM.失败', error, LogCategory.SESSION);
      return false;
    }
  }

  /**
   * 从 LLM 响应中提取压缩结果候选 JSON
   *
   * 提取顺序：
   * 1. fenced code block（```json ... ``` 或 ``` ... ```）
   * 2. 混合文本中的嵌入 JSON
   * 3. 纯 JSON 响应
   */
  private extractCompressionCandidates(response: string): any[] {
    const candidates: any[] = [];
    const rawJsonTexts: string[] = [];

    const fencedRegex = /```(?:json)?\s*([\s\S]*?)\s*```/gi;
    let match: RegExpExecArray | null = null;
    while ((match = fencedRegex.exec(response)) !== null) {
      const jsonText = (match[1] || '').trim();
      if (jsonText) {
        rawJsonTexts.push(jsonText);
      }
    }

    const embeddedJson = extractEmbeddedJson(response);
    for (const item of embeddedJson) {
      const jsonText = item.jsonText.trim();
      if (jsonText) {
        rawJsonTexts.push(jsonText);
      }
    }

    const trimmed = response.trim();
    if (trimmed.startsWith('{') || trimmed.startsWith('[')) {
      rawJsonTexts.push(trimmed);
    }

    const uniqueTexts = Array.from(new Set(rawJsonTexts));
    for (const jsonText of uniqueTexts) {
      try {
        const parsed = JSON.parse(jsonText);
        if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) {
          candidates.push(parsed);
        }
      } catch {
        // 忽略非法 JSON 片段，继续尝试其他候选
      }
    }

    return candidates;
  }

  private validateMemoryContent(input: any): { valid: true; content: MemoryContent } | { valid: false; error: string } {
    if (!input || typeof input !== 'object') {
      return { valid: false, error: 'content is not object' };
    }

    // ========== 元数据验证 ==========
    const requiredStringFields = ['sessionId', 'sessionName', 'created', 'lastUpdated'] as const;
    for (const field of requiredStringFields) {
      if (typeof input[field] !== 'string' || !input[field].trim()) {
        return { valid: false, error: `missing or invalid ${field}` };
      }
    }
    if (typeof input.tokenEstimate !== 'number' || !Number.isFinite(input.tokenEstimate)) {
      return { valid: false, error: 'tokenEstimate invalid' };
    }

    // ========== 可选字符串字段验证（允许空字符串）==========
    const optionalStringFields = ['primaryIntent', 'currentWork'] as const;
    for (const field of optionalStringFields) {
      if (input[field] !== undefined && typeof input[field] !== 'string') {
        return { valid: false, error: `${field} must be string or undefined` };
      }
    }

    // ========== 数组字段归一化 ==========
    const arrayFields = [
      'currentTasks', 'completedTasks', 'keyDecisions', 'codeChanges',
      'importantContext', 'pendingIssues', 'userConstraints', 'userMessages',
      'nextSteps', 'resolvedIssues', 'rejectedApproaches'
    ] as const;
    for (const field of arrayFields) {
      if (!Array.isArray(input[field])) {
        // 允许缺失的数组字段，自动初始化为空数组
        input[field] = [];
      }
    }

    const now = new Date().toISOString();
    const toNonEmptyString = (value: unknown): string | undefined => {
      if (typeof value !== 'string') {
        return undefined;
      }
      const trimmed = value.trim();
      return trimmed ? trimmed : undefined;
    };
    const autoId = (prefix: string, index: number): string =>
      `${prefix}_${Date.now().toString(36)}_${index.toString(36)}`;

    const normalizeTaskStatus = (
      status: unknown,
      defaultStatus: 'pending' | 'in_progress' | 'completed' | 'failed'
    ): 'pending' | 'in_progress' | 'completed' | 'failed' => {
      if (typeof status !== 'string') {
        return defaultStatus;
      }
      const normalized = status.toLowerCase().replace(/[\s-]/g, '_');
      switch (normalized) {
        case 'pending':
        case 'todo':
          return 'pending';
        case 'in_progress':
        case 'doing':
        case 'running':
          return 'in_progress';
        case 'completed':
        case 'done':
        case 'success':
          return 'completed';
        case 'failed':
        case 'failure':
        case 'error':
          return 'failed';
        default:
          return defaultStatus;
      }
    };

    const normalizeTaskRecord = (
      task: unknown,
      index: number,
      defaultTaskStatus: 'pending' | 'in_progress' | 'completed' | 'failed'
    ): any | null => {
      if (typeof task === 'string') {
        const description = toNonEmptyString(task);
        if (!description) {
          return null;
        }
        return {
          id: autoId('task', index),
          description,
          status: defaultTaskStatus,
          timestamp: now,
        };
      }
      if (!task || typeof task !== 'object') {
        return null;
      }

      const record = task as Record<string, unknown>;
      const description = toNonEmptyString(record.description)
        || toNonEmptyString(record.title)
        || toNonEmptyString(record.task)
        || toNonEmptyString(record.name);
      if (!description) {
        return null;
      }

      return {
        id: toNonEmptyString(record.id) || autoId('task', index),
        description,
        status: normalizeTaskStatus(record.status, defaultTaskStatus),
        assignedWorker: toNonEmptyString(record.assignedWorker) || toNonEmptyString(record.worker),
        result: toNonEmptyString(record.result) || toNonEmptyString(record.summary),
        timestamp: toNonEmptyString(record.timestamp) || now,
      };
    };

    const normalizeTasks = (
      tasks: unknown[],
      defaultTaskStatus: 'pending' | 'in_progress' | 'completed' | 'failed'
    ): any[] => tasks
      .map((task, index) => normalizeTaskRecord(task, index, defaultTaskStatus))
      .filter((task): task is Record<string, unknown> => Boolean(task));

    input.currentTasks = normalizeTasks(input.currentTasks as unknown[], 'pending');
    input.completedTasks = normalizeTasks(input.completedTasks as unknown[], 'completed');

    input.keyDecisions = (input.keyDecisions as unknown[])
      .map((decision, index) => {
        if (typeof decision === 'string') {
          const description = toNonEmptyString(decision);
          if (!description) {
            return null;
          }
          return {
            id: autoId('decision', index),
            description,
            reason: '压缩保留',
            timestamp: now,
          };
        }
        if (!decision || typeof decision !== 'object') {
          return null;
        }
        const value = decision as Record<string, unknown>;
        const description = toNonEmptyString(value.description);
        if (!description) {
          return null;
        }
        return {
          id: toNonEmptyString(value.id) || autoId('decision', index),
          description,
          reason: toNonEmptyString(value.reason) || '压缩保留',
          timestamp: toNonEmptyString(value.timestamp) || now,
        };
      })
      .filter(Boolean);

    const normalizeCodeAction = (action: unknown): 'add' | 'modify' | 'delete' => {
      if (typeof action !== 'string') {
        return 'modify';
      }
      const normalized = action.toLowerCase();
      if (normalized.includes('add') || normalized.includes('create') || normalized.includes('new')) {
        return 'add';
      }
      if (normalized.includes('delete') || normalized.includes('remove')) {
        return 'delete';
      }
      return 'modify';
    };

    input.codeChanges = (input.codeChanges as unknown[])
      .map((change) => {
        if (!change || typeof change !== 'object') {
          return null;
        }
        const value = change as Record<string, unknown>;
        const file = toNonEmptyString(value.file) || toNonEmptyString(value.path);
        if (!file) {
          return null;
        }
        return {
          file,
          action: normalizeCodeAction(value.action),
          summary: toNonEmptyString(value.summary) || toNonEmptyString(value.description) || '',
          timestamp: toNonEmptyString(value.timestamp) || now,
        };
      })
      .filter(Boolean);

    input.importantContext = (input.importantContext as unknown[])
      .map((item) => toNonEmptyString(item))
      .filter((item): item is string => Boolean(item));

    input.pendingIssues = (input.pendingIssues as unknown[])
      .map((issue, index) => {
        if (typeof issue === 'string') {
          const description = toNonEmptyString(issue);
          if (!description) {
            return null;
          }
          return {
            id: autoId('issue', index),
            description,
            source: 'system',
            timestamp: now,
          };
        }
        if (!issue || typeof issue !== 'object') {
          return null;
        }
        const value = issue as Record<string, unknown>;
        const description = toNonEmptyString(value.description) || toNonEmptyString(value.problem);
        if (!description) {
          return null;
        }
        const source = toNonEmptyString(value.source);
        const normalizedSource = (source === 'user' || source === 'system' || source === 'ai') ? source : 'system';
        return {
          id: toNonEmptyString(value.id) || autoId('issue', index),
          description,
          source: normalizedSource,
          timestamp: toNonEmptyString(value.timestamp) || now,
        };
      })
      .filter(Boolean);

    input.userConstraints = (input.userConstraints as unknown[])
      .map((item) => toNonEmptyString(item))
      .filter((item): item is string => Boolean(item));

    input.userMessages = (input.userMessages as unknown[])
      .map((msg) => {
        if (!msg || typeof msg !== 'object') {
          return null;
        }
        const value = msg as Record<string, unknown>;
        const content = toNonEmptyString(value.content);
        if (!content) {
          return null;
        }
        return {
          content,
          timestamp: toNonEmptyString(value.timestamp) || now,
          isKeyInstruction: Boolean(value.isKeyInstruction),
        };
      })
      .filter(Boolean);

    input.nextSteps = (input.nextSteps as unknown[])
      .map((item) => toNonEmptyString(item))
      .filter((item): item is string => Boolean(item));

    input.resolvedIssues = (input.resolvedIssues as unknown[])
      .map((issue, index) => {
        if (!issue || typeof issue !== 'object') {
          return null;
        }
        const value = issue as Record<string, unknown>;
        const problem = toNonEmptyString(value.problem) || toNonEmptyString(value.description);
        if (!problem) {
          return null;
        }
        return {
          id: toNonEmptyString(value.id) || autoId('resolved', index),
          problem,
          rootCause: toNonEmptyString(value.rootCause) || 'unknown',
          solution: toNonEmptyString(value.solution) || '',
          timestamp: toNonEmptyString(value.timestamp) || now,
        };
      })
      .filter(Boolean);

    input.rejectedApproaches = (input.rejectedApproaches as unknown[])
      .map((approach, index) => {
        if (!approach || typeof approach !== 'object') {
          return null;
        }
        const value = approach as Record<string, unknown>;
        const detail = toNonEmptyString(value.approach) || toNonEmptyString(value.description);
        if (!detail) {
          return null;
        }
        const rejectedBy = toNonEmptyString(value.rejectedBy) === 'technical' ? 'technical' : 'user';
        return {
          id: toNonEmptyString(value.id) || autoId('rejected', index),
          approach: detail,
          reason: toNonEmptyString(value.reason) || '',
          rejectedBy,
          timestamp: toNonEmptyString(value.timestamp) || now,
        };
      })
      .filter(Boolean);

    // ========== 设置默认值 ==========
    if (input.primaryIntent === undefined) {
      input.primaryIntent = '';
    }
    if (input.currentWork === undefined) {
      input.currentWork = '';
    }
    input.tokenEstimate = Math.max(0, Math.floor(input.tokenEstimate));

    return { valid: true, content: input as MemoryContent };
  }

  /**
   * 摘要多条消息（用于即时上下文压缩）
   */
  async summarizeMessages(messages: Array<{ role: string; content: string }>): Promise<string> {
    if (!this.adapter) {
      // 简单摘要：取每条消息的前100字符
      return messages.map(m =>
        `[${m.role}]: ${m.content.substring(0, 100)}...`
      ).join('\n');
    }

    const prompt = `请将以下对话摘要为简洁的要点（不超过200字）：

${messages.map(m => `[${m.role}]: ${m.content}`).join('\n\n')}

摘要：`;

    try {
      return await this.adapter.sendMessage(prompt);
    } catch {
      return messages.map(m =>
        `[${m.role}]: ${m.content.substring(0, 100)}...`
      ).join('\n');
    }
  }
}
