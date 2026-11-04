import type { ExecutionSteps, ExecutionStepStatus } from '../shared/app-server-protocol.generated';
import type { CanonicalTurnItem } from '../shared/protocol/canonical-turn';
const statuses: ExecutionStepStatus[] = ['pending', 'running', 'completed', 'blocked', 'failed', 'canceled'];
function record(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === 'object' && !Array.isArray(value);
}
function strings(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((v) => typeof v === 'string' && v.trim()) && new Set(value).size === value.length;
}
/** 只消费 daemon 步骤事实；不能根据工具名称或 Turn 终态推断步骤结果。 */
export function readExecutionSteps(value: unknown): ExecutionSteps | null {
  if (!record(value) || typeof value.executionId !== 'string' || !value.executionId
    || typeof value.turnId !== 'string' || !value.turnId || typeof value.taskId !== 'string' || !value.taskId
    || !Number.isSafeInteger(value.revision) || Number(value.revision) < 1
    || !Number.isSafeInteger(value.createdAt) || !Number.isSafeInteger(value.updatedAt)
    || !Array.isArray(value.steps) || value.steps.length < 2 || value.steps.length > 12) return null;
  const ids = new Set<string>();
  for (const step of value.steps) {
    if (!record(step) || typeof step.stepId !== 'string' || !step.stepId || ids.has(step.stepId)
      || typeof step.title !== 'string' || !step.title.trim() || !statuses.includes(step.status as ExecutionStepStatus)
      || !strings(step.dependsOn) || !strings(step.evidenceRefs) || !strings(step.taskIds)
      || (step.reason !== undefined && typeof step.reason !== 'string')) return null;
    ids.add(step.stepId);
  }
  return value as unknown as ExecutionSteps;
}
export function executionStepEvidence(snapshot: ExecutionSteps, stepId: string, items: readonly CanonicalTurnItem[]): CanonicalTurnItem[] {
  const step = snapshot.steps.find((step) => step.stepId === stepId);
  if (!step) return [];
  return items.filter((item) => item.turnId === snapshot.turnId
    && item.worker?.taskId === snapshot.taskId
    && !item.metadata?.executionSteps
    && (item.metadata?.executionStepId === stepId || step.evidenceRefs.includes(item.itemId) || step.evidenceRefs.includes(item.tool?.callId || '')));
}

/** 仍需要用户关注的快照：有步骤在执行、待执行或受阻；全部终结后不再置底悬浮。 */
export function isExecutionStepsActive(snapshot: ExecutionSteps): boolean {
  return snapshot.steps.some((step) => step.status === 'running' || step.status === 'pending' || step.status === 'blocked');
}
/** 置底条显示的当前步骤：优先执行中，其次受阻/失败，最后第一个待执行。 */
export function currentExecutionStep(snapshot: ExecutionSteps) {
  return snapshot.steps.find((step) => step.status === 'running')
    ?? snapshot.steps.find((step) => step.status === 'blocked' || step.status === 'failed')
    ?? snapshot.steps.find((step) => step.status === 'pending')
    ?? null;
}

/**
 * 选出需要置底悬浮的步骤快照：只看时间线里最新的一份，且必须属于当前运行轮次并仍有活动步骤。
 * 最新一份已全部终结时不回头选更早的快照，否则旧计划会重新浮起。
 */
export function selectPinnedExecutionSteps<T extends { message: { metadata?: Record<string, unknown> } }>(
  items: readonly T[],
  runtimeTurnId: string,
): { item: T; snapshot: ExecutionSteps } | null {
  for (let index = items.length - 1; index >= 0; index -= 1) {
    const snapshot = readExecutionSteps(items[index].message.metadata?.executionSteps);
    if (!snapshot) continue;
    if (runtimeTurnId && snapshot.turnId !== runtimeTurnId) return null;
    return isExecutionStepsActive(snapshot) ? { item: items[index], snapshot } : null;
  }
  return null;
}
