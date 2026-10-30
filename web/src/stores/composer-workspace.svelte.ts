export interface ComposerWorkspaceOption {
  workspaceId: string;
  name: string;
  rootPath: string;
  rootPathRef?: string;
  isActive: boolean;
  primarySourceFolderId?: string;
  sourceFolders?: Array<{ sourceFolderId: string; path: string; pathRef?: string; name?: string; isPrimary: boolean }>;
}

export const composerWorkspaceState = $state({
  workspaces: [] as ComposerWorkspaceOption[],
  hasUnsavedInput: false,
  sourceFolderSelections: {} as Record<string, string>,
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
    primarySourceFolderId: input.primarySourceFolderId,
    sourceFolders: input.sourceFolders?.map((folder) => ({ ...folder })),
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
      || workspace.sourceFolders?.some((folder) => folder.path === normalizedPath || folder.pathRef === normalizedPath)
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
  const nextWorkspaces = workspaces
    .map(normalizeWorkspaceOption)
    .filter((workspace): workspace is ComposerWorkspaceOption => workspace !== null);
  composerWorkspaceState.workspaces = nextWorkspaces;
  // 项目更新/移除后清理页面级目录选择，避免已撤销的 sourceFolderId 继续
  // 被当作下一轮提交目标；真正的授权事实仍由 daemon 在接纳时重新冻结。
  const available = new Map(
    nextWorkspaces.map((workspace) => [
      workspace.workspaceId,
      new Set((workspace.sourceFolders || []).map((folder) => folder.sourceFolderId)),
    ]),
  );
  for (const key of Object.keys(composerWorkspaceState.sourceFolderSelections)) {
    const separator = key.indexOf('\u0000');
    const workspaceId = separator < 0 ? key : key.slice(0, separator);
    const sourceFolderId = composerWorkspaceState.sourceFolderSelections[key];
    if (!available.get(workspaceId)?.has(sourceFolderId)) {
      delete composerWorkspaceState.sourceFolderSelections[key];
    }
  }
}

export function resolveComposerWorkspace(
  currentWorkspaceId: string | null | undefined,
  currentWorkspacePath: string | null | undefined,
): ComposerWorkspaceOption | null {
  return findWorkspaceByBinding(currentWorkspaceId, currentWorkspacePath)
    ?? currentBindingWorkspace(currentWorkspaceId, currentWorkspacePath);
}

/** 仅保存当前页面的目录选择；任务和授权绑定始终由 daemon 冻结。 */
function sourceFolderSelectionKey(workspaceId: string, sessionId?: string | null): string {
  return `${workspaceId.trim()}\u0000${sessionId?.trim() || ''}`;
}

export function selectedComposerSourceFolderId(workspaceId: string, sessionId?: string | null): string {
  const project = findWorkspaceById(workspaceId);
  const selected = composerWorkspaceState.sourceFolderSelections[sourceFolderSelectionKey(workspaceId, sessionId)];
  if (selected && project?.sourceFolders?.some((folder) => folder.sourceFolderId === selected)) {
    return selected;
  }
  return project?.primarySourceFolderId || '';
}

export function selectComposerSourceFolder(workspaceId: string, sessionId: string | null | undefined, sourceFolderId: string): void {
  if (!workspaceId.trim()) return;
  composerWorkspaceState.sourceFolderSelections[sourceFolderSelectionKey(workspaceId, sessionId)] = sourceFolderId;
}

export function resolveComposerSourceFolder(workspaceId: string, sessionId?: string | null) {
  const project = findWorkspaceById(workspaceId);
  const id = selectedComposerSourceFolderId(workspaceId, sessionId);
  return project?.sourceFolders?.find((folder) => folder.sourceFolderId === id) ?? null;
}

export function composerSourceFolderForPath(workspaceId: string, path: string): string | undefined {
  const normalize = (value: string) => value.replaceAll('\\', '/').replace(/\/+$/, '');
  const candidate = normalize(path);
  return findWorkspaceById(workspaceId)?.sourceFolders?.find((folder) => {
    const root = normalize(folder.path);
    return candidate === root || candidate.startsWith(`${root}/`);
  })?.sourceFolderId;
}
