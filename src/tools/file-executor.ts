/**
 * 文件执行器
 * 提供文件查看、创建、编辑、插入、批量编辑功能
 *
 * 读写层：优先使用 VSCode Document API，保证与编辑器状态同步
 * 匹配层：精确匹配 → 缩进转换 → 空白规范化 → 模糊匹配 → 探针回退
 *
 * 工具:
 * - file_view: 查看文件内容或目录结构
 * - file_create: 创建或写入完整文件内容
 * - file_edit: 文本替换（精确 + 模糊容错） / 撤销
 * - file_insert: 在指定行插入文本
 * - file_bulk_edit: 批量跨文件替换（读取 JSON patch 文件，逐文件复用 executeStrReplace）
 */

import * as vscode from 'vscode';
import * as fs from 'fs/promises';
import * as path from 'path';
import { ToolExecutor, ExtendedToolDefinition } from './types';
import { ToolCall, ToolResult, FileChangeMetadata } from '../llm/types';
import { logger, LogCategory } from '../logging';
import { WorkspaceRoots } from '../workspace/workspace-roots';

/** 行号误差容忍度（20%，对齐 Augment _lineNumberErrorTolerance） */
const LINE_NUMBER_ERROR_TOLERANCE = 0.2;

/** 模糊匹配相似度阈值（85%） */
const FUZZY_MATCH_THRESHOLD = 0.85;

/** 模糊匹配锚点搜索窗口（锚点上下各 30 行） */
const FUZZY_SEARCH_WINDOW = 30;

/** 探针回退上下文行数（目标行号上下各 20 行） */
const PROBE_CONTEXT_LINES = 20;

/** 写入后等待最终内容稳定的最大轮次 */
const POST_WRITE_SETTLE_MAX_ATTEMPTS = 6;

/** 写入后每轮重读间隔（ms） */
const POST_WRITE_SETTLE_DELAY_MS = 120;

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
  numLinesDiff?: number;
}

/** 成功替换条目的结构化结果（用于写入后统一重算） */
interface SuccessfulReplaceEntry {
  index: number;
  message: string;
  newStrStartLine: number;
  newStrEndLine: number;
  numLinesDiff: number;
}

/** 批量编辑 JSON 文件中的单个文件条目 */
interface BulkEditFileEntry {
  path: string;
  edits: Array<{
    old_str: string;
    new_str: string;
    old_str_start_line?: number;
    old_str_end_line?: number;
  }>;
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
      this.getFileBulkEditDefinition(),
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
      description: `Edit a file by replacing text. Supports multiple replacements in one call.

Notes for text replacement:
* Use file_view to read the file before editing. If the same file is already fresh in current context, do not repeat file_view.
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
   * file_bulk_edit 工具定义
   * 读取 JSON patch 文件，逐文件复用 executeStrReplace 执行批量跨文件替换
   */
  private getFileBulkEditDefinition(): ExtendedToolDefinition {
    return {
      name: 'file_bulk_edit',
      description: `Apply bulk edits across multiple files from a JSON patch file.

This tool reads a JSON file containing edit instructions for multiple files and applies them atomically per file using the same fuzzy-matching engine as file_edit.

The JSON file must be an array of file entries:
\`\`\`json
[
  {
    "path": "/absolute/path/to/file.ts",
    "edits": [
      { "old_str": "original text", "new_str": "replacement text", "old_str_start_line": 10, "old_str_end_line": 12 }
    ]
  }
]
\`\`\`

Each entry's edits follow the same rules as file_edit: old_str must match exactly, old_str_start_line/old_str_end_line are 1-based and optional but recommended for disambiguation.

Usage:
1. Use launch-process to run a script (Python/Bash) that computes the edits
2. The script outputs the JSON patch file (e.g. /tmp/bulk_edits.json)
3. Call file_bulk_edit with the path to that JSON file

IMPORTANT:
* The script must NOT modify source files directly
* All file paths in the JSON must be absolute
* Each file's edits are applied independently; a failure in one file does not block others`,
      input_schema: {
        type: 'object',
        properties: {
          edits_file: {
            type: 'string',
            description: 'Absolute path to the JSON patch file containing bulk edit instructions'
          }
        },
        required: ['edits_file']
      },
      metadata: {
        source: 'builtin',
        category: 'file',
        tags: ['file', 'edit', 'bulk', 'development']
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
    return toolName === 'file_view' || toolName === 'file_create' || toolName === 'file_edit' || toolName === 'file_insert' || toolName === 'file_bulk_edit';
  }

