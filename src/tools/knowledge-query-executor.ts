/**
 * 知识库查询执行器
 *
 * 提供 project_knowledge_query 工具，让 Worker/Orchestrator
 * 按需拉取项目知识库中的 ADR、FAQ、经验记录等内容。
 *
 * 设计原则（对标 Claude Code s05）：
 * - System Prompt 仅注入"知识库索引"（标题列表）
 * - 具体内容通过本工具按需拉取，作为 tool_result 返回
 * - 避免全量预载导致的 Token 浪费和注意力稀释
 */

import type { ToolExecutor, ExtendedToolDefinition } from './types';
import type { ToolCall, ToolResult } from '../llm/types';
import type { ProjectKnowledgeBase } from '../knowledge/project-knowledge-base';
import {
  GovernedKnowledgeContextService,
  type GovernedKnowledgeAuditMetadata,
} from '../knowledge/governed-knowledge-context-service';

/**
 * 知识片段类型
 */
type KnowledgeCategory = 'adr' | 'faq' | 'learning' | 'all';

export class KnowledgeQueryExecutor implements ToolExecutor {
  private knowledgeBaseGetter: () => ProjectKnowledgeBase | undefined;
  private auditGetter?: () => GovernedKnowledgeAuditMetadata | undefined;

  constructor(
    knowledgeBaseGetter: () => ProjectKnowledgeBase | undefined,
    auditGetter?: () => GovernedKnowledgeAuditMetadata | undefined,
  ) {
    this.knowledgeBaseGetter = knowledgeBaseGetter;
    this.auditGetter = auditGetter;
  }

  /**
   * 获取工具定义
   */
  getToolDefinition(): ExtendedToolDefinition {
    return {
      name: 'project_knowledge_query',
      description:
        'Fetch project guidelines, architectural decisions (ADRs), FAQs, or past learnings from the project knowledge base. ' +
        'Use this when you need to understand project conventions, past decisions, or specific guidelines before making changes.',
      input_schema: {
        type: 'object',
        properties: {
          category: {
            type: 'string',
            enum: ['adr', 'faq', 'learning', 'all'],
            description:
              'Category of knowledge to fetch. ' +
              '"adr" = Architecture Decision Records, ' +
              '"faq" = Frequently Asked Questions, ' +
              '"learning" = Past experience/lessons learned, ' +
              '"all" = All categories (summary mode)',
          },
          id: {
            type: 'string',
            description:
              'Specific record ID to fetch (e.g. "adr-001"). ' +
              'If omitted, returns a summary list of all records in the category.',
          },
          query: {
            type: 'string',
            description:
              'Optional search keyword to filter results. ' +
              'Matches against titles, content, and tags.',
          },
        },
        required: ['category'],
      },
      metadata: { source: 'builtin' as const },
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
    return toolName === 'project_knowledge_query';
  }

  /**
   * 执行工具调用
   */
  async execute(toolCall: ToolCall): Promise<ToolResult> {
    const args = toolCall.arguments as {
      category?: KnowledgeCategory;
      id?: string;
      query?: string;
    };

    const knowledgeBase = this.knowledgeBaseGetter();
    if (!knowledgeBase) {
      return {
        toolCallId: toolCall.id,
        content: 'Project knowledge base is not available. No project guidelines found.',
        isError: false,
      };
    }

    try {
      const category = args.category || 'all';
      if (!['adr', 'faq', 'learning', 'all'].includes(category)) {
        return {
          toolCallId: toolCall.id,
          content: `Unknown category: ${category}. Use one of: adr, faq, learning, all`,
          isError: true,
        };
      }

      const governedKnowledge = new GovernedKnowledgeContextService(knowledgeBase);
      const audit: GovernedKnowledgeAuditMetadata = {
        purpose: 'tool_query',
        consumer: 'knowledge_query_executor',
        ...(this.auditGetter?.() || {}),
      };
      const result = governedKnowledge.buildQueryResult({
        category,
        id: args.id?.trim(),
        query: args.query?.trim(),
      }, audit);

      return {
        toolCallId: toolCall.id,
        content: result.content,
        isError: false,
      };
    } catch (error: any) {
      return {
        toolCallId: toolCall.id,
        content: `Error querying project knowledge: ${error.message}`,
        isError: true,
      };
    }
  }
}
