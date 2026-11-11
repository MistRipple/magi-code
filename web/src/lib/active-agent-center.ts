import type {
  AgentProjectionDto,
  AgentRunOutcome,
  AgentRunProjectionDto,
} from '../shared/rust-backend-types';

export interface ActiveAgentGroups {
  queued: AgentProjectionDto[];
  running: AgentProjectionDto[];
  attention: AgentProjectionDto[];
  completed: AgentProjectionDto[];
}

export interface ActiveAgentSummary {
  queuedCount: number;
  activeCount: number;
  attentionCount: number;
  completedCount: number;
  triggerCount: number;
}

export function isAgentRuntimeActive(agent: AgentProjectionDto): boolean {
  return agent.status === 'pending'
    || agent.status === 'running'
    || agent.lifecycle === 'queued'
    || agent.lifecycle === 'running';
}

export interface AgentRuntimeTiming {
  active: boolean;
  startedAt: number;
  completedAt: number;
  durationMs: number;
}

/**
 * 代理投影的 `updatedAt` 在终态由任务状态迁移写入，因此它是子代理完成时刻的权威值。
 * 运行中则只提供起点，具体计时由展示层使用同一时钟持续刷新。
 */
export function agentRuntimeTiming(agent: AgentProjectionDto, nowMs: number): AgentRuntimeTiming {
  const startedAt = Number.isFinite(agent.startedAt) ? Math.max(0, agent.startedAt) : 0;
  const active = isAgentRuntimeActive(agent);
  const completedAt = !active
    && typeof agent.completedAt === 'number'
    && Number.isFinite(agent.completedAt)
    ? Math.max(startedAt, agent.completedAt)
    : 0;
  const durationMs = !active
    && typeof agent.responseDurationMs === 'number'
    && Number.isFinite(agent.responseDurationMs)
    ? Math.max(0, Math.floor(agent.responseDurationMs))
    : (active ? Math.max(0, nowMs - startedAt) : 0);
  return {
    active,
    startedAt,
    completedAt,
    durationMs,
  };
}

function requiresAttention(agent: AgentProjectionDto): boolean {
  return agent.status === 'failed'
    || agent.lifecycle === 'failed'
    || agent.lifecycle === 'degraded';
}

function isRunning(agent: AgentProjectionDto): boolean {
  return isAgentRuntimeActive(agent) && !isQueued(agent);
}

function isQueued(agent: AgentProjectionDto): boolean {
  return agent.status === 'pending' || agent.lifecycle === 'queued';
}

export function groupActiveAgents(
  agents: ReadonlyArray<AgentProjectionDto>,
): ActiveAgentGroups {
  const groups: ActiveAgentGroups = {
    queued: [],
    running: [],
    attention: [],
    completed: [],
  };

  for (const agent of agents) {
    if (requiresAttention(agent)) {
      groups.attention.push(agent);
    } else if (isQueued(agent)) {
      groups.queued.push(agent);
    } else if (isRunning(agent)) {
      groups.running.push(agent);
    } else {
      groups.completed.push(agent);
    }
  }

  return groups;
}

function outcomeRequiresAgentCenter(outcome: AgentRunOutcome | null | undefined): boolean {
  return outcome === 'degraded'
    || outcome === 'recoverable'
    || outcome === 'failed'
    || outcome === 'killed';
}

export function buildActiveAgentSummary(groups: ActiveAgentGroups): ActiveAgentSummary {
  const queuedCount = groups.queued.length;
  const activeCount = groups.running.length;
  const attentionCount = groups.attention.length;
  const completedCount = groups.completed.length;
  return {
    queuedCount,
    activeCount,
    attentionCount,
    completedCount,
    triggerCount: queuedCount + activeCount + attentionCount + completedCount,
  };
}

/**
 * 新标签页要列出的“正在做事”的代理：执行中的在前（较新启动的优先），排队中的在后。
 * 已完成的归运行中心的完成分组，失败/降级的由运行中心提示，两者都不在这里重复。
 */
export function selectLauncherAgents(
  agents: ReadonlyArray<AgentProjectionDto>,
): AgentProjectionDto[] {
  const groups = groupActiveAgents(agents);
  const newestFirst = (left: AgentProjectionDto, right: AgentProjectionDto) => right.startedAt - left.startedAt;
  return [...groups.running].sort(newestFirst).concat([...groups.queued].sort(newestFirst));
}

/** 一句话概括当前这轮代理运行；状态条的提示和代理列表的标题共用。 */
export function summarizeActiveAgentRun(
  summary: ActiveAgentSummary,
  outcome: AgentRunOutcome | null | undefined,
  t: (key: string, params?: Record<string, string | number>) => string,
): string {
  if (outcome === 'recoverable') return t('activeAgentCenter.summaryRecoverable');
  if (outcome === 'failed') return t('activeAgentCenter.summaryFailed');
  if (outcome === 'killed') return t('activeAgentCenter.summaryKilled');
  if (outcome === 'degraded') return t('activeAgentCenter.summaryDegraded');
  if (summary.attentionCount > 0) {
    return t('activeAgentCenter.summaryAttention', { active: summary.activeCount, attention: summary.attentionCount });
  }
  if (summary.queuedCount > 0 && summary.activeCount > 0) {
    return t('activeAgentCenter.summaryQueuedRunning', { queued: summary.queuedCount, active: summary.activeCount });
  }
  if (summary.queuedCount > 0) return t('activeAgentCenter.summaryQueued', { count: summary.queuedCount });
  if (summary.activeCount > 0) return t('activeAgentCenter.summaryRunning', { count: summary.activeCount });
  return t('activeAgentCenter.summaryCompleted', { count: summary.completedCount });
}

export function agentDurationSeconds(agent: AgentProjectionDto, nowMs: number): number {
  return Math.floor(agentRuntimeTiming(agent, nowMs).durationMs / 1000);
}

export function formatAgentDuration(totalSeconds: number): string {
  const seconds = Math.max(0, Math.floor(totalSeconds));
  if (seconds < 60) {
    return `${seconds}s`;
  }
  const totalMinutes = Math.floor(seconds / 60);
  if (totalMinutes < 60) {
    return `${totalMinutes}m ${seconds % 60}s`;
  }
  const hours = Math.floor(totalMinutes / 60);
  return `${hours}h ${String(totalMinutes % 60).padStart(2, '0')}m`;
}

/** 只展示当前运行中的代理或需要关注的结果；缺失投影立即清空视图。 */
export function selectActiveAgentRunProjection(
  projection: AgentRunProjectionDto | null,
): AgentRunProjectionDto | null {
  if (!projection?.root_task.task_id.trim()) return null;
  return projection.agents.length > 0 || outcomeRequiresAgentCenter(projection.outcome)
    ? projection
    : null;
}