  /**
   * 执行工具调用
   */
  async execute(toolCall: ToolCall): Promise<ToolResult> {
    // file_bulk_edit 使用 edits_file 而非 path，独立处理
    if (toolCall.name === 'file_bulk_edit') {
      const editsFile = (toolCall.arguments as any)?.edits_file as string;
      if (!editsFile) {
        return { toolCallId: toolCall.id, content: 'Error: edits_file is required', isError: true };
      }
      try {
        return await this.executeBulkEdit(toolCall.id, editsFile);
      } catch (error: any) {
        logger.error('FileExecutor error', { tool: toolCall.name, error: error.message }, LogCategory.TOOLS);
        return { toolCallId: toolCall.id, content: `Error: ${error.message}`, isError: true };
      }
    }

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

    // 读取原始内容（覆写场景用于 diff）
    let originalContent = '';
    try {
      originalContent = await this.readFileContent(filePath);
    } catch { /* 新建文件，原始内容为空 */ }

    // 快照回调（覆写时保护原始内容）
    this.onBeforeWrite?.(filePath);

    // 通过 WorkspaceEdit 创建/覆写文件
    await this.createFileViaWorkspaceEdit(filePath, finalContent);

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
   * 执行批量跨文件编辑
   * 读取 JSON patch 文件 → 逐文件解析 edits → 复用 executeStrReplace 执行替换
   */
  private async executeBulkEdit(toolCallId: string, editsFilePath: string): Promise<ToolResult> {
    // 1. 读取并解析 JSON patch 文件
    let rawContent: string;
    try {
      rawContent = await fs.readFile(editsFilePath, 'utf-8');
    } catch (error: any) {
      return { toolCallId, content: `Error: cannot read edits file: ${error.message}`, isError: true };
    }

    let fileEntries: BulkEditFileEntry[];
    try {
      fileEntries = JSON.parse(rawContent);
    } catch (error: any) {
      return { toolCallId, content: `Error: invalid JSON in edits file: ${error.message}`, isError: true };
    }

    if (!Array.isArray(fileEntries) || fileEntries.length === 0) {
      return { toolCallId, content: 'Error: edits file must be a non-empty JSON array', isError: true };
    }

    // 2. 逐文件执行替换
    const allResults: string[] = [];
    let successCount = 0;
    let failCount = 0;

    for (const fileEntry of fileEntries) {
      if (!fileEntry.path || !Array.isArray(fileEntry.edits) || fileEntry.edits.length === 0) {
        allResults.push(`[SKIP] Invalid entry (missing path or edits)`);
        failCount++;
        continue;
      }

      // 路径解析
      const pathResolution = this.resolveWorkspacePath(fileEntry.path, true);
      if (!pathResolution.absolutePath) {
        allResults.push(`[FAIL] ${fileEntry.path}: ${pathResolution.error || 'path outside workspace'}`);
        failCount++;
        continue;
      }
      const resolvedPath = pathResolution.absolutePath;

      // 将 BulkEditFileEntry.edits 转换为 EditEntry[]
      const entries: EditEntry[] = fileEntry.edits.map((edit, idx) => ({
        index: idx,
        oldStr: edit.old_str,
        newStr: edit.new_str,
        startLine: edit.old_str_start_line,
        endLine: edit.old_str_end_line,
      }));

      // 复用 executeStrReplace 执行替换
      const result = await this.executeStrReplace(toolCallId, resolvedPath, entries);
      const displayPath = this.workspaceRoots.toDisplayPath(resolvedPath);

      if (result.isError) {
        allResults.push(`[FAIL] ${displayPath}: ${result.content}`);
        failCount++;
      } else {
        allResults.push(`[OK] ${displayPath}: ${entries.length} edit(s) applied`);
        successCount++;
      }
    }

    // 3. 汇总结果
    const summary = `Bulk edit complete: ${successCount} file(s) succeeded, ${failCount} file(s) failed.`;
    const content = summary + '\n\n' + allResults.join('\n');
    const isError = successCount === 0 && failCount > 0;

    return { toolCallId, content, isError };
  }

  /**
   * 执行多条替换（核心方法）
   * 读文件一次 → 按 startLine 降序逐条替换 → 通过 WorkspaceEdit 写入
   */
  private async executeStrReplace(
    toolCallId: string,
    filePath: string,
    entries: EditEntry[]
  ): Promise<ToolResult> {
    // 通过 VSCode Document API 读取（获取编辑器中最新状态）
    let content: string;
    try {
      content = await this.readFileContent(filePath);
    } catch (error: any) {
      if (error.code === 'ENOENT' || error.message?.includes('cannot open')) {
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
    const originalContent = content;
    const results: string[] = [];
    const successfulEntries: SuccessfulReplaceEntry[] = [];
    let hasError = false;
    let hasSuccess = false;

    for (const entry of sorted) {
      const result = this.matchAndReplace(content, entry);
      if (result.error) {
        results.push(`[FAILED] Entry #${entry.index}: ${result.error}`);
        hasError = true;
        continue;
      }
      content = result.newContent!;
      hasSuccess = true;

      const successEntry: SuccessfulReplaceEntry = {
        index: entry.index,
        message: result.message || 'OK',
        newStrStartLine: result.newStrStartLine!,
        newStrEndLine: result.newStrEndLine!,
        numLinesDiff: result.numLinesDiff ?? 0,
      };
      successfulEntries.push(successEntry);
      const successMsg = this.buildEditSuccessMessage(
        successEntry.message,
        content,
        successEntry.newStrStartLine,
        successEntry.newStrEndLine
      );
      results.push(`[OK] Entry #${entry.index}: ${successMsg}`);
    }

    // 全部失败：不写入，直接返回错误
    if (!hasSuccess) {
      return { toolCallId, content: results.join('\n'), isError: true };
    }

    // 保存撤销信息（记录写入前的状态）
    this.undoStack.set(filePath, originalContent);

    // 快照回调
    this.onBeforeWrite?.(filePath);

    // 通过 WorkspaceEdit 写入（进入 VSCode 原生撤销栈）
    await this.writeFileViaWorkspaceEdit(filePath, content);
    const finalContent = await this.readSettledFileContent(filePath, content);
    this.rebaseSuccessfulEntries(successfulEntries, content, finalContent);

    logger.info('File edited (file_edit)', {
      path: filePath,
      entryCount: entries.length,
      successCount: successfulEntries.length,
      failedCount: entries.length - successfulEntries.length,
      changedAfterWrite: finalContent !== content,
    }, LogCategory.TOOLS);

    const fileChange = this.buildFileChangeMetadata(originalContent, finalContent, filePath, 'modify');

    // 用最终内容重建成功条目的消息（行号可能因 format-on-save 变动）
    const finalSuccessMessages = successfulEntries.map(entry =>
      `[OK] Entry #${entry.index}: ${this.buildEditSuccessMessage(
        entry.message,
        finalContent,
        entry.newStrStartLine,
        entry.newStrEndLine
      )}`
    );

    // 收集失败条目的消息（已在循环中以 [FAILED] 前缀记录）
    const failedMessages = results.filter(r => r.startsWith('[FAILED]'));
    const hasNoEffectiveChange = (fileChange.additions ?? 0) === 0 && (fileChange.deletions ?? 0) === 0;
    const inMemoryChanged = originalContent !== content;
    const settledChanged = originalContent !== finalContent;
    const postWriteRewritten = content !== finalContent;

    if (hasNoEffectiveChange) {
      const allMessages = [...finalSuccessMessages, ...failedMessages];
      const diagnostics: string[] = [];
      if (!inMemoryChanged) {
        diagnostics.push('- 内存态无增量：替换结果在同次调用内相互抵消，或归一化后等价。');
      }
      if (inMemoryChanged && !settledChanged) {
        diagnostics.push('- 写入后回到原文：可能被 format-on-save 或外部进程重写。');
      }
      if (postWriteRewritten) {
        diagnostics.push('- 写入缓冲与最终落盘不同：检测到保存后有二次改写。');
      }
      if (diagnostics.length === 0) {
        diagnostics.push('- 未检测到最终文本差异：请重新 file_view 并基于最新内容生成 old_str。');
      }

      logger.warn('file_edit produced no effective changes', {
        path: filePath,
        entryCount: entries.length,
        inMemoryChanged,
        settledChanged,
        postWriteRewritten,
      }, LogCategory.TOOLS);

      return {
        toolCallId,
        content: `Error: file_edit produced no effective text changes after write/save settle.\n原因诊断:\n${diagnostics.join('\n')}\n${allMessages.join('\n')}\n\nHint: run file_view for the latest content and regenerate old_str/line anchors.`,
        isError: true,
      };
    }

    // 单条且无错误时简化输出
    if (entries.length === 1 && !hasError) {
      return {
        toolCallId,
        content: finalSuccessMessages[0].replace(/^\[OK\] Entry #\d+: /, ''),
        isError: false,
        fileChange,
      };
    }

    // 组合最终消息：成功条目 + 失败条目
    const allMessages = [...finalSuccessMessages, ...failedMessages];
    const summary = hasError
      ? `Partial: ${successfulEntries.length}/${entries.length} replacements applied (${failedMessages.length} failed).`
      : `OK: ${entries.length} replacements applied.`;

    return {
      toolCallId,
      content: `${summary}\n${allMessages.join('\n')}`,
      isError: hasError,
      fileChange,
    };
  }

  /**
   * 构建替换成功消息（基于最终内容输出准确行号与片段）
   */
  private buildEditSuccessMessage(
    baseMessage: string,
    content: string,
    startLine: number,
    endLine: number
  ): string {
    const lines = content.split('\n');
    if (lines.length === 0) {
      return baseMessage;
    }

    const safeStart = Math.max(0, Math.min(startLine, lines.length - 1));
    const safeEnd = Math.max(safeStart, Math.min(endLine, lines.length - 1));
    const snippet = lines.slice(safeStart, safeEnd + 1);
    const maxSnippetLines = 20;
    const truncated = snippet.length > maxSnippetLines;
    const displaySnippet = truncated ? snippet.slice(0, maxSnippetLines) : snippet;
    const numberedSnippet = displaySnippet
      .map((line, i) => `${safeStart + i + 1}\t${line}`)
      .join('\n');

    let message = baseMessage;
    message += `\nnew_str starts at line ${safeStart + 1} and ends at line ${safeEnd + 1}.`;
    message += `\n\nSnippet of edited section:\n${numberedSnippet}`;
    if (truncated) {
      message += `\n... (${snippet.length - maxSnippetLines} more lines)`;
    }
    return message;
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

  /**
   * 将成功替换条目重映射到最终文件状态：
   * 1) 先按行号升序做多条编辑 line-shift 对齐
   * 2) 再基于最终内容做行号映射（处理格式化导致的偏移）
   */
  private rebaseSuccessfulEntries(
    entries: SuccessfulReplaceEntry[],
    beforeExternalChanges: string,
    finalContent: string
  ): void {
    if (entries.length === 0) {
      return;
    }

    const sortedByLine = [...entries].sort((a, b) => a.newStrStartLine - b.newStrStartLine);
    let lineShift = 0;
    for (const entry of sortedByLine) {
      entry.newStrStartLine += lineShift;
      entry.newStrEndLine += lineShift;
      lineShift += entry.numLinesDiff;
    }

    if (beforeExternalChanges === finalContent) {
      return;
    }

    const beforeLines = beforeExternalChanges.split('\n');
    const afterLines = finalContent.split('\n');
    const lineMap = this.buildLooseLineMap(beforeLines, afterLines);

    for (const entry of entries) {
      const mappedStart = this.remapLineNumber(entry.newStrStartLine, lineMap, beforeLines, afterLines);
      const mappedEnd = this.remapLineNumber(entry.newStrEndLine, lineMap, beforeLines, afterLines);
      entry.newStrStartLine = Math.min(mappedStart, mappedEnd);
      entry.newStrEndLine = Math.max(mappedStart, mappedEnd);
    }
  }

  /**
   * 构建宽松行映射（忽略前后空白，保持单调递增）
   */
  private buildLooseLineMap(beforeLines: string[], afterLines: string[]): number[] {
    const normalize = (line: string) => line.trim();
    const linePositions = new Map<string, number[]>();

    for (let i = 0; i < afterLines.length; i++) {
      const key = normalize(afterLines[i]);
      const positions = linePositions.get(key);
      if (positions) {
        positions.push(i);
      } else {
        linePositions.set(key, [i]);
      }
    }

    const map = new Array<number>(beforeLines.length).fill(-1);
    const keyCursor = new Map<string, number>();
    let lastMatched = -1;

    for (let i = 0; i < beforeLines.length; i++) {
      const key = normalize(beforeLines[i]);
      const positions = linePositions.get(key);
      if (!positions || positions.length === 0) {
        continue;
      }

      let cursor = keyCursor.get(key) ?? 0;
      while (cursor < positions.length && positions[cursor] <= lastMatched) {
        cursor++;
      }
      if (cursor >= positions.length) {
        continue;
      }

      const matched = positions[cursor];
      map[i] = matched;
      keyCursor.set(key, cursor + 1);
      lastMatched = matched;
    }

    return map;
  }

  /**
   * 将单行号映射到最终内容，未命中时使用邻近锚点 + 相似度窗口兜底
   */
  private remapLineNumber(
    targetLine: number,
    lineMap: number[],
    beforeLines: string[],
    afterLines: string[]
  ): number {
    if (afterLines.length === 0) {
      return 0;
    }
    if (beforeLines.length === 0) {
      return Math.max(0, Math.min(targetLine, afterLines.length - 1));
    }

    const clamped = Math.max(0, Math.min(targetLine, beforeLines.length - 1));
    if (lineMap[clamped] !== -1) {
      return lineMap[clamped];
    }

    for (let offset = 1; offset < beforeLines.length; offset++) {
      const upper = clamped - offset;
      if (upper >= 0 && lineMap[upper] !== -1) {
        return Math.max(0, Math.min(lineMap[upper] + offset, afterLines.length - 1));
      }
      const lower = clamped + offset;
      if (lower < beforeLines.length && lineMap[lower] !== -1) {
        return Math.max(0, Math.min(lineMap[lower] - offset, afterLines.length - 1));
      }
    }

    const approximate = Math.max(0, Math.min(clamped, afterLines.length - 1));
    const sourceLine = beforeLines[clamped];
    let bestLine = approximate;
    let bestScore = this.computeLineSimilarity(sourceLine, afterLines[approximate]);

    const window = 20;
    const start = Math.max(0, approximate - window);
    const end = Math.min(afterLines.length - 1, approximate + window);

    for (let i = start; i <= end; i++) {
      const score = this.computeLineSimilarity(sourceLine, afterLines[i]);
      if (score > bestScore) {
        bestScore = score;
        bestLine = i;
      }
    }

    return bestLine;
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
   * 完整管线：换行符规范化 → 空文件处理 → 精确匹配 → 缩进互转 → 空白规范化 → 模糊匹配 → 探针回退
   */
  private matchAndReplace(
    content: string,
    entry: EditEntry
  ): ReplaceResult {
    // 换行符规范化
    let oldStr = this.normalizeLineEndings(entry.oldStr);
    let newStr = this.normalizeLineEndings(entry.newStr);
    const normalizedContent = this.normalizeLineEndings(content);
    const { startLine, endLine } = entry;

    // old_str 和 new_str 相同
    if (oldStr === newStr) {
      return { error: 'old_str and new_str are identical. No replacement needed.' };
    }

    // 空文件特殊处理
    if (oldStr.trim() === '') {
      if (normalizedContent.trim() === '') {
        const newStrLines = newStr.split('\n');
        return {
          newContent: newStr,
          message: 'OK (empty file replaced)',
          newStrStartLine: 0,
          newStrEndLine: Math.max(0, newStrLines.length - 1),
          numLinesDiff: Math.max(0, newStrLines.length - 1),
        };
      }
      return { error: 'old_str is empty, which is only allowed when the file is empty or contains only whitespace.' };
    }

    // ── 阶段 1：精确匹配 ──
    let matches = this.findAllMatches(normalizedContent, oldStr);

    // ── 阶段 2：缩进互转 ──
    if (matches.length === 0) {
      const indentFix = this.tryTabIndentFix(normalizedContent, oldStr, newStr);
      if (indentFix.matches.length > 0) {
        matches = indentFix.matches;
        oldStr = indentFix.oldStr;
        newStr = indentFix.newStr;
      }
    }

    // ── 阶段 3：行尾空白规范化 ──
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

    // ── 阶段 4：模糊匹配（精确和规范化均失败时启用） ──
    if (matches.length === 0) {
      const contentLines = normalizedContent.split('\n');
      const oldStrLines = oldStr.split('\n');

      // 先尝试带锚点的局部模糊匹配；若失败且给了锚点，再做一次全局模糊回退，
      // 处理“前序编辑/格式化导致块整体位移，超出锚点窗口”的场景。
      let fuzzyResult = this.fuzzyMatchBlock(contentLines, oldStrLines, startLine, endLine);
      let fuzzyMode: 'anchored' | 'global' = 'anchored';

      if (!fuzzyResult && (startLine !== undefined || endLine !== undefined)) {
        fuzzyResult = this.fuzzyMatchBlock(contentLines, oldStrLines);
        if (fuzzyResult) {
          fuzzyMode = 'global';
        }
      }

      if (fuzzyResult) {
        logger.info('Fuzzy match succeeded', {
          mode: fuzzyMode,
          similarity: fuzzyResult.similarity.toFixed(3),
          matchedLines: `${fuzzyResult.startLine + 1}-${fuzzyResult.endLine + 1}`,
          anchorLines: startLine !== undefined ? `${startLine}-${endLine}` : 'none',
        }, LogCategory.TOOLS);

        // 模糊匹配成功：用文件中实际匹配到的代码块替代 oldStr 进行替换
        return this.applyReplacementAtLines(
          contentLines,
          fuzzyResult.startLine,
          fuzzyResult.endLine,
          newStr,
          fuzzyMode === 'anchored'
            ? `OK (fuzzy match, similarity: ${(fuzzyResult.similarity * 100).toFixed(1)}%)`
            : `OK (fuzzy global fallback, similarity: ${(fuzzyResult.similarity * 100).toFixed(1)}%)`
        );
      }
    }

    // ── 阶段 5：探针回退（所有匹配策略均失败） ──
    if (matches.length === 0) {
      const contentLines = normalizedContent.split('\n');
      const errorMsg = this.buildProbeErrorMessage(contentLines, oldStr, startLine, endLine);
      return { error: errorMsg };
    }

    // 确定使用哪个匹配
    let matchIdx: number;

    if (matches.length === 1) {
      matchIdx = 0;
    } else {
      // 多匹配：需要行号来消歧
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

    return this.applyReplacementAtLines(
      contentLines,
      match.startLine,
      match.startLine + oldStrLineCount - 1,
      newStr,
      'OK'
    );
  }

  /**
   * 在指定行范围执行替换（matchAndReplace 和 fuzzyMatch 的共用逻辑）
   */
  private applyReplacementAtLines(
    contentLines: string[],
    replaceStartLine: number,
    replaceEndLine: number,
    newStr: string,
    message: string
  ): ReplaceResult {
    const newStrLines = newStr.split('\n');

    const before = contentLines.slice(0, replaceStartLine).join('\n');
    const after = contentLines.slice(replaceEndLine + 1).join('\n');

    let newContent: string;
    if (before && after) {
      newContent = before + '\n' + newStr + '\n' + after;
    } else if (before) {
      newContent = before + '\n' + newStr;
    } else if (after) {
      newContent = newStr + '\n' + after;
    } else {
      newContent = newStr;
    }

    const newStrStartLine = replaceStartLine;
    const newStrEndLine = replaceStartLine + newStrLines.length - 1;
    const replacedLineCount = replaceEndLine - replaceStartLine + 1;
    const numLinesDiff = newStrLines.length - replacedLineCount;

    return {
      newContent,
      message,
      newStrStartLine,
      newStrEndLine,
      numLinesDiff,
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
   * 插入文本
   * 支持多条插入，底部优先处理，通过 WorkspaceEdit 一次写入
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

  // ============================================================================
  // VSCode Document I/O 层
  // 优先读取编辑器已打开文档（保证拿到未保存的最新状态），
  // 写入通过 WorkspaceEdit 应用，进入 VSCode 原生撤销栈
  // ============================================================================

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

  // ============================================================================
  // 模糊匹配引擎
  // 当精确匹配和缩进/空白规范化均失败时启用
  // 基于行级相似度评分 + 锚点滑动窗口搜索
  // ============================================================================

  /**
   * 计算两个字符串的行级相似度（0.0 ~ 1.0）
   * 使用 token 化的 Jaccard 系数，兼顾效率和准确度
   */
  private computeLineSimilarity(lineA: string, lineB: string): number {
    const a = lineA.trim();
    const b = lineB.trim();

    // 完全相同
    if (a === b) return 1.0;

    // 其中一个为空行
    if (a === '' && b === '') return 1.0;
    if (a === '' || b === '') return 0.0;

    // 提取 token（按非字母数字字符分割）
    const tokensA = new Set(a.split(/[^a-zA-Z0-9_$]+/).filter(Boolean));
    const tokensB = new Set(b.split(/[^a-zA-Z0-9_$]+/).filter(Boolean));

    if (tokensA.size === 0 && tokensB.size === 0) {
      // 纯符号行：直接字符比较
      return this.computeCharSimilarity(a, b);
    }

    // Jaccard 系数
    let intersection = 0;
    for (const token of tokensA) {
      if (tokensB.has(token)) intersection++;
    }
    const union = tokensA.size + tokensB.size - intersection;
    return union === 0 ? 1.0 : intersection / union;
  }

  /**
   * 字符级相似度（用于纯符号行的 fallback）
   * 使用双向最长公共子序列长度比率
   */
  private computeCharSimilarity(a: string, b: string): number {
    if (a === b) return 1.0;
    const maxLen = Math.max(a.length, b.length);
    if (maxLen === 0) return 1.0;

    // 使用简化的 LCS 长度计算（空间优化为 O(min(m,n))）
    const short = a.length <= b.length ? a : b;
    const long = a.length <= b.length ? b : a;
    const prev = new Array(short.length + 1).fill(0);
    const curr = new Array(short.length + 1).fill(0);

    for (let i = 1; i <= long.length; i++) {
      for (let j = 1; j <= short.length; j++) {
        if (long[i - 1] === short[j - 1]) {
          curr[j] = prev[j - 1] + 1;
        } else {
          curr[j] = Math.max(prev[j], curr[j - 1]);
        }
      }
      // 交换 prev 和 curr
      for (let j = 0; j <= short.length; j++) {
        prev[j] = curr[j];
        curr[j] = 0;
      }
    }

    const lcsLength = prev[short.length];
    return lcsLength / maxLen;
  }

  /**
   * 计算代码块的整体相似度（各行相似度的加权平均）
   * 非空行权重高于空行，避免空行稀释匹配质量
   */
  private computeBlockSimilarity(linesA: string[], linesB: string[]): number {
    if (linesA.length !== linesB.length) return 0.0;
    if (linesA.length === 0) return 1.0;

    let totalWeight = 0;
    let weightedSum = 0;

    for (let i = 0; i < linesA.length; i++) {
      const weight = linesA[i].trim() === '' && linesB[i].trim() === '' ? 0.3 : 1.0;
      const sim = this.computeLineSimilarity(linesA[i], linesB[i]);
      weightedSum += sim * weight;
      totalWeight += weight;
    }

    return totalWeight === 0 ? 1.0 : weightedSum / totalWeight;
  }

  /**
   * 模糊匹配核心：在锚点附近搜索与 oldStr 最相似的代码块
   *
   * 搜索策略：
   * 1. 以 startLine/endLine 为锚点确定搜索窗口
   * 2. 在窗口内滑动等长块，逐一计算相似度
   * 3. 取最高相似度且超过阈值的位置
   *
   * 返回 null 表示未找到足够相似的匹配
   */
  private fuzzyMatchBlock(
    contentLines: string[],
    oldStrLines: string[],
    anchorStartLine?: number,
    anchorEndLine?: number
  ): { startLine: number; endLine: number; similarity: number } | null {
    const blockLen = oldStrLines.length;
    if (blockLen === 0 || blockLen > contentLines.length) return null;

    // 确定搜索范围
    let searchStart: number;
    let searchEnd: number;

    if (anchorStartLine !== undefined && anchorEndLine !== undefined) {
      // 锚点模式：在锚点附近搜索（0-based）
      const anchor0 = anchorStartLine - 1; // 转 0-based
      searchStart = Math.max(0, anchor0 - FUZZY_SEARCH_WINDOW);
      searchEnd = Math.min(contentLines.length - blockLen, anchor0 + FUZZY_SEARCH_WINDOW);
    } else {
      // 无锚点：全文搜索
      searchStart = 0;
      searchEnd = contentLines.length - blockLen;
    }

    // 安全边界
    searchStart = Math.max(0, searchStart);
    searchEnd = Math.min(contentLines.length - blockLen, searchEnd);

    let bestMatch: { startLine: number; endLine: number; similarity: number } | null = null;

    for (let pos = searchStart; pos <= searchEnd; pos++) {
      const candidateLines = contentLines.slice(pos, pos + blockLen);
      const similarity = this.computeBlockSimilarity(oldStrLines, candidateLines);

      if (similarity >= FUZZY_MATCH_THRESHOLD) {
        if (!bestMatch || similarity > bestMatch.similarity) {
          bestMatch = {
            startLine: pos,
            endLine: pos + blockLen - 1,
            similarity,
          };
        }
      }
    }

    return bestMatch;
  }

  // ============================================================================
  // 探针回退（Probe Fallback）
  // 当精确匹配和模糊匹配均失败时，提取目标区域附近的真实代码上下文，
  // 帮助 LLM 基于最新代码重新生成正确的编辑指令
  // ============================================================================

  /**
   * 生成探针回退错误信息
   * 包含锚点附近的实际代码，引导 LLM 重新生成编辑
   */
  private buildProbeErrorMessage(
    contentLines: string[],
    oldStr: string,
    anchorStartLine?: number,
    anchorEndLine?: number
  ): string {
    const nearMatches = this.findFirstLineMatches(contentLines.join('\n'), oldStr);
    let msg = 'Error: old_str not found in file. Exact match and fuzzy match both failed.';

    // 如果有锚点行号，提取附近上下文
    if (anchorStartLine !== undefined) {
      const anchor0 = anchorStartLine - 1; // 0-based
      const probeStart = Math.max(0, anchor0 - PROBE_CONTEXT_LINES);
      const probeEnd = Math.min(
        contentLines.length,
        (anchorEndLine ?? anchorStartLine) - 1 + PROBE_CONTEXT_LINES + 1
      );

      const contextSnippet = contentLines
        .slice(probeStart, probeEnd)
        .map((line, i) => `${String(probeStart + i + 1).padStart(6)}\t${line}`)
        .join('\n');

      msg += `\n\nActual code near lines ${probeStart + 1}-${probeEnd} (use this to regenerate your edit):\n${contextSnippet}`;
    }

    if (nearMatches.length > 0) {
      msg += `\n\nHint: old_str first line appears near line(s): ${nearMatches.join(', ')}. Use file_view to verify.`;
    } else {
      msg += '\n\nHint: old_str first line not found anywhere in the file. Use file_view to re-read.';
    }

    return msg;
  }
}
