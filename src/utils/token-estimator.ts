/**
 * 统一 Token 估算工具
 *
 * 使用保守口径以防止 context limit 溢出：
 * - 代码/混合内容：约 3 字符 ≈ 1 token（比 4 字符/token 多预留 ~33% 安全裕度）
 * - CJK 文字：约 1.5 字符 ≈ 1 token
 *
 * 实际 tokenizer 的比率受字符分布影响较大：
 * - 纯英文散文：~4 字符/token
 * - 代码（特殊字符密集）：~2.5-3.5 字符/token
 * - 中文：~1.2-1.8 字符/token
 * 此处取偏保守值，优先避免 API 的 context_length_exceeded 错误。
 */

export const DEFAULT_CHARS_PER_TOKEN = 3;
export const DEFAULT_CJK_CHARS_PER_TOKEN = 1.5;

export interface MixedLanguageEstimationOptions {
  cjkCharsPerToken?: number;
  otherCharsPerToken?: number;
}

export function estimateTokenCount(
  text: string,
  charsPerToken: number = DEFAULT_CHARS_PER_TOKEN
): number {
  if (!text) {
    return 0;
  }
  return Math.ceil(text.length / charsPerToken);
}

export function estimateMixedLanguageTokenCount(
  text: string,
  options: MixedLanguageEstimationOptions = {}
): number {
  if (!text) {
    return 0;
  }

  const cjkCharsPerToken = options.cjkCharsPerToken ?? DEFAULT_CJK_CHARS_PER_TOKEN;
  const otherCharsPerToken = options.otherCharsPerToken ?? DEFAULT_CHARS_PER_TOKEN;

  const cjkChars = (text.match(/[\u4e00-\u9fff]/g) || []).length;
  const otherChars = text.length - cjkChars;

  return Math.ceil(cjkChars / cjkCharsPerToken + otherChars / otherCharsPerToken);
}

export function estimateMaxCharsForTokens(
  tokens: number,
  charsPerToken: number = DEFAULT_CHARS_PER_TOKEN
): number {
  if (tokens <= 0) {
    return 0;
  }
  return Math.floor(tokens * charsPerToken);
}
