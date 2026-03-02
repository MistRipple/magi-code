/**
 * 文件执行器
 * 提供文件查看、创建、编辑、插入功能
 *
 * 读写层：优先使用 VSCode Document API，保证与编辑器状态同步
 * 编辑层：基于大模型意图的全量重写 (LLM as Editor)，结合 FileMutex 保证高并发场景下的读写安全
 *
 * 工具:
 * - file_view: 查看文件内容或目录结构
 * - file_create: 创建或写入完整文件内容
 * - file_edit: 基于意图的文件修改（LLM 委托） / 撤销
 * - file_insert: 在指定行插入文本
 */

import * as vscode from 'vscode';
import * as fs from 'fs/promises';
import * as path from 'path';
import { ToolExecutor, ExtendedToolDefinition } from './types';
import { ToolCall, ToolResult, FileChangeMetadata } from '../llm/types';
import { logger, LogCategory } from '../logging';
import { WorkspaceRoots } from '../workspace/workspace-roots';
import { FileMutex } from '../utils/file-mutex';



/** 写入后等待最终内容稳定的最大轮次 */
const POST_WRITE_SETTLE_MAX_ATTEMPTS = 6;

/** 写入后每轮重读间隔（ms） */
const POST_WRITE_SETTLE_DELAY_MS = 120;

/** fileChange.diff 最大行数（防止超大差异导致消息/渲染抖动） */
const MAX_DIFF_LINES = 1200;



/** 单条插入条目（对齐 Augment InsertEntry） */
interface InsertEntry {
  index: number;
  insertLine: number;
  newStr: string;
}

/**
 * 文件执行器
 */
export class FileExecutor implements ToolExecutor {
  private workspaceRoots: WorkspaceRoots;
  private undoStack: Map<string, string> = new Map();
  private fileMutex: FileMutex;
  private llmEditHandler?: (filePath: string, fileContent: string, summary: string, detailedDesc: string) => Promise<string>;

  /** 文件写入前回调（用于快照系统在写入前保存原始内容） */
  private onBeforeWrite?: (filePath: string) => void;

  constructor(workspaceRoots: WorkspaceRoots, fileMutex: FileMutex) {
    this.workspaceRoots = workspaceRoots;
    this.fileMutex = fileMutex;
  }

  /**
   * 原地更新工作区根目录（避免重建实例导致 handler/回调丢失）
   * 仅清除与旧路径绑定的撤销历史
   */
  updateWorkspaceRoots(workspaceRoots: WorkspaceRoots): void {
    this.workspaceRoots = workspaceRoots;
    this.undoStack.clear();
  }

