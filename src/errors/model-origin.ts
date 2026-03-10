export const MODEL_ERROR_PREFIX = '[MODEL_CAUSE]';

export type ModelOriginIssueKind =
  | 'prefixed'
  | 'auth'
  | 'quota'
  | 'rate_limit'
  | 'context_limit'
  | 'model_unavailable'
  | 'timeout'
  | 'network'
  | 'empty_response'
  | 'tool_param_parse'
  | 'reasoning_leak'
  | 'unknown';

export interface ModelOriginIssueClassification {
  isModelCause: boolean;
  message: string;
  normalized: string;
  kind?: ModelOriginIssueKind;
}

function normalizeReason(reason: string): string {
  const trimmed = (reason || '').trim();
  if (!trimmed) {
    return '';
  }
  const unwrapped = trimmed
    .replace(/^LLM 执行失败[:：]\s*/i, '')
    .replace(/^任务执行失败[:：]\s*/i, '')
    .trim();
  return unwrapped;
}

function hasReasoningLeak(normalized: string): boolean {
  const hasAnalysisHeading = /\*\*\s*Analyzing\b/i.test(normalized);
  const hasFirstPersonReasoning = /\bI need to\b|\bI should\b|\bI still need\b|\bI must\b/i.test(normalized);
  return (hasAnalysisHeading || hasFirstPersonReasoning) && normalized.length >= 120;
}

function pick(normalized: string, message: string, kind: ModelOriginIssueKind): ModelOriginIssueClassification {
  return {
    isModelCause: true,
    message,
    normalized,
    kind,
  };
}

export function classifyModelOriginIssue(reason: string): ModelOriginIssueClassification {
  const normalized = normalizeReason(reason);
  if (!normalized) {
    return {
      isModelCause: false,
      message: reason,
      normalized,
    };
  }

  const prefixedIndex = normalized.indexOf(MODEL_ERROR_PREFIX);
  if (prefixedIndex >= 0) {
    const stripped = normalized.slice(prefixedIndex + MODEL_ERROR_PREFIX.length).trim();
    return pick(normalized, stripped || '模型调用异常，已按降级链路处理。', 'prefixed');
  }

  const lower = normalized.toLowerCase();
  if (lower.includes('auth_unavailable') || lower.includes('no auth available') || lower.includes('invalid api key') || lower.includes('unauthorized') || lower.includes('forbidden')) {
    return pick(normalized, '模型服务鉴权失败，请检查 API Key/Token 或对应供应商登录状态。', 'auth');
  }
  if (lower.includes('quota') || lower.includes('insufficient') || lower.includes('billing') || lower.includes('payment required')) {
    return pick(normalized, '模型服务额度不足或计费受限，请检查账户配额与计费状态。', 'quota');
  }
  if (lower.includes('rate limit') || lower.includes('too many requests') || lower.includes('429')) {
    return pick(normalized, '模型服务触发限流，请稍后重试。', 'rate_limit');
  }
  if (lower.includes('context length') || lower.includes('maximum context') || lower.includes('prompt is too long') || lower.includes('token limit')) {
    return pick(normalized, '模型上下文超出限制，请缩小输入范围后重试。', 'context_limit');
  }
  if (lower.includes('model_not_found') || lower.includes('model not found') || lower.includes('unknown model') || lower.includes('invalid model')) {
    return pick(normalized, '模型名称或可用性异常，请检查模型配置是否正确。', 'model_unavailable');
  }
  if (lower.includes('timeout') || lower.includes('timed out') || lower.includes('request ended without sending') || lower.includes('stream ended')) {
    return pick(normalized, '模型服务请求超时或流中断，请稍后重试。', 'timeout');
  }
  if (lower.includes('fetch failed') || lower.includes('network') || lower.includes('socket hang up') || lower.includes('econnreset') || lower.includes('econnrefused') || lower.includes('enotfound') || lower.includes('tls') || lower.includes('certificate')) {
    return pick(normalized, '模型服务网络连接异常，请检查网络环境后重试。', 'network');
  }
  if (lower.includes('error occurred while processing your request') || lower.includes('help.openai.com')) {
    return pick(normalized, '模型服务暂时不可用（上游处理中断），系统已自动重试；若持续失败请稍后再试。', 'model_unavailable');
  }
  if (normalized.includes('LLM 响应为空') || normalized.includes('Error during LLM edit generation') || normalized.includes('model returned non-string content')) {
    return pick(normalized, '模型本轮未返回可执行内容，已自动结束本轮。请重试，或补充更明确的输入与约束。', 'empty_response');
  }
  if (lower.includes('tool parameter parse failed') || lower.includes('工具参数解析失败')) {
    return pick(normalized, '模型输出格式异常（工具参数解析失败），已自动结束本轮。请重试。', 'tool_param_parse');
  }
  if (hasReasoningLeak(normalized)) {
    return pick(normalized, '模型本轮输出偏离执行链路，未按协议完成任务。已中止本轮，请重试。', 'reasoning_leak');
  }

  return {
    isModelCause: false,
    message: normalized,
    normalized,
  };
}

export function isModelOriginIssue(reason: string): boolean {
  return classifyModelOriginIssue(reason).isModelCause;
}

export function toModelOriginUserMessage(reason: string): string {
  const result = classifyModelOriginIssue(reason);
  if (result.isModelCause) {
    return result.message;
  }
  return result.normalized || (reason || '').trim();
}

export function toSurfacedModelCauseError(reason: string): string {
  const result = classifyModelOriginIssue(reason);
  if (!result.isModelCause) {
    return result.normalized || reason;
  }
  return `${MODEL_ERROR_PREFIX} ${result.message}`;
}
