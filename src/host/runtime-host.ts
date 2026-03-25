import * as fs from 'fs';
import * as fsp from 'fs/promises';
import * as path from 'path';
import { exec } from 'child_process';
import { promisify } from 'util';
import * as vscode from 'vscode';
import type { SnapshotManager } from '../snapshot-manager';
import type { UnifiedSessionManager } from '../session';
import { WorkspaceRoots, type WorkspaceFolderInfo } from '../workspace/workspace-roots';
import { WorktreeManager } from '../workspace/worktree-manager';
import {
  type DiagnosticsHost,
  type FileSystemHost,
  type GitHost,
  type HostCapabilities,
  type HostDiagnostic,
  type LspHost,
  type RuntimeHostContext,
  type SessionHost,
  type WorkspaceHost,
  type WorkspaceRef,
  toSessionRef,
} from './types';

const execAsync = promisify(exec);

interface CreateRuntimeHostContextInput {
  workspaceRoot: string;
  workspaceFolders: WorkspaceFolderInfo[];
  sessionManager: UnifiedSessionManager;
  snapshotManager: SnapshotManager;
  getCurrentSessionId: () => string | null | undefined;
  workspaceRefs?: WorkspaceRef[];
}

function buildWorkspaceId(workspacePath: string): string {
  return Buffer.from(path.resolve(workspacePath)).toString('base64url');
}

function resolveWorkspaceRefs(input: CreateRuntimeHostContextInput): WorkspaceRef[] {
  if (Array.isArray(input.workspaceRefs) && input.workspaceRefs.length > 0) {
    return input.workspaceRefs.map((workspace) => ({
      workspaceId: workspace.workspaceId,
      rootPath: path.resolve(workspace.rootPath),
      displayName: workspace.displayName,
    }));
  }
  return input.workspaceFolders.map((folder) => ({
    workspaceId: buildWorkspaceId(folder.path),
    rootPath: path.resolve(folder.path),
    displayName: folder.name,
  }));
}

function createWorkspaceHost(workspaceRefs: WorkspaceRef[], workspaceRoot: string): WorkspaceHost {
  const normalizedRoot = path.resolve(workspaceRoot);
  return {
    async getCurrentWorkspace(): Promise<WorkspaceRef | null> {
      return workspaceRefs.find((workspace) => workspace.rootPath === normalizedRoot) || workspaceRefs[0] || null;
    },
    async listWorkspaces(): Promise<WorkspaceRef[]> {
      return [...workspaceRefs];
    },
  };
}

function createSessionHost(
  sessionManager: UnifiedSessionManager,
  getCurrentSessionId: () => string | null | undefined,
): SessionHost {
  return {
    async getCurrentSessionId(): Promise<string | null> {
      return (getCurrentSessionId() || '').trim() || null;
    },
    async listSessions() {
      return sessionManager.getSessionMetas().map(toSessionRef);
    },
  };
}

function createFileSystemHost(): FileSystemHost {
  return {
    async readFile(targetPath: string): Promise<string> {
      return fsp.readFile(targetPath, 'utf8');
    },
    async writeFile(targetPath: string, content: string): Promise<void> {
      await fsp.mkdir(path.dirname(targetPath), { recursive: true });
      await fsp.writeFile(targetPath, content, 'utf8');
    },
    async exists(targetPath: string): Promise<boolean> {
      return fs.existsSync(targetPath);
    },
  };
}

function createTerminalHost(workspaceRoot: string) {
  return {
    async run(command: string, cwd?: string): Promise<{ exitCode: number; stdout: string; stderr: string }> {
      try {
        const { stdout, stderr } = await execAsync(command, {
          cwd: cwd || workspaceRoot,
          maxBuffer: 10 * 1024 * 1024,
        });
        return {
          exitCode: 0,
          stdout: stdout || '',
          stderr: stderr || '',
        };
      } catch (error) {
        const failure = error as NodeJS.ErrnoException & {
          code?: number | string;
          stdout?: string;
          stderr?: string;
        };
        return {
          exitCode: typeof failure.code === 'number' ? failure.code : 1,
          stdout: failure.stdout || '',
          stderr: failure.stderr || failure.message || String(error),
        };
      }
    },
  };
}

class NodeGitHost implements GitHost {
  private readonly managers = new Map<string, WorktreeManager>();

  constructor(private readonly defaultWorkspaceRoot: string) {}

  private resolveWorkspaceRoot(workspacePath?: string): string {
    const normalized = typeof workspacePath === 'string' ? workspacePath.trim() : '';
    return path.resolve(normalized || this.defaultWorkspaceRoot);
  }

