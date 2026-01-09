/**
 * MemoryDocument - 会话 Memory 文档管理
 * 负责 Memory 文档的读写、更新和序列化
 */

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
        this.content = JSON.parse(data);
        console.log(`[MemoryDocument] 已加载会话 Memory: ${this.sessionId}`);
      } else {
        console.log(`[MemoryDocument] 创建新的会话 Memory: ${this.sessionId}`);
        await this.save();
      }
    } catch (error) {
      console.error(`[MemoryDocument] 加载失败:`, error);
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
      
      this.content.lastUpdated = new Date().toISOString();
      this.content.tokenEstimate = this.estimateTokens();
      
      fs.writeFileSync(this.filePath, JSON.stringify(this.content, null, 2));
      this.dirty = false;
      console.log(`[MemoryDocument] 已保存会话 Memory: ${this.sessionId}`);
    } catch (error) {
      console.error(`[MemoryDocument] 保存失败:`, error);
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
      if (result) task.result = result;
      
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
    this.content.codeChanges.push({
      ...change,
      timestamp: new Date().toISOString()
    });
    this.dirty = true;
  }

  /**
   * 添加重要上下文
   */
  addImportantContext(context: string): void {
    if (!this.content.importantContext.includes(context)) {
      this.content.importantContext.push(context);
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
      lines.push('## 🎯 当前任务');
      c.currentTasks.forEach(t => {
        const status = t.status === 'in_progress' ? '[/]' : '[ ]';
        lines.push(`- ${status} ${t.description}${t.assignedWorker ? ` (${t.assignedWorker})` : ''}`);
      });
      lines.push('');
    }

    // 已完成任务
    if (c.completedTasks.length > 0) {
      lines.push('## ✅ 已完成任务');
      c.completedTasks.slice(-10).forEach(t => { // 只显示最近10个
        const status = t.status === 'completed' ? '[x]' : '[!]';
        lines.push(`- ${status} ${t.description}${t.result ? ` - ${t.result}` : ''}`);
      });
      lines.push('');
    }

    // 关键决策
    if (c.keyDecisions.length > 0) {
      lines.push('## 🔑 关键决策');
      c.keyDecisions.forEach((d, i) => {
        lines.push(`${i + 1}. ${d.description}: ${d.reason}`);
      });
      lines.push('');
    }

    // 代码变更
    if (c.codeChanges.length > 0) {
      lines.push('## 📝 代码变更摘要');
      c.codeChanges.slice(-20).forEach(ch => { // 只显示最近20个
        lines.push(`- \`${ch.file}\`: ${ch.summary}`);
      });
      lines.push('');
    }

    // 重要上下文
    if (c.importantContext.length > 0) {
      lines.push('## 🧠 重要上下文');
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

