/**
 * Agent Registry 前端类型定义
 *
 * 从 magi 原始 src/orchestrator/registry/types.ts 提取前端所需子集。
 * 前端自包含版本 — 不引入后端 orchestrator 重量级依赖。
 */

import type { LLMConfig, ModelApiProtocol } from './agent-types';

// ============================================================================
// Normalizer 族
// ============================================================================

export type NormalizerFamily = 'anthropic' | 'openai' | 'google';

// ============================================================================
// 停滞检测配置
// ============================================================================

export interface StallDetectionConfig {
  consecutiveFailThreshold: number;
  totalFailLimit: number;
  stallWarnLevel1: number;
  stallWarnLevel2: number;
  stallWarnLevel3: number;
  stallAbortThreshold: number;
  maxTotalRounds: number;
  noOutputWarn: number;
  noOutputForce: number;
  noOutputAbort: number;
}

// ============================================================================
// ModelEngine（用户配置）
// ============================================================================

/** 连接器自动配置结果（仅 T3 配置成功后写入，设计基线 §5.7.3）。 */
export interface ModelEngineOriginConnector {
  id: string;
  /** 当前生效通道；切换通道推进上下文 epoch。 */
  channel: 'magi_connect' | 'openai_tunnel';
  revision: string;
  configuredAt: number;
}

/**
 * 引擎来源元数据（A6）。
 *
 * 缺省（没有 `origin`）等价于 `{ kind: 'user' }`：用户自填引擎。
 * Web 引擎额外记录发现时的浏览器会话与账号等级，用于 `refresh_required` 判定。
 */
export interface ModelEngineOrigin {
  kind: 'web' | 'user';
  /** Web 引擎专用：发现时的应用级浏览器会话 id。 */
  browserSessionId?: string;
  /** Web 引擎专用：发现时间（Unix 毫秒）。 */
  discoveredAt?: number;
  /** Web 引擎专用：发现时的账号等级。 */
  accountHint?: 'plus' | 'pro' | 'free' | 'unknown';
  connector?: ModelEngineOriginConnector;
}

export interface ModelEngine {
  id: string;
  displayName: string;
  /**
   * 连接配置。**Web 引擎（`apiProtocol === 'chatgpt_web'`）没有 `llm`**：
   * 来源、可用窗口与档位在顶层，不写 baseUrl / apiKey / model（A22）。
   */
  llm?: LLMConfig;
  /** 顶层协议标签；Web 引擎为 `chatgpt_web`。 */
  apiProtocol?: ModelApiProtocol;
  /** Web 引擎：该族的可用输入窗口，由 Web 侧上限表按账号等级写入。 */
  contextWindowTokens?: number;
  /** Web 引擎：该族支持的强度档位取值域（会话内选择器据此置灰）。 */
  efforts?: string[];
  /** 引擎级开关：是否允许工具能力（A9，默认开启）。 */
  toolsEnabled?: boolean;
  /** 引擎级开关：每轮新建对话（默认关闭，§5.6）。 */
  newChatPerTurn?: boolean;
  /** Web 引擎 T2 工具轮数上限（默认 20；阶段 5 诊断 / 主行动可调整）。 */
  toolRoundLimit?: number;
  /** 来源标注；缺省等价于用户自填。 */
  origin?: ModelEngineOrigin;
  runtime?: {
    requestTimeoutMs?: number;
    stallPolicy?: {
      maxTotalRounds?: number;
      noOutputWarn?: number;
      noOutputAbort?: number;
      consecutiveFailThreshold?: number;
      totalFailLimit?: number;
      stallWarnLevel1?: number;
      stallWarnLevel2?: number;
      stallWarnLevel3?: number;
      stallAbortThreshold?: number;
      noOutputForce?: number;
    };
  };
}

// ============================================================================
// AgentBinding（用户配置，极轻量）
// ============================================================================

export interface AgentBinding {
  templateId: string;
  /**
   * 「继承编排模型 vs 显式绑定 engine」的唯一字段：
   * - 空串：继承 orchestrator 当前模型
   * - 非空：显式绑定到指定 engine
   *
   * 不再保留 `modelSource` 二次枚举——单一事实源避免双轨编码同一比特。
   */
  engineId: string;
  bindingRevision: number;
  order: number;
  uiOverrides?: {
    visibleInTabs?: boolean;
  };
  profileOverrides?: {
    focus?: string[];
    constraints?: string[];
  };
}

// ============================================================================
// 展示快照
// ============================================================================

export interface AgentDisplaySnapshot {
  agentId: string;
  displayName: string;
  colorToken?: string;
  icon?: string;
}

// ============================================================================
// 配置文件结构
// ============================================================================

export interface AgentRegistryConfig {
  version: '3.0';
  engines: ModelEngine[];
  agents: AgentBinding[];
}
