/**
 * # MessageHub API 文档
 *
 * MessageHub 是系统的统一消息与事件中心，负责协调编排者(Orchestrator)、工作单元(Worker)和系统(System)之间的消息流转。
 * 它实现了消息的去重、节流和统一路由，确保 UI 呈现的一致性。
 *
 * ## 架构设计（门面模式）
 *
 * MessageHub 现在是一个薄的门面层，内部组合三个组件：
 * - MessageFactory（业务层）：语义化 API，构造 StandardMessage
 * - MessagePipeline（协议层）：校验、去重、节流、生命周期管理
 * - MessageBus（传输层）：事件发射
 *
 * ## 核心职责
 * 1. **统一出口**: 所有 UI 消息统一通过 MessageHub 发送
 * 2. **主从分离**: 主对话区只承载编排者叙事；Worker 输出在各自 Tab 显示
 * 3. **智能流控**: 内置消息去重（ID/内容）和流式节流（默认 100ms）
 *
 * ## API 概览
 *
 * ### 1. 生命周期管理
 * - `newTrace()`: 生成新 Trace ID，开启新会话
 * - `setTraceId(id)`: 设置当前 Trace ID
 * - `getTraceId()`: 获取当前 Trace ID
 *
 * ### 2. 编排者叙事 (主对话区)
 * - `progress(phase: string, content: string, options?)`: 汇报当前阶段进度
 * - `result(content: string, options?)`: 汇报最终执行结果
 * - `orchestratorMessage(content: string, options?)`: 发送分析/规划类消息
 * - `subTaskCard(subTask: SubTaskView)`: 展示/更新子任务卡片状态
 * - `taskAssignment(assignments)`: 发送任务分配宣告（主对话区）
 *
 * ### 3. Worker 交互 (Worker Tab)
 * - `workerOutput(worker: string, content: string, options?)`: 发送 Worker 执行日志
 * - `workerInstruction(worker, content, metadata?)`: 发送任务说明到 Worker Tab
 *
 * ### 4. 系统与错误
 * - `systemNotice(content: string, metadata?)`: 发送系统级通知
 * - `error(err: string, options?)`: 上报错误信息
 *
 * ### 5. 全局通信
 * - `broadcast(msg: string | StandardMessage, options?)`: 向所有组件广播消息
 *
 * ## 典型用法
 *
 * ```typescript
 * const hub = new MessageHub();
 * hub.newTrace(); // 开始新会话
 *
 * // 1. 阶段汇报
 * hub.progress('Planning', '正在制定执行计划...');
 *
 * // 2. 下发任务
 * hub.subTaskCard({
 *   id: 'task-01',
 *   title: '分析依赖',
 *   status: 'running',
 *   worker: 'claude'
 * });
 *
 * // 3. Worker 执行 (独立 Tab)
 * hub.workerOutput('claude', '读取 package.json...');
 *
 * // 4. 任务完成
 * hub.subTaskCard({
 *   id: 'task-01',
 *   title: '分析依赖',
 *   status: 'completed',
 *   worker: 'claude',
 *   summary: '分析完成，发现 3 个问题'
 * });
 *
 * // 5. 最终结果
 * hub.result('依赖分析已完成，准备进行优化。');
 * ```
 *
 * ## 事件订阅
 *
 * ```typescript
 * hub.on('unified:message', (msg) => { ... }); // 监听标准消息
 * hub.on('processingStateChanged', (state) => { ... }); // 监听忙碌状态
 * hub.on('broadcast', (data) => { ... }); // 监听广播消息
 * ```
 */

import type { WorkerSlot } from '../../types';
import type { StandardMessage, MessageMetadata, ContentBlock, StreamUpdate, NotifyLevel, DataMessageType } from '../../protocol/message-protocol';
import { MessageType, ControlMessageType } from '../../protocol/message-protocol';

// 导入三层组件
import { MessageBus, type ProcessingState, type BroadcastData } from './message-bus';
import { MessagePipeline, type PipelineConfig, type RequestMessageSummary } from './message-pipeline';
import { MessageFactory, type SubTaskView } from './message-factory';

// 重新导出类型，保持向后兼容
export type { SubTaskView } from './message-factory';
export type { ProcessingState, BroadcastData } from './message-bus';
export type { RequestMessageSummary, PipelineConfig } from './message-pipeline';

