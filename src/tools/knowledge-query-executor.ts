/**
 * 知识库查询执行器
 *
 * 提供 fetch_project_guidelines 工具，让 Worker/Orchestrator
 * 按需拉取项目知识库中的 ADR、FAQ、经验记录等内容。
 *
 * 设计原则（对标 Claude Code s05）：
 * - System Prompt 仅注入"知识库索引"（标题列表）
 * - 具体内容通过本工具按需拉取，作为 tool_result 返回
 * - 避免全量预载导致的 Token 浪费和注意力稀释
 */

import type { ToolExecutor, ExtendedToolDefinition } from './types';
import type { ToolCall, ToolResult } from '../llm/types';
import type { ProjectKnowledgeBase, ADRRecord, FAQRecord, LearningRecord } from '../knowledge/project-knowledge-base';

/**
 * 知识片段类型
 */
type KnowledgeCategory = 'adr' | 'faq' | 'learning' | 'all';

export class KnowledgeQueryExecutor implements ToolExecutor {
  private knowledgeBaseGetter: () => ProjectKnowledgeBase | undefined;

  constructor(knowledgeBaseGetter: () => ProjectKnowledgeBase | undefined) {
    this.knowledgeBaseGetter = knowledgeBaseGetter;
  }

  /**
   * 获取工具定义
   */
  getToolDefinition(): ExtendedToolDefinition {
    return {
      name: 'fetch_project_guidelines',
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
    return toolName === 'fetch_project_guidelines';
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

    const category = args.category || 'all';
    const id = args.id?.trim();
    const query = args.query?.trim().toLowerCase();

    try {
      // 如果指定了 ID，直接返回单条记录
      if (id) {
        return this.fetchById(toolCall.id, knowledgeBase, id);
      }

      // 按类别返回列表
      switch (category) {
        case 'adr':
          return this.fetchADRs(toolCall.id, knowledgeBase, query);
        case 'faq':
          return this.fetchFAQs(toolCall.id, knowledgeBase, query);
        case 'learning':
          return this.fetchLearnings(toolCall.id, knowledgeBase, query);
        case 'all':
          return this.fetchAll(toolCall.id, knowledgeBase, query);
        default:
          return {
            toolCallId: toolCall.id,
            content: `Unknown category: ${category}. Use one of: adr, faq, learning, all`,
            isError: true,
          };
      }
    } catch (error: any) {
      return {
        toolCallId: toolCall.id,
        content: `Error fetching project guidelines: ${error.message}`,
        isError: true,
      };
    }
  }

  private fetchById(
    toolCallId: string,
    kb: ProjectKnowledgeBase,
    id: string
  ): ToolResult {
    // 尝试 ADR
    const adr = kb.getADR(id);
    if (adr) {
      return {
        toolCallId,
        content: this.formatADRDetail(adr),
      };
    }

    // 尝试 FAQ
    const faqs = kb.getFAQs();
    const faq = faqs.find(f => f.id === id);
    if (faq) {
      return {
        toolCallId,
        content: this.formatFAQDetail(faq),
      };
    }

    // 尝试 Learning
    const learnings = kb.getLearnings();
    const learning = learnings.find(l => l.id === id);
    if (learning) {
      return {
        toolCallId,
        content: this.formatLearningDetail(learning),
      };
    }

    return {
      toolCallId,
      content: `No record found with ID: ${id}`,
      isError: false,
    };
  }

  private fetchADRs(
    toolCallId: string,
    kb: ProjectKnowledgeBase,
    query?: string
  ): ToolResult {
    let adrs = kb.getADRs({ status: 'accepted' });
    if (query) {
      adrs = adrs.filter(adr =>
        adr.title.toLowerCase().includes(query) ||
        adr.decision.toLowerCase().includes(query) ||
        adr.context.toLowerCase().includes(query)
      );
    }

    if (adrs.length === 0) {
      return {
        toolCallId,
        content: query
          ? `No ADRs match query "${query}".`
          : 'No accepted ADRs found in the project knowledge base.',
      };
    }

    const lines = adrs.map(adr =>
      `### ${adr.id}: ${adr.title}\n**Status**: ${adr.status}\n**Decision**: ${adr.decision}\n**Context**: ${adr.context}`
    );

    return {
      toolCallId,
      content: `# Architecture Decision Records (${adrs.length} found)\n\n${lines.join('\n\n---\n\n')}`,
    };
  }

  private fetchFAQs(
    toolCallId: string,
    kb: ProjectKnowledgeBase,
    query?: string
  ): ToolResult {
    let faqs = kb.getFAQs();
    if (query) {
      faqs = faqs.filter(faq =>
        faq.question.toLowerCase().includes(query) ||
        faq.answer.toLowerCase().includes(query) ||
        faq.tags.some(t => t.toLowerCase().includes(query))
      );
    }

    if (faqs.length === 0) {
      return {
        toolCallId,
        content: query
          ? `No FAQs match query "${query}".`
          : 'No FAQs found in the project knowledge base.',
      };
    }

    const lines = faqs.map(faq =>
      `**Q**: ${faq.question}\n**A**: ${faq.answer}`
    );

    return {
      toolCallId,
      content: `# FAQs (${faqs.length} found)\n\n${lines.join('\n\n---\n\n')}`,
    };
  }

  private fetchLearnings(
    toolCallId: string,
    kb: ProjectKnowledgeBase,
    query?: string
  ): ToolResult {
    let learnings = kb.getLearnings();
    if (query) {
      learnings = learnings.filter(l =>
        l.content.toLowerCase().includes(query) ||
        l.context.toLowerCase().includes(query)
      );
    }

    if (learnings.length === 0) {
      return {
        toolCallId,
        content: query
          ? `No learnings match query "${query}".`
          : 'No learnings found in the project knowledge base.',
      };
    }

    const lines = learnings.map(l =>
      `### ${l.id}\n**Context**: ${l.context}\n**Insight**: ${l.content}`
    );

    return {
      toolCallId,
      content: `# Past Learnings (${learnings.length} found)\n\n${lines.join('\n\n---\n\n')}`,
    };
  }

  private fetchAll(
    toolCallId: string,
    kb: ProjectKnowledgeBase,
    query?: string
  ): ToolResult {
    const sections: string[] = [];

    const adrs = kb.getADRs({ status: 'accepted' });
    if (adrs.length > 0) {
      const filtered = query
        ? adrs.filter(a => a.title.toLowerCase().includes(query!) || a.decision.toLowerCase().includes(query!))
        : adrs;
      if (filtered.length > 0) {
        sections.push(`## ADRs (${filtered.length})\n${filtered.map(a => `- [${a.id}] ${a.title}`).join('\n')}`);
      }
    }

    const faqs = kb.getFAQs();
    if (faqs.length > 0) {
      const filtered = query
        ? faqs.filter(f => f.question.toLowerCase().includes(query!) || f.tags.some(t => t.toLowerCase().includes(query!)))
        : faqs;
      if (filtered.length > 0) {
        sections.push(`## FAQs (${filtered.length})\n${filtered.map(f => `- [${f.id}] ${f.question}`).join('\n')}`);
      }
    }

    const learnings = kb.getLearnings();
    if (learnings.length > 0) {
      const filtered = query
        ? learnings.filter(l => l.content.toLowerCase().includes(query!))
        : learnings;
      if (filtered.length > 0) {
        sections.push(`## Learnings (${filtered.length})\n${filtered.map(l => `- [${l.id}] ${l.content.substring(0, 80)}`).join('\n')}`);
      }
    }

    if (sections.length === 0) {
      return {
        toolCallId,
        content: query
          ? `No knowledge base entries match query "${query}".`
          : 'Project knowledge base is empty.',
      };
    }

    return {
      toolCallId,
      content: `# Project Knowledge Base Index\n\nUse fetch_project_guidelines with a specific ID to get full details.\n\n${sections.join('\n\n')}`,
    };
  }

  // ============================================================================
  // 格式化方法
  // ============================================================================

  private formatADRDetail(adr: ADRRecord): string {
    const parts = [
      `# ADR: ${adr.title}`,
      `**ID**: ${adr.id}`,
      `**Status**: ${adr.status}`,
      `**Date**: ${new Date(adr.date).toISOString().split('T')[0]}`,
      '',
      `## Context\n${adr.context}`,
      `## Decision\n${adr.decision}`,
      `## Consequences\n${adr.consequences}`,
    ];

    if (adr.alternatives && adr.alternatives.length > 0) {
      parts.push(`## Alternatives Considered\n${adr.alternatives.map(a => `- ${a}`).join('\n')}`);
    }
    if (adr.relatedFiles && adr.relatedFiles.length > 0) {
      parts.push(`## Related Files\n${adr.relatedFiles.map(f => `- ${f}`).join('\n')}`);
    }

    return parts.join('\n');
  }

  private formatFAQDetail(faq: FAQRecord): string {
    const parts = [
      `# FAQ: ${faq.question}`,
      `**ID**: ${faq.id}`,
      `**Category**: ${faq.category}`,
      `**Tags**: ${faq.tags.join(', ')}`,
      '',
      `## Answer\n${faq.answer}`,
    ];

    if (faq.relatedFiles && faq.relatedFiles.length > 0) {
      parts.push(`## Related Files\n${faq.relatedFiles.map(f => `- ${f}`).join('\n')}`);
    }

    return parts.join('\n');
  }

  private formatLearningDetail(learning: LearningRecord): string {
    return [
      `# Learning: ${learning.id}`,
      `**Date**: ${new Date(learning.createdAt).toISOString().split('T')[0]}`,
      '',
      `## Context\n${learning.context}`,
      `## Insight\n${learning.content}`,
    ].join('\n');
  }
}

