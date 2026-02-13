/**
 * 文件执行器
 * 提供文件查看、创建、编辑、插入功能，拆分为四个独立工具
 *
 * 工具:
 * - file_view: 查看文件内容或目录结构
 * - file_create: 创建或写入完整文件内容
 * - file_edit: 精确文本替换 / 撤销
 * - file_insert: 在指定行插入文本
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

  /** 文件写入前回调（用于快照系统在写入前保存原始内容） */
  private onBeforeWrite?: (filePath: string) => void;

  constructor(workspaceRoot: string) {
    this.workspaceRoot = workspaceRoot;
  }

  /**
   * 设置文件写入前回调
   * 每次 file_create/file_edit/file_insert 写入文件前会调用此回调
   */
  setBeforeWriteCallback(callback: (filePath: string) => void): void {
    this.onBeforeWrite = callback;
  }

  /**
   * 获取所有工具定义
   */
  getToolDefinitions(): ExtendedToolDefinition[] {
    return [
      this.getFileViewDefinition(),
      this.getFileCreateDefinition(),
      this.getFileEditDefinition(),
      this.getFileInsertDefinition(),
    ];
  }

  /**
   * file_view 工具定义
   */
  private getFileViewDefinition(): ExtendedToolDefinition {
    return {
      name: 'file_view',
      description: `View file content with line numbers, or list directory structure (up to 2 levels deep).

When path is a directory, returns a tree listing of its contents.
When path is a file, returns the file content with line numbers.

Options:
* view_range: [start, end] - Show specific line range (1-based, inclusive)
* search_query_regex: Search for patterns using regex
* case_sensitive: Control case sensitivity for search (default: false)
* context_lines: Lines of context around matches (default: 5)
When using regex search, only matching lines and their context are shown.
Strongly prefer search_query_regex over view_range when looking for specific symbols.

IMPORTANT:
* This is the primary tool for reading files and browsing directories
* Use on a directory path to explore project structure (e.g. path: "." or path: "src")
* Use on a file path to read file contents
* DO NOT use launch-process with ls/find/cat to explore files - use this tool instead
* Always use this tool to read a file before editing it`,
      input_schema: {
        type: 'object',
        properties: {
          path: {
            type: 'string',
            description: 'File or directory path relative to workspace root'
          },
          view_range: {
            type: 'array',
            items: { type: 'number' },
            description: 'Line range [start, end] for view (1-based, inclusive)'
          },
          search_query_regex: {
            type: 'string',
            description: 'Regex pattern to search within file'
          },
          case_sensitive: {
            type: 'boolean',
            description: 'Case sensitive search (default: false)'
          },
          context_lines: {
            type: 'number',
            description: 'Context lines around matches (default: 5)'
          }
        },
        required: ['path']
      },
      metadata: {
        source: 'builtin',
        category: 'file',
        tags: ['file', 'view', 'read']
      }
    };
  }

  /**
   * file_create 工具定义
   */
  private getFileCreateDefinition(): ExtendedToolDefinition {
    return {
      name: 'file_create',
      description: `Create a new file or overwrite an existing file with complete content.

* Creates parent directories automatically if they don't exist
* If the file already exists, it will be overwritten (snapshot system preserves the original)

IMPORTANT:
* Use this tool to create new files or completely rewrite existing files
* For partial modifications, use file_edit instead`,
      input_schema: {
        type: 'object',
        properties: {
          path: {
            type: 'string',
            description: 'File path relative to workspace root'
          },
          file_text: {
            type: 'string',
            description: 'Complete file content to write'
          }
        },
        required: ['path', 'file_text']
      },
      metadata: {
        source: 'builtin',
        category: 'file',
        tags: ['file', 'create', 'write']
      }
    };
  }

  /**
   * file_edit 工具定义
   */
  private getFileEditDefinition(): ExtendedToolDefinition {
    return {
      name: 'file_edit',
      description: `Edit a file by replacing text or undoing the last edit.

Modes:
1. Text replacement (old_str + new_str): Replace exact text in file
2. Undo (undo: true): Revert the last edit to the file

Notes for text replacement:
* ALWAYS use file_view to read the file before editing
* old_str must match EXACTLY including whitespace
* new_str can be empty to delete content
* Use old_str_start_line and old_str_end_line to disambiguate multiple occurrences
* Try to fit as many edits in one tool call as possible

IMPORTANT:
* For creating new files or full rewrites, use file_create instead
* DO NOT use sed/awk/shell commands for editing
* DO NOT fall back to removing and recreating files`,
      input_schema: {
        type: 'object',
        properties: {
          path: {
            type: 'string',
            description: 'File path relative to workspace root'
          },
          old_str: {
            type: 'string',
            description: 'String to replace (for text replacement mode)'
          },
          new_str: {
            type: 'string',
            description: 'Replacement string (for text replacement mode)'
          },
          old_str_start_line: {
            type: 'number',
            description: 'Start line number of old_str to disambiguate (1-based, inclusive)'
          },
          old_str_end_line: {
            type: 'number',
            description: 'End line number of old_str to disambiguate (1-based, inclusive)'
          },
          undo: {
            type: 'boolean',
            description: 'Set to true to undo the last edit to this file'
          }
        },
        required: ['path']
      },
      metadata: {
        source: 'builtin',
        category: 'file',
        tags: ['file', 'edit', 'development']
      }
    };
  }

  /**
   * file_insert 工具定义
   */
  private getFileInsertDefinition(): ExtendedToolDefinition {
    return {
      name: 'file_insert',
      description: `Insert text at a specific line number in a file.

* insert_line is 1-based line number
* Text is inserted AFTER the specified line
* Use insert_line: 0 to insert at the beginning of the file
* If the file does not exist, it will be created`,
      input_schema: {
        type: 'object',
        properties: {
          path: {
            type: 'string',
            description: 'File path relative to workspace root'
          },
          insert_line: {
            type: 'number',
            description: 'Line number to insert after (1-based, use 0 for beginning)'
          },
          new_str: {
            type: 'string',
            description: 'Text to insert'
          }
        },
        required: ['path', 'insert_line', 'new_str']
      },
      metadata: {
        source: 'builtin',
        category: 'file',
        tags: ['file', 'insert', 'development']
      }
    };
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
    return toolName === 'file_view' || toolName === 'file_create' || toolName === 'file_edit' || toolName === 'file_insert';
  }

  /**
   * 执行工具调用
   */
  async execute(toolCall: ToolCall): Promise<ToolResult> {
    const filePath = (toolCall.arguments as any)?.path as string;

    if (!filePath) {
      return {
        toolCallId: toolCall.id,
        content: 'Error: path is required',
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

    logger.debug('FileExecutor executing', { tool: toolCall.name, path: filePath }, LogCategory.TOOLS);

    try {
      switch (toolCall.name) {
        case 'file_view':
          return await this.executeView(toolCall.id, resolved, toolCall.arguments);
        case 'file_create':
          return await this.executeCreate(toolCall.id, resolved, toolCall.arguments);
        case 'file_edit':
          return await this.executeEdit(toolCall.id, resolved, toolCall.arguments);
        case 'file_insert':
          return await this.executeInsert(toolCall.id, resolved, toolCall.arguments);
        default:
          return {
            toolCallId: toolCall.id,
            content: `Error: unsupported tool ${toolCall.name}`,
            isError: true
          };
      }
    } catch (error: any) {
      logger.error('FileExecutor error', { tool: toolCall.name, error: error.message }, LogCategory.TOOLS);
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
    const searchQuery = args.search_query_regex as string | undefined;
    const caseSensitive = args.case_sensitive as boolean | undefined ?? false;
    const contextLines = args.context_lines as number | undefined ?? 5;

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
      const content = await fs.readFile(filePath, 'utf-8');
      const lines = content.split('\n');

      // 如果有正则搜索，优先使用搜索模式
      if (searchQuery) {
        return this.executeViewWithSearch(
          toolCallId,
          lines,
          searchQuery,
          caseSensitive,
          contextLines,
          viewRange
        );
      }

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
      // 增强错误反馈：文件不存在时提供相似文件建议
      if (error.code === 'ENOENT') {
        const suggestions = await this.findSimilarFiles(filePath);
        let errorMsg = `Error: File not found: ${path.relative(this.workspaceRoot, filePath)}`;
        if (suggestions.length > 0) {
          errorMsg += `\n\nDid you mean one of these?\n${suggestions.map(s => `  - ${s}`).join('\n')}`;
        }
        return {
          toolCallId,
          content: errorMsg,
          isError: true
        };
      }
      return {
        toolCallId,
        content: `Error reading file: ${error.message}`,
        isError: true
      };
    }
  }

  /**
   * 带正则搜索的文件查看
   */
  private executeViewWithSearch(
    toolCallId: string,
    lines: string[],
    searchQuery: string,
    caseSensitive: boolean,
    contextLines: number,
    viewRange?: [number, number]
  ): ToolResult {
    try {
      const regex = new RegExp(searchQuery, caseSensitive ? 'g' : 'gi');

      // 确定搜索范围
      let startLine = 1;
      let endLine = lines.length;
      if (viewRange && viewRange.length === 2) {
        startLine = Math.max(1, viewRange[0]);
        endLine = viewRange[1] === -1 ? lines.length : Math.min(lines.length, viewRange[1]);
      }

      // 查找匹配行
      const matchingLineIndices: number[] = [];
      for (let i = startLine - 1; i < endLine; i++) {
        regex.lastIndex = 0;
        if (regex.test(lines[i])) {
          matchingLineIndices.push(i);
        }
      }

      if (matchingLineIndices.length === 0) {
        return {
          toolCallId,
          content: `No matches found for pattern: ${searchQuery}`,
          isError: false
        };
      }

      // 构建带上下文的输出
      const outputLines: string[] = [];
      let lastPrintedLine = -1;

      for (const matchIdx of matchingLineIndices) {
        const contextStart = Math.max(0, matchIdx - contextLines);
        const contextEnd = Math.min(lines.length - 1, matchIdx + contextLines);

        // 如果与上一个匹配区域不连续，添加省略号
        if (lastPrintedLine >= 0 && contextStart > lastPrintedLine + 1) {
          outputLines.push('...');
        }

        // 输出上下文和匹配行
        for (let i = contextStart; i <= contextEnd; i++) {
          if (i > lastPrintedLine) {
            const lineNum = String(i + 1).padStart(6);
            const marker = i === matchIdx ? '>' : ' ';
            outputLines.push(`${lineNum}${marker}\t${lines[i]}`);
            lastPrintedLine = i;
          }
        }
      }

      outputLines.push(`\n[Found ${matchingLineIndices.length} matches]`);

      const result = outputLines.join('\n');
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
        content: `Error in regex search: ${error.message}`,
        isError: true
      };
    }
  }

  /**
   * 查找相似文件（用于错误提示）
   */
  private async findSimilarFiles(targetPath: string): Promise<string[]> {
    const targetName = path.basename(targetPath).toLowerCase();
    const targetDir = path.dirname(targetPath);
    const suggestions: string[] = [];

    try {
      // 尝试在同目录下查找相似文件
      const entries = await fs.readdir(targetDir, { withFileTypes: true });
      for (const entry of entries) {
        if (entry.isFile()) {
          const name = entry.name.toLowerCase();
          // 简单的相似度检查：包含目标名称的一部分
          if (name.includes(targetName.slice(0, 3)) || targetName.includes(name.slice(0, 3))) {
            suggestions.push(path.relative(this.workspaceRoot, path.join(targetDir, entry.name)));
            if (suggestions.length >= 5) break;
          }
        }
      }
    } catch {
      // 目录不存在，忽略
    }

    return suggestions;
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
   * 创建/写入完整文件内容
   */
  private async executeCreate(
    toolCallId: string,
    filePath: string,
    args: Record<string, any>
  ): Promise<ToolResult> {
    const fileText = args.file_text ?? '';

    // 检查文件是否已存在
    let fileExists = false;
    try {
      await fs.access(filePath);
      fileExists = true;
    } catch {
      // 文件不存在
    }

    // 创建目录
    await fs.mkdir(path.dirname(filePath), { recursive: true });

    // 快照回调（覆写时保护原始内容）
    this.onBeforeWrite?.(filePath);

    // 写入文件
    await fs.writeFile(filePath, fileText, 'utf-8');

    if (fileExists) {
      logger.info('File overwritten via file_create', { path: filePath }, LogCategory.TOOLS);
      return {
        toolCallId,
        content: 'OK: file overwritten (file already existed)',
        isError: false
      };
    }

    logger.info('File created via file_create', { path: filePath }, LogCategory.TOOLS);

    return {
      toolCallId,
      content: 'OK: file created',
      isError: false
    };
  }

  /**
   * 编辑文件（文本替换 / 撤销）
   */
  private async executeEdit(
    toolCallId: string,
    filePath: string,
    args: Record<string, any>
  ): Promise<ToolResult> {
    // 模式 1：撤销
    if (args.undo === true) {
      return await this.executeUndo(toolCallId, filePath);
    }

    // 模式 2：文本替换
    return await this.executeStrReplace(toolCallId, filePath, args);
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
    const startLine = args.old_str_start_line as number | undefined;
    const endLine = args.old_str_end_line as number | undefined;

    if (typeof oldStr !== 'string') {
      return {
        toolCallId,
        content: 'Error: old_str is required for text replacement mode',
        isError: true
      };
    }

    if (oldStr === newStr) {
      return {
        toolCallId,
        content: 'Error: No replacement was performed because old_str and new_str are identical.',
        isError: true
      };
    }

    let content: string;
    try {
      content = await fs.readFile(filePath, 'utf-8');
    } catch (error: any) {
      // 增强错误反馈：文件不存在时提供相似文件建议
      if (error.code === 'ENOENT') {
        const suggestions = await this.findSimilarFiles(filePath);
        let errorMsg = `Error: File not found: ${path.relative(this.workspaceRoot, filePath)}`;
        if (suggestions.length > 0) {
          errorMsg += `\n\nDid you mean one of these?\n${suggestions.map(s => `  - ${s}`).join('\n')}`;
        }
        return {
          toolCallId,
          content: errorMsg,
          isError: true
        };
      }
      return {
        toolCallId,
        content: `Error: ${error.message}`,
        isError: true
      };
    }

    const lines = content.split('\n');

    // 如果指定了行号范围，在该范围内查找 old_str
    if (startLine !== undefined && endLine !== undefined) {
      // 验证行号范围
      if (startLine < 1 || endLine < startLine || endLine > lines.length) {
        return {
          toolCallId,
          content: `Error: Invalid line range [${startLine}, ${endLine}]. File has ${lines.length} lines.`,
          isError: true
        };
      }

      // 提取指定行范围的内容
      const rangeContent = lines.slice(startLine - 1, endLine).join('\n');

      if (!rangeContent.includes(oldStr)) {
        return {
          toolCallId,
          content: `Error: old_str not found in lines ${startLine}-${endLine}.\n\nContent in that range:\n${rangeContent.substring(0, 500)}${rangeContent.length > 500 ? '...' : ''}`,
          isError: true
        };
      }

      // 计算范围内的偏移量
      let beforeRange = '';
      if (startLine > 1) {
        beforeRange = lines.slice(0, startLine - 1).join('\n') + '\n';
      }
      const afterRange = endLine < lines.length ? '\n' + lines.slice(endLine).join('\n') : '';

      // 在范围内执行替换
      const updatedRange = rangeContent.replace(oldStr, newStr);

      // 保存撤销信息
      this.undoStack.set(filePath, content);

      // 快照回调
      this.onBeforeWrite?.(filePath);

      // 组装最终内容
      const updated = beforeRange + updatedRange + afterRange;
      await fs.writeFile(filePath, updated, 'utf-8');

      logger.info('File edited (file_edit with line range)', { path: filePath, startLine, endLine }, LogCategory.TOOLS);

      return {
        toolCallId,
        content: `OK: edit applied in lines ${startLine}-${endLine}`,
        isError: false
      };
    }

    // 没有行号范围，使用全文搜索逻辑
    const index = content.indexOf(oldStr);
    if (index === -1) {
      return {
        toolCallId,
        content: 'Error: old_str not found in file. Make sure old_str matches EXACTLY including whitespace.',
        isError: true
      };
    }

    // 检查是否有多个匹配
    const secondIndex = content.indexOf(oldStr, index + 1);
    if (secondIndex !== -1) {
      // 找出所有匹配的行号
      const matchLines: number[] = [];
      let searchPos = 0;
      while (true) {
        const pos = content.indexOf(oldStr, searchPos);
        if (pos === -1) break;
        const lineNum = content.substring(0, pos).split('\n').length;
        matchLines.push(lineNum);
        searchPos = pos + 1;
      }

      return {
        toolCallId,
        content: `Error: old_str appears multiple times in the file (at lines: ${matchLines.join(', ')}).\n\nUse old_str_start_line and old_str_end_line parameters to specify which occurrence to replace.`,
        isError: true
      };
    }

    // 保存撤销信息
    this.undoStack.set(filePath, content);

    // 快照回调
    this.onBeforeWrite?.(filePath);

    // 执行替换
    const updated = content.replace(oldStr, newStr);
    await fs.writeFile(filePath, updated, 'utf-8');

    logger.info('File edited (file_edit)', { path: filePath }, LogCategory.TOOLS);

    return {
      toolCallId,
      content: 'OK: edit applied',
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
    const insertText = args.new_str ?? '';

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

    // 快照回调
    this.onBeforeWrite?.(filePath);

    // 在指定行后插入
    lines.splice(insertLine, 0, insertText);

    // 创建目录并写入
    await fs.mkdir(path.dirname(filePath), { recursive: true });
    await fs.writeFile(filePath, lines.join('\n'), 'utf-8');

    logger.info('File edited (file_insert)', { path: filePath, line: insertLine }, LogCategory.TOOLS);

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

    // 快照回调（undo 也是文件变更，需要在写入前记录原始状态）
    this.onBeforeWrite?.(filePath);

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
