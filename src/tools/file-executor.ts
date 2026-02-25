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
import { ToolCall, ToolResult, FileChangeMetadata } from '../llm/types';
import { logger, LogCategory } from '../logging';
import { WorkspaceRoots } from '../workspace/workspace-roots';

/** 行号误差容忍度（20%，对齐 Augment _lineNumberErrorTolerance） */
const LINE_NUMBER_ERROR_TOLERANCE = 0.2;

/** 单条替换条目 */
interface EditEntry {
  index: number;
  oldStr: string;
  newStr: string;
  startLine?: number;
  endLine?: number;
}

/** 单条插入条目（对齐 Augment InsertEntry） */
interface InsertEntry {
  index: number;
  insertLine: number;
  newStr: string;
}

/** 匹配位置信息（0-based 行号） */
interface MatchLocation {
  startLine: number;
  endLine: number;
}

/** 缩进信息 */
interface IndentInfo {
  type: 'tab' | 'space';
  size: number;
}

/** 单条替换结果 */
interface ReplaceResult {
  newContent?: string;
  message?: string;
  error?: string;
  newStrStartLine?: number;  // 0-based
  newStrEndLine?: number;    // 0-based
}

/**
 * 文件执行器
 */
export class FileExecutor implements ToolExecutor {
  private workspaceRoots: WorkspaceRoots;
  private undoStack: Map<string, string> = new Map();

  /** 文件写入前回调（用于快照系统在写入前保存原始内容） */
  private onBeforeWrite?: (filePath: string) => void;

