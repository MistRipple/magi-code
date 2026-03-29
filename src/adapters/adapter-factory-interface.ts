/**
 * 适配器工厂接口
 * 统一 Worker 和 LLM 适配器工厂的接口
 */

import { EventEmitter } from 'events';
import { AgentType, ModelAutonomyCapability } from '../types/agent-types';
import type { ToolManager, ToolExecutionContext } from '../tools/tool-manager';
import type { MCPToolExecutor } from '../tools/mcp-executor';
import type { DecisionHookEvent } from '../llm/types';
import type { PlanMode } from '../orchestrator/plan-ledger';
import type { MessageMetadata } from '../protocol/message-protocol';
import type { EffectiveToolPolicy } from '../tools/tool-policy';

/**
 * 适配器输出范围配置
 * 控制 LLM 响应的输出行为
 */
export interface AdapterOutputScope {
  /** 本轮显式生效的规划模式 */
  planningMode?: PlanMode;

  /** 是否包含思考内容（thinking blocks） */
  includeThinking?: boolean;

  /**
   * 历史模式
   * - 'session': 复用当前编排会话历史（默认）
   * - 'isolated': 仅使用当前轮输入，避免项目/编排历史污染简单问答
   */
  historyMode?: 'session' | 'isolated';

  /** 是否包含工具调用信息 */
  includeToolCalls?: boolean;

  /** 当前请求唯一生效的工具策略 */
  toolPolicy?: EffectiveToolPolicy;

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
  decisionHook?: (event: DecisionHookEvent) => string[];

  /**
   * 临时系统提示词（可选）
   * 如果提供，将覆盖适配器的默认系统提示词（仅对当前请求生效）
   */
  systemPrompt?: string;

  /**
   * 显式请求标识（可选）
   * 用于将 LLM 流式输出绑定到 UI 层已创建的占位消息。
   * 取代已废弃的全局 requestContext，从架构源头消除并发竞态。
   */
  requestId?: string;

  /**
   * 当前输出流的显式归属元数据。
   * Worker 普通流式输出必须通过该字段绑定 assignment/request，
   * 前端禁止再依赖消息类型或到达位置猜测归属。
   */
  messageMetadata?: Pick<
    MessageMetadata,
    | 'assignmentId'
    | 'missionId'
    | 'requestId'
    | 'worker'
    | 'sessionId'
    | 'turnId'
    | 'dispatchWaveId'
    | 'laneId'
    | 'workerCardId'
  >;

  /**
   * 工具执行上下文（可选）
   * 用于将本次请求绑定到特定的执行作用域（如 worktree 隔离目录）。
   *
   * 工程约束（必须遵守）：
   * - 后续新增写工具或执行入口时，必须复用同一 toolExecutionContext/worktreePath 链路。
   * - 严禁在工具层自行拼接或绕过该链路注入路径（禁止旁路），否则会破坏写隔离与冲突控制。
   */
  toolExecutionContext?: Partial<ToolExecutionContext>;
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
      | 'failed'
      | 'cancelled'
      | 'stalled'
      | 'budget_exceeded'
      | 'external_wait_timeout'
      | 'external_abort'
      | 'upstream_model_error'
      | 'interrupted'
      | 'unknown';
    rounds: number;
    snapshot?: {
      progressVector?: {
        terminalRequiredTodos?: number;
        acceptedCriteria?: number;
        criticalPathResolved?: number;
        unresolvedBlockers?: number;
      };
      reviewState?: {
        accepted?: number;
        total?: number;
      };
      blockerState?: {
        open?: number;
        score?: number;
        externalWaitOpen?: number;
        maxExternalWaitAgeMs?: number;
      };
      budgetState?: {
        elapsedMs?: number;
        tokenUsed?: number;
        errorRate?: number;
      };
      requiredTotal?: number;
      failedRequired?: number;
      runningOrPendingRequired?: number;
      sourceEventIds?: string[];
    };
    shadow?: {
      enabled: boolean;
      reason: string;
      consistent: boolean;
      note?: string;
    };
    decisionTrace?: Array<{
      round: number;
      phase: 'no_tool' | 'tool' | 'handoff' | 'finalize';
      action: 'continue' | 'continue_with_prompt' | 'terminate' | 'handoff' | 'fallback';
      requiredTotal: number;
      reason?: string;
      candidates?: string[];
      gateState?: {
        noProgressStreak: number;
        budgetBreachStreak: number;
        externalWaitBreachStreak: number;
        consecutiveUpstreamModelErrors: number;
      };
      note?: string;
      timestamp: number;
    }>;
    nextSteps?: string[];
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
  sendSilentMessage(
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

  /**
   * 获取当前用户请求的规划模式
   */
  getRequestedPlanningMode(): PlanMode;

  /**
   * 获取编排模型的自治能力
   */
  getOrchestratorModelCapability?(): ModelAutonomyCapability;
}
