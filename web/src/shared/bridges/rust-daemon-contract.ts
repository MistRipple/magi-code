import type { SessionBootstrapSnapshot } from '../session-bootstrap';
import type {
  AppState,
  ContentBlock,
  Message,
  OrchestrationRuntimeAssignmentSummary,
  OrchestratorRuntimeState,
  Session,
  SessionNotificationRecord,
  SubTaskItem,
  SessionTimelineProjection,
  Task,
  TimelineProjectionArtifact,
} from '../../types/message';
import { buildEmptyWorkspaceAppState } from './empty-workspace-state';

export type BootstrapPayload = SessionBootstrapSnapshot & {
  agent?: {
    runtimeEpoch?: string;
  };
  workspace: {
    workspaceId: string;
    name: string;
    rootPath: string;
  };
};

export interface RustEventEnvelope {
  event_id?: string;
  event_type?: string;
  category?: string;
  occurred_at?: number;
  sequence?: number;
  workspace_id?: string | null;
  session_id?: string | null;
  mission_id?: string | null;
  assignment_id?: string | null;
  task_id?: string | null;
  payload?: Record<string, unknown> | null;
}

interface RustBootstrapSessionRecord {
  session_id?: string;
  title?: string | null;
  created_at?: number;
  updated_at?: number;
}

interface RustBootstrapWorkspaceRecord {
  workspace_id?: string;
  root_path?: string;
}

interface RustNotificationRecord {
  notification_id?: string;
  session_id?: string;
  kind?: string;
  message?: string;
  created_at?: number;
  handled?: boolean;
}

interface RustTimelineEntry {
  entry_id?: string;
  session_id?: string;
  kind?: string;
  message?: string;
  occurred_at?: number;
}

interface RustMissionRuntimeSummary {
  mission_id?: string;
  latest_event_type?: string | null;
  current_status?: string | null;
  active_task_ids?: string[];
}

interface RustAssignmentRuntimeSummary {
  assignment_id?: string;
  mission_id?: string | null;
  completed_task_count?: number;
  failed_task_count?: number;
  task_ids?: string[];
  latest_event_type?: string | null;
  current_status?: string | null;
}

interface RustTaskRuntimeSummary {
  task_id?: string;
  mission_id?: string | null;
  assignment_id?: string | null;
  latest_event_type?: string | null;
  current_status?: string | null;
  failed_dispatch_count?: number;
}

interface RustSessionRuntimeSummary {
  session_id?: string;
  active_mission_ids?: string[];
  active_task_ids?: string[];
  recovery_ids?: string[];
  latest_event_type?: string | null;
  current_status?: string | null;
  last_update?: number | null;
  execution_chain_ref?: string | null;
  recovery_ref?: string | null;
}

interface RustRuntimeReadModelDto {
  meta?: {
    latest_sequence?: number;
  };
  details?: {
    missions?: RustMissionRuntimeSummary[];
    assignments?: RustAssignmentRuntimeSummary[];
    tasks?: RustTaskRuntimeSummary[];
    sessions?: RustSessionRuntimeSummary[];
  };
  overview?: {
    activity?: {
      active_mission_ids?: string[];
      active_task_ids?: string[];
    };
  };
  operations?: {
    attention?: {
      failed_mission_ids?: string[];
      failed_task_ids?: string[];
      pending_recovery_ids?: string[];
    };
    work_queues?: {
      running_mission_ids?: string[];
      running_task_ids?: string[];
      pending_recovery_ids?: string[];
    };
  };
  recovery?: {
    active_recovery_ids?: string[];
  };
}

interface RustBootstrapDto {
  generated_at?: number;
  current_session?: RustBootstrapSessionRecord | null;
  sessions?: RustBootstrapSessionRecord[];
  timeline?: RustTimelineEntry[];
  workspaces?: RustBootstrapWorkspaceRecord[];
  runtime_read_model?: RustRuntimeReadModelDto;
  notifications?: RustNotificationRecord[];
  recent_events?: RustEventEnvelope[];
  agent?: {
    runtimeEpoch?: string;
  };
}

interface NormalizedNotificationEntry {
  sessionId: string;
  record: SessionNotificationRecord;
}

function normalizeString(value: unknown): string {
  return typeof value === 'string' ? value.trim() : '';
}

