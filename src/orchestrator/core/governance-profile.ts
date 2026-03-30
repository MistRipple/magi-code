/**
 * 治理配置模块 — 统一定义编排者/Worker 的预算、权限和容忍度参数。
 *
 * 设计意图：
 * 1. 将散落在 orchestrator-adapter.ts 和 adapter-factory.ts 中的硬编码常量收归此处
 * 2. 用「基础值 × 倍率」公式表达 Standard → Deep 的倍率关系，而非两组独立常量
 * 3. 引入 requestComplexity 维度：Deep+simple 场景自动降低预算、放宽编排者写权限
 *    治理不再是粗暴的二选一，而是 mode × complexity 的四象限组合
 */

import type { PlanMode } from '../plan-ledger';

// ============ 请求复杂度信号 ============

/**
 * 请求复杂度（由 RequestClassifier 输出）
 * - simple: 单文件小修改、改 typo、调格式、加一行等明确的小范围改动
 * - complex: 跨文件重构、从 0 到 1 的系统设计、或无法明确判定为简单的请求
 */
export type RequestComplexity = 'simple' | 'complex';

// ============ 编排者写权限策略 ============

/**
 * 编排者对代码写操作的权限等级
 * - allowed: 编排者可以直接使用写工具（Standard 模式默认）
 * - limited: 编排者允许有限直改（Deep+simple：允许写工具，但 Prompt 约束 ≤ 1 文件）
 * - forbidden: 编排者完全禁止写工具（Deep+complex：必须委派 Worker）
 */
export type OrchestratorWritePolicy = 'allowed' | 'limited' | 'forbidden';

// ============ 编排者预算接口 ============

export interface OrchestratorBudget {
  maxDurationMs: number;
  maxTokenUsage: number;
  maxErrorRate: number;
  maxRounds: number;
}

// ============ 治理配置 ============

export interface GovernanceProfile {
  /** 规划模式 */
  mode: PlanMode;
  /** 请求复杂度 */
  complexity: RequestComplexity;
  /** 编排者执行预算 */
  orchestratorBudget: OrchestratorBudget;
  /** 编排者写权限策略 */
  orchestratorWritePolicy: OrchestratorWritePolicy;
  /** 编排者无进展容忍轮次 */
  noProgressStreakThreshold: number;
  /** Worker 轮次倍率（应用于 StallDetectionConfig.maxTotalRounds） */
  workerRoundsMultiplier: number;
  /** 总恢复轮次上限 */
  totalRecoveryRoundsLimit: number;
}

// ============ 基础值常量 ============
// 以 Standard 模式为基准定义基础值

const BASE_ORCHESTRATOR_BUDGET: OrchestratorBudget = {
  maxDurationMs: 420_000,   // 7 分钟
  maxTokenUsage: 120_000,
  maxErrorRate: 0.7,
  maxRounds: 30,
};

/** Deep 模式相对于 Standard 的倍率系数 */
const DEEP_MULTIPLIERS = {
  duration: 900_000 / 420_000,    // ≈ 2.14
  tokenUsage: 280_000 / 120_000,  // ≈ 2.33
  rounds: 80 / 30,                // ≈ 2.67
  errorRateBonus: 0.1,            // 容错率 +10%
  workerRounds: 3,                // Worker 轮次 ×3
  noProgressStreak: 3,            // 无进展容忍 3 轮
  recoveryRounds: 20,             // 恢复轮次上限
} as const;

const STANDARD_DEFAULTS = {
  noProgressStreak: 2,
  workerRoundsMultiplier: 1,
  recoveryRounds: 10,
} as const;

/**
 * Deep+simple 场景的折减系数：给予比 Standard 更高但比 Deep+complex 更低的预算，
 * 避免简单请求浪费 Deep 的完整 80 轮预算。
 */
const DEEP_SIMPLE_DISCOUNT = 0.6;

// ============ 核心 API ============

/**
 * 计算治理配置：基于 mode × complexity 四象限组合输出统一治理参数。
 *
 * | mode     | complexity | 编排者写权限 | 预算水位 | Worker 倍率 |
 * |----------|-----------|------------|---------|------------|
 * | standard | simple    | allowed    | 基础     | ×1          |
 * | standard | complex   | allowed    | 基础     | ×1          |
 * | deep     | simple    | limited    | 基础 × 折减 | ×2       |
 * | deep     | complex   | forbidden  | 完整 Deep  | ×3       |
 */
export function resolveGovernanceProfile(
  mode: PlanMode,
  complexity: RequestComplexity,
): GovernanceProfile {
  if (mode === 'deep') {
    if (complexity === 'simple') {
      return buildDeepSimpleProfile();
    }
    return buildDeepComplexProfile();
  }
  return buildStandardProfile(complexity);
}

