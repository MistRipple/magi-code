/**
 * MemoryDocument - 会话 Memory 文档管理
 * 负责 Memory 文档的读写、更新和序列化
 */

import { logger, LogCategory } from '../logging';
import * as fs from 'fs';
import * as path from 'path';
import {
  MemoryContent,
  TaskRecord,
  Decision,
  CodeChange,
  createEmptyMemoryContent
} from './types';

export class MemoryDocument {
  private filePath: string;
  private content: MemoryContent;
  private dirty: boolean = false;

  constructor(
    private sessionId: string,
    private sessionName: string,
    private storagePath: string
  ) {
    this.filePath = path.join(storagePath, sessionId, 'memory.json');
    this.content = createEmptyMemoryContent(sessionId, sessionName);
  }

  /**
   * 加载 Memory 文档
   */
  async load(): Promise<void> {
    try {
      if (fs.existsSync(this.filePath)) {
        const data = fs.readFileSync(this.filePath, 'utf-8');
        const parsed = JSON.parse(data);
        this.content = this.normalizeContent(parsed);
        logger.info('上下文记忆.加载.完成', { sessionId: this.sessionId }, LogCategory.SESSION);
      } else {
        logger.info('上下文记忆.加载.新建', { sessionId: this.sessionId }, LogCategory.SESSION);
        await this.save();
      }
    } catch (error) {
      logger.error('上下文记忆.加载.失败', error, LogCategory.SESSION);
      this.content = createEmptyMemoryContent(this.sessionId, this.sessionName);
    }
  }

  /**
   * 保存 Memory 文档
   */
  async save(): Promise<void> {
    try {
      const dir = path.dirname(this.filePath);
      if (!fs.existsSync(dir)) {
        fs.mkdirSync(dir, { recursive: true });
      }
      
      this.content = this.normalizeContent(this.content);
      this.content.lastUpdated = new Date().toISOString();
      this.content.tokenEstimate = this.estimateTokens();
      
      fs.writeFileSync(this.filePath, JSON.stringify(this.content, null, 2));
      this.dirty = false;
      logger.info('上下文记忆.保存.完成', { sessionId: this.sessionId }, LogCategory.SESSION);
    } catch (error) {
      logger.error('上下文记忆.保存.失败', error, LogCategory.SESSION);
      throw error;
    }
  }

  /**
   * 获取 Memory 内容
   */
  getContent(): MemoryContent {
    return { ...this.content };
  }

  /**
   * 添加当前任务
   */
  addCurrentTask(task: Omit<TaskRecord, 'timestamp'>): void {
    if (!task || typeof task !== 'object') {
      logger.warn('上下文记忆.任务_无效', { reason: 'task invalid' }, LogCategory.SESSION);
      return;
    }
    if (typeof task.id !== 'string' || task.id.trim().length === 0) {
      logger.warn('上下文记忆.任务_无效', { reason: 'missing id' }, LogCategory.SESSION);
      return;
    }
    if (typeof task.description !== 'string' || task.description.trim().length === 0) {
      logger.warn('上下文记忆.任务_无效', { reason: 'missing description', taskId: task.id }, LogCategory.SESSION);
      return;
    }
    this.content.currentTasks.push({
      ...task,
      timestamp: new Date().toISOString()
    });
    this.dirty = true;
  }

  /**
   * 更新任务状态
   */
  updateTaskStatus(taskId: string, status: TaskRecord['status'], result?: string): void {
    const task = this.content.currentTasks.find(t => t.id === taskId);
    if (task) {
      task.status = status;
      if (typeof result === 'string') {
        task.result = result;
      } else if (result !== undefined) {
        logger.warn('上下文记忆.任务_结果_无效', { taskId }, LogCategory.SESSION);
      }
      
      // 如果任务完成或失败，移动到已完成列表
      if (status === 'completed' || status === 'failed') {
        this.content.completedTasks.push(task);
        this.content.currentTasks = this.content.currentTasks.filter(t => t.id !== taskId);
      }
      this.dirty = true;
    }
  }

  /**
   * 添加关键决策
   */
  addDecision(decision: Omit<Decision, 'timestamp'>): void {
    this.content.keyDecisions.push({
      ...decision,
      timestamp: new Date().toISOString()
    });
    this.dirty = true;
  }

  /**
   * 添加代码变更记录
   */
  addCodeChange(change: Omit<CodeChange, 'timestamp'>): void {
    if (!change || typeof change !== 'object') {
      logger.warn('上下文记忆.变更_无效', { reason: 'change invalid' }, LogCategory.SESSION);
      return;
    }
    const file = typeof change.file === 'string' ? change.file.trim() : '';
    const summary = typeof change.summary === 'string' ? change.summary : '';
    const action = change.action;
    if (!file || !['add', 'modify', 'delete'].includes(String(action))) {
      logger.warn('上下文记忆.变更_无效', { file, action }, LogCategory.SESSION);
      return;
    }
    this.content.codeChanges.push({
      ...change,
      file,
      summary,
      timestamp: new Date().toISOString()
    });
    this.dirty = true;
  }

