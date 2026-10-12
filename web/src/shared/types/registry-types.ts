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

export interface ModelEngine {
  id: string;
  displayName: string;
  /** daemon 注册的插件引擎只读投影，不能进入本地模型配置写入路径。 */
  source?: 'plugin' | 'user';
  pluginId?: string;
  pluginContributionId?: string;
  description?: string;
  /**
   * 连接配置（baseUrl / apiKey / model）。
   */
  llm?: LLMConfig;
  /** 协议标签；GPT Web 不是注册表引擎，这里只会是用户自填的 provider 协议。 */
  apiProtocol?: ModelApiProtocol;
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
