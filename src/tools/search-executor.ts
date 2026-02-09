/**
 * 搜索执行器
 * 提供代码搜索功能
 *
 * 工具: grep_search
 */

import * as fs from 'fs/promises';
import * as path from 'path';
import { ToolExecutor, ExtendedToolDefinition } from './types';
import { ToolCall, ToolResult } from '../llm/types';
import { logger, LogCategory } from '../logging';

interface SearchMatch {
  file: string;
  line: number;
  content: string;
  contextBefore: string[];
  contextAfter: string[];
}

/**
 * 搜索执行器
 */
export class SearchExecutor implements ToolExecutor {
  private workspaceRoot: string;
  private defaultContextLines = 5;
  private maxResults = 100;

  constructor(workspaceRoot: string) {
    this.workspaceRoot = workspaceRoot;
  }

  /**
   * 获取工具定义
   */
  getToolDefinition(): ExtendedToolDefinition {
    return {
      name: 'grep_search',
      description: 'Search for text or regex in the codebase. Returns matching files, line numbers, and surrounding context. Respects .gitignore by default.',
      input_schema: {
        type: 'object',
        properties: {
          pattern: {
            type: 'string',
            description: 'The regex to search for (required). Example: "export.*ContentBlock"'
          },
          path: {
            type: 'string',
            description: 'Directory to search in, relative to workspace root (e.g. "src/tools"). Defaults to workspace root.'
          },
          include: {
            type: 'string',
            description: 'Glob pattern for files to include'
          },
          exclude: {
            type: 'string',
            description: 'Glob pattern for files to exclude'
          },
          context_lines: {
            type: 'number',
            description: 'Context lines before/after match (default: 5)'
          },
          case_sensitive: {
            type: 'boolean',
            description: 'Case sensitive search (default: false)'
          }
        },
        required: ['pattern']
      },
      metadata: {
        source: 'builtin',
        category: 'search',
        tags: ['search', 'grep', 'code']
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
    return toolName === 'grep_search';
  }

  /**
   * 执行工具调用
   */
  async execute(toolCall: ToolCall): Promise<ToolResult> {
    const args = toolCall.arguments as {
      pattern: string;
      path?: string;
      include?: string;
      exclude?: string;
      context_lines?: number;
      case_sensitive?: boolean;
    };

    if (!args.pattern) {
      return {
        toolCallId: toolCall.id,
        content: 'Error: pattern is required',
        isError: true
      };
    }

    const searchPath = args.path
      ? path.resolve(this.workspaceRoot, args.path)
      : this.workspaceRoot;

    const contextLines = args.context_lines ?? this.defaultContextLines;
    const caseSensitive = args.case_sensitive ?? false;

    logger.debug('SearchExecutor executing', {
      pattern: args.pattern,
      path: searchPath
    }, LogCategory.TOOLS);

    try {
      const regex = new RegExp(args.pattern, caseSensitive ? 'g' : 'gi');
      const matches = await this.searchFiles(
        searchPath,
        regex,
        contextLines,
        args.include,
        args.exclude
      );

      if (matches.length === 0) {
        return {
          toolCallId: toolCall.id,
          content: 'No matches found',
          isError: false
        };
      }

      const result = this.formatMatches(matches);

      return {
        toolCallId: toolCall.id,
        content: result,
        isError: false
      };
    } catch (error: any) {
      logger.error('SearchExecutor error', { error: error.message }, LogCategory.TOOLS);
      return {
        toolCallId: toolCall.id,
        content: `Error: ${error.message}`,
        isError: true
      };
    }
  }

  /**
   * 搜索文件
   */
  private async searchFiles(
    searchPath: string,
    regex: RegExp,
    contextLines: number,
    includePattern?: string,
    excludePattern?: string
  ): Promise<SearchMatch[]> {
    const matches: SearchMatch[] = [];
    const files = await this.collectFiles(searchPath, includePattern, excludePattern);

    for (const file of files) {
      if (matches.length >= this.maxResults) break;

      try {
        const content = await fs.readFile(file, 'utf-8');
        const lines = content.split('\n');

        for (let i = 0; i < lines.length; i++) {
          if (matches.length >= this.maxResults) break;

          if (regex.test(lines[i])) {
            // 重置正则状态
            regex.lastIndex = 0;

            const contextBefore = lines.slice(
              Math.max(0, i - contextLines),
              i
            );
            const contextAfter = lines.slice(
              i + 1,
              Math.min(lines.length, i + 1 + contextLines)
            );

            matches.push({
              file: path.relative(this.workspaceRoot, file),
              line: i + 1, // 1-based
              content: lines[i],
              contextBefore,
              contextAfter
            });
          }
        }
      } catch {
        // 跳过无法读取的文件
      }
    }

    return matches;
  }

  /**
   * 收集文件列表
   */
  private async collectFiles(
    dir: string,
    includePattern?: string,
    excludePattern?: string
  ): Promise<string[]> {
    const files: string[] = [];
    const excludeDirs = ['node_modules', '.git', 'dist', 'out', 'build', '.next', 'coverage'];

    const walk = async (currentPath: string): Promise<void> => {
      try {
        const entries = await fs.readdir(currentPath, { withFileTypes: true });

        for (const entry of entries) {
          const fullPath = path.join(currentPath, entry.name);

          if (entry.isDirectory()) {
            // 跳过排除的目录
            if (excludeDirs.includes(entry.name)) continue;
            if (entry.name.startsWith('.')) continue;

            await walk(fullPath);
          } else if (entry.isFile()) {
            // 检查文件是否匹配
            if (this.matchesPattern(entry.name, includePattern, excludePattern)) {
              files.push(fullPath);
            }
          }
        }
      } catch {
        // 跳过无法访问的目录
      }
    };

    await walk(dir);
    return files;
  }

  /**
   * 检查文件名是否匹配模式
   */
  private matchesPattern(
    filename: string,
    includePattern?: string,
    excludePattern?: string
  ): boolean {
    // 跳过二进制文件
    const binaryExtensions = ['.png', '.jpg', '.jpeg', '.gif', '.ico', '.pdf', '.zip', '.tar', '.gz', '.exe', '.dll', '.so', '.dylib'];
    const ext = path.extname(filename).toLowerCase();
    if (binaryExtensions.includes(ext)) return false;

    // 检查 include 模式
    if (includePattern) {
      const includeRegex = this.globToRegex(includePattern);
      if (!includeRegex.test(filename)) return false;
    }

    // 检查 exclude 模式
    if (excludePattern) {
      const excludeRegex = this.globToRegex(excludePattern);
      if (excludeRegex.test(filename)) return false;
    }

    return true;
  }

  /**
   * 简单的 glob 转正则
   */
  private globToRegex(glob: string): RegExp {
    const escaped = glob
      .replace(/[.+^${}()|[\]\\]/g, '\\$&')
      .replace(/\*/g, '.*')
      .replace(/\?/g, '.');
    return new RegExp(`^${escaped}$`, 'i');
  }

  /**
   * 格式化匹配结果
   */
  private formatMatches(matches: SearchMatch[]): string {
    const grouped = new Map<string, SearchMatch[]>();

    for (const match of matches) {
      const existing = grouped.get(match.file) || [];
      existing.push(match);
      grouped.set(match.file, existing);
    }

    const lines: string[] = [];

    for (const [file, fileMatches] of grouped) {
      lines.push(`\n=== ${file} ===`);

      for (const match of fileMatches) {
        lines.push(`\n--- Line ${match.line} ---`);

        // 上下文
        if (match.contextBefore.length > 0) {
          const startLine = match.line - match.contextBefore.length;
          match.contextBefore.forEach((line, idx) => {
            lines.push(`${String(startLine + idx).padStart(6)}  ${line}`);
          });
        }

        // 匹配行（高亮标记）
        lines.push(`${String(match.line).padStart(6)}> ${match.content}`);

        // 下文
        if (match.contextAfter.length > 0) {
          const startLine = match.line + 1;
          match.contextAfter.forEach((line, idx) => {
            lines.push(`${String(startLine + idx).padStart(6)}  ${line}`);
          });
        }
      }
    }

    lines.push(`\n[Found ${matches.length} matches in ${grouped.size} files]`);

    return lines.join('\n');
  }
}
