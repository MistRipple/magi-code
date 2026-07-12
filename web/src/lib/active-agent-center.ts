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

function isAgentRuntimeActive(agent: AgentProjectionDto): boolean {
  return agent.status === 'pending'
    || agent.status === 'running'
    || agent.lifecycle === 'queued'
    || agent.lifecycle === 'running';
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
  const startedAt = Number.isFinite(agent.startedAt) ? Math.max(0, agent.startedAt) : 0;
  if (startedAt <= 0) {
    return 0;
  }
  const terminalUpdatedAt = Number.isFinite(agent.updatedAt)
    ? Math.max(startedAt, agent.updatedAt)
    : startedAt;
  const endAt = isAgentRuntimeActive(agent)
    ? Math.max(startedAt, nowMs)
    : terminalUpdatedAt;
  return Math.max(0, Math.floor((endAt - startedAt) / 1000));
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
