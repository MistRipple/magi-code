export interface ComposerWorkspaceOption {
  workspaceId: string;
  name: string;
  rootPath: string;
  rootPathRef?: string;
  isActive: boolean;
}

export const composerWorkspaceState = $state({
  workspaces: [] as ComposerWorkspaceOption[],
  hasUnsavedInput: false,
});

export function setComposerHasUnsavedInput(value: boolean): void {
  composerWorkspaceState.hasUnsavedInput = value;
}

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

/** 项目列表只提供选择项；会话归属完全由 daemon 导航绑定决定。 */
export function syncComposerWorkspaces(workspaces: ComposerWorkspaceOption[]): void {
  composerWorkspaceState.workspaces = workspaces
    .map(normalizeWorkspaceOption)
    .filter((workspace): workspace is ComposerWorkspaceOption => workspace !== null);
}

export function resolveComposerWorkspace(
  currentWorkspaceId: string | null | undefined,
  currentWorkspacePath: string | null | undefined,
): ComposerWorkspaceOption | null {
  return findWorkspaceByBinding(currentWorkspaceId, currentWorkspacePath)
    ?? currentBindingWorkspace(currentWorkspaceId, currentWorkspacePath);
}
