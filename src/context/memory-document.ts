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
  Issue,
  ResolvedIssue,
  RejectedApproach,
  UserMessage,
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
  addPendingIssue(issue: string | Issue): void {
    const issueObj: Issue = typeof issue === 'string'
      ? {
          id: `issue_${Date.now()}_${Math.random().toString(36).slice(2, 7)}`,
          description: issue,
          source: 'system',
          timestamp: new Date().toISOString()
        }
      : issue;

    if (!this.content.pendingIssues.some(i => i.id === issueObj.id || i.description === issueObj.description)) {
      this.content.pendingIssues.push(issueObj);
      this.dirty = true;
    }
  }

  /**
   * 移除已解决的问题
   */
  resolvePendingIssue(issueIdOrDesc: string): void {
    this.content.pendingIssues = this.content.pendingIssues.filter(
      i => i.id !== issueIdOrDesc && i.description !== issueIdOrDesc
    );
    this.dirty = true;
  }

  // ========== 新增字段的辅助方法 ==========

  /**
   * 设置用户核心意图
   */
  setPrimaryIntent(intent: string): void {
    if (typeof intent === 'string' && intent.trim()) {
      this.content.primaryIntent = intent.trim();
      this.dirty = true;
    }
  }

  /**
   * 添加用户约束条件
   */
  addUserConstraint(constraint: string): void {
    if (typeof constraint === 'string' && constraint.trim()) {
      const trimmed = constraint.trim();
      if (!this.content.userConstraints.includes(trimmed)) {
        this.content.userConstraints.push(trimmed);
        this.dirty = true;
      }
    }
  }

  /**
   * 添加用户消息记录
   */
  addUserMessage(content: string, isKeyInstruction: boolean = false): void {
    if (typeof content === 'string' && content.trim()) {
      this.content.userMessages.push({
        content: content.trim(),
        timestamp: new Date().toISOString(),
        isKeyInstruction
      });
      this.dirty = true;
    }
  }

  /**
   * 设置当前工作状态
   */
  setCurrentWork(work: string): void {
    if (typeof work === 'string') {
      this.content.currentWork = work.trim();
      this.dirty = true;
    }
  }

  /**
   * 添加下一步建议
   */
  addNextStep(step: string): void {
    if (typeof step === 'string' && step.trim()) {
      const trimmed = step.trim();
      if (!this.content.nextSteps.includes(trimmed)) {
        this.content.nextSteps.push(trimmed);
        this.dirty = true;
      }
    }
  }

  /**
   * 清空下一步建议（任务完成时调用）
   */
  clearNextSteps(): void {
    if (this.content.nextSteps.length > 0) {
      this.content.nextSteps = [];
      this.dirty = true;
    }
  }

  /**
   * 添加已解决问题
   */
  addResolvedIssue(problem: string, rootCause: string, solution: string): void {
    if (typeof problem === 'string' && problem.trim()) {
      const resolved: ResolvedIssue = {
        id: `resolved_${Date.now()}_${Math.random().toString(36).slice(2, 7)}`,
        problem: problem.trim(),
        rootCause: rootCause?.trim() || '',
        solution: solution?.trim() || '',
        timestamp: new Date().toISOString()
      };
      this.content.resolvedIssues.push(resolved);
      this.dirty = true;
    }
  }

  /**
   * 添加被拒绝的方案
   */
  addRejectedApproach(approach: string, reason: string, rejectedBy: 'user' | 'technical' = 'user'): void {
    if (typeof approach === 'string' && approach.trim()) {
      // 检查是否已存在相同方案
      const exists = this.content.rejectedApproaches.some(r => r.approach === approach.trim());
      if (!exists) {
        const rejected: RejectedApproach = {
          id: `rejected_${Date.now()}_${Math.random().toString(36).slice(2, 7)}`,
          approach: approach.trim(),
          reason: reason?.trim() || '',
          rejectedBy,
          timestamp: new Date().toISOString()
        };
        this.content.rejectedApproaches.push(rejected);
        this.dirty = true;
      }
    }
  }

  /**
   * 将待解决问题标记为已解决（移动并记录）
   */
  markIssueResolved(issueIdOrDesc: string, rootCause: string, solution: string): void {
    const issueIndex = this.content.pendingIssues.findIndex(
      i => i.id === issueIdOrDesc || i.description === issueIdOrDesc
    );
    if (issueIndex !== -1) {
      const issue = this.content.pendingIssues[issueIndex];
      // 添加到已解决列表
      this.addResolvedIssue(issue.description, rootCause, solution);
      // 从待解决列表移除
      this.content.pendingIssues.splice(issueIndex, 1);
      this.dirty = true;
    }
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
      // 用户意图（新增字段）
      primaryIntent: typeof source.primaryIntent === 'string' ? source.primaryIntent : '',
      userConstraints: Array.isArray(source.userConstraints)
        ? source.userConstraints.filter((c): c is string => typeof c === 'string')
        : [],
      currentWork: typeof source.currentWork === 'string' ? source.currentWork : '',
      nextSteps: Array.isArray(source.nextSteps)
        ? source.nextSteps.filter((s): s is string => typeof s === 'string')
        : [],
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
      .map((i: any) => {
        if (typeof i === 'string') {
           return {
             id: `issue_${Date.now()}_${Math.random().toString(36).slice(2, 7)}`,
             description: i,
             source: 'system',
             timestamp: now
           } as Issue;
        }
        if (typeof i === 'object' && i !== null && typeof i.description === 'string') {
          return {
            id: i.id || `issue_${Date.now()}_${Math.random().toString(36).slice(2, 7)}`,
            description: i.description,
            source: i.source || 'system',
            timestamp: i.timestamp || now
          } as Issue;
        }
        return null;
      })
      .filter((i): i is Issue => i !== null);

    // 规范化 userMessages
    const safeUserMessages = Array.isArray(source.userMessages) ? source.userMessages : [];
    result.userMessages = safeUserMessages
      .map((m: any) => {
        if (typeof m === 'object' && m !== null && typeof m.content === 'string') {
          return {
            content: m.content,
            timestamp: typeof m.timestamp === 'string' ? m.timestamp : now,
            isKeyInstruction: m.isKeyInstruction === true
          } as UserMessage;
        }
        return null;
      })
      .filter((m): m is UserMessage => m !== null);

    // 规范化 resolvedIssues
    const safeResolved = Array.isArray(source.resolvedIssues) ? source.resolvedIssues : [];
    result.resolvedIssues = safeResolved
      .map((r: any) => {
        if (typeof r === 'object' && r !== null && typeof r.problem === 'string') {
          return {
            id: r.id || `resolved_${Date.now()}_${Math.random().toString(36).slice(2, 7)}`,
            problem: r.problem,
            rootCause: typeof r.rootCause === 'string' ? r.rootCause : '',
            solution: typeof r.solution === 'string' ? r.solution : '',
            timestamp: typeof r.timestamp === 'string' ? r.timestamp : now
          } as ResolvedIssue;
        }
        return null;
      })
      .filter((r): r is ResolvedIssue => r !== null);

    // 规范化 rejectedApproaches
    const safeRejected = Array.isArray(source.rejectedApproaches) ? source.rejectedApproaches : [];
    result.rejectedApproaches = safeRejected
      .map((r: any) => {
        if (typeof r === 'object' && r !== null && typeof r.approach === 'string') {
          return {
            id: r.id || `rejected_${Date.now()}_${Math.random().toString(36).slice(2, 7)}`,
            approach: r.approach,
            reason: typeof r.reason === 'string' ? r.reason : '',
            rejectedBy: r.rejectedBy === 'user' ? 'user' : 'technical',
            timestamp: typeof r.timestamp === 'string' ? r.timestamp : now
          } as RejectedApproach;
        }
        return null;
      })
      .filter((r): r is RejectedApproach => r !== null);

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
   * 完整版：包含所有新增字段
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

    // ========== 用户意图（核心）==========
    if (c.primaryIntent) {
      lines.push('## 🎯 用户核心意图');
      lines.push(c.primaryIntent);
      lines.push('');
    }

    if (c.userConstraints.length > 0) {
      lines.push('## ⚠️ 用户约束条件');
      c.userConstraints.forEach(constraint => {
        lines.push(`- ${constraint}`);
      });
      lines.push('');
    }

    if (c.userMessages.length > 0) {
      lines.push('## 💬 用户关键消息');
      // 只显示标记为关键的消息，或最近5条
      const keyMessages = c.userMessages.filter(m => m.isKeyInstruction);
      const displayMessages = keyMessages.length > 0 ? keyMessages : c.userMessages.slice(-5);
      displayMessages.forEach(msg => {
        const marker = msg.isKeyInstruction ? '🔑' : '💬';
        lines.push(`- ${marker} "${msg.content}"`);
      });
      lines.push('');
    }

    // ========== 当前状态 ==========
    if (c.currentWork) {
      lines.push('## 🔄 当前工作');
      lines.push(c.currentWork);
      lines.push('');
    }

    // 当前任务
    if (c.currentTasks.length > 0) {
      lines.push('## 📋 当前任务');
      c.currentTasks.forEach(t => {
        const status = t.status === 'in_progress' ? '[/]' : '[ ]';
        lines.push(`- ${status} ${t.description}${t.assignedWorker ? ` (${t.assignedWorker})` : ''}`);
      });
      lines.push('');
    }

    // 下一步建议
    if (c.nextSteps.length > 0) {
      lines.push('## ⏭️ 下一步建议');
      c.nextSteps.forEach((step, i) => {
        lines.push(`${i + 1}. ${step}`);
      });
      lines.push('');
    }

    // ========== 技术上下文 ==========
    // 关键决策
    if (c.keyDecisions.length > 0) {
      lines.push('## 🔧 关键决策');
      c.keyDecisions.forEach((d, i) => {
        lines.push(`${i + 1}. **${d.description}**: ${d.reason}`);
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
      lines.push('## 📚 重要上下文');
      c.importantContext.forEach(ctx => {
        lines.push(`- ${ctx}`);
      });
      lines.push('');
    }

    // ========== 问题跟踪 ==========
    // 待解决问题
    if (c.pendingIssues.length > 0) {
      lines.push('## 📌 待解决问题');
      c.pendingIssues.forEach(issue => {
        lines.push(`- [${issue.source}] ${issue.description}`);
      });
      lines.push('');
    }

    // 已解决问题
    if (c.resolvedIssues.length > 0) {
      lines.push('## ✅ 已解决问题');
      c.resolvedIssues.slice(-5).forEach(issue => { // 只显示最近5个
        lines.push(`- **问题**: ${issue.problem}`);
        if (issue.rootCause) {
          lines.push(`  - **根因**: ${issue.rootCause}`);
        }
        if (issue.solution) {
          lines.push(`  - **方案**: ${issue.solution}`);
        }
      });
      lines.push('');
    }

    // 被拒绝的方案
    if (c.rejectedApproaches.length > 0) {
      lines.push('## ❌ 被拒绝的方案');
      c.rejectedApproaches.forEach(r => {
        const byLabel = r.rejectedBy === 'user' ? '用户拒绝' : '技术不可行';
        lines.push(`- ~~${r.approach}~~ (${byLabel}: ${r.reason})`);
      });
      lines.push('');
    }

    // ========== 已完成任务（放最后，可压缩）==========
    if (c.completedTasks.length > 0) {
      lines.push('## ✓ 已完成任务');
      c.completedTasks.slice(-10).forEach(t => { // 只显示最近10个
        const status = t.status === 'completed' ? '[x]' : '[!]';
        lines.push(`- ${status} ${t.description}${t.result ? ` - ${t.result}` : ''}`);
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
   * 升级版：支持所有新字段
   */
  pruneOldData(options: {
    keepCompletedTasks?: number;
    keepCodeChanges?: number;
    keepUserMessages?: number;
    keepResolvedIssues?: number;
    keepRejectedApproaches?: number;
  } = {}): void {
    const {
      keepCompletedTasks = 5,
      keepCodeChanges = 10,
      keepUserMessages = 10,
      keepResolvedIssues = 5,
      keepRejectedApproaches = 5
    } = options;

    let changed = false;

    if (this.content.completedTasks.length > keepCompletedTasks) {
      this.content.completedTasks = this.content.completedTasks.slice(-keepCompletedTasks);
      changed = true;
    }
    if (this.content.codeChanges.length > keepCodeChanges) {
      this.content.codeChanges = this.content.codeChanges.slice(-keepCodeChanges);
      changed = true;
    }
    if (this.content.userMessages.length > keepUserMessages) {
      // 优先保留关键指令
      const keyMessages = this.content.userMessages.filter(m => m.isKeyInstruction);
      const regularMessages = this.content.userMessages.filter(m => !m.isKeyInstruction);
      const keepRegular = Math.max(0, keepUserMessages - keyMessages.length);
      this.content.userMessages = [
        ...keyMessages,
        ...regularMessages.slice(-keepRegular)
      ];
      changed = true;
    }
    if (this.content.resolvedIssues.length > keepResolvedIssues) {
      this.content.resolvedIssues = this.content.resolvedIssues.slice(-keepResolvedIssues);
      changed = true;
    }
    if (this.content.rejectedApproaches.length > keepRejectedApproaches) {
      this.content.rejectedApproaches = this.content.rejectedApproaches.slice(-keepRejectedApproaches);
      changed = true;
    }

    if (changed) {
      this.dirty = true;
    }
  }
}