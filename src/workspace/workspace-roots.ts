import * as fs from 'fs';
import * as path from 'path';

export interface WorkspaceFolderInfo {
  name: string;
  path: string;
}

export interface ResolvedWorkspacePath {
  absolutePath: string;
  workspaceName: string;
  workspacePath: string;
  displayPath: string;
}

export interface ResolvePathOptions {
  mustExist?: boolean;
  preferWorkspacePath?: string;
}

export class WorkspaceRoots {
  private readonly folders: WorkspaceFolderInfo[];
  private readonly normalizedFolders: WorkspaceFolderInfo[];
  private readonly nameMap: Map<string, WorkspaceFolderInfo>;

  constructor(folders: WorkspaceFolderInfo[]) {
    if (!Array.isArray(folders) || folders.length === 0) {
      throw new Error('workspace folders must not be empty');
    }

    const normalized = folders
      .filter(folder => folder && folder.path)
      .map(folder => ({
        name: folder.name,
        path: path.resolve(folder.path),
      }));

    if (normalized.length === 0) {
      throw new Error('workspace folders must not be empty');
    }

    this.normalizedFolders = this.ensureUniqueNames(normalized);
    this.folders = this.normalizedFolders
      .slice()
      .sort((a, b) => b.path.length - a.path.length);
    this.nameMap = new Map(this.normalizedFolders.map(folder => [folder.name, folder]));
  }

  getPrimaryFolder(): WorkspaceFolderInfo {
    return this.normalizedFolders[0];
  }

  getFolders(): WorkspaceFolderInfo[] {
    return this.normalizedFolders.slice();
  }

  getRootPaths(): string[] {
    return this.normalizedFolders.map(folder => folder.path);
  }

  hasMultipleRoots(): boolean {
    return this.normalizedFolders.length > 1;
  }

  toDisplayPath(targetPath: string): string {
    const normalized = path.resolve(targetPath);
    const matched = this.findContainingFolder(normalized);
    if (!matched) {
      return normalized;
    }

    const relative = path.relative(matched.path, normalized);
    if (!this.hasMultipleRoots()) {
      return relative || '.';
    }
    return relative ? `${matched.name}/${relative}` : matched.name;
  }

  resolvePath(inputPath: string, options: ResolvePathOptions = {}): ResolvedWorkspacePath | null {
    const normalizedInput = (inputPath || '').trim();
    if (!normalizedInput) {
      return null;
    }

    if (path.isAbsolute(normalizedInput)) {
      return this.resolveAbsolutePath(path.resolve(normalizedInput), options.mustExist === true);
    }

    const explicit = this.resolveByWorkspacePrefix(normalizedInput, options.mustExist === true);
    if (explicit) {
      return explicit;
    }

    const preferred = this.resolveByPreferredWorkspace(normalizedInput, options);
    if (preferred) {
      return preferred;
    }

    if (this.hasMultipleRoots()) {
      if (options.mustExist) {
        const existing = this.normalizedFolders
          .map(folder => this.buildResolved(folder, path.resolve(folder.path, normalizedInput)))
          .filter(candidate => fs.existsSync(candidate.absolutePath));

        if (existing.length === 1) {
          return existing[0];
        }
        if (existing.length > 1) {
          throw new Error(
            `路径 "${inputPath}" 在多个工作区同时存在，请使用 "<工作区名>/${inputPath}" 指定目标`
          );
        }
        throw new Error(
          `路径 "${inputPath}" 在所有工作区都不存在，请使用 "<工作区名>/${inputPath}" 明确目标`
        );
      }

      throw new Error(
        `多工作区写入必须显式指定工作区前缀，请使用 "<工作区名>/${inputPath}"`
      );
    }

    const primary = this.getPrimaryFolder();
    const resolved = this.buildResolved(primary, path.resolve(primary.path, normalizedInput));
    return resolved;
  }

  private resolveByWorkspacePrefix(inputPath: string, mustExist: boolean): ResolvedWorkspacePath | null {
    const slash = inputPath.indexOf('/');
    if (slash <= 0) {
      return null;
    }

    const workspaceName = inputPath.substring(0, slash);
    const workspace = this.nameMap.get(workspaceName);
    if (!workspace) {
      return null;
    }

    const suffix = inputPath.substring(slash + 1);
    const absolute = path.resolve(workspace.path, suffix || '.');
    if (!this.isInsideWorkspace(absolute, workspace.path)) {
      return null;
    }

    const resolved = this.buildResolved(workspace, absolute);
    if (mustExist && !fs.existsSync(resolved.absolutePath)) {
      return null;
    }
    return resolved;
  }

  private resolveByPreferredWorkspace(inputPath: string, options: ResolvePathOptions): ResolvedWorkspacePath | null {
    if (!options.preferWorkspacePath) {
      return null;
    }

    const preferred = this.normalizedFolders.find(
      folder => path.resolve(folder.path) === path.resolve(options.preferWorkspacePath as string)
    );
    if (!preferred) {
      return null;
    }

    const absolute = path.resolve(preferred.path, inputPath);
    if (!this.isInsideWorkspace(absolute, preferred.path)) {
      return null;
    }

    const resolved = this.buildResolved(preferred, absolute);
    if (options.mustExist && !fs.existsSync(resolved.absolutePath)) {
      return null;
    }
    return resolved;
  }

  private resolveAbsolutePath(absolute: string, mustExist: boolean): ResolvedWorkspacePath | null {
    const matched = this.findContainingFolder(absolute);
    if (!matched) {
      return null;
    }

    if (mustExist && !fs.existsSync(absolute)) {
      return null;
    }
    return this.buildResolved(matched, absolute);
  }

  private findContainingFolder(targetPath: string): WorkspaceFolderInfo | null {
    const normalized = path.resolve(targetPath);
    for (const folder of this.folders) {
      if (this.isInsideWorkspace(normalized, folder.path)) {
        return folder;
      }
    }
    return null;
  }

  private isInsideWorkspace(targetPath: string, workspacePath: string): boolean {
    const normalizedRoot = path.resolve(workspacePath) + path.sep;
    return targetPath === path.resolve(workspacePath) || targetPath.startsWith(normalizedRoot);
  }

  private buildResolved(workspace: WorkspaceFolderInfo, absolutePath: string): ResolvedWorkspacePath {
    return {
      absolutePath,
      workspaceName: workspace.name,
      workspacePath: workspace.path,
      displayPath: this.toDisplayPath(absolutePath),
    };
  }

  private ensureUniqueNames(folders: WorkspaceFolderInfo[]): WorkspaceFolderInfo[] {
    const counts = new Map<string, number>();
    return folders.map(folder => {
      const current = counts.get(folder.name) || 0;
      const next = current + 1;
      counts.set(folder.name, next);
      if (next === 1) {
        return folder;
      }
      return { ...folder, name: `${folder.name}-${next}` };
    });
  }
}
