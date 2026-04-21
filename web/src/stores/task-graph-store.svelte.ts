/**
 * Task Graph Store - fetches and caches Task Projection data from the Rust backend.
 *
 * Uses Svelte 5 runes for reactive state management.
 * Provides task tree data from TaskProjectionDto for the new Task-based UI.
 */

import type {
  TaskProjectionDto,
  TaskKind,
  TaskStatus,
  AssignmentLeaseDto,
} from '../shared/rust-backend-types';
import { RustDaemonClient } from '../shared/rust-daemon-client';
import { resolveAgentBaseUrl } from '../web/agent-api';
import { onBridgeMessage } from '../shared/bridges/bridge-runtime';

// ─── Types ──────────────────────────────────────────────────────────

export interface TaskGraphState {
  /** The full projection DTO from the backend, if loaded. */
  projection: TaskProjectionDto | null;
  /** Active leases keyed by task_id. */
  leases: Map<string, AssignmentLeaseDto>;
  /** Whether a fetch is in progress. */
  loading: boolean;
  /** Last error message, if any. */
  error: string | null;
  /** The root task ID being tracked, or null if none. */
  rootTaskId: string | null;
}

// ─── Singleton reactive state ───────────────────────────────────────

let _projection = $state<TaskProjectionDto | null>(null);
let _leases = $state<Map<string, AssignmentLeaseDto>>(new Map());
let _loading = $state(false);
let _error = $state<string | null>(null);
let _rootTaskId = $state<string | null>(null);
let _refreshTimer: ReturnType<typeof setInterval> | null = null;
let _sseUnsubscribe: (() => void) | null = null;
let _sseDebounceTimer: ReturnType<typeof setTimeout> | null = null;
const SSE_DEBOUNCE_MS = 300;

// ─── Helpers ────────────────────────────────────────────────────────

function createClient(): RustDaemonClient {
  return new RustDaemonClient(resolveAgentBaseUrl());
}

// ─── Public API ─────────────────────────────────────────────────────

/**
 * Returns the current reactive task graph state.
 * Read individual fields inside Svelte components to get fine-grained reactivity.
 */
export function getTaskGraphState(): TaskGraphState {
  return {
    get projection() { return _projection; },
    get leases() { return _leases; },
    get loading() { return _loading; },
    get error() { return _error; },
    get rootTaskId() { return _rootTaskId; },
  };
}

/**
 * Fetch the Task Projection for the given root task ID.
 * Also fetches active leases for any Running tasks in the projection.
 */
export async function fetchTaskProjection(rootTaskId: string): Promise<void> {
  _rootTaskId = rootTaskId;
  _loading = true;
  _error = null;

  try {
    const client = createClient();
    const projection = await client.getTaskProjection(rootTaskId);
    _projection = projection;

    // Fetch leases for running tasks
    const runningTaskIds = projection.running_tasks ?? [];
    const leaseMap = new Map<string, AssignmentLeaseDto>();
    const leasePromises = runningTaskIds.map(async (taskId) => {
      try {
        const lease = await client.getTaskLease(taskId);
        if (lease && lease.lease_status === 'Active') {
          leaseMap.set(taskId, lease);
        }
      } catch {
        // Lease fetch failure is non-critical; task may not have an active lease
      }
    });
    await Promise.all(leasePromises);
    _leases = leaseMap;

    _error = null;
  } catch (err) {
    _error = err instanceof Error ? err.message : String(err);
  } finally {
    _loading = false;
  }
}

/**
 * Refresh the currently tracked projection (if a rootTaskId is set).
 */
export async function refreshTaskProjection(): Promise<void> {
  if (_rootTaskId) {
    await fetchTaskProjection(_rootTaskId);
  }
}

// ─── SSE event subscription ─────────────────────────────────────────

/**
 * Subscribe to real-time task SSE events from the centralized bridge connection.
 * When task-domain events arrive (task.graph.created, task.status.changed, etc.),
 * triggers a debounced refresh of the task projection.
 */
function connectToSSE(): void {
  if (_sseUnsubscribe) {
    return; // already subscribed
  }
  _sseUnsubscribe = onBridgeMessage((message) => {
    if (message.type !== 'rustTaskEvent') {
      return;
    }
    // Only refresh if we have an active root task to track
    if (!_rootTaskId || _loading) {
      return;
    }
    // Debounce rapid event bursts (e.g. multiple status changes in quick succession)
    if (_sseDebounceTimer !== null) {
      clearTimeout(_sseDebounceTimer);
    }
    _sseDebounceTimer = setTimeout(() => {
      _sseDebounceTimer = null;
      refreshTaskProjection();
    }, SSE_DEBOUNCE_MS);
  });
}

/**
 * Unsubscribe from SSE events and cancel any pending debounce timer.
 */
function disconnectFromSSE(): void {
  if (_sseDebounceTimer !== null) {
    clearTimeout(_sseDebounceTimer);
    _sseDebounceTimer = null;
  }
  if (_sseUnsubscribe) {
    _sseUnsubscribe();
    _sseUnsubscribe = null;
  }
}

/**
 * Start auto-refreshing the task projection at the given interval (ms).
 * Also subscribes to real-time SSE events for immediate updates.
 * Calling this again replaces the previous timer.
 */
export function startAutoRefresh(intervalMs = 5000): void {
  stopAutoRefresh();
  connectToSSE();
  _refreshTimer = setInterval(() => {
    if (_rootTaskId && !_loading) {
      refreshTaskProjection();
    }
  }, intervalMs);
}

/**
 * Stop auto-refreshing and disconnect from SSE events.
 */
export function stopAutoRefresh(): void {
  if (_refreshTimer !== null) {
    clearInterval(_refreshTimer);
    _refreshTimer = null;
  }
  disconnectFromSSE();
}

/**
 * Clear the task graph state, stop any auto-refresh, and disconnect SSE.
 */
export function clearTaskGraph(): void {
  stopAutoRefresh();
  _projection = null;
  _leases = new Map();
  _loading = false;
  _error = null;
  _rootTaskId = null;
}

// ─── View helpers ───────────────────────────────────────────────────

/** Map TaskKind to a short display label. */
export function getTaskKindLabel(kind: TaskKind): string {
  switch (kind) {
    case 'Objective': return 'OBJ';
    case 'Phase': return 'PHA';
    case 'WorkPackage': return 'WPK';
    case 'Action': return 'ACT';
    case 'Validation': return 'VAL';
    case 'Repair': return 'RPR';
    case 'Decision': return 'DEC';
    default: return kind;
  }
}

/** Map TaskStatus to a CSS-friendly modifier string. */
export function getTaskStatusModifier(status: TaskStatus): string {
  switch (status) {
    case 'Ready': return 'ready';
    case 'Running': return 'running';
    case 'Completed': return 'completed';
    case 'Failed': return 'failed';
    case 'Blocked': return 'blocked';
    case 'Cancelled': return 'cancelled';
    case 'Skipped': return 'skipped';
    case 'Draft': return 'draft';
    case 'AwaitingApproval': return 'awaiting';
    case 'Verifying': return 'verifying';
    case 'Repairing': return 'repairing';
    default: return 'unknown';
  }
}

/** Map TaskKind to an icon name (matching existing Icon component names). */
export function getTaskKindIcon(kind: TaskKind): string {
  switch (kind) {
    case 'Objective': return 'target';
    case 'Phase': return 'list';
    case 'WorkPackage': return 'grid';
    case 'Action': return 'play';
    case 'Validation': return 'check-circle';
    case 'Repair': return 'wrench';
    case 'Decision': return 'alert-circle';
    default: return 'circle';
  }
}
