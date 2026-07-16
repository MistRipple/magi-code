export interface ComposerWorkspaceOption {
  workspaceId: string;
  name: string;
  rootPath: string;
  rootPathRef?: string;
  isActive: boolean;
}

export const composerWorkspaceState = $state({
  workspaces: [] as ComposerWorkspaceOption[],
  recentWorkspaceId: '',
  draftWorkspaceId: '',
});

function normalizeWorkspaceId(value: string | null | undefined): string {
  return typeof value === 'string' ? value.trim() : '';
}

function normalizeWorkspacePath(value: string | null | undefined): string {
  return typeof value === 'string' ? value.trim() : '';
}

function folderName(path: string): string {
  const cleaned = path.replace(/[\\/]+$/, '');
  const segments = cleaned.split(/[\\/]/).filter(Boolean);
  return segments.at(-1) || cleaned || 'Workspace';
}

function normalizeWorkspaceOption(input: ComposerWorkspaceOption): ComposerWorkspaceOption | null {
  const workspaceId = normalizeWorkspaceId(input.workspaceId);
  const rootPath = normalizeWorkspacePath(input.rootPath);
  if (!workspaceId || !rootPath) {
    return null;
  }
  const name = normalizeWorkspacePath(input.name) || folderName(rootPath);
  return {
    workspaceId,
    rootPath,
    rootPathRef: normalizeWorkspacePath(input.rootPathRef) || undefined,
    name,
    isActive: input.isActive === true,
  };
}

function findWorkspaceById(workspaceId: string): ComposerWorkspaceOption | null {
  const normalizedId = normalizeWorkspaceId(workspaceId);
  if (!normalizedId) return null;
  return composerWorkspaceState.workspaces.find((workspace) => workspace.workspaceId === normalizedId) ?? null;
}

function findWorkspaceByBinding(
  workspaceId: string | null | undefined,
  workspacePath: string | null | undefined,
): ComposerWorkspaceOption | null {
  const normalizedId = normalizeWorkspaceId(workspaceId);
  const normalizedPath = normalizeWorkspacePath(workspacePath);
  if (normalizedId) {
    const byId = findWorkspaceById(normalizedId);
    if (byId) return byId;
  }
  if (normalizedPath) {
    return composerWorkspaceState.workspaces.find((workspace) => (
      workspace.rootPathRef === normalizedPath || workspace.rootPath === normalizedPath
    )) ?? null;
  }
  return null;
}

function currentBindingWorkspace(
  workspaceId: string | null | undefined,
  workspacePath: string | null | undefined,
): ComposerWorkspaceOption | null {
  const normalizedId = normalizeWorkspaceId(workspaceId);
  const normalizedPath = normalizeWorkspacePath(workspacePath);
  if (!normalizedId || !normalizedPath) {
    return null;
  }
  return {
    workspaceId: normalizedId,
    rootPath: normalizedPath,
    name: folderName(normalizedPath),
    isActive: false,
  };
}

function defaultWorkspace(): ComposerWorkspaceOption | null {
  return findWorkspaceById(composerWorkspaceState.recentWorkspaceId)
    ?? composerWorkspaceState.workspaces.find((workspace) => workspace.isActive)
    ?? composerWorkspaceState.workspaces[0]
    ?? null;
}

export function syncComposerWorkspaces(
  workspaces: ComposerWorkspaceOption[],
  recentWorkspaceId: string | null | undefined,
): void {
  const normalized = workspaces
    .map(normalizeWorkspaceOption)
    .filter((workspace): workspace is ComposerWorkspaceOption => workspace !== null);
  composerWorkspaceState.workspaces = normalized;

  const normalizedRecentId = normalizeWorkspaceId(recentWorkspaceId);
  if (normalizedRecentId && normalized.some((workspace) => workspace.workspaceId === normalizedRecentId)) {
    composerWorkspaceState.recentWorkspaceId = normalizedRecentId;
  } else if (!findWorkspaceById(composerWorkspaceState.recentWorkspaceId)) {
    composerWorkspaceState.recentWorkspaceId = defaultWorkspace()?.workspaceId ?? '';
  }

  if (composerWorkspaceState.draftWorkspaceId && !findWorkspaceById(composerWorkspaceState.draftWorkspaceId)) {
    composerWorkspaceState.draftWorkspaceId = '';
  }
}

export function selectComposerDraftWorkspace(workspaceId: string): ComposerWorkspaceOption | null {
  const workspace = findWorkspaceById(workspaceId);
  if (!workspace) return null;
  composerWorkspaceState.draftWorkspaceId = workspace.workspaceId;
  composerWorkspaceState.recentWorkspaceId = workspace.workspaceId;
  return workspace;
}

export function resolveComposerWorkspace(
  currentWorkspaceId: string | null | undefined,
  currentWorkspacePath: string | null | undefined,
  preferDraftWorkspace: boolean,
): ComposerWorkspaceOption | null {
  if (preferDraftWorkspace) {
    return findWorkspaceById(composerWorkspaceState.draftWorkspaceId)
      ?? findWorkspaceByBinding(currentWorkspaceId, currentWorkspacePath)
      ?? currentBindingWorkspace(currentWorkspaceId, currentWorkspacePath)
      ?? defaultWorkspace();
  }
  return findWorkspaceByBinding(currentWorkspaceId, currentWorkspacePath)
    ?? currentBindingWorkspace(currentWorkspaceId, currentWorkspacePath);
}
