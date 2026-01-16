/**
 * MessageBridge - 消息桥接层
 * 
 * 连接 CLIAdapterFactory 和 Webview，负责：
 * 1. 接收 CLI 原始输出
 * 2. 通过 Normalizer 转换为标准消息
 * 3. 发送标准消息到 Webview
 * 
 * 这是消息标准化架构的核心组件
 */

import { EventEmitter } from 'events';
import type { CLIType } from '../cli/types';
import type { CLIAdapterFactory, AdapterOutputScope } from '../cli/adapter-factory';
import { createNormalizer, BaseNormalizer } from './index';
import {
  StandardMessage,
  StreamUpdate,
  MessageSource,
  MessageLifecycle,
  generateMessageId,
} from '../protocol';

/**
 * MessageBridge 事件
 */
export interface MessageBridgeEvents {
  /** 新的标准消息 */
  message: (message: StandardMessage) => void;
  /** 消息更新（流式） */
  update: (update: StreamUpdate) => void;
  /** 消息完成 */
  complete: (message: StandardMessage) => void;
  /** 错误 */
  error: (error: Error, cli?: CLIType) => void;
}

/**
 * 活跃的消息流
 */
interface ActiveStream {
  messageId: string;
  traceId: string;
  cli: CLIType;
  source: MessageSource;
  normalizer: BaseNormalizer;
  startTime: number;
}

/**
 * MessageBridge 配置
 */
export interface MessageBridgeConfig {
  /** 是否启用调试日志 */
  debug?: boolean;
  /** 流式消息超时时间（毫秒） */
  streamTimeout?: number;
}

/**
 * 消息桥接层
 * 将 CLI 原始输出转换为标准消息格式
 */
export class MessageBridge extends EventEmitter {
  private factory: CLIAdapterFactory;
  private config: MessageBridgeConfig;
  private normalizers: Map<string, BaseNormalizer> = new Map();
  private activeStreams: Map<string, ActiveStream> = new Map();
  private traceIdCounter = 0;

  constructor(factory: CLIAdapterFactory, config?: MessageBridgeConfig) {
    super();
    this.factory = factory;
    this.config = {
      debug: false,
      streamTimeout: 5 * 60 * 1000, // 5 分钟
      ...config,
    };

    this.setupFactoryListeners();
  }

  /**
   * 设置工厂事件监听
   */
  private setupFactoryListeners(): void {
    // 监听流开始事件
    this.factory.on('streamStart', ({ type, source, adapterRole }) => {
      this.handleStreamStart(type, source || (adapterRole === 'orchestrator' ? 'orchestrator' : 'worker'));
    });

    // 监听原始输出
    this.factory.on('output', ({ type, chunk, source, adapterRole }) => {
      const msgSource = source || (adapterRole === 'orchestrator' ? 'orchestrator' : 'worker');
      this.handleOutput(type, chunk, msgSource);
    });

    // 监听响应完成
    this.factory.on('response', ({ type, response, source, adapterRole }) => {
      const msgSource = source || (adapterRole === 'orchestrator' ? 'orchestrator' : 'worker');
      this.handleResponse(type, response, msgSource);
    });

    // 监听错误
    this.factory.on('error', ({ type, error }) => {
      this.handleError(type, error);
    });
  }

  /**
   * 生成追踪 ID
   */
  private generateTraceId(): string {
    return `trace-${Date.now()}-${++this.traceIdCounter}`;
  }

  /**
   * 获取 Normalizer 键
   */
  private getNormalizerKey(cli: CLIType, source: MessageSource): string {
    return `${cli}:${source}`;
  }

  /**
   * 获取或创建 Normalizer
   */
  private getOrCreateNormalizer(cli: CLIType, source: MessageSource): BaseNormalizer {
    const key = this.getNormalizerKey(cli, source);
    let normalizer = this.normalizers.get(key);
    
    if (!normalizer) {
      normalizer = createNormalizer(cli, source, this.config.debug);
      
      // 设置 Normalizer 事件监听
      normalizer.on('message', (message) => {
        this.emit('message', message);
      });
      
      normalizer.on('update', (update) => {
        this.emit('update', update);
      });
      
      normalizer.on('complete', (messageId, message) => {
        this.emit('complete', message);
        // 清理活跃流
        this.activeStreams.delete(messageId);
      });
      
      normalizer.on('error', (error) => {
        this.emit('error', error, cli);
      });
      
      this.normalizers.set(key, normalizer);
    }
    
    return normalizer;
  }

