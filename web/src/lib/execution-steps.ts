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
