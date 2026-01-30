/**
 * 文件执行器
 * 提供文件查看、创建、编辑功能
 *
 * 工具: text_editor
 * 命令: view, create, str_replace, insert, undo_edit
 */

import * as fs from 'fs/promises';
import * as path from 'path';
import { ToolExecutor, ExtendedToolDefinition } from './types';
import { ToolCall, ToolResult } from '../llm/types';
import { logger, LogCategory } from '../logging';

/**
 * 文件执行器
 */
export class FileExecutor implements ToolExecutor {
  private workspaceRoot: string;
  private undoStack: Map<string, string> = new Map();

  constructor(workspaceRoot: string) {
    this.workspaceRoot = workspaceRoot;
  }

  /**
   * 获取工具定义
   */
  getToolDefinition(): ExtendedToolDefinition {
    return {
      name: 'text_editor',
      description: `Edit text files using commands like view, create, str_replace, insert, and undo_edit.

Commands:
* view - View file content with line numbers (cat -n style)
* create - Create a new file (cannot overwrite existing files)
* str_replace - Replace text in file (old_str must match EXACTLY)
* insert - Insert text at a specific line number
* undo_edit - Undo the last edit to a file

Notes for str_replace:
* old_str must match EXACTLY including whitespace
* new_str can be empty to delete content

Notes for insert:
* insert_line is 1-based line number
* Text is inserted AFTER the specified line
* Use insert_line: 0 to insert at the beginning

IMPORTANT:
* This is the only tool for editing files
* DO NOT use sed/awk/shell commands for editing
* Use view command before editing to see file content`,
      input_schema: {
        type: 'object',
        properties: {
          command: {
            type: 'string',
            enum: ['view', 'create', 'str_replace', 'insert', 'undo_edit'],
            description: 'The editing command to execute'
          },
          path: {
            type: 'string',
            description: 'File path relative to workspace'
          },
          file_text: {
            type: 'string',
            description: 'Content for create command'
          },
          old_str: {
            type: 'string',
            description: 'String to replace (for str_replace)'
          },
          new_str: {
            type: 'string',
            description: 'Replacement string (for str_replace)'
          },
          insert_line: {
            type: 'number',
            description: 'Line number to insert at (for insert)'
          },
          insert_text: {
            type: 'string',
            description: 'Text to insert (for insert)'
          },
          view_range: {
            type: 'array',
            items: { type: 'number' },
            description: 'Line range [start, end] for view (1-based, inclusive)'
          }
        },
        required: ['command', 'path']
      },
      metadata: {
        source: 'builtin',
        category: 'file',
        tags: ['file', 'edit', 'development']
      }
    };
  }

  /**
   * 获取工具定义列表（兼容多工具执行器）
   */
  getToolDefinitions(): ExtendedToolDefinition[] {
    return [this.getToolDefinition()];
  }

  /**
   * 获取所有工具（实现 ToolExecutor 接口）
   */
  async getTools(): Promise<ExtendedToolDefinition[]> {
    return this.getToolDefinitions();
  }

  /**
   * 检查工具是否可用
   */
  async isAvailable(toolName: string): Promise<boolean> {
    return toolName === 'text_editor';
  }

  /**
   * 执行工具调用
   */
  async execute(toolCall: ToolCall): Promise<ToolResult> {
    const { command, path: filePath } = toolCall.arguments as {
      command: string;
      path: string;
    };

    if (!command || !filePath) {
      return {
        toolCallId: toolCall.id,
        content: 'Error: command and path are required',
        isError: true
      };
    }

    const resolved = this.resolveWorkspacePath(filePath);
    if (!resolved) {
      return {
        toolCallId: toolCall.id,
        content: `Error: path is outside workspace: ${filePath}`,
        isError: true
      };
    }

    logger.debug('FileExecutor executing', { command, path: filePath }, LogCategory.TOOLS);

    try {
      switch (command) {
        case 'view':
          return await this.executeView(toolCall.id, resolved, toolCall.arguments);
        case 'create':
          return await this.executeCreate(toolCall.id, resolved, toolCall.arguments);
        case 'str_replace':
          return await this.executeStrReplace(toolCall.id, resolved, toolCall.arguments);
        case 'insert':
          return await this.executeInsert(toolCall.id, resolved, toolCall.arguments);
        case 'undo_edit':
          return await this.executeUndo(toolCall.id, resolved);
        default:
          return {
            toolCallId: toolCall.id,
            content: `Error: unsupported command ${command}`,
            isError: true
          };
      }
    } catch (error: any) {
      logger.error('FileExecutor error', { command, error: error.message }, LogCategory.TOOLS);
      return {
        toolCallId: toolCall.id,
        content: `Error: ${error.message}`,
        isError: true
      };
    }
  }

  /**
   * 查看文件内容
   */
  private async executeView(
    toolCallId: string,
    filePath: string,
    args: Record<string, any>
  ): Promise<ToolResult> {
    const viewRange = args.view_range as [number, number] | undefined;

    try {
      const stat = await fs.stat(filePath);

      if (stat.isDirectory()) {
        // 列出目录内容（最多2层深度）
        const content = await this.listDirectory(filePath, 2);
        return {
          toolCallId,
          content,
          isError: false
        };
      }

      // 读取文件内容
      let content = await fs.readFile(filePath, 'utf-8');
      const lines = content.split('\n');

      // 应用行范围
      let startLine = 1;
      let endLine = lines.length;

      if (viewRange && viewRange.length === 2) {
        startLine = Math.max(1, viewRange[0]);
        endLine = viewRange[1] === -1 ? lines.length : Math.min(lines.length, viewRange[1]);
      }

      // 格式化输出（带行号）
      const result = lines
        .slice(startLine - 1, endLine)
        .map((line, idx) => `${String(startLine + idx).padStart(6)}\t${line}`)
        .join('\n');

      // 截断过长的输出
      const maxChars = 50000;
      if (result.length > maxChars) {
        return {
          toolCallId,
          content: result.substring(0, maxChars) + '\n<response clipped>',
          isError: false
        };
      }

      return {
        toolCallId,
        content: result,
        isError: false
      };
    } catch (error: any) {
      return {
        toolCallId,
        content: `Error reading file: ${error.message}`,
        isError: true
      };
    }
  }

