/**
 * 内容解析器模块
 * 将模型原始输出解析为统一的 ContentBlock 格式
 *
 * 设计目标：
 * 1. 后端统一处理所有 Agent 输出格式
 * 2. 解析为结构化的 ContentBlock
 * 3. 前端负责渲染（使用 marked + highlight.js）
 */

import { ContentBlock, PlanBlock } from '../protocol/message-protocol';

// 重新导出类型，方便其他模块使用
export type { ContentBlock, PlanBlock };

/**
 * 移除 ANSI 转义序列（Agent 输出的颜色代码）
 * 🔧 增强：支持更多类型的 ANSI 序列
 */
export function stripAnsi(text: string): string {
  return String(text)
    // CSI 序列 (Control Sequence Introducer): ESC [ ... 字母
    .replace(/\x1b\[[0-9;]*[a-zA-Z]/g, '')
    // OSC 序列 (Operating System Command): ESC ] ... BEL 或 ESC \
    .replace(/\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)/g, '')
    // 单字符转义序列: ESC 后跟单个字符
    .replace(/\x1b[NOPXZn\\^_@]/g, '')
    // DCS/PM/APC 序列: ESC P/^/_ ... ST
    .replace(/\x1b[P^_][^\x1b]*\x1b\\/g, '')
    // 简单的 ESC 后跟一个字符
    .replace(/\x1b./g, '');
}

/**
 * 移除零宽字符（可能导致复制粘贴问题）
 * 🔧 增强：扩展处理范围
 */
export function stripZeroWidth(text: string): string {
  return String(text).replace(
    /[\u200B-\u200F\u2028-\u202F\u2060-\u206F\uFEFF\u00AD\u180E]/g,
    ''
  );
}

/**
 * 预处理 Agent 输出内容
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
 * 提取内容中的裸露 JSON 对象（不在代码块中的 JSON）
 * 🔧 新增：处理 AI 响应中混合的 JSON 对象
 */
export function extractEmbeddedJson(content: string): Array<{
  jsonText: string;
  startIndex: number;
  endIndex: number;
}> {
  const results: Array<{ jsonText: string; startIndex: number; endIndex: number }> = [];

  // 匹配 JSON 对象或数组的正则（简化版，匹配大括号或方括号）
  // 从 { 或 [ 开始，找到匹配的结束符
  let i = 0;
  while (i < content.length) {
    const char = content[i];

    // 跳过代码块中的内容
    if (content.substring(i, i + 3) === '```') {
      const endIndex = content.indexOf('```', i + 3);
      if (endIndex !== -1) {
        i = endIndex + 3;
        continue;
      }
    }

    if (char === '{' || char === '[') {
      // 尝试提取 JSON
      const extracted = tryExtractJsonAt(content, i);
      if (extracted) {
        results.push(extracted);
        i = extracted.endIndex;
        continue;
      }
    }
    i++;
  }

  return results;
}

/**
 * 尝试从指定位置提取 JSON
 */