  private getManager(workspacePath?: string): WorktreeManager {
    const resolvedRoot = this.resolveWorkspaceRoot(workspacePath);
    const existing = this.managers.get(resolvedRoot);
    if (existing) {
      return existing;
    }
    const manager = new WorktreeManager(resolvedRoot);
    this.managers.set(resolvedRoot, manager);
    return manager;
  }

  isGitRepository(workspacePath?: string): boolean {
    return this.getManager(workspacePath).isGitRepository();
  }

  acquireWorktree(options: { workspacePath: string; taskId: string }) {
    return this.getManager(options.workspacePath).acquire(options.taskId);
  }

  mergeWorktree(options: { workspacePath: string; taskId: string }) {
    return this.getManager(options.workspacePath).merge(options.taskId);
  }

  releaseWorktree(options: { workspacePath: string; taskId: string }): void {
    this.getManager(options.workspacePath).release(options.taskId);
  }

  releaseAllWorktrees(workspacePath?: string): void {
    this.getManager(workspacePath).releaseAll();
  }
}

function resolveAbsolutePath(workspaceRoot: string, filePath: string): string {
  return path.isAbsolute(filePath)
    ? path.resolve(filePath)
    : path.resolve(workspaceRoot, filePath);
}

function serializeDiagnostic(file: string, diagnostic: vscode.Diagnostic): HostDiagnostic {
  return {
    file,
    line: diagnostic.range.start.line + 1,
    message: diagnostic.message,
    severity: diagnostic.severity === vscode.DiagnosticSeverity.Error ? 'error' : 'warning',
  };
}

function createDiagnosticsHost(workspaceRoot: string): DiagnosticsHost {
  return {
    async getDiagnostics(filePath?: string): Promise<HostDiagnostic[]> {
      if (typeof filePath === 'string' && filePath.trim()) {
        const absolutePath = resolveAbsolutePath(workspaceRoot, filePath);
        const uri = vscode.Uri.file(absolutePath);
        return vscode.languages.getDiagnostics(uri)
          .filter((diagnostic) =>
            diagnostic.severity === vscode.DiagnosticSeverity.Error
            || diagnostic.severity === vscode.DiagnosticSeverity.Warning)
          .map((diagnostic) => serializeDiagnostic(absolutePath, diagnostic));
      }

      const allDiagnostics = vscode.languages.getDiagnostics();
      return allDiagnostics.flatMap(([uri, diagnostics]) =>
        diagnostics
          .filter((diagnostic) =>
            diagnostic.severity === vscode.DiagnosticSeverity.Error
            || diagnostic.severity === vscode.DiagnosticSeverity.Warning)
          .map((diagnostic) => serializeDiagnostic(uri.fsPath, diagnostic)));
    },
  };
}

function createLspHost(workspaceRoot: string): LspHost {
  return {
    async query(symbol: string, filePath?: string): Promise<unknown> {
      if (typeof filePath === 'string' && filePath.trim()) {
        const uri = vscode.Uri.file(resolveAbsolutePath(workspaceRoot, filePath));
        return vscode.commands.executeCommand('vscode.executeDocumentSymbolProvider', uri);
      }
      return vscode.commands.executeCommand('vscode.executeWorkspaceSymbolProvider', symbol);
    },
  };
}

export function createRuntimeHostContext(input: CreateRuntimeHostContextInput): RuntimeHostContext {
  const workspaceFolders = input.workspaceFolders.length > 0
    ? input.workspaceFolders.map((folder) => ({ name: folder.name, path: path.resolve(folder.path) }))
    : [{ name: path.basename(input.workspaceRoot), path: path.resolve(input.workspaceRoot) }];
  const workspaceRefs = resolveWorkspaceRefs({
    ...input,
    workspaceFolders,
  });
  const normalizedWorkspaceRoot = path.resolve(input.workspaceRoot);
  const workspaceRoots = new WorkspaceRoots(workspaceFolders);
  const capabilities: HostCapabilities = {
    workspace: createWorkspaceHost(workspaceRefs, normalizedWorkspaceRoot),
    session: createSessionHost(input.sessionManager, input.getCurrentSessionId),
    fs: createFileSystemHost(),
    terminal: createTerminalHost(normalizedWorkspaceRoot),
    git: new NodeGitHost(normalizedWorkspaceRoot),
    lsp: createLspHost(normalizedWorkspaceRoot),
    diagnostics: createDiagnosticsHost(normalizedWorkspaceRoot),
  };

  return {
    workspaceRoot: workspaceRoots.getPrimaryFolder().path,
    workspaceFolders: workspaceRoots.getFolders(),
    sessionManager: input.sessionManager,
    snapshotManager: input.snapshotManager,
    capabilities,
  };
}
