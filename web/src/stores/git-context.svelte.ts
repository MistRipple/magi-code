import {
  acceptWorkspaceGitContext,
  checkoutWorkspaceBranch,
  createWorkspaceBranch,
  fetchWorkspaceBranches,
  type GitBranch,
  type GitOperationResult,
  type WorkspaceBranchesResult,
  type WorkspaceVcsStatus,
} from '../web/agent-api';

export type GitContextOperation =
  | 'branchSwitch'
  | 'branchCreate'
  | 'contextAccept'
  | 'branchMerge'
  | 'branchDelete'
  | 'remoteBranchDelete'
  | 'worktreeCreate'
  | 'worktreeRemove';

export interface GitContextBinding {
  workspaceId?: string;
  workspacePath?: string;
  sessionId?: string;
}

export const gitContextState = $state({
  bindingKey: '',
  loading: false,
  loaded: false,
  isRepo: false,
  currentBranch: null as string | null,
  branches: [] as string[],
  remoteBranches: [] as string[],
  structuredBranches: [] as GitBranch[],
  status: null as WorkspaceVcsStatus | null,
  contextRevision: null as number | null,
  head: null as string | null,
  worktreePath: null as string | null,
  contextDrift: false,
  error: null as string | null,
  operation: null as GitContextOperation | null,
});

let refreshRequestSeq = 0;
let refreshInFlight: Promise<WorkspaceBranchesResult | null> | null = null;
let refreshInFlightKey = '';

function invalidateGitContextRefresh(): void {
  refreshRequestSeq += 1;
  refreshInFlight = null;
  refreshInFlightKey = '';
}

function normalizeBindingPart(value?: string | null): string {
  return typeof value === 'string' ? value.trim() : '';
}

export function gitContextBindingKey(binding: GitContextBinding): string {
  const workspaceId = normalizeBindingPart(binding.workspaceId);
  const workspacePath = normalizeBindingPart(binding.workspacePath);
  const sessionId = normalizeBindingPart(binding.sessionId);
  if (workspaceId) return `id:${workspaceId}\u0000${sessionId}`;
  if (workspacePath) return `path:${workspacePath}\u0000${sessionId}`;
  return '';
}

function normalizedBinding(binding: GitContextBinding): GitContextBinding {
  const workspaceId = normalizeBindingPart(binding.workspaceId);
  const workspacePath = normalizeBindingPart(binding.workspacePath);
  const sessionId = normalizeBindingPart(binding.sessionId);
  return {
    ...(workspaceId ? { workspaceId } : {}),
    ...(workspacePath ? { workspacePath } : {}),
    ...(sessionId ? { sessionId } : {}),
  };
}

function clearGitContext(bindingKey = ''): void {
  gitContextState.bindingKey = bindingKey;
  gitContextState.loading = false;
  gitContextState.loaded = false;
  gitContextState.isRepo = false;
  gitContextState.currentBranch = null;
  gitContextState.branches = [];
  gitContextState.remoteBranches = [];
  gitContextState.structuredBranches = [];
  gitContextState.status = null;
  gitContextState.contextRevision = null;
  gitContextState.head = null;
  gitContextState.worktreePath = null;
  gitContextState.contextDrift = false;
  gitContextState.error = null;
  gitContextState.operation = null;
}

function applyGitContextResult(result: WorkspaceBranchesResult, bindingKey: string): void {
  gitContextState.bindingKey = bindingKey;
  gitContextState.loading = false;
  gitContextState.loaded = true;
  gitContextState.isRepo = result.isRepo;
  gitContextState.currentBranch = result.currentBranch;
  gitContextState.branches = result.branches;
  gitContextState.remoteBranches = result.remoteBranches ?? [];
  gitContextState.structuredBranches = result.structuredBranches ?? [];
  gitContextState.status = result.status;
  gitContextState.contextRevision = result.sessionContext?.contextRevision ?? null;
  gitContextState.head = result.observation?.head ?? null;
  gitContextState.worktreePath = result.observation?.worktreePath ?? null;
  gitContextState.contextDrift = result.contextDrift === true;
  gitContextState.error = null;
}

export function clearGitContextError(): void {
  gitContextState.error = null;
}

