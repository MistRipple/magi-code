export interface FilePreviewScopeBinding {
  scope: 'personal' | 'workspace';
  workspaceId: string;
  workspacePath: string;
  sessionId: string;
}

export interface FilePreviewScopeInput {
  metadataWorkspaceId?: string;
  metadataWorkspacePath?: string;
  metadataSessionId?: string;
  imageDataUrl?: string;
  currentBinding: FilePreviewScopeBinding;
  selectedWorkspaceId?: string;
  selectedWorkspacePath?: string;
  workspacePathForId: (workspaceId: string) => string;
  activeWorkspaceId?: string;
  activeSessionId?: string;
}

export interface FilePreviewScopeResult {
  workspaceId: string;
  workspacePath: string;
  sessionId: string;
  imageDataUrl: string;
}

function trim(value: string | null | undefined): string {
  return typeof value === 'string' ? value.trim() : '';
}

/**
 * 解析文件预览的真实作用域。
 * 内存图片不属于文件树；个人会话不能继承之前工作区的侧栏选择。
 */
export function resolveFilePreviewScope(input: FilePreviewScopeInput): FilePreviewScopeResult {
  const imageDataUrl = trim(input.imageDataUrl);
  const currentBinding = input.currentBinding;
  const explicitWorkspaceId = trim(input.metadataWorkspaceId);
  // 消息携带 sessionId 且显式空项目时属于个人会话，不能借用侧栏项目。
  const explicitPersonal = Boolean(trim(input.metadataSessionId))
    && input.metadataWorkspaceId === '' && !trim(input.metadataWorkspacePath);
  const useCurrent = imageDataUrl || explicitPersonal || currentBinding.scope === 'personal';
  const workspaceId = explicitPersonal ? '' : explicitWorkspaceId || (
    useCurrent ? trim(currentBinding.workspaceId) : trim(input.selectedWorkspaceId)
  );
  const sessionId = trim(input.metadataSessionId)
    || (workspaceId === trim(currentBinding.workspaceId) ? trim(currentBinding.sessionId) : '')
    || (workspaceId && workspaceId === trim(input.activeWorkspaceId) ? trim(input.activeSessionId) : '');
  const workspacePath = explicitPersonal ? '' : trim(input.metadataWorkspacePath)
    || (useCurrent ? trim(currentBinding.workspacePath) : trim(input.selectedWorkspacePath))
    || trim(input.workspacePathForId(workspaceId))
    || (workspaceId === trim(currentBinding.workspaceId) ? trim(currentBinding.workspacePath) : '');
  return { workspaceId, workspacePath, sessionId, imageDataUrl };
}
