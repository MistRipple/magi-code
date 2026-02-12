/**
 * ACE 代码检索执行器
 * 提供代码库上下文检索功能
 *
 * 工具: codebase_retrieval
 *
 * 参考 Augment 的 codebase-retrieval 工具设计：
 * - 接收自然语言查询
 * - 返回相关代码片段
 * - 支持跨语言检索
 * - 实时索引，结果反映代码库当前状态
 * - 支持 .gitignore 配置隔离
 *
 * 降级策略：
 * - ACE 可用时：优先使用远程语义搜索
 * - ACE 不可用时：通过 LocalCodeSearchService 回退到本地三级搜索（PKB + Grep + LSP）
 *
 * 配置来源：由 ToolManager 通过 configureAce() 方法统一管理
 * 配置存储：~/.magi/config.json 的 promptEnhance 字段
 */

import { ToolExecutor, ExtendedToolDefinition } from './types';
import { ToolCall, ToolResult } from '../llm/types';
import { AceIndexManager, IndexResult, SearchResult } from '../ace/index-manager';
import { logger, LogCategory } from '../logging';
import type { LocalCodeSearchService } from '../services/local-code-search-service';

/**
 * ACE 执行器
 * 提供代码库语义搜索功能
 *
 * 注意：配置由 ToolManager 统一管理，不直接读取配置文件
 * 降级：当 ACE 不可用时，通过 LocalCodeSearchService 回退到本地三级搜索
 */
export class AceExecutor implements ToolExecutor {
  private workspaceRoot: string;
  private baseUrl: string;
  private token: string;
  private indexManager: AceIndexManager | null = null;
  private isIndexing = false;
  private lastIndexResult: IndexResult | null = null;
  private localSearchService: LocalCodeSearchService | null = null;
  /** ACE 搜索超时（毫秒），超时后自动降级到本地搜索 */
  private static readonly ACE_SEARCH_TIMEOUT = 8000;

  constructor(workspaceRoot: string, baseUrl?: string, token?: string) {
    this.workspaceRoot = workspaceRoot;
    this.baseUrl = baseUrl || '';
    this.token = token || '';

    // 如果传入了配置，初始化索引管理器
    if (this.baseUrl && this.token) {
      this.indexManager = new AceIndexManager(workspaceRoot, this.baseUrl, this.token);
      logger.info('AceExecutor initialized with API', { baseUrl: this.baseUrl }, LogCategory.TOOLS);
    } else {
      logger.info('AceExecutor initialized without config, use configureAce() to enable', undefined, LogCategory.TOOLS);
    }
  }

