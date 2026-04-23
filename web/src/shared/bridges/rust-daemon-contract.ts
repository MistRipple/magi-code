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
  TimelineProjectionRenderEntry,
} from '../../types/message';
import { buildEmptyWorkspaceAppState } from './empty-workspace-state';
import { isRuntimeInternalTool } from '../tool-visibility';

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
  sessionId?: string;
  title?: string | null;
  createdAt?: number;
  updatedAt?: number;
  messageCount?: number;
}

interface RustBootstrapWorkspaceRecord {
  workspaceId?: string;
  rootPath?: string;
}

interface RustNotificationRecord {
  notificationId?: string;
  sessionId?: string;
  kind?: string;
  message?: string;
  createdAt?: number;
  handled?: boolean;
}

interface RustTimelineEntry {
  entryId?: string;
  sessionId?: string;
  kind?: string;
  message?: string;
  occurredAt?: number;
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
  active_execution_group_ids?: string[];
  active_task_ids?: string[];
  recovery_ids?: string[];
  latest_event_type?: string | null;
  current_status?: string | null;
  last_update?: number | null;
  mission_id?: string | null;
  root_task_id?: string | null;
  root_task_status?: string | null;
  root_task_created_at?: number | null;
  execution_chain_ref?: string | null;
  recovery_ref?: string | null;
  has_recoverable_chain?: boolean;
  recoverable_branch_count?: number;
  active_branches?: RustSessionRuntimeBranchSummary[];
  current_turn?: RustSessionRuntimeTurnSummary | null;
  turn_items?: RustSessionRuntimeTurnItemSummary[];
  worker_lanes?: RustSessionRuntimeTurnLaneSummary[];
}

interface RustSessionRuntimeBranchSummary {
  task_id?: string;
  worker_id?: string;
  status?: string;
  stage?: string;
  lease_id?: string | null;
  execution_intent_ref?: string | null;
  binding_lifecycle?: string | null;
  checkpoint_stage?: string | null;
  next_step_index?: number | null;
  checkpoint_at?: number | null;
  resume_mode?: string | null;
  is_primary?: boolean;
}

interface RustSessionRuntimeTurnSummary {
  turn_id?: string;
  turn_seq?: number;
  accepted_at?: number | null;
  status?: string;
  user_message?: string | null;
  mission_id?: string | null;
  root_task_id?: string | null;
  execution_chain_ref?: string | null;
}

interface RustSessionRuntimeTurnItemSummary {
  item_id?: string;
  item_seq?: number;
  lane_id?: string | null;
  lane_seq?: number | null;
  kind?: string;
  status?: string;
  source?: string;
  title?: string | null;
  content?: string | null;
  task_id?: string | null;
  worker_id?: string | null;
  role_id?: string | null;
  tool_call_id?: string | null;
  tool_name?: string | null;
  tool_status?: string | null;
  tool_result?: string | null;
  tool_error?: string | null;
  thread_visible?: boolean;
  worker_visible?: boolean;
}

interface RustSessionRuntimeTurnLaneSummary {
  lane_id?: string;
  lane_seq?: number;
  task_id?: string;
  worker_id?: string;
  role_id?: string | null;
  title?: string;
  status?: string;
  is_primary?: boolean;
}

interface RustRuntimeReadModelDto {
  meta?: {
    latest_sequence?: number;
  };
  details?: {
    execution_groups?: unknown[];
    assignments?: RustAssignmentRuntimeSummary[];
    tasks?: RustTaskRuntimeSummary[];
    workers?: unknown[];
    sessions?: RustSessionRuntimeSummary[];
  };
  overview?: {
    activity?: {
      active_execution_group_ids?: string[];
      active_task_ids?: string[];
    };
  };
  operations?: {
    attention?: {
      failed_execution_group_ids?: string[];
      failed_task_ids?: string[];
      pending_recovery_ids?: string[];
    };
    work_queues?: {
      running_execution_group_ids?: string[];
      running_task_ids?: string[];
      pending_recovery_ids?: string[];
    };
  };
  recovery?: {
    active_recovery_ids?: string[];
  };
}

interface RustBootstrapDto {
  generatedAt?: number;
  currentSession?: RustBootstrapSessionRecord | null;
  sessions?: RustBootstrapSessionRecord[];
  timeline?: RustTimelineEntry[];
  pendingChanges?: unknown[];
  workspaces?: RustBootstrapWorkspaceRecord[];
  runtimeReadModel?: RustRuntimeReadModelDto;
  notifications?: RustNotificationRecord[];
  recentEvents?: RustEventEnvelope[];
  agent?: {
    runtimeEpoch?: string;
  };
}

interface RustTimelinePageDto {
  sessionId?: string;
  hasMoreBefore?: boolean;
  beforeCursor?: string | null;
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
  return getRuntimeDetailEntries(runtimeReadModel, 'tasks')
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
      active_execution_group_ids: normalizeStringArray(entry.active_execution_group_ids),
      active_task_ids: normalizeStringArray(entry.active_task_ids),
      recovery_ids: normalizeStringArray(entry.recovery_ids),
      latest_event_type: normalizeString(entry.latest_event_type) || undefined,
      current_status: normalizeString(entry.current_status) || undefined,
      last_update: typeof entry.last_update === 'number' ? Math.floor(entry.last_update) : undefined,
      mission_id: normalizeString(entry.mission_id) || undefined,
      root_task_id: normalizeString(entry.root_task_id) || undefined,
      root_task_status: normalizeString(entry.root_task_status) || undefined,
      root_task_created_at: typeof entry.root_task_created_at === 'number'
        ? Math.floor(entry.root_task_created_at)
        : undefined,
      execution_chain_ref: normalizeString(entry.execution_chain_ref) || undefined,
      recovery_ref: normalizeString(entry.recovery_ref) || undefined,
      has_recoverable_chain: entry.has_recoverable_chain === true,
      recoverable_branch_count: typeof entry.recoverable_branch_count === 'number'
        ? Math.floor(entry.recoverable_branch_count)
        : undefined,
      current_turn: (() => {
        const turn = normalizeObjectRecord(entry.current_turn);
        if (!turn) {
          return undefined;
        }
        return {
          turn_id: normalizeString(turn.turn_id) || undefined,
          turn_seq: typeof turn.turn_seq === 'number' ? Math.floor(turn.turn_seq) : undefined,
          accepted_at: typeof turn.accepted_at === 'number' ? Math.floor(turn.accepted_at) : undefined,
          status: normalizeString(turn.status) || undefined,
          user_message: normalizeString(turn.user_message) || undefined,
          mission_id: normalizeString(turn.mission_id) || undefined,
          root_task_id: normalizeString(turn.root_task_id) || undefined,
          execution_chain_ref: normalizeString(turn.execution_chain_ref) || undefined,
        };
      })(),
      turn_items: Array.isArray(entry.turn_items)
        ? entry.turn_items
          .map((item) => normalizeObjectRecord(item))
          .filter((item): item is Record<string, unknown> => item !== null)
          .map((item) => ({
            item_id: normalizeString(item.item_id) || undefined,
            item_seq: typeof item.item_seq === 'number' ? Math.floor(item.item_seq) : undefined,
            lane_id: normalizeString(item.lane_id) || undefined,
            lane_seq: typeof item.lane_seq === 'number' ? Math.floor(item.lane_seq) : undefined,
            kind: normalizeString(item.kind) || undefined,
            status: normalizeString(item.status) || undefined,
            source: normalizeString(item.source) || undefined,
            title: normalizeString(item.title) || undefined,
            content: normalizeString(item.content) || undefined,
            task_id: normalizeString(item.task_id) || undefined,
            worker_id: normalizeString(item.worker_id) || undefined,
            role_id: normalizeString(item.role_id) || undefined,
            tool_call_id: normalizeString(item.tool_call_id) || undefined,
            tool_name: normalizeString(item.tool_name) || undefined,
            tool_status: normalizeString(item.tool_status) || undefined,
            tool_result: normalizeString(item.tool_result) || undefined,
            tool_error: normalizeString(item.tool_error) || undefined,
            thread_visible: item.thread_visible !== false,
            worker_visible: item.worker_visible === true,
          }))
        : [],
      worker_lanes: Array.isArray(entry.worker_lanes)
        ? entry.worker_lanes
          .map((lane) => normalizeObjectRecord(lane))
          .filter((lane): lane is Record<string, unknown> => lane !== null)
          .map((lane) => ({
            lane_id: normalizeString(lane.lane_id) || undefined,
            lane_seq: typeof lane.lane_seq === 'number' ? Math.floor(lane.lane_seq) : undefined,
            task_id: normalizeString(lane.task_id) || undefined,
            worker_id: normalizeString(lane.worker_id) || undefined,
            role_id: normalizeString(lane.role_id) || undefined,
            title: normalizeString(lane.title) || undefined,
            status: normalizeString(lane.status) || undefined,
            is_primary: lane.is_primary === true,
          }))
        : [],
      active_branches: Array.isArray(entry.active_branches)
        ? entry.active_branches
          .map((branch) => normalizeObjectRecord(branch))
          .filter((branch): branch is Record<string, unknown> => branch !== null)
          .map((branch) => ({
            task_id: normalizeString(branch.task_id) || undefined,
            worker_id: normalizeString(branch.worker_id) || undefined,
            status: normalizeString(branch.status) || undefined,
            stage: normalizeString(branch.stage) || undefined,
            lease_id: normalizeString(branch.lease_id) || undefined,
            execution_intent_ref: normalizeString(branch.execution_intent_ref) || undefined,
            binding_lifecycle: normalizeString(branch.binding_lifecycle) || undefined,
            checkpoint_stage: normalizeString(branch.checkpoint_stage) || undefined,
            next_step_index: typeof branch.next_step_index === 'number'
              ? Math.floor(branch.next_step_index)
              : undefined,
            checkpoint_at: typeof branch.checkpoint_at === 'number'
              ? Math.floor(branch.checkpoint_at)
              : undefined,
            resume_mode: normalizeString(branch.resume_mode) || undefined,
            is_primary: branch.is_primary === true,
          }))
        : [],
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

function resolveEventPayload(event: RustEventEnvelope): Record<string, unknown> {
  return event.payload || {};
}

function resolveEventMissionId(event: RustEventEnvelope): string {
  const payload = resolveEventPayload(event);
  return normalizeString(event.mission_id) || normalizeString(payload.mission_id);
}

function resolveEventAssignmentId(event: RustEventEnvelope): string {
  const payload = resolveEventPayload(event);
  return normalizeString(event.assignment_id) || normalizeString(payload.assignment_id);
}

function resolveEventTaskId(event: RustEventEnvelope): string {
  const payload = resolveEventPayload(event);
  return normalizeString(event.task_id) || normalizeString(payload.task_id);
}

function resolveEventWorkerId(event: RustEventEnvelope): string {
  const payload = resolveEventPayload(event);
  return normalizeString(payload.worker_id);
}

function normalizeRustSessions(
  payload: RustBootstrapDto,
  generatedAt: number,
): Session[] {
  const sessions: Session[] = [];
  const seenSessionIds = new Set<string>();
  const appendSession = (session: RustBootstrapSessionRecord | null | undefined): void => {
    if (!session) {
      return;
    }
    const sessionId = normalizeString(session.sessionId);
    if (!sessionId) {
      return;
    }
    if (seenSessionIds.has(sessionId)) {
      return;
    }
    seenSessionIds.add(sessionId);
    const createdAt = normalizeNumber(session.createdAt, generatedAt);
    const updatedAt = normalizeNumber(session.updatedAt, createdAt);
    const messageCount = normalizeNumber(session.messageCount, NaN);
    sessions.push({
      id: sessionId,
      name: normalizeString(session.title) || undefined,
      createdAt,
      updatedAt,
      ...(Number.isFinite(messageCount) ? { messageCount } : {}),
    });
  };

  for (const session of Array.isArray(payload.sessions) ? payload.sessions : []) {
    appendSession(session);
  }
  if (payload.currentSession) {
    const currentSessionId = normalizeString(payload.currentSession.sessionId);
    if (currentSessionId && !seenSessionIds.has(currentSessionId)) {
      sessions.unshift({
        id: currentSessionId,
        name: normalizeString(payload.currentSession.title) || undefined,
        createdAt: normalizeNumber(payload.currentSession.createdAt, generatedAt),
        updatedAt: normalizeNumber(
          payload.currentSession.updatedAt,
          normalizeNumber(payload.currentSession.createdAt, generatedAt),
        ),
        ...(Number.isFinite(normalizeNumber(payload.currentSession.messageCount, NaN))
          ? { messageCount: normalizeNumber(payload.currentSession.messageCount, NaN) }
          : {}),
      });
    }
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
  const selectedWorkspace = workspaces.find((workspace) => normalizeString(workspace.workspaceId) === requestedWorkspaceId)
    || workspaces[0]
    || null;
  const workspaceId = requestedWorkspaceId || normalizeString(selectedWorkspace?.workspaceId);
  const rootPath = requestedWorkspacePath || normalizeString(selectedWorkspace?.rootPath);
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
    const missionId = resolveEventMissionId(event);
    const assignmentId = resolveEventAssignmentId(event);
    const taskId = resolveEventTaskId(event);
    const missionTitle = normalizeString(payload.mission_title);
    const assignmentTitle = normalizeString(payload.assignment_title);
    const taskTitle = normalizeString(payload.task_title);
    const workerId = resolveEventWorkerId(event);

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
  if (normalized.includes('cancel') || normalized.includes('abort')) {
    return 'cancelled';
  }
  if (normalized.includes('reject')) {
    return 'failed';
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
  sessionId: string,
): OrchestrationRuntimeAssignmentSummary[] {
  const assignmentEntries = normalizeAssignmentRuntimeEntries(runtimeReadModel);
  const { taskEntries, activeMissionIds } = resolveSessionTaskEntries(runtimeReadModel, sessionId);
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
    const missionId = normalizeString(assignment.mission_id);
    if (!assignmentId) {
      continue;
    }
    if (missionId && activeMissionIds.size > 0 && !activeMissionIds.has(missionId)) {
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

function resolveSessionTaskEntries(
  runtimeReadModel: RustRuntimeReadModelDto | undefined,
  sessionId: string,
): {
  activeSession: RustSessionRuntimeSummary | undefined;
  taskEntries: RustTaskRuntimeSummary[];
  activeMissionIds: Set<string>;
} {
  const allTaskEntries = normalizeTaskRuntimeEntries(runtimeReadModel);
  const activeSession = normalizeSessionRuntimeEntries(runtimeReadModel)
    .find((entry) => normalizeString(entry.session_id) === sessionId);
  const activeTaskIds = new Set(normalizeStringArray(activeSession?.active_task_ids));
  const activeMissionIds = new Set(normalizeStringArray(activeSession?.active_execution_group_ids));
  if (activeMissionIds.size === 0 && activeTaskIds.size > 0) {
    for (const task of allTaskEntries) {
      const taskId = normalizeString(task.task_id);
      const missionId = normalizeString(task.mission_id);
      if (taskId && missionId && activeTaskIds.has(taskId)) {
        activeMissionIds.add(missionId);
      }
    }
  }
  const taskEntries = allTaskEntries
    .filter((entry) => {
      const taskId = normalizeString(entry.task_id);
      const missionId = normalizeString(entry.mission_id);
      return activeTaskIds.has(taskId) || (missionId && activeMissionIds.has(missionId));
    });
  return { activeSession, taskEntries, activeMissionIds };
}

function buildSessionTaskStatusSummary(
  runtimeReadModel: RustRuntimeReadModelDto | undefined,
  sessionId: string,
): {
  activeSession: RustSessionRuntimeSummary | undefined;
  taskEntries: RustTaskRuntimeSummary[];
  runningTaskIds: string[];
  failedTaskIds: string[];
} {
  const { activeSession, taskEntries } = resolveSessionTaskEntries(runtimeReadModel, sessionId);
  const runningTaskIds: string[] = [];
  const failedTaskIds: string[] = [];

  for (const task of taskEntries) {
    const taskId = normalizeString(task.task_id);
    if (!taskId) {
      continue;
    }
    const status = normalizeSubTaskStatus(
      normalizeString(task.current_status),
      typeof task.failed_dispatch_count === 'number' ? task.failed_dispatch_count : 0,
    );
    if (status === 'running') {
      runningTaskIds.push(taskId);
      continue;
    }
    if (status === 'failed' || status === 'blocked') {
      failedTaskIds.push(taskId);
    }
  }

  return {
    activeSession,
    taskEntries,
    runningTaskIds,
    failedTaskIds,
  };
}

function deriveProcessingState(
  runtimeReadModel: RustRuntimeReadModelDto | undefined,
  sessionId: string,
): AppState['processingState'] {
  const { activeSession, runningTaskIds } = buildSessionTaskStatusSummary(runtimeReadModel, sessionId);
  const isProcessing = runningTaskIds.length > 0;

  if (!isProcessing) {
    return null;
  }

  return {
    isProcessing: true,
    source: 'orchestrator',
    agent: 'orchestrator',
    startedAt: normalizeNumber(activeSession?.root_task_created_at, 0),
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

  const { activeSession, runningTaskIds, failedTaskIds } = buildSessionTaskStatusSummary(
    runtimeReadModel,
    sessionId,
  );
  const hasRecoverableChain = activeSession?.has_recoverable_chain === true;
  const recoverableBranchCount = typeof activeSession?.recoverable_branch_count === 'number'
    ? activeSession.recoverable_branch_count
    : 0;
  const canContinueCurrentSession = hasRecoverableChain && recoverableBranchCount > 0;
  const status = runningTaskIds.length > 0
    ? 'running'
    : canContinueCurrentSession
      ? 'paused'
      : failedTaskIds.length > 0
        ? 'failed'
        : activeSession
          ? normalizeTaskStatus(normalizeString(activeSession.root_task_status || activeSession.current_status)) === 'completed'
            ? 'completed'
            : 'idle'
          : 'idle';

  return {
    sessionId: sessionId || undefined,
    status,
    phase: runningTaskIds.length > 0 ? 'execute' : (canContinueCurrentSession ? 'paused' : 'idle'),
    errors: failedTaskIds.length > 0 ? failedTaskIds.map((id) => `task_failed:${id}`) : [],
    statusChangedAt: normalizeNumber(activeSession?.last_update, generatedAt),
    lastEventAt: normalizeNumber(activeSession?.last_update, generatedAt),
    canResume: canContinueCurrentSession,
    runtimeReason: activeSession?.latest_event_type || undefined,
    assignments,
    chain: activeSession?.execution_chain_ref
        ? {
          chainId: activeSession.execution_chain_ref,
          status: activeSession.root_task_status || activeSession.current_status || 'unknown',
          recoverable: canContinueCurrentSession,
          attempt: 1,
          createdAt: normalizeNumber(activeSession.root_task_created_at, generatedAt),
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
    const notificationId = normalizeString(notification.notificationId);
    const sessionId = normalizeString(notification.sessionId);
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
        createdAt: normalizeNumber(notification.createdAt, Date.now()),
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
  isStreaming?: boolean;
  rustEventType?: string;
  blocks?: ContentBlock[];
  source?: Message['source'];
  metadata?: Record<string, unknown>;
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
    source: input.source || 'orchestrator',
    content: displayContent,
    blocks,
    timestamp: input.timestamp,
    updatedAt: input.timestamp,
    isStreaming: input.isStreaming === true,
    isComplete: input.isStreaming !== true,
    type: input.type,
    noticeType: input.type === 'system-notice' ? 'info' : undefined,
    metadata: {
      ...(input.metadata || {}),
      sessionId: input.sessionId,
      eventSeq: input.eventSeq,
      timelineAnchorTimestamp: input.timestamp,
      rustEventType: input.rustEventType,
    },
  };
}

function resolveSerializedToolName(block: Record<string, unknown>): string {
  const toolCall = block.toolCall;
  if (toolCall && typeof toolCall === 'object' && !Array.isArray(toolCall)) {
    const nestedName = normalizeString((toolCall as Record<string, unknown>).name);
    if (nestedName) {
      return nestedName;
    }
  }
  return normalizeString(block.toolName)
    || normalizeString(block.toolId)
    || normalizeString(block.toolCallId);
}

function normalizeSerializedContentBlock(block: unknown): ContentBlock | null {
  if (!block || typeof block !== 'object' || Array.isArray(block)) {
    return null;
  }
  const record = block as Record<string, unknown>;
  const type = normalizeString(record.type);
  const content = normalizeString(record.content);
  if ((type === 'tool_call' || type === 'tool_result') && isRuntimeInternalTool(resolveSerializedToolName(record))) {
    return null;
  }
  switch (type) {
    case 'text':
      return content ? { type: 'text', content } : null;
    case 'code':
      return {
        ...record,
        type: 'code',
        content,
        language: normalizeString(record.language) || undefined,
      } as ContentBlock;
    case 'thinking':
    case 'tool_call':
    case 'tool_result':
    case 'file_change':
    case 'plan':
    case 'dispatch_group':
      return {
        ...record,
        type,
        content,
      } as ContentBlock;
    default:
      return content ? { type: 'text', content } : null;
  }
}

function parseSerializedMessageContent(rawContent: string): {
  content: string;
  blocks?: ContentBlock[];
} {
  const content = normalizeString(rawContent);
  if (!content || !content.startsWith('{')) {
    return { content };
  }
  try {
    const parsed = JSON.parse(content) as { blocks?: unknown[] };
    if (!Array.isArray(parsed.blocks)) {
      return { content };
    }
    const blocks = parsed.blocks
      .map((block) => normalizeSerializedContentBlock(block))
      .filter((block): block is ContentBlock => block !== null);
    const textContent = blocks
      .filter((block) => block.type === 'text')
      .map((block) => block.content)
      .filter((blockContent) => blockContent.trim().length > 0)
      .join('\n')
      .trim();
    if (blocks.length === 0 && !textContent) {
      return { content: '' };
    }
    return {
      content: textContent,
      blocks,
    };
  } catch {
    return { content };
  }
}

function resolveSessionRuntimeEntry(
  runtimeReadModel: RustRuntimeReadModelDto | undefined,
  sessionId: string,
): RustSessionRuntimeSummary | undefined {
  return normalizeSessionRuntimeEntries(runtimeReadModel)
    .find((entry) => normalizeString(entry.session_id) === sessionId);
}

function buildCurrentTurnItemMessage(
  sessionId: string,
  turnId: string,
  turnStatus: string,
  acceptedAt: number,
  item: RustSessionRuntimeTurnItemSummary,
  lane: RustSessionRuntimeTurnLaneSummary | undefined,
  workerId: string,
  streamingFallbackItemId: string,
): Message {
  const itemId = normalizeString(item.item_id) || `turn-item:${turnId}:${Math.random()}`;
  const itemKind = normalizeString(item.kind);
  const normalizedStatus = normalizeString(item.status);
  const normalizedSource = normalizeString(item.source);
  const toolName = normalizeString(item.tool_name) || 'tool';
  const toolStatus = normalizeString(item.tool_status) || normalizedStatus || 'pending';
  const usesPhaseStreamingFallback = streamingFallbackItemId === itemId
    && (turnStatus === 'accepted' || turnStatus === 'running')
    && (normalizedStatus === 'running' || normalizedStatus === 'pending')
    && (itemKind === 'assistant_phase' || itemKind === 'worker_phase');
  const itemContent = normalizeString(item.content);
  const readableContent = (itemKind === 'assistant_final' || itemKind === 'worker_completed')
    ? (extractReadableTextFromTurnContent(itemContent) || itemContent)
    : itemContent;
  const rawContent = readableContent
    || normalizeString(item.title)
    || itemKind
    || '执行项';
  const content = usesPhaseStreamingFallback ? '' : rawContent;
  const metadata: Record<string, unknown> = {
    turnId,
    turnItemKind: itemKind,
    laneId: normalizeString(item.lane_id) || undefined,
    laneSeq: typeof item.lane_seq === 'number' ? item.lane_seq : undefined,
    laneStatus: normalizeString(lane?.status) || undefined,
    laneTitle: normalizeString(lane?.title) || undefined,
    taskId: normalizeString(item.task_id) || undefined,
    worker: workerId || undefined,
    roleId: normalizeString(item.role_id) || normalizeString(lane?.role_id) || undefined,
    blockSeq: typeof item.item_seq === 'number' ? item.item_seq : 0,
  };
  const displaySource = itemKind.startsWith('worker_')
    ? (workerId || normalizedSource || 'orchestrator')
    : (normalizedSource || 'orchestrator');

  if (itemKind === 'tool_call_started' || itemKind === 'worker_tool_call_started') {
    return createRustTimelineMessage({
      id: itemId,
      content,
      timestamp: acceptedAt,
      sessionId,
      eventSeq: 0,
      role: 'assistant',
      type: 'text',
      source: displaySource,
      blocks: [{
        type: 'tool_call',
        content,
        toolCall: {
          id: normalizeString(item.tool_call_id) || itemId,
          name: toolName,
          arguments: {},
          status: (toolStatus as 'pending' | 'running' | 'success' | 'error') || 'running',
        },
      }],
      metadata,
    });
  }

  if (itemKind === 'tool_call_result' || itemKind === 'worker_tool_call_result') {
    const normalizedStatus = toolStatus === 'error' ? 'error' : 'success';
    return createRustTimelineMessage({
      id: itemId,
      content,
      timestamp: acceptedAt,
      sessionId,
      eventSeq: 0,
      role: 'assistant',
      type: 'text',
      source: displaySource,
      blocks: [{
        type: 'tool_call',
        content,
        toolCall: {
          id: normalizeString(item.tool_call_id) || itemId,
          name: toolName,
          arguments: {},
          status: normalizedStatus,
          result: normalizeString(item.tool_result) || undefined,
          error: normalizeString(item.tool_error) || undefined,
        },
      }],
      metadata,
    });
  }

  if (itemKind === 'assistant_stream' || itemKind === 'worker_stream') {
    const turnStillActive = turnStatus === 'accepted' || turnStatus === 'running';
    return createRustTimelineMessage({
      id: itemId,
      content,
      timestamp: acceptedAt,
      sessionId,
      eventSeq: 0,
      role: 'assistant',
      type: 'text',
      source: displaySource,
      isStreaming: turnStillActive && (normalizedStatus === 'running' || normalizedStatus === 'pending'),
      metadata,
    });
  }

  return createRustTimelineMessage({
    id: itemId,
    content,
    timestamp: acceptedAt,
    sessionId,
    eventSeq: 0,
    role: 'assistant',
    type: itemKind === 'assistant_final' ? 'text' : 'progress',
    source: displaySource,
    isStreaming: usesPhaseStreamingFallback,
    metadata,
  });
}

function isUserRequestTurnItemKind(kind: string): boolean {
  return kind === 'user_message';
}

function isExecutionPhaseTurnItemKind(kind: string): boolean {
  return kind === 'assistant_phase' || kind === 'worker_phase';
}

function buildCurrentTurnWorkerLaneMetadata(
  sessionEntry: RustSessionRuntimeSummary | undefined,
): Array<{
  laneId?: string;
  laneSeq?: number;
  taskId?: string;
  worker?: string;
  roleId?: string;
  title?: string;
  status?: string;
  isPrimary?: boolean;
}> {
  const laneById = new Map<string, {
    laneId?: string;
    laneSeq?: number;
    taskId?: string;
    worker?: string;
    roleId?: string;
    title?: string;
    status?: string;
    isPrimary?: boolean;
  }>();

  const upsertLane = (lane: {
    laneId?: string;
    laneSeq?: number;
    taskId?: string;
    worker?: string;
    roleId?: string;
    title?: string;
    status?: string;
    isPrimary?: boolean;
  }) => {
    const laneId = normalizeString(lane.laneId);
    if (!laneId) {
      return;
    }
    const existing = laneById.get(laneId);
    laneById.set(laneId, {
      laneId,
      laneSeq: typeof existing?.laneSeq === 'number'
        ? existing.laneSeq
        : lane.laneSeq,
      taskId: existing?.taskId || normalizeString(lane.taskId) || undefined,
      worker: existing?.worker || normalizeString(lane.worker) || undefined,
      roleId: existing?.roleId || normalizeString(lane.roleId) || undefined,
      title: existing?.title || normalizeString(lane.title) || undefined,
      status: normalizeString(lane.status) || existing?.status || undefined,
      isPrimary: existing?.isPrimary === true || lane.isPrimary === true,
    });
  };

  for (const lane of sessionEntry?.worker_lanes || []) {
    upsertLane({
      laneId: normalizeString(lane.lane_id) || undefined,
      laneSeq: typeof lane.lane_seq === 'number' ? lane.lane_seq : undefined,
      taskId: normalizeString(lane.task_id) || undefined,
      worker: normalizeString(lane.worker_id) || undefined,
      roleId: normalizeString(lane.role_id) || undefined,
      title: normalizeString(lane.title) || undefined,
      status: normalizeString(lane.status) || undefined,
      isPrimary: lane.is_primary === true,
    });
  }

  for (const item of sessionEntry?.turn_items || []) {
    if (item.worker_visible !== true) {
      continue;
    }
    upsertLane({
      laneId: normalizeString(item.lane_id) || undefined,
      laneSeq: typeof item.lane_seq === 'number' ? item.lane_seq : undefined,
      taskId: normalizeString(item.task_id) || undefined,
      worker: normalizeString(item.worker_id) || undefined,
      roleId: normalizeString(item.role_id) || undefined,
      title: normalizeString(item.title) || undefined,
      status: normalizeString(item.status) || undefined,
      isPrimary: false,
    });
  }

  return Array.from(laneById.values()).sort((left, right) => {
    const leftSeq = typeof left.laneSeq === 'number' ? left.laneSeq : Number.MAX_SAFE_INTEGER;
    const rightSeq = typeof right.laneSeq === 'number' ? right.laneSeq : Number.MAX_SAFE_INTEGER;
    if (leftSeq !== rightSeq) {
      return leftSeq - rightSeq;
    }
    return normalizeString(left.laneId).localeCompare(normalizeString(right.laneId));
  });
}

function extractReadableTextFromTurnContent(rawContent: unknown): string {
  const rawText = normalizeString(rawContent);
  if (!rawText) {
    return '';
  }
  if (!rawText.startsWith('{')) {
    return rawText;
  }
  try {
    const parsed = JSON.parse(rawText) as { blocks?: unknown[] };
    if (!Array.isArray(parsed.blocks)) {
      return rawText;
    }
    const textBlocks = parsed.blocks
      .map((block) => {
        if (!block || typeof block !== 'object') {
          return '';
        }
        const record = block as Record<string, unknown>;
        if (normalizeString(record.type) !== 'text') {
          return '';
        }
        return normalizeString(record.content);
      })
      .filter((content) => content.length > 0 && !content.trim().startsWith('{'));
    return textBlocks.join('\n').trim();
  } catch {
    return rawText;
  }
}

function normalizeComparableTurnText(rawContent: unknown): string {
  return extractReadableTextFromTurnContent(rawContent)
    .replace(/\s+/g, ' ')
    .trim();
}

function resolveWorkerOutputSlotKey(item: RustSessionRuntimeTurnItemSummary): string {
  const laneId = normalizeString(item.lane_id);
  const taskId = normalizeString(item.task_id);
  const workerIdentity = normalizeString(item.role_id) || normalizeString(item.worker_id);
  if (!laneId && !taskId && !workerIdentity) {
    return '';
  }
  return [
    laneId,
    taskId,
    workerIdentity,
    normalizeString(item.title),
  ].join('|');
}

function buildSuppressedCurrentTurnItemIds(
  turnItems: RustSessionRuntimeTurnItemSummary[],
): Set<string> {
  const suppressedItemIds = new Set<string>();
  const completedWorkerTextBySlot = new Map<string, string>();
  const resolvedToolCallIds = new Set<string>();
  let assistantFinalText = '';

  for (const item of turnItems) {
    const itemKind = normalizeString(item.kind);
    if (itemKind === 'assistant_final') {
      assistantFinalText = normalizeComparableTurnText(item.content) || assistantFinalText;
      continue;
    }
    if (itemKind === 'worker_completed') {
      const slotKey = resolveWorkerOutputSlotKey(item);
      const completedText = normalizeComparableTurnText(item.content);
      if (slotKey && completedText) {
        completedWorkerTextBySlot.set(slotKey, completedText);
      }
      continue;
    }
    if (itemKind === 'tool_call_result' || itemKind === 'worker_tool_call_result') {
      const toolCallId = normalizeString(item.tool_call_id);
      if (toolCallId) {
        resolvedToolCallIds.add(toolCallId);
      }
    }
  }

  for (const item of turnItems) {
    const itemId = normalizeString(item.item_id);
    if (!itemId) {
      continue;
    }
    const itemKind = normalizeString(item.kind);
    if (itemKind === 'assistant_stream') {
      const streamText = normalizeComparableTurnText(item.content);
      if (
        streamText
        && assistantFinalText
        && (
          assistantFinalText === streamText
          || assistantFinalText.includes(streamText)
          || streamText.includes(assistantFinalText)
        )
      ) {
        suppressedItemIds.add(itemId);
      }
      continue;
    }
    if (itemKind === 'worker_stream') {
      const slotKey = resolveWorkerOutputSlotKey(item);
      const streamText = normalizeComparableTurnText(item.content);
      const completedText = slotKey ? completedWorkerTextBySlot.get(slotKey) || '' : '';
      if (
        streamText
        && completedText
        && (
          completedText === streamText
          || completedText.includes(streamText)
          || streamText.includes(completedText)
        )
      ) {
        suppressedItemIds.add(itemId);
      }
      continue;
    }
    if (itemKind === 'tool_call_started' || itemKind === 'worker_tool_call_started') {
      const toolCallId = normalizeString(item.tool_call_id);
      if (toolCallId && resolvedToolCallIds.has(toolCallId)) {
        suppressedItemIds.add(itemId);
      }
    }
  }

  return suppressedItemIds;
}

function buildCurrentTurnArtifacts(
  sessionId: string,
  runtimeReadModel: RustRuntimeReadModelDto | undefined,
  generatedAt: number,
): TimelineProjectionArtifact[] {
  if (!sessionId) {
    return [];
  }
  const sessionEntry = resolveSessionRuntimeEntry(runtimeReadModel, sessionId);
  const turn = sessionEntry?.current_turn;
  const turnId = normalizeString(turn?.turn_id);
  if (!turnId || !Array.isArray(sessionEntry?.turn_items) || sessionEntry.turn_items.length === 0) {
    return [];
  }

  const acceptedAt = typeof turn?.accepted_at === 'number' ? turn.accepted_at : generatedAt;
  const turnStatus = normalizeString(turn?.status);
  const userMessage = normalizeString(turn?.user_message);
  const laneById = new Map(
    (sessionEntry?.worker_lanes || [])
      .map((lane) => [normalizeString(lane.lane_id), lane] as const)
      .filter(([laneId]) => laneId.length > 0),
  );
  const turnItems = [...(sessionEntry?.turn_items || [])]
    .filter((item) => normalizeString(item.item_id).length > 0)
    .sort((left, right) => {
      const leftSeq = typeof left.item_seq === 'number' ? left.item_seq : 0;
      const rightSeq = typeof right.item_seq === 'number' ? right.item_seq : 0;
      return leftSeq - rightSeq;
    });
  const suppressedCurrentTurnItemIds = buildSuppressedCurrentTurnItemIds(turnItems);
  const hasLiveStreamItem = turnItems.some((item) => {
    const itemKind = normalizeString(item.kind);
    const itemStatus = normalizeString(item.status);
    return (itemKind === 'assistant_stream' || itemKind === 'worker_stream')
      && (itemStatus === 'running' || itemStatus === 'pending');
  });
  const turnStillActive = turnStatus === 'accepted' || turnStatus === 'running';
  const streamingFallbackItemId = !turnStillActive || hasLiveStreamItem
    ? ''
    : [...turnItems]
      .reverse()
      .find((item) => {
        const itemKind = normalizeString(item.kind);
        const itemStatus = normalizeString(item.status);
        return (itemKind === 'assistant_phase' || itemKind === 'worker_phase')
          && (itemStatus === 'running' || itemStatus === 'pending');
      })
      ?.item_id
      ?.trim()
      || '';

  const executionItems = turnItems
    .map((item) => {
      const itemId = normalizeString(item.item_id);
      const itemKind = normalizeString(item.kind);
      if (isUserRequestTurnItemKind(itemKind)) {
        return null;
      }
      if (suppressedCurrentTurnItemIds.has(itemId)) {
        return null;
      }
      if (
        isExecutionPhaseTurnItemKind(itemKind)
        && itemId !== streamingFallbackItemId
        && item.worker_visible !== true
      ) {
        return null;
      }
      const laneId = normalizeString(item.lane_id);
      const lane = laneId ? laneById.get(laneId) : undefined;
      const workerId = normalizeString(item.worker_id)
        || normalizeString(lane?.worker_id)
        || '';
      const roleId = normalizeString(item.role_id)
        || normalizeString(lane?.role_id)
        || '';
      const timestamp = acceptedAt + (typeof item.item_seq === 'number' ? item.item_seq : 0);
      const workerTabs = item.worker_visible && roleId ? [roleId] : [];
      const isWorkerTurnItem = itemKind.startsWith('worker_');
      return {
        itemId,
        itemOrder: typeof item.item_seq === 'number' ? item.item_seq : 0,
        anchorEventSeq: 0,
        latestEventSeq: 0,
        cardStreamSeq: 0,
        timestamp,
        worker: roleId || workerId || undefined,
        threadVisible: isWorkerTurnItem ? false : item.thread_visible !== false,
        workerTabs,
        messageIds: [normalizeString(item.item_id)],
        message: buildCurrentTurnItemMessage(
          sessionId,
          turnId,
          turnStatus,
          timestamp,
          item,
          lane,
          workerId,
          streamingFallbackItemId,
        ),
      };
    })
    .filter((item): item is NonNullable<typeof item> => item !== null)
    .filter((item) => item.threadVisible || item.workerTabs.length > 0);

  const artifacts: TimelineProjectionArtifact[] = [];
  if (userMessage) {
    artifacts.push({
      artifactId: `rust-turn-user:${turnId}`,
      kind: 'message',
      displayOrder: 1,
      anchorEventSeq: 0,
      latestEventSeq: 0,
      cardStreamSeq: 0,
      timestamp: acceptedAt,
      threadVisible: true,
      workerTabs: [],
      messageIds: [`rust-turn-user:${turnId}`],
      message: createRustTimelineMessage({
        id: `rust-turn-user:${turnId}`,
        content: userMessage,
        timestamp: acceptedAt,
        sessionId,
        eventSeq: 0,
        role: 'user',
        type: 'user_input',
        metadata: {
          turnId,
          turnSeq: typeof turn?.turn_seq === 'number' ? turn.turn_seq : undefined,
        },
      }),
    });
  }

  if (executionItems.length === 0) {
    return artifacts;
  }

  artifacts.push({
    artifactId: `rust-turn:${turnId}`,
    kind: 'message',
    displayOrder: artifacts.length + 1,
    anchorEventSeq: 0,
    latestEventSeq: 0,
    cardStreamSeq: 0,
    timestamp: acceptedAt,
    threadVisible: true,
    workerTabs: Array.from(new Set(executionItems.flatMap((item) => item.workerTabs))),
    messageIds: [`rust-turn:${turnId}`],
    message: createRustTimelineMessage({
      id: `rust-turn:${turnId}`,
      content: '',
      timestamp: acceptedAt,
      sessionId,
      eventSeq: 0,
      role: 'assistant',
      type: 'progress',
      metadata: {
        turnId,
        turnSeq: typeof turn?.turn_seq === 'number' ? turn.turn_seq : undefined,
        currentTurnWorkerLanes: buildCurrentTurnWorkerLaneMetadata(sessionEntry),
      },
    }),
    executionItems,
  });

  return artifacts;
}

function resolveTimelineMessageRole(kind: string): Pick<Message, 'role' | 'type'> | null {
  if (kind === 'UserMessage') {
    return { role: 'user', type: 'user_input' };
  }
  if (kind === 'AssistantMessage') {
    return { role: 'assistant', type: 'text' };
  }
  return null;
}

function buildTimelineMessageArtifacts(
  sessionId: string,
  timeline: RustTimelineEntry[] | undefined,
): TimelineProjectionArtifact[] {
  if (!sessionId || !Array.isArray(timeline)) {
    return [];
  }
  return timeline
    .map((entry, index): TimelineProjectionArtifact | null => {
      const entrySessionId = normalizeString(entry.sessionId);
      if (entrySessionId !== sessionId) {
        return null;
      }
      const messageShape = resolveTimelineMessageRole(normalizeString(entry.kind));
      const parsedContent = parseSerializedMessageContent(normalizeString(entry.message));
      if (!messageShape || (!parsedContent.content && (!parsedContent.blocks || parsedContent.blocks.length === 0))) {
        return null;
      }
      const timestamp = normalizeNumber(entry.occurredAt, 0);
      const entryId = normalizeString(entry.entryId) || `timeline-${sessionId}-${timestamp}-${index}`;
      return {
        artifactId: `rust-timeline:${entryId}`,
        kind: 'message' as const,
        displayOrder: index + 1,
        anchorEventSeq: index + 1,
        latestEventSeq: index + 1,
        cardStreamSeq: 0,
        timestamp,
        threadVisible: true,
        workerTabs: [],
        messageIds: [`rust-timeline:${entryId}`],
        message: createRustTimelineMessage({
          id: `rust-timeline:${entryId}`,
          content: parsedContent.content,
          timestamp,
          sessionId,
          eventSeq: index + 1,
          role: messageShape.role,
          type: messageShape.type,
          rustEventType: normalizeString(entry.kind),
          blocks: parsedContent.blocks,
          metadata: {
            timelineEntryId: entryId,
          },
        }),
      } satisfies TimelineProjectionArtifact;
    })
    .filter((artifact): artifact is TimelineProjectionArtifact => artifact !== null);
}

function shouldIncludeCurrentTurnArtifacts(
  sessionId: string,
  runtimeReadModel: RustRuntimeReadModelDto | undefined,
): boolean {
  const sessionEntry = resolveSessionRuntimeEntry(runtimeReadModel, sessionId);
  const turnStatus = normalizeString(sessionEntry?.current_turn?.status);
  if (turnStatus === 'accepted' || turnStatus === 'running') {
    return true;
  }
  const hasWorkerLane = Array.isArray(sessionEntry?.worker_lanes) && sessionEntry.worker_lanes.length > 0;
  if (hasWorkerLane) {
    return true;
  }
  return (sessionEntry?.turn_items || []).some((item) => {
    const itemKind = normalizeString(item.kind);
    return item.worker_visible === true
      || itemKind === 'tool_call_started'
      || itemKind === 'tool_call_result'
      || itemKind === 'worker_tool_call_started'
      || itemKind === 'worker_tool_call_result'
      || itemKind === 'worker_completed';
  });
}

function resolveCurrentTurnAcceptedAt(
  runtimeReadModel: RustRuntimeReadModelDto | undefined,
  sessionId: string,
  generatedAt: number,
): number | null {
  const turn = resolveSessionRuntimeEntry(runtimeReadModel, sessionId)?.current_turn;
  if (!normalizeString(turn?.turn_id)) {
    return null;
  }
  const acceptedAt = normalizeNumber(turn?.accepted_at, NaN);
  return Number.isFinite(acceptedAt) ? acceptedAt : generatedAt;
}

function buildContractProjectionRenderEntries(
  artifacts: TimelineProjectionArtifact[],
  displayContext: 'thread' | 'worker',
  panelKey?: string,
): TimelineProjectionRenderEntry[] {
  const renderEntries: Array<{
    entryId: string;
    artifactId: string;
    executionItemId?: string;
    timestamp: number;
    displayOrder: number;
    itemOrder: number;
  }> = [];

  for (const artifact of artifacts) {
    const artifactVisible = displayContext === 'thread'
      ? artifact.threadVisible
      : Boolean(panelKey && artifact.workerTabs.includes(panelKey));
    const executionItems = Array.isArray(artifact.executionItems) ? artifact.executionItems : [];

    if (artifactVisible && executionItems.length === 0) {
      renderEntries.push({
        entryId: `artifact:${artifact.artifactId}`,
        artifactId: artifact.artifactId,
        timestamp: artifact.timestamp,
        displayOrder: typeof artifact.displayOrder === 'number' ? artifact.displayOrder : 0,
        itemOrder: 0,
      });
    }

    for (const item of executionItems) {
      const itemVisible = displayContext === 'thread'
        ? item.threadVisible
        : Boolean(panelKey && item.workerTabs.includes(panelKey));
      if (!itemVisible) {
        continue;
      }
      renderEntries.push({
        entryId: `item:${artifact.artifactId}:${item.itemId}`,
        artifactId: artifact.artifactId,
        executionItemId: item.itemId,
        timestamp: item.timestamp,
        displayOrder: typeof artifact.displayOrder === 'number' ? artifact.displayOrder : 0,
        itemOrder: typeof item.itemOrder === 'number' ? item.itemOrder : 0,
      });
    }
  }

  return renderEntries
    .sort((left, right) => {
      if (left.timestamp !== right.timestamp) {
        return left.timestamp - right.timestamp;
      }
      if (left.displayOrder !== right.displayOrder) {
        return left.displayOrder - right.displayOrder;
      }
      if (left.itemOrder !== right.itemOrder) {
        return left.itemOrder - right.itemOrder;
      }
      return left.entryId.localeCompare(right.entryId);
    })
    .map((entry) => ({
      entryId: entry.entryId,
      artifactId: entry.artifactId,
      ...(entry.executionItemId ? { executionItemId: entry.executionItemId } : {}),
    }));
}

function buildTimelineProjection(
  sessionId: string,
  generatedAt: number,
  payload: RustBootstrapDto,
): SessionTimelineProjection {
  const timelineArtifacts = buildTimelineMessageArtifacts(sessionId, payload.timeline);
  const currentTurnArtifacts = shouldIncludeCurrentTurnArtifacts(sessionId, payload.runtimeReadModel)
    ? buildCurrentTurnArtifacts(sessionId, payload.runtimeReadModel, generatedAt)
    : [];
  const currentTurnAcceptedAt = currentTurnArtifacts.length > 0
    ? resolveCurrentTurnAcceptedAt(payload.runtimeReadModel, sessionId, generatedAt)
    : null;
  const historyArtifacts = currentTurnAcceptedAt === null
    ? timelineArtifacts
    : timelineArtifacts.filter((artifact) => artifact.timestamp < currentTurnAcceptedAt);
  const artifacts = [...historyArtifacts, ...currentTurnArtifacts];
  const panelKeys = new Set<string>();
  for (const artifact of artifacts) {
    for (const panelKey of artifact.workerTabs) {
      if (typeof panelKey === 'string' && panelKey.trim()) {
        panelKeys.add(panelKey.trim());
      }
    }
  }
  const latestSequence = normalizeNumber(payload.runtimeReadModel?.meta?.latest_sequence, 0);

  return {
    schemaVersion: 'session-timeline-projection.v2',
    sessionId,
    updatedAt: generatedAt,
    lastAppliedEventSeq: latestSequence,
    artifacts,
    threadRenderEntries: buildContractProjectionRenderEntries(artifacts, 'thread'),
    workerRenderEntries: Array.from(panelKeys).reduce<Record<string, TimelineProjectionRenderEntry[]>>((acc, panelKey) => {
      acc[panelKey] = buildContractProjectionRenderEntries(artifacts, 'worker', panelKey);
      return acc;
    }, {}),
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
  const generatedAt = normalizeNumber(payload.generatedAt, Date.now());
  const sessions = normalizeRustSessions(payload, generatedAt);
  const selectedSessionId = normalizeString(options.sessionId)
    || normalizeString(payload.currentSession?.sessionId)
    || sessions[0]?.id
    || '';
  const currentSession = sessions.find((session) => session.id === selectedSessionId) ?? sessions[0];
  const workspace = resolveSelectedWorkspace(payload, options);
  const normalizedEvents = Array.isArray(payload.recentEvents)
    ? payload.recentEvents.map((event) => normalizeEventEnvelope(event)).filter((event): event is RustEventEnvelope => event !== null)
    : [];
  const assignments = buildAssignmentsFromRuntime(
    payload.runtimeReadModel,
    normalizedEvents,
    currentSession?.id || '',
  );
  const processingState = deriveProcessingState(
    payload.runtimeReadModel,
    currentSession?.id || '',
  );
  const state: AppState = {
    ...buildEmptyWorkspaceAppState(generatedAt),
    sessions,
    currentSession,
    currentSessionId: currentSession?.id || '',
    isProcessing: Boolean(processingState?.isProcessing),
    processingState,
    pendingChanges: Array.isArray(payload.pendingChanges) ? payload.pendingChanges : [],
    pendingChangesStateVersion: generatedAt,
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
      payload.runtimeReadModel,
      assignments,
      currentSession?.id || '',
      generatedAt,
    ),
  };
}

export function readRustTimelinePageMeta(rawPayload: unknown): {
  sessionId: string;
  hasMoreBefore: boolean;
  beforeCursor: string | null;
} {
  const payload = (rawPayload ?? {}) as RustTimelinePageDto;
  return {
    sessionId: normalizeString(payload.sessionId),
    hasMoreBefore: payload.hasMoreBefore === true,
    beforeCursor: normalizeString(payload.beforeCursor) || null,
  };
}
