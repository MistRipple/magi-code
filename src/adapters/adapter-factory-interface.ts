/**
 * 适配器工厂接口
 * 统一 Worker 和 LLM 适配器工厂的接口
 */

import { EventEmitter } from 'events';
import { AgentType } from '../types/agent-types';
import type { ToolManager } from '../tools/tool-manager';
import type { MCPToolExecutor } from '../tools/mcp-executor';

/**
 * 适配器输出范围配置
 * 控制 LLM 响应的输出行为
 */
export interface AdapterOutputScope {
  /** 是否包含思考内容（thinking blocks） */
  includeThinking?: boolean;

  /** 是否包含工具调用信息 */
  includeToolCalls?: boolean;

  /** 消息来源标识 */
  source?: 'orchestrator' | 'worker' | 'user';

  /**
   * 消息可见性
   * - 'user': 用户可见（默认）
   * - 'system': 仅系统日志可见
   * - 'debug': 仅调试模式可见
   */
  visibility?: 'user' | 'system' | 'debug';

  /** 适配器角色 */
  adapterRole?: 'orchestrator' | 'worker';

  /**
   * 决策点回调（工具调用前/后/思考阶段）
   * 返回需要注入的补充指令列表
   */
  decisionHook?: (event: {
    type: 'thinking' | 'tool_call' | 'tool_result';
    toolName?: string;
    toolArgs?: any;
    toolResult?: string;
  }) => string[];

  /**
   * 临时系统提示词（可选）
   * 如果提供，将覆盖适配器的默认系统提示词（仅对当前请求生效）
   */
  systemPrompt?: string;
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
  /**
   * 编排器本轮运行态（仅 orchestrator 返回）
   */
  orchestratorRuntime?: {
    reason:
      | 'completed'
      | 'failure_limit'
      | 'round_limit'
      | 'interrupted'
      | 'unknown';
    rounds: number;
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
   * 静默发送消息（不推送到 UI），用于内部自检等场景。
   * 直接用底层 client 非流式调用，对话历史正常更新。
   */
  sendSilentMessage?(
    agent: AgentType,
    message: string,
  ): Promise<AdapterResponse>;

  /**
   * 中断指定代理的当前操作
   */
  interrupt(agent: AgentType): Promise<void>;

  /**
   * 中断所有适配器的当前请求（不销毁适配器）
   */
  interruptAll(): Promise<void>;

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
  getToolManager(): ToolManager;

  /**
   * 清除特定适配器
   */
  clearAdapter(agent: AgentType): Promise<void>;

  /**
   * 获取 MCP 执行器
   */
  getMCPExecutor(): MCPToolExecutor | null;

  /**
   * 重新加载 MCP 配置
   */
  reloadMCP(): Promise<void>;

  /**
   * 重新加载 Skills
   */
  reloadSkills(): Promise<void>;

  /**
   * 刷新用户规则
   */
  refreshUserRules(): Promise<void>;

  /**
   * 重置所有适配器的 Token 累计
   */
  resetAllTokenUsage(): void;

  /**
   * 获取环境提示词（IDE 状态 + 工具 + 用户规则等）
   */
  getEnvironmentPrompt(): string;

  /**
   * 获取用户规则提示词
   */
  getUserRulesPrompt(): string;

  /**
   * 查询当前是否处于深度任务模式
   */
  isDeepTask(): boolean;
}
