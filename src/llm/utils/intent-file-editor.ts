import type { LLMClient, LLMMessage } from '../types';

export interface IntentFileEditParams {
  filePath: string;
  fileContent: string;
  summary: string;
  detailedDescription: string;
}

export type IntentFileEditFailureCode =
  | 'intent_file_edit_empty_response'
  | 'intent_file_edit_meta_output'
  | 'intent_file_edit_truncated_output';

export class IntentFileEditError extends Error {
  constructor(
    public readonly code: IntentFileEditFailureCode,
    message: string,
  ) {
    super(message);
    this.name = 'IntentFileEditError';
  }
}

const FILE_EDIT_START_MARKER = '<<MAGI_EDITED_FILE_START>>';
const FILE_EDIT_END_MARKER = '<<MAGI_EDITED_FILE_END>>';

interface ExtractedEditedFileContent {
  content: string;
  source: 'markers' | 'fenced_code_block' | 'unclosed_fenced_code_block' | 'plain_text';
}

/**
 * 使用指定 LLM 客户端执行意图驱动文件编辑。
 * 返回修改后的完整文件内容。
 */
export async function runIntentDrivenFileEdit(
  client: LLMClient,
  params: IntentFileEditParams,
): Promise<string> {
  const maxRecoveryAttempts = 2;
  let recoveryHint = '';
  let requestedMaxTokens = resolveIntentFileEditMaxTokens(params.fileContent);

  for (let attempt = 0; attempt <= maxRecoveryAttempts; attempt += 1) {
    const prompt = buildIntentFileEditPrompt(params, recoveryHint);

    const messages: LLMMessage[] = [{ role: 'user', content: prompt }];
    const response = await client.sendMessage({
      messages,
      systemPrompt: [
        '你是一个严格的文件编辑器。',
        '你的唯一职责是根据修改意图输出编辑后的完整文件内容。',
        `输出格式必须且只能是：${FILE_EDIT_START_MARKER} 与 ${FILE_EDIT_END_MARKER} 包裹的完整文件内容。`,
        '禁止输出分析、解释、计划、思考、说明文字、Markdown 标题、代码块围栏或额外 JSON。',
        '如果无需修改，也必须按同样格式输出原始完整文件内容。',
      ].join('\n'),
      temperature: 0,
      maxTokens: requestedMaxTokens,
      stream: false,
    });

    const isLastAttempt = attempt >= maxRecoveryAttempts;
    if (response.stopReason === 'max_tokens') {
      if (isLastAttempt) {
        throw new IntentFileEditError(
          'intent_file_edit_truncated_output',
          'file_edit 专用模型输出在达到 max_tokens 后被截断，未返回完整文件内容。',
        );
      }
      requestedMaxTokens = Math.min(requestedMaxTokens * 2, 24_000);
      recoveryHint = buildTruncationRecoveryHint(attempt + 1, response.content);
      continue;
    }

    const extracted = extractEditedFileContent(response.content);
    if (!extracted.content.trim()) {
      if (isLastAttempt) {
        throw new IntentFileEditError(
          'intent_file_edit_empty_response',
          'file_edit 专用模型未返回任何可用内容。',
        );
      }
      recoveryHint = buildRecoveryHint(attempt + 1, response.content);
      continue;
    }
    if (extracted.content && !isLikelyMetaReasoningOutput(extracted.content)) {
      return extracted.content;
    }
    if (isLastAttempt) {
      break;
    }
    recoveryHint = buildRecoveryHint(attempt + 1, extracted.content || response.content);
  }

  throw new IntentFileEditError(
    'intent_file_edit_meta_output',
    `intent_file_edit_unusable_output: 连续 ${maxRecoveryAttempts + 1} 次未返回可应用的完整文件内容（输出偏离了 file_edit 协议）`,
  );
}

/**
 * 从 LLM 返回文本中提取编辑后的完整文件内容。
 * 单一协议优先使用显式边界标记；若模型未完全遵守，则从候选块中提取最可能的完整文件内容。
 */
export function extractEditedFileContent(output: string | undefined | null): ExtractedEditedFileContent {
  if (!output) {
    return { content: '', source: 'plain_text' };
  }

  const markerContent = extractMarkedContent(output);
  if (markerContent !== null) {
    return { content: markerContent, source: 'markers' };
  }

  const fencedBlocks = Array.from(output.matchAll(/```[a-zA-Z0-9_-]*\r?\n([\s\S]*?)\r?\n```/g))
    .map((match) => match[1]?.trimEnd() || '')
    .filter(Boolean);
  if (fencedBlocks.length > 0) {
    return {
      content: pickMostLikelyFilePayload(fencedBlocks),
      source: 'fenced_code_block',
    };
  }

  const unclosedBlocks = Array.from(output.matchAll(/```[a-zA-Z0-9_-]*\r?\n([\s\S]*)$/g))
    .map((match) => (match[1] || '').replace(/\r?\n```\s*$/, '').trimEnd())
    .filter(Boolean);
  if (unclosedBlocks.length > 0) {
    return {
      content: pickMostLikelyFilePayload(unclosedBlocks),
      source: 'unclosed_fenced_code_block',
    };
  }

  return {
    content: output.trim(),
    source: 'plain_text',
  };
}

