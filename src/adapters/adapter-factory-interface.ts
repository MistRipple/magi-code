/**
 * 适配器工厂接口
 * 统一 CLI 和 LLM 适配器工厂的接口
 */

import { EventEmitter } from 'events';
import { AgentType, WorkerSlot } from '../types/agent-types';

/**
 * 适配器输出范围
 */
export interface AdapterOutputScope {
  includeThinking?: boolean;
  includeToolCalls?: boolean;
  source?: 'orchestrator' | 'worker' | 'user';
  streamToUI?: boolean;
  adapterRole?: 'orchestrator' | 'worker';
  messageMeta?: Record<string, any>;
}

/**
 * 适配器响应
 */
export interface AdapterResponse {
  content: string;
  done: boolean;
  error?: string;
  tokenUsage?: {
    inputTokens: number;
    outputTokens: number;
  };
}

/**
 * 适配器工厂接口
 */
export interface IAdapterFactory extends EventEmitter {
  /**
   * 发送消息到指定代理
   */
  sendMessage(
    agent: WorkerSlot,
    message: string,
    images?: string[],
    options?: AdapterOutputScope
  ): Promise<AdapterResponse>;

  /**
   * 中断指定代理的当前操作
   */
  interrupt(agent: AgentType): Promise<void>;

  /**
   * 关闭所有适配器
   */
  shutdown(): Promise<void>;

  /**
   * 检查代理是否已连接
   */
  isConnected(agent: AgentType): boolean;

  /**
   * 检查代理是否忙碌
   */
  isBusy(agent: AgentType): boolean;
}