/**
 * MessageHub 事件类型
 */
export interface MessageHubEvents {
  /** 标准消息（来自 LLM/内部流） */
  'unified:message': (message: StandardMessage) => void;
  /** 标准流式更新 */
  'unified:update': (update: StreamUpdate) => void;
  /** 标准完成消息 */
  'unified:complete': (message: StandardMessage) => void;
  /** 广播消息 */
  'broadcast': (data: BroadcastData) => void;
  /** 处理状态变化 */
  'processingStateChanged': (state: ProcessingState) => void;
}

/** MessageHub 配置 */
export interface MessageHubConfig {
  /** 是否启用去重/节流（特性开关） */
  enabled: boolean;
  /** 流式消息最小发送间隔（毫秒） */
  minStreamInterval: number;
  /** 消息历史保留时间（毫秒） */
  retentionTime: number;
  /** 调试模式 */
  debug: boolean;
}

/**
 * MessageHub - 统一消息出口（门面模式）
 *
 * 组合 MessageBus、MessagePipeline、MessageFactory 三层组件，
 * 对外提供统一的语义化 API，保持向后兼容。
 */
export class MessageHub {
  private bus: MessageBus;
  private pipeline: MessagePipeline;
  private factory: MessageFactory;

  constructor(traceId?: string, config?: Partial<MessageHubConfig>) {
    // 1. 初始化传输层（事件发射）
    this.bus = new MessageBus({ debug: config?.debug });

    // 2. 初始化协议层（校验、去重、节流）
    this.pipeline = new MessagePipeline(this.bus, {
      enabled: config?.enabled ?? true,
      minStreamInterval: config?.minStreamInterval ?? 0,
      retentionTime: config?.retentionTime ?? 5 * 60 * 1000,
      debug: config?.debug ?? false,
    });

    // 3. 初始化业务层（语义化 API）
    this.factory = new MessageFactory(this.pipeline, traceId);
  }

  // ==========================================================================
  // 生命周期管理（委托给 MessageFactory）
  // ==========================================================================

  setTraceId(traceId: string): void {
    this.factory.setTraceId(traceId);
  }

  getTraceId(): string {
    return this.factory.getTraceId();
  }

  newTrace(): string {
    return this.factory.newTrace();
  }

  setRequestContext(requestId?: string): void {
    this.factory.setRequestContext(requestId);
  }

  getRequestContext(): string | undefined {
    return this.factory.getRequestContext();
  }

  // ==========================================================================
  // 请求统计（委托给 MessagePipeline）
  // ==========================================================================

  getRequestMessageStats(requestId: string): RequestMessageSummary | undefined {
    return this.pipeline.getRequestMessageStats(requestId);
  }

  finalizeRequestContext(requestId: string): RequestMessageSummary | undefined {
    return this.pipeline.finalizeRequestContext(requestId);
  }

  getRequestMessageId(requestId: string): string | undefined {
    return this.pipeline.getRequestMessageId(requestId);
  }

  getDeadLetterCount(): number {
    return this.pipeline.getDeadLetterCount();
  }

  // ==========================================================================
  // 编排者叙事 API（委托给 MessageFactory）
  // ==========================================================================

  progress(phase: string, content: string, options?: { percentage?: number; metadata?: MessageMetadata }): void {
    this.factory.progress(phase, content, options);
  }

  result(content: string, options?: { success?: boolean; metadata?: MessageMetadata }): void {
    this.factory.result(content, options);
  }

  orchestratorMessage(content: string, options?: { type?: MessageType; metadata?: MessageMetadata }): void {
    this.factory.orchestratorMessage(content, options);
  }

  subTaskCard(subTask: SubTaskView): void {
    this.factory.subTaskCard(subTask);
  }

  taskAssignment(assignments: Array<{ worker: WorkerSlot; shortTitle: string }>, options?: { reason?: string }): void {
    this.factory.taskAssignment(assignments, options);
  }

  // ==========================================================================
  // Worker 交互 API（委托给 MessageFactory）
  // ==========================================================================

  workerOutput(worker: WorkerSlot, content: string, options?: { blocks?: ContentBlock[]; metadata?: MessageMetadata }): void {
    this.factory.workerOutput(worker, content, options);
  }

