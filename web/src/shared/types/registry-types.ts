/**
 * Agent Registry 前端类型定义
 *
 * 从 magi 原始 src/orchestrator/registry/types.ts 提取前端所需子集。
 * 前端自包含版本 — 不引入后端 orchestrator 重量级依赖。
 */

import type { LLMConfig } from './agent-types';

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
  llm: LLMConfig;
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

export type AgentModelSource = 'orchestrator' | 'engine';

export interface AgentBinding {
  templateId: string;
  modelSource: AgentModelSource;
  engineId: string;
  enabled: boolean;
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

// ============================================================================
// 工具函数
// ============================================================================

export function deriveNormalizerFamily(
  provider: string,
  _openaiProtocol?: string,
): NormalizerFamily {
  if (provider === 'anthropic') return 'anthropic';
  if (provider === 'google') return 'google';
  return 'openai';
}
