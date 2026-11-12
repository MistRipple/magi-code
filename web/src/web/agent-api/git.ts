import type { AgentBindingOverride } from '../agent-binding-context';
import type { GitExpectedContext, GitOperationResult, WorkspaceBranchesResult } from './types';
import { postWorkspacePathBoundJson } from './core';

export async function fetchWorkspaceBranches(
  bindingOverride?: AgentBindingOverride,
): Promise<WorkspaceBranchesResult> {
  return await postWorkspacePathBoundJson<WorkspaceBranchesResult>('/api/workspace/vcs/branches', { includeRemote: true }, 'fetch workspace branches', bindingOverride);
}

function gitExpectedPayload(expected?: GitExpectedContext): Record<string, unknown> {
  return {
    ...(typeof expected?.contextRevision === 'number' ? { expectedContextRevision: expected.contextRevision } : {}),
    ...(expected?.branch ? { expectedBranch: expected.branch } : {}),
    ...(expected?.head ? { expectedHead: expected.head } : {}),
    ...(expected?.worktreePath ? { expectedWorktreePath: expected.worktreePath } : {}),
  };
}

export async function checkoutWorkspaceBranch(
  branch: string,
  expected?: GitExpectedContext,
  bindingOverride?: AgentBindingOverride,
): Promise<GitOperationResult> {
  return await postWorkspacePathBoundJson<GitOperationResult>('/api/workspace/vcs/branch/switch', {
    branch,
    ...gitExpectedPayload(expected),
  }, 'switch workspace branch', bindingOverride);
}

export async function createWorkspaceBranch(
  branch: string,
  expected?: GitExpectedContext,
  bindingOverride?: AgentBindingOverride,
): Promise<GitOperationResult> {
  return await postWorkspacePathBoundJson<GitOperationResult>('/api/workspace/vcs/branch/create', {
    branch,
    switch: true,
    ...gitExpectedPayload(expected),
  }, 'create workspace branch', bindingOverride);
}

export async function previewWorkspaceMerge(
  target: string,
  expected?: GitExpectedContext,
  bindingOverride?: AgentBindingOverride,
): Promise<GitOperationResult> {
  return await postWorkspacePathBoundJson<GitOperationResult>('/api/workspace/vcs/merge/preview', {
    target,
    ...gitExpectedPayload(expected),
  }, 'preview workspace merge', bindingOverride);
}

export async function mergeWorkspaceBranch(
  target: string,
  ffOnly: boolean,
  expected?: GitExpectedContext,
  bindingOverride?: AgentBindingOverride,
): Promise<GitOperationResult> {
  return await postWorkspacePathBoundJson<GitOperationResult>('/api/workspace/vcs/merge', {
    target,
    ffOnly,
    confirm: true,
    ...gitExpectedPayload(expected),
  }, 'merge workspace branch', bindingOverride);
}

export async function deleteWorkspaceBranch(
  branch: string,
  options: { remote?: string; force?: boolean; confirmForce?: boolean; confirmRemote?: boolean },
  expected?: GitExpectedContext,
  bindingOverride?: AgentBindingOverride,
): Promise<GitOperationResult> {
  return await postWorkspacePathBoundJson<GitOperationResult>('/api/workspace/vcs/branch/delete', {
    branch,
    ...options,
    ...gitExpectedPayload(expected),
  }, 'delete workspace branch', bindingOverride);
}

export async function fetchWorkspaceWorktrees(
  bindingOverride?: AgentBindingOverride,
): Promise<GitOperationResult> {
  return await postWorkspacePathBoundJson<GitOperationResult>('/api/workspace/vcs/worktree/list', {}, 'list workspace worktrees', bindingOverride);
}

export async function createWorkspaceWorktree(
  mode: 'readOnly' | 'writable',
  options: { base?: string; branch?: string; allocationKey?: string },
  expected?: GitExpectedContext,
  bindingOverride?: AgentBindingOverride,
): Promise<GitOperationResult> {
  return await postWorkspacePathBoundJson<GitOperationResult>('/api/workspace/vcs/worktree/create', {
    mode,
    ...options,
    ...gitExpectedPayload(expected),
  }, 'create workspace worktree', bindingOverride);
}

export async function removeWorkspaceWorktree(
  path: string,
  options: { force?: boolean; confirmForce?: boolean },
  expected?: GitExpectedContext,
  bindingOverride?: AgentBindingOverride,
): Promise<GitOperationResult> {
  return await postWorkspacePathBoundJson<GitOperationResult>('/api/workspace/vcs/worktree/remove', {
    path,
    ...options,
    ...gitExpectedPayload(expected),
  }, 'remove workspace worktree', bindingOverride);
}

export async function acceptWorkspaceGitContext(
  expectedContextRevision: number | null,
  bindingOverride?: AgentBindingOverride,
): Promise<GitOperationResult> {
  return await postWorkspacePathBoundJson<GitOperationResult>('/api/workspace/vcs/context/accept', {
    ...(typeof expectedContextRevision === 'number'
      ? { expectedContextRevision }
      : {}),
  }, 'accept workspace git context', bindingOverride);
}
