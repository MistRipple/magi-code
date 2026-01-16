/**
 * 内容解析器模块
 * 将 CLI 原始输出解析为统一的 ContentBlock 格式
 *
 * 设计目标：
 * 1. 后端统一处理所有 CLI 输出格式
 * 2. 解析为结构化的 ContentBlock
 * 3. 前端负责渲染（使用 marked + highlight.js）
 */

import { ContentBlock } from '../protocol/message-protocol';

// 重新导出类型，方便其他模块使用
export type { ContentBlock };

/**
 * 移除 ANSI 转义序列（CLI 输出的颜色代码）
 */
export function stripAnsi(text: string): string {
  return String(text).replace(/\x1b\[[0-9;]*[a-zA-Z]/g, '');
}

/**
 * 移除零宽字符（可能导致复制粘贴问题）
 */
export function stripZeroWidth(text: string): string {
  return String(text).replace(/[\u200B-\u200D\uFEFF]/g, '');
}

/**
 * 预处理 CLI 输出内容
 * 清理 ANSI 转义序列、零宽字符等
 */
export function sanitizeCliOutput(text: string): string {
  if (!text) return '';
  let result = String(text);
  // 1. 移除 ANSI 转义序列
  result = stripAnsi(result);
  // 2. 移除零宽字符
  result = stripZeroWidth(result);
  // 3. 规范化换行符
  result = result.replace(/\r\n/g, '\n').replace(/\r/g, '\n');
  return result;
}

/**
 * 折叠多余的空行
 */
export function collapseExtraBlankLines(text: string): string {
  if (!text) return '';
  const normalized = String(text).replace(/\r\n/g, '\n');
  // 保留代码块内的空行
  const parts = normalized.split(/```/);
  return parts.map((part, idx) => {
    if (idx % 2 === 1) return part; // 代码块内部不处理
    return part.replace(/\n{3,}/g, '\n\n');
  }).join('```');
}

/**
 * 规范化纯文本
 */
export function normalizePlainText(text: string): string {
  if (!text) return '';
  return String(text)
    .replace(/<br\s*\/?>/gi, '\n')
    .replace(/\\r\\n/g, '\n')
    .replace(/\\n/g, '\n')
    .replace(/\\r/g, '\n')
    .replace(/\\t/g, '\t');
}

/**
 * 提取 JSON 信息
 */
export function extractJsonInfo(content: string): { isJson: boolean; jsonText: string } {
  if (!content) return { isJson: false, jsonText: '' };
  const trimmed = content.trim();
  if (!trimmed) return { isJson: false, jsonText: '' };
  if (!(trimmed.startsWith('{') || trimmed.startsWith('['))) {
    return { isJson: false, jsonText: '' };
  }
  try {
    JSON.parse(trimmed);
    return { isJson: true, jsonText: trimmed };
  } catch {
    return { isJson: false, jsonText: '' };
  }
}

/**
 * 提取单个代码块
 */
export function extractSingleCodeFence(content: string): { lang: string; body: string; filepath?: string } | null {
  if (!content) return null;
  const trimmed = content.trim();
  // 匹配 ```lang:filepath 或 ```lang filepath 或 ```lang
  const match = trimmed.match(/^```(\w*)(?::([^\s\n]+)|\s+([^\n]+))?\s*\n([\s\S]*?)\n?```\s*$/);
  if (!match) return null;
  const lang = match[1] || '';
  const filepath = match[2] || match[3] || undefined;
  const body = match[4] || '';
  return { lang, body, filepath };
}

/**
 * 判断是否应该渲染为代码块
 */
export function shouldRenderAsCodeBlock(content: string): boolean {
  if (!content) return false;
  const trimmed = content.trim();
  if (!trimmed) return false;
  if (trimmed.startsWith('```')) return false;
  // JSON 格式
  if (trimmed.startsWith('{') || trimmed.startsWith('[')) return true;
  if (!content.includes('\n')) return false;
  // 带行号的输出
  if (/^\s*\d+→/m.test(content)) return true;
  if (/^\s*\d+\s*[:|>]/m.test(content)) return true;
  if (/^\s*\d+\s*-\s+/m.test(content)) return true;
  if (/^\s*\d+\)\s+/m.test(content)) return true;
  // 缩进代码
  return /^(\s{2,}|\t)/m.test(content);
}

/**
 * 检测内容是否包含 Markdown 语法
 */
export function hasMarkdownSyntax(content: string): boolean {
  if (!content) return false;
  // 代码块、标题、列表、粗体、引用、分隔线
  return /```|^#{1,3} |^\* |\*\*|^\d+\. |^> |^---$/m.test(content);
}

/**
 * 检测是否为任务摘要
 */
export function isSummaryContent(content: string): boolean {
  if (!content) return false;
  return content.includes('执行完成') ||
         content.includes('任务完成') ||
         content.includes('已完成');
}

/**
 * 检测是否为任务分析
 */
export function isTaskContent(content: string): boolean {
  if (!content) return false;
  return content.includes('任务分析') || content.includes('执行计划');
}

/**
 * 检测是否为进度信息
 */
export function isProgressContent(content: string): boolean {
  if (!content) return false;
  return content.includes('正在') ||
         content.includes('开始') ||
         content.includes('处理中');
}

/**
 * 检测是否为错误信息
 */
export function isErrorContent(content: string): boolean {
  if (!content) return false;
  const lower = content.toLowerCase();
  return lower.includes('error') ||
         lower.includes('错误') ||
         lower.includes('失败') ||
         lower.includes('exception');
}

/**
 * 提取代码块（支持多个）
 */
export function extractCodeBlocks(content: string): Array<{
  lang: string;
  code: string;
  filepath?: string;
  startIndex: number;
  endIndex: number;
}> {
  const blocks: Array<{
    lang: string;
    code: string;
    filepath?: string;
    startIndex: number;
    endIndex: number;
  }> = [];

  // 匹配 ```lang:filepath 或 ```lang filepath 或 ```lang
  const regex = /```(\w*)(?::([^\s\n]+)|\s+([^\n]*))?\n([\s\S]*?)```/g;
  let match;

  while ((match = regex.exec(content)) !== null) {
    blocks.push({
      lang: match[1] || 'text',
      filepath: match[2] || match[3]?.trim() || undefined,
      code: match[4] || '',
      startIndex: match.index,
      endIndex: match.index + match[0].length,
    });
  }

  return blocks;
}

/**
 * 核心函数：将原始内容解析为 ContentBlock 数组
 * 这是后端统一处理 CLI 输出的入口
 */
export function parseContentToBlocks(
  rawContent: string,
  options?: {
    toolCalls?: Array<{ name: string; input: unknown; status?: string }>;
    source?: string;
  }
): ContentBlock[] {
  if (!rawContent) return [];

  // 1. 预处理：清理 ANSI、零宽字符等
  const sanitized = sanitizeCliOutput(rawContent);
  const content = collapseExtraBlankLines(sanitized);
  const trimmed = content.trim();

  if (!trimmed) return [];

  const blocks: ContentBlock[] = [];

  // 2. 提取代码块
  const codeBlocks = extractCodeBlocks(content);

  if (codeBlocks.length > 0) {
    // 有代码块，需要分段处理
    let lastIndex = 0;

    for (const codeBlock of codeBlocks) {
      // 代码块之前的文本
      if (codeBlock.startIndex > lastIndex) {
        const textBefore = content.slice(lastIndex, codeBlock.startIndex).trim();
        if (textBefore) {
          blocks.push(...parseTextContent(textBefore));
        }
      }

      // 代码块本身
      const lang = codeBlock.lang || 'text';
      blocks.push({
        type: 'code',
        content: codeBlock.code,
        language: lang,
        filename: codeBlock.filepath,
      } as ContentBlock);

      lastIndex = codeBlock.endIndex;
    }

    // 最后一个代码块之后的文本
    if (lastIndex < content.length) {
      const textAfter = content.slice(lastIndex).trim();
      if (textAfter) {
        blocks.push(...parseTextContent(textAfter));
      }
    }
  } else {
    // 没有代码块，直接解析文本
    blocks.push(...parseTextContent(trimmed));
  }

  // 3. 添加工具调用块
  if (options?.toolCalls && options.toolCalls.length > 0) {
    for (const tool of options.toolCalls) {
      blocks.push({
        type: 'tool_call',
        toolName: tool.name,
        toolId: `tool-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`,
        status: (tool.status as 'pending' | 'running' | 'completed' | 'failed') || 'completed',
        input: typeof tool.input === 'object' ? tool.input as Record<string, unknown> : { value: tool.input },
      } as ContentBlock);
    }
  }

  return blocks;
}

/**
 * 解析纯文本内容（不含代码块）
 * 返回与 message-protocol.ts 兼容的 ContentBlock 类型
 */
function parseTextContent(text: string): ContentBlock[] {
  if (!text) return [];
  const trimmed = text.trim();
  if (!trimmed) return [];

  // 检测内容类型
  const jsonInfo = extractJsonInfo(trimmed);

  // JSON 内容 -> 作为代码块
  if (jsonInfo.isJson) {
    return [{
      type: 'code',
      content: jsonInfo.jsonText,
      language: 'json',
    } as ContentBlock];
  }

  // Markdown 内容
  if (hasMarkdownSyntax(trimmed)) {
    return [{
      type: 'text',
      content: trimmed,
      isMarkdown: true,
    } as ContentBlock];
  }

  // 应该渲染为代码块的内容（如带行号的输出）
  if (shouldRenderAsCodeBlock(trimmed)) {
    return [{
      type: 'code',
      content: trimmed,
      language: 'text',
    } as ContentBlock];
  }

  // 普通文本
  const normalizedText = normalizePlainText(trimmed);
  return [{
    type: 'text',
    content: normalizedText,
    isMarkdown: false,
  } as ContentBlock];
}
