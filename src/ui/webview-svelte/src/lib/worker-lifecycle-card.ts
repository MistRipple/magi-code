import type { Message, WaitForWorkersResult } from '../types/message';
import type { WorkerRuntimeMap, WorkerRuntimeStatus } from './worker-panel-state';
import { selectWorkerRuntime, selectWorkerRuntimeStatus } from './worker-panel-state';
import { normalizeWorkerSlot } from './message-classifier';
import { buildWaitResultFromTaskCardMessage, resolveTaskCardKeyFromMetadata } from './task-card-runtime';

export type CardWorkerStatus = 'pending' | 'running' | 'completed' | 'failed' | 'cancelled' | 'skipped';

export interface WorkerTaskCardEvidence {
  commandsRun?: number;
  testsPassed?: boolean;
  typeCheckPassed?: boolean;
  filesChanged?: number;
}

export interface WorkerLaneTaskItem {
  taskId: string;
  title: string;
  status: 'pending' | 'waiting_deps' | 'running' | 'completed' | 'failed' | 'skipped' | 'cancelled';
  dependsOn?: string[];
  isCurrent?: boolean;
}

export interface WorkerLaneTaskCardSnapshot {
  taskId: string;
  title: string;
  worker?: string;
  status: 'pending' | 'waiting_deps' | 'running' | 'completed' | 'failed' | 'skipped' | 'cancelled';
  summary?: string;
  fullSummary?: string;
  error?: string;
  failureCode?: string;
  recoverable?: boolean;
  modifiedFiles?: string[];
  createdFiles?: string[];
  duration?: number;
}

export interface WorkerTaskCardData {
  title?: string;
  instruction?: string;
  summary?: string;
  fullSummary?: string;
  status?: CardWorkerStatus;
  description?: string;
  executor?: string;
  agent?: string;
  worker?: string;
  duration?: number | string;
  startedAt?: number;
  changes?: string[];
  modifiedFiles?: string[];
  createdFiles?: string[];
  verification?: string[];
  error?: string;
  failureCode?: string;
  recoverable?: boolean;
  toolCount?: number;
  sessionId?: string;
  isResumed?: boolean;
  evidence?: WorkerTaskCardEvidence;
  dispatchWaveId?: string;
  laneId?: string;
  workerCardId?: string;
  waveIndex?: number;
  laneTasks?: WorkerLaneTaskItem[];
  laneTaskCards?: WorkerLaneTaskCardSnapshot[];
  laneCurrentTaskId?: string;
  laneIndex?: number;
  laneTotal?: number;
}

export interface WorkerLifecycleCardViewModel {
  card: WorkerTaskCardData;
  startedAtOverride?: number;
  runtimeStatus?: WorkerRuntimeStatus;
  waitResult: WaitForWorkersResult | null;
  showWaitReport: boolean;
}

type WorkerWaitResultMap = Record<string, WaitForWorkersResult | null>;

function normalizeTaskText(value: unknown): string {
  if (typeof value !== 'string') return '';
  return value.replace(/^#{1,6}\s+/gm, '').trim();
}

function resolveLaneCurrentTaskTitle(metadata: Record<string, unknown> | undefined): string {
  const currentTaskId = typeof metadata?.laneCurrentTaskId === 'string'
    ? metadata.laneCurrentTaskId.trim()
    : '';
  if (!currentTaskId || !Array.isArray(metadata?.laneTasks)) {
    return '';
  }

  const currentTask = metadata.laneTasks.find((item) => (
    !!item
    && typeof item === 'object'
    && typeof (item as { taskId?: unknown }).taskId === 'string'
    && ((item as { taskId: string }).taskId.trim() === currentTaskId)
  )) as { title?: unknown } | undefined;

  return normalizeTaskText(currentTask?.title);
}

function resolveLaneTasks(metadata: Record<string, unknown> | undefined): WorkerLaneTaskItem[] {
  if (!Array.isArray(metadata?.laneTasks)) {
    return [];
  }
  return metadata.laneTasks
    .filter((item): item is Record<string, unknown> => Boolean(item && typeof item === 'object'))
    .map((item) => {
      const taskId = typeof item.taskId === 'string' ? item.taskId.trim() : '';
      const title = typeof item.title === 'string' ? normalizeTaskText(item.title) : '';
      const status = typeof item.status === 'string' ? item.status : '';
      if (!taskId || !title || !status) {
        return null;
      }
      return {
        taskId,
        title,
        status: status as WorkerLaneTaskItem['status'],
        ...(Array.isArray(item.dependsOn)
          ? {
              dependsOn: item.dependsOn.filter((value): value is string => typeof value === 'string' && value.trim().length > 0),
            }
          : {}),
        ...(item.isCurrent === true ? { isCurrent: true } : {}),
      };
    })
    .filter((item): item is WorkerLaneTaskItem => Boolean(item));
}

function resolveLaneTaskCards(metadata: Record<string, unknown> | undefined): WorkerLaneTaskCardSnapshot[] {
  if (!Array.isArray(metadata?.laneTaskCards)) {
    return [];
  }
  return metadata.laneTaskCards
    .filter((item): item is Record<string, unknown> => Boolean(item && typeof item === 'object'))
    .map((item) => {
      const taskId = typeof item.taskId === 'string' ? item.taskId.trim() : '';
      const title = typeof item.title === 'string' ? normalizeTaskText(item.title) : '';
      const status = typeof item.status === 'string' ? item.status : '';
      if (!taskId || !title || !status) {
        return null;
      }
      return {
        taskId,
        title,
        status: status as WorkerLaneTaskCardSnapshot['status'],
        ...(typeof item.worker === 'string' && item.worker.trim()
          ? { worker: item.worker.trim() }
          : {}),
        ...(typeof item.summary === 'string' && item.summary.trim()
          ? { summary: item.summary }
          : {}),
        ...(typeof item.fullSummary === 'string' && item.fullSummary.trim()
          ? { fullSummary: item.fullSummary }
          : {}),
        ...(typeof item.error === 'string' && item.error.trim()
          ? { error: item.error }
          : {}),
        ...(typeof item.failureCode === 'string' && item.failureCode.trim()
          ? { failureCode: item.failureCode.trim() }
          : {}),
        ...(typeof item.recoverable === 'boolean'
          ? { recoverable: item.recoverable }
          : {}),
        ...(Array.isArray(item.modifiedFiles)
          ? {
              modifiedFiles: item.modifiedFiles
                .filter((file): file is string => typeof file === 'string' && file.trim().length > 0)
                .map((file) => file.trim()),
            }
          : {}),
        ...(Array.isArray(item.createdFiles)
          ? {
              createdFiles: item.createdFiles
                .filter((file): file is string => typeof file === 'string' && file.trim().length > 0)
                .map((file) => file.trim()),
            }
          : {}),
        ...(typeof item.duration === 'number' && Number.isFinite(item.duration) && item.duration >= 0
          ? { duration: Math.floor(item.duration) }
          : {}),
      };
    })
    .filter((item): item is WorkerLaneTaskCardSnapshot => Boolean(item));
}

function orderLaneTaskCards(
  laneTasks: WorkerLaneTaskItem[],
  laneTaskCards: WorkerLaneTaskCardSnapshot[],
): WorkerLaneTaskCardSnapshot[] {
  if (laneTaskCards.length === 0) {
    return [];
  }
  const byTaskId = new Map<string, WorkerLaneTaskCardSnapshot>();
  for (const item of laneTaskCards) {
    byTaskId.set(item.taskId, item);
  }
  const ordered: WorkerLaneTaskCardSnapshot[] = [];
  for (const task of laneTasks) {
    const matched = byTaskId.get(task.taskId);
    if (!matched) {
      continue;
    }
    ordered.push(matched);
    byTaskId.delete(task.taskId);
  }
  for (const item of laneTaskCards) {
    if (byTaskId.has(item.taskId)) {
      ordered.push(item);
      byTaskId.delete(item.taskId);
    }
  }
  return ordered;
}

function buildLaneTaskSummary(
  laneTasks: WorkerLaneTaskItem[],
  laneTaskCards: WorkerLaneTaskCardSnapshot[],
  options: { preferFullSummary?: boolean } = {},
): string {
  const orderedCards = orderLaneTaskCards(laneTasks, laneTaskCards);
  const sections: string[] = [];

  for (const item of orderedCards) {
    const detail = normalizeTaskText(
      (options.preferFullSummary ? item.fullSummary : '')
      || item.summary
      || item.error
      || '',
    );
    if (!detail) {
      continue;
    }
    sections.push(`## ${item.title}\n${detail}`);
  }

  return sections.join('\n\n').trim();
}

function mergeLaneFiles(
  laneTaskCards: WorkerLaneTaskCardSnapshot[],
  field: 'modifiedFiles' | 'createdFiles',
): string[] {
  return Array.from(new Set(
    laneTaskCards.flatMap((item) => Array.isArray(item[field]) ? item[field] : []),
  ));
}

function mapLifecycleStatus(status?: string): CardWorkerStatus | undefined {
  switch (status) {
    case 'running':
    case 'in_progress':
      return 'running';
    case 'pending':
    case 'blocked':
    case 'paused':
    case 'waiting_deps':
      return 'pending';
    case 'completed':
      return 'completed';
    case 'failed':
      return 'failed';
    case 'skipped':
      return 'skipped';
    case 'cancelled':
      return 'cancelled';
    default:
      return undefined;
  }
}

function mapRuntimeStatusToCard(status?: WorkerRuntimeStatus): CardWorkerStatus | undefined {
  switch (status) {
    case 'pending':
      return 'pending';
    case 'running':
      return 'running';
    case 'blocked':
      return 'pending';
    case 'failed':
      return 'failed';
    case 'completed':
      return 'completed';
    case 'cancelled':
      return 'cancelled';
    default:
      return undefined;
  }
}

function isTerminalCardStatus(status?: CardWorkerStatus): boolean {
  return status === 'completed'
    || status === 'failed'
    || status === 'skipped'
    || status === 'cancelled';
}

/**
 * 从 instruction 消息的 laneTasks 计算 lane 整体状态。
 *
 * 当 instruction + task_card 被投影合并后，subTaskCard.status 只反映最后一个
 * 单任务的状态（可能是 completed），但 laneTasks 包含 lane 内所有任务的真实状态。
 * 如果 lane 中还有 running/pending 任务，整体状态不应显示为 completed。
 *
 * 优先级：running > blocked > failed > pending > cancelled > completed
 */
function resolveLaneOverallStatus(metadata: Record<string, unknown>): CardWorkerStatus | undefined {
  const laneTasks = metadata.laneTasks;
  if (!Array.isArray(laneTasks) || laneTasks.length === 0) return undefined;

  let hasRunning = false;
  let hasBlocked = false;
  let hasFailed = false;
  let hasPending = false;
  let hasCancelled = false;
  let hasCompleted = false;

  for (const task of laneTasks) {
    if (!task || typeof task !== 'object') continue;
    const status = (task as { status?: unknown }).status;
    if (typeof status !== 'string') continue;
    switch (status) {
      case 'running':
      case 'in_progress':
        hasRunning = true;
        break;
      case 'blocked':
        hasBlocked = true;
        break;
      case 'failed':
        hasFailed = true;
        break;
      case 'pending':
      case 'paused':
      case 'waiting_deps':
        hasPending = true;
        break;
      case 'cancelled':
        hasCancelled = true;
        break;
      case 'completed':
      case 'skipped':
        hasCompleted = true;
        break;
    }
  }

  if (hasRunning) return 'running';
  if (hasBlocked) return 'pending';
  if (hasFailed) return 'failed';
  if (hasPending) return 'pending';
  if (hasCancelled) return 'cancelled';
  if (hasCompleted) return 'completed';
  return undefined;
}

export function deriveWorkerLifecycleCardViewModel({
  message,
  workerRuntimeMap,
  workerWaitResults,
}: {
  message: Message;
  workerRuntimeMap?: Partial<WorkerRuntimeMap>;
  workerWaitResults?: WorkerWaitResultMap;
}): WorkerLifecycleCardViewModel | null {
  if (message.type !== 'instruction' && message.type !== 'task_card') {
    return null;
  }

  const metadata = (message.metadata || {}) as Record<string, unknown>;
  const mergedSubTaskCard = metadata.subTaskCard && typeof metadata.subTaskCard === 'object'
    ? metadata.subTaskCard as Record<string, unknown>
    : null;
  const instructionWorkerName = normalizeWorkerSlot(metadata.worker || metadata.agent);
  const instructionTargetWorker = typeof metadata.worker === 'string'
    ? metadata.worker
    : (typeof metadata.agent === 'string' ? metadata.agent : undefined);
  const explicitDescription = typeof metadata.description === 'string'
    ? normalizeTaskText(metadata.description)
    : '';
  const laneTasks = resolveLaneTasks(metadata);
  const laneTaskCards = resolveLaneTaskCards(metadata);
  const isMultiTaskLane = laneTasks.length > 1 || laneTaskCards.length > 1;
  const laneSummary = buildLaneTaskSummary(laneTasks, laneTaskCards);
  const laneFullSummary = buildLaneTaskSummary(laneTasks, laneTaskCards, { preferFullSummary: true });
  const laneModifiedFiles = mergeLaneFiles(laneTaskCards, 'modifiedFiles');
  const laneCreatedFiles = mergeLaneFiles(laneTaskCards, 'createdFiles');
  const laneFailureCard = laneTaskCards.find((item) => item.status === 'failed' && typeof item.error === 'string' && item.error.trim());
  const laneCurrentTaskTitle = resolveLaneCurrentTaskTitle(metadata);
  const messageContent = normalizeTaskText(message.content || '');
  const instructionBodyText = explicitDescription
    || messageContent
    || laneCurrentTaskTitle
    || (typeof mergedSubTaskCard?.title === 'string' ? normalizeTaskText(mergedSubTaskCard.title) : '');
  const cardDescription = typeof mergedSubTaskCard?.description === 'string'
    ? normalizeTaskText(mergedSubTaskCard.description)
    : '';
  const cardInstruction = typeof mergedSubTaskCard?.instruction === 'string'
    ? normalizeTaskText(mergedSubTaskCard.instruction)
    : '';
  const instruction = explicitDescription
    || cardDescription
    || cardInstruction
    || (message.type === 'instruction' ? instructionBodyText : '')
    || laneCurrentTaskTitle
    || (typeof mergedSubTaskCard?.title === 'string' ? normalizeTaskText(mergedSubTaskCard.title) : '');
  const baseSummary = typeof mergedSubTaskCard?.summary === 'string' ? mergedSubTaskCard.summary : undefined;
  const baseFullSummary = typeof mergedSubTaskCard?.fullSummary === 'string' ? mergedSubTaskCard.fullSummary : undefined;
  const aggregatedSummary = isMultiTaskLane ? (laneSummary || baseSummary) : baseSummary;
  const aggregatedFullSummary = isMultiTaskLane ? (laneFullSummary || aggregatedSummary) : baseFullSummary;

  const card: WorkerTaskCardData = {
    title: !isMultiTaskLane && typeof mergedSubTaskCard?.title === 'string'
      ? normalizeTaskText(mergedSubTaskCard.title)
      : (instruction || undefined),
    instruction,
    summary: aggregatedSummary,
    fullSummary: aggregatedFullSummary,
    description: instruction,
    worker: instructionTargetWorker
      || (typeof metadata.assignedWorker === 'string' ? metadata.assignedWorker : undefined)
      || (typeof mergedSubTaskCard?.worker === 'string' ? mergedSubTaskCard.worker : undefined),
    modifiedFiles: laneModifiedFiles.length > 0
      ? laneModifiedFiles
      : (Array.isArray(mergedSubTaskCard?.modifiedFiles)
          ? mergedSubTaskCard.modifiedFiles as string[]
          : undefined),
    createdFiles: laneCreatedFiles.length > 0
      ? laneCreatedFiles
      : (Array.isArray(mergedSubTaskCard?.createdFiles)
          ? mergedSubTaskCard.createdFiles as string[]
          : undefined),
    duration: typeof mergedSubTaskCard?.duration === 'number'
      ? mergedSubTaskCard.duration
      : undefined,
    error: isMultiTaskLane
      ? laneFailureCard?.error
      : (typeof mergedSubTaskCard?.error === 'string' ? mergedSubTaskCard.error : undefined),
    failureCode: isMultiTaskLane
      ? laneFailureCard?.failureCode
      : (typeof mergedSubTaskCard?.failureCode === 'string' ? mergedSubTaskCard.failureCode : undefined),
    dispatchWaveId: typeof metadata.dispatchWaveId === 'string' ? metadata.dispatchWaveId : undefined,
    laneId: typeof metadata.laneId === 'string' ? metadata.laneId : undefined,
    workerCardId: typeof metadata.workerCardId === 'string' ? metadata.workerCardId : undefined,
    laneTasks,
    laneTaskCards,
    laneCurrentTaskId: typeof metadata.laneCurrentTaskId === 'string' ? metadata.laneCurrentTaskId : undefined,
    laneIndex: typeof metadata.laneIndex === 'number' ? metadata.laneIndex : undefined,
    laneTotal: typeof metadata.laneTotal === 'number' ? metadata.laneTotal : undefined,
  };

  const rawWorker = (mergedSubTaskCard as { worker?: unknown } | null)?.worker
    || metadata.assignedWorker
    || instructionWorkerName
    || metadata.worker;
  const cardWorker = normalizeWorkerSlot(rawWorker);
  const runtimeState = cardWorker ? selectWorkerRuntime(workerRuntimeMap, cardWorker) : null;
  const runtimeStatus = cardWorker ? selectWorkerRuntimeStatus(workerRuntimeMap, cardWorker) : undefined;
  const cardKey = resolveTaskCardKeyFromMetadata(metadata);
  const persistedTaskCardWaitResult = buildWaitResultFromTaskCardMessage(message)?.result || null;
  const waitResult = (cardKey && workerWaitResults?.[cardKey]) || persistedTaskCardWaitResult;
  const metadataCard = mergedSubTaskCard as { status?: string } | null;
  const metadataCardStatus = mapLifecycleStatus(metadataCard?.status);
  const timelineAnchorTimestamp = typeof metadata.timelineAnchorTimestamp === 'number'
    && Number.isFinite(metadata.timelineAnchorTimestamp)
    && metadata.timelineAnchorTimestamp > 0
    ? Math.floor(metadata.timelineAnchorTimestamp)
    : (typeof mergedSubTaskCard?.timelineAnchorTimestamp === 'number'
      && Number.isFinite(mergedSubTaskCard.timelineAnchorTimestamp)
      && mergedSubTaskCard.timelineAnchorTimestamp > 0
      ? Math.floor(mergedSubTaskCard.timelineAnchorTimestamp)
      : undefined);

  // 卡片状态只保留两层语义：
  // 1) 持久化状态：消息自身携带的结构化快照（instruction 的 laneTasks / task_card 的 subTaskCard.status）
  // 2) 运行时覆盖：workerRuntime.status，仅用于当前仍在执行的卡片
  const laneOverallStatus = message.type === 'instruction'
    ? resolveLaneOverallStatus(metadata)
    : undefined;
  const persistedCardStatus = laneOverallStatus || metadataCardStatus;
  const runtimeCardStatus = mapRuntimeStatusToCard(runtimeStatus);
  const activeRuntimeCard = Boolean(
    runtimeCardStatus
    && !isTerminalCardStatus(persistedCardStatus)
    && (runtimeStatus === 'running' || runtimeStatus === 'pending' || runtimeStatus === 'blocked')
  );

  return {
    card: {
      ...card,
      status: persistedCardStatus,
      summary: activeRuntimeCard ? undefined : card.summary,
      error: activeRuntimeCard ? undefined : card.error,
      failureCode: activeRuntimeCard ? undefined : card.failureCode,
    },
    startedAtOverride: runtimeState?.timerStartAt
      || (message.type === 'instruction' ? (timelineAnchorTimestamp || message.timestamp) : undefined),
    runtimeStatus,
    waitResult,
    showWaitReport: message.type === 'instruction',
  };
}