  /**
   * 处理流开始
   */
  private handleStreamStart(cli: CLIType, source: MessageSource): void {
    const normalizer = this.getOrCreateNormalizer(cli, source);
    const traceId = this.generateTraceId();
    const messageId = normalizer.startStream(traceId, source);
    
    const stream: ActiveStream = {
      messageId,
      traceId,
      cli,
      source,
      normalizer,
      startTime: Date.now(),
    };
    
    this.activeStreams.set(messageId, stream);
    this.debug(`[MessageBridge] 流开始: ${cli}/${source} -> ${messageId}`);
  }

  /**
   * 处理原始输出
   */
  private handleOutput(cli: CLIType, chunk: string, source: MessageSource): void {
    // 查找活跃的流
    const stream = this.findActiveStream(cli, source);
    
    if (stream) {
      // 有活跃流，处理增量
      stream.normalizer.processChunk(stream.messageId, chunk);
    } else {
      // 没有活跃流，创建新流
      this.handleStreamStart(cli, source);
      const newStream = this.findActiveStream(cli, source);
      if (newStream) {
        newStream.normalizer.processChunk(newStream.messageId, chunk);
      }
    }
  }

  /**
   * 处理响应完成
   */
  private handleResponse(cli: CLIType, response: any, source: MessageSource): void {
    const stream = this.findActiveStream(cli, source);
    
    if (stream) {
      const error = response.error ? String(response.error) : undefined;
      stream.normalizer.endStream(stream.messageId, error);
      this.debug(`[MessageBridge] 流结束: ${cli}/${source} -> ${stream.messageId}`);
    }
  }

  /**
   * 处理错误
   */
  private handleError(cli: CLIType, error: Error): void {
    // 结束所有该 CLI 的活跃流
    for (const [messageId, stream] of this.activeStreams) {
      if (stream.cli === cli) {
        stream.normalizer.endStream(messageId, error.message);
      }
    }
    
    this.emit('error', error, cli);
  }

  /**
   * 查找活跃的流
   */
  private findActiveStream(cli: CLIType, source: MessageSource): ActiveStream | null {
    for (const stream of this.activeStreams.values()) {
      if (stream.cli === cli && stream.source === source) {
        return stream;
      }
    }
    return null;
  }

  /**
   * 中断指定 CLI 的所有流
   */
  interruptStreams(cli: CLIType): StandardMessage[] {
    const interrupted: StandardMessage[] = [];
    
    for (const [messageId, stream] of this.activeStreams) {
      if (stream.cli === cli) {
        const message = stream.normalizer.interruptStream(messageId);
        if (message) {
          interrupted.push(message);
        }
      }
    }
    
    return interrupted;
  }

  /**
   * 中断所有流
   */
  interruptAllStreams(): StandardMessage[] {
    const interrupted: StandardMessage[] = [];
    
    for (const [messageId, stream] of this.activeStreams) {
      const message = stream.normalizer.interruptStream(messageId);
      if (message) {
        interrupted.push(message);
      }
    }
    
    return interrupted;
  }

  /**
   * 获取活跃流数量
   */
  getActiveStreamCount(): number {
    return this.activeStreams.size;
  }

  /**
   * 检查是否有活跃流
   */
  hasActiveStreams(): boolean {
    return this.activeStreams.size > 0;
  }

  /**
   * 清理资源
   */
  dispose(): void {
    this.interruptAllStreams();
    this.normalizers.clear();
    this.activeStreams.clear();
    this.removeAllListeners();
  }

  /**
   * 调试日志
   */
  private debug(message: string, ...args: unknown[]): void {
    if (this.config.debug) {
      console.log(message, ...args);
    }
  }

  // 事件类型安全
  on<K extends keyof MessageBridgeEvents>(event: K, listener: MessageBridgeEvents[K]): this {
    return super.on(event, listener);
  }

  off<K extends keyof MessageBridgeEvents>(event: K, listener: MessageBridgeEvents[K]): this {
    return super.off(event, listener);
  }

  emit<K extends keyof MessageBridgeEvents>(event: K, ...args: Parameters<MessageBridgeEvents[K]>): boolean {
    return super.emit(event, ...args);
  }
}