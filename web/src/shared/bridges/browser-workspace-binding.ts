export interface BrowserWorkspaceBinding {
  workspaceId: string;
  workspacePath: string;
}

export const WORKSPACE_ID_STORAGE_KEY = 'magi-workspace-id';
export const WORKSPACE_PATH_STORAGE_KEY = 'magi-workspace-path';
const SESSION_ID_STORAGE_KEY = 'magi-session-id';

function safeSessionStorageGetItem(key: string): string {
  if (typeof window === 'undefined') {
    return '';
  }
  try {
    return window.sessionStorage.getItem(key)?.trim() || '';
  } catch (error) {
    console.warn(`[browser-workspace-binding] sessionStorage 读取失败(${key})`, error);
    return '';
  }
}

function safeSessionStorageSetItem(key: string, value: string): void {
  if (typeof window === 'undefined') {
    return;
  }
  try {
    window.sessionStorage.setItem(key, value);
  } catch (error) {
    console.warn(`[browser-workspace-binding] sessionStorage 写入失败(${key})`, error);
  }
}

function safeSessionStorageRemoveItem(key: string): void {
  if (typeof window === 'undefined') {
    return;
  }
  try {
    window.sessionStorage.removeItem(key);
  } catch (error) {
    console.warn(`[browser-workspace-binding] sessionStorage 删除失败(${key})`, error);
  }
}

export function readStoredBrowserWorkspaceBinding(): BrowserWorkspaceBinding {
  return {
    workspaceId: safeSessionStorageGetItem(WORKSPACE_ID_STORAGE_KEY),
    workspacePath: safeSessionStorageGetItem(WORKSPACE_PATH_STORAGE_KEY),
  };
}

export function persistStoredBrowserWorkspaceBinding(binding: BrowserWorkspaceBinding): void {
  const workspaceId = binding.workspaceId.trim();
  const workspacePath = binding.workspacePath.trim();

  if (workspaceId) {
    safeSessionStorageSetItem(WORKSPACE_ID_STORAGE_KEY, workspaceId);
  } else {
    safeSessionStorageRemoveItem(WORKSPACE_ID_STORAGE_KEY);
  }

  if (workspacePath) {
    safeSessionStorageSetItem(WORKSPACE_PATH_STORAGE_KEY, workspacePath);
  } else {
    safeSessionStorageRemoveItem(WORKSPACE_PATH_STORAGE_KEY);
  }

  safeSessionStorageRemoveItem(SESSION_ID_STORAGE_KEY);
}

export function clearStoredBrowserWorkspaceBinding(): void {
  safeSessionStorageRemoveItem(WORKSPACE_ID_STORAGE_KEY);
  safeSessionStorageRemoveItem(WORKSPACE_PATH_STORAGE_KEY);
  safeSessionStorageRemoveItem(SESSION_ID_STORAGE_KEY);
}
