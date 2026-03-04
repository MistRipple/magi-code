/**
 * 代码库检索执行器
 *
 * 工具: codebase_retrieval
 * 实现路径: 本地检索基础设施（PKB + Grep + LSP）
 */

import { ToolExecutor, ExtendedToolDefinition } from './types';
import { ToolCall, ToolResult } from '../llm/types';
import { logger, LogCategory } from '../logging';
import type { CodebaseRetrievalService } from '../services/codebase-retrieval-service';

interface CodebaseRetrievalArgs {
  query: string;
  max_results?: number;
  scope_paths?: string[];
  ensure_indexed?: boolean;
}

export class CodebaseRetrievalExecutor implements ToolExecutor {
  private service: CodebaseRetrievalService | null = null;

  getToolDefinition(): ExtendedToolDefinition {
    return {
      name: 'codebase_retrieval',
      description: `Search and retrieve relevant code from the local codebase using semantic and structural retrieval.

This tool combines:
1. Project knowledge index search (semantic relevance)
2. Grep keyword matching (exact pattern support)
3. LSP symbol search (definition and symbol context)

Use this when:
- You need high-level codebase understanding
- You don't know exact file locations
- You want relevant snippets before precise edits`,
      input_schema: {
        type: 'object',
        properties: {
          query: {
            type: 'string',
            description: 'Natural language description of the code you are looking for'
          },
          max_results: {
            type: 'number',
            description: 'Maximum number of relevant result groups (default: 10)'
          },
          scope_paths: {
            type: 'array',
            description: 'Optional workspace-relative directories/files to scope retrieval (e.g. ["src/ui", "src/tools/tool-manager.ts"])',
            items: { type: 'string' }
          },
          ensure_indexed: {
            type: 'boolean',
            description: 'Compatibility field, ignored by local retrieval pipeline'
          }
        },
        required: ['query']
      },
      metadata: {
        source: 'builtin',
        category: 'search',
        tags: ['search', 'code', 'semantic', 'retrieval', 'local']
      }
    };
  }

  async getTools(): Promise<ExtendedToolDefinition[]> {
    return [this.getToolDefinition()];
  }

  async isAvailable(toolName: string): Promise<boolean> {
    if (toolName !== 'codebase_retrieval') return false;
    return !!this.service?.isAvailable;
  }

  setCodebaseRetrievalService(service: CodebaseRetrievalService): void {
    this.service = service;
    logger.info('CodebaseRetrievalExecutor.检索服务已注入', undefined, LogCategory.TOOLS);
  }

  async execute(toolCall: ToolCall, signal?: AbortSignal): Promise<ToolResult> {
    const args = toolCall.arguments as CodebaseRetrievalArgs;
    const query = typeof args?.query === 'string' ? args.query.trim() : '';

    if (!query) {
      return {
        toolCallId: toolCall.id,
        content: 'Error: query is required',
        isError: true
      };
    }

    if (signal?.aborted) {
      return {
        toolCallId: toolCall.id,
        content: '任务已中断',
        isError: true
      };
    }

    if (!this.service?.isAvailable) {
      return {
        toolCallId: toolCall.id,
        content: '代码检索服务暂不可用：本地索引尚未就绪',
        isError: true
      };
    }

    try {
      const maxResults = typeof args.max_results === 'number' && Number.isFinite(args.max_results)
        ? Math.max(1, Math.min(30, Math.floor(args.max_results)))
        : 10;
      const scopePaths = Array.isArray(args.scope_paths) ? args.scope_paths : [];
      const content = await this.service.search(query, maxResults, scopePaths);

      if (!content) {
        return {
          toolCallId: toolCall.id,
          content: '未找到相关代码（本地检索无结果）',
          isError: false
        };
      }

      return {
        toolCallId: toolCall.id,
        content,
        isError: false
      };
    } catch (error: any) {
      logger.error('CodebaseRetrievalExecutor.执行失败', { error: error?.message }, LogCategory.TOOLS);
      return {
        toolCallId: toolCall.id,
        content: `代码检索失败: ${error?.message || String(error)}`,
        isError: true
      };
    }
  }
}