function tryExtractJsonAt(content: string, startIndex: number): { jsonText: string; startIndex: number; endIndex: number } | null {
  const startChar = content[startIndex];
  const endChar = startChar === '{' ? '}' : ']';
  let depth = 0;
  let inString = false;
  let escapeNext = false;

  for (let i = startIndex; i < content.length; i++) {
    const char = content[i];

    if (escapeNext) {
      escapeNext = false;
      continue;
    }

    if (char === '\\') {
      escapeNext = true;
      continue;
    }

    if (char === '"') {
      inString = !inString;
      continue;
    }

    if (inString) continue;

    if (char === startChar) {
      depth++;
    } else if (char === endChar) {
      depth--;
      if (depth === 0) {
        // 找到匹配的结束符
        const jsonText = content.substring(startIndex, i + 1);
        try {
          JSON.parse(jsonText);
          return {
            jsonText,
            startIndex,
            endIndex: i + 1,
          };
        } catch {
          return null;
        }
      }
    }
  }

  return null;
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
 * 🔧 修复：避免误判 Markdown 有序列表
 */
export function shouldRenderAsCodeBlock(content: string): boolean {
  if (!content) return false;
  const trimmed = content.trim();
  if (!trimmed) return false;
  // 已经是代码块围栏格式
  if (trimmed.startsWith('```')) return false;
  // JSON 格式
  if (trimmed.startsWith('{') || trimmed.startsWith('[')) return true;
  // 单行内容不作为代码块
  if (!content.includes('\n')) return false;

  // 🔧 检测带行号的特殊输出格式（Agent 工具输出）
  // 注意：避免误判 Markdown 有序列表

  // 特殊行号格式：数字→（如 "1→ code"）
  if (/^\s*\d+→/m.test(content)) return true;
  // 行号格式：数字: 或 数字> （如 "1: code" 或 "1> code"）
  if (/^\s*\d+\s*[:>]/m.test(content)) return true;

  // 🔧 移除误判有序列表的检测
  // 之前的规则 /^\s*\d+\.\s+/m 会错误匹配 "1. 列表项"
  // 之前的规则 /^\s*\d+\)\s+/m 也会错误匹配括号式列表
  // 之前的规则 /^\s*\d+\s*-\s+/m 也可能误判

  // 只检测明确的缩进代码（2+ 空格或制表符开头）
  // 且要求多行都有缩进，避免单行缩进误判
  const lines = content.split('\n');
  const indentedLines = lines.filter(l => /^\s{2,}|^\t/.test(l) && l.trim());
  // 至少 3 行有缩进才认为是代码块
  return indentedLines.length >= 3;
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
 * 检测并解析规划 JSON
 * 返回 PlanBlock 如果内容是有效的规划 JSON，否则返回 null
 */
export function tryParsePlanJson(jsonContent: string): PlanBlock | null {
  if (!jsonContent) return null;

  try {
    const parsed = JSON.parse(jsonContent.trim());

    // 规划 JSON 必须包含 goal 字段
    if (!parsed.goal || typeof parsed.goal !== 'string') {
      return null;
    }

    // 需要至少有 constraints 或 analysis 之一才算规划
    if (!parsed.constraints && !parsed.analysis) {
      return null;
    }

    // 构建 PlanBlock
    const planBlock: PlanBlock = {
      type: 'plan',
      goal: parsed.goal,
      rawJson: jsonContent.trim(),
    };

    if (parsed.analysis && typeof parsed.analysis === 'string') {
      planBlock.analysis = parsed.analysis;
    }

    if (Array.isArray(parsed.constraints)) {
      planBlock.constraints = parsed.constraints.filter((c: unknown) => typeof c === 'string');
    }

    if (Array.isArray(parsed.acceptanceCriteria)) {
      planBlock.acceptanceCriteria = parsed.acceptanceCriteria.filter((c: unknown) => typeof c === 'string');
    }

    if (parsed.riskLevel && ['low', 'medium', 'high'].includes(parsed.riskLevel)) {
      planBlock.riskLevel = parsed.riskLevel;
    }

    if (Array.isArray(parsed.riskFactors)) {
      planBlock.riskFactors = parsed.riskFactors.filter((c: unknown) => typeof c === 'string');
    }

    console.log('[content-parser] 成功解析规划 JSON:', {
      goal: planBlock.goal.substring(0, 50) + '...',
      hasAnalysis: !!planBlock.analysis,
      constraintsCount: planBlock.constraints?.length || 0,
    });

    return planBlock;
  } catch {
    return null;
  }
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
  // 🔧 修复：使用 [^\S\n]+ 替代 \s+，避免匹配换行符
  const regex = /```(\w*)(?::([^\s\n]+)|[^\S\n]+([^\n]*))?\n([\s\S]*?)```/g;
  let match;

  while ((match = regex.exec(content)) !== null) {
    // 🔧 修复：只有看起来像文件路径的才当作 filepath
    // 路径应该包含 / 或 \ 或文件扩展名（.xxx）
    let candidateFilepath = match[2] || match[3]?.trim() || undefined;
    let codeContent = match[4] || '';

    // 🔧 关键修复：如果捕获的内容不是有效路径，需要将其添加回代码内容
    // 例如：```json {\n"goal":... 中的 { 被捕获为 match[3]，但不是路径
    if (candidateFilepath && !isValidFilepath(candidateFilepath)) {
      // 将被错误捕获的内容添加回代码开头
      // match[3] 是空格后的内容，需要加换行符连接代码
      if (match[3]?.trim()) {
        codeContent = match[3].trim() + '\n' + codeContent;
      }
      candidateFilepath = undefined;
    }

    blocks.push({
      lang: match[1] || 'text',
      filepath: candidateFilepath,
      code: codeContent,
      startIndex: match.index,
      endIndex: match.index + match[0].length,
    });
  }

  return blocks;
}

/**
 * 判断字符串是否看起来像有效的文件路径
 * 有效路径应该包含: / 或 \ 或 .扩展名
 */
function isValidFilepath(candidate: string): boolean {
  if (!candidate || candidate.length > 200) return false;
  // 包含路径分隔符
  if (candidate.includes('/') || candidate.includes('\\')) return true;
  // 包含常见文件扩展名
  if (/\.\w{1,10}$/.test(candidate)) return true;
  // 排除看起来像代码的内容（包含空格、括号、花括号等）
  if (/[\s(){}[\]<>=;,]/.test(candidate)) return false;
  return false;
}

/**
 * 核心函数：将原始内容解析为 ContentBlock 数组
 * 这是后端统一处理 Agent 输出的入口
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
  let content = collapseExtraBlankLines(sanitized);
  const trimmed = content.trim();

  if (!trimmed) return [];

  const blocks: ContentBlock[] = [];

  // 2. 🔧 移除裸露的 JSON 对象（用户不需要看到原始 JSON）
  const embeddedJsons = extractEmbeddedJson(content);
  if (embeddedJsons.length > 0) {
    console.log('[content-parser] 发现裸露 JSON:', embeddedJsons.length, '个');
    embeddedJsons.forEach((json, idx) => {
      console.log(`[content-parser] JSON ${idx + 1}:`, {
        startIndex: json.startIndex,
        endIndex: json.endIndex,
        length: json.jsonText.length,
        preview: json.jsonText.substring(0, 100) + '...'
      });
    });

    // 从后往前移除，避免索引变化
    for (let i = embeddedJsons.length - 1; i >= 0; i--) {
      const json = embeddedJsons[i];
      // 移除 JSON 及其前后的空行
      const before = content.substring(0, json.startIndex).trimEnd();
      const after = content.substring(json.endIndex).trimStart();
      content = before + (before && after ? '\n\n' : '') + after;
    }

    console.log('[content-parser] 移除 JSON 后的内容长度:', content.length);
  }

  // 3. 提取代码块
  const codeBlocks = extractCodeBlocks(content);

  // 🔧 新增：检查内容是否以代码块开头
  const startsWithCodeBlock = codeBlocks.length > 0 && codeBlocks[0].startIndex === 0;

  console.log('[content-parser] 代码块检查:', {
    codeBlocksCount: codeBlocks.length,
    startsWithCodeBlock,
    firstCodeBlockLang: codeBlocks[0]?.lang,
    // 🔍 增强调试：记录首个代码块的首行内容
    firstCodeBlockFirstLine: codeBlocks[0]?.code?.split('\n')[0] || 'N/A',
    firstCodeBlockContentLength: codeBlocks[0]?.code?.length || 0,
  });

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

      // 🔧 新增：如果不是以代码块开头，且当前代码块是 JSON，则标记为嵌入式
      if (!startsWithCodeBlock && codeBlock.lang === 'json') {
        console.log('[content-parser] 标记嵌入式 JSON 代码块:', {
          startIndex: codeBlock.startIndex,
          length: codeBlock.code.length,
        });
        // 添加 isEmbedded 标记，前端会隐藏这个代码块
        blocks.push({
          type: 'code',
          content: codeBlock.code,
          language: codeBlock.lang,
          filename: codeBlock.filepath,
          isEmbedded: true,  // 标记为嵌入式，前端不渲染
        } as ContentBlock);
        lastIndex = codeBlock.endIndex;
        continue;
      }

      // 代码块本身
      const lang = codeBlock.lang || 'text';

      // 🔧 新增：尝试解析规划 JSON，返回 PlanBlock 而不是 CodeBlock
      if (lang === 'json') {
        const planBlock = tryParsePlanJson(codeBlock.code);
        if (planBlock) {
          // 成功解析为规划块
          blocks.push(planBlock);
          lastIndex = codeBlock.endIndex;
          continue;
        }
      }

      // 普通代码块
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
    const finalContent = content.trim();
    if (finalContent) {
      blocks.push(...parseTextContent(finalContent));
    }
  }

  // 4. 添加工具调用块
  if (options?.toolCalls && options.toolCalls.length > 0) {
    for (const tool of options.toolCalls) {
      // 后端统一序列化 input 为 JSON 字符串
      let inputStr: string | undefined;
      if (tool.input !== undefined && tool.input !== null) {
        inputStr = typeof tool.input === 'string' ? tool.input : JSON.stringify(tool.input, null, 2);
      }
      blocks.push({
        type: 'tool_call',
        toolName: tool.name,
        toolId: `tool-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`,
        status: (tool.status as 'pending' | 'running' | 'completed' | 'failed') || 'completed',
        input: inputStr,
      } as ContentBlock);
    }
  }

  return blocks;
}

/**
 * 解析纯文本内容（不含代码块）
 * 返回 message-protocol.ts 定义的 ContentBlock 类型
 */
function parseTextContent(text: string): ContentBlock[] {
  if (!text) return [];
  const trimmed = text.trim();
  if (!trimmed) return [];

  // 检测内容类型
  const jsonInfo = extractJsonInfo(trimmed);

  // 🔧 只有在纯 JSON 时才作为代码块（整个内容都是 JSON，没有其他文本）
  // 如果 JSON 混合在其他文本中，说明是 AI 的解释，应该保持原样
  if (jsonInfo.isJson && trimmed === jsonInfo.jsonText) {
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