function isLikelyMetaReasoningOutput(content: string): boolean {
  const normalized = (content || '').trim();
  if (!normalized) {
    return true;
  }

  const lower = normalized.toLowerCase();
  if (lower.startsWith('**analyzing') || lower.startsWith('analyzing ')) {
    return true;
  }

  const firstLine = normalized
    .split('\n')
    .map((line) => line.trim())
    .find(Boolean) || normalized;

  if (/^(以下是|这是|修改说明|说明：|分析：|总结：|here(?:'|’)s|here is|updated file|i(?:'|’)ve|i have)/i.test(firstLine)) {
    return true;
  }

  let hitCount = 0;
  const indicators = [
    /\*\*\s*Analyzing\b/i,
    /\bI need to\b/i,
    /\bI should\b/i,
    /\bI still need\b/i,
    /\bI must\b/i,
    /\bThis raises questions\b/i,
    /需要进一步分析/,
    /先分析一下/,
    /下面是修改/,
    /修改后的完整文件/,
    /请查看以下内容/,
  ];
  for (const indicator of indicators) {
    if (indicator.test(normalized)) {
      hitCount += 1;
    }
  }

  return hitCount >= 2 && normalized.length >= 100;
}

function buildRecoveryHint(attempt: number, unusableOutput: string): string {
  const firstLine = unusableOutput
    .split('\n')
    .map(line => line.trim())
    .find(Boolean) || unusableOutput.trim();
  const compact = firstLine.length > 160 ? `${firstLine.slice(0, 160)}...` : firstLine;

  return `\n\n【上次输出无效，自动恢复重试第 ${attempt} 次】` +
    `\n上一轮返回了非文件内容输出（示例：${compact}）。` +
    `\n本轮必须且只能输出以下结构：` +
    `\n${FILE_EDIT_START_MARKER}` +
    `\n<完整文件内容>` +
    `\n${FILE_EDIT_END_MARKER}` +
    `\n禁止输出任何分析、解释、计划、思考、标题、代码块围栏。`;
}

function buildTruncationRecoveryHint(attempt: number, unusableOutput: string): string {
  const compact = unusableOutput
    .replace(/\s+/g, ' ')
    .trim()
    .slice(0, 160);
  return `\n\n【上次输出被截断，自动恢复重试第 ${attempt} 次】` +
    `\n上一轮未返回完整文件内容（片段：${compact || '空输出'}）。` +
    `\n本轮必须一次性输出完整文件内容，并使用固定边界标记，不得省略结尾。`;
}

function buildIntentFileEditPrompt(params: IntentFileEditParams, recoveryHint: string): string {
  return [
    '你是一个专业的代码编辑 Agent。',
    '你的任务是将用户的修改意图准确地应用到当前文件。',
    '',
    `目标文件路径: ${params.filePath}`,
    '',
    '当前文件内容:',
    '```',
    params.fileContent,
    '```',
    '',
    `修改摘要 (Summary): ${params.summary}`,
    `详细描述 (Detailed Intent): ${params.detailedDescription}`,
    '',
    '输出要求：',
    `1. 仅输出 ${FILE_EDIT_START_MARKER} 与 ${FILE_EDIT_END_MARKER} 包裹的完整文件内容。`,
    '2. 不要输出任何分析、解释、计划、思考、说明文字、Markdown 标题或代码块围栏。',
    '3. 如果无需修改，也必须输出原始完整文件内容。',
    '',
    FILE_EDIT_START_MARKER,
    '<完整文件内容>',
    FILE_EDIT_END_MARKER,
    recoveryHint,
  ].join('\n');
}

function extractMarkedContent(output: string): string | null {
  const startIndex = output.indexOf(FILE_EDIT_START_MARKER);
  const endIndex = output.lastIndexOf(FILE_EDIT_END_MARKER);
  if (startIndex < 0 || endIndex < 0 || endIndex <= startIndex) {
    return null;
  }
  const content = output
    .slice(startIndex + FILE_EDIT_START_MARKER.length, endIndex)
    .replace(/^\r?\n/, '')
    .trimEnd();
  return content;
}

function pickMostLikelyFilePayload(candidates: string[]): string {
  return candidates.reduce((best, candidate) => (
    candidate.length >= best.length ? candidate : best
  ), '');
}

function resolveIntentFileEditMaxTokens(fileContent: string): number {
  const approxTokens = Math.ceil(fileContent.length / 3);
  return Math.max(2048, Math.min(12_000, approxTokens + 1024));
}
