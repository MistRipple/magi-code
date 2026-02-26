/**
 * 统一 Token 估算工具
 *
 * 统一使用保守口径：约 4 字符 ≈ 1 token。
 */

export const DEFAULT_CHARS_PER_TOKEN = 4;
export const DEFAULT_CJK_CHARS_PER_TOKEN = 2;

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
