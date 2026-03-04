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
如果你认为没有必要修改，或者意图无法应用，请直接输出原内容。`;

  const messages: LLMMessage[] = [{ role: 'user', content: prompt }];
  const response = await client.sendMessage({
    messages,
    systemPrompt: '你是一个严格的编辑器程序，你的唯一职责是输出被编辑后的文件内容，严格遵守用户的意图。',
    temperature: 0.1,
    stream: false,
  });

  return extractEditedFileContent(response.content);
}

/**
 * 从 LLM 返回文本中提取编辑后的完整文件内容。
 * 支持标准代码块、未闭合代码块、纯文本三种输出形式。
 */
export function extractEditedFileContent(output: string): string {
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

