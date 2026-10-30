export type OrchestratorReasoningEffort = 'none' | 'minimal' | 'low' | 'medium' | 'high' | 'xhigh' | 'max' | 'ultra';

const CHATGPT_WEB_ENGINE_PREFIX = 'plugin/openai.chatgpt-web/';

function isChatGptWebEngine(value: unknown): boolean {
  return typeof value === 'string' && value.trim().startsWith(CHATGPT_WEB_ENGINE_PREFIX);
}

/** The GPT Web engine uses `default` as an internal family placeholder. */
function isProviderPlaceholderModel(value: unknown): boolean {
  return typeof value === 'string' && value.trim().toLowerCase() === 'default';
}


export function normalizeOrchestratorReasoningEffort(
  value: unknown,
): OrchestratorReasoningEffort | null {
  return value === 'none' || value === 'minimal' || value === 'low' || value === 'medium' || value === 'high' || value === 'xhigh' || value === 'max' || value === 'ultra'
    ? value
    : null;
}

export function resolveOrchestratorReasoningEffort(
  sessionConfig: Record<string, unknown> | null | undefined,
  effectiveConfig: Record<string, unknown> | null | undefined,
): OrchestratorReasoningEffort | null {
  if (sessionConfig && Object.hasOwn(sessionConfig, 'reasoningEffort') && sessionConfig.reasoningEffort === null) return null;
  if (!sessionConfig?.reasoningEffort && effectiveConfig && Object.hasOwn(effectiveConfig, 'reasoningEffort') && effectiveConfig.reasoningEffort === null) return null;
  return normalizeOrchestratorReasoningEffort(sessionConfig?.reasoningEffort)
    ?? normalizeOrchestratorReasoningEffort(effectiveConfig?.reasoningEffort)
    ?? null;
}

export function resolveOrchestratorModel(
  sessionConfig: Record<string, unknown> | null | undefined,
  effectiveConfig: Record<string, unknown> | null | undefined,
  providerModels: readonly string[] = [],
): string {
  const candidates = [
    sessionConfig?.model,
    effectiveConfig?.model,
  ];
  for (const candidate of candidates) {
    if (typeof candidate === 'string' && candidate.trim()) {
      const model = candidate.trim();
      if (isProviderPlaceholderModel(model)
        && !isChatGptWebEngine(sessionConfig?.engineId)
        && effectiveConfig?.apiProtocol !== 'chatgpt_web'
        && !providerModels.includes(model)) {
        continue;
      }
      return model;
    }
  }
  return '';
}

export function withOrchestratorReasoningEffort(
  currentConfig: Record<string, unknown>,
  reasoningEffort: OrchestratorReasoningEffort | null,
  patch: Record<string, unknown> = {},
): Record<string, unknown> {
  return {
    ...currentConfig,
    ...patch,
    reasoningEffort,
  };
}

/**
 * 提取新建会话需要继承的会话级主模型配置。
 * 只复制模型和思考强度，不携带连接地址、密钥等全局配置。
 */
export function copyOrchestratorSessionConfig(
  sessionConfig: Record<string, unknown> | null | undefined,
  effectiveConfig: Record<string, unknown> | null | undefined,
): Record<string, unknown> {
  const sourceUsesWebEngine = isChatGptWebEngine(sessionConfig?.engineId)
    || effectiveConfig?.apiProtocol === 'chatgpt_web';
  const model = sourceUsesWebEngine ? '' : resolveOrchestratorModel(sessionConfig, effectiveConfig);
  const reasoningEffort = sourceUsesWebEngine
    ? null
    : resolveOrchestratorReasoningEffort(sessionConfig, effectiveConfig);
  return {
    ...(model ? { model } : {}),
    reasoningEffort,
  };
}
