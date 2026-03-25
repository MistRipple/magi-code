import type {
  Mission,
  MissionContinuationPolicy,
  MissionDeliveryStatus,
  MissionPhase,
  MissionStatus,
} from '../mission';
import type { PlanMode, PlanRecord, PlanRuntimeState, PlanStatus } from '../plan-ledger';
import type { ExecutionChainStatus } from './execution-chain-types';

export type RuntimeTruthAuthority =
  | 'plan_ledger'
  | 'mission_projection'
  | 'timeline_projection'
  | 'recovery_projection'
  | 'orchestrator_transient'
  | 'execution_chain';

export type RuntimeTruthDomain =
  | 'plan_runtime'
  | 'mission_lifecycle'
  | 'mission_delivery'
  | 'timeline_rendering'
  | 'recovery_resume'
  | 'round_execution_context'
  | 'execution_chain_lifecycle';

export interface RuntimeTruthContractItem {
  domain: RuntimeTruthDomain;
  authority: RuntimeTruthAuthority;
  responsibility: string;
  consumers: string[];
}

/**
 * 编排运行时真相源契约。
 *
 * 这份契约的目的不是再造一套状态，而是把当前已经存在的几层状态
 * 用代码形式固化下来，避免不同调用方继续各自理解“该信谁”。
 */
export const ORCHESTRATION_RUNTIME_TRUTH_CONTRACT: readonly RuntimeTruthContractItem[] = Object.freeze([
  {
    domain: 'plan_runtime',
    authority: 'plan_ledger',
    responsibility: '执行运行态、attempt、runtime facets、恢复与治理裁决',
    consumers: ['recovery', 'continuation', 'verification', 'runtime diagnostics'],
  },
  {
    domain: 'mission_lifecycle',
    authority: 'mission_projection',
    responsibility: '任务列表生命周期、任务详情状态、业务视角展示',
    consumers: ['task list', 'task detail', 'session task query'],
  },
  {
    domain: 'mission_delivery',
    authority: 'mission_projection',
    responsibility: '交付状态、交付摘要、交付告警',
    consumers: ['task delivery ui', 'delivery summary'],
  },
  {
    domain: 'timeline_rendering',
    authority: 'timeline_projection',
    responsibility: '主线消息、工具卡片、任务卡片的统一时间轴投影',
    consumers: ['thread timeline', 'worker panels', 'session restore'],
  },
  {
    domain: 'recovery_resume',
    authority: 'recovery_projection',
    responsibility: 'mission -> session -> plan 关联解析与恢复所需快照读取',
    consumers: ['mission resume', 'ledger event binding', 'runtime resume'],
  },
  {
    domain: 'round_execution_context',
    authority: 'orchestrator_transient',
    responsibility: '当前执行轮次的瞬态工作记忆，不参与持久化裁决',
    consumers: ['MissionDrivenEngine current round only'],
  },
  {
    domain: 'execution_chain_lifecycle',
    authority: 'execution_chain',
    responsibility: '执行链的完整生命周期管理，是停止/继续/恢复/放弃的唯一操作目标',
    consumers: ['cancel/resume', 'session persist', 'UI chain status', 'runtime diagnostics'],
  },
]);

export interface MissionProjection {
  source: 'mission_projection';
  missionId: string;
  sessionId: string;
  title: string;
  prompt: string;
  goal: string;
  status: MissionStatus;
  phase: MissionPhase;
  deliveryStatus: MissionDeliveryStatus;
  deliverySummary?: string;
  deliveryDetails?: string;
  deliveryWarnings: string[];
  continuationPolicy: MissionContinuationPolicy;
  continuationReason?: string;
  createdAt: number;
  updatedAt: number;
  startedAt?: number;
  completedAt?: number;
  failureReason?: string;
}