function normalizeNumber(value: unknown, fallback: number): number {
  return typeof value === 'number' && Number.isFinite(value) ? Math.floor(value) : fallback;
}

function normalizeStringArray(value: unknown): string[] {
  return Array.isArray(value)
    ? value.map((item) => normalizeString(item)).filter((item) => item.length > 0)
    : [];
}

function normalizeObjectRecord(value: unknown): Record<string, unknown> | null {
  return value && typeof value === 'object' && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

function getRuntimeDetailEntries(
  runtimeReadModel: RustRuntimeReadModelDto | undefined,
  key: string,
): unknown[] {
  const details = normalizeObjectRecord(runtimeReadModel?.details);
  const entries = details?.[key];
  return Array.isArray(entries) ? entries : [];
}

function normalizeAssignmentRuntimeEntries(
  runtimeReadModel: RustRuntimeReadModelDto | undefined,
): RustAssignmentRuntimeSummary[] {
  return getRuntimeDetailEntries(runtimeReadModel, 'assignments')
    .map((entry) => normalizeObjectRecord(entry))
    .filter((entry): entry is Record<string, unknown> => entry !== null)
    .map((entry) => ({
      assignment_id: normalizeString(entry.assignment_id) || undefined,
      mission_id: normalizeString(entry.mission_id) || undefined,
      completed_task_count: typeof entry.completed_task_count === 'number'
        ? Math.floor(entry.completed_task_count)
        : undefined,
      failed_task_count: typeof entry.failed_task_count === 'number'
        ? Math.floor(entry.failed_task_count)
        : undefined,
      task_ids: normalizeStringArray(entry.task_ids),
      latest_event_type: normalizeString(entry.latest_event_type) || undefined,
      current_status: normalizeString(entry.current_status) || undefined,
    }));
}

function normalizeTaskRuntimeEntries(
  runtimeReadModel: RustRuntimeReadModelDto | undefined,
): RustTaskRuntimeSummary[] {
  const taskEntries = getRuntimeDetailEntries(runtimeReadModel, 'tasks');
  const legacyTaskEntries = taskEntries.length > 0 ? [] : getRuntimeDetailEntries(
    runtimeReadModel,
    'todos',
  );
  const sourceEntries = taskEntries.length > 0 ? taskEntries : legacyTaskEntries;
  return sourceEntries
    .map((entry) => normalizeObjectRecord(entry))
    .filter((entry): entry is Record<string, unknown> => entry !== null)
    .map((entry) => ({
      task_id: normalizeString(entry.task_id) || undefined,
      mission_id: normalizeString(entry.mission_id) || undefined,
      assignment_id: normalizeString(entry.assignment_id) || undefined,
      latest_event_type: normalizeString(entry.latest_event_type) || undefined,
      current_status: normalizeString(entry.current_status) || undefined,
      failed_dispatch_count: typeof entry.failed_dispatch_count === 'number'
        ? Math.floor(entry.failed_dispatch_count)
        : undefined,
    }));
}

function normalizeSessionRuntimeEntries(
  runtimeReadModel: RustRuntimeReadModelDto | undefined,
): RustSessionRuntimeSummary[] {
  return getRuntimeDetailEntries(runtimeReadModel, 'sessions')
    .map((entry) => normalizeObjectRecord(entry))
    .filter((entry): entry is Record<string, unknown> => entry !== null)
    .map((entry) => ({
      session_id: normalizeString(entry.session_id) || undefined,
      active_mission_ids: normalizeStringArray(entry.active_mission_ids),
      active_task_ids: normalizeStringArray(entry.active_task_ids),
      recovery_ids: normalizeStringArray(entry.recovery_ids),
      latest_event_type: normalizeString(entry.latest_event_type) || undefined,
      current_status: normalizeString(entry.current_status) || undefined,
      last_update: typeof entry.last_update === 'number' ? Math.floor(entry.last_update) : undefined,
      execution_chain_ref: normalizeString(entry.execution_chain_ref) || undefined,
      recovery_ref: normalizeString(entry.recovery_ref) || undefined,
    }));
}

function normalizeEventEnvelope(raw: unknown): RustEventEnvelope | null {
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) {
    return null;
  }
  const record = raw as Record<string, unknown>;
  const eventId = normalizeString(record.event_id);
  const eventType = normalizeString(record.event_type);
  if (!eventId || !eventType) {
    return null;
  }
  return {
    event_id: eventId,
    event_type: eventType,
    category: normalizeString(record.category),
    occurred_at: typeof record.occurred_at === 'number' ? Math.floor(record.occurred_at) : undefined,
    sequence: typeof record.sequence === 'number' ? Math.floor(record.sequence) : undefined,
    workspace_id: normalizeString(record.workspace_id) || undefined,
    session_id: normalizeString(record.session_id) || undefined,
    mission_id: normalizeString(record.mission_id) || undefined,
    assignment_id: normalizeString(record.assignment_id) || undefined,
    task_id: normalizeString(record.task_id) || undefined,
    payload: record.payload && typeof record.payload === 'object' && !Array.isArray(record.payload)
      ? record.payload as Record<string, unknown>
      : null,
  };
}

