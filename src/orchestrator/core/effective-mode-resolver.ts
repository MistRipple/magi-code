import type { PlanMode } from '../plan-ledger';
import type { LLMConfig, ModelAutonomyCapability } from '../../types/agent-types';

export interface EffectiveModeInput {
  planningMode: PlanMode;
  modelCapability?: ModelAutonomyCapability;
}

export interface EffectiveModeResolution {
  planningMode: PlanMode;
  requestedPlanningMode: PlanMode;
  modelCapability: ModelAutonomyCapability;
  allowDeepContinuation: boolean;
  allowAutoGovernanceResume: boolean;
  /** 是否发生了模式降级（用户请求 deep 但实际降级为 standard） */
  degraded: boolean;
  /** 降级原因（仅当 degraded 为 true 时有值） */
  degradedReason?: string;
}

function isCapability(value: unknown): value is ModelAutonomyCapability {
  return value === 'C0' || value === 'C1' || value === 'C2' || value === 'C3';
}

export function resolveModelAutonomyCapability(
  config?: Pick<LLMConfig, 'provider' | 'model' | 'enableThinking' | 'reasoningEffort' | 'autonomyCapability'> | null,
): ModelAutonomyCapability {
  if (!config) {
    return 'C2';
  }

  if (isCapability(config.autonomyCapability)) {
    return config.autonomyCapability;
  }

  const model = (config.model || '').toLowerCase();
  const reasoningEffort = config.reasoningEffort ?? 'medium';
  const enableThinking = config.enableThinking === true;
  const highAutonomyHints = [
    'gpt-5',
    'o3',
    'o4',
    'claude-4',
    'opus-4',
    'sonnet-4',
    'gemini-2.5',
    'gemini 2.5',
  ];
  const deepPlanningHints = [
    'claude-3.7',
    'claude-3.5',
    'sonnet',
    'opus',
    'gpt-4.1',
    'gpt-4o',
    'gemini-1.5',
    'gemini-2.0',
  ];

  if (
    enableThinking
    || reasoningEffort === 'high'
    || reasoningEffort === 'xhigh'
    || highAutonomyHints.some((hint) => model.includes(hint))
  ) {
    return 'C3';
  }

  if (
    reasoningEffort === 'medium'
    || deepPlanningHints.some((hint) => model.includes(hint))
  ) {
    return 'C2';
  }

  return 'C1';
}

export function resolveEffectiveMode(input: EffectiveModeInput): EffectiveModeResolution {
  const modelCapability = input.modelCapability ?? 'C3';
  const allowsDeepPlanning = modelCapability === 'C1' || modelCapability === 'C3';
  const requestedDeep = input.planningMode === 'deep';
  const planningMode: PlanMode = requestedDeep && allowsDeepPlanning
    ? 'deep'
    : 'standard';
  const degraded = requestedDeep && !allowsDeepPlanning;

  return {
    planningMode,
    requestedPlanningMode: input.planningMode,
    modelCapability,
    allowDeepContinuation: planningMode === 'deep',
    allowAutoGovernanceResume: true,
    degraded,
    degradedReason: degraded
      ? `当前模型自治能力为 ${modelCapability}，Deep 模式要求 C1 或 C3，已自动降级为 Standard 模式`
      : undefined,
  };
}