export interface TimelineProjectionContext {
  source: 'timeline_projection';
  missionId: string;
  sessionId: string;
  planId?: string;
  requestId?: string;
  deliveryStatus?: MissionDeliveryStatus;
  runtimeReason?: string;
  finalStatus?: 'completed' | 'failed' | 'cancelled' | 'paused';
}

export interface RecoveryProjection {
  source: 'recovery_projection';
  mission: MissionProjection;
  sessionId: string;
  missionId: string;
  planId: string;
  planStatus: PlanStatus;
  planMode: PlanMode;
  planRevision: number;
  planVersion: number;
  turnId: string;
  runtime: PlanRuntimeState;
}

export interface MissionPlanScope {
  missionId: string;
  sessionId: string;
  planId: string;
}

export function resolveRuntimeTruthContract(domain: RuntimeTruthDomain): RuntimeTruthContractItem {
  const matched = ORCHESTRATION_RUNTIME_TRUTH_CONTRACT.find((item) => item.domain === domain);
  if (!matched) {
    throw new Error(`未定义的运行时真相源领域: ${domain}`);
  }
  return matched;
}

export function toMissionProjection(mission: Mission): MissionProjection {
  return {
    source: 'mission_projection',
    missionId: mission.id,
    sessionId: mission.sessionId,
    title: mission.title || '',
    prompt: mission.userPrompt,
    goal: mission.goal,
    status: mission.status,
    phase: mission.phase,
    deliveryStatus: mission.deliveryStatus,
    deliverySummary: mission.deliverySummary,
    deliveryDetails: mission.deliveryDetails,
    deliveryWarnings: Array.isArray(mission.deliveryWarnings) ? [...mission.deliveryWarnings] : [],
    continuationPolicy: mission.continuationPolicy,
    continuationReason: mission.continuationReason,
    createdAt: mission.createdAt,
    updatedAt: mission.updatedAt,
    startedAt: mission.startedAt,
    completedAt: mission.completedAt,
    failureReason: mission.failureReason,
  };
}

export function toRecoveryProjection(mission: Mission, plan: PlanRecord): RecoveryProjection {
  return {
    source: 'recovery_projection',
    mission: toMissionProjection(mission),
    sessionId: plan.sessionId,
    missionId: mission.id,
    planId: plan.planId,
    planStatus: plan.status,
    planMode: plan.mode,
    planRevision: plan.revision,
    planVersion: plan.version,
    turnId: plan.turnId,
    runtime: {
      acceptance: {
        ...plan.runtime.acceptance,
        criteria: plan.runtime.acceptance.criteria.map((criterion) => ({ ...criterion })),
      },
      review: { ...plan.runtime.review },
      replan: { ...plan.runtime.replan },
      wait: { ...plan.runtime.wait },
      phase: {
        ...plan.runtime.phase,
        remainingPhases: [...plan.runtime.phase.remainingPhases],
      },
      termination: { ...plan.runtime.termination },
    },
  };
}

/**
 * 执行链投影
 *
 * 执行链是"停止 / 继续 / 放弃"三个用户动作的唯一操作目标：
 *
 * - **停止**（interruptCurrentTask）：
 *   将 running 链转换为 interrupted + recoverable，构建 ResumeSnapshot
 * - **继续**（handleContinueTask → prepareChainResume）：
 *   查找最近的 interrupted + recoverable 链，转为 resuming → running，
 *   复用原 missionId 而非重新立项
 * - **放弃**（abandonChain）：
 *   将链标记为 cancelled + 不可恢复
 *
 * 执行链与 session 通过 beforeSaveHook / afterLoadHook 持久化，
 * 进程重启时通过 convergeOnStartup() 收敛孤链。
 */
export interface ExecutionChainProjection {
  source: 'execution_chain';
  chainId: string;
  sessionId: string;
  status: ExecutionChainStatus;
  currentMissionId?: string;
  currentPlanId?: string;
  recoverable: boolean;
  attempt: number;
  createdAt: number;
  updatedAt: number;
}