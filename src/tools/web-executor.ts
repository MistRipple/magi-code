/**
 * Web 执行器
 * 提供网络搜索和内容获取功能
 *
 * 工具: web_search, web_fetch
 */

import { ToolExecutor, ExtendedToolDefinition } from './types';
import { ToolCall, ToolResult } from '../llm/types';
import { logger, LogCategory } from '../logging';

/**
 * Web 执行器
 */
export class WebExecutor implements ToolExecutor {
  constructor() {
    // Web 执行器不需要工作区路径
  }

  /**
   * 获取所有工具定义
   */
  getToolDefinitions(): ExtendedToolDefinition[] {
    return [
      this.getWebSearchDefinition(),
      this.getWebFetchDefinition()
    ];
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
    return toolName === 'web_search' || toolName === 'web_fetch';
  }

  /**
   * web_search 工具定义
   */
  private getWebSearchDefinition(): ExtendedToolDefinition {
    return {
      name: 'web_search',
      description: `Search the web for information.

Use for:
* Finding documentation and API references
* Looking up current events or recent information
* Searching for code examples and solutions
* Verifying facts and specifications

Tips:
* Use specific, well-formed queries
* Include version numbers when searching for docs
* Results are summarized for context efficiency`,
      input_schema: {
        type: 'object',
        properties: {
          query: {
            type: 'string',
            description: 'The search query to execute'
          }
        },
        required: ['query']
      },
      metadata: {
        source: 'builtin',
        category: 'web',
        tags: ['web', 'search', 'internet']
      }
    };
  }

  /**
   * web_fetch 工具定义
   */
  private getWebFetchDefinition(): ExtendedToolDefinition {
    return {
      name: 'web_fetch',
      description: `Fetch and analyze content from a URL.

Use for:
* Reading documentation pages
* Analyzing API references
* Extracting code examples
* Understanding error messages from links

Tips:
* Provide a clear prompt to guide content extraction
* Large pages are automatically summarized
* Works best with public, accessible URLs`,
      input_schema: {
        type: 'object',
        properties: {
          url: {
            type: 'string',
            description: 'The URL to fetch content from'
          },
          prompt: {
            type: 'string',
            description: 'Optional prompt to guide content extraction'
          }
        },
        required: ['url']
      },
      metadata: {
        source: 'builtin',
        category: 'web',
        tags: ['web', 'fetch', 'url']
      }
    };
  }

  /**
   * 执行工具调用
   */
  async execute(toolCall: ToolCall): Promise<ToolResult> {
    const { name } = toolCall;

    logger.debug('WebExecutor executing', { tool: name }, LogCategory.TOOLS);

    try {
      switch (name) {
        case 'web_search':
          return await this.executeWebSearch(toolCall);
        case 'web_fetch':
          return await this.executeWebFetch(toolCall);
        default:
          return {
            toolCallId: toolCall.id,
            content: `Error: unknown tool ${name}`,
            isError: true
          };
      }
    } catch (error: any) {
      logger.error('WebExecutor error', { tool: name, error: error.message }, LogCategory.TOOLS);
      return {
        toolCallId: toolCall.id,
        content: `Error: ${error.message}`,
        isError: true
      };
    }
  }

  /**
   * 执行网络搜索
   */
  private async executeWebSearch(toolCall: ToolCall): Promise<ToolResult> {
    const { query } = toolCall.arguments as { query: string };

    if (!query) {
      return {
        toolCallId: toolCall.id,
        content: 'Error: query is required',
        isError: true
      };
    }

    logger.info('Web search', { query }, LogCategory.TOOLS);

    // 使用 DuckDuckGo 搜索（无需 API key）
    try {
      const searchUrl = `https://html.duckduckgo.com/html/?q=${encodeURIComponent(query)}`;
      const response = await fetch(searchUrl, {
        headers: {
          'User-Agent': 'Mozilla/5.0 (compatible; MultiCLI/1.0)',
          'Accept': 'text/html'
        }
      });

      if (!response.ok) {
        return {
          toolCallId: toolCall.id,
          content: `Search failed: HTTP ${response.status}`,
          isError: true
        };
      }

      const html = await response.text();
      const results = this.parseSearchResults(html);

      if (results.length === 0) {
        return {
          toolCallId: toolCall.id,
          content: 'No search results found',
          isError: false
        };
      }

      const formatted = results
        .slice(0, 10)
        .map((r, i) => `${i + 1}. ${r.title}\n   ${r.url}\n   ${r.snippet}`)
        .join('\n\n');

      return {
        toolCallId: toolCall.id,
        content: `Search results for "${query}":\n\n${formatted}`,
        isError: false
      };
    } catch (error: any) {
      return {
        toolCallId: toolCall.id,
        content: `Search error: ${error.message}`,
        isError: true
      };
    }
  }

