import type { WorkerLaneStatus } from '../types/message';

export type DispatchPresentationStepKind = 'planning' | 'batch' | 'delivery' | 'stage';

export interface DispatchPresentationStageInput {
  key: string;
  title: string;
  status: WorkerLaneStatus;
}

export interface DispatchPresentationStep<TStage extends DispatchPresentationStageInput> {
  key: string;
  kind: DispatchPresentationStepKind;
  title: string;
  batchLabel?: string;
  displayIndex: number;
  status: WorkerLaneStatus;
  stages: TStage[];
  totalStageCount: number;
  completedStageCount: number;
}

const CHINESE_BATCH_PATTERN = /第?\s*([一二三四五六七八九十百千万两0-9]+)\s*批/;
const ENGLISH_BATCH_PATTERN = /\bbatch\s*([a-z0-9]+)\b/i;

function normalizeText(value: string): string {
  return value.trim().replace(/\s+/g, ' ');
}

function extractBatchLabel(title: string): string {
  const normalized = normalizeText(title);
  const chineseMatch = normalized.match(CHINESE_BATCH_PATTERN);
  if (chineseMatch?.[1]) {
    return `第${chineseMatch[1]}批`;
  }
  const englishMatch = normalized.match(ENGLISH_BATCH_PATTERN);
  if (englishMatch?.[1]) {
    return `Batch ${englishMatch[1].toUpperCase()}`;
  }
  return '';
}

function isPlanningTitle(title: string): boolean {
  return /规划|计划|拆解|分析|梳理目标|任务理解|需求确认/.test(title);
}

function isDeliveryTitle(title: string): boolean {
  return /交付|总结|收口|最终|完成/.test(title);
}

function normalizeStepTitle(title: string): string {
  const normalized = normalizeText(title) || '执行步骤';
  return normalized
    .replace(/\s+(验证|校验|复核)$/u, '$1')
    .replace(/^(验证|校验|复核)\s+/u, '$1');
}

function resolveStepPresentation(title: string): {
  kind: DispatchPresentationStepKind;
  title: string;
  batchLabel?: string;
} {
  const batchLabel = extractBatchLabel(title);
  if (batchLabel) {
    return {
      kind: 'batch',
      title: normalizeStepTitle(title),
      batchLabel,
    };
  }
  if (isPlanningTitle(title)) {
    return { kind: 'planning', title: normalizeStepTitle(title) };
  }
  if (isDeliveryTitle(title)) {
    return { kind: 'delivery', title: normalizeStepTitle(title) };
  }
  return {
    kind: 'stage',
    title: normalizeStepTitle(title),
  };
}

export function buildDispatchPresentationSteps<TStage extends DispatchPresentationStageInput>(
  stages: TStage[],
): DispatchPresentationStep<TStage>[] {
  return stages.map((stage, index) => {
    const presentation = resolveStepPresentation(stage.title);
    return {
      key: stage.key || `dispatch-step-${index + 1}`,
      kind: presentation.kind,
      title: presentation.title,
      batchLabel: presentation.batchLabel,
      displayIndex: index + 1,
      status: stage.status,
      stages: [stage],
      totalStageCount: 1,
      completedStageCount: stage.status === 'completed' ? 1 : 0,
    };
  });
}

export function countDispatchBatchSteps(
  steps: Array<Pick<DispatchPresentationStep<DispatchPresentationStageInput>, 'kind' | 'title' | 'batchLabel'>>,
): number {
  const batchLabels = new Set<string>();
  for (const step of steps) {
    if (step.kind !== 'batch') continue;
    batchLabels.add(step.batchLabel || extractBatchLabel(step.title) || step.title);
  }
  return batchLabels.size;
}