  /**
   * 注册大模型文件编辑回调
   */
  setLlmEditHandler(handler: (filePath: string, fileContent: string, summary: string, detailedDesc: string) => Promise<string>) {
    this.llmEditHandler = handler;
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
* Read a file before editing it; if the same file has already been fully read in current context, do not repeatedly read it again`,
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
      description: `Edit a file by providing an intent. Uses a specialized LLM agent to accurately apply the changes.

Notes for text replacement:
* Use file_view to read the file before editing. If the same file is already fresh in current context, do not repeat file_view.
* Provide a brief edit_summary and a detailed_edit_description.
* detailed_edit_description should contain the exact intention, the logic to change, and if necessary, search/replace snippets to help the agent locate the exact place.
* Set undo to true to revert the last edit.

IMPORTANT:
* For creating new files or full rewrites, use file_create instead
* DO NOT use sed/awk/shell commands for editing`,
      input_schema: {
        type: 'object',
        properties: {
          path: {
            type: 'string',
            description: 'File path relative to workspace root'
          },
          edit_summary: {
            type: 'string',
            description: 'A brief description of the edit to be made. 1-2 sentences.'
          },
          detailed_edit_description: {
            type: 'string',
            description: 'A detailed and precise description of the edit. Can include natural language, context, and code snippets.'
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
* Use file_view before inserting into an existing file. If this is a new file path, file_insert can create it directly.
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

      // 读取文件内容（通过 VSCode Document API 获取编辑器最新状态）
      const content = await this.readFileContent(filePath);
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

      // 应用行范围（对越界范围做显式归一化，避免返回空结果导致后续行号漂移）
      let startLine = 1;
      let endLine = lines.length;

      if (viewRange && viewRange.length === 2) {
        const requestedStart = viewRange[0];
        const requestedEnd = viewRange[1];
        const totalLines = lines.length;

        startLine = Math.max(1, Math.min(totalLines, requestedStart));
        endLine = requestedEnd === -1
          ? totalLines
          : Math.max(1, Math.min(totalLines, requestedEnd));

        if (startLine > endLine) {
          return {
            toolCallId,
            content: `Error: Invalid view_range [${requestedStart}, ${requestedEnd}]. File has ${totalLines} lines.`,
            isError: true
          };
        }
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
        const requestedStart = viewRange[0];
        const requestedEnd = viewRange[1];
        const maxIndex = lines.length - 1;
        searchStart = Math.max(0, Math.min(maxIndex, requestedStart - 1));
        searchEnd = requestedEnd === -1
          ? maxIndex
          : Math.max(0, Math.min(maxIndex, requestedEnd - 1));

        if (searchStart > searchEnd) {
          return {
            toolCallId,
            content: `Error: Invalid view_range [${requestedStart}, ${requestedEnd}] for regex search. File has ${lines.length} lines.`,
            isError: true
          };
        }
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
   * 创建/写入文件内容
   * 通过 WorkspaceEdit 创建，进入 VSCode 原生撤销栈
   */
  private async executeCreate(
    toolCallId: string,
    filePath: string,
    args: Record<string, any>
  ): Promise<ToolResult> {
    const fileContent: string = args.file_content ?? '';
    const addLastLineNewline: boolean = args.add_last_line_newline ?? true;
    const finalContent = fileContent + (addLastLineNewline ? '\n' : '');

    return await this.fileMutex.runExclusive(filePath, async () => {
      // 读取原始内容（覆写场景用于 diff）
      let originalContent = '';
      try {
        originalContent = await this.readFileContent(filePath);
      } catch { /* 新建文件，原始内容为空 */ }

      // 快照回调（覆写时保护原始内容）
      this.onBeforeWrite?.(filePath);

      // 通过 WorkspaceEdit 创建/覆写文件
      await this.createFileViaWorkspaceEdit(filePath, finalContent);
      const settledContent = await this.readSettledFileContent(filePath, finalContent);

      logger.info('File created via file_create', { path: filePath }, LogCategory.TOOLS);

      const changeType = originalContent ? 'modify' as const : 'create' as const;
      return {
        toolCallId,
        content: `OK: file created at ${this.workspaceRoots.toDisplayPath(filePath)}`,
        isError: false,
        fileChange: this.buildFileChangeMetadata(originalContent, settledContent, filePath, changeType),
      };
    });
  }

  /**
   * 编辑文件（意图驱动 + LLM as Editor）
   */
  private async executeEdit(
    toolCallId: string,
    filePath: string,
    args: Record<string, any>
  ): Promise<ToolResult> {
    // 模式 1：撤销（同样需要加锁，防止与同文件的并发写操作冲突）
    if (args.undo === true) {
      return await this.fileMutex.runExclusive(filePath, () => this.executeUndo(toolCallId, filePath));
    }

    const editSummary = args.edit_summary as string | undefined;
    const detailedEditDesc = args.detailed_edit_description as string | undefined;

    if (!editSummary || !detailedEditDesc) {
      return {
        toolCallId,
        content: 'Error: edit_summary and detailed_edit_description are required for file editing.',
        isError: true
      };
    }

    if (!this.llmEditHandler) {
      return {
        toolCallId,
        content: 'Error: LLM edit handler is not registered. Cannot perform intent-driven file edit.',
        isError: true
      };
    }

    return await this.fileMutex.runExclusive(filePath, async () => {
      let originalContent = '';
      try {
        originalContent = await this.readFileContent(filePath);
      } catch (error: any) {
        return {
          toolCallId,
          content: `Error: Cannot read file ${this.workspaceRoots.toDisplayPath(filePath)} for editing: ${error.message}`,
          isError: true
        };
      }

      logger.info('Delegating file edit to LLM handler', { path: filePath, summary: editSummary }, LogCategory.TOOLS);

      let newContent: string;
      try {
        newContent = await this.llmEditHandler!(filePath, originalContent, editSummary, detailedEditDesc);
      } catch (error: any) {
        return {
          toolCallId,
          content: `Error during LLM edit generation: ${error.message}`,
          isError: true
        };
      }

      if (newContent === originalContent) {
        return {
          toolCallId,
          content: `No changes were made to ${this.workspaceRoots.toDisplayPath(filePath)}. The new content is identical to the original.`,
          isError: false
        };
      }

      // 保存旧版本用于撤销
      this.undoStack.set(filePath, originalContent);

      // 写前快照回调
      this.onBeforeWrite?.(filePath);

      // 写入文件
      await this.createFileViaWorkspaceEdit(filePath, newContent);
      const settledContent = await this.readSettledFileContent(filePath, newContent);

      return {
        toolCallId,
        content: `Successfully edited ${this.workspaceRoots.toDisplayPath(filePath)} based on intent.\nSummary: ${editSummary}`,
        isError: false,
        fileChange: this.buildFileChangeMetadata(originalContent, settledContent, filePath, 'modify'),
      };
    });
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
   * 插入文本
   * 支持多条插入，底部优先处理，通过 WorkspaceEdit 一次写入
   */
  private async executeInsert(
    toolCallId: string,
    filePath: string,
    args: Record<string, any>
  ): Promise<ToolResult> {
    // 1. 提取并验证插入条目（纯参数校验，不涉及文件读写，无需加锁）
    const entries = this.extractInsertEntries(args);
    const validationError = this.validateInsertEntries(entries);
    if (validationError) {
      return { toolCallId, content: `Error: ${validationError}`, isError: true };
    }

    return await this.fileMutex.runExclusive(filePath, async () => {
      // 2. 通过 VSCode Document API 读取文件内容
      let content = '';
      let isNewFile = false;
      try {
        content = await this.readFileContent(filePath);
      } catch {
        // 文件不存在，从空内容开始（file_insert 允许自动创建）
        isNewFile = true;
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
            message: `Invalid insert_line: ${entry.insertLine}. File currently has ${lines.length} lines, valid range is [0, ${lines.length}]`
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

      // 9. 通过 WorkspaceEdit 写入
      if (isNewFile) {
        await this.createFileViaWorkspaceEdit(filePath, currentContent);
      } else {
        await this.writeFileViaWorkspaceEdit(filePath, currentContent);
      }
      const finalContent = await this.readSettledFileContent(filePath, currentContent);

      logger.info('File edited (file_insert)', {
        path: filePath,
        entries: entries.length,
        errors: errors.length,
        changedAfterWrite: finalContent !== currentContent,
      }, LogCategory.TOOLS);

      // 10. 构建响应（包含代码片段）
      const finalLines = finalContent.split('\n');
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
        fileChange: this.buildFileChangeMetadata(normalized, finalContent, filePath, 'modify'),
      };
    });
  }

  /**
   * 撤销编辑
   * 通过 WorkspaceEdit 恢复到上一个状态
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

    await this.writeFileViaWorkspaceEdit(filePath, previous);
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
  private resolveWorkspacePath(inputPath: string, mustExist: boolean): { absolutePath: string | null; error?: string } {
    try {
      const resolved = this.workspaceRoots.resolvePath(inputPath, { mustExist });
      return { absolutePath: resolved?.absolutePath || null };
    } catch (error: any) {
      return { absolutePath: null, error: `Error: ${error.message}` };
    }
  }


  /**
   * 构建 FileChangeMetadata（供 ToolResult.fileChange 使用）
   * 使用统一 diff 生成真实 additions/deletions 与 diff 文本
   */
  private buildFileChangeMetadata(
    originalContent: string,
    newContent: string,
    filePath: string,
    changeType: 'create' | 'modify' | 'delete'
  ): FileChangeMetadata {
    const displayPath = this.workspaceRoots.toDisplayPath(filePath);
    const { additions, deletions, diff } = this.generateUnifiedDiff(originalContent, newContent, displayPath);

    return {
      filePath: displayPath,
      changeType,
      additions,
      deletions,
      diff,
    };
  }

  /**
   * 生成统一 diff，并统计增删行数
   * 注意：仅统计 hunk 行，避免将 diff 头部（---/+++）计入变更。
   */
  private generateUnifiedDiff(
    originalContent: string,
    newContent: string,
    displayPath: string
  ): { additions: number; deletions: number; diff: string } {
    if (originalContent === newContent) {
      return { additions: 0, deletions: 0, diff: '' };
    }

    const diffLib = require('diff') as {
      structuredPatch: (
        oldFileName: string,
        newFileName: string,
        oldStr: string,
        newStr: string,
        oldHeader?: string,
        newHeader?: string,
        options?: { context?: number }
      ) => {
        hunks?: Array<{
          oldStart: number;
          oldLines: number;
          newStart: number;
          newLines: number;
          lines: string[];
        }>;
      };
    };

    const patch = diffLib.structuredPatch(
      displayPath,
      displayPath,
      originalContent,
      newContent,
      '',
      '',
      { context: 3 }
    );
    const hunks = Array.isArray(patch.hunks) ? patch.hunks : [];

    let additions = 0;
    let deletions = 0;
    const diffLines: string[] = [`--- ${displayPath}`, `+++ ${displayPath}`];

    for (const hunk of hunks) {
      diffLines.push(`@@ -${hunk.oldStart},${hunk.oldLines} +${hunk.newStart},${hunk.newLines} @@`);
      for (const line of hunk.lines) {
        diffLines.push(line);
        if (line.startsWith('+')) additions += 1;
        if (line.startsWith('-')) deletions += 1;
      }
    }

    if (hunks.length === 0) {
      return { additions: 0, deletions: 0, diff: '' };
    }

    const maxDiffLines = MAX_DIFF_LINES;
    if (diffLines.length <= maxDiffLines) {
      return { additions, deletions, diff: diffLines.join('\n') };
    }

    const truncated = [
      ...diffLines.slice(0, maxDiffLines),
      `... (diff truncated: ${diffLines.length - maxDiffLines} more lines)`,
    ];

    logger.warn('fileChange diff truncated due to size limit', {
      filePath: displayPath,
      totalDiffLines: diffLines.length,
      maxDiffLines,
      additions,
      deletions,
    }, LogCategory.TOOLS);

    return { additions, deletions, diff: truncated.join('\n') };
  }

  /**
   * 读取文件内容（优先从 VSCode 已打开文档获取）
   * 保证拿到编辑器中可能未保存的最新状态
   */
  private async readFileContent(filePath: string): Promise<string> {
    const uri = vscode.Uri.file(filePath);

    // 优先从已打开的文档中读取（可能包含未保存的修改）
    const openDoc = vscode.workspace.textDocuments.find(
      doc => doc.uri.fsPath === uri.fsPath
    );
    if (openDoc) {
      return openDoc.getText();
    }

    // 文档未在编辑器中打开，通过 openTextDocument 读取磁盘内容
    const doc = await vscode.workspace.openTextDocument(uri);
    return doc.getText();
  }

  /**
   * 通过 WorkspaceEdit 写入文件内容（替换整个文档）
   * 修改进入 VSCode 原生撤销栈，解决"脏文件"状态不同步问题
   */
  private async writeFileViaWorkspaceEdit(filePath: string, newContent: string): Promise<void> {
    const uri = vscode.Uri.file(filePath);
    const doc = await vscode.workspace.openTextDocument(uri);

    const edit = new vscode.WorkspaceEdit();
    const fullRange = new vscode.Range(
      doc.lineAt(0).range.start,
      doc.lineAt(doc.lineCount - 1).range.end
    );
    edit.replace(uri, fullRange, newContent);

    const success = await vscode.workspace.applyEdit(edit);
    if (!success) {
      throw new Error('VSCode WorkspaceEdit 应用失败');
    }

    // 持久化到磁盘
    await doc.save();
  }

  /**
   * 通过 WorkspaceEdit 创建新文件
   * 如果文件已存在则覆写（与原始 file_create 行为一致）
   */
  private async createFileViaWorkspaceEdit(filePath: string, content: string): Promise<void> {
    const uri = vscode.Uri.file(filePath);

    // 确保目录存在（WorkspaceEdit.createFile 不会自动创建父目录）
    await fs.mkdir(path.dirname(filePath), { recursive: true });

    const edit = new vscode.WorkspaceEdit();

    // 检查文件是否已存在
    let fileExists = false;
    try {
      await fs.access(filePath);
      fileExists = true;
    } catch {
      // 文件不存在
    }

    if (fileExists) {
      // 已有文件：打开文档并替换全部内容
      const doc = await vscode.workspace.openTextDocument(uri);
      const fullRange = new vscode.Range(
        doc.lineAt(0).range.start,
        doc.lineAt(doc.lineCount - 1).range.end
      );
      edit.replace(uri, fullRange, content);
    } else {
      // 新文件：先创建再插入内容
      edit.createFile(uri, { overwrite: false, ignoreIfExists: false });
      edit.insert(uri, new vscode.Position(0, 0), content);
    }

    const success = await vscode.workspace.applyEdit(edit);
    if (!success) {
      throw new Error('VSCode WorkspaceEdit 创建文件失败');
    }

    // 持久化到磁盘
    const doc = await vscode.workspace.openTextDocument(uri);
    await doc.save();
  }

  /**
   * 换行符规范化（对齐 Augment nY()）
   * 将 \r\n 统一转换为 \n
   */
  private normalizeLineEndings(str: string): string {
    return str.replace(/\r\n/g, '\n');
  }

  /**
   * 写入后等待文件内容稳定，获取最终状态（覆盖 format-on-save 异步改写）
   */
  private async readSettledFileContent(filePath: string, writtenContent: string): Promise<string> {
    let lastContent = writtenContent;
    let stableRounds = 0;

    for (let i = 0; i < POST_WRITE_SETTLE_MAX_ATTEMPTS; i++) {
      await this.delay(POST_WRITE_SETTLE_DELAY_MS);
      let current: string;
      try {
        current = await this.readFileContent(filePath);
      } catch {
        return lastContent;
      }
      if (current === lastContent) {
        stableRounds += 1;
        if (stableRounds >= 2) {
          return current;
        }
      } else {
        lastContent = current;
        stableRounds = 0;
      }
    }

    return lastContent;
  }

  private async delay(ms: number): Promise<void> {
    await new Promise<void>(resolve => setTimeout(resolve, ms));
  }
}