  constructor(workspaceRoots: WorkspaceRoots) {
    this.workspaceRoots = workspaceRoots;
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

多工作区路径规则:
* 单工作区可直接使用相对路径，如 "src/index.ts"
* 多工作区写入必须使用 "<工作区名>/相对路径"（例如 "backend/src/app.ts"）
* 多工作区读取可省略前缀，但若同名路径冲突会要求补充前缀

Options:
* view_range: [start, end] - Show specific line range (1-based, inclusive). Setting end to -1 shows all lines from start to end of file.
* search_query_regex: Search for patterns using regex
* case_sensitive: Control case sensitivity for search (default: false)
* context_lines_before: Lines of context before each match (default: 5)
* context_lines_after: Lines of context after each match (default: 5)
* type: "file" or "directory" (default: "file")
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
          type: {
            type: 'string',
            description: "Type of path: 'file' or 'directory' (default: 'file')"
          },
          view_range: {
            type: 'array',
            items: { type: 'number' },
            description: 'Line range [start, end] for view (1-based, inclusive). Setting end to -1 shows all lines from start.'
          },
          search_query_regex: {
            type: 'string',
            description: 'Regex pattern to search within file'
          },
          case_sensitive: {
            type: 'boolean',
            description: 'Case sensitive search (default: false)'
          },
          context_lines_before: {
            type: 'number',
            description: 'Lines of context before each match (default: 5)'
          },
          context_lines_after: {
            type: 'number',
            description: 'Lines of context after each match (default: 5)'
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
   * file_create 工具定义（对齐 Augment save-file）
   */
  private getFileCreateDefinition(): ExtendedToolDefinition {
    return {
      name: 'file_create',
      description: `Save a new file. Use this tool to write new files with the attached content.
Generate \`instructions_reminder\` first to remind yourself to limit the file content to at most 150 lines.
It CANNOT modify existing files. Do NOT use this tool to edit an existing file by overwriting it entirely.
Use the file_edit tool to edit existing files instead.`,
      input_schema: {
        type: 'object',
        properties: {
          instructions_reminder: {
            type: 'string',
            description: "Should be exactly this string: 'LIMIT THE FILE CONTENT TO AT MOST 150 LINES. IF MORE CONTENT NEEDS TO BE ADDED USE THE file_edit TOOL TO EDIT THE FILE AFTER IT HAS BEEN CREATED.'"
          },
          path: {
            type: 'string',
            description: 'The path of the file to save'
          },
          file_content: {
            type: 'string',
            description: 'The content of the file'
          },
          add_last_line_newline: {
            type: 'boolean',
            description: 'Whether to add a newline at the end of the file (default: true)'
          }
        },
        required: ['instructions_reminder', 'path', 'file_content']
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
      description: `Edit a file by replacing text. Supports multiple replacements in one call.

Notes for text replacement:
* ALWAYS use file_view to read the file before editing
* Specify old_str_1, new_str_1, old_str_start_line_1 and old_str_end_line_1 for the first replacement, old_str_2, new_str_2, old_str_start_line_2 and old_str_end_line_2 for the second replacement, and so on
* old_str_start_line and old_str_end_line are 1-based line numbers (both inclusive)
* old_str must match EXACTLY one or more consecutive lines from the original file. Be mindful of whitespace!
* new_str can be empty to delete content
* It is important to specify old_str_start_line and old_str_end_line to disambiguate between multiple occurrences of old_str in the file
* Make sure that line ranges from different entries do not overlap
* To make multiple replacements in one tool call, add multiple sets of numbered parameters
* Set undo to true to revert the last edit

IMPORTANT:
* For creating new files or full rewrites, use file_create instead
* DO NOT use sed/awk/shell commands for editing
* DO NOT fall back to removing and recreating files
* Try to fit as many edits in one tool call as possible`,
      input_schema: {
        type: 'object',
        properties: {
          path: {
            type: 'string',
            description: 'File path relative to workspace root'
          },
          old_str_1: {
            type: 'string',
            description: 'String to replace for 1st replacement. Use old_str_2, old_str_3, etc. for more.'
          },
          new_str_1: {
            type: 'string',
            description: 'Replacement string for 1st replacement. Use new_str_2, new_str_3, etc. for more.'
          },
          old_str_start_line_1: {
            type: 'number',
            description: 'Start line of old_str_1 (1-based). Use old_str_start_line_2, etc. for more.'
          },
          old_str_end_line_1: {
            type: 'number',
            description: 'End line of old_str_1 (1-based). Use old_str_end_line_2, etc. for more.'
          },
          instruction_reminder: {
            type: 'string',
            description: "Reminder to limit edits to at most 150 lines. Should be exactly this string: 'ALWAYS BREAK DOWN EDITS INTO SMALLER CHUNKS OF AT MOST 150 LINES EACH.'"
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
   * file_insert 工具定义（对齐 Augment str-replace-editor insert 命令）
   */
  private getFileInsertDefinition(): ExtendedToolDefinition {
    return {
      name: 'file_insert',
      description: `Insert text at a specific line number in a file. Supports multiple insertions in one call.

Notes for using this tool:
* Specify \`insert_line_1\` and \`new_str_1\` properties for the first insertion, \`insert_line_2\` and \`new_str_2\` for the second insertion, and so on
* The \`insert_line_1\` parameter specifies the line number after which to insert the new string
* The \`insert_line_1\` parameter is 1-based line number
* To insert at the very beginning of the file, use \`insert_line_1: 0\`
* To make multiple insertions in one tool call add multiple sets of insertion parameters. For example, \`insert_line_1\` and \`new_str_1\` properties for the first insertion, \`insert_line_2\` and \`new_str_2\` for the second insertion, etc.

IMPORTANT:
* Use the file_view tool to read a file before inserting into it
* If the file does not exist, it will be created`,
      input_schema: {
        type: 'object',
        properties: {
          instruction_reminder: {
            type: 'string',
            description: "Reminder to limit edits to at most 150 lines. Should be exactly this string: 'ALWAYS BREAK DOWN EDITS INTO SMALLER CHUNKS OF AT MOST 150 LINES EACH.'"
          },
          path: {
            type: 'string',
            description: 'File path relative to workspace root'
          },
          insert_line_1: {
            type: 'integer',
            description: 'Required parameter for insert. The line number after which to insert the new string. This line number is relative to the state of the file before any insertions in the current tool call have been applied.'
          },
          new_str_1: {
            type: 'string',
            description: 'The string to insert.'
          }
        },
        required: ['path']
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

    const pathResolution = this.resolveWorkspacePath(
      filePath,
      this.shouldRequireExistingPath(toolCall.name, toolCall.arguments)
    );
    if (!pathResolution.absolutePath) {
      return {
        toolCallId: toolCall.id,
        content: pathResolution.error || `Error: path is outside workspace: ${filePath}`,
        isError: true
      };
    }
    const resolved = pathResolution.absolutePath;

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

  private shouldRequireExistingPath(toolName: string, args: Record<string, any>): boolean {
    if (toolName === 'file_view') {
      return true;
    }
    if (toolName === 'file_edit') {
      return args?.undo !== true;
    }
    return false;
  }

  /**
   * 查看文件内容
   */
  private async executeView(
    toolCallId: string,
    filePath: string,
    args: Record<string, any>
  ): Promise<ToolResult> {
    // — 输入验证（对齐 Augment validateInputs） —
    const pathType: string = args.type ?? 'file';
    if (args.type !== undefined && typeof args.type !== 'string') {
      return { toolCallId, content: "Error: Parameter 'type' must be a string", isError: true };
    }
    if (args.view_range !== undefined) {
      if (!Array.isArray(args.view_range) || args.view_range.length !== 2) {
        return { toolCallId, content: "Error: Parameter 'view_range' must be an array of two numbers", isError: true };
      }
      if (!args.view_range.every((v: any) => typeof v === 'number')) {
        return { toolCallId, content: "Error: Parameter 'view_range' must contain only numbers", isError: true };
      }
    }
    if (args.search_query_regex !== undefined && typeof args.search_query_regex !== 'string') {
      return { toolCallId, content: "Error: Parameter 'search_query_regex' must be a string", isError: true };
    }
    if (args.case_sensitive !== undefined && typeof args.case_sensitive !== 'boolean') {
      return { toolCallId, content: "Error: Parameter 'case_sensitive' must be a boolean", isError: true };
    }
    if (args.context_lines_before !== undefined) {
      if (typeof args.context_lines_before !== 'number' || !Number.isInteger(args.context_lines_before) || args.context_lines_before < 0) {
        return { toolCallId, content: "Error: Parameter 'context_lines_before' must be a non-negative integer", isError: true };
      }
    }
    if (args.context_lines_after !== undefined) {
      if (typeof args.context_lines_after !== 'number' || !Number.isInteger(args.context_lines_after) || args.context_lines_after < 0) {
        return { toolCallId, content: "Error: Parameter 'context_lines_after' must be a non-negative integer", isError: true };
      }
    }

    const viewRange = args.view_range as [number, number] | undefined;
    const searchQuery = args.search_query_regex as string | undefined;
    const caseSensitive = args.case_sensitive ?? false;
    const contextLinesBefore: number = args.context_lines_before ?? 5;
    const contextLinesAfter: number = args.context_lines_after ?? 5;

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
          filePath,
          lines,
          searchQuery,
          caseSensitive,
          contextLinesBefore,
          contextLinesAfter,
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
        if (pathType === 'directory') {
          return { toolCallId, content: `Error: Directory not found: ${this.workspaceRoots.toDisplayPath(filePath)}`, isError: true };
        }
        const suggestions = await this.findSimilarFiles(filePath);
        let errorMsg = `Error: File not found: ${this.workspaceRoots.toDisplayPath(filePath)}`;
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
   * 带正则搜索的文件查看（对齐 Augment handleRegexSearch）
   */
  private executeViewWithSearch(
    toolCallId: string,
    filePath: string,
    lines: string[],
    searchQuery: string,
    caseSensitive: boolean,
    contextLinesBefore: number,
    contextLinesAfter: number,
    viewRange?: [number, number]
  ): ToolResult {
    try {
      // 对齐 Augment: 不使用 'g' flag（test() 逐行检测即可）
      const flags = caseSensitive ? '' : 'i';
      const regex = new RegExp(searchQuery, flags);

      // 确定搜索范围
      let searchStart = 0;
      let searchEnd = lines.length - 1;
      if (viewRange && viewRange.length === 2) {
        searchStart = Math.max(0, viewRange[0] - 1);
        searchEnd = viewRange[1] === -1 ? lines.length - 1 : Math.min(lines.length - 1, viewRange[1] - 1);
      }

      // 查找匹配行
      const matches: Array<{ lineNum: number; line: string }> = [];
      for (let i = searchStart; i <= searchEnd && i < lines.length; i++) {
        regex.lastIndex = 0;
        if (regex.test(lines[i])) {
          matches.push({ lineNum: i + 1, line: lines[i] });
        }
      }

      if (matches.length === 0) {
        const scopeInfo = viewRange ? ` within lines ${searchStart + 1}-${searchEnd + 1}` : '';
        const displayPath = this.workspaceRoots.toDisplayPath(filePath);
        return {
          toolCallId,
          content: `No matches found for regex pattern: ${searchQuery}${scopeInfo} in ${displayPath}`,
          isError: false
        };
      }

      // 构建带上下文的输出（对齐 Augment 格式）
      const outputLines: string[] = [];
      const displayPath = this.workspaceRoots.toDisplayPath(filePath);
      outputLines.push(`Regex search results for pattern: ${searchQuery} in ${displayPath}`);
      if (viewRange) {
        outputLines.push(`Search limited to lines ${searchStart + 1}-${searchEnd + 1}`);
      }
      outputLines.push(`Found ${matches.length} matching lines:\n`);

      let lastPrintedLine = -1;

      for (const match of matches) {
        const matchIdx = match.lineNum - 1;
        const contextStart = Math.max(0, matchIdx - contextLinesBefore);
        const contextEnd = Math.min(lines.length - 1, matchIdx + contextLinesAfter);

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

      outputLines.push(`\nTotal matches: ${matches.length}`);
      outputLines.push(`Total lines in file: ${lines.length}`);

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
      if (error instanceof SyntaxError || error.message?.includes('Invalid regular expression')) {
        return {
          toolCallId,
          content: `Invalid regex pattern: ${searchQuery} - ${error.message}`,
          isError: true
        };
      }
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
            suggestions.push(this.workspaceRoots.toDisplayPath(path.join(targetDir, entry.name)));
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
   * 创建/写入文件内容（对齐 Augment save-file）
   */
  private async executeCreate(
    toolCallId: string,
    filePath: string,
    args: Record<string, any>
  ): Promise<ToolResult> {
    const fileContent: string = args.file_content ?? '';
    const addLastLineNewline: boolean = args.add_last_line_newline ?? true;
    const finalContent = fileContent + (addLastLineNewline ? '\n' : '');

    // 读取原始内容（覆写场景用于 diff）
    let originalContent = '';
    try {
      originalContent = await fs.readFile(filePath, 'utf-8');
    } catch { /* 新建文件，原始内容为空 */ }

    // 创建目录
    await fs.mkdir(path.dirname(filePath), { recursive: true });

    // 快照回调（覆写时保护原始内容）
    this.onBeforeWrite?.(filePath);

    // 写入文件
    await fs.writeFile(filePath, finalContent, 'utf-8');

    logger.info('File created via file_create', { path: filePath }, LogCategory.TOOLS);

    const changeType = originalContent ? 'modify' as const : 'create' as const;
    return {
      toolCallId,
      content: `OK: file created at ${this.workspaceRoots.toDisplayPath(filePath)}`,
      isError: false,
      fileChange: this.buildFileChangeMetadata(originalContent, finalContent, filePath, changeType),
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

    // 提取替换条目（支持单条和多条编号参数）
    const entries = this.extractEditEntries(args);
    if (entries.length === 0) {
      return {
        toolCallId,
        content: 'Error: old_str_1 is required for text replacement. Use old_str_1/new_str_1, old_str_2/new_str_2, etc.',
        isError: true
      };
    }

    // 模式 2：文本替换
    return await this.executeStrReplace(toolCallId, filePath, entries);
  }

  /**
   * 从 args 中提取替换条目（对齐 Augment Kde() 函数）
   * 扫描编号参数 old_str_1, old_str_2, ...
   */
  private extractEditEntries(args: Record<string, any>): EditEntry[] {
    const entries: EditEntry[] = [];

    const numberedKeys = Object.keys(args)
      .filter(k => /^old_str_\d+$/.test(k))
      .sort((a, b) => parseInt(a.replace('old_str_', '')) - parseInt(b.replace('old_str_', '')));

    for (const key of numberedKeys) {
      const suffix = key.replace('old_str_', '');
      const oldStr = args[`old_str_${suffix}`];
      const newStr = args[`new_str_${suffix}`] ?? '';

      if (typeof oldStr !== 'string') continue;

      entries.push({
        index: parseInt(suffix),
        oldStr,
        newStr: typeof newStr === 'string' ? newStr : '',
        startLine: args[`old_str_start_line_${suffix}`] as number | undefined,
        endLine: args[`old_str_end_line_${suffix}`] as number | undefined,
      });
    }

    return entries;
  }

  /**
   * 执行多条替换（核心方法）
   * 读文件一次 → 按 startLine 降序逐条替换 → 写文件一次
   */
  private async executeStrReplace(
    toolCallId: string,
    filePath: string,
    entries: EditEntry[]
  ): Promise<ToolResult> {
    // 读文件
    let content: string;
    try {
      content = await fs.readFile(filePath, 'utf-8');
    } catch (error: any) {
      if (error.code === 'ENOENT') {
        const suggestions = await this.findSimilarFiles(filePath);
        let errorMsg = `Error: File not found: ${this.workspaceRoots.toDisplayPath(filePath)}`;
        if (suggestions.length > 0) {
          errorMsg += `\n\nDid you mean one of these?\n${suggestions.map(s => `  - ${s}`).join('\n')}`;
        }
        return { toolCallId, content: errorMsg, isError: true };
      }
      return { toolCallId, content: `Error: ${error.message}`, isError: true };
    }

    // 按 startLine 降序排序（从文件底部开始替换，避免行号偏移）
    const sorted = [...entries].sort((a, b) => {
      const aLine = a.startLine ?? -1;
      const bLine = b.startLine ?? -1;
      return bLine - aLine;
    });

    // 检查条目间行号范围是否重叠
    for (let i = 0; i < sorted.length; i++) {
      const overlap = this.findOverlappingEntry(sorted[i], sorted, i);
      if (overlap) {
        return {
          toolCallId,
          content: `Error: entry #${sorted[i].index} line range [${sorted[i].startLine}-${sorted[i].endLine}] overlaps with entry #${overlap.index} [${overlap.startLine}-${overlap.endLine}].`,
          isError: true
        };
      }
    }

    // 逐条执行替换，累积内容变更
    const originalContent = content; // 保留原始内容用于 diff 计算
    const results: string[] = [];
    let hasError = false;

    for (const entry of sorted) {
      const result = this.matchAndReplace(content, entry);
      if (result.error) {
        results.push(`Entry #${entry.index}: ${result.error}`);
        hasError = true;
        break; // 遇到错误立即停止，不写入部分结果
      }
      content = result.newContent!;

      // 生成包含代码片段的成功消息（对齐 Augment 响应格式）
      let successMsg = result.message || 'OK';
      if (result.newStrStartLine !== undefined && result.newStrEndLine !== undefined) {
        const snippetLines = content.split('\n');
        const snippetStart = Math.max(0, result.newStrStartLine);
        const snippetEnd = Math.min(snippetLines.length, result.newStrEndLine + 1);
        const snippet = snippetLines.slice(snippetStart, snippetEnd);
        // 限制代码片段行数，避免过长响应
        const maxSnippetLines = 20;
        const truncated = snippet.length > maxSnippetLines;
        const displaySnippet = truncated ? snippet.slice(0, maxSnippetLines) : snippet;
        const numberedSnippet = displaySnippet
          .map((line, i) => `${snippetStart + i + 1}\t${line}`)
          .join('\n');

        successMsg += `\nnew_str starts at line ${result.newStrStartLine + 1} and ends at line ${result.newStrEndLine + 1}.`;
        successMsg += `\n\nSnippet of edited section:\n${numberedSnippet}`;
        if (truncated) successMsg += `\n... (${snippet.length - maxSnippetLines} more lines)`;
      }
      results.push(`Entry #${entry.index}: ${successMsg}`);
    }

    if (hasError) {
      return { toolCallId, content: results.join('\n'), isError: true };
    }

    // 保存撤销信息
    this.undoStack.set(filePath, await fs.readFile(filePath, 'utf-8'));

    // 快照回调
    this.onBeforeWrite?.(filePath);

    // 一次写入
    await fs.writeFile(filePath, content, 'utf-8');

    logger.info('File edited (file_edit)', {
      path: filePath,
      entryCount: entries.length,
    }, LogCategory.TOOLS);

    const fileChange = this.buildFileChangeMetadata(originalContent, content, filePath, 'modify');

    // 单条时简化输出
    if (entries.length === 1) {
      return {
        toolCallId,
        content: results[0].replace(/^Entry #\d+: /, ''),
        isError: false,
        fileChange,
      };
    }

    return {
      toolCallId,
      content: `OK: ${entries.length} replacements applied.\n${results.join('\n')}`,
      isError: false,
      fileChange,
    };
  }

  /**
   * 检查条目间行号范围是否重叠
   */
  private findOverlappingEntry(entry: EditEntry, allEntries: EditEntry[], skipIndex: number): EditEntry | null {
    if (entry.startLine === undefined || entry.endLine === undefined) return null;
    for (let i = 0; i < allEntries.length; i++) {
      if (i === skipIndex) continue;
      const other = allEntries[i];
      if (other.startLine === undefined || other.endLine === undefined) continue;
      // 检查是否有交集
      if (entry.startLine <= other.endLine && other.startLine <= entry.endLine) {
        return other;
      }
    }
    return null;
  }

  /**
   * 单条条目的匹配与替换（纯计算，不读写文件）
   * 对齐 Augment singleStrReplace() 完整管线：
   * 换行符规范化 → 空文件处理 → 精确匹配 → 缩进互转 → 空白规范化 → 行号容忍
   */
  private matchAndReplace(
    content: string,
    entry: EditEntry
  ): ReplaceResult {
    // 换行符规范化（对齐 Augment nY()）
    let oldStr = this.normalizeLineEndings(entry.oldStr);
    let newStr = this.normalizeLineEndings(entry.newStr);
    const normalizedContent = this.normalizeLineEndings(content);
    const { startLine, endLine } = entry;

    // old_str 和 new_str 相同
    if (oldStr === newStr) {
      return { error: 'old_str and new_str are identical. No replacement needed.' };
    }

    // 空文件特殊处理（对齐 Augment: old_str 为空仅当文件也为空时允许）
    if (oldStr.trim() === '') {
      if (normalizedContent.trim() === '') {
        const newStrLines = newStr.split('\n');
        return {
          newContent: newStr,
          message: 'OK (empty file replaced)',
          newStrStartLine: 0,
          newStrEndLine: Math.max(0, newStrLines.length - 1)
        };
      }
      return { error: 'old_str is empty, which is only allowed when the file is empty or contains only whitespace.' };
    }

    // 查找所有精确匹配（对齐 Augment XD()）
    let matches = this.findAllMatches(normalizedContent, oldStr);

    // 无精确匹配 → 尝试缩进互转（对齐 Augment tryTabIndentFix()）
    if (matches.length === 0) {
      const indentFix = this.tryTabIndentFix(normalizedContent, oldStr, newStr);
      if (indentFix.matches.length > 0) {
        matches = indentFix.matches;
        oldStr = indentFix.oldStr;
        newStr = indentFix.newStr;
      }
    }

    // 无匹配 → 尝试行尾空白规范化
    if (matches.length === 0) {
      const trimmed = this.tryTrimmedMatch(normalizedContent, oldStr);
      if (trimmed) {
        const trimMatches = this.findAllMatches(normalizedContent, trimmed);
        if (trimMatches.length > 0) {
          matches = trimMatches;
          oldStr = trimmed;
        }
      }
    }

    // 全部匹配策略失败
    if (matches.length === 0) {
      const nearMatches = this.findFirstLineMatches(normalizedContent, oldStr);
      let msg = 'Error: old_str not found in file.';
      if (startLine !== undefined && endLine !== undefined) {
        const rangeLines = normalizedContent.split('\n');
        const effectiveEnd = Math.min(endLine, rangeLines.length);
        const rangeContent = rangeLines.slice(startLine - 1, effectiveEnd).join('\n');
        msg = `Error: old_str not found in lines ${startLine}-${effectiveEnd}.`;
        msg += `\n\nContent in that range:\n${rangeContent.substring(0, 500)}${rangeContent.length > 500 ? '...' : ''}`;
      }
      if (nearMatches.length > 0) {
        msg += `\n\nHint: old_str first line appears near line(s): ${nearMatches.join(', ')}. Use file_view to verify.`;
      } else {
        msg += '\n\nHint: old_str not found anywhere in the file. Use file_view to re-read.';
      }
      return { error: msg };
    }

    // 确定使用哪个匹配
    let matchIdx: number;

    if (matches.length === 1) {
      matchIdx = 0;
    } else {
      // 多匹配：需要行号来消歧（对齐 Augment FLt()）
      if (startLine === undefined || endLine === undefined) {
        const lineNums = matches.map(m => m.startLine + 1);
        return {
          error: `old_str appears multiple times (at lines: ${lineNums.join(', ')}). Use old_str_start_line and old_str_end_line to specify which occurrence.`
        };
      }
      // 1-based → 0-based
      matchIdx = this.findClosestMatch(matches, startLine - 1, endLine - 1);
      if (matchIdx === -1) {
        return { error: `No match found close to the provided line numbers (${startLine}, ${endLine}).` };
      }
    }

    // 执行替换
    const match = matches[matchIdx];
    const contentLines = normalizedContent.split('\n');
    const oldStrLineCount = oldStr.split('\n').length;
    const newStrLines = newStr.split('\n');

    const before = contentLines.slice(0, match.startLine).join('\n');
    const after = contentLines.slice(match.startLine + oldStrLineCount).join('\n');

    let newContent: string;
    if (before && after) newContent = before + '\n' + newStr + '\n' + after;
    else if (before) newContent = before + '\n' + newStr;
    else if (after) newContent = newStr + '\n' + after;
    else newContent = newStr;

    const newStrStartLine = match.startLine;
    const newStrEndLine = match.startLine + newStrLines.length - 1;

    return {
      newContent,
      message: `OK`,
      newStrStartLine,
      newStrEndLine
    };
  }

  /**
   * 从 args 中提取插入条目（对齐 Augment $de() 函数）
   * 扫描编号参数 insert_line_1 + new_str_1, insert_line_2 + new_str_2, ...
   */
  private extractInsertEntries(args: Record<string, any>): InsertEntry[] {
    const entries: InsertEntry[] = [];

    const insertLineKeys = Object.keys(args)
      .filter(k => k.startsWith('insert_line_') && /^insert_line_\d+$/.test(k));
    insertLineKeys.sort((a, b) =>
      parseInt(a.replace('insert_line_', '')) - parseInt(b.replace('insert_line_', ''))
    );

    for (const key of insertLineKeys) {
      const suffix = key.replace('insert_line_', '');
      if (`new_str_${suffix}` in args) {
        entries.push({
          index: parseInt(suffix),
          insertLine: args[`insert_line_${suffix}`],
          newStr: args[`new_str_${suffix}`]
        });
      }
    }

    return entries;
  }

  /**
   * 验证插入条目（对齐 Augment HD() 函数）
   */
  private validateInsertEntries(entries: InsertEntry[]): string | null {
    if (entries.length === 0) {
      return 'Missing required parameters: insert_line_1 and new_str_1';
    }
    for (const entry of entries) {
      if (!Number.isInteger(entry.insertLine) || entry.insertLine < 0) {
        return `Invalid parameter insert_line (index ${entry.index}): must be a non-negative integer, got ${entry.insertLine}`;
      }
      if (typeof entry.newStr !== 'string') {
        return `Invalid parameter new_str (index ${entry.index}): must be a string`;
      }
    }
    return null;
  }

  /**
   * 插入文本（对齐 Augment handleInsert）
   * 支持多条插入，底部优先处理，单次读写
   */
  private async executeInsert(
    toolCallId: string,
    filePath: string,
    args: Record<string, any>
  ): Promise<ToolResult> {
    // 1. 提取并验证插入条目
    const entries = this.extractInsertEntries(args);
    const validationError = this.validateInsertEntries(entries);
    if (validationError) {
      return { toolCallId, content: `Error: ${validationError}`, isError: true };
    }

    // 2. 读取文件内容
    let content = '';
    try {
      content = await fs.readFile(filePath, 'utf-8');
    } catch (error: any) {
      if (error?.code !== 'ENOENT') {
        return { toolCallId, content: `Error: ${error.message}`, isError: true };
      }
      // 文件不存在，从空内容开始
    }

    // 3. 规范化换行符
    const normalized = this.normalizeLineEndings(content);
    let currentContent = normalized;

    // 4. 按 insertLine 降序排序（底部优先，避免行号偏移）
    const sorted = [...entries].sort((a, b) => b.insertLine - a.insertLine);

    // 5. 逐条处理插入
    const results: Array<{ index: number; isError: boolean; message: string }> = [];
    for (const entry of sorted) {
      const lines = currentContent.split('\n');
      const insertNewStr = this.normalizeLineEndings(entry.newStr);

      if (entry.insertLine < 0 || entry.insertLine > lines.length) {
        results.push({
          index: entry.index,
          isError: true,
          message: `Invalid insert_line: ${entry.insertLine}. Must be in range [0, ${lines.length}]`
        });
        continue;
      }

      const insertedLines = insertNewStr.split('\n');
      currentContent = [
        ...lines.slice(0, entry.insertLine),
        ...insertedLines,
        ...lines.slice(entry.insertLine)
      ].join('\n');

      results.push({
        index: entry.index,
        isError: false,
        message: `Inserted at line ${entry.insertLine}, ${insertedLines.length} line(s) added`
      });
    }

    // 6. 检查是否有任何错误条目
    const errors = results.filter(r => r.isError);
    if (errors.length === entries.length) {
      // 全部失败，不写入
      return {
        toolCallId,
        content: errors.map(e => `Error (index ${e.index}): ${e.message}`).join('\n'),
        isError: true
      };
    }

    // 7. 保存撤销信息
    this.undoStack.set(filePath, content);

    // 8. 快照回调
    this.onBeforeWrite?.(filePath);

    // 9. 写入文件
    await fs.mkdir(path.dirname(filePath), { recursive: true });
    await fs.writeFile(filePath, currentContent, 'utf-8');

    logger.info('File edited (file_insert)', {
      path: filePath,
      entries: entries.length,
      errors: errors.length
    }, LogCategory.TOOLS);

    // 10. 构建响应（包含代码片段）
    const finalLines = currentContent.split('\n');
    const successResults = results.filter(r => !r.isError);
    let responseMsg = successResults.map(r => r.message).join('\n');

    if (errors.length > 0) {
      responseMsg += '\n' + errors.map(e => `Error (index ${e.index}): ${e.message}`).join('\n');
    }

    // 生成代码片段（按原始顺序，最多展示 20 行）
    const snippetLines: string[] = [];
    const originalOrder = [...results].sort((a, b) => a.index - b.index);
    for (const r of originalOrder) {
      if (!r.isError) {
        snippetLines.push(`  ${r.message}`);
      }
    }
    if (snippetLines.length > 0 && finalLines.length <= 200) {
      const maxSnippetLines = Math.min(finalLines.length, 20);
      const snippet = finalLines.slice(0, maxSnippetLines)
        .map((line: string, i: number) => `${String(i + 1).padStart(4)}\t${line}`)
        .join('\n');
      responseMsg += `\n\nFile preview (first ${maxSnippetLines} lines):\n${snippet}`;
      if (finalLines.length > maxSnippetLines) {
        responseMsg += `\n... (${finalLines.length - maxSnippetLines} more lines)`;
      }
    }

    return {
      toolCallId,
      content: responseMsg,
      isError: false,
      fileChange: this.buildFileChangeMetadata(normalized, currentContent, filePath, 'modify'),
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
   * 行尾空白规范化匹配
   * 将 content 和 oldStr 逐行 trimEnd 后匹配，返回 content 中对应的原始文本
   */
  private tryTrimmedMatch(content: string, oldStr: string): string | null {
    const contentLines = content.split('\n');
    const oldStrLines = oldStr.split('\n');
    const trimmedOldLines = oldStrLines.map(l => l.trimEnd());
    const trimmedOld = trimmedOldLines.join('\n');

    // 逐行 trimEnd 后的内容
    const trimmedContentLines = contentLines.map(l => l.trimEnd());
    const trimmedContent = trimmedContentLines.join('\n');

    const idx = trimmedContent.indexOf(trimmedOld);
    if (idx === -1) return null;

    // 确保唯一匹配
    if (trimmedContent.indexOf(trimmedOld, idx + 1) !== -1) return null;

    // 映射回原始行：找到匹配起始行号
    const matchStartLine = trimmedContent.substring(0, idx).split('\n').length - 1;
    const matchEndLine = matchStartLine + oldStrLines.length;

    // 从原始内容中提取对应行
    return contentLines.slice(matchStartLine, matchEndLine).join('\n');
  }

  /**
   * 查找 oldStr 首行在文件中的近似匹配位置
   */
  private findFirstLineMatches(content: string, oldStr: string): number[] {
    const firstLine = oldStr.split('\n')[0].trim();
    if (firstLine.length < 6) return [];  // 太短的行没有参考价值

    const lines = content.split('\n');
    const matches: number[] = [];
    for (let i = 0; i < lines.length; i++) {
      if (lines[i].trim().includes(firstLine)) {
        matches.push(i + 1);  // 1-based
      }
    }
    return matches.slice(0, 5);  // 最多返回5个
  }

  /**
   * 解析工作区相对路径
   */
  private resolveWorkspacePath(inputPath: string, mustExist: boolean): { absolutePath: string | null; error?: string } {
    try {
      const resolved = this.workspaceRoots.resolvePath(inputPath, { mustExist });
      return { absolutePath: resolved?.absolutePath || null };
    } catch (error: any) {
      return { absolutePath: null, error: `Error: ${error.message}` };
    }
  }

  /**
   * 换行符规范化（对齐 Augment nY()）
   * 将 \r\n 统一转换为 \n
   */
  private normalizeLineEndings(str: string): string {
    return str.replace(/\r\n/g, '\n');
  }

  /**
   * 查找所有精确匹配位置（对齐 Augment XD()）
   * 返回每个匹配的 0-based 起止行号
   */
  private findAllMatches(content: string, search: string): MatchLocation[] {
    const contentLines = content.split('\n');
    const searchLines = search.split('\n');
    const matches: MatchLocation[] = [];

    if (search.trim() === '' || searchLines.length > contentLines.length) return matches;

    // 单行搜索：逐行 includes
    if (searchLines.length === 1) {
      contentLines.forEach((line, idx) => {
        if (line.includes(search)) matches.push({ startLine: idx, endLine: idx });
      });
      return matches;
    }

    // 多行搜索：indexOf 定位 + 行号计算
    let pos = 0;
    let idx: number;
    while ((idx = content.indexOf(search, pos)) !== -1) {
      const before = content.substring(0, idx);
      const through = content.substring(0, idx + search.length);
      const startLine = (before.match(/\n/g) || []).length;
      const endLine = (through.match(/\n/g) || []).length;
      matches.push({ startLine, endLine });
      pos = idx + 1;
    }
    return matches;
  }

  /**
   * 检测缩进类型（对齐 Augment iY()）
   */
  private detectIndentation(str: string): IndentInfo {
    const lines = str.split('\n');
    let spaceCount = 0, tabCount = 0, firstSpaceSize = 0;
    for (const line of lines) {
      if (line.trim() === '') continue;
      const spaceMatch = line.match(/^( +)/);
      const tabMatch = line.match(/^(\t+)/);
      if (spaceMatch) {
        spaceCount++;
        if (firstSpaceSize === 0) firstSpaceSize = spaceMatch[1].length;
      } else if (tabMatch) {
        tabCount++;
      }
    }
    return tabCount > spaceCount
      ? { type: 'tab', size: 1 }
      : { type: 'space', size: firstSpaceSize || 2 };
  }

  /**
   * Tab/Space 缩进自动互转匹配（对齐 Augment tryTabIndentFix()）
   * 当文件用 tab 而 old_str 也用 tab 时，尝试去掉一层缩进后匹配
   */
  private tryTabIndentFix(
    content: string,
    oldStr: string,
    newStr: string
  ): { matches: MatchLocation[]; oldStr: string; newStr: string } {
    const contentIndent = this.detectIndentation(content);
    const oldStrIndent = this.detectIndentation(oldStr);
    const newStrIndent = this.detectIndentation(newStr);

    if (
      contentIndent.type === 'tab' &&
      oldStrIndent.type === 'tab' &&
      (newStrIndent.type === 'tab' || newStr.trim() === '')
    ) {
      // 检查是否符合缩进模式（对齐 Augment dUe()）
      const followsPattern = (s: string, indent: IndentInfo): boolean =>
        s.split('\n').every(line => {
          if (line.trim() === '') return true;
          const re = indent.type === 'tab' ? /^\t/ : new RegExp(`^ {1,${indent.size}}`);
          return re.test(line);
        });

      if (followsPattern(oldStr, contentIndent) && followsPattern(newStr, contentIndent)) {
        // 转换缩进（对齐 Augment uUe()）
        const convert = (s: string, indent: IndentInfo): string => {
          const re = indent.type === 'tab' ? /^\t/ : new RegExp(`^ {1,${indent.size}}`);
          return s.split('\n').map(line => line.replace(re, '')).join('\n');
        };
        const convertedOld = convert(oldStr, contentIndent);
        const convertedNew = convert(newStr, contentIndent);
        const matches = this.findAllMatches(content, convertedOld);
        if (matches.length > 0) {
          return { matches, oldStr: convertedOld, newStr: convertedNew };
        }
      }
    }

    return { matches: [], oldStr, newStr };
  }

  /**
   * 行号容忍匹配（对齐 Augment FLt()）
   * 在多个匹配中找到最接近目标行号的那个，允许 20% 误差
   */
  private findClosestMatch(
    matches: MatchLocation[],
    targetStartLine: number,
    targetEndLine: number
  ): number {
    if (matches.length === 0) return -1;
    if (matches.length === 1) return 0;

    // 精确匹配优先
    for (let i = 0; i < matches.length; i++) {
      if (matches[i].startLine === targetStartLine && matches[i].endLine === targetEndLine) {
        return i;
      }
    }

    // 找最近的匹配
    let closestIdx = -1;
    let closestDist = Number.MAX_SAFE_INTEGER;
    for (let i = 0; i < matches.length; i++) {
      const dist = Math.abs(matches[i].startLine - targetStartLine);
      if (dist < closestDist) {
        closestDist = dist;
        closestIdx = i;
      }
    }

    if (closestIdx === -1) return -1;

    // 找第二近的，计算容忍阈值
    let secondDist = Number.MAX_SAFE_INTEGER;
    let secondIdx = -1;
    for (let i = 0; i < matches.length; i++) {
      if (i === closestIdx) continue;
      const dist = Math.abs(matches[i].startLine - targetStartLine);
      if (dist < secondDist) {
        secondDist = dist;
        secondIdx = i;
      }
    }

    const gap = Math.abs(matches[secondIdx].startLine - matches[closestIdx].startLine);
    const threshold = Math.floor(gap / 2 * LINE_NUMBER_ERROR_TOLERANCE);
    return closestDist <= threshold ? closestIdx : -1;
  }

  /**
   * 生成 unified diff 格式文本，用于前端 FileChangeCard 差异化渲染
   * 对比原始内容和新内容，输出带上下文行的 unified diff
   */
  private generateUnifiedDiff(originalContent: string, newContent: string, filePath: string): { diff: string; additions: number; deletions: number } {
    const oldLines = originalContent.split('\n');
    const newLines = newContent.split('\n');

    // 逐行 LCS diff
    const diffOps = this.computeLineDiff(oldLines, newLines);

    // 将 diff 操作转为带上下文的 hunks
    const contextLines = 3;
    const hunks = this.buildHunks(diffOps, contextLines);

    if (hunks.length === 0) {
      return { diff: '', additions: 0, deletions: 0 };
    }

    let additions = 0;
    let deletions = 0;
    const lines: string[] = [
      `--- a/${this.workspaceRoots.toDisplayPath(filePath)}`,
      `+++ b/${this.workspaceRoots.toDisplayPath(filePath)}`,
    ];

    for (const hunk of hunks) {
      lines.push(`@@ -${hunk.oldStart},${hunk.oldCount} +${hunk.newStart},${hunk.newCount} @@`);
      for (const op of hunk.ops) {
        if (op.type === 'equal') {
          lines.push(` ${op.line}`);
        } else if (op.type === 'delete') {
          lines.push(`-${op.line}`);
          deletions++;
        } else if (op.type === 'insert') {
          lines.push(`+${op.line}`);
          additions++;
        }
      }
    }

    return { diff: lines.join('\n'), additions, deletions };
  }

  /**
   * 逐行 diff（简单但有效的 O(n*m) 贪心算法）
   */
  private computeLineDiff(
    oldLines: string[],
    newLines: string[]
  ): Array<{ type: 'equal' | 'delete' | 'insert'; line: string }> {
    const result: Array<{ type: 'equal' | 'delete' | 'insert'; line: string }> = [];
    let i = 0, j = 0;

    while (i < oldLines.length || j < newLines.length) {
      if (i >= oldLines.length) {
        result.push({ type: 'insert', line: newLines[j] });
        j++;
      } else if (j >= newLines.length) {
        result.push({ type: 'delete', line: oldLines[i] });
        i++;
      } else if (oldLines[i] === newLines[j]) {
        result.push({ type: 'equal', line: oldLines[i] });
        i++;
        j++;
      } else {
        const oldMatch = newLines.indexOf(oldLines[i], j);
        const newMatch = oldLines.indexOf(newLines[j], i);

        if (oldMatch === -1 && newMatch === -1) {
          result.push({ type: 'delete', line: oldLines[i] });
          result.push({ type: 'insert', line: newLines[j] });
          i++;
          j++;
        } else if (oldMatch !== -1 && (newMatch === -1 || oldMatch - j <= newMatch - i)) {
          while (j < oldMatch) {
            result.push({ type: 'insert', line: newLines[j] });
            j++;
          }
        } else {
          while (i < newMatch) {
            result.push({ type: 'delete', line: oldLines[i] });
            i++;
          }
        }
      }
    }

    return result;
  }

  /**
   * 将 diff 操作序列转为带上下文的 hunks
   */
  private buildHunks(
    ops: Array<{ type: 'equal' | 'delete' | 'insert'; line: string }>,
    contextSize: number
  ): Array<{ oldStart: number; oldCount: number; newStart: number; newCount: number; ops: Array<{ type: 'equal' | 'delete' | 'insert'; line: string }> }> {
    // 找出所有变更操作的索引
    const changeIndices: number[] = [];
    for (let i = 0; i < ops.length; i++) {
      if (ops[i].type !== 'equal') {
        changeIndices.push(i);
      }
    }
    if (changeIndices.length === 0) return [];

    // 将连续变更（含上下文）合并为 hunk
    const hunks: Array<{ startIdx: number; endIdx: number }> = [];
    let hunkStart = Math.max(0, changeIndices[0] - contextSize);
    let hunkEnd = Math.min(ops.length - 1, changeIndices[0] + contextSize);

    for (let k = 1; k < changeIndices.length; k++) {
      const newStart = Math.max(0, changeIndices[k] - contextSize);
      const newEnd = Math.min(ops.length - 1, changeIndices[k] + contextSize);
      if (newStart <= hunkEnd + 1) {
        hunkEnd = newEnd;
      } else {
        hunks.push({ startIdx: hunkStart, endIdx: hunkEnd });
        hunkStart = newStart;
        hunkEnd = newEnd;
      }
    }
    hunks.push({ startIdx: hunkStart, endIdx: hunkEnd });

    // 构建带行号的 hunk
    return hunks.map(h => {
      const hunkOps = ops.slice(h.startIdx, h.endIdx + 1);
      // 计算 hunk 起始行号
      let oldLine = 1, newLine = 1;
      for (let i = 0; i < h.startIdx; i++) {
        if (ops[i].type === 'equal' || ops[i].type === 'delete') oldLine++;
        if (ops[i].type === 'equal' || ops[i].type === 'insert') newLine++;
      }
      let oldCount = 0, newCount = 0;
      for (const op of hunkOps) {
        if (op.type === 'equal' || op.type === 'delete') oldCount++;
        if (op.type === 'equal' || op.type === 'insert') newCount++;
      }
      return { oldStart: oldLine, oldCount, newStart: newLine, newCount, ops: hunkOps };
    });
  }

  /**
   * 构建 FileChangeMetadata（供 ToolResult.fileChange 使用）
   */
  private buildFileChangeMetadata(
    originalContent: string,
    newContent: string,
    filePath: string,
    changeType: 'create' | 'modify' | 'delete'
  ): FileChangeMetadata {
    const { diff, additions, deletions } = this.generateUnifiedDiff(originalContent, newContent, filePath);
    return {
      filePath: this.workspaceRoots.toDisplayPath(filePath),
      changeType,
      additions,
      deletions,
      diff,
    };
  }
}