function normalizeRustSessions(
  payload: RustBootstrapDto,
  generatedAt: number,
): Session[] {
  if (!Array.isArray(payload.sessions)) {
    return [];
  }
  const sessions: Session[] = [];
  for (const session of payload.sessions) {
    const sessionId = normalizeString(session.session_id);
    if (!sessionId) {
      continue;
    }
    const createdAt = normalizeNumber(session.created_at, generatedAt);
    const updatedAt = normalizeNumber(session.updated_at, createdAt);
    sessions.push({
      id: sessionId,
      name: normalizeString(session.title) || undefined,
      createdAt,
      updatedAt,
    });
  }
  return sessions;
}

function deriveBootstrapWorkspaceName(rootPath: string, workspaceId: string): string {
  const fallbackName = rootPath
    .split(/[\\/]/)
    .map((part) => part.trim())
    .filter((part) => part.length > 0)
    .pop();
  return fallbackName || workspaceId || 'workspace';
}

function resolveSelectedWorkspace(
  payload: RustBootstrapDto,
  options: { workspaceId?: string; workspacePath?: string },
): { workspaceId: string; rootPath: string; name: string } {
  const requestedWorkspaceId = normalizeString(options.workspaceId);
  const requestedWorkspacePath = normalizeString(options.workspacePath);
  const workspaces = Array.isArray(payload.workspaces) ? payload.workspaces : [];
  const selectedWorkspace = workspaces.find((workspace) => normalizeString(workspace.workspace_id) === requestedWorkspaceId)
    || workspaces[0]
    || null;
  const workspaceId = requestedWorkspaceId || normalizeString(selectedWorkspace?.workspace_id);
  const rootPath = requestedWorkspacePath || normalizeString(selectedWorkspace?.root_path);
  return {
    workspaceId,
    rootPath,
    name: deriveBootstrapWorkspaceName(rootPath, workspaceId),
  };
}

function buildLookupMaps(events: RustEventEnvelope[]): {
  missionTitles: Map<string, string>;
  assignmentTitles: Map<string, string>;
  taskTitles: Map<string, string>;
  assignmentWorkers: Map<string, string>;
} {
  const missionTitles = new Map<string, string>();
  const assignmentTitles = new Map<string, string>();
  const taskTitles = new Map<string, string>();
  const assignmentWorkers = new Map<string, string>();

  for (const event of events) {
    const payload = event.payload;
    if (!payload) {
      continue;
    }
    const missionId = normalizeString(payload.mission_id) || normalizeString(event.mission_id);
    const assignmentId = normalizeString(payload.assignment_id) || normalizeString(event.assignment_id);
    const taskId = normalizeString(payload.task_id) || normalizeString(event.task_id);
    const missionTitle = normalizeString(payload.mission_title);
    const assignmentTitle = normalizeString(payload.assignment_title);
    const taskTitle = normalizeString(payload.task_title);
    const workerId = normalizeString(payload.worker_id);

    if (missionId && missionTitle && !missionTitles.has(missionId)) {
      missionTitles.set(missionId, missionTitle);
    }
    if (assignmentId && assignmentTitle && !assignmentTitles.has(assignmentId)) {
      assignmentTitles.set(assignmentId, assignmentTitle);
    }
    if (taskId && taskTitle && !taskTitles.has(taskId)) {
      taskTitles.set(taskId, taskTitle);
    }
    if (assignmentId && workerId && !assignmentWorkers.has(assignmentId)) {
      assignmentWorkers.set(assignmentId, workerId);
    }
  }

  return {
    missionTitles,
    assignmentTitles,
    taskTitles,
    assignmentWorkers,
  };
}

function shortenId(value: string, prefix: string): string {
  if (!value) {
    return prefix;
  }
  if (value.length <= 20) {
    return value;
  }
  return `${prefix}-${value.slice(-8)}`;
}

function normalizeTaskStatus(status: string, failedCount = 0): Task['status'] {
  const normalized = status.toLowerCase();
  if (normalized.includes('fail') || normalized.includes('block') || failedCount > 0) {
    return 'failed';
  }
  if (normalized.includes('success') || normalized.includes('complete')) {
    return 'completed';
  }
  if (normalized.includes('run') || normalized.includes('resume') || normalized.includes('execute')) {
    return 'running';
  }
  if (normalized.includes('pause') || normalized.includes('wait')) {
    return 'paused';
  }
  return 'pending';
}

function normalizeSubTaskStatus(status: string, failedDispatchCount = 0): SubTaskItem['status'] {
  const normalized = status.toLowerCase();
  if (normalized.includes('approval')) {
    return 'awaiting_approval';
  }
  if (normalized.includes('review')) {
    return 'review_required';
  }
  if (normalized.includes('block') || failedDispatchCount > 0) {
    return 'blocked';
  }
  if (normalized.includes('fail')) {
    return 'failed';
  }
  if (normalized.includes('success') || normalized.includes('complete')) {
    return 'completed';
  }
  if (normalized.includes('run') || normalized.includes('resume') || normalized.includes('execute')) {
    return 'running';
  }
  if (normalized.includes('pause')) {
    return 'paused';
  }
  if (normalized.includes('wait')) {
    return 'waiting_deps';
  }
  if (normalized.includes('skip')) {
    return 'skipped';
  }
  return 'pending';
}

function buildAssignmentsFromRuntime(
  runtimeReadModel: RustRuntimeReadModelDto | undefined,
  events: RustEventEnvelope[],
): OrchestrationRuntimeAssignmentSummary[] {
  const assignmentEntries = normalizeAssignmentRuntimeEntries(runtimeReadModel);
  const taskEntries = normalizeTaskRuntimeEntries(runtimeReadModel);
  const tasksByAssignment = new Map<string, RustTaskRuntimeSummary[]>();
  for (const task of taskEntries) {
    const assignmentId = normalizeString(task.assignment_id);
    if (!assignmentId) {
      continue;
    }
    const bucket = tasksByAssignment.get(assignmentId) || [];
    bucket.push(task);
    tasksByAssignment.set(assignmentId, bucket);
  }
  const lookups = buildLookupMaps(events);

  const assignments: OrchestrationRuntimeAssignmentSummary[] = [];
  for (const assignment of assignmentEntries) {
    const assignmentId = normalizeString(assignment.assignment_id);
    if (!assignmentId) {
      continue;
    }
    const assignmentTasks = tasksByAssignment.get(assignmentId) || [];
    const assignmentTaskStatuses = assignmentTasks.map((task) => normalizeSubTaskStatus(
      normalizeString(task.current_status),
      typeof task.failed_dispatch_count === 'number' ? task.failed_dispatch_count : 0,
    ));
    const taskTotal = assignmentTasks.length || normalizeStringArray(assignment.task_ids).length;
    const completedTaskCount = assignmentTasks.length > 0
      ? assignmentTaskStatuses.filter((status) => status === 'completed' || status === 'skipped').length
      : normalizeNumber(assignment.completed_task_count, 0);
    const failedTaskCount = assignmentTasks.length > 0
      ? assignmentTaskStatuses.filter((status) => status === 'failed' || status === 'blocked').length
      : normalizeNumber(assignment.failed_task_count, 0);
    const runningTaskCount = assignmentTaskStatuses.filter((status) => status === 'running').length;
    const awaitingApprovalTaskCount = assignmentTaskStatuses.filter((status) => status === 'awaiting_approval').length;
    const reviewRequiredTaskCount = assignmentTaskStatuses.filter((status) => status === 'review_required').length;
    const blockedTaskCount = assignmentTaskStatuses.filter((status) => status === 'blocked').length;
    const progress = taskTotal > 0 ? Math.round((completedTaskCount / taskTotal) * 100) : 0;

    assignments.push({
      assignmentId,
      workerId: lookups.assignmentWorkers.get(assignmentId) || undefined,
      title: lookups.assignmentTitles.get(assignmentId) || shortenId(assignmentId, 'assignment'),
      status: normalizeString(assignment.current_status) || 'pending',
      progress,
      taskTotal,
      awaitingApprovalTaskCount,
      reviewRequiredTaskCount,
      blockedTaskCount,
      completedTaskCount,
      failedTaskCount,
      runningTaskCount,
    });
  }
  return assignments;
}

function deriveProcessingState(
  runtimeReadModel: RustRuntimeReadModelDto | undefined,
  generatedAt: number,
): AppState['processingState'] {
  const runningMissionIds = normalizeStringArray(runtimeReadModel?.operations?.work_queues?.running_mission_ids);
  const runningTaskIds = normalizeStringArray(runtimeReadModel?.operations?.work_queues?.running_task_ids);
  const pendingRecoveryIds = normalizeStringArray(runtimeReadModel?.operations?.work_queues?.pending_recovery_ids);
  const isProcessing = runningMissionIds.length > 0 || runningTaskIds.length > 0 || pendingRecoveryIds.length > 0;

  if (!isProcessing) {
    return null;
  }

  return {
    isProcessing: true,
    source: 'orchestrator',
    agent: 'orchestrator',
    startedAt: generatedAt,
    pendingRequestIds: [],
  };
}

function deriveRuntimeState(
  runtimeReadModel: RustRuntimeReadModelDto | undefined,
  assignments: OrchestrationRuntimeAssignmentSummary[],
  sessionId: string,
  generatedAt: number,
): OrchestratorRuntimeState | null {
  if (!runtimeReadModel) {
    return null;
  }

  const runningMissionIds = normalizeStringArray(runtimeReadModel.operations?.work_queues?.running_mission_ids);
  const pendingRecoveryIds = normalizeStringArray(runtimeReadModel.operations?.work_queues?.pending_recovery_ids);
  const failedMissionIds = normalizeStringArray(runtimeReadModel.operations?.attention?.failed_mission_ids);
  const activeSession = normalizeSessionRuntimeEntries(runtimeReadModel)
    .find((entry) => normalizeString(entry.session_id) === sessionId);
  const status = runningMissionIds.length > 0
    ? 'running'
    : pendingRecoveryIds.length > 0
      ? 'waiting'
      : failedMissionIds.length > 0
        ? 'failed'
        : activeSession
          ? normalizeTaskStatus(normalizeString(activeSession.current_status)) === 'completed'
            ? 'completed'
            : 'idle'
          : 'idle';

  return {
    sessionId: sessionId || undefined,
    status,
    phase: runningMissionIds.length > 0 ? 'execute' : (pendingRecoveryIds.length > 0 ? 'recovery' : 'idle'),
    errors: failedMissionIds.length > 0 ? failedMissionIds.map((id) => `mission_failed:${id}`) : [],
    statusChangedAt: normalizeNumber(activeSession?.last_update, generatedAt),
    lastEventAt: normalizeNumber(activeSession?.last_update, generatedAt),
    canResume: pendingRecoveryIds.length > 0,
    runtimeReason: activeSession?.latest_event_type || undefined,
    assignments,
    chain: activeSession?.execution_chain_ref
      ? {
          chainId: activeSession.execution_chain_ref,
          status: activeSession.current_status || 'unknown',
          recoverable: normalizeString(activeSession.recovery_ref).length > 0,
          attempt: 1,
          createdAt: normalizeNumber(activeSession.last_update, generatedAt),
          updatedAt: normalizeNumber(activeSession.last_update, generatedAt),
        }
      : undefined,
    opsView: null,
    runtimeSnapshot: null,
    runtimeDecisionTrace: [],
  };
}

function normalizeNotificationKind(kind: string): SessionNotificationRecord['kind'] {
  if (kind === 'incident' || kind === 'audit' || kind === 'center') {
    return kind;
  }
  return 'toast';
}

function normalizeNotifications(
  notifications: RustNotificationRecord[] | undefined,
): NormalizedNotificationEntry[] {
  if (!Array.isArray(notifications)) {
    return [];
  }
  const normalized: NormalizedNotificationEntry[] = [];
  for (const notification of notifications) {
    const notificationId = normalizeString(notification.notification_id);
    const sessionId = normalizeString(notification.session_id);
    const message = normalizeString(notification.message);
    if (!notificationId || !sessionId || !message) {
      continue;
    }
    normalized.push({
      sessionId,
      record: {
        notificationId,
        kind: normalizeNotificationKind(normalizeString(notification.kind)),
        level: 'info',
        message,
        createdAt: normalizeNumber(notification.created_at, Date.now()),
        read: Boolean(notification.handled),
        persistToCenter: true,
        actionRequired: false,
        countUnread: !notification.handled,
      },
    });
  }
  return normalized;
}

function createRustTimelineMessage(input: {
  id: string;
  content: string;
  timestamp: number;
  sessionId: string;
  eventSeq: number;
  role: Message['role'];
  type: Message['type'];
  rustEventType?: string;
  blocks?: ContentBlock[];
}): Message {
  const blocks: ContentBlock[] = input.blocks && input.blocks.length > 0
    ? input.blocks
    : [{ type: 'text', content: input.content }];
  const displayContent = blocks
    .filter((b) => b.type === 'text')
    .map((b) => b.content)
    .join('\n') || input.content;
  return {
    id: input.id,
    role: input.role,
    source: 'orchestrator',
    content: displayContent,
    blocks,
    timestamp: input.timestamp,
    updatedAt: input.timestamp,
    isStreaming: false,
    isComplete: true,
    type: input.type,
    noticeType: input.type === 'system-notice' ? 'info' : undefined,
    metadata: {
      sessionId: input.sessionId,
      eventSeq: input.eventSeq,
      timelineAnchorTimestamp: input.timestamp,
      rustEventType: input.rustEventType,
    },
  };
}

function tryParseStructuredBlocks(raw: string): ContentBlock[] | undefined {
  if (!raw.startsWith('{')) {
    return undefined;
  }
  try {
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed?.blocks)) {
      return undefined;
    }
    return (parsed.blocks as Array<Record<string, unknown>>).map((block, i) => {
      const blockType = typeof block.type === 'string' ? block.type : 'text';
      const content = typeof block.content === 'string' ? block.content : '';
      if (blockType === 'tool_call' && block.toolCall && typeof block.toolCall === 'object') {
        const tc = block.toolCall as Record<string, unknown>;
        return {
          type: 'tool_call' as const,
          content,
          id: `tool-block-${i}`,
          toolCall: {
            id: typeof tc.id === 'string' ? tc.id : `tc-${i}`,
            name: typeof tc.name === 'string' ? tc.name : 'unknown',
            arguments: (typeof tc.arguments === 'object' && tc.arguments !== null
              ? tc.arguments
              : {}) as Record<string, unknown>,
            status: (typeof tc.status === 'string' ? tc.status : 'success') as 'success' | 'error',
            result: typeof tc.result === 'string' ? tc.result : undefined,
          },
        };
      }
      return { type: 'text' as const, content, id: `text-block-${i}` };
    });
  } catch {
    return undefined;
  }
}

function buildTimelineEntryArtifacts(
  sessionId: string,
  timeline: RustTimelineEntry[] | undefined,
): TimelineProjectionArtifact[] {
  if (!Array.isArray(timeline)) {
    return [];
  }
  const artifacts: TimelineProjectionArtifact[] = [];
  const sessionTimeline = timeline.filter((entry) => normalizeString(entry.session_id) === sessionId);
  for (const [index, entry] of sessionTimeline.entries()) {
    const entryId = normalizeString(entry.entry_id);
    const message = normalizeString(entry.message);
    if (!entryId || !message) {
      continue;
    }
    const timestamp = normalizeNumber(entry.occurred_at, Date.now());
    const kind = normalizeString(entry.kind);
    let role: 'user' | 'assistant' | 'system' = 'system';
    let type: 'user_input' | 'text' | 'system-notice' = 'system-notice';
    if (kind === 'UserMessage') {
      role = 'user';
      type = 'user_input';
    } else if (kind === 'AssistantMessage') {
      role = 'assistant';
      type = 'text';
    }
    const parsedBlocks = kind === 'AssistantMessage' ? tryParseStructuredBlocks(message) : undefined;
    artifacts.push({
      artifactId: `rust-timeline:${entryId}`,
      kind: 'message',
      displayOrder: index + 1,
      anchorEventSeq: 0,
      latestEventSeq: 0,
      cardStreamSeq: 0,
      timestamp,
      threadVisible: true,
      workerTabs: [],
      messageIds: [`rust-timeline:${entryId}`],
      message: createRustTimelineMessage({
        id: `rust-timeline:${entryId}`,
        content: parsedBlocks ? '' : message,
        timestamp,
        sessionId,
        eventSeq: 0,
        role,
        type,
        blocks: parsedBlocks,
      }),
    });
  }
  return artifacts;
}

function summarizeRustEvent(event: RustEventEnvelope): string {
  const eventType = normalizeString(event.event_type);
  const payload = event.payload || {};
  const text = normalizeString(payload.text);
  switch (eventType) {
    case 'session.action.accepted':
      return text || '已提交会话消息';
    case 'task.execute.accepted':
      return text || '已提交任务执行请求';
    case 'recovery.resume.executed':
      return '已执行恢复流程';
    case 'mission.created':
      return '已创建任务链';
    case 'assignment.created':
      return '已创建执行分配';
    case 'task.created':
    case 'todo.created':
      return '已创建任务';
    case 'task.dispatched':
    case 'todo.dispatched':
      return '任务开始执行';
    case 'task.completed':
    case 'todo.completed':
      return '任务已完成';
    case 'task.failed':
    case 'todo.failed':
      return '任务执行失败';
    case 'mission.execution.overview':
      return '执行概览已更新';
    case 'mission.resumed.from_recovery':
      return '任务链已从恢复继续';
    case 'worker.resumed.from_recovery':
      return 'Worker 已从恢复继续';
    case 'worker.resumed.from_dispatch':
      return 'Worker 已接入执行链';
    default:
      return `事件：${eventType}`;
  }
}

function buildEventArtifacts(
  sessionId: string,
  runtimeReadModel: RustRuntimeReadModelDto | undefined,
  events: RustEventEnvelope[],
): TimelineProjectionArtifact[] {
  const activeSession = normalizeSessionRuntimeEntries(runtimeReadModel)
    .find((entry) => normalizeString(entry.session_id) === sessionId);
  const relevantMissionIds = new Set(normalizeStringArray(activeSession?.active_mission_ids));
  const relevantTaskIds = new Set(normalizeStringArray(activeSession?.active_task_ids));

  return events
    .filter((event) => {
      const eventSessionId = normalizeString(event.session_id);
      if (eventSessionId) {
        return eventSessionId === sessionId;
      }
      const missionId = normalizeString(event.mission_id);
      if (missionId && relevantMissionIds.has(missionId)) {
        return true;
      }
      const taskId = normalizeString(event.task_id);
      return Boolean(taskId && relevantTaskIds.has(taskId));
    })
    .sort((left, right) => normalizeNumber(left.sequence, 0) - normalizeNumber(right.sequence, 0))
    .flatMap((event, index) => {
      const eventId = normalizeString(event.event_id);
      if (!eventId) {
        return [];
      }
      const timestamp = normalizeNumber(event.occurred_at, Date.now());
      const sequence = normalizeNumber(event.sequence, 0);
      const artifacts: TimelineProjectionArtifact[] = [];

      const eventType = normalizeString(event.event_type);
      const suppressedTypes = [
        'session.action.accepted', 'task.execute.accepted',
        'tool.invoked', 'tool.usage.recorded',
        'task.llm.started', 'task.llm.completed',
        'task.tool.invoked',
        'task.lease.granted', 'task.lease.completed',
        'task.ready', 'task.started',
        'task.status.changed',
        'task.graph.created',
        'task.dispatched', 'task.completed', 'task.failed',
        'message.created',
        'mission.execution.overview',
        'mission.created', 'assignment.created',
        'task.created',
      ];
      if (suppressedTypes.some((t) => eventType === t)) {
        return artifacts;
      }

      artifacts.push({
        artifactId: `rust-event:${eventId}`,
        kind: 'message',
        displayOrder: index + 1,
        anchorEventSeq: sequence,
        latestEventSeq: sequence,
        cardStreamSeq: 0,
        timestamp,
        threadVisible: true,
        workerTabs: [],
        messageIds: [`rust-event:${eventId}`],
        message: createRustTimelineMessage({
          id: `rust-event:${eventId}`,
          content: summarizeRustEvent(event),
          timestamp,
          sessionId,
          eventSeq: sequence,
          role: 'system',
          type: 'system-notice',
          rustEventType: event.event_type,
        }),
      });

      return artifacts;
    });
}

function buildTimelineProjection(
  sessionId: string,
  generatedAt: number,
  payload: RustBootstrapDto,
): SessionTimelineProjection {
  const recentEvents = Array.isArray(payload.recent_events)
    ? payload.recent_events.map((event) => normalizeEventEnvelope(event)).filter((event): event is RustEventEnvelope => event !== null)
    : [];
  const timelineArtifacts = buildTimelineEntryArtifacts(sessionId, payload.timeline);
  const eventArtifacts = buildEventArtifacts(sessionId, payload.runtime_read_model, recentEvents);
  const artifacts = [...timelineArtifacts, ...eventArtifacts];
  const maxEventSeq = recentEvents.reduce((max, event) => Math.max(max, normalizeNumber(event.sequence, 0)), 0);
  const latestSequence = Math.max(
    normalizeNumber(payload.runtime_read_model?.meta?.latest_sequence, 0),
    maxEventSeq,
  );

  return {
    schemaVersion: 'session-timeline-projection.v2',
    sessionId,
    updatedAt: generatedAt,
    lastAppliedEventSeq: latestSequence,
    artifacts,
    threadRenderEntries: [],
    workerRenderEntries: {},
  };
}

export function isRustEventEnvelope(value: unknown): value is RustEventEnvelope {
  return normalizeEventEnvelope(value) !== null;
}

export function parseRustEventEnvelope(rawData: string): RustEventEnvelope | null {
  try {
    return normalizeEventEnvelope(JSON.parse(rawData));
  } catch {
    return null;
  }
}

export function normalizeRustBootstrapPayload(
  rawPayload: unknown,
  options: { workspaceId?: string; workspacePath?: string; sessionId?: string } = {},
): BootstrapPayload {
  const payload = (rawPayload ?? {}) as RustBootstrapDto;
  const generatedAt = normalizeNumber(payload.generated_at, Date.now());
  const sessions = normalizeRustSessions(payload, generatedAt);
  const selectedSessionId = normalizeString(options.sessionId)
    || normalizeString(payload.current_session?.session_id)
    || sessions[0]?.id
    || '';
  const currentSession = sessions.find((session) => session.id === selectedSessionId) ?? sessions[0];
  const workspace = resolveSelectedWorkspace(payload, options);
  const normalizedEvents = Array.isArray(payload.recent_events)
    ? payload.recent_events.map((event) => normalizeEventEnvelope(event)).filter((event): event is RustEventEnvelope => event !== null)
    : [];
  const assignments = buildAssignmentsFromRuntime(payload.runtime_read_model, normalizedEvents);
  const processingState = deriveProcessingState(payload.runtime_read_model, generatedAt);
  const state: AppState = {
    ...buildEmptyWorkspaceAppState(generatedAt),
    sessions,
    currentSession,
    currentSessionId: currentSession?.id || '',
    isProcessing: Boolean(processingState?.isProcessing),
    processingState,
    stateUpdatedAt: generatedAt,
  };
  const timelineProjection = buildTimelineProjection(currentSession?.id || '', generatedAt, payload);
  const normalizedNotifications = normalizeNotifications(payload.notifications);

  return {
    agent: {
      runtimeEpoch: normalizeString(payload.agent?.runtimeEpoch)
        || (currentSession?.id && workspace.workspaceId ? `${workspace.workspaceId}:${currentSession.id}` : String(generatedAt)),
    },
    workspace,
    sessionId: currentSession?.id || '',
    sessions,
    state,
    timelineProjection,
    notifications: currentSession?.id
        ? {
          sessionId: currentSession.id,
          notifications: {
            lastUpdatedAt: generatedAt,
            records: normalizedNotifications
              .filter((item) => item.sessionId === currentSession.id)
              .map((item) => item.record),
          },
        }
      : undefined,
    queuedMessages: [],
    orchestratorRuntimeState: deriveRuntimeState(
      payload.runtime_read_model,
      assignments,
      currentSession?.id || '',
      generatedAt,
    ),
  };
}
