import type { SessionMeta } from '../../session';
import type { TaskView } from '../../task/task-view-adapter';
import type { LocaleCode } from '../../i18n/types';
import type {
  InteractionMode,
  LogEntry,
  PendingChange,
  UIProcessingState,
  UIState,
  WorkerStatus,
} from '../../types';
export interface OrchestrationRuntimeStateQueryInput {
  sessionId?: string | null;
  sessions?: SessionMeta[];
  taskViews?: TaskView[];
  locale?: LocaleCode;
  workerStatuses: WorkerStatus[];
  pendingChanges: PendingChange[];
  isRunning: boolean;
  logs: LogEntry[];
  interactionMode: InteractionMode;
  interactionModeUpdatedAt?: number;
  orchestratorPhase?: string;
  activePlan?: UIState['activePlan'];
  planHistory?: UIState['planHistory'];
  processingState?: UIProcessingState | null;
  stateUpdatedAt?: number;
  recovered?: boolean;
}

type UITask = NonNullable<UIState['tasks']>[number];

function normalizeSessionId(sessionId?: string | null): string | undefined {
  const normalized = typeof sessionId === 'string' ? sessionId.trim() : '';
  return normalized || undefined;
}

export class OrchestrationRuntimeQueryService {
  queryState(input: OrchestrationRuntimeStateQueryInput): UIState {
    const tasks = (Array.isArray(input.taskViews) ? input.taskViews : [])
      .map((taskView) => this.toUITask(taskView))
      .sort((left, right) => {
        const leftTimestamp = Number(left?.startedAt || left?.createdAt || 0);
        const rightTimestamp = Number(right?.startedAt || right?.createdAt || 0);
        return rightTimestamp - leftTimestamp;
      });

    return {
      currentSessionId: normalizeSessionId(input.sessionId),
      sessions: Array.isArray(input.sessions) ? [...input.sessions] : [],
      currentTask: tasks.find((task) => task?.status === 'running') ?? tasks[0],
      tasks,
      locale: input.locale,
      activePlan: input.activePlan,
      planHistory: input.planHistory,
      workerStatuses: [...input.workerStatuses],
      pendingChanges: [...input.pendingChanges],
      isRunning: input.isRunning,
      logs: [...input.logs],
      interactionMode: input.interactionMode,
      interactionModeUpdatedAt: input.interactionModeUpdatedAt,
      orchestratorPhase: input.orchestratorPhase,
      processingState: input.processingState
        ? {
            ...input.processingState,
            pendingRequestIds: [...input.processingState.pendingRequestIds],
          }
        : null,
      stateUpdatedAt: input.stateUpdatedAt ?? Date.now(),
      recovered: input.recovered ?? false,
    };
  }

  private toUITask(taskView: TaskView): UITask {
    return {
      id: taskView.id,
      name: taskView.title || taskView.goal || taskView.prompt,
      prompt: taskView.prompt,
      description: taskView.goal,
      status: taskView.status,
      priority: taskView.priority,
      deliveryStatus: taskView.deliveryStatus,
      deliverySummary: taskView.deliverySummary,
      deliveryDetails: taskView.deliveryDetails,
      deliveryWarnings: taskView.deliveryWarnings,
      subTasks: taskView.subTasks,
      createdAt: taskView.createdAt,
      startedAt: taskView.startedAt,
      completedAt: taskView.completedAt,
      progress: taskView.progress,
      missionId: taskView.missionId,
      failureReason: taskView.failureReason,
    } as unknown as UITask;
  }
}
