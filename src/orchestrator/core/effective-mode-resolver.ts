import type { InteractionMode } from '../../types';
import type { PlanMode } from '../plan-ledger';
import type { LLMConfig, ModelAutonomyCapability } from '../../types/agent-types';

export interface EffectiveModeInput {
  interactionMode: InteractionMode;
  planningMode: PlanMode;
  modelCapability?: ModelAutonomyCapability;
}

export interface EffectiveModeResolution {
  interactionMode: InteractionMode;
  planningMode: PlanMode;
  requestedInteractionMode: InteractionMode;
  requestedPlanningMode: PlanMode;
  modelCapability: ModelAutonomyCapability;
  allowDeepContinuation: boolean;
  allowAutoGovernanceResume: boolean;
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
  const allowsAutoInteraction = modelCapability === 'C2' || modelCapability === 'C3';
  const allowsDeepPlanning = modelCapability === 'C1' || modelCapability === 'C3';
  const interactionMode: InteractionMode = input.interactionMode === 'auto' && allowsAutoInteraction
    ? 'auto'
    : 'ask';
  const planningMode: PlanMode = input.planningMode === 'deep' && allowsDeepPlanning
    ? 'deep'
    : 'standard';

  return {
    interactionMode,
    planningMode,
    requestedInteractionMode: input.interactionMode,
    requestedPlanningMode: input.planningMode,
    modelCapability,
    allowDeepContinuation: planningMode === 'deep',
    allowAutoGovernanceResume: interactionMode === 'auto',
  };
}