  /**
   * 添加重要上下文
   */
  addImportantContext(context: string): void {
    if (typeof context !== 'string') {
      logger.warn('上下文记忆.重要上下文_无效', { reason: 'not string' }, LogCategory.SESSION);
      return;
    }
    const trimmed = context.trim();
    if (!trimmed) {
      return;
    }
    if (!this.content.importantContext.includes(trimmed)) {
      this.content.importantContext.push(trimmed);
      this.dirty = true;
    }
  }

  /**
   * 添加待解决问题
   */
  addPendingIssue(issue: string): void {
    if (!this.content.pendingIssues.includes(issue)) {
      this.content.pendingIssues.push(issue);
      this.dirty = true;
    }
  }

  /**
   * 移除已解决的问题
   */
  resolvePendingIssue(issue: string): void {
    this.content.pendingIssues = this.content.pendingIssues.filter(i => i !== issue);
    this.dirty = true;
  }

  /**
   * 规范化 Memory 内容（结构校验 + 非法数据剔除）
   */
  private normalizeContent(raw: unknown): MemoryContent {
    const base = createEmptyMemoryContent(this.sessionId, this.sessionName);
    const now = new Date().toISOString();
    if (!raw || typeof raw !== 'object') {
      return base;
    }
    const source = raw as Record<string, unknown>;

    const result: MemoryContent = {
      ...base,
      created: typeof source.created === 'string' && source.created ? source.created : base.created,
      lastUpdated: typeof source.lastUpdated === 'string' && source.lastUpdated ? source.lastUpdated : base.lastUpdated,
      tokenEstimate: typeof source.tokenEstimate === 'number' && Number.isFinite(source.tokenEstimate)
        ? source.tokenEstimate
        : base.tokenEstimate,
    };

    let dropped = 0;
    const safeTasks = Array.isArray(source.currentTasks) ? source.currentTasks : [];
    result.currentTasks = safeTasks
      .filter((t) => t && typeof t === 'object')
      .map((t) => {
        const task = t as Record<string, unknown>;
        const id = typeof task.id === 'string' ? task.id.trim() : '';
        const description = typeof task.description === 'string' ? task.description.trim() : '';
        const status = task.status;
        if (!id || !description || !['pending', 'in_progress', 'completed', 'failed'].includes(String(status))) {
          dropped += 1;
          return null;
        }
        return {
          id,
          description,
          status: status as TaskRecord['status'],
          assignedWorker: typeof task.assignedWorker === 'string' ? task.assignedWorker : undefined,
          result: typeof task.result === 'string' ? task.result : undefined,
          timestamp: typeof task.timestamp === 'string' ? task.timestamp : now,
        } as TaskRecord;
      })
      .filter(Boolean) as TaskRecord[];

    const safeCompleted = Array.isArray(source.completedTasks) ? source.completedTasks : [];
    result.completedTasks = safeCompleted
      .filter((t) => t && typeof t === 'object')
      .map((t) => {
        const task = t as Record<string, unknown>;
        const id = typeof task.id === 'string' ? task.id.trim() : '';
        const description = typeof task.description === 'string' ? task.description.trim() : '';
        const status = task.status;
        if (!id || !description || !['pending', 'in_progress', 'completed', 'failed'].includes(String(status))) {
          dropped += 1;
          return null;
        }
        return {
          id,
          description,
          status: status as TaskRecord['status'],
          assignedWorker: typeof task.assignedWorker === 'string' ? task.assignedWorker : undefined,
          result: typeof task.result === 'string' ? task.result : undefined,
          timestamp: typeof task.timestamp === 'string' ? task.timestamp : now,
        } as TaskRecord;
      })
      .filter(Boolean) as TaskRecord[];

    const safeDecisions = Array.isArray(source.keyDecisions) ? source.keyDecisions : [];
    result.keyDecisions = safeDecisions
      .filter((d) => d && typeof d === 'object')
      .map((d) => {
        const decision = d as Record<string, unknown>;
        const id = typeof decision.id === 'string' ? decision.id.trim() : '';
        const description = typeof decision.description === 'string' ? decision.description.trim() : '';
        const reason = typeof decision.reason === 'string' ? decision.reason.trim() : '';
        if (!id || !description || !reason) {
          dropped += 1;
          return null;
        }
        return {
          id,
          description,
          reason,
          timestamp: typeof decision.timestamp === 'string' ? decision.timestamp : now,
        } as Decision;
      })
      .filter(Boolean) as Decision[];

    const safeChanges = Array.isArray(source.codeChanges) ? source.codeChanges : [];
    result.codeChanges = safeChanges
      .filter((c) => c && typeof c === 'object')
      .map((c) => {
        const change = c as Record<string, unknown>;
        const file = typeof change.file === 'string' ? change.file.trim() : '';
        const action = change.action;
        const summary = typeof change.summary === 'string' ? change.summary : '';
        if (!file || !['add', 'modify', 'delete'].includes(String(action))) {
          dropped += 1;
          return null;
        }
        return {
          file,
          action: action as CodeChange['action'],
          summary,
          timestamp: typeof change.timestamp === 'string' ? change.timestamp : now,
        } as CodeChange;
      })
      .filter(Boolean) as CodeChange[];

    const safeContext = Array.isArray(source.importantContext) ? source.importantContext : [];
    result.importantContext = safeContext
      .filter((ctx) => typeof ctx === 'string')
      .map((ctx) => ctx.trim())
      .filter(Boolean);

    const safeIssues = Array.isArray(source.pendingIssues) ? source.pendingIssues : [];
    result.pendingIssues = safeIssues
      .filter((issue) => typeof issue === 'string')
      .map((issue) => issue.trim())
      .filter(Boolean);

    if (dropped > 0) {
      logger.warn('上下文记忆.规范化.丢弃无效记录', { dropped }, LogCategory.SESSION);
    }
    return result;
  }

