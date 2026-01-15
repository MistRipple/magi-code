/**
 * Worker Agent - 执行者代理基类
 *
 * 核心职责：
 * - 接收编排者分配的任务
 * - 执行编码任务
 * - 向编排者汇报进度和结果
 *
 * 所有 Worker（包括 Worker Claude）都继承此基类
 */

import { EventEmitter } from 'events';
import { CLIAdapterFactory } from '../cli/adapter-factory';
import { CLIResponse } from '../cli/types';
import { MessageBus, globalMessageBus } from './message-bus';
import { SnapshotManager } from '../snapshot-manager';
import { PermissionMatrix } from '../types';
import {
  WorkerType,
  WorkerState,
  WorkerInfo,
  WorkerEvents,
  SubTask,
  ExecutionResult,
  BusMessage,
  TaskDispatchMessage,
  TaskCancelMessage,
  OrchestratorCommandMessage,
} from './protocols/types';

/** Worker 配置 */
export interface WorkerConfig {
  type: WorkerType;
  cliFactory: CLIAdapterFactory;
  messageBus?: MessageBus;
  orchestratorId?: string;
  snapshotManager?: SnapshotManager; 
  permissions?: PermissionMatrix;
}

/**
 * Worker Agent 基类
 * 封装 CLI 适配器，提供统一的任务执行和汇报接口
 */
export class WorkerAgent extends EventEmitter {
  readonly id: string;
  readonly type: WorkerType;

  protected cliFactory: CLIAdapterFactory;
  protected messageBus: MessageBus;
  protected orchestratorId: string;
  protected snapshotManager?: SnapshotManager; 
  protected permissions: PermissionMatrix;

  private _state: WorkerState = 'idle';
  private currentTaskId: string | null = null;
  private currentSubTaskId: string | null = null;
  private currentContextSnapshot: string | null = null;
  private abortController: AbortController | null = null;
  private unsubscribers: Array<() => void> = [];

  constructor(config: WorkerConfig) {
    super();
    this.id = `worker_${config.type}_${Date.now()}`;
    this.type = config.type;
    this.cliFactory = config.cliFactory;
    this.messageBus = config.messageBus || globalMessageBus;
    this.orchestratorId = config.orchestratorId || 'orchestrator';
    this.snapshotManager = config.snapshotManager; 
    this.permissions = config.permissions || { allowEdit: true, allowBash: true, allowWeb: true };

    this.setupMessageHandlers();
  }

  /** 获取当前状态 */
  get state(): WorkerState {
    return this._state;
  }

  /** 获取 Worker 信息 */
  get info(): WorkerInfo {
    return {
      id: this.id,
      type: this.type,
      state: this._state,
      currentTaskId: this.currentTaskId || undefined,
      lastActivity: Date.now(),
    };
  }

  /** 设置快照管理器 */
  setSnapshotManager(manager: SnapshotManager): void {
    this.snapshotManager = manager;
  }

  /** 设置状态 */
  protected setState(state: WorkerState): void {
    if (this._state !== state) {
      this._state = state;
      this.emit('stateChange', state);
    }
  }

  /** 设置消息处理器 */
  private setupMessageHandlers(): void {
    // 订阅发送给自己的消息
    const unsubSelf = this.messageBus.subscribe(this.id, (msg) => this.handleMessage(msg));
    this.unsubscribers.push(unsubSelf);

    // 订阅任务分发消息
    const unsubDispatch = this.messageBus.subscribe('task_dispatch', (msg) => {
      if (msg.target === this.id) {
        this.handleMessage(msg);
      }
    });
    this.unsubscribers.push(unsubDispatch);

    // 订阅编排者命令
    const unsubCommand = this.messageBus.subscribe('orchestrator_command', (msg) => {
      this.handleMessage(msg);
    });
    this.unsubscribers.push(unsubCommand);
  }

