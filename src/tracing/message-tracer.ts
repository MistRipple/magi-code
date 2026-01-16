/**
 * MessageTracer - 全链路消息追踪
 * 用于调试和监控消息在各层之间的流转
 */

import { v4 as uuidv4 } from 'uuid';
import { EventEmitter } from 'events';

/** 追踪消息的来源/目标层 */
export type TraceLayer =
  | 'webview'
  | 'webview-provider'
  | 'orchestrator'
  | 'worker-pool'
  | 'cli-adapter'
  | 'session-manager'
  | 'cli-process';

/** 追踪消息类型 */
export type TraceMessageType = 'request' | 'response' | 'event' | 'stream' | 'error';

/** 追踪记录 */
export interface TraceRecord {
  traceId: string;
  parentTraceId?: string;
  messageType: TraceMessageType;
  source: TraceLayer;
  target: TraceLayer;
  timestamp: number;
  duration?: number;
  summary: string;
  payload?: unknown;
  error?: string;
  metadata?: Record<string, unknown>;
}

/** 追踪上下文 */
export interface TraceContext {
  traceId: string;
  parentTraceId?: string;
  startTime: number;
  source: TraceLayer;
}

/** 追踪配置 */
export interface TracerConfig {
  enabled: boolean;
  includePayload: boolean;
  maxRecords: number;
  consoleOutput: boolean;
  consoleLevel: 'verbose' | 'normal' | 'minimal';
}

const DEFAULT_CONFIG: TracerConfig = {
  enabled: true,
  includePayload: false,
  maxRecords: 1000,
  consoleOutput: true,
  consoleLevel: 'normal',
};

/**
 * 消息追踪器
 */
export class MessageTracer extends EventEmitter {
  private config: TracerConfig;
  private records: TraceRecord[] = [];
  private activeContexts: Map<string, TraceContext> = new Map();

  constructor(config?: Partial<TracerConfig>) {
    super();
    this.config = { ...DEFAULT_CONFIG, ...config };
  }

  configure(config: Partial<TracerConfig>): void {
    this.config = { ...this.config, ...config };
  }

  generateTraceId(): string {
    return `trace-${uuidv4().substring(0, 8)}-${Date.now()}`;
  }

  startTrace(source: TraceLayer, parentTraceId?: string): TraceContext {
    const traceId = this.generateTraceId();
    const context: TraceContext = {
      traceId,
      parentTraceId,
      startTime: Date.now(),
      source,
    };
    this.activeContexts.set(traceId, context);
    return context;
  }

  endTrace(traceId: string): void {
    this.activeContexts.delete(traceId);
  }

  trace(
    traceId: string,
    messageType: TraceMessageType,
    source: TraceLayer,
    target: TraceLayer,
    summary: string,
    options?: { payload?: unknown; error?: string; metadata?: Record<string, unknown> }
  ): void {
    if (!this.config.enabled) return;

    const context = this.activeContexts.get(traceId);
    const record: TraceRecord = {
      traceId,
      parentTraceId: context?.parentTraceId,
      messageType,
      source,
      target,
      timestamp: Date.now(),
      duration: context ? Date.now() - context.startTime : undefined,
      summary,
      payload: this.config.includePayload ? options?.payload : undefined,
      error: options?.error,
      metadata: options?.metadata,
    };

    this.addRecord(record);
    this.logToConsole(record);
    this.emit('trace', record);
  }

  traceRequest(traceId: string, source: TraceLayer, target: TraceLayer, summary: string, payload?: unknown): void {
    this.trace(traceId, 'request', source, target, summary, { payload });
  }

  traceResponse(traceId: string, source: TraceLayer, target: TraceLayer, summary: string, payload?: unknown): void {
    this.trace(traceId, 'response', source, target, summary, { payload });
  }

  traceError(traceId: string, source: TraceLayer, target: TraceLayer, error: string, payload?: unknown): void {
    this.trace(traceId, 'error', source, target, `Error: ${error}`, { error, payload });
  }

  traceStream(traceId: string, source: TraceLayer, target: TraceLayer, chunkSize: number): void {
    this.trace(traceId, 'stream', source, target, `Stream chunk: ${chunkSize} bytes`);
  }

  private addRecord(record: TraceRecord): void {
    this.records.push(record);
    if (this.records.length > this.config.maxRecords) {
      this.records = this.records.slice(-this.config.maxRecords);
    }
  }

  private logToConsole(record: TraceRecord): void {
    if (!this.config.consoleOutput) return;

    const timestamp = new Date(record.timestamp).toISOString().substring(11, 23);
    const arrow = record.messageType === 'response' ? '<-' : '->';
    const prefix = `[${timestamp}] [${record.traceId.substring(0, 12)}]`;
    const flow = `${record.source} ${arrow} ${record.target}`;

    switch (this.config.consoleLevel) {
      case 'verbose':
        console.log(`${prefix} ${flow}: ${record.summary}`, record.payload || '');
        break;
      case 'normal':
        console.log(`${prefix} ${flow}: ${record.summary}`);
        break;
      case 'minimal':
        if (record.messageType === 'error') {
          console.log(`${prefix} ${flow}: ${record.summary}`);
        }
        break;
    }
  }

  getRecords(filter?: {
    traceId?: string;
    source?: TraceLayer;
    target?: TraceLayer;
    messageType?: TraceMessageType;
    since?: number;
  }): TraceRecord[] {
    let result = [...this.records];
    if (filter?.traceId) {
      result = result.filter(r => r.traceId === filter.traceId || r.parentTraceId === filter.traceId);
    }
    if (filter?.source) {
      result = result.filter(r => r.source === filter.source);
    }
    if (filter?.target) {
      result = result.filter(r => r.target === filter.target);
    }
    if (filter?.messageType) {
      result = result.filter(r => r.messageType === filter.messageType);
    }
    if (filter?.since) {
      const since = filter.since;
      result = result.filter(r => r.timestamp >= since);
    }
    return result;
  }

  getTraceChain(traceId: string): TraceRecord[] {
    return this.records
      .filter(r => r.traceId === traceId || r.parentTraceId === traceId)
      .sort((a, b) => a.timestamp - b.timestamp);
  }

  getStats(): {
    totalRecords: number;
    byType: Record<TraceMessageType, number>;
    byLayer: Record<TraceLayer, number>;
    errorCount: number;
    avgDuration: number;
  } {
    const byType: Record<string, number> = {};
    const byLayer: Record<string, number> = {};
    let errorCount = 0;
    let totalDuration = 0;
    let durationCount = 0;

    for (const record of this.records) {
      byType[record.messageType] = (byType[record.messageType] || 0) + 1;
      byLayer[record.source] = (byLayer[record.source] || 0) + 1;
      if (record.error) errorCount++;
      if (record.duration) {
        totalDuration += record.duration;
        durationCount++;
      }
    }

    return {
      totalRecords: this.records.length,
      byType: byType as Record<TraceMessageType, number>,
      byLayer: byLayer as Record<TraceLayer, number>,
      errorCount,
      avgDuration: durationCount > 0 ? totalDuration / durationCount : 0,
    };
  }

  clear(): void {
    this.records = [];
    this.activeContexts.clear();
  }

  export(): string {
    return JSON.stringify(this.records, null, 2);
  }
}

// 导出全局单例
export const globalTracer = new MessageTracer();