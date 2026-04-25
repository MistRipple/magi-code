import type { SessionBootstrapSnapshot } from '../session-bootstrap';
import type {
  AppState,
  ContentBlock,
  DispatchGroupLane,
  Message,
  OrchestrationRuntimeAssignmentSummary,
  OrchestrationRuntimeFailureRootCause,
  OrchestrationRuntimeOpsView,
  OrchestrationRuntimeRecoverySummary,
  OrchestrationRuntimeTimelineEntry,
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

interface RustExecutionGroupRuntimeSummary {
  mission_id?: string;
  latest_event_type?: string | null;
  current_status?: string | null;
  failed_dispatch_count?: number;
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

interface RustWorkerRuntimeSummary {
  worker_id?: string;
  tool_call_count?: number;
  failed_dispatch_count?: number;
  current_task_id?: string | null;
  latest_event_type?: string | null;
  current_status?: string | null;
  current_stage?: string | null;
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
  execution_chain_ref?: string | null;
  recovery_ref?: string | null;
  has_recoverable_chain?: boolean;
  recoverable_branch_count?: number;
  active_branches?: RustSessionRuntimeBranchSummary[];
  current_turn?: RustSessionRuntimeTurnSummary | null;
  turn_items?: RustSessionRuntimeTurnItemSummary[];
  worker_lanes?: RustSessionRuntimeTurnLaneSummary[];
}

interface RustSessionRuntimeTurnSummary {
  turn_id?: string;
  turn_seq?: number;
  accepted_at?: number | null;
  status?: string | null;
  user_message?: string | null;
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
  tool_arguments?: unknown;
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

interface RustSessionRuntimeBranchSummary {
  task_id?: string;
  worker_id?: string;
  status?: string;
  stage?: string;
  lease_id?: string | null;
  execution_intent_ref?: string | null;
  binding_lifecycle?: string | null;
  last_checkpoint_at?: number | null;
  is_primary?: boolean;
}

/** 后端 RecoveryDiagnosticSummaryEntry 的 TS 映射 */
interface RustRecoveryDiagnosticSummary {
  recovery_id?: string;
  event_count?: number;
  latest_stage?: string;
  latest_event_type?: string;
  latest_sequence?: number;
  latest_occurred_at?: number;
  workspace_id?: string | null;
  session_id?: string | null;
  mission_id?: string | null;
  assignment_id?: string | null;
  task_id?: string | null;
  worker_id?: string | null;
  execution_chain_ref?: string | null;
  diagnostic_summary?: string | null;
  current_status?: string;
}

/** 后端 RuntimeDiagnosticSummary 的 TS 映射 */
interface RustRuntimeDiagnosticSummary {
  running_execution_group_count?: number;
  failed_execution_group_count?: number;
  running_task_count?: number;
  failed_task_count?: number;
  running_assignment_count?: number;
  failed_assignment_count?: number;
  active_worker_count?: number;
  failed_worker_count?: number;
  blocked_tool_count?: number;
  failed_tool_count?: number;
  governance_total_count?: number;
  governance_allowed_count?: number;
  governance_needs_approval_count?: number;
  governance_blocked_count?: number;
  governance_rejected_count?: number;
  pending_recovery_count?: number;
  resumed_recovery_count?: number;
}

/** 后端 EventCategoryCounts 的 TS 映射 */
interface RustEventCategoryCounts {
  domain?: number;
  audit?: number;
  usage?: number;
  projection?: number;
  system?: number;
}

interface RustRuntimeReadModelDto {
  meta?: {
    latest_sequence?: number;
  };
  details?: {
    execution_groups?: RustExecutionGroupRuntimeSummary[];
    assignments?: RustAssignmentRuntimeSummary[];
    tasks?: RustTaskRuntimeSummary[];
    workers?: RustWorkerRuntimeSummary[];
    sessions?: RustSessionRuntimeSummary[];
  };
  overview?: {
    category_counts?: RustEventCategoryCounts;
    activity?: {
      active_execution_group_ids?: string[];
      active_task_ids?: string[];
    };
    diagnostics?: RustRuntimeDiagnosticSummary;
  };
  operations?: {
    attention?: {
      failed_execution_group_ids?: string[];
      failed_task_ids?: string[];
      failed_assignment_ids?: string[];
      failed_worker_ids?: string[];
      blocked_tool_names?: string[];
      pending_recovery_ids?: string[];
    };
    work_queues?: {
      running_execution_group_ids?: string[];
      running_task_ids?: string[];
      running_assignment_ids?: string[];
      active_worker_ids?: string[];
      pending_recovery_ids?: string[];
    };
    dispatch?: {
      total_dispatches?: number;
      resume_dispatches?: number;
      latest_dispatch_reason?: string | null;
      active_assignment_ids?: string[];
    };
    resume_observation?: {
      total_recoveries?: number;
      resume_command_count?: number;
      resume_dispatch_count?: number;
      mission_resumed_count?: number;
      worker_resumed_count?: number;
    };
  };
  recovery?: {
    active_recovery_ids?: string[];
    summaries?: RustRecoveryDiagnosticSummary[];
  };
}

interface RustBootstrapDto {
  generatedAt?: number;
  currentSession?: RustBootstrapSessionRecord | null;
  sessions?: RustBootstrapSessionRecord[];
  timeline?: RustTimelineEntry[];
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

function parseJsonObjectLike(value: unknown): Record<string, unknown> | null {
  const direct = normalizeObjectRecord(value);
  if (direct) {
    return direct;
  }
  if (typeof value !== 'string') {
    return null;
  }
  const trimmed = value.trim();
  if (!trimmed) {
    return null;
  }
  try {
    return normalizeObjectRecord(JSON.parse(trimmed));
  } catch {
    return null;
  }
}

function getRuntimeDetailEntries(
  runtimeReadModel: RustRuntimeReadModelDto | undefined,
  key: string,
): unknown[] {
  const details = normalizeObjectRecord(runtimeReadModel?.details);
  const entries = details?.[key];
  return Array.isArray(entries) ? entries : [];
}

function normalizeExecutionGroupRuntimeEntries(
  runtimeReadModel: RustRuntimeReadModelDto | undefined,
): RustExecutionGroupRuntimeSummary[] {
  return getRuntimeDetailEntries(runtimeReadModel, 'execution_groups')
    .map((entry) => normalizeObjectRecord(entry))
    .filter((entry): entry is Record<string, unknown> => entry !== null)
    .map((entry) => ({
      mission_id: normalizeString(entry.mission_id) || undefined,
      latest_event_type: normalizeString(entry.latest_event_type) || undefined,
      current_status: normalizeString(entry.current_status) || undefined,
      failed_dispatch_count: typeof entry.failed_dispatch_count === 'number'
        ? Math.floor(entry.failed_dispatch_count)
        : undefined,
      active_task_ids: normalizeStringArray(entry.active_task_ids),
    }));
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

function normalizeWorkerRuntimeEntries(
  runtimeReadModel: RustRuntimeReadModelDto | undefined,
): RustWorkerRuntimeSummary[] {
  return getRuntimeDetailEntries(runtimeReadModel, 'workers')
    .map((entry) => normalizeObjectRecord(entry))
    .filter((entry): entry is Record<string, unknown> => entry !== null)
    .map((entry) => ({
      worker_id: normalizeString(entry.worker_id) || undefined,
      tool_call_count: typeof entry.tool_call_count === 'number'
        ? Math.floor(entry.tool_call_count)
        : undefined,
      failed_dispatch_count: typeof entry.failed_dispatch_count === 'number'
        ? Math.floor(entry.failed_dispatch_count)
        : undefined,
      current_task_id: normalizeString(entry.current_task_id) || undefined,
      latest_event_type: normalizeString(entry.latest_event_type) || undefined,
      current_status: normalizeString(entry.current_status) || undefined,
      current_stage: normalizeString(entry.current_stage) || undefined,
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
      execution_chain_ref: normalizeString(entry.execution_chain_ref) || undefined,
      recovery_ref: normalizeString(entry.recovery_ref) || undefined,
      has_recoverable_chain: entry.has_recoverable_chain === true,
      recoverable_branch_count: typeof entry.recoverable_branch_count === 'number'
        ? Math.floor(entry.recoverable_branch_count)
        : undefined,
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
            last_checkpoint_at: typeof branch.last_checkpoint_at === 'number'
              ? Math.floor(branch.last_checkpoint_at)
              : undefined,
            is_primary: branch.is_primary === true,
          }))
        : [],
      current_turn: normalizeObjectRecord(entry.current_turn) as RustSessionRuntimeTurnSummary | null,
      turn_items: Array.isArray(entry.turn_items)
        ? entry.turn_items
          .map((item) => normalizeObjectRecord(item))
          .filter((item): item is Record<string, unknown> => item !== null)
          .map((item) => item as RustSessionRuntimeTurnItemSummary)
        : [],
      worker_lanes: Array.isArray(entry.worker_lanes)
        ? entry.worker_lanes
          .map((lane) => normalizeObjectRecord(lane))
          .filter((lane): lane is Record<string, unknown> => lane !== null)
          .map((lane) => lane as RustSessionRuntimeTurnLaneSummary)
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

function resolveEventSessionId(event: RustEventEnvelope): string {
  const payload = resolveEventPayload(event);
  return normalizeString(event.session_id) || normalizeString(payload.session_id);
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

function resolveEventToolCallId(event: RustEventEnvelope): string {
  const payload = resolveEventPayload(event);
  return normalizeString(payload.tool_call_id);
}

function resolveEventToolName(event: RustEventEnvelope): string {
  const payload = resolveEventPayload(event);
  return normalizeString(payload.tool_name);
}

function resolveEventToolKind(event: RustEventEnvelope): string {
  const payload = resolveEventPayload(event);
  return normalizeString(payload.tool_kind);
}

function resolveEventStatus(event: RustEventEnvelope): string {
  const payload = resolveEventPayload(event);
  return normalizeString(payload.status);
}

function buildTaskMissionLookup(
  runtimeReadModel: RustRuntimeReadModelDto | undefined,
): Map<string, string> {
  const taskMissionByTaskId = new Map<string, string>();
  for (const task of normalizeTaskRuntimeEntries(runtimeReadModel)) {
    const taskId = normalizeString(task.task_id);
    const missionId = normalizeString(task.mission_id);
    if (taskId && missionId) {
      taskMissionByTaskId.set(taskId, missionId);
    }
  }
  return taskMissionByTaskId;
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
    const sessionId = normalizeString(session.sessionId);
    if (!sessionId) {
      continue;
    }
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

function normalizeWorkerLaneStatus(
  status: string,
  failedDispatchCount = 0,
): DispatchGroupLane['status'] {
  const normalized = normalizeSubTaskStatus(status, failedDispatchCount);
  switch (normalized) {
    case 'awaiting_approval':
    case 'review_required':
    case 'blocked':
    case 'running':
    case 'completed':
    case 'failed':
    case 'pending':
      return normalized;
    case 'paused':
    case 'waiting_deps':
      return 'pending';
    case 'skipped':
      return 'cancelled';
    default:
      return 'pending';
  }
}

function normalizeDispatchGroupStatus(
  status: string,
  failedDispatchCount = 0,
): 'pending' | 'running' | 'completed' | 'failed' | 'cancelled' {
  const normalized = status.toLowerCase();
  if (normalized.includes('cancel')) {
    return 'cancelled';
  }
  const laneStatus = normalizeSubTaskStatus(status, failedDispatchCount);
  switch (laneStatus) {
    case 'completed':
    case 'skipped':
      return 'completed';
    case 'failed':
    case 'blocked':
      return 'failed';
    case 'awaiting_approval':
    case 'review_required':
    case 'running':
      return 'running';
    default:
      return 'pending';
  }
}

function deriveDispatchGroupStatus(
  lanes: DispatchGroupLane[],
  fallbackStatus: string,
  failedDispatchCount = 0,
): 'pending' | 'running' | 'completed' | 'failed' | 'cancelled' {
  if (lanes.some((lane) => lane.status === 'failed' || lane.status === 'blocked')) {
    return 'failed';
  }
  if (lanes.some((lane) => lane.status === 'running'
    || lane.status === 'awaiting_approval'
    || lane.status === 'review_required')) {
    return 'running';
  }
  if (lanes.some((lane) => lane.status === 'pending')) {
    return 'pending';
  }
  if (lanes.length > 0 && lanes.every((lane) => lane.status === 'cancelled')) {
    return 'cancelled';
  }
  if (lanes.length > 0 && lanes.every((lane) => lane.status === 'completed' || lane.status === 'cancelled')) {
    return lanes.some((lane) => lane.status === 'completed') ? 'completed' : 'cancelled';
  }
  return normalizeDispatchGroupStatus(fallbackStatus, failedDispatchCount);
}

function normalizeWorkerLiveActivity(worker: RustWorkerRuntimeSummary): string | undefined {
  const stage = normalizeString(worker.current_stage);
  if (stage) {
    return stage.replace(/_/g, ' ');
  }
  const eventType = normalizeString(worker.latest_event_type);
  return eventType || undefined;
}

function resolveDispatchArtifactTimestamp(
  missionId: string,
  taskIds: string[],
  workerIds: string[],
  events: RustEventEnvelope[],
  fallback: number,
): number {
  const relevantTaskIds = new Set(taskIds);
  const relevantWorkerIds = new Set(workerIds);
  let latestTimestamp = fallback;

  for (const event of events) {
    const eventMissionId = resolveEventMissionId(event);
    const eventTaskId = resolveEventTaskId(event);
    const payloadWorkerId = resolveEventWorkerId(event);
    const isRelevant = eventMissionId === missionId
      || relevantTaskIds.has(eventTaskId)
      || relevantWorkerIds.has(payloadWorkerId);
    if (!isRelevant) {
      continue;
    }
    latestTimestamp = Math.max(latestTimestamp, normalizeNumber(event.occurred_at, fallback));
  }

  return latestTimestamp;
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

function resolveSessionMissionIds(
  runtimeReadModel: RustRuntimeReadModelDto | undefined,
  sessionId: string,
  events: RustEventEnvelope[],
): string[] {
  const missionIds: string[] = [];
  const pushMissionId = (missionId: string) => {
    if (missionId && !missionIds.includes(missionId)) {
      missionIds.push(missionId);
    }
  };

  const activeSession = normalizeSessionRuntimeEntries(runtimeReadModel)
    .find((entry) => normalizeString(entry.session_id) === sessionId);
  for (const missionId of normalizeStringArray(activeSession?.active_execution_group_ids)) {
    pushMissionId(missionId);
  }

  const taskMissionByTaskId = buildTaskMissionLookup(runtimeReadModel);
  for (const taskId of normalizeStringArray(activeSession?.active_task_ids)) {
    pushMissionId(taskMissionByTaskId.get(taskId) || '');
  }

  for (const event of events) {
    const eventSessionId = resolveEventSessionId(event);
    if (eventSessionId !== sessionId) {
      continue;
    }
    const missionId = resolveEventMissionId(event)
      || taskMissionByTaskId.get(resolveEventTaskId(event))
      || '';
    pushMissionId(missionId);
  }

  return missionIds;
}

function deriveProcessingState(
  runtimeReadModel: RustRuntimeReadModelDto | undefined,
  sessionId: string,
): AppState['processingState'] {
  const { runningTaskIds } = buildSessionTaskStatusSummary(runtimeReadModel, sessionId);
  const isProcessing = runningTaskIds.length > 0;

  if (!isProcessing) {
    return null;
  }

  return {
    isProcessing: true,
    source: 'orchestrator',
    agent: 'orchestrator',
    startedAt: 0,
    pendingRequestIds: [],
  };
}

/**
 * 从后端 recovery.summaries 构建前端 OrchestrationRuntimeRecoverySummary。
 * 聚合所有恢复摘要条目，提取最新快照和任务统计。
 */
function deriveRecoverySummary(
  runtimeReadModel: RustRuntimeReadModelDto,
): OrchestrationRuntimeRecoverySummary | undefined {
  const summaries = runtimeReadModel.recovery?.summaries;
  const activeIds = normalizeStringArray(runtimeReadModel.recovery?.active_recovery_ids);
  if (!Array.isArray(summaries) || summaries.length === 0) {
    return undefined;
  }
  // 取最新的 recovery 摘要
  const sorted = [...summaries].sort(
    (a, b) => normalizeNumber(b.latest_occurred_at, 0) - normalizeNumber(a.latest_occurred_at, 0),
  );
  const latest = sorted[0];
  const pendingCount = activeIds.length;
  const completedCount = summaries.filter(
    (s) => normalizeString(s.current_status) === 'consumed'
      || normalizeString(s.current_status) === 'worker_resumed',
  ).length;

  return {
    continuationPolicy: pendingCount > 0 ? 'resumable' : 'none',
    continuationReason: latest.diagnostic_summary || undefined,
    latestSnapshotId: normalizeString(latest.recovery_id) || undefined,
    latestSnapshotCreatedAt: normalizeNumber(latest.latest_occurred_at, undefined as unknown as number) || undefined,
    pendingTaskCount: pendingCount,
    completedTaskCount: completedCount,
    runningTaskCount: summaries.filter(
      (s) => normalizeString(s.current_status) === 'ready',
    ).length,
  };
}

/**
 * 从后端 recentEvents 构建前端 OrchestrationRuntimeTimelineEntry[]。
 * 仅筛选与当前 session 相关的事件。
 */
function deriveRecentTimeline(
  recentEvents: RustEventEnvelope[],
  sessionId: string,
): OrchestrationRuntimeTimelineEntry[] {
  if (!Array.isArray(recentEvents) || recentEvents.length === 0) {
    return [];
  }
  return recentEvents
    .filter((event) => {
      const eventSessionId = normalizeString(event.session_id);
      // 保留匹配当前 session 的事件，或无 session 归属的系统事件
      return !eventSessionId || eventSessionId === sessionId;
    })
    .map((event) => ({
      eventId: normalizeString(event.event_id) || `evt-${normalizeNumber(event.sequence, 0)}`,
      seq: normalizeNumber(event.sequence, 0),
      timestamp: normalizeNumber(event.occurred_at, 0),
      type: normalizeString(event.event_type) || 'unknown',
      summary: buildEventSummary(event),
      diffCount: 0,
    }))
    .sort((a, b) => a.seq - b.seq);
}

/** 从 EventEnvelope 构建可读摘要 */
function buildEventSummary(event: RustEventEnvelope): string {
  const eventType = normalizeString(event.event_type);
  const parts: string[] = [];
  if (eventType) {
    parts.push(eventType);
  }
  const taskId = normalizeString(event.task_id);
  if (taskId) {
    parts.push(`task:${taskId}`);
  }
  const missionId = normalizeString(event.mission_id);
  if (missionId && !taskId) {
    parts.push(`mission:${missionId}`);
  }
  return parts.join(' ') || 'event';
}

/**
 * 从后端 operations.attention 构建 failureRootCause（仅在有失败时）。
 */
function deriveFailureRootCause(
  runtimeReadModel: RustRuntimeReadModelDto,
  generatedAt: number,
): OrchestrationRuntimeFailureRootCause | undefined {
  const attention = runtimeReadModel.operations?.attention;
  if (!attention) {
    return undefined;
  }
  const failedTaskIds = normalizeStringArray(attention.failed_task_ids);
  const failedWorkerIds = normalizeStringArray(attention.failed_worker_ids);
  const failedAssignmentIds = normalizeStringArray(attention.failed_assignment_ids);
  if (failedTaskIds.length === 0 && failedWorkerIds.length === 0 && failedAssignmentIds.length === 0) {
    return undefined;
  }
  const parts: string[] = [];
  if (failedTaskIds.length > 0) {
    parts.push(`${failedTaskIds.length} 个任务失败 (${failedTaskIds.join(', ')})`);
  }
  if (failedWorkerIds.length > 0) {
    parts.push(`${failedWorkerIds.length} 个 Worker 失败`);
  }
  if (failedAssignmentIds.length > 0) {
    parts.push(`${failedAssignmentIds.length} 个分配失败`);
  }
  return {
    summary: parts.join('; '),
    taskId: failedTaskIds[0] || undefined,
    assignmentId: failedAssignmentIds[0] || undefined,
    occurredAt: generatedAt,
  };
}

/**
 * 构建 opsView：聚合 recovery、timeline、diagnostics 等后端已有数据。
 */
function deriveOpsView(
  runtimeReadModel: RustRuntimeReadModelDto,
  recentEvents: RustEventEnvelope[],
  sessionId: string,
  generatedAt: number,
): OrchestrationRuntimeOpsView | null {
  const recovery = deriveRecoverySummary(runtimeReadModel);
  const recentTimeline = deriveRecentTimeline(recentEvents, sessionId);
  const failureRootCause = deriveFailureRootCause(runtimeReadModel, generatedAt);
  const diagnostics = runtimeReadModel.overview?.diagnostics;
  const categoryCounts = runtimeReadModel.overview?.category_counts;

  // 计算总事件数
  const eventCount = (categoryCounts?.domain ?? 0)
    + (categoryCounts?.audit ?? 0)
    + (categoryCounts?.usage ?? 0)
    + (categoryCounts?.projection ?? 0)
    + (categoryCounts?.system ?? 0);

  // 没有任何有意义数据时返回 null，避免空壳
  const hasContent = recentTimeline.length > 0
    || recovery !== undefined
    || failureRootCause !== undefined
    || eventCount > 0
    || (diagnostics && (
      (diagnostics.running_task_count ?? 0) > 0
      || (diagnostics.failed_task_count ?? 0) > 0
      || (diagnostics.active_worker_count ?? 0) > 0
    ));
  if (!hasContent) {
    return null;
  }

  return {
    scope: {
      sessionId,
    },
    timelinePath: 'bootstrap',
    eventCount,
    diffCount: 0,
    recentTimeline,
    recentStateDiffs: [],
    recovery,
    failureRootCause,
  };
}

function deriveRuntimeState(
  runtimeReadModel: RustRuntimeReadModelDto | undefined,
  assignments: OrchestrationRuntimeAssignmentSummary[],
  recentEvents: RustEventEnvelope[],
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
  const status = runningTaskIds.length > 0
    ? 'running'
    : hasRecoverableChain
      ? 'paused'
      : failedTaskIds.length > 0
        ? 'failed'
        : activeSession
          ? normalizeTaskStatus(normalizeString(activeSession.root_task_status || activeSession.current_status)) === 'completed'
            ? 'completed'
            : 'idle'
          : 'idle';

  const opsView = deriveOpsView(runtimeReadModel, recentEvents, sessionId, generatedAt);

  return {
    sessionId: sessionId || undefined,
    status,
    phase: runningTaskIds.length > 0 ? 'execute' : (hasRecoverableChain ? 'paused' : 'idle'),
    errors: failedTaskIds.length > 0 ? failedTaskIds.map((id) => `task_failed:${id}`) : [],
    statusChangedAt: normalizeNumber(activeSession?.last_update, generatedAt),
    lastEventAt: normalizeNumber(activeSession?.last_update, generatedAt),
    canResume: hasRecoverableChain,
    runtimeReason: activeSession?.latest_event_type || undefined,
    assignments,
    chain: activeSession?.execution_chain_ref
      ? {
          chainId: activeSession.execution_chain_ref,
          status: activeSession.root_task_status || activeSession.current_status || 'unknown',
          recoverable: hasRecoverableChain && recoverableBranchCount > 0,
          attempt: 1,
          createdAt: normalizeNumber(activeSession.last_update, generatedAt),
          updatedAt: normalizeNumber(activeSession.last_update, generatedAt),
        }
      : undefined,
    opsView,
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
    isStreaming: false,
    isComplete: true,
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
        const toolArguments = parseJsonObjectLike(tc.arguments) || {};
        const toolStatus = turnItemStatusToToolStatus(
          typeof tc.status === 'string' ? tc.status : 'success',
        );
        return {
          type: 'tool_call' as const,
          content,
          id: `tool-block-${i}`,
          toolCall: {
            id: typeof tc.id === 'string' ? tc.id : `tc-${i}`,
            name: typeof tc.name === 'string' ? tc.name : 'unknown',
            arguments: toolArguments,
            status: toolStatus,
            result: typeof tc.result === 'string' ? tc.result : undefined,
            error: typeof tc.error === 'string' ? tc.error : undefined,
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
  const sessionTimeline = timeline.filter((entry) => normalizeString(entry.sessionId) === sessionId);
  for (const [index, entry] of sessionTimeline.entries()) {
    const entryId = normalizeString(entry.entryId);
    const message = normalizeString(entry.message);
    if (!entryId || !message) {
      continue;
    }
    const timestamp = normalizeNumber(entry.occurredAt, Date.now());
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

function buildDispatchArtifacts(
  sessionId: string,
  runtimeReadModel: RustRuntimeReadModelDto | undefined,
  events: RustEventEnvelope[],
  generatedAt: number,
  displayOrderOffset: number,
): TimelineProjectionArtifact[] {
  if (!runtimeReadModel || !sessionId) {
    return [];
  }
  const executionGroups = normalizeExecutionGroupRuntimeEntries(runtimeReadModel);
  const executionGroupByMission = new Map<string, RustExecutionGroupRuntimeSummary>();
  for (const entry of executionGroups) {
    const missionId = normalizeString(entry.mission_id);
    if (missionId) {
      executionGroupByMission.set(missionId, entry);
    }
  }

  const tasks = normalizeTaskRuntimeEntries(runtimeReadModel);
  const taskById = new Map<string, RustTaskRuntimeSummary>();
  for (const task of tasks) {
    const taskId = normalizeString(task.task_id);
    if (taskId) {
      taskById.set(taskId, task);
    }
  }
  const activeMissionIds = resolveSessionMissionIds(runtimeReadModel, sessionId, events);
  if (activeMissionIds.length === 0) {
    return [];
  }

  const workers = normalizeWorkerRuntimeEntries(runtimeReadModel)
    .filter((worker) => normalizeString(worker.worker_id).length > 0);
  if (workers.length === 0) {
    return [];
  }

  const lookups = buildLookupMaps(events);
  const artifacts: TimelineProjectionArtifact[] = [];

  for (const [index, missionId] of activeMissionIds.entries()) {
    const executionGroup = executionGroupByMission.get(missionId);
    const activeTaskIds = executionGroup?.active_task_ids?.length
      ? executionGroup.active_task_ids
      : tasks
        .filter((task) => normalizeString(task.mission_id) === missionId)
        .map((task) => normalizeString(task.task_id))
        .filter((taskId) => taskId.length > 0);
    const activeTaskIdSet = new Set(activeTaskIds);
    const lanes = workers
      .filter((worker) => {
        const currentTaskId = normalizeString(worker.current_task_id);
        if (!currentTaskId) {
          return false;
        }
        if (activeTaskIdSet.has(currentTaskId)) {
          return true;
        }
        return normalizeString(taskById.get(currentTaskId)?.mission_id) === missionId;
      })
      .sort((left, right) => normalizeString(left.worker_id).localeCompare(normalizeString(right.worker_id)))
      .map((worker): DispatchGroupLane => {
        const workerId = normalizeString(worker.worker_id);
        const currentTaskId = normalizeString(worker.current_task_id);
        const currentTask = currentTaskId ? taskById.get(currentTaskId) : undefined;
        const status = currentTask
          ? normalizeWorkerLaneStatus(
            normalizeString(currentTask.current_status),
            typeof currentTask.failed_dispatch_count === 'number' ? currentTask.failed_dispatch_count : 0,
          )
          : normalizeWorkerLaneStatus(
            normalizeString(executionGroup?.current_status),
            typeof executionGroup?.failed_dispatch_count === 'number' ? executionGroup.failed_dispatch_count : 0,
          );
        const title = (currentTaskId && lookups.taskTitles.get(currentTaskId))
          || workerId;

        return {
          laneId: `${missionId}:${workerId}`,
          laneVersion: 1,
          worker: workerId,
          title,
          status,
          liveActivity: normalizeWorkerLiveActivity(worker),
          toolUseCount: typeof worker.tool_call_count === 'number' ? worker.tool_call_count : undefined,
          jumpTarget: { workerTabId: workerId },
        };
      });

    if (lanes.length === 0) {
      continue;
    }

    const workerIds = lanes
      .map((lane) => normalizeString(lane.worker))
      .filter((workerId) => workerId.length > 0);
    const summaryTitle = lookups.missionTitles.get(missionId) || shortenId(missionId, 'mission');
    const timestamp = resolveDispatchArtifactTimestamp(
      missionId,
      activeTaskIds,
      workerIds,
      events,
      generatedAt,
    );
    const block: ContentBlock = {
      type: 'dispatch_group',
      content: '',
      blockId: `dispatch-group-${missionId}`,
      dispatchWaveId: missionId,
      status: deriveDispatchGroupStatus(
        lanes,
        normalizeString(executionGroup?.current_status),
        typeof executionGroup?.failed_dispatch_count === 'number' ? executionGroup.failed_dispatch_count : 0,
      ),
      summaryText: `${summaryTitle} · ${lanes.length} 个 worker`,
      lanes,
    };
    const messageId = `rust-dispatch:${missionId}`;

    artifacts.push({
      artifactId: messageId,
      kind: 'message',
      displayOrder: displayOrderOffset + index + 1,
      anchorEventSeq: 0,
      latestEventSeq: 0,
      cardStreamSeq: 0,
      timestamp,
      dispatchWaveId: missionId,
      threadVisible: true,
      workerTabs: workerIds,
      messageIds: [messageId],
      message: createRustTimelineMessage({
        id: messageId,
        content: '',
        timestamp,
        sessionId,
        eventSeq: 0,
        role: 'assistant',
        type: 'text',
        blocks: [block],
      }),
    });
  }

  return artifacts;
}

function buildProjectionRenderEntries(
  artifacts: TimelineProjectionArtifact[],
  displayContext: 'thread' | 'worker',
  workerId?: string,
): Array<{ entryId: string; artifactId: string }> {
  return artifacts
    .filter((artifact) => displayContext === 'thread'
      ? artifact.threadVisible
      : Boolean(workerId && artifact.workerTabs.includes(workerId)))
    .map((artifact) => ({
      entryId: `artifact:${artifact.artifactId}`,
      artifactId: artifact.artifactId,
    }));
}

function buildWorkerRenderEntriesFromArtifacts(
  artifacts: TimelineProjectionArtifact[],
): Record<string, Array<{ entryId: string; artifactId: string }>> {
  const workers = new Set<string>();
  for (const artifact of artifacts) {
    for (const workerId of artifact.workerTabs) {
      if (workerId) {
        workers.add(workerId);
      }
    }
  }
  return Object.fromEntries(
    [...workers].map((workerId) => [
      workerId,
      buildProjectionRenderEntries(artifacts, 'worker', workerId),
    ]),
  );
}

function toolStatusLooksTerminalError(normalizedStatus: string): boolean {
  return normalizedStatus.includes('fail')
    || normalizedStatus.includes('error')
    || normalizedStatus.includes('cancel')
    || normalizedStatus.includes('reject')
    || normalizedStatus.includes('block')
    || normalizedStatus.includes('deny')
    || normalizedStatus.includes('approval')
    || normalizedStatus.includes('abort')
    || normalizedStatus.includes('kill')
    || normalizedStatus.includes('timeout');
}

function turnItemStatusToToolStatus(status: string): 'pending' | 'running' | 'success' | 'error' {
  const normalized = status.toLowerCase();
  if (toolStatusLooksTerminalError(normalized)) {
    return 'error';
  }
  if (normalized.includes('running') || normalized.includes('pending')) {
    return normalized.includes('running') ? 'running' : 'pending';
  }
  return 'success';
}

function turnWorkerStatusToLaneStatus(status: string): DispatchGroupLane['status'] {
  switch (status.toLowerCase()) {
    case 'running':
    case 'verifying':
    case 'repairing':
      return 'running';
    case 'blocked':
    case 'awaiting_approval':
      return 'awaiting_approval';
    case 'completed':
    case 'skipped':
      return 'completed';
    case 'failed':
      return 'failed';
    case 'cancelled':
      return 'cancelled';
    default:
      return 'pending';
  }
}

function buildCurrentTurnArtifacts(
  sessionId: string,
  runtimeReadModel: RustRuntimeReadModelDto | undefined,
  generatedAt: number,
): TimelineProjectionArtifact[] {
  const session = normalizeSessionRuntimeEntries(runtimeReadModel)
    .find((entry) => normalizeString(entry.session_id) === sessionId);
  const turn = session?.current_turn;
  const turnId = normalizeString(turn?.turn_id);
  if (!turnId) {
    return [];
  }
  const items = Array.isArray(session?.turn_items)
    ? session.turn_items
      .filter((item) => normalizeString(item.item_id).length > 0)
      .sort((left, right) => normalizeNumber(left.item_seq, 0) - normalizeNumber(right.item_seq, 0))
    : [];
  if (items.length === 0) {
    return [];
  }
  const finalItemIds = new Set(
    items
      .filter((item) => normalizeString(item.kind) === 'assistant_final')
      .map((item) => normalizeString(item.item_id)),
  );
  const hasAssistantFinal = finalItemIds.size > 0;
  const workerLaneById = new Map(
    (Array.isArray(session?.worker_lanes) ? session.worker_lanes : [])
      .map((lane) => [normalizeString(lane.lane_id), lane] as const)
      .filter(([laneId]) => laneId.length > 0),
  );
  const toolItemsByCallId = new Map<string, RustSessionRuntimeTurnItemSummary>();
  for (const item of items) {
    const kind = normalizeString(item.kind);
    if (kind !== 'tool_call_started' && kind !== 'tool_call_result') {
      continue;
    }
    const toolCallId = normalizeString(item.tool_call_id) || normalizeString(item.item_id);
    const current = toolItemsByCallId.get(toolCallId);
    if (!current || kind === 'tool_call_result') {
      toolItemsByCallId.set(toolCallId, item);
    }
  }
  const toolResultItemIds = new Set(
    [...toolItemsByCallId.values()].map((item) => normalizeString(item.item_id)),
  );

  const artifacts: TimelineProjectionArtifact[] = [];
  for (const item of items) {
    const kind = normalizeString(item.kind);
    const itemId = normalizeString(item.item_id);
    if (!itemId) {
      continue;
    }
    if (kind === 'assistant_stream' && hasAssistantFinal) {
      continue;
    }
    if ((kind === 'tool_call_started' || kind === 'tool_call_result') && !toolResultItemIds.has(itemId)) {
      continue;
    }
    const laneId = normalizeString(item.lane_id);
    const workerId = normalizeString(item.worker_id)
      || normalizeString(workerLaneById.get(laneId)?.worker_id);
    const timestamp = normalizeNumber(turn?.accepted_at, generatedAt) + normalizeNumber(item.item_seq, 0);
    const threadVisible = kind === 'user_message'
      || item.thread_visible === true
      || kind === 'assistant_stream'
      || kind === 'assistant_final'
      || kind === 'assistant_phase'
      || kind === 'tool_call_started'
      || kind === 'tool_call_result';
    const workerVisible = item.worker_visible === true || Boolean(workerId && laneId);
    const workerTabs = workerVisible && workerId ? [workerId] : [];
    let role: Message['role'] = 'assistant';
    let type: Message['type'] = 'text';
    let source: Message['source'] = 'orchestrator';
    let blocks: ContentBlock[] | undefined;
    const content = normalizeString(item.content) || normalizeString(item.title);

    if (kind === 'user_message') {
      role = 'user';
      type = 'user_input';
      source = 'user';
    } else if (kind === 'assistant_phase') {
      blocks = [{
        type: 'thinking',
        content,
        thinking: { content, isComplete: normalizeString(item.status) !== 'running' },
      }];
    } else if (kind === 'tool_call_started' || kind === 'tool_call_result') {
      const toolName = normalizeString(item.tool_name) || normalizeString(item.title) || 'tool';
      const status = turnItemStatusToToolStatus(normalizeString(item.tool_status) || normalizeString(item.status));
      const toolArguments = parseJsonObjectLike(item.tool_arguments) || {};
      blocks = [{
        type: 'tool_call',
        content: '',
        toolCall: {
          id: normalizeString(item.tool_call_id) || itemId,
          name: toolName,
          arguments: toolArguments,
          status,
          result: normalizeString(item.tool_result) || undefined,
          error: normalizeString(item.tool_error) || undefined,
        },
      }];
    } else if (kind.startsWith('worker_') && workerId) {
      const lane = workerLaneById.get(laneId);
      const laneTitle = normalizeString(lane?.title) || normalizeString(item.title) || workerId;
      blocks = [{
        type: 'dispatch_group',
        content: '',
        blockId: `turn-worker-${itemId}`,
        dispatchWaveId: turnId,
        status: turnWorkerStatusToLaneStatus(normalizeString(item.status)),
        summaryText: laneTitle,
        lanes: [{
          laneId: laneId || `lane-${workerId}`,
          laneVersion: 1,
          worker: workerId,
          title: laneTitle,
          status: turnWorkerStatusToLaneStatus(normalizeString(lane?.status) || normalizeString(item.status)),
          tasks: [{
            taskId: normalizeString(item.task_id) || normalizeString(lane?.task_id),
            title: laneTitle,
            status: turnWorkerStatusToLaneStatus(normalizeString(lane?.status) || normalizeString(item.status)),
            isCurrent: true,
            seq: normalizeNumber(item.lane_seq, 0),
          }],
          jumpTarget: { workerTabId: workerId },
        }],
      }];
    }

    const artifactId = `turn:${turnId}:${itemId}`;
    artifacts.push({
      artifactId,
      kind: kind.includes('tool') ? 'tool' : 'message',
      displayOrder: normalizeNumber(turn?.turn_seq, 0) * 1000 + normalizeNumber(item.item_seq, 0),
      anchorEventSeq: 0,
      latestEventSeq: 0,
      cardStreamSeq: normalizeNumber(item.item_seq, 0),
      timestamp,
      laneId: laneId || undefined,
      worker: workerId || undefined,
      threadVisible,
      workerTabs,
      messageIds: [artifactId],
      message: createRustTimelineMessage({
        id: artifactId,
        content,
        timestamp,
        sessionId,
        eventSeq: 0,
        role,
        type,
        blocks,
        source,
        metadata: {
          turnId,
          turnItemId: itemId,
          turnItemKind: kind,
          laneId: laneId || undefined,
          workerId: workerId || undefined,
        },
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
    case 'session.continue.executed':
      return '已继续当前会话执行链';
    case 'recovery.resume.executed':
      return '已执行恢复流程';
    case 'mission.created':
      return '已创建任务链';
    case 'assignment.created':
      return '已创建执行分配';
    case 'task.created':
      return '已创建任务';
    case 'task.dispatched':
      return '任务开始执行';
    case 'task.completed':
      return '任务已完成';
    case 'task.failed':
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

function normalizeToolArtifactStatus(status: string, eventType: string): 'pending' | 'running' | 'success' | 'error' {
  const normalized = status.toLowerCase();
  if (toolStatusLooksTerminalError(normalized)) {
    return 'error';
  }
  if (normalized.includes('success') || normalized.includes('complete') || normalized.includes('succeed')) {
    return 'success';
  }
  if (normalized.includes('run')) {
    return 'running';
  }
  return eventType === 'task.tool.invoked' ? 'running' : 'pending';
}

function normalizeToolArtifactSource(toolKind: string): 'builtin' | 'mcp' | 'skill' {
  const normalized = toolKind.toLowerCase();
  if (normalized.includes('mcp')) {
    return 'mcp';
  }
  if (normalized.includes('skill')) {
    return 'skill';
  }
  return 'builtin';
}

function summarizeToolArtifactResult(toolName: string, status: 'pending' | 'running' | 'success' | 'error'): string {
  const normalizedToolName = toolName || 'tool';
  switch (status) {
    case 'success':
      return `${normalizedToolName} 已执行成功`;
    case 'error':
      return `${normalizedToolName} 执行失败`;
    case 'running':
      return `${normalizedToolName} 正在执行`;
    default:
      return `${normalizedToolName} 等待执行`;
  }
}

function buildToolArtifacts(
  sessionId: string,
  runtimeReadModel: RustRuntimeReadModelDto | undefined,
  events: RustEventEnvelope[],
  displayOrderOffset: number,
): TimelineProjectionArtifact[] {
  if (!sessionId) {
    return [];
  }

  const { taskEntries } = resolveSessionTaskEntries(runtimeReadModel, sessionId);
  const relevantTaskIds = new Set(
    taskEntries
      .map((entry) => normalizeString(entry.task_id))
      .filter((taskId) => taskId.length > 0),
  );
  const relevantMissionIds = new Set(resolveSessionMissionIds(runtimeReadModel, sessionId, events));
  const taskMissionByTaskId = buildTaskMissionLookup(runtimeReadModel);

  type ToolArtifactAggregate = {
    toolCallId: string;
    toolName: string;
    toolSource: 'builtin' | 'mcp' | 'skill';
    taskId: string;
    missionId: string;
    workerId: string;
    status: 'pending' | 'running' | 'success' | 'error';
    anchorEventSeq: number;
    latestEventSeq: number;
    timestamp: number;
    rustEventType: string;
  };

  const groups = new Map<string, ToolArtifactAggregate>();
  const relevantEvents = events
    .filter((event) => {
      const eventType = normalizeString(event.event_type);
      if (eventType !== 'task.tool.invoked' && eventType !== 'tool.invoked' && eventType !== 'tool.usage.recorded') {
        return false;
      }
      const eventSessionId = resolveEventSessionId(event);
      if (eventSessionId) {
        return eventSessionId === sessionId;
      }
      const eventMissionId = resolveEventMissionId(event)
        || taskMissionByTaskId.get(resolveEventTaskId(event))
        || '';
      if (eventMissionId && relevantMissionIds.has(eventMissionId)) {
        return true;
      }
      const eventTaskId = resolveEventTaskId(event);
      return Boolean(eventTaskId && relevantTaskIds.has(eventTaskId));
    })
    .sort((left, right) => normalizeNumber(left.sequence, 0) - normalizeNumber(right.sequence, 0));

  for (const event of relevantEvents) {
    const toolCallId = resolveEventToolCallId(event);
    const eventType = normalizeString(event.event_type);
    if (!toolCallId) {
      continue;
    }
    const status = normalizeToolArtifactStatus(resolveEventStatus(event), eventType);
    const taskId = resolveEventTaskId(event);
    const missionId = resolveEventMissionId(event) || taskMissionByTaskId.get(taskId) || '';
    const timestamp = normalizeNumber(event.occurred_at, Date.now());
    const sequence = normalizeNumber(event.sequence, 0);
    const current = groups.get(toolCallId);
    groups.set(toolCallId, {
      toolCallId,
      toolName: resolveEventToolName(event) || current?.toolName || 'tool',
      toolSource: normalizeToolArtifactSource(resolveEventToolKind(event) || current?.toolSource || 'builtin'),
      taskId: taskId || current?.taskId || '',
      missionId: missionId || current?.missionId || '',
      workerId: resolveEventWorkerId(event) || current?.workerId || '',
      status,
      anchorEventSeq: current?.anchorEventSeq ?? sequence,
      latestEventSeq: sequence,
      timestamp,
      rustEventType: eventType || current?.rustEventType || 'tool.invoked',
    });
  }

  return [...groups.values()]
    .sort((left, right) => left.timestamp - right.timestamp)
    .map((entry, index): TimelineProjectionArtifact => {
      const resultSummary = summarizeToolArtifactResult(entry.toolName, entry.status);
      const standardized = entry.status === 'success' || entry.status === 'error'
        ? {
            schemaVersion: 'tool-result.v1' as const,
            source: entry.toolSource,
            toolName: entry.toolName,
            toolCallId: entry.toolCallId,
            status: entry.status === 'error' ? 'error' as const : 'success' as const,
            message: resultSummary,
          }
        : undefined;
      const toolCallBlock: ContentBlock = {
        type: 'tool_call',
        content: '',
        toolCall: {
          id: entry.toolCallId,
          name: entry.toolName,
          arguments: {},
          status: entry.status,
          standardized: standardized || undefined,
        },
      };
      const blocks: ContentBlock[] = [toolCallBlock];
      if (entry.status === 'success' || entry.status === 'error') {
        blocks.push({
          type: 'tool_result',
          content: resultSummary,
          toolCall: {
            id: entry.toolCallId,
            name: entry.toolName,
            arguments: {},
            status: entry.status,
            result: entry.status === 'success' ? resultSummary : undefined,
            error: entry.status === 'error' ? resultSummary : undefined,
            standardized: standardized || undefined,
          },
        });
      }
      const artifactId = `rust-tool:${entry.toolCallId}`;
      return {
        artifactId,
        kind: 'message',
        displayOrder: displayOrderOffset + index + 1,
        anchorEventSeq: entry.anchorEventSeq,
        latestEventSeq: entry.latestEventSeq,
        cardStreamSeq: 0,
        timestamp: entry.timestamp,
        dispatchWaveId: entry.missionId || undefined,
        threadVisible: true,
        workerTabs: entry.workerId ? [entry.workerId] : [],
        messageIds: [artifactId],
        message: createRustTimelineMessage({
          id: artifactId,
          content: '',
          timestamp: entry.timestamp,
          sessionId,
          eventSeq: entry.latestEventSeq,
          role: 'assistant',
          type: 'text',
          blocks,
          source: entry.workerId || 'orchestrator',
          rustEventType: entry.rustEventType,
          metadata: {
            taskId: entry.taskId || undefined,
            missionId: entry.missionId || undefined,
            worker: entry.workerId || undefined,
          },
        }),
      };
    });
}

function buildEventArtifacts(
  sessionId: string,
  runtimeReadModel: RustRuntimeReadModelDto | undefined,
  events: RustEventEnvelope[],
): TimelineProjectionArtifact[] {
  const { taskEntries } = resolveSessionTaskEntries(runtimeReadModel, sessionId);
  const relevantTaskIds = new Set(
    taskEntries
      .map((entry) => normalizeString(entry.task_id))
      .filter((taskId) => taskId.length > 0),
  );
  const relevantMissionIds = new Set(
    taskEntries
      .map((entry) => normalizeString(entry.mission_id))
      .filter((missionId) => missionId.length > 0),
  );

  return events
    .filter((event) => {
      const eventSessionId = resolveEventSessionId(event);
      if (eventSessionId) {
        return eventSessionId === sessionId;
      }
      const missionId = resolveEventMissionId(event);
      if (missionId && relevantMissionIds.has(missionId)) {
        return true;
      }
      const taskId = resolveEventTaskId(event);
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
        'session.action.accepted',
        'tool.invoked', 'tool.usage.recorded',
        'task.llm.started', 'task.llm.completed', 'task.llm.delta',
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
  const currentTurnArtifacts = buildCurrentTurnArtifacts(sessionId, payload.runtimeReadModel, generatedAt);
  if (currentTurnArtifacts.length > 0) {
    const maxItemSeq = currentTurnArtifacts.reduce(
      (max, artifact) => Math.max(max, artifact.cardStreamSeq),
      0,
    );
    return {
      schemaVersion: 'session-timeline-projection.v2',
      sessionId,
      updatedAt: generatedAt,
      lastAppliedEventSeq: maxItemSeq,
      artifacts: currentTurnArtifacts,
      threadRenderEntries: buildProjectionRenderEntries(currentTurnArtifacts, 'thread'),
      workerRenderEntries: buildWorkerRenderEntriesFromArtifacts(currentTurnArtifacts),
    };
  }
  const recentEvents = Array.isArray(payload.recentEvents)
    ? payload.recentEvents.map((event) => normalizeEventEnvelope(event)).filter((event): event is RustEventEnvelope => event !== null)
    : [];
  const timelineArtifacts = buildTimelineEntryArtifacts(sessionId, payload.timeline);
  const eventArtifacts = buildEventArtifacts(sessionId, payload.runtimeReadModel, recentEvents);
  const toolArtifacts = buildToolArtifacts(
    sessionId,
    payload.runtimeReadModel,
    recentEvents,
    timelineArtifacts.length + eventArtifacts.length,
  );
  const dispatchArtifacts = buildDispatchArtifacts(
    sessionId,
    payload.runtimeReadModel,
    recentEvents,
    generatedAt,
    timelineArtifacts.length + eventArtifacts.length + toolArtifacts.length,
  );
  const artifacts = [...timelineArtifacts, ...eventArtifacts, ...toolArtifacts, ...dispatchArtifacts];
  const maxEventSeq = recentEvents.reduce((max, event) => Math.max(max, normalizeNumber(event.sequence, 0)), 0);
  const latestSequence = Math.max(
    normalizeNumber(payload.runtimeReadModel?.meta?.latest_sequence, 0),
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
  const generatedAt = normalizeNumber(payload.generatedAt, Date.now());
  const sessions = normalizeRustSessions(payload, generatedAt);
  const selectedSessionId = normalizeString(options.sessionId)
    || normalizeString(payload.currentSession?.sessionId)
    || '';
  const currentSession = selectedSessionId
    ? sessions.find((session) => session.id === selectedSessionId)
    : undefined;
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
      normalizedEvents,
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
