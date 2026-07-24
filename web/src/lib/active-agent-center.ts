import type { AgentProjectionDto } from '../shared/rust-backend-types';

export interface ActiveAgentGroups {
  running: AgentProjectionDto[];
  attention: AgentProjectionDto[];
  completed: AgentProjectionDto[];
}

export interface ActiveAgentSummary {
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
  return isAgentRuntimeActive(agent);
}

export function groupActiveAgents(
  agents: ReadonlyArray<AgentProjectionDto>,
): ActiveAgentGroups {
  const groups: ActiveAgentGroups = {
    running: [],
    attention: [],
    completed: [],
  };

  for (const agent of agents) {
    if (requiresAttention(agent)) {
      groups.attention.push(agent);
    } else if (isRunning(agent)) {
      groups.running.push(agent);
    } else {
      groups.completed.push(agent);
    }
  }

  return groups;
}

export function shouldShowActiveAgentCenter(groups: ActiveAgentGroups): boolean {
  return groups.running.length > 0
    || groups.attention.length > 0
    || groups.completed.length > 0;
}

export function buildActiveAgentSummary(groups: ActiveAgentGroups): ActiveAgentSummary {
  const activeCount = groups.running.length;
  const attentionCount = groups.attention.length;
  const completedCount = groups.completed.length;
  return {
    activeCount,
    attentionCount,
    completedCount,
    triggerCount: activeCount + attentionCount + completedCount,
  };
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
    return `${totalMinutes}m ${String(seconds % 60).padStart(2, '0')}s`;
  }
  const hours = Math.floor(totalMinutes / 60);
  return `${hours}h ${String(totalMinutes % 60).padStart(2, '0')}m`;
}

export function shouldPinAgentProjection(
  rootTaskId: string,
  agentCount: number,
  dismissedRootTaskId: string,
): boolean {
  const normalizedRootTaskId = rootTaskId.trim();
  return normalizedRootTaskId.length > 0
    && agentCount > 0
    && normalizedRootTaskId !== dismissedRootTaskId.trim();
}
