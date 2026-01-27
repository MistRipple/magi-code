/**
 * 适配器工厂接口
 * 统一 Worker 和 LLM 适配器工厂的接口
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
   * @param agent - 代理类型，包括 'orchestrator' 和 Worker 槽位
   */
  sendMessage(
    agent: AgentType,
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

  /**
   * 清除特定适配器的对话历史（可选）
   */
  clearAdapterHistory?(agent: AgentType): void;

  /**
   * 清除所有适配器的对话历史（可选）
   */
  clearAllAdapterHistories?(): void;

  /**
   * 获取适配器历史信息（可选）
   */
  getAdapterHistoryInfo?(agent: AgentType): { messages: number; chars: number } | null;

  /**
   * 获取 ToolManager（可选）
   */
  getToolManager?(): import('../tools/tool-manager').ToolManager;
}