export async function refreshGitContext(
  binding: GitContextBinding,
  options: { force?: boolean } = {},
): Promise<WorkspaceBranchesResult | null> {
  const normalized = normalizedBinding(binding);
  const bindingKey = gitContextBindingKey(normalized);
  if (!bindingKey) {
    invalidateGitContextRefresh();
    clearGitContext();
    return null;
  }
  if (!options.force && refreshInFlight && refreshInFlightKey === bindingKey) {
    return refreshInFlight;
  }
  if (gitContextState.bindingKey !== bindingKey) {
    clearGitContext(bindingKey);
  }
  gitContextState.loading = true;
  gitContextState.error = null;
  const requestSeq = ++refreshRequestSeq;
  const request = fetchWorkspaceBranches(normalized)
    .then((result) => {
      if (requestSeq === refreshRequestSeq && gitContextState.bindingKey === bindingKey) {
        applyGitContextResult(result, bindingKey);
      }
      return result;
    })
    .catch((error) => {
      if (requestSeq === refreshRequestSeq && gitContextState.bindingKey === bindingKey) {
        gitContextState.loading = false;
        if (!gitContextState.loaded) {
          gitContextState.loaded = true;
          gitContextState.isRepo = false;
        }
        gitContextState.error = error instanceof Error ? error.message : String(error);
      }
      throw error;
    })
    .finally(() => {
      if (refreshInFlight === request) {
        refreshInFlight = null;
        refreshInFlightKey = '';
      }
    });
  refreshInFlight = request;
  refreshInFlightKey = bindingKey;
  return request;
}

export function currentGitExpectedContext(): {
  contextRevision?: number;
  branch?: string | null;
  head?: string | null;
  worktreePath?: string | null;
} {
  return {
    contextRevision: gitContextState.contextRevision ?? undefined,
    branch: gitContextState.currentBranch,
    head: gitContextState.head,
    worktreePath: gitContextState.worktreePath,
  };
}

function applyOperationObservation(result: GitOperationResult): void {
  gitContextState.currentBranch = result.observation?.branch ?? gitContextState.currentBranch;
  gitContextState.head = result.observation?.head ?? gitContextState.head;
  gitContextState.worktreePath = result.observation?.worktreePath ?? gitContextState.worktreePath;
  gitContextState.contextRevision = result.sessionContext?.contextRevision ?? gitContextState.contextRevision;
}

function notifyWorkspaceGitChanged(reason: string): void {
  if (typeof window === 'undefined') return;
  window.dispatchEvent(new CustomEvent('magi:workspaceContentChanged', {
    detail: { reason, branch: gitContextState.currentBranch },
  }));
}

export async function runGitContextOperation(
  operation: GitContextOperation,
  binding: GitContextBinding,
  action: () => Promise<GitOperationResult>,
): Promise<GitOperationResult> {
  if (gitContextState.operation) {
    throw new Error('Git 操作正在执行');
  }
  const operationBindingKey = gitContextBindingKey(binding);
  if (!operationBindingKey || gitContextState.bindingKey !== operationBindingKey) {
    throw new Error('Git 工作区上下文已切换');
  }
  gitContextState.operation = operation;
  gitContextState.error = null;
  try {
    const result = await action();
    if (result.ok) {
      if (gitContextState.bindingKey === operationBindingKey) {
        applyOperationObservation(result);
        notifyWorkspaceGitChanged(operation);
        try {
          await refreshGitContext(binding, { force: true });
        } catch (error) {
          console.warn('[git-context] Git 操作成功，但刷新仓库上下文失败:', error);
        }
      }
    } else if (gitContextState.bindingKey === operationBindingKey) {
      gitContextState.error = result.error?.message || null;
    }
    return result;
  } finally {
    gitContextState.operation = null;
  }
}

export function switchGitContextBranch(
  binding: GitContextBinding,
  branch: string,
): Promise<GitOperationResult> {
  return runGitContextOperation('branchSwitch', binding, () => (
    checkoutWorkspaceBranch(branch, currentGitExpectedContext(), normalizedBinding(binding))
  ));
}

export function createGitContextBranch(
  binding: GitContextBinding,
  branch: string,
): Promise<GitOperationResult> {
  return runGitContextOperation('branchCreate', binding, () => (
    createWorkspaceBranch(branch, currentGitExpectedContext(), normalizedBinding(binding))
  ));
}

export function acceptCurrentGitContext(
  binding: GitContextBinding,
): Promise<GitOperationResult> {
  return runGitContextOperation('contextAccept', binding, () => (
    acceptWorkspaceGitContext(gitContextState.contextRevision, normalizedBinding(binding))
  ));
}
