import type { Mission, MissionStorageManager } from '../mission';
import type { PlanLedgerService } from '../plan-ledger';
import type { MissionPlanScope, MissionProjection, RecoveryProjection } from './runtime-truth-contract';
import { toMissionProjection, toRecoveryProjection } from './runtime-truth-contract';

export type MissionPlanScopeResolveFailureReason =
  | 'missing_mission_id'
  | 'missing_mission'
  | 'missing_session'
  | 'missing_plan';

export interface MissionPlanScopeResolution {
  scope: MissionPlanScope | null;
  reason?: MissionPlanScopeResolveFailureReason;
}

export class OrchestrationReadModelService {
  constructor(
    private readonly missionStorage: MissionStorageManager,
    private readonly planLedger: PlanLedgerService,
  ) {}

  toMissionProjection(mission: Mission): MissionProjection {
    return toMissionProjection(mission);
  }

  async getMissionProjection(missionId: string): Promise<MissionProjection | null> {
    const normalizedMissionId = typeof missionId === 'string' ? missionId.trim() : '';
    if (!normalizedMissionId) {
      return null;
    }
    const mission = await this.missionStorage.load(normalizedMissionId);
    return mission ? toMissionProjection(mission) : null;
  }

  async listMissionProjectionsBySession(sessionId: string): Promise<MissionProjection[]> {
    const normalizedSessionId = typeof sessionId === 'string' ? sessionId.trim() : '';
    if (!normalizedSessionId) {
      return [];
    }
    const missions = await this.missionStorage.listBySession(normalizedSessionId);
    return missions.map((mission) => toMissionProjection(mission));
  }

  async resolveMissionPlanScope(input: {
    missionId: string;
    preferredSessionId?: string;
    preferredPlanId?: string;
  }): Promise<MissionPlanScope | null> {
    const resolution = await this.resolveMissionPlanScopeDetailed(input);
    return resolution.scope;
  }

  async resolveMissionPlanScopeDetailed(input: {
    missionId: string;
    preferredSessionId?: string;
    preferredPlanId?: string;
  }): Promise<MissionPlanScopeResolution> {
    const missionId = typeof input.missionId === 'string' ? input.missionId.trim() : '';
    if (!missionId) {
      return { scope: null, reason: 'missing_mission_id' };
    }

    const preferredSessionId = typeof input.preferredSessionId === 'string'
      ? input.preferredSessionId.trim()
      : '';
    const preferredPlanId = typeof input.preferredPlanId === 'string'
      ? input.preferredPlanId.trim()
      : '';

    if (preferredSessionId && preferredPlanId) {
      const preferredPlan = this.planLedger.getPlan(preferredSessionId, preferredPlanId);
      if (preferredPlan && (preferredPlan.missionId || '').trim() === missionId) {
        return {
          scope: {
            missionId,
            sessionId: preferredSessionId,
            planId: preferredPlanId,
          },
        };
      }
    }

    const mission = await this.missionStorage.load(missionId);
    if (!mission) {
      return { scope: null, reason: 'missing_mission' };
    }
    const sessionId = typeof mission?.sessionId === 'string' ? mission.sessionId.trim() : '';
    if (!sessionId) {
      return { scope: null, reason: 'missing_session' };
    }

    // 恢复场景需要找到被中断的 plan，即使它已被标记为终态（cancelled/failed）。
    // 传 includeTerminal: true 确保即使 plan 状态为终态也能找到。
    const plan = this.planLedger.getLatestPlanByMission(sessionId, missionId, { includeTerminal: true });
    if (!plan) {
      return { scope: null, reason: 'missing_plan' };
    }

    return {
      scope: {
        missionId,
        sessionId,
        planId: plan.planId,
      },
    };
  }

  async getRecoveryProjectionByMission(input: {
    missionId: string;
    preferredSessionId?: string;
    preferredPlanId?: string;
  }): Promise<RecoveryProjection | null> {
    const missionId = typeof input.missionId === 'string' ? input.missionId.trim() : '';
    if (!missionId) {
      return null;
    }

    const mission = await this.missionStorage.load(missionId);
    if (!mission) {
      return null;
    }

    const scope = await this.resolveMissionPlanScope({
      missionId,
      preferredSessionId: input.preferredSessionId,
      preferredPlanId: input.preferredPlanId,
    });
    if (!scope) {
      return null;
    }

    const plan = this.planLedger.getPlan(scope.sessionId, scope.planId);
    if (!plan) {
      return null;
    }

    return toRecoveryProjection(mission, plan);
  }
}
