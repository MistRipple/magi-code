/**
 * Web 执行器
 * 提供网络搜索和内容获取功能
 *
 * 工具: web_search, web_fetch
 */

import { ToolExecutor, ExtendedToolDefinition } from './types';
import { ToolCall, ToolResult } from '../llm/types';
import { logger, LogCategory } from '../logging';

const BROWSER_UA = 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36';

/**
 * Web 执行器
 */
export class WebExecutor implements ToolExecutor {
  constructor() {}

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
      description: `Fetch and extract content from a URL as Markdown.

Use for:
* Reading documentation pages
* Analyzing API references
* Extracting code examples from web pages
* Understanding error messages from links

Tips:
* HTML pages are converted to clean Markdown with structure preserved
* Code blocks, headings, lists, links, and tables are retained
* Large pages are automatically truncated
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
            description: 'Optional prompt to describe what information you need from the page'
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
  async execute(toolCall: ToolCall, signal?: AbortSignal): Promise<ToolResult> {
    const { name } = toolCall;

    logger.debug('WebExecutor executing', { tool: name }, LogCategory.TOOLS);

    try {
      switch (name) {
        case 'web_search':
          return await this.executeWebSearch(toolCall, signal);
        case 'web_fetch':
          return await this.executeWebFetch(toolCall, signal);
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
  private async executeWebSearch(toolCall: ToolCall, signal?: AbortSignal): Promise<ToolResult> {
    const { query } = toolCall.arguments as { query: string };

    if (!query) {
      return {
        toolCallId: toolCall.id,
        content: 'Error: query is required',
        isError: true
      };
    }

    logger.info('Web search', { query }, LogCategory.TOOLS);

    try {
      const searchUrl = `https://html.duckduckgo.com/html/?q=${encodeURIComponent(query)}`;
      const response = await fetch(searchUrl, {
        headers: {
          'User-Agent': BROWSER_UA,
          'Accept': 'text/html,application/xhtml+xml',
          'Accept-Language': 'en-US,en;q=0.9,zh-CN;q=0.8,zh;q=0.7',
        },
        redirect: 'follow',
        signal: signal ? AbortSignal.any([signal, AbortSignal.timeout(15000)]) : AbortSignal.timeout(15000),
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
          content: `No search results found for "${query}"`,
          isError: false
        };
      }

      const formatted = results
        .slice(0, 10)
        .map((r, i) => `${i + 1}. **${r.title}**\n   ${r.url}\n   ${r.snippet}`)
        .join('\n\n');

