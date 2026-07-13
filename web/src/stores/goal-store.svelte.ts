import type { CurrentGoalResponseDto } from '../shared/rust-backend-types';
import { RustDaemonClient } from '../shared/rust-daemon-client';
import { resolveAgentBaseUrl } from '../web/agent-api';

export interface GoalState {
  response: CurrentGoalResponseDto | null;
  loading: boolean;
  error: string | null;
}

interface InternalGoalState extends GoalState {
  sessionId: string;
  workspaceId: string;
  workspacePath: string;
  fetchGeneration: number;
  requestController: AbortController | null;
}

const EMPTY_GOAL_STATE: GoalState = {
  response: null,
  loading: false,
  error: null,
};
const GOAL_REQUEST_TIMEOUT_MS = 8_000;

let goalStates = $state<Record<string, InternalGoalState>>({});

function normalizeKey(value: string | null | undefined): string {
  return typeof value === 'string' ? value.trim() : '';
}

function goalScopeKey(
  workspaceId: string | null | undefined,
  sessionId: string | null | undefined,
): string {
  const sid = normalizeKey(sessionId);
  if (!sid) return '';
  const wid = normalizeKey(workspaceId);
  return wid ? `${wid}\u0000${sid}` : `session:${sid}`;
}

function createClient(): RustDaemonClient {
  return new RustDaemonClient(resolveAgentBaseUrl());
}

function writeGoalState(key: string, patch: Partial<InternalGoalState>): InternalGoalState {
  if (!goalStates[key]) {
    goalStates[key] = {
      ...EMPTY_GOAL_STATE,
      sessionId: '',
      workspaceId: '',
      workspacePath: '',
      fetchGeneration: 0,
      requestController: null,
    };
  }
  Object.assign(goalStates[key], patch);
  return goalStates[key];
}

export function ensureGoalState(
  sessionId: string | null | undefined,
  workspaceId: string | null | undefined,
  workspacePath: string | null | undefined,
): GoalState {
  const sid = normalizeKey(sessionId);
  const wid = normalizeKey(workspaceId);
  const path = normalizeKey(workspacePath);
  const key = goalScopeKey(wid, sid);
  if (!key) return EMPTY_GOAL_STATE;
  if (!goalStates[key]) {
    return writeGoalState(key, {
      ...EMPTY_GOAL_STATE,
      sessionId: sid,
      workspaceId: wid,
      workspacePath: path,
      fetchGeneration: 0,
    });
  }
  return writeGoalState(key, {
    sessionId: sid,
    workspaceId: wid,
    workspacePath: path || goalStates[key].workspacePath,
  });
}

export function getGoalState(
  sessionId: string | null | undefined,
  workspaceId: string | null | undefined,
): GoalState {
  const sid = normalizeKey(sessionId);
  if (!sid) return EMPTY_GOAL_STATE;
  const key = goalScopeKey(workspaceId, sessionId);
  return key && goalStates[key] ? goalStates[key] : EMPTY_GOAL_STATE;
}

export function applyCurrentGoalResponse(response: CurrentGoalResponseDto): void {
  const key = goalScopeKey(response.workspaceId, response.sessionId);
  if (!key) return;
  writeGoalState(key, {
    sessionId: normalizeKey(response.sessionId),
    workspaceId: normalizeKey(response.workspaceId),
    workspacePath: normalizeKey(response.workspacePath),
    response,
    loading: false,
    error: null,
  });
}

export async function refreshCurrentGoal(
  sessionId: string | null | undefined,
  workspaceId: string | null | undefined,
  workspacePath: string | null | undefined,
): Promise<void> {
  const state = ensureGoalState(sessionId, workspaceId, workspacePath) as InternalGoalState;
  if (!state.sessionId) return;
  if (state.requestController) return;
  const generation = state.fetchGeneration + 1;
  const key = goalScopeKey(state.workspaceId, state.sessionId);
  if (!key) return;
  const hasExistingResponse = Boolean(goalStates[key]?.response);
  writeGoalState(key, {
    loading: !hasExistingResponse,
    error: null,
    fetchGeneration: generation,
  });
  const controller = new AbortController();
  state.requestController = controller;
  const timeout = setTimeout(() => controller.abort('goal_request_timeout'), GOAL_REQUEST_TIMEOUT_MS);
  try {
    const response = await createClient().getCurrentGoal(
      state.sessionId,
      state.workspaceId,
      state.workspacePath,
      controller.signal,
    );
    const current = goalStates[key];
    if (!current || current.fetchGeneration !== generation) return;
    writeGoalState(key, {
      response,
      loading: false,
      error: null,
    });
  } catch (error) {
    const current = goalStates[key];
    if (!current || current.fetchGeneration !== generation) return;
    writeGoalState(key, {
      loading: false,
      error: controller.signal.aborted
        ? 'goal_request_timeout'
        : error instanceof Error ? error.message : String(error),
    });
  } finally {
    clearTimeout(timeout);
    const current = goalStates[key];
    if (current?.fetchGeneration === generation && current.requestController === controller) {
      current.requestController = null;
      current.loading = false;
    }
  }
}
