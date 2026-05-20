/**
 * Markdown 处理工具库
 * 用于处理流式输出、特殊标签转换和代码块补全
 */

/**
 * 预处理 Markdown 内容
 * @param content 原始 Markdown 文本
 * @param isStreaming 是否处于流式输出模式
 * @returns 处理后的 Markdown 文本
 */
export function preprocessMarkdown(content: string, isStreaming: boolean): string {
  if (!content) return '';

  let processed = content;

  // 0. 修复被转义的换行符（字面量 \n → 真正的换行）
  // 只处理 fenced code block 之外的文本，避免破坏代码块中的 \n 字面量
  processed = replaceEscapedNewlinesOutsideFences(processed);

  // 1. 特殊标签处理：将 <think> 转换为引用块
  // 防止 marked 将其视为 HTML 块而忽略内部的 Markdown 格式
  processed = processed
    .replace(/<think>/g, '\n> **Thinking:**\n')
    .replace(/<\/think>/g, '\n\n');

  // 2. 非流式完成态不应展示只剩围栏头的空代码块。
  // 这种内容通常来自上游流式协议截断，正文已保留，孤立围栏不是用户可读内容。
  if (!isStreaming) {
    processed = removeTrailingEmptyFence(processed);
  }

  // 3. 流式输出时的特殊处理：检测并补全未闭合的代码块
  if (isStreaming) {
    // 检测代码块标记 (``` 或 ~~~)，匹配行首或行内的标记
    // 简单的启发式检测：如果标记数量是奇数，说明最后一个未闭合
    const fences = processed.match(/^ {0,3}(`{3,}|~{3,})/gm);
    
    if (fences && fences.length % 2 !== 0) {
      const lastFence = fences[fences.length - 1].trim();
      // 补全闭合标记，强制 marked 解析为代码块
      processed += '\n' + lastFence;
    }
  }

  return processed;
}

export interface StreamingMarkdownParts {
  stable: string;
  volatile: string;
}

export function splitStreamingMarkdown(content: string): StreamingMarkdownParts {
  if (!content) {
    return { stable: '', volatile: '' };
  }
  const boundary = findLastStableStreamingBoundary(content);
  if (boundary <= 0) {
    return { stable: '', volatile: content };
  }
  if (boundary >= content.length) {
    return { stable: content, volatile: '' };
  }
  return {
    stable: content.slice(0, boundary),
    volatile: content.slice(boundary),
  };
}

function findLastStableStreamingBoundary(content: string): number {
  let lastBoundary = 0;
  let offset = 0;
  let inFence = false;
  let fenceMarker = '';
  let fenceLength = 0;
  const lines = content.match(/[^\n]*(?:\n|$)/g) ?? [];

  for (const line of lines) {
    if (!line) {
      continue;
    }
    const nextOffset = offset + line.length;
    const hasLineEnding = line.endsWith('\n');
    const lineText = hasLineEnding ? line.slice(0, -1) : line;
    const fenceMatch = lineText.match(/^ {0,3}(`{3,}|~{3,})/);
    let closedFence = false;

    if (fenceMatch) {
      const markerToken = fenceMatch[1];
      const markerChar = markerToken[0];
      const markerLength = Math.max(3, markerToken.length);
      if (!inFence) {
        const rest = lineText.slice(fenceMatch[0].length);
        const hasInlineClosingFence = new RegExp(`${markerChar}{${markerLength},}`).test(rest);
        if (!hasInlineClosingFence) {
          inFence = true;
          fenceMarker = markerChar;
          fenceLength = markerLength;
        }
      } else if (
        fenceMarker === markerChar
        && markerLength >= fenceLength
      ) {
        inFence = false;
        fenceMarker = '';
        fenceLength = 0;
        closedFence = true;
      }
    }

    if (
      hasLineEnding
      && !inFence
      && (
        lineText.trim() === ''
        || closedFence
        || isAtxHeadingLine(lineText)
      )
    ) {
      lastBoundary = nextOffset;
    }
    offset = nextOffset;
  }

  return lastBoundary;
}

function isAtxHeadingLine(line: string): boolean {
  return /^ {0,3}#{1,6}(?:\s+|$)/.test(line.trimEnd());
}

function removeTrailingEmptyFence(input: string): string {
  const lines = input.split('\n');
  let lastContentLineIndex = lines.length - 1;
  while (lastContentLineIndex >= 0 && !lines[lastContentLineIndex].trim()) {
    lastContentLineIndex -= 1;
  }
  if (lastContentLineIndex < 0) {
    return '';
  }
  const fenceLinePattern = /^ {0,3}(`{3,}|~{3,})[^\n]*$/u;
  if (!fenceLinePattern.test(lines[lastContentLineIndex])) {
    return input;
  }
  const fenceCount = lines
    .slice(0, lastContentLineIndex + 1)
    .filter((line) => fenceLinePattern.test(line))
    .length;
  if (fenceCount % 2 === 0) {
    return input;
  }
  return lines.slice(0, lastContentLineIndex).join('\n').trimEnd();
}

function replaceEscapedNewlinesOutsideFences(input: string): string {
  const lines = input.split('\n');
  const output: string[] = [];
  let inFence = false;
  let fenceMarker = '';

  for (const line of lines) {
    const fenceMatch = line.match(/^ {0,3}(`{3,}|~{3,})/);
    if (fenceMatch) {
      const markerToken = fenceMatch[1];
      const markerChar = markerToken[0];
      if (!inFence) {
        const markerLength = Math.max(3, markerToken.length);
        const rest = line.slice(fenceMatch[0].length);
        // 同行开闭（```inline```）不改变代码块状态，避免污染后续行。
        const hasInlineClosingFence = new RegExp(`${markerChar}{${markerLength},}`).test(rest);
        if (!hasInlineClosingFence) {
          inFence = true;
          fenceMarker = markerChar;
        }
      } else if (fenceMarker === markerChar) {
        inFence = false;
        fenceMarker = '';
      }
      output.push(line);
      continue;
    }
    output.push(inFence ? line : line.replace(/\\n/g, '\n'));
  }

  return output.join('\n');
}