  /** 处理消息 */
  private async handleMessage(message: BusMessage): Promise<void> {
    switch (message.type) {
      case 'task_dispatch':
        await this.handleTaskDispatch(message as TaskDispatchMessage);
        break;
      case 'task_cancel':
        await this.handleTaskCancel(message as TaskCancelMessage);
        break;
      case 'orchestrator_command':
        await this.handleOrchestratorCommand(message as OrchestratorCommandMessage);
        break;
    }
  }

  /** 处理任务分发 */
  private async handleTaskDispatch(message: TaskDispatchMessage): Promise<void> {
    const { taskId, subTask, context } = message.payload;
    
    if (this._state !== 'idle') {
      console.warn(`[WorkerAgent ${this.id}] 收到任务但当前状态为 ${this._state}，忽略`);
      return;
    }

    await this.executeTask(taskId, subTask, context);
  }

  /** 处理任务取消 */
  private async handleTaskCancel(message: TaskCancelMessage): Promise<void> {
    const { taskId, subTaskId } = message.payload;
    
    if (this.currentTaskId === taskId && (!subTaskId || this.currentSubTaskId === subTaskId)) {
      await this.cancel();
    }
  }

  /** 处理编排者命令 */
  private async handleOrchestratorCommand(message: OrchestratorCommandMessage): Promise<void> {
    const { command } = message.payload;

    switch (command) {
      case 'cancel_all':
        await this.cancel();
        break;
      case 'status_check':
        this.reportReady();
        break;
    }
  }