  /**
   * 估算 Token 数量（简单估算：字符数 / 4）
   */
  estimateTokens(): number {
    const jsonStr = JSON.stringify(this.content);
    return Math.ceil(jsonStr.length / 4);
  }

  /**
   * 检查是否需要压缩
   */
  needsCompression(tokenLimit: number = 8000, lineLimit: number = 200): boolean {
    const tokens = this.estimateTokens();
    const lines = this.toMarkdown().split('\n').length;
    return tokens > tokenLimit || lines > lineLimit;
  }

  /**
   * 获取是否有未保存的更改
   */
  isDirty(): boolean {
    return this.dirty;
  }

  /**
   * 转换为 Markdown 格式（用于展示和压缩）
   */
  toMarkdown(): string {
    const c = this.content;
    const lines: string[] = [
      `# Session Memory: ${c.sessionName}`,
      `Created: ${c.created}`,
      `Last Updated: ${c.lastUpdated}`,
      `Token Estimate: ~${c.tokenEstimate}`,
      ''
    ];

    // 当前任务
    if (c.currentTasks.length > 0) {
      lines.push('## 当前任务');
      c.currentTasks.forEach(t => {
        const status = t.status === 'in_progress' ? '[/]' : '[ ]';
        lines.push(`- ${status} ${t.description}${t.assignedWorker ? ` (${t.assignedWorker})` : ''}`);
      });
      lines.push('');
    }

    // 已完成任务
    if (c.completedTasks.length > 0) {
      lines.push('## 已完成任务');
      c.completedTasks.slice(-10).forEach(t => { // 只显示最近10个
        const status = t.status === 'completed' ? '[x]' : '[!]';
        lines.push(`- ${status} ${t.description}${t.result ? ` - ${t.result}` : ''}`);
      });
      lines.push('');
    }

    // 关键决策
    if (c.keyDecisions.length > 0) {
      lines.push('## 关键决策');
      c.keyDecisions.forEach((d, i) => {
        lines.push(`${i + 1}. ${d.description}: ${d.reason}`);
      });
      lines.push('');
    }

    // 代码变更
    if (c.codeChanges.length > 0) {
      lines.push('## 代码变更摘要');
      c.codeChanges.slice(-20).forEach(ch => { // 只显示最近20个
        lines.push(`- \`${ch.file}\`: ${ch.summary}`);
      });
      lines.push('');
    }

    // 重要上下文
    if (c.importantContext.length > 0) {
      lines.push('## 重要上下文');
      c.importantContext.forEach(ctx => {
        lines.push(`- ${ctx}`);
      });
      lines.push('');
    }

    // 待解决问题
    if (c.pendingIssues.length > 0) {
      lines.push('## 📌 待解决问题');
      c.pendingIssues.forEach(issue => {
        lines.push(`- ${issue}`);
      });
      lines.push('');
    }

    return lines.join('\n');
  }

  /**
   * 用压缩后的内容替换当前内容
   */
  replaceContent(newContent: Partial<MemoryContent>): void {
    this.content = {
      ...this.content,
      ...newContent,
      lastUpdated: new Date().toISOString()
    };
    this.content.tokenEstimate = this.estimateTokens();
    this.dirty = true;
  }

  /**
   * 清理旧数据（保留最近的记录）
   */
  pruneOldData(keepCompletedTasks: number = 5, keepCodeChanges: number = 10): void {
    if (this.content.completedTasks.length > keepCompletedTasks) {
      this.content.completedTasks = this.content.completedTasks.slice(-keepCompletedTasks);
    }
    if (this.content.codeChanges.length > keepCodeChanges) {
      this.content.codeChanges = this.content.codeChanges.slice(-keepCodeChanges);
    }
    this.dirty = true;
  }
}