  workerError(worker: WorkerSlot, content: string, options?: { metadata?: MessageMetadata }): void {
    this.factory.workerError(worker, content, options);
  }

  workerSummary(worker: WorkerSlot, content: string, options?: { metadata?: MessageMetadata }): void {
    this.factory.workerSummary(worker, content, options);
  }

  workerInstruction(worker: WorkerSlot, content: string, metadata?: { assignmentId?: string; missionId?: string }): void {
    this.factory.workerInstruction(worker, content, metadata);
  }

  // ==========================================================================
  // 系统与错误 API（委托给 MessageFactory）
  // ==========================================================================

  systemNotice(content: string, metadata?: MessageMetadata): void {
    this.factory.systemNotice(content, metadata);
  }

  error(errorMsg: string, options?: { details?: Record<string, unknown>; recoverable?: boolean }): void {
    this.factory.error(errorMsg, options);
  }

  // ==========================================================================
  // 广播 API（委托给 MessageFactory + MessageBus）
  // ==========================================================================

  broadcast(message: string | StandardMessage, options?: { target?: string; metadata?: MessageMetadata }): void {
    const msg = this.factory.broadcast(message, options);
    this.bus.emitBroadcast({
      message: msg,
      target: options?.target,
      timestamp: Date.now(),
    });
  }

  // ==========================================================================
  // 控制消息 API（委托给 MessageFactory）
  // ==========================================================================

  sendControl(controlType: ControlMessageType, payload: Record<string, unknown>): void {
    this.factory.sendControl(controlType, payload);
  }

  notify(content: string, level: NotifyLevel = 'info', duration?: number): void {
    this.factory.notify(content, level, duration);
  }

  data(dataType: DataMessageType, payload: Record<string, unknown>): void {
    this.factory.data(dataType, payload);
  }

  phaseChange(phase: string, isRunning: boolean, taskId?: string): void {
    this.factory.phaseChange(phase, isRunning, taskId);
  }

  taskAccepted(requestId: string): void {
    this.factory.taskAccepted(requestId);
  }

  taskRejected(requestId: string, reason: string): void {
    this.factory.taskRejected(requestId, reason);
  }

  workerStatus(worker: string, available: boolean, model?: string): void {
    this.factory.workerStatus(worker, available, model);
  }

  // ==========================================================================
  // 核心消息通道 API（委托给 MessagePipeline）
  // ==========================================================================

  sendMessage(message: StandardMessage): boolean {
    const requestId = this.factory.getRequestContext();
    return this.pipeline.process(message, requestId);
  }

  sendUpdate(update: StreamUpdate): boolean {
    return this.pipeline.processUpdate(update);
  }

  // ==========================================================================
  // 处理状态 API（委托给 MessagePipeline）
  // ==========================================================================

  getProcessingState(): ProcessingState {
    return this.pipeline.getProcessingState();
  }

  forceProcessingState(isProcessing: boolean): void {
    this.pipeline.forceProcessingState(isProcessing);
  }

  // ==========================================================================
  // 事件订阅 API（委托给 MessageBus）
  // ==========================================================================

  on<K extends keyof MessageHubEvents>(event: K, listener: MessageHubEvents[K]): this {
    this.bus.on(event, listener as (...args: unknown[]) => void);
    return this;
  }

  off<K extends keyof MessageHubEvents>(event: K, listener: MessageHubEvents[K]): this {
    this.bus.off(event, listener as (...args: unknown[]) => void);
    return this;
  }

  once<K extends keyof MessageHubEvents>(event: K, listener: MessageHubEvents[K]): this {
    this.bus.once(event, listener as (...args: unknown[]) => void);
    return this;
  }

  emit<K extends keyof MessageHubEvents>(event: K, ...args: Parameters<MessageHubEvents[K]>): boolean {
    return this.bus.emit(event, ...args);
  }

  removeAllListeners(event?: keyof MessageHubEvents): this {
    if (event) {
      this.bus.removeAllListeners(event);
    } else {
      this.bus.removeAllListeners();
    }
    return this;
  }

  // ==========================================================================
  // 资源释放
  // ==========================================================================

  dispose(): void {
    this.pipeline.dispose();
    this.bus.removeAllListeners();
  }
}

/**
 * 全局 MessageHub 实例
 * 用于整个应用的统一消息出口
 */
export const globalMessageHub = new MessageHub();
