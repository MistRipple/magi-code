import type { AgentRunProjectionDto } from '../shared/rust-backend-types';
import type { ToolCallStatus } from '../types/message';

export type AgentChildStatus = 'queued' | 'running' | 'completed' | 'failed' | 'killed' | 'degraded';
export interface AgentChildState { status: AgentChildStatus; updatedAt: number; }

export function normalizeAgentChildStatus(value: unknown): AgentChildStatus | undefined {
  if (typeof value !== 'string') return undefined;
  switch (value.trim().toLowerCase()) {
    case 'pending': case 'queued': return 'queued';
    case 'running': case 'started': return 'running';
    case 'completed': case 'succeeded': case 'success': return 'completed';
    case 'failed': case 'missing': return 'failed';
    case 'killed': case 'cancelled': case 'canceled': return 'killed';
    case 'degraded': case 'blocked': return 'degraded';
    default: return undefined;
  }
}

export function agentChildVisualStatus(status: AgentChildStatus | undefined): ToolCallStatus | 'degraded' | undefined {
  switch (status) {
    case 'queued': return 'pending';
    case 'completed': return 'success';
    case 'failed': case 'killed': return 'error';
    default: return status;
  }
}

/** 当前 TaskStore 投影与 canonical 执行事实按提交时间取新值，派发回执不参与。 */
export function resolveAgentChildStatus(
  projection: AgentRunProjectionDto | null,
  taskId: string,
  canonicalStatus: unknown,
  canonicalUpdatedAt: unknown,
): AgentChildStatus | undefined {
  const task = projection?.root_task.task_id === taskId
    ? projection.root_task
    : projection?.tasks.find((task) => task.task_id === taskId);
  const agent = projection?.agents.find((agent) => agent.agentRunId === taskId);
  const projected = agent?.lifecycle === 'degraded'
    ? 'degraded'
    : normalizeAgentChildStatus(task?.status ?? agent?.status);
  const canonical = normalizeAgentChildStatus(canonicalStatus);
  if (canonical && (!projected || (typeof canonicalUpdatedAt === 'number'
    && canonicalUpdatedAt >= (task?.updated_at ?? agent?.updatedAt ?? 0)))) return canonical;
  return projected ?? canonical;
}
