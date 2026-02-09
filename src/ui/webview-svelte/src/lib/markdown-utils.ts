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
  // 只处理代码块外的文本，避免破坏代码块中的 \n 字面量
  const parts = processed.split(/```/);
  processed = parts.map((part, idx) => {
    if (idx % 2 === 1) return part; // 代码块内部不处理
    return part.replace(/\\n/g, '\n');
  }).join('```');

  // 1. 特殊标签处理：将 <think> 转换为引用块
  // 防止 marked 将其视为 HTML 块而忽略内部的 Markdown 格式
  processed = processed
    .replace(/<think>/g, '\n> **Thinking:**\n')
    .replace(/<\/think>/g, '\n\n');

  // 2. 流式输出时的特殊处理：检测并补全未闭合的代码块
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
