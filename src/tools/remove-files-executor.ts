/**
 * 文件删除执行器
 * 提供安全的文件删除功能
 *
 * 工具: file_remove
 */

import * as fs from 'fs/promises';
import * as path from 'path';
import * as vscode from 'vscode';
import { ToolExecutor, ExtendedToolDefinition } from './types';
import { ToolCall, ToolResult } from '../llm/types';
import { logger, LogCategory } from '../logging';
import { WorkspaceRoots } from '../workspace/workspace-roots';

/**
 * 文件删除执行器
 */
export class RemoveFilesExecutor implements ToolExecutor {
  private workspaceRoots: WorkspaceRoots;
  private deletedFiles: Map<string, string> = new Map(); // 用于恢复
  private effectiveWorkspaceRootsGetter?: () => WorkspaceRoots;

  /** 文件删除前回调（用于快照系统在删除前保存原始内容） */
  private onBeforeWrite?: (filePath: string) => void;
  /** 文件删除后回调（用于 UI 实时刷新待处理变更） */
  private onAfterWrite?: (filePath: string) => void;

  constructor(workspaceRoots: WorkspaceRoots) {
    this.workspaceRoots = workspaceRoots;
  }

  private getEffectiveRoots(): WorkspaceRoots {
    return this.effectiveWorkspaceRootsGetter?.() ?? this.workspaceRoots;
  }

  setEffectiveRootsGetter(getter: () => WorkspaceRoots): void {
    this.effectiveWorkspaceRootsGetter = getter;
  }

  /**
   * 设置文件删除前回调
   */
  setBeforeWriteCallback(callback: (filePath: string) => void): void {
    this.onBeforeWrite = callback;
  }

  /**
   * 设置文件删除后回调
   */
  setAfterWriteCallback(callback: (filePath: string) => void): void {
    this.onAfterWrite = callback;
  }

  /**
   * 获取工具定义
   */
  getToolDefinition(): ExtendedToolDefinition {
    return {
      name: 'file_remove',
      description: `Delete files from workspace safely.

* Supports batch deletion
* Files can be recovered (backup mechanism)
* Path safety validation

IMPORTANT:
* Do NOT use shell commands (rm) to delete files
* This is the only safe way to delete files
* 多工作区写入必须使用 "<工作区名>/相对路径"`,
      input_schema: {
        type: 'object',
        properties: {
          paths: {
            type: 'array',
            items: { type: 'string' },
            description: 'Array of file paths to remove (relative to workspace)'
          }
        },
        required: ['paths']
      },
      metadata: {
        source: 'builtin',
        category: 'file',
        tags: ['file', 'delete', 'remove']
      }
    };
  }

  /**
   * 获取所有工具（实现 ToolExecutor 接口）
   */
  async getTools(): Promise<ExtendedToolDefinition[]> {
    return [this.getToolDefinition()];
  }

  /**
   * 检查工具是否可用
   */
  async isAvailable(toolName: string): Promise<boolean> {
    return toolName === 'file_remove';
  }

