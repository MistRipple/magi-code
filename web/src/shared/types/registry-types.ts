/**
 * Agent Registry 前端类型定义
 *
 * 从 magi 原始 src/orchestrator/registry/types.ts 提取前端所需子集。
 * 前端自包含版本 — 不引入后端 orchestrator 重量级依赖。
 */

import type { LLMConfig, ModelApiProtocol } from './agent-types';

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
