import type { InteractionMode } from '../../types';
import type { ModelAutonomyCapability } from '../../types/agent-types';
import type { RequirementAnalysis } from '../protocols/types';
import type { PlanMode } from '../plan-ledger';
import { extractPrimaryIntent, extractUserConstraints } from './mission-driven-engine-helpers';
import { resolveEffectiveMode, type EffectiveModeResolution } from './effective-mode-resolver';
import { classifyRequest, type RequestClassification } from './request-classifier';

export interface EntryRoutingDecision {
  requiresModification: boolean;
  executionMode: RequirementAnalysis['executionMode'];
  entryPath: RequirementAnalysis['entryPath'];
  historyMode: RequirementAnalysis['historyMode'];
  includeThinking: RequirementAnalysis['includeThinking'];
  includeToolCalls: RequirementAnalysis['includeToolCalls'];
  decisionFactors?: string[];
  reason: RequirementAnalysis['reason'];
}

export interface OrchestrationEntryResolution {
  requestedPlanningMode: PlanMode;
  effectiveMode: EffectiveModeResolution;
  classification: RequestClassification;
  requirementAnalysis: RequirementAnalysis;
  routingDecision: EntryRoutingDecision;
}

function extractPreDraftAcceptanceCriteria(
  prompt: string,
  goal: string,
  constraints: string[],
): string[] {
  const constraintSet = new Set(constraints);
  const segments = prompt
    .split(/\n+/)
    .flatMap(line => line.split(/[。！？；;]+/))
    .map(segment => segment.trim())
    .filter(segment => segment.length > 0);
  const matched = Array.from(new Set(
    segments
      .filter(segment => /(?:验收|完成标准|成功标准|acceptance|验证|确保|通过|输出|结果)/i.test(segment))
      .filter(segment => !constraintSet.has(segment))
      .map(segment => (segment.length > 120 ? `${segment.substring(0, 120)}...` : segment))
  )).slice(0, 5);

  if (matched.length > 0) {
    return matched;
  }

  return goal ? [`完成目标：${goal}`] : [];
}

function assessPreDraftRisk(
  prompt: string,
  effectivePlanningMode: PlanMode,
  classification: RequestClassification,
  constraints: string[],
  acceptanceCriteria: string[],
): { riskLevel: 'low' | 'medium' | 'high'; riskFactors: string[] } {
  const riskFactors: string[] = [];
  let score = 0;

  if (effectivePlanningMode === 'deep' && classification.entryPolicy.entryPath === 'task_execution') {
    score += 2;
    riskFactors.push('任务运行在 deep 规划模式');
  }
  if (classification.hasHighImpactIntent) {
    score += 2;
    riskFactors.push('需求涉及高影响改动');
  } else if (classification.hasWriteIntent) {
    score += 1;
    riskFactors.push('需求包含代码修改');
  }
  if (prompt.length >= 280) {
    score += 1;
    riskFactors.push('需求描述较长');
  }
  if (constraints.length >= 3) {
    score += 1;
    riskFactors.push('用户约束较多');
  }
  if (acceptanceCriteria.length >= 4) {
    score += 1;
    riskFactors.push('验收标准较多');
  }

  const riskLevel: 'low' | 'medium' | 'high' = score >= 4
    ? 'high'
    : score >= 2
      ? 'medium'
      : 'low';

  return { riskLevel, riskFactors };
}

export function buildRequirementAnalysis(
  prompt: string,
  effectivePlanningMode: PlanMode,
  classification?: RequestClassification,
): RequirementAnalysis {
  const resolvedClassification = classification || classifyRequest(prompt, effectivePlanningMode);
  const goal = extractPrimaryIntent(prompt) || prompt.trim();
  const constraints = extractUserConstraints(prompt);
  const acceptanceCriteria = extractPreDraftAcceptanceCriteria(prompt, goal, constraints);
  const riskAssessment = assessPreDraftRisk(
    prompt,
    effectivePlanningMode,
    resolvedClassification,
    constraints,
    acceptanceCriteria,
  );
  const analysisParts = [
    resolvedClassification.entryPolicy.entryPath === 'direct_response'
      ? `围绕“${goal}”直接回答用户问题`
      : resolvedClassification.entryPolicy.entryPath === 'lightweight_analysis'
        ? `围绕“${goal}”进行只读分析`
        : `围绕“${goal}”建立执行计划`,
    constraints.length > 0 ? `需遵守 ${constraints.length} 条用户约束` : '当前未识别出额外用户约束',
    acceptanceCriteria.length > 0 ? `验收以 ${acceptanceCriteria.length} 条标准为准` : '验收标准待后续调度补充',
    `风险等级为 ${riskAssessment.riskLevel}`,
  ];

  return {
    goal,
    analysis: analysisParts.join('；'),
    constraints,
    acceptanceCriteria,
    riskLevel: riskAssessment.riskLevel,
    riskFactors: riskAssessment.riskFactors,
    entryPath: resolvedClassification.entryPolicy.entryPath,
    executionMode: resolvedClassification.entryPolicy.entryPath === 'direct_response'
      ? 'direct'
      : resolvedClassification.entryPolicy.entryPath === 'lightweight_analysis'
        ? 'analysis'
        : undefined,
    includeThinking: resolvedClassification.entryPolicy.includeThinking,
    includeToolCalls: resolvedClassification.entryPolicy.includeToolCalls,
    allowedToolNames: resolvedClassification.entryPolicy.allowedToolNames,
    historyMode: resolvedClassification.entryPolicy.historyMode,
    requiresModification: resolvedClassification.requiresModification,
    decisionFactors: [...resolvedClassification.decisionFactors],
    reason: resolvedClassification.reason,
  };
}

export function resolveOrchestrationEntry(input: {
  prompt: string;
  requestedPlanningMode: PlanMode;
  interactionMode: InteractionMode;
  modelCapability?: ModelAutonomyCapability;
}): OrchestrationEntryResolution {
  const effectiveMode = resolveEffectiveMode({
    interactionMode: input.interactionMode,
    planningMode: input.requestedPlanningMode,
    modelCapability: input.modelCapability,
  });
  const classification = classifyRequest(input.prompt, effectiveMode.planningMode);
  const requirementAnalysis = buildRequirementAnalysis(input.prompt, effectiveMode.planningMode, classification);

  return {
    requestedPlanningMode: input.requestedPlanningMode,
    effectiveMode,
    classification,
    routingDecision: {
      requiresModification: requirementAnalysis.requiresModification ?? false,
      executionMode: requirementAnalysis.executionMode,
      entryPath: requirementAnalysis.entryPath,
      historyMode: requirementAnalysis.historyMode,
      includeThinking: requirementAnalysis.includeThinking ?? false,
      includeToolCalls: requirementAnalysis.includeToolCalls ?? false,
      decisionFactors: requirementAnalysis.decisionFactors,
      reason: requirementAnalysis.reason,
    },
    requirementAnalysis,
  };
}
