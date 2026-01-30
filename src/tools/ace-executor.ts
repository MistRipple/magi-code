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
 * 配置来源：由 ToolManager 通过 configureAce() 方法统一管理
 * 配置存储：~/.multicli/config.json 的 promptEnhance 字段
 */

import { ToolExecutor, ExtendedToolDefinition } from './types';
import { ToolCall, ToolResult } from '../llm/types';
import { AceIndexManager, IndexResult, SearchResult } from '../ace/index-manager';
import { logger, LogCategory } from '../logging';

/**
 * ACE 执行器
 * 提供代码库语义搜索功能
 *
 * 注意：配置由 ToolManager 统一管理，不直接读取配置文件
 */
export class AceExecutor implements ToolExecutor {
  private workspaceRoot: string;
  private baseUrl: string;
  private token: string;
  private indexManager: AceIndexManager | null = null;
  private isIndexing = false;
  private lastIndexResult: IndexResult | null = null;

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
- "Show how class X is used in file Y" (use text_editor view)

Parameters:
* query: Natural language description of the code you are looking for (required)
* ensure_indexed: Whether to ensure index is up-to-date before search (default: true)`,
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
   */
  async isAvailable(toolName: string): Promise<boolean> {
    return toolName === 'codebase_retrieval';
  }

  /**
   * 执行工具调用
   */
  async execute(toolCall: ToolCall): Promise<ToolResult> {
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

    const ensureIndexed = args.ensure_indexed !== false; // 默认 true

    logger.debug('AceExecutor executing', {
      query: args.query,
      ensureIndexed
    }, LogCategory.TOOLS);

    try {
      // 检查是否配置了 ACE API
      if (!this.indexManager) {
        return {
          toolCallId: toolCall.id,
          content: `Error: ACE API not configured.

To enable semantic code search, configure the following:
- ACE_API_URL: The ACE server URL
- ACE_API_TOKEN: The authentication token

Without ACE configuration, please use grep_search for pattern-based code search.`,
          isError: true
        };
      }

      // 执行语义搜索
      const result = await this.indexManager.search(args.query, ensureIndexed);

      if (result.status === 'error') {
        return {
          toolCallId: toolCall.id,
          content: `Search failed: ${result.content}`,
          isError: true
        };
      }

      // 格式化输出
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
    } catch (error: any) {
      logger.error('AceExecutor error', { error: error.message }, LogCategory.TOOLS);
      return {
        toolCallId: toolCall.id,
        content: `Error: ${error.message}`,
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