  /**
   * 解析搜索结果
   */
  private parseSearchResults(html: string): Array<{ title: string; url: string; snippet: string }> {
    const results: Array<{ title: string; url: string; snippet: string }> = [];

    // 简单的 HTML 解析（提取搜索结果）
    const resultRegex = /<a[^>]+class="result__a"[^>]*href="([^"]+)"[^>]*>([^<]+)<\/a>[\s\S]*?<a[^>]+class="result__snippet"[^>]*>([^<]+)<\/a>/gi;

    let match;
    while ((match = resultRegex.exec(html)) !== null) {
      results.push({
        url: match[1],
        title: this.decodeHtml(match[2].trim()),
        snippet: this.decodeHtml(match[3].trim())
      });
    }

    // 备用解析（简化版）
    if (results.length === 0) {
      const linkRegex = /<a[^>]+href="(https?:\/\/[^"]+)"[^>]*>([^<]{10,})<\/a>/gi;
      while ((match = linkRegex.exec(html)) !== null && results.length < 10) {
        if (!match[1].includes('duckduckgo.com')) {
          results.push({
            url: match[1],
            title: this.decodeHtml(match[2].trim()),
            snippet: ''
          });
        }
      }
    }

    return results;
  }

  /**
   * 执行 URL 内容获取
   */
  private async executeWebFetch(toolCall: ToolCall): Promise<ToolResult> {
    const { url, prompt } = toolCall.arguments as { url: string; prompt?: string };

    if (!url) {
      return {
        toolCallId: toolCall.id,
        content: 'Error: url is required',
        isError: true
      };
    }

    // 验证 URL
    try {
      new URL(url);
    } catch {
      return {
        toolCallId: toolCall.id,
        content: 'Error: invalid URL format',
        isError: true
      };
    }

    logger.info('Web fetch', { url }, LogCategory.TOOLS);

    try {
      const response = await fetch(url, {
        headers: {
          'User-Agent': 'Mozilla/5.0 (compatible; MultiCLI/1.0)',
          'Accept': 'text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8'
        },
        signal: AbortSignal.timeout(30000) // 30秒超时
      });

      if (!response.ok) {
        return {
          toolCallId: toolCall.id,
          content: `Fetch failed: HTTP ${response.status}`,
          isError: true
        };
      }

      const contentType = response.headers.get('content-type') || '';
      let content: string;

      if (contentType.includes('application/json')) {
        const json = await response.json();
        content = JSON.stringify(json, null, 2);
      } else {
        const html = await response.text();
        content = this.extractTextFromHtml(html);
      }

      // 截断过长的内容
      const maxLength = 50000;
      if (content.length > maxLength) {
        content = content.substring(0, maxLength) + '\n\n[Content truncated]';
      }

      const result = prompt
        ? `URL: ${url}\nPrompt: ${prompt}\n\nContent:\n${content}`
        : `URL: ${url}\n\nContent:\n${content}`;

      return {
        toolCallId: toolCall.id,
        content: result,
        isError: false
      };
    } catch (error: any) {
      return {
        toolCallId: toolCall.id,
        content: `Fetch error: ${error.message}`,
        isError: true
      };
    }
  }

  /**
   * 从 HTML 提取文本内容
   */
  private extractTextFromHtml(html: string): string {
    // 移除脚本和样式
    let text = html
      .replace(/<script[^>]*>[\s\S]*?<\/script>/gi, '')
      .replace(/<style[^>]*>[\s\S]*?<\/style>/gi, '')
      .replace(/<noscript[^>]*>[\s\S]*?<\/noscript>/gi, '');

    // 移除 HTML 标签
    text = text.replace(/<[^>]+>/g, ' ');

    // 解码 HTML 实体
    text = this.decodeHtml(text);

    // 清理空白
    text = text
      .replace(/\s+/g, ' ')
      .replace(/\n\s*\n/g, '\n')
      .trim();

    return text;
  }

  /**
   * 解码 HTML 实体
   */
  private decodeHtml(html: string): string {
    const entities: Record<string, string> = {
      '&amp;': '&',
      '&lt;': '<',
      '&gt;': '>',
      '&quot;': '"',
      '&#39;': "'",
      '&nbsp;': ' ',
      '&#x27;': "'",
      '&#x2F;': '/',
      '&#x60;': '`',
      '&#x3D;': '='
    };

    return html.replace(/&[^;]+;/g, entity => entities[entity] || entity);
  }
}
