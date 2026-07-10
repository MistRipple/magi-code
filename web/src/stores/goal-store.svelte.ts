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

function timeoutAfter(ms: number): Promise<never> {
  return new Promise((_, reject) => {
    setTimeout(() => reject(new Error('goal_request_timeout')), ms);
  });
}

function writeGoalState(key: string, patch: Partial<InternalGoalState>): InternalGoalState {
  if (!goalStates[key]) {
    goalStates[key] = {
      ...EMPTY_GOAL_STATE,
      sessionId: '',
      workspaceId: '',
      workspacePath: '',
      fetchGeneration: 0,
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

export async function refreshCurrentGoal(
  sessionId: string | null | undefined,
  workspaceId: string | null | undefined,
  workspacePath: string | null | undefined,
): Promise<void> {
  const state = ensureGoalState(sessionId, workspaceId, workspacePath) as InternalGoalState;
  if (!state.sessionId) return;
  const generation = state.fetchGeneration + 1;
  const key = goalScopeKey(state.workspaceId, state.sessionId);
  if (!key) return;
  const hasExistingResponse = Boolean(goalStates[key]?.response);
  writeGoalState(key, {
    loading: !hasExistingResponse,
    error: null,
    fetchGeneration: generation,
  });
  setTimeout(() => {
    const current = goalStates[key];
    if (current?.loading) {
      writeGoalState(key, {
        loading: false,
        error: current.error ?? 'goal_request_timeout',
      });
    }
  }, GOAL_REQUEST_TIMEOUT_MS);
  try {
    const response = await Promise.race(
      [
        createClient().getCurrentGoal(
          state.sessionId,
          state.workspaceId,
          state.workspacePath,
        ),
        timeoutAfter(GOAL_REQUEST_TIMEOUT_MS),
      ],
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
      error: error instanceof Error ? error.message : String(error),
    });
  }
}
