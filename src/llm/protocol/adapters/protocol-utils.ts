import { logger, LogCategory } from '../../../logging';

export type DeltaMode = 'unknown' | 'delta' | 'cumulative';

export interface NormalizedTokenUsage {
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens?: number;
  cacheWriteTokens?: number;
}

export function computeSuffixPrefixOverlap(text: string, incoming: string): number {
  const max = Math.min(text.length, incoming.length, 4096);
  for (let len = max; len > 0; len--) {
    if (text.slice(-len) === incoming.slice(0, len)) {
      return len;
    }
  }
  return 0;
}

export function normalizeStreamDelta(
  incoming: string,
  emittedText: string,
  mode: DeltaMode,
): { delta: string; mode: DeltaMode } {
  if (!incoming) {
    return { delta: '', mode };
  }
  if (!emittedText) {
    return { delta: incoming, mode };
  }

  let resolvedMode = mode;
  if (resolvedMode === 'unknown') {
    resolvedMode = incoming.length > emittedText.length && incoming.startsWith(emittedText)
      ? 'cumulative'
      : 'delta';
  }

  if (resolvedMode !== 'cumulative') {
    return { delta: incoming, mode: resolvedMode };
  }

  if (incoming.length <= emittedText.length && emittedText.endsWith(incoming)) {
    return { delta: '', mode: resolvedMode };
  }
  if (incoming.startsWith(emittedText)) {
    return { delta: incoming.slice(emittedText.length), mode: resolvedMode };
  }

  const overlap = computeSuffixPrefixOverlap(emittedText, incoming);
  return { delta: incoming.slice(overlap), mode: resolvedMode };
}

export function is400ToolSchemaError(error: any): boolean {
  const status = error?.status || error?.response?.status;
  if (status !== 400) return false;
  const msg = String(error?.message || error?.error?.message || '');
  return /invalid.*schema|invalid.*tool|invalid.*function/i.test(msg);
}

export function isChunkParseError(error: any): boolean {
  const msg = String(error?.message || '');
  return /unexpected.*after json|unexpected.*json|json.*position/i.test(msg);
}

export function sanitizeSchema(schema: any): any {
  if (!schema || typeof schema !== 'object') {
    return { type: 'object', properties: {} };
  }

  const sanitized: any = {
    type: schema.type || 'object',
  };

  if (schema.properties && typeof schema.properties === 'object') {
    sanitized.properties = {};
    for (const [key, value] of Object.entries(schema.properties)) {
      sanitized.properties[key] = sanitizeProperty(value);
    }
  } else {
    sanitized.properties = {};
  }

  if (Array.isArray(schema.required) && schema.required.length > 0) {
    const validRequired = schema.required.filter((r: string) => sanitized.properties[r] !== undefined);
    if (validRequired.length > 0) {
      sanitized.required = validRequired;
    }
  }

  return sanitized;
}

export function sanitizeProperty(prop: any): any {
  if (!prop || typeof prop !== 'object') {
    return { type: 'string' };
  }

  const sanitized: any = {};

  sanitized.type = prop.type || 'string';
  if (prop.description) {
    sanitized.description = String(prop.description);
  }
  if (Array.isArray(prop.enum) && prop.enum.length > 0) {
    sanitized.enum = prop.enum;
  }

  if (prop.type === 'array' && prop.items) {
    sanitized.items = sanitizeProperty(prop.items);
  }

  if (prop.type === 'object' && prop.properties) {
    sanitized.properties = {};
    for (const [key, value] of Object.entries(prop.properties)) {
      sanitized.properties[key] = sanitizeProperty(value);
    }
    if (Array.isArray(prop.required) && prop.required.length > 0) {
      sanitized.required = prop.required;
    }
  }

  return sanitized;
}

export function normalizeToolResultBlock(
  block: any,
  context: string,
  provider: string,
  model: string,
): { toolUseId: string; content: string; isError: boolean } | null {
  const toolUseId = typeof block?.tool_use_id === 'string' ? block.tool_use_id.trim() : '';
  if (!toolUseId) {
    logger.warn('忽略缺少 tool_use_id 的 tool_result', {
      provider,
      model,
      context,
    }, LogCategory.LLM);
    return null;
  }

  const standardizedStatus = typeof block?.standardized?.status === 'string'
    ? block.standardized.status
    : '';
  const standardizedMessage = typeof block?.standardized?.message === 'string'
    ? block.standardized.message.trim()
    : '';

  const isError = block?.is_error === true || (standardizedStatus !== '' && standardizedStatus !== 'success');
  const rawContent = block?.content;
  const stringContent = typeof rawContent === 'string'
    ? rawContent
    : (rawContent == null ? '' : JSON.stringify(rawContent));
  const content = stringContent.trim()
    ? stringContent
    : (isError ? (standardizedMessage || 'Tool execution failed') : '[empty result]');

  return {
    toolUseId,
    content,
    isError,
  };
}

export function toOpenAIToolMessageContent(normalized: { content: string; isError: boolean }): string {
  const content = normalized.content || '[empty result]';
  if (!normalized.isError) {
    return content;
  }
  if (/^\s*(\[error\]|error[:\]])/i.test(content)) {
    return content;
  }
  return `[Error] ${content}`;
}

