/**
 * 文件删除执行器
 * 提供安全的文件删除功能
 *
 * 工具: remove_files
 */

import * as fs from 'fs/promises';
import * as path from 'path';
import { ToolExecutor, ExtendedToolDefinition } from './types';
import { ToolCall, ToolResult } from '../llm/types';
import { logger, LogCategory } from '../logging';

/**
 * 文件删除执行器
 */
export class RemoveFilesExecutor implements ToolExecutor {
  private workspaceRoot: string;
  private deletedFiles: Map<string, string> = new Map(); // 用于恢复

  /** 文件删除前回调（用于快照系统在删除前保存原始内容） */
  private onBeforeWrite?: (filePath: string) => void;

  constructor(workspaceRoot: string) {
    this.workspaceRoot = workspaceRoot;
  }

  /**
   * 设置文件删除前回调
   */
  setBeforeWriteCallback(callback: (filePath: string) => void): void {
    this.onBeforeWrite = callback;
  }

  /**
   * 获取工具定义
   */
  getToolDefinition(): ExtendedToolDefinition {
    return {
      name: 'remove_files',
      description: `Delete files from workspace safely.

* Supports batch deletion
* Files can be recovered (backup mechanism)
* Path safety validation

IMPORTANT:
* Do NOT use shell commands (rm) to delete files
* This is the only safe way to delete files
* Paths must be relative to workspace root`,
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
    return toolName === 'remove_files';
  }

  /**
   * 执行工具调用
   */
  async execute(toolCall: ToolCall): Promise<ToolResult> {
    const args = toolCall.arguments as { paths: string[] };

    if (!args.paths || !Array.isArray(args.paths) || args.paths.length === 0) {
      return {
        toolCallId: toolCall.id,
        content: 'Error: paths array is required and must not be empty',
        isError: true
      };
    }

    logger.debug('RemoveFilesExecutor executing', { paths: args.paths }, LogCategory.TOOLS);

    const results: string[] = [];
    let successCount = 0;
    let errorCount = 0;

    for (const filePath of args.paths) {
      const resolved = this.resolveWorkspacePath(filePath);

      if (!resolved) {
        results.push(`✗ ${filePath}: path is outside workspace`);
        errorCount++;
        continue;
      }

      try {
        // 检查文件是否存在
        const stat = await fs.stat(resolved);

        if (stat.isDirectory()) {
          results.push(`✗ ${filePath}: is a directory (use recursive delete for directories)`);
          errorCount++;
          continue;
        }

        // 备份文件内容（用于恢复）
        const content = await fs.readFile(resolved, 'utf-8');
        this.deletedFiles.set(resolved, content);

        // 快照回调（在删除前通知快照系统保存原始内容）
        this.onBeforeWrite?.(resolved);

        // 删除文件
        await fs.unlink(resolved);

        results.push(`✓ ${filePath}: deleted`);
        successCount++;

        logger.info('File deleted', { path: filePath }, LogCategory.TOOLS);
      } catch (error: any) {
        if (error.code === 'ENOENT') {
          results.push(`✗ ${filePath}: file not found`);
        } else {
          results.push(`✗ ${filePath}: ${error.message}`);
        }
        errorCount++;
      }
    }

    const summary = `\nDeleted: ${successCount}, Errors: ${errorCount}`;
    const content = results.join('\n') + summary;

    return {
      toolCallId: toolCall.id,
      content,
      isError: errorCount > 0 && successCount === 0
    };
  }

  /**
   * 恢复已删除的文件
   */
  async restoreFile(filePath: string): Promise<boolean> {
    const resolved = this.resolveWorkspacePath(filePath);
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
      path.relative(this.workspaceRoot, p)
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
  private resolveWorkspacePath(inputPath: string): string | null {
    const resolved = path.resolve(this.workspaceRoot, inputPath);
    const normalizedRoot = path.resolve(this.workspaceRoot) + path.sep;

    // 检查路径是否在工作区内
    if (!resolved.startsWith(normalizedRoot) && resolved !== path.resolve(this.workspaceRoot)) {
      return null;
    }

    return resolved;
  }
}