  /**
   * 列出目录内容
   */
  private async listDirectory(dirPath: string, maxDepth: number, currentDepth = 0): Promise<string> {
    if (currentDepth >= maxDepth) {
      return '';
    }

    const entries = await fs.readdir(dirPath, { withFileTypes: true });
    const lines: string[] = [];
    const indent = '  '.repeat(currentDepth);

    for (const entry of entries) {
      if (entry.name.startsWith('.')) continue; // 跳过隐藏文件

      const fullPath = path.join(dirPath, entry.name);

      if (entry.isDirectory()) {
        lines.push(`${indent}${entry.name}/`);
        const subContent = await this.listDirectory(fullPath, maxDepth, currentDepth + 1);
        if (subContent) {
          lines.push(subContent);
        }
      } else {
        lines.push(`${indent}${entry.name}`);
      }
    }

    return lines.join('\n');
  }

  /**
   * 创建新文件
   */
  private async executeCreate(
    toolCallId: string,
    filePath: string,
    args: Record<string, any>
  ): Promise<ToolResult> {
    const fileText = args.file_text ?? '';

    // 检查文件是否已存在
    try {
      await fs.access(filePath);
      return {
        toolCallId,
        content: 'Error: file already exists. Use str_replace to edit existing files.',
        isError: true
      };
    } catch {
      // 文件不存在，可以创建
    }

    // 创建目录
    await fs.mkdir(path.dirname(filePath), { recursive: true });

    // 写入文件
    await fs.writeFile(filePath, fileText, 'utf-8');

    logger.info('File created', { path: filePath }, LogCategory.TOOLS);

    return {
      toolCallId,
      content: 'OK: file created',
      isError: false
    };
  }

  /**
   * 替换文本
   */
  private async executeStrReplace(
    toolCallId: string,
    filePath: string,
    args: Record<string, any>
  ): Promise<ToolResult> {
    const oldStr = args.old_str;
    const newStr = args.new_str ?? '';

    if (typeof oldStr !== 'string') {
      return {
        toolCallId,
        content: 'Error: old_str is required',
        isError: true
      };
    }

    let content: string;
    try {
      content = await fs.readFile(filePath, 'utf-8');
    } catch (error: any) {
      return {
        toolCallId,
        content: `Error: ${error.message}`,
        isError: true
      };
    }

    const index = content.indexOf(oldStr);
    if (index === -1) {
      return {
        toolCallId,
        content: 'Error: old_str not found in file',
        isError: true
      };
    }

    // 保存撤销信息
    this.undoStack.set(filePath, content);

    // 执行替换
    const updated = content.replace(oldStr, newStr);
    await fs.writeFile(filePath, updated, 'utf-8');

    logger.info('File edited (str_replace)', { path: filePath }, LogCategory.TOOLS);

    return {
      toolCallId,
      content: 'OK: str_replace applied',
      isError: false
    };
  }

  /**
   * 插入文本
   */
  private async executeInsert(
    toolCallId: string,
    filePath: string,
    args: Record<string, any>
  ): Promise<ToolResult> {
    const insertLine = Number(args.insert_line);
    const insertText = args.insert_text ?? '';

    if (!Number.isFinite(insertLine) || insertLine < 0) {
      return {
        toolCallId,
        content: 'Error: insert_line must be a non-negative number',
        isError: true
      };
    }

    let content = '';
    try {
      content = await fs.readFile(filePath, 'utf-8');
    } catch (error: any) {
      if (error?.code !== 'ENOENT') {
        return {
          toolCallId,
          content: `Error: ${error.message}`,
          isError: true
        };
      }
      // 文件不存在，从空内容开始
    }

    const lines = content.split('\n');

    if (insertLine > lines.length) {
      return {
        toolCallId,
        content: 'Error: insert_line out of range',
        isError: true
      };
    }

    // 保存撤销信息
    this.undoStack.set(filePath, content);

    // 在指定行后插入
    lines.splice(insertLine, 0, insertText);

    // 创建目录并写入
    await fs.mkdir(path.dirname(filePath), { recursive: true });
    await fs.writeFile(filePath, lines.join('\n'), 'utf-8');

    logger.info('File edited (insert)', { path: filePath, line: insertLine }, LogCategory.TOOLS);

    return {
      toolCallId,
      content: 'OK: text inserted',
      isError: false
    };
  }

  /**
   * 撤销编辑
   */
  private async executeUndo(toolCallId: string, filePath: string): Promise<ToolResult> {
    if (!this.undoStack.has(filePath)) {
      return {
        toolCallId,
        content: 'Error: no undo history for this file',
        isError: true
      };
    }

    const previous = this.undoStack.get(filePath)!;
    await fs.writeFile(filePath, previous, 'utf-8');
    this.undoStack.delete(filePath);

    logger.info('File undo applied', { path: filePath }, LogCategory.TOOLS);

    return {
      toolCallId,
      content: 'OK: undo applied',
      isError: false
    };
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