function toSafeTokenNumber(value: unknown): number {
  if (typeof value === 'number' && Number.isFinite(value)) {
    return Math.max(0, Math.trunc(value));
  }
  if (typeof value === 'string') {
    const parsed = Number(value);
    if (Number.isFinite(parsed)) {
      return Math.max(0, Math.trunc(parsed));
    }
  }
  return 0;
}

function pickFirstTokenNumber(...values: unknown[]): number {
  for (const value of values) {
    const tokenNumber = toSafeTokenNumber(value);
    if (tokenNumber > 0) {
      return tokenNumber;
    }
  }
  return 0;
}

export function normalizeAnthropicUsage(rawUsage: any): NormalizedTokenUsage {
  const inputTokens = pickFirstTokenNumber(rawUsage?.input_tokens, rawUsage?.inputTokens);
  const outputTokens = pickFirstTokenNumber(rawUsage?.output_tokens, rawUsage?.outputTokens);
  const cacheReadTokens = pickFirstTokenNumber(
    rawUsage?.cache_read_input_tokens,
    rawUsage?.cacheReadInputTokens,
    rawUsage?.cache_read_tokens,
    rawUsage?.cacheReadTokens,
  );
  const cacheWriteTokens = pickFirstTokenNumber(
    rawUsage?.cache_creation_input_tokens,
    rawUsage?.cacheCreationInputTokens,
    rawUsage?.cache_creation_tokens,
    rawUsage?.cacheWriteTokens,
  );

  return {
    inputTokens,
    outputTokens,
    cacheReadTokens: cacheReadTokens || undefined,
    cacheWriteTokens: cacheWriteTokens || undefined,
  };
}

export function normalizeOpenAIUsage(rawUsage: any): NormalizedTokenUsage {
  return {
    inputTokens: pickFirstTokenNumber(
      rawUsage?.prompt_tokens,
      rawUsage?.promptTokens,
      rawUsage?.input_tokens,
      rawUsage?.inputTokens,
    ),
    outputTokens: pickFirstTokenNumber(
      rawUsage?.completion_tokens,
      rawUsage?.completionTokens,
      rawUsage?.output_tokens,
      rawUsage?.outputTokens,
    ),
  };
}

export function parseToolArguments(
  raw: unknown,
  context: string,
  provider: string,
  model: string,
): { value: Record<string, any>; error?: string; rawText?: string } {
  if (raw === undefined || raw === null || raw === '') {
    return { value: {} };
  }

  if (typeof raw === 'object') {
    if (Array.isArray(raw)) {
      return {
        value: {} as any,
        error: '参数解析后为数组，工具参数必须是对象',
        rawText: JSON.stringify(raw),
      };
    }
    return { value: raw as Record<string, any> };
  }

  if (typeof raw !== 'string') {
    logger.error('Tool arguments 类型异常', {
      provider,
      model,
      context,
      argType: typeof raw,
    }, LogCategory.LLM);
    return {
      value: null as any,
      error: `参数类型异常: ${typeof raw}`,
      rawText: String(raw),
    };
  }

  const text = raw.trim();
  if (!text) {
    return { value: {} };
  }

  try {
    const parsed = JSON.parse(text);
    if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) {
      return { value: parsed as Record<string, any>, rawText: text };
    }
    logger.error('Tool arguments 解析结果非对象', {
      provider,
      model,
      context,
      parsedType: typeof parsed,
    }, LogCategory.LLM);
    return {
      value: null as any,
      error: `参数 JSON 解析后不是对象: ${typeof parsed}`,
      rawText: text,
    };
  } catch (error: any) {
    const extracted = extractFirstJSONObject(text);
    if (extracted && extracted !== text) {
      try {
        const recovered = JSON.parse(extracted);
        if (recovered && typeof recovered === 'object' && !Array.isArray(recovered)) {
          logger.info('Tool arguments 解析失败后已成功恢复 JSON', {
            provider,
            model,
            context,
          }, LogCategory.LLM);
          return { value: recovered as Record<string, any>, rawText: text };
        }
      } catch (recoveryError: any) {
        logger.info('工具参数尝试恢复解析失败', {
          error: recoveryError?.message,
          extractedText: extracted,
        }, LogCategory.LLM);
      }
    }
    logger.error('Tool arguments JSON 解析彻底失败', {
      provider,
      model,
      context,
      error: error?.message || String(error),
      rawSnippet: text.substring(0, 300),
    }, LogCategory.LLM);
    return {
      value: null as any,
      error: `参数 JSON 解析失败: ${error?.message || String(error)}`,
      rawText: text,
    };
  }
}

export function extractFirstJSONObject(text: string): string | null {
  if (!text) {
    return null;
  }

  const start = text.indexOf('{');
  if (start === -1) {
    return null;
  }

  let depth = 0;
  let inString = false;
  let escaped = false;

  for (let i = start; i < text.length; i++) {
    const ch = text[i];

    if (inString) {
      if (escaped) {
        escaped = false;
      } else if (ch === '\\') {
        escaped = true;
      } else if (ch === '"') {
        inString = false;
      }
      continue;
    }

    if (ch === '"') {
      inString = true;
      continue;
    }

    if (ch === '{') {
      depth++;
      continue;
    }

    if (ch === '}') {
      depth--;
      if (depth === 0) {
        return text.slice(start, i + 1);
      }
      if (depth < 0) {
        return null;
      }
    }
  }

  return null;
}
