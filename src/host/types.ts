import type { SnapshotManager } from '../snapshot-manager';
import type { UnifiedSessionManager, SessionMeta } from '../session';
import type { WorkspaceFolderInfo } from '../workspace/workspace-roots';
import type { WorktreeAllocation, WorktreeMergeResult } from '../workspace/worktree-manager';

export interface WorkspaceRef {
  workspaceId: string;
  rootPath: string;
  displayName: string;
}

export interface SessionRef {
  sessionId: string;
  title: string;
  updatedAt: number;
}

export interface WorkspaceHost {
  getCurrentWorkspace(): Promise<WorkspaceRef | null>;
  listWorkspaces(): Promise<WorkspaceRef[]>;
}

export interface SessionHost {
  getCurrentSessionId(): Promise<string | null>;
  listSessions(): Promise<SessionRef[]>;
}

export interface FileSystemHost {
  readFile(path: string): Promise<string>;
  writeFile(path: string, content: string): Promise<void>;
  exists(path: string): Promise<boolean>;
}

export interface TerminalHost {
  run(command: string, cwd?: string): Promise<{ exitCode: number; stdout: string; stderr: string }>;
}

export interface GitHost {
  isGitRepository(workspacePath?: string): boolean;
  acquireWorktree(options: { workspacePath: string; taskId: string }): WorktreeAllocation | null;
  mergeWorktree(options: { workspacePath: string; taskId: string }): WorktreeMergeResult;
  releaseWorktree(options: { workspacePath: string; taskId: string }): void;
  releaseAllWorktrees?(workspacePath?: string): void;
}

export interface LspHost {
  query(symbol: string, filePath?: string): Promise<unknown>;
}

export interface HostDiagnostic {
  file: string;
  line: number;
  message: string;
  severity: 'error' | 'warning';
}

export interface DiagnosticsHost {
  getDiagnostics(filePath?: string): Promise<HostDiagnostic[]>;
}

export interface HostCapabilities {
  workspace: WorkspaceHost;
  session: SessionHost;
  fs: FileSystemHost;
  terminal: TerminalHost;
  git: GitHost;
  lsp?: LspHost;
  diagnostics?: DiagnosticsHost;
}

export interface RuntimeHostContext {
  workspaceRoot: string;
  workspaceFolders: WorkspaceFolderInfo[];
  sessionManager: UnifiedSessionManager;
  snapshotManager: SnapshotManager;
  capabilities: HostCapabilities;
}

export function toSessionRef(meta: SessionMeta): SessionRef {
  return {
    sessionId: meta.id,
    title: meta.name || meta.id,
    updatedAt: meta.updatedAt,
  };
}