      return {
        toolCallId: toolCall.id,
        content: `Search results for "${query}":\n\n${formatted}`,
        isError: false
      };
    } catch (error: any) {
      if (error.name === 'TimeoutError') {
        return {
          toolCallId: toolCall.id,
          content: 'Search timed out (15s). Try a simpler query.',
          isError: true
        };
      }
      return {
        toolCallId: toolCall.id,
        content: `Search error: ${error.message}`,
        isError: true
      };
    }
  }

  /**
   * 解析 DuckDuckGo HTML 搜索结果
   */
  private parseSearchResults(html: string): Array<{ title: string; url: string; snippet: string }> {
    const results: Array<{ title: string; url: string; snippet: string }> = [];

    // DuckDuckGo HTML 版结果块：每个结果在 <div class="result ..."> 内
    // 标题链接: <a class="result__a" href="...">Title</a>
    // 摘要: <a class="result__snippet" href="...">Snippet text</a>
    // 改进正则：snippet 内部可能含有 HTML 标签（如 <b>）
    const resultBlockRegex = /<a[^>]+class="result__a"[^>]*href="([^"]+)"[^>]*>([\s\S]*?)<\/a>[\s\S]*?<a[^>]+class="result__snippet"[^>]*>([\s\S]*?)<\/a>/gi;

    let match;
    while ((match = resultBlockRegex.exec(html)) !== null) {
      const rawUrl = match[1];
      const rawTitle = match[2];
      const rawSnippet = match[3];

      // 解码 DuckDuckGo 跳转 URL
      const url = this.decodeDuckDuckGoUrl(rawUrl);
      const title = this.stripHtmlTags(this.decodeHtml(rawTitle)).trim();
      const snippet = this.stripHtmlTags(this.decodeHtml(rawSnippet)).trim();

      if (url && title) {
        results.push({ url, title, snippet });
      }
    }

    return results;
  }

  /**
   * 解码 DuckDuckGo 跳转 URL
   * DuckDuckGo HTML 版的链接格式: //duckduckgo.com/l/?uddg=ENCODED_URL&rut=...
   */
  private decodeDuckDuckGoUrl(rawUrl: string): string {
    if (rawUrl.includes('duckduckgo.com/l/?')) {
      try {
        const urlObj = new URL(rawUrl, 'https://duckduckgo.com');
        const actualUrl = urlObj.searchParams.get('uddg');
        if (actualUrl) return actualUrl;
      } catch {
        // 解析失败则使用原始 URL
      }
    }
    // 补全协议
    if (rawUrl.startsWith('//')) return `https:${rawUrl}`;
    return rawUrl;
  }

  /**
   * 执行 URL 内容获取
   */
  private async executeWebFetch(toolCall: ToolCall, signal?: AbortSignal): Promise<ToolResult> {
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
          'User-Agent': BROWSER_UA,
          'Accept': 'text/html,application/xhtml+xml,application/xml;q=0.9,application/json;q=0.8,*/*;q=0.7',
          'Accept-Language': 'en-US,en;q=0.9,zh-CN;q=0.8,zh;q=0.7',
        },
        redirect: 'follow',
        signal: signal ? AbortSignal.any([signal, AbortSignal.timeout(30000)]) : AbortSignal.timeout(30000),
      });

      if (!response.ok) {
        return {
          toolCallId: toolCall.id,
          content: `Fetch failed: HTTP ${response.status} ${response.statusText}`,
          isError: true
        };
      }

      const contentType = response.headers.get('content-type') || '';
      let content: string;

      if (contentType.includes('application/json')) {
        const json = await response.json();
        content = '```json\n' + JSON.stringify(json, null, 2) + '\n```';
      } else if (contentType.includes('text/plain')) {
        content = await response.text();
      } else {
        // HTML → Markdown（轻量转换）
        const html = await response.text();
        content = this.htmlToMarkdown(html);
      }

      // 截断过长的内容
      const maxLength = 50000;
      if (content.length > maxLength) {
        content = content.substring(0, maxLength) + '\n\n---\n*[Content truncated at 50,000 characters]*';
      }

      const header = prompt
        ? `**URL**: ${url}\n**Prompt**: ${prompt}\n\n---\n\n`
        : `**URL**: ${url}\n\n---\n\n`;

      return {
        toolCallId: toolCall.id,
        content: header + content,
        isError: false
      };
    } catch (error: any) {
      if (error.name === 'TimeoutError') {
        return {
          toolCallId: toolCall.id,
          content: `Fetch timed out (30s) for ${url}`,
          isError: true
        };
      }
      return {
        toolCallId: toolCall.id,
        content: `Fetch error: ${error.message}`,
        isError: true
      };
    }
  }

  /**
   * 将 HTML 转换为 Markdown
   * 采用轻量规则，优先保留标题、代码、链接、列表和段落。
   */
  private htmlToMarkdown(html: string): string {
    const mainContent = this.extractMainContent(html);
    const withoutNoise = mainContent
      .replace(/<script[\s\S]*?<\/script>/gi, '')
      .replace(/<style[\s\S]*?<\/style>/gi, '')
      .replace(/<noscript[\s\S]*?<\/noscript>/gi, '')
      .replace(/<(nav|footer|header|aside|iframe)[^>]*>[\s\S]*?<\/\1>/gi, '');

    const markdown = withoutNoise
      .replace(/<h([1-6])[^>]*>([\s\S]*?)<\/h\1>/gi, (_m, level, text) => {
        const hashes = '#'.repeat(Number(level));
        return `\n${hashes} ${this.stripHtmlTags(this.decodeHtml(text)).trim()}\n`;
      })
      .replace(/<pre[^>]*>\s*<code[^>]*>([\s\S]*?)<\/code>\s*<\/pre>/gi, (_m, code) => {
        return `\n\`\`\`\n${this.decodeHtml(code).trim()}\n\`\`\`\n`;
      })
      .replace(/<code[^>]*>([\s\S]*?)<\/code>/gi, (_m, code) => `\`${this.decodeHtml(code).trim()}\``)
      .replace(/<a[^>]*href="([^"]+)"[^>]*>([\s\S]*?)<\/a>/gi, (_m, href, text) => {
        const title = this.stripHtmlTags(this.decodeHtml(text)).trim() || href;
        return `[${title}](${href})`;
      })
      .replace(/<li[^>]*>([\s\S]*?)<\/li>/gi, (_m, text) => `\n- ${this.stripHtmlTags(this.decodeHtml(text)).trim()}`)
      .replace(/<br\s*\/?>/gi, '\n')
      .replace(/<\/p>/gi, '\n\n')
      .replace(/<p[^>]*>/gi, '')
      .replace(/<\/div>/gi, '\n')
      .replace(/<div[^>]*>/gi, '')
      .replace(/<[^>]+>/g, '')
      .replace(/\n{3,}/g, '\n\n');

    return this.decodeHtml(markdown).trim();
  }

  /**
   * 提取页面主体内容
   * 优先级: <main> > <article> > <div role="main"> > <body> > 全部
   */
  private extractMainContent(html: string): string {
    // 尝试提取 <main> 标签内容
    const mainMatch = html.match(/<main[^>]*>([\s\S]*?)<\/main>/i);
    if (mainMatch) return mainMatch[1];

    // 尝试提取 <article> 标签内容
    const articleMatch = html.match(/<article[^>]*>([\s\S]*?)<\/article>/i);
    if (articleMatch) return articleMatch[1];

    // 尝试提取 role="main" 元素
    const roleMainMatch = html.match(/<div[^>]+role="main"[^>]*>([\s\S]*?)<\/div>/i);
    if (roleMainMatch) return roleMainMatch[1];

    // 尝试提取 <body>
    const bodyMatch = html.match(/<body[^>]*>([\s\S]*?)<\/body>/i);
    if (bodyMatch) return bodyMatch[1];

    return html;
  }

  /**
   * 移除 HTML 标签（用于搜索结果解析）
   */
  private stripHtmlTags(text: string): string {
    return text.replace(/<[^>]+>/g, '');
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
      '&#x3D;': '=',
      '&apos;': "'",
      '&mdash;': '—',
      '&ndash;': '–',
      '&hellip;': '…',
      '&laquo;': '«',
      '&raquo;': '»',
      '&bull;': '•',
      '&copy;': '©',
      '&reg;': '®',
      '&trade;': '™',
    };

    // 命名实体
    let result = html.replace(/&[a-zA-Z]+;/g, entity => entities[entity] || entity);
    // 十进制数字实体 &#123;
    result = result.replace(/&#(\d+);/g, (_, code) => {
      const num = parseInt(code, 10);
      return num > 0 && num < 0x10FFFF ? String.fromCodePoint(num) : '';
    });
    // 十六进制数字实体 &#x1F4A9;
    result = result.replace(/&#x([0-9a-fA-F]+);/g, (_, code) => {
      const num = parseInt(code, 16);
      return num > 0 && num < 0x10FFFF ? String.fromCodePoint(num) : '';
    });

    return result;
  }
}