  /**
   * 执行任务
   * 核心方法：接收子任务，调用 CLI 执行，汇报结果
   */
  async executeTask(taskId: string, subTask: SubTask, context?: string): Promise<ExecutionResult> {
    const startTime = Date.now();
    this.currentTaskId = taskId;
    this.currentSubTaskId = subTask.id;
    this.currentContextSnapshot = context ? this.truncateSnapshot(context) : null;
    this.abortController = new AbortController();

    this.setState('executing');

    const files = (subTask.targetFiles || []).filter(Boolean);
    const summary = [
      `任务描述: ${subTask.description}`,
      `目标文件: ${files.length > 0 ? files.join(', ') : '无'}`
    ].join('\n');
    this.cliFactory.emitOrchestratorMessageToUI(this.type, summary);

    // 汇报任务开始
    this.messageBus.reportProgress(
      this.id,
      this.orchestratorId,
      taskId,
      subTask.id,
      'started',
      { message: `开始执行: ${subTask.description}` }
    );

    try {
      // 构建执行 prompt
      const prompt = this.buildExecutionPrompt(subTask, context);

      // 调用 CLI 执行
      const response = await this.executeCLI(prompt);

      // 检查是否被取消
      if (this.abortController.signal.aborted) {
        throw new Error('任务已被取消');
      }

     
      if (response.fileChanges && response.fileChanges.length > 0 && this.snapshotManager) {
        for (const change of response.fileChanges) {
          try {
            this.snapshotManager.createSnapshot(change.filePath, this.type, subTask.id);
          } catch (err) {
            console.warn(`[WorkerAgent ${this.id}] 创建快照失败: ${change.filePath}`, err);
          }
        }
      }

      const result: ExecutionResult = {
        workerId: this.id,
        workerType: this.type,
        taskId,
        subTaskId: subTask.id,
        result: this.formatResultContent(subTask, response.content),
        success: !response.error,
        duration: Date.now() - startTime,
        modifiedFiles: response.fileChanges?.map(f => f.filePath),
        error: response.error,
        inputTokens: response.tokenUsage?.inputTokens,
        outputTokens: response.tokenUsage?.outputTokens,
      };

      this.setState('completed');

      // 汇报任务完成
      this.messageBus.reportTaskCompleted(this.id, this.orchestratorId, result);
      this.emit('completed', result);

      this.cleanup();
      return result;

    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);

      this.setState('failed');

      // 汇报任务失败
      this.messageBus.reportTaskFailed(
        this.id,
        this.orchestratorId,
        taskId,
        subTask.id,
        errorMsg,
        true // canRetry
      );
      this.emit('failed', errorMsg);

      const result: ExecutionResult = {
        workerId: this.id,
        workerType: this.type,
        taskId,
        subTaskId: subTask.id,
        result: '',
        success: false,
        duration: Date.now() - startTime,
        error: errorMsg,
      };

      this.cleanup();
      return result;
    }
  }

  /**
   * 构建执行 prompt
   */
  protected buildExecutionPrompt(subTask: SubTask, context?: string): string {
    const filesHint = subTask.targetFiles?.length
      ? `\n\n**目标文件**: ${subTask.targetFiles.join(', ')}`
      : '';

    const contextHint = context ? `\n\n**上下文**:\n${context}` : '';
    const permissionHint = this.buildPermissionHint();
    const canEdit = this.permissions.allowEdit !== false;

    if (subTask.kind === 'architecture') {
      return `${subTask.prompt}${contextHint}${permissionHint}

**执行模式**: 架构与契约设计
- 仅输出设计要点与契约约束，不修改任何文件
- 不调用工具，不生成代码块
- 不展示分析过程，输出简洁，控制在 15 行以内

**重要**: 请使用中文回复。`;
    }

    if (subTask.kind === 'background' || subTask.background) {
      return `${subTask.prompt}${filesHint}${contextHint}${permissionHint}

**执行模式**: 后台探索
- 不修改任何文件，输出简明结论与可操作建议
- 避免长篇大论，聚焦结论与证据
- 不调用工具

**重要**: 请使用中文回复。`;
    }

    if (subTask.kind === 'integration') {
      return `${subTask.prompt}${filesHint}${contextHint}${permissionHint}`;
    }

    const claudeConciseHint = this.type === 'claude'
      ? '\n\n**输出要求**:\n- 不展示分析过程，不复述用户需求或计划\n- 输出控制在 8-12 行以内\n- 仅保留必要步骤与关键结论'
      : '';

    if (!canEdit) {
      return `${subTask.prompt}${filesHint}${contextHint}${permissionHint}${claudeConciseHint}

**执行模式**: 只读分析
- 不修改任何文件，提供修改建议或差异说明
- 不调用工具
- 输出简要结论与可执行建议

**重要**: 请使用中文回复，包括代码注释也使用中文。`;
    }

    return `${subTask.prompt}${filesHint}${contextHint}${permissionHint}${claudeConciseHint}

**执行模式**: 直接修改
- 你拥有完整的文件写入权限，可以直接修改文件
- 完成必要的更改以完成任务
- 完成后提供简要的更改说明
- 严禁修改 .multicli/ 目录内的计划与状态文件

**重要**: 请使用中文回复，包括代码注释也使用中文。`;
  }

  private formatResultContent(subTask: SubTask, content?: string): string {
    const raw = (content || '').trim();
    if (!raw) return '';
    if (subTask.kind === 'background' || subTask.background) {
      return this.formatBackgroundSummary(raw);
    }
    return this.truncateText(raw, 4000);
  }

  private formatBackgroundSummary(content: string): string {
    const points = this.extractSummaryPoints(content, 6);
    const summary = ['背景摘要:'].concat(points.map(point => `- ${point}`)).join('\n');
    return this.truncateText(summary, 1200);
  }

  private extractSummaryPoints(content: string, limit: number): string[] {
    const parsed = this.tryExtractJson(content);
    if (parsed && typeof parsed === 'object') {
      const points: string[] = [];
      const fields = ['summary', 'conclusion', 'result', 'recommendation', 'suggestions', 'notes'];
      for (const field of fields) {
        const value = (parsed as Record<string, unknown>)[field];
        if (typeof value === 'string' && value.trim()) {
          points.push(value.trim());
        } else if (Array.isArray(value)) {
          value.forEach(item => {
            if (typeof item === 'string' && item.trim()) {
              points.push(item.trim());
            }
          });
        }
        if (points.length >= limit) break;
      }
      if (points.length > 0) {
        return points.slice(0, limit);
      }
    }

    const lines = content.split('\n').map(line => line.trim()).filter(Boolean);
    if (lines.length > 0) {
      return lines.slice(0, limit);
    }
    const sentences = content.split(/[。！？.!?]/).map(s => s.trim()).filter(Boolean);
    return sentences.slice(0, limit);
  }

  private tryExtractJson(content: string): unknown | null {
    const match = content.match(/\{[\s\S]*\}/);
    if (!match) return null;
    try {
      return JSON.parse(match[0]);
    } catch {
      return null;
    }
  }

  private truncateText(text: string, maxChars: number): string {
    if (text.length <= maxChars) return text;
    return `${text.slice(0, maxChars)}...`;
  }

  private buildPermissionHint(): string {
    const hints: string[] = [];
    if (!this.permissions.allowEdit) {
      hints.push('- 不要修改文件');
    }
    if (!this.permissions.allowBash) {
      hints.push('- 禁止执行命令或脚本');
    }
    if (!this.permissions.allowWeb) {
      hints.push('- 禁止访问网络或外部资源');
    }
    if (hints.length === 0) {
      return '';
    }
    return `\n\n**权限约束**:\n${hints.join('\n')}`;
  }

  /**
   * 调用 CLI 执行任务
   */
  protected async executeCLI(prompt: string): Promise<CLIResponse> {
    const adapter = this.cliFactory.getOrCreate(this.type);

    if (!adapter.isConnected) {
      await adapter.connect();
    }

    const waitForIdle = async (timeoutMs: number = 60000) => {
      const start = Date.now();
      while (Date.now() - start < timeoutMs) {
        if (!adapter.isBusy) {
          return;
        }
        await new Promise(resolve => setTimeout(resolve, 200));
      }
      throw new Error(`${this.type} CLI is busy`);
    };

    await waitForIdle();

    // 设置输出监听，转发流式输出
    const outputHandler = (chunk: string) => {
      this.emit('output', chunk);
      if (this.currentTaskId && this.currentSubTaskId) {
        this.messageBus.reportProgress(
          this.id,
          this.orchestratorId,
          this.currentTaskId,
          this.currentSubTaskId,
          'in_progress',
          { output: chunk }
        );
      }
    };

    adapter.on('output', outputHandler);

    try {
      const response = await adapter.sendMessage(prompt, undefined, {
        taskId: this.currentTaskId ?? undefined,
        subTaskId: this.currentSubTaskId ?? undefined,
        intent: 'worker_execute',
        contextSnapshot: this.currentContextSnapshot ?? undefined,
      });
      return response;
    } finally {
      adapter.off('output', outputHandler);
    }
  }

  /**
   * 取消当前任务
   */
  async cancel(): Promise<void> {
    if (this._state !== 'executing') {
      return;
    }

    console.log(`[WorkerAgent ${this.id}] 取消任务`);

    // 触发中断信号
    this.abortController?.abort();

    // 中断 CLI
    const adapter = this.cliFactory.getAdapter(this.type);
    if (adapter) {
      await adapter.interrupt();
    }

    this.setState('cancelled');
    this.cleanup();
  }

  /**
   * 汇报 Worker 就绪状态
   */
  reportReady(): void {
    this.messageBus.reportWorkerReady(this.id, this.orchestratorId, this.info);
  }

  /**
   * 清理任务状态
   */
  private cleanup(): void {
    this.currentTaskId = null;
    this.currentSubTaskId = null;
    this.currentContextSnapshot = null;
    this.abortController = null;
    this.setState('idle');
  }

  private truncateSnapshot(context: string, maxChars: number = 6000): string {
    const trimmed = context.trim();
    if (trimmed.length <= maxChars) return trimmed;
    return trimmed.slice(0, maxChars) + '\n...';
  }

  /**
   * 销毁 Worker
   */
  dispose(): void {
    // 取消所有订阅
    this.unsubscribers.forEach(unsub => unsub());
    this.unsubscribers = [];

    // 清理状态
    this.cleanup();
    this.removeAllListeners();
  }
}
