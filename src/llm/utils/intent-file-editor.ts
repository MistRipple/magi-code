import type { LLMClient, LLMMessage } from '../types';

export interface IntentFileEditParams {
  filePath: string;
  fileContent: string;
  summary: string;
  detailedDescription: string;
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

  for (let attempt = 0; attempt <= maxRecoveryAttempts; attempt += 1) {
    const prompt = `你是一个专业的代码编辑 Agent。
你的任务是将用户的修改意图准确地应用到以下文件中。

目标文件路径: ${params.filePath}

当前文件内容:
\`\`\`
${params.fileContent}
\`\`\`

修改摘要 (Summary): ${params.summary}
详细描述 (Detailed Intent): ${params.detailedDescription}

请输出修改后的完整文件内容，必须包裹在 \`\`\` 代码块中。不要添加任何多余的解释说明、不要在代码块外部添加 markdown。
如果你认为没有必要修改，或者意图无法应用，请直接输出原内容。${recoveryHint}`;

    const messages: LLMMessage[] = [{ role: 'user', content: prompt }];
    const response = await client.sendMessage({
      messages,
      systemPrompt: '你是一个严格的编辑器程序，你的唯一职责是输出被编辑后的文件内容，严格遵守用户的意图。',
      temperature: 0.1,
      stream: false,
    });

    const extracted = extractEditedFileContent(response.content);
    const isLastAttempt = attempt >= maxRecoveryAttempts;
    if (!isLikelyMetaReasoningOutput(extracted)) {
      return extracted;
    }
    if (isLastAttempt) {
      break;
    }
    recoveryHint = buildRecoveryHint(attempt + 1, extracted);
  }

  throw new Error(`intent_file_edit_unusable_output: 连续 ${maxRecoveryAttempts + 1} 次未返回可应用的文件内容（疑似输出了模型分析文本）`);
}

/**
 * 从 LLM 返回文本中提取编辑后的完整文件内容。
 * 支持标准代码块、未闭合代码块、纯文本三种输出形式。
 */
export function extractEditedFileContent(output: string | undefined | null): string {
  if (!output) return '';

  const codeBlockMatch = output.match(/```[a-zA-Z]*\r?\n([\s\S]*?)\r?\n```/);
  if (codeBlockMatch) {
    return codeBlockMatch[1];
  }

  const unclosedMatch = output.match(/```[a-zA-Z]*\r?\n([\s\S]*)$/);
  if (unclosedMatch) {
    return unclosedMatch[1].replace(/\r?\n```\s*$/, '');
  }

  return output.trim();
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
    `\n上一轮返回了分析文本而非完整文件内容（示例：${compact}）。` +
    `\n本轮禁止输出任何分析、解释、计划、思考；只能输出完整文件内容代码块。`;
}