  /**
   * 获取工具定义
   */
  getToolDefinition(): ExtendedToolDefinition {
    return {
      name: 'codebase_retrieval',
      description: `Search and retrieve relevant code from the codebase using semantic search.

This is the codebase context engine. It:
1. Takes a natural language description of the code you are looking for
2. Uses semantic search to find the most relevant code snippets
3. Maintains a real-time index of the codebase, results reflect current state
4. Can retrieve across different programming languages
5. Only reflects the current state on disk, has no version control history
6. Respects .gitignore for file exclusion

When to use:
- When you don't know which files contain the information you need
- When you want to gather high-level information about a task
- When you want to understand the codebase structure

Good query examples:
- "Where is the function that handles user authentication?"
- "What tests are there for the login functionality?"
- "How is the database connected to the application?"

Bad query examples (use grep_search instead):
- "Find definition of class Foo" (use grep_search)
- "Find all references to function bar" (use grep_search)
- "Show how class X is used in file Y" (use text_editor view)`,
      input_schema: {
        type: 'object',
        properties: {
          query: {
            type: 'string',
            description: 'Natural language description of the code you are looking for'
          },
          ensure_indexed: {
            type: 'boolean',
            description: 'Whether to ensure index is up-to-date before search (default: true)'
          }
        },
        required: ['query']
      },
      metadata: {
        source: 'builtin',
        category: 'search',
        tags: ['search', 'code', 'semantic', 'context', 'ace']
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
   * ACE 已配置或本地搜索服务可用时均返回 true
   */
  async isAvailable(toolName: string): Promise<boolean> {
    if (toolName !== 'codebase_retrieval') return false;
    return !!this.indexManager || !!this.localSearchService?.isAvailable;
  }

  /**
   * 注入本地搜索服务
   * 由 WebviewProvider 在初始化时调用（替代原闭包注入）
   */
  setLocalSearchService(service: LocalCodeSearchService): void {
    this.localSearchService = service;
    logger.info('AceExecutor.本地搜索服务已注入', undefined, LogCategory.TOOLS);
  }

  /**
   * 执行工具调用
   * 策略：ACE 可用 → 远程语义搜索；ACE 不可用 → 本地索引搜索
   */
  async execute(toolCall: ToolCall, signal?: AbortSignal): Promise<ToolResult> {
    const args = toolCall.arguments as {
      query: string;
      ensure_indexed?: boolean;
    };

    if (!args.query) {
      return {
        toolCallId: toolCall.id,
        content: 'Error: query is required',
        isError: true
      };
    }

    // 中断检查
    if (signal?.aborted) {
      return {
        toolCallId: toolCall.id,
        content: '任务已中断',
        isError: true
      };
    }

    const ensureIndexed = args.ensure_indexed !== false;

    logger.debug('AceExecutor executing', {
      query: args.query,
      ensureIndexed,
      hasAce: !!this.indexManager,
      hasFallback: !!this.localSearchService,
    }, LogCategory.TOOLS);

    try {
      // 策略 1: ACE 远程语义搜索（带超时降级）
      if (this.indexManager) {
        const searchPromise = this.indexManager.search(args.query, ensureIndexed);
        const timeoutPromise = new Promise<SearchResult>((_, reject) =>
          setTimeout(() => reject(new Error('ACE_TIMEOUT')), AceExecutor.ACE_SEARCH_TIMEOUT)
        );

        let result: SearchResult;
        try {
          result = await Promise.race([searchPromise, timeoutPromise]);
        } catch (raceError: any) {
          if (raceError?.message === 'ACE_TIMEOUT') {
            logger.warn('AceExecutor.ACE搜索超时，降级到本地搜索', {
              query: args.query.substring(0, 50),
              timeout: AceExecutor.ACE_SEARCH_TIMEOUT,
            }, LogCategory.TOOLS);
            return await this.executeLocalFallback(toolCall.id, args.query, `超时 ${AceExecutor.ACE_SEARCH_TIMEOUT}ms`);
          }
          throw raceError;
        }

        if (result.status === 'error') {
          // ACE 搜索失败，尝试回退到本地
          logger.warn('AceExecutor.ACE搜索失败，尝试本地回退', {
            error: result.content,
          }, LogCategory.TOOLS);
          return await this.executeLocalFallback(toolCall.id, args.query);
        }

        const output: string[] = [];
        if (result.stats) {
          output.push(`Query: "${result.stats.query}"`);
          output.push(`Searched ${result.stats.total_blobs} code blocks`);
          output.push('');
        }
        output.push(result.content);

        return {
          toolCallId: toolCall.id,
          content: output.join('\n'),
          isError: false
        };
      }

      // 策略 2: ACE 未配置，使用本地索引搜索
      return await this.executeLocalFallback(toolCall.id, args.query);
    } catch (error: any) {
      logger.error('AceExecutor error', { error: error.message }, LogCategory.TOOLS);
      // ACE 异常时仍尝试本地回退
      return await this.executeLocalFallback(toolCall.id, args.query, error.message);
    }
  }

  /**
   * 本地搜索回退
   * 使用 LocalCodeSearchService（三级搜索）作为 ACE 替代
   */
  private async executeLocalFallback(
    toolCallId: string,
    query: string,
    aceError?: string
  ): Promise<ToolResult> {
    if (!this.localSearchService?.isAvailable) {
      return {
        toolCallId,
        content: `Error: ACE API not configured and local search service not available.

To enable semantic code search, configure ACE:
- ACE_API_URL: The ACE server URL
- ACE_API_TOKEN: The authentication token

Without ACE, use grep_search for pattern-based code search.`,
        isError: true
      };
    }

    try {
      const localResult = await this.localSearchService.search(query, 10);

      if (localResult) {
        const header = aceError
          ? `[ACE 不可用 (${aceError})，已回退到本地三级搜索]\n\n`
          : `[本地三级搜索 — ACE 未配置]\n\n`;

        logger.info('AceExecutor.本地回退搜索成功', {
          query: query.substring(0, 50),
          resultLength: localResult.length,
        }, LogCategory.TOOLS);

        return {
          toolCallId,
          content: header + localResult,
          isError: false
        };
      }

      return {
        toolCallId,
        content: '未找到相关代码（本地三级搜索无结果）',
        isError: false
      };
    } catch (fallbackError: any) {
      logger.warn('AceExecutor.本地回退搜索失败', {
        error: fallbackError.message,
      }, LogCategory.TOOLS);

      return {
        toolCallId,
        content: `Search unavailable: ACE not configured, local search error: ${fallbackError.message}`,
        isError: true
      };
    }
  }

  /**
   * 手动触发索引
   */
  async reindex(): Promise<IndexResult> {
    if (!this.indexManager) {
      return {
        status: 'error',
        message: 'ACE API not configured'
      };
    }

    try {
      this.isIndexing = true;
      const result = await this.indexManager.indexProject();
      this.lastIndexResult = result;
      return result;
    } finally {
      this.isIndexing = false;
    }
  }

  /**
   * 获取索引状态
   */
  getIndexStatus(): { isIndexing: boolean; lastResult: IndexResult | null; isConfigured: boolean } {
    return {
      isIndexing: this.isIndexing,
      lastResult: this.lastIndexResult,
      isConfigured: !!this.indexManager
    };
  }

  /**
   * 更新配置（由 ToolManager 调用）
   * @param workspaceRoot 工作区根目录
   * @param baseUrl ACE API 地址（必须提供）
   * @param token ACE API 密钥（必须提供）
   */
  updateConfig(workspaceRoot: string, baseUrl?: string, token?: string): void {
    this.workspaceRoot = workspaceRoot;
    this.baseUrl = baseUrl || '';
    this.token = token || '';

    if (this.baseUrl && this.token) {
      this.indexManager = new AceIndexManager(workspaceRoot, this.baseUrl, this.token);
      logger.info('AceExecutor config updated', { baseUrl: this.baseUrl }, LogCategory.TOOLS);
    } else {
      this.indexManager = null;
      logger.info('AceExecutor config cleared', undefined, LogCategory.TOOLS);
    }
  }

  /**
   * 检查是否已配置
   */
  isConfigured(): boolean {
    return !!this.indexManager;
  }
}