  /**
   * 执行工具调用
   */
  async execute(toolCall: ToolCall): Promise<ToolResult> {
    const args = toolCall.arguments as { paths: string[] };

    if (!args.paths || !Array.isArray(args.paths) || args.paths.length === 0) {
      return this.buildStandardizedErrorResult(
        toolCall.id,
        'paths array is required and must not be empty',
        'file_remove_invalid_args',
      );
    }
    const invalidPathIndex = args.paths.findIndex((value) => typeof value !== 'string' || !value.trim());
    if (invalidPathIndex >= 0) {
      return this.buildStandardizedErrorResult(
        toolCall.id,
        `paths[${invalidPathIndex}] must be a non-empty string`,
        'file_remove_invalid_args',
      );
    }

    logger.debug('RemoveFilesExecutor executing', { paths: args.paths }, LogCategory.TOOLS);

    const results: string[] = [];
    let successCount = 0;
    let errorCount = 0;
    let primaryErrorCode: string | undefined;

    for (const filePath of args.paths) {
      const resolvedResult = this.resolveWorkspacePath(filePath);
      const resolved = resolvedResult.absolutePath;

      if (!resolved) {
        results.push(`✗ ${filePath}: ${resolvedResult.error || 'path is outside workspace'}`);
        errorCount++;
        primaryErrorCode ||= 'file_path_outside_workspace';
        continue;
      }

      try {
        // 检查文件是否存在
        const stat = await fs.stat(resolved);

        if (stat.isDirectory()) {
          results.push(`✗ ${filePath}: is a directory (use recursive delete for directories)`);
          errorCount++;
          primaryErrorCode ||= 'file_remove_directory_unsupported';
          continue;
        }

        // 备份文件内容（用于恢复）
        const content = await fs.readFile(resolved, 'utf-8');
        this.deletedFiles.set(resolved, content);

        // 快照回调（在删除前通知快照系统保存原始内容）
        this.onBeforeWrite?.(resolved);

        // 通过 WorkspaceEdit 删除文件，保持与其他文件工具一致的 VSCode 写入链
        const uri = vscode.Uri.file(resolved);
        const edit = new vscode.WorkspaceEdit();
        edit.deleteFile(uri, { recursive: false, ignoreIfNotExists: false });
        const applied = await vscode.workspace.applyEdit(edit);
        if (!applied) {
          throw new Error('VSCode WorkspaceEdit 删除文件失败');
        }
        this.onAfterWrite?.(resolved);

        results.push(`✓ ${filePath}: deleted`);
        successCount++;

        logger.info('File deleted', { path: filePath }, LogCategory.TOOLS);
      } catch (error: any) {
        if (error.code === 'ENOENT') {
          results.push(`✗ ${filePath}: file not found`);
          primaryErrorCode ||= 'file_remove_not_found';
        } else {
          results.push(`✗ ${filePath}: ${error.message}`);
          primaryErrorCode ||= error?.message?.includes('WorkspaceEdit')
            ? 'file_remove_apply_failed'
            : 'file_remove_execution_failed';
        }
        errorCount++;
      }
    }

    const summary = `\nDeleted: ${successCount}, Errors: ${errorCount}`;
    const content = results.join('\n') + summary;

    const result: ToolResult = {
      toolCallId: toolCall.id,
      content,
      isError: errorCount > 0 && successCount === 0
    };
    if (result.isError && primaryErrorCode) {
      result.standardized = {
        schemaVersion: 'tool-result.v1',
        source: 'builtin',
        toolName: 'file_remove',
        toolCallId: toolCall.id,
        status: 'error',
        message: content,
        errorCode: primaryErrorCode,
      };
    }
    return result;
  }

  /**
   * 恢复已删除的文件
   */
  async restoreFile(filePath: string): Promise<boolean> {
    const resolved = this.resolveWorkspacePath(filePath).absolutePath;
    if (!resolved) return false;

    const content = this.deletedFiles.get(resolved);
    if (!content) return false;

    try {
      await fs.mkdir(path.dirname(resolved), { recursive: true });
      await fs.writeFile(resolved, content, 'utf-8');
      this.deletedFiles.delete(resolved);

      logger.info('File restored', { path: filePath }, LogCategory.TOOLS);
      return true;
    } catch {
      return false;
    }
  }

  /**
   * 获取可恢复的文件列表
   */
  getRecoverableFiles(): string[] {
    return Array.from(this.deletedFiles.keys()).map(p =>
      this.getEffectiveRoots().toDisplayPath(p)
    );
  }

  /**
   * 清除恢复缓存
   */
  clearRecoveryCache(): void {
    this.deletedFiles.clear();
  }

  /**
   * 解析工作区相对路径
   */
  private resolveWorkspacePath(inputPath: string): { absolutePath: string | null; error?: string } {
    try {
      const resolved = this.getEffectiveRoots().resolvePath(inputPath, { mustExist: false });
      return { absolutePath: resolved?.absolutePath || null };
    } catch (error: any) {
      return { absolutePath: null, error: error.message };
    }
  }

  private buildStandardizedErrorResult(
    toolCallId: string,
    message: string,
    errorCode: string,
  ): ToolResult {
    const normalizedMessage = message.startsWith('Error') ? message : `Error: ${message}`;
    return {
      toolCallId,
      content: normalizedMessage,
      isError: true,
      standardized: {
        schemaVersion: 'tool-result.v1',
        source: 'builtin',
        toolName: 'file_remove',
        toolCallId,
        status: 'error',
        message: normalizedMessage,
        errorCode,
      },
    };
  }
}
