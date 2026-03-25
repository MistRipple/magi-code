import { logger, LogCategory } from '../../logging';
import type { RequirementAnalysis } from '../protocols/types';
import { type PlanMode, type PlanRecord, PlanLedgerService } from '../plan-ledger';
import type { EffectiveModeResolution } from './effective-mode-resolver';
import type { PlanGovernanceAssessment } from './orchestration-control-plane-types';

interface PlanMutationContext {
  op: string;
  sessionId: string;
  planId: string;
  missionId?: string;
}

interface RecoveryPlanLookupInput {
  missionId: string;
  preferredSessionId?: string;
  preferredPlanId?: string;
}

export interface ResolveExecutionPlanInput {
  sessionId: string;
  turnId: string;
  prompt: string;
  adapterPlanMode: PlanMode;
  requirementAnalysis: RequirementAnalysis;
  resumeMissionId?: string;
}

export interface ResolveExecutionPlanResult {
  executionPlan: PlanRecord;
  currentPlanId: string;
  currentPlanMode: PlanMode;
  effectiveMode: EffectiveModeResolution;
  requirementAnalysis: RequirementAnalysis;
}

export interface StartOrchestratorAttemptInput {
  sessionId: string;
  plan: PlanRecord;
  turnId: string;
}

export interface OrchestrationPlanControllerDependencies {
  planLedger: PlanLedgerService;
  resolveEffectiveMode: (planningMode: PlanMode) => EffectiveModeResolution;
  mergeRequirementAnalysisWithPlan: (base: RequirementAnalysis, plan: PlanRecord) => RequirementAnalysis;
  loadRecoveryPlanRecord: (input: RecoveryPlanLookupInput) => Promise<PlanRecord | null>;
  requirePlanMutation: (
    record: PlanRecord | null,
    context: PlanMutationContext,
  ) => PlanRecord;
  evaluatePlanGovernance: (
    sessionId: string,
    plan: PlanRecord,
    userPrompt: string,
  ) => Promise<PlanGovernanceAssessment>;
  buildFallbackGovernanceAssessment: (error: unknown) => PlanGovernanceAssessment;
}

export class OrchestrationPlanController {
  constructor(
    private readonly deps: OrchestrationPlanControllerDependencies,
  ) {}

  async resolveExecutionPlan(input: ResolveExecutionPlanInput): Promise<ResolveExecutionPlanResult> {
    const resumeMissionId = input.resumeMissionId?.trim();
    if (resumeMissionId) {
      const resumedPlan = await this.deps.loadRecoveryPlanRecord({
        missionId: resumeMissionId,
        preferredSessionId: input.sessionId,
      });
      if (!resumedPlan) {
        throw new Error(`任务 ${resumeMissionId} 缺少可恢复计划，已终止恢复执行`);
      }

      const executionPlan = this.deps.requirePlanMutation(
        await this.deps.planLedger.markExecuting(
          input.sessionId,
          resumedPlan.planId,
          {
            expectedRevision: resumedPlan.revision,
            auditReason: 'resume-mission-ledger-recovery',
          },
        ),
        {
          op: 'resume-markExecuting',
          sessionId: input.sessionId,
          planId: resumedPlan.planId,
          missionId: resumeMissionId,
        },
      );

      logger.info('编排器.PlanController.恢复执行计划', {
        sessionId: input.sessionId,
        missionId: resumeMissionId,
        planId: executionPlan.planId,
        mode: executionPlan.mode,
        revision: executionPlan.revision,
      }, LogCategory.ORCHESTRATOR);

      return {
        executionPlan,
        currentPlanId: executionPlan.planId,
        currentPlanMode: executionPlan.mode,
        effectiveMode: this.deps.resolveEffectiveMode(executionPlan.mode),
        requirementAnalysis: this.deps.mergeRequirementAnalysisWithPlan(input.requirementAnalysis, resumedPlan),
      };
    }

    const draftPlan = await this.deps.planLedger.createDraft({
      sessionId: input.sessionId,
      turnId: input.turnId,
      missionId: input.turnId || undefined,
      mode: input.adapterPlanMode,
      prompt: input.prompt,
      summary: input.requirementAnalysis.goal,
      analysis: input.requirementAnalysis.analysis,
      constraints: input.requirementAnalysis.constraints,
      acceptanceCriteria: input.requirementAnalysis.acceptanceCriteria,
      riskLevel: input.requirementAnalysis.riskLevel,
    });

    let governanceAssessment: PlanGovernanceAssessment;
    try {
      governanceAssessment = await this.deps.evaluatePlanGovernance(input.sessionId, draftPlan, input.prompt);
    } catch (error) {
      logger.warn('编排器.PlanController.治理评估降级', {
        sessionId: input.sessionId,
        planId: draftPlan.planId,
        error: error instanceof Error ? error.message : String(error),
      }, LogCategory.ORCHESTRATOR);
      governanceAssessment = this.deps.buildFallbackGovernanceAssessment(error);
    }

    const approvedPlan = this.deps.requirePlanMutation(
      await this.deps.planLedger.approve(
        input.sessionId,
        draftPlan.planId,
        'system:auto',
        `governance:auto;decision=${governanceAssessment.decision};risk=${governanceAssessment.riskScore.toFixed(3)};confidence=${governanceAssessment.confidence.toFixed(3)};reasons=${governanceAssessment.reasons.join('|') || 'none'}`,
        {
          expectedRevision: draftPlan.revision,
          auditReason: 'plan-governance:auto-approve',
        },
      ),
      {
        op: 'plan-approve-auto',
        sessionId: input.sessionId,
        planId: draftPlan.planId,
      },
    );

    const executionPlan = this.deps.requirePlanMutation(
      await this.deps.planLedger.markExecuting(
        input.sessionId,
        draftPlan.planId,
        {
          expectedRevision: approvedPlan.revision,
          auditReason: 'plan-governance:auto-mark-executing',
        },
      ),
      {
        op: 'plan-mark-executing-auto',
        sessionId: input.sessionId,
        planId: draftPlan.planId,
      },
    );

    return {
      executionPlan,
      currentPlanId: draftPlan.planId,
      currentPlanMode: executionPlan.mode,
      effectiveMode: this.deps.resolveEffectiveMode(executionPlan.mode),
      requirementAnalysis: input.requirementAnalysis,
    };
  }

  async startOrchestratorAttempt(input: StartOrchestratorAttemptInput): Promise<string> {
    const targetId = input.turnId || `orchestrator-${Date.now()}`;
    this.deps.requirePlanMutation(
      await this.deps.planLedger.startAttempt(
        input.sessionId,
        input.plan.planId,
        {
          scope: 'orchestrator',
          targetId,
          reason: 'orchestrator-execution-started',
        },
        {
          expectedRevision: input.plan.revision,
          auditReason: 'orchestrator-execution-start',
        },
      ),
      {
        op: 'orchestrator-start-attempt',
        sessionId: input.sessionId,
        planId: input.plan.planId,
      },
    );
    return targetId;
  }
}