/**
 * 快速获取编排者预算（供 orchestrator-adapter.ts 使用）。
 * 直接按 mode 返回预算，不依赖 complexity。
 * 如需精细化预算，使用 resolveGovernanceProfile()。
 */
export function resolveOrchestratorBudget(mode: PlanMode): OrchestratorBudget {
  if (mode === 'deep') {
    return buildDeepOrchestratorBudget();
  }
  return { ...BASE_ORCHESTRATOR_BUDGET };
}

/**
 * 获取编排者无进展容忍轮次。
 */
export function resolveNoProgressStreakThreshold(mode: PlanMode): number {
  return mode === 'deep'
    ? DEEP_MULTIPLIERS.noProgressStreak
    : STANDARD_DEFAULTS.noProgressStreak;
}

// ============ 内部构建函数 ============

function buildDeepOrchestratorBudget(): OrchestratorBudget {
  return {
    maxDurationMs: Math.round(BASE_ORCHESTRATOR_BUDGET.maxDurationMs * DEEP_MULTIPLIERS.duration),
    maxTokenUsage: Math.round(BASE_ORCHESTRATOR_BUDGET.maxTokenUsage * DEEP_MULTIPLIERS.tokenUsage),
    maxErrorRate: Math.min(1, BASE_ORCHESTRATOR_BUDGET.maxErrorRate + DEEP_MULTIPLIERS.errorRateBonus),
    maxRounds: Math.round(BASE_ORCHESTRATOR_BUDGET.maxRounds * DEEP_MULTIPLIERS.rounds),
  };
}

function buildDeepSimpleBudget(): OrchestratorBudget {
  const deepBudget = buildDeepOrchestratorBudget();
  const standardBudget = BASE_ORCHESTRATOR_BUDGET;
  // 简单请求预算 = Standard + (Deep - Standard) × 折减系数
  return {
    maxDurationMs: Math.round(standardBudget.maxDurationMs + (deepBudget.maxDurationMs - standardBudget.maxDurationMs) * DEEP_SIMPLE_DISCOUNT),
    maxTokenUsage: Math.round(standardBudget.maxTokenUsage + (deepBudget.maxTokenUsage - standardBudget.maxTokenUsage) * DEEP_SIMPLE_DISCOUNT),
    maxErrorRate: standardBudget.maxErrorRate + (deepBudget.maxErrorRate - standardBudget.maxErrorRate) * DEEP_SIMPLE_DISCOUNT,
    maxRounds: Math.round(standardBudget.maxRounds + (deepBudget.maxRounds - standardBudget.maxRounds) * DEEP_SIMPLE_DISCOUNT),
  };
}

function buildStandardProfile(complexity: RequestComplexity): GovernanceProfile {
  return {
    mode: 'standard',
    complexity,
    orchestratorBudget: { ...BASE_ORCHESTRATOR_BUDGET },
    orchestratorWritePolicy: 'allowed',
    noProgressStreakThreshold: STANDARD_DEFAULTS.noProgressStreak,
    workerRoundsMultiplier: STANDARD_DEFAULTS.workerRoundsMultiplier,
    totalRecoveryRoundsLimit: STANDARD_DEFAULTS.recoveryRounds,
  };
}

function buildDeepSimpleProfile(): GovernanceProfile {
  return {
    mode: 'deep',
    complexity: 'simple',
    orchestratorBudget: buildDeepSimpleBudget(),
    // Deep+simple：编排者允许有限直改（≤ 1 文件的小改动），不强制走 Worker 重链路
    orchestratorWritePolicy: 'limited',
    noProgressStreakThreshold: DEEP_MULTIPLIERS.noProgressStreak,
    // Worker 倍率适中（不是完整 ×3）
    workerRoundsMultiplier: 2,
    totalRecoveryRoundsLimit: Math.round(DEEP_MULTIPLIERS.recoveryRounds * DEEP_SIMPLE_DISCOUNT),
  };
}

function buildDeepComplexProfile(): GovernanceProfile {
  return {
    mode: 'deep',
    complexity: 'complex',
    orchestratorBudget: buildDeepOrchestratorBudget(),
    // Deep+complex：编排者完全禁止写工具，所有实现通过 Worker 委派
    orchestratorWritePolicy: 'forbidden',
    noProgressStreakThreshold: DEEP_MULTIPLIERS.noProgressStreak,
    workerRoundsMultiplier: DEEP_MULTIPLIERS.workerRounds,
    totalRecoveryRoundsLimit: DEEP_MULTIPLIERS.recoveryRounds,
  };
}

