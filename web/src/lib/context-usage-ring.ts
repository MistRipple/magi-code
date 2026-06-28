// 上下文用量圆环的纯逻辑层。
// 抽离自 ContextUsageRing.svelte，便于在 golden 测试里覆盖多场景，
// 同时与 RuntimeStatePanel 的 budget 展示约定保持一致。

export type ContextRingTone = 'normal' | 'notice' | 'warning' | 'danger';

export interface ContextRingInput {
  usageRatio?: number | null;
  tokenUsed?: number | null;
  remainingTokens?: number | null;
  tokenLimit?: number | null;
  warningLevel?: ContextRingTone | string | null;
}

export interface ContextRingGeometry {
  radius: number;
  circumference: number;
  dashOffset: number;
}

export interface ContextRingView {
  hasData: boolean;
  clampedRatio: number;
  percentText: string;
  labelText: string;
  tone: ContextRingTone;
  geometry: ContextRingGeometry;
}

export interface ContextRingDetailItem {
  key: 'usage' | 'remaining' | 'limit';
  text: string;
}

export const RING_RADIUS = 7;

export function hasUsageData(ratio: number | null | undefined): boolean {
  return ratio != null && Number.isFinite(ratio);
}

export function clampUsageRatio(ratio: number | null | undefined): number {
  if (!hasUsageData(ratio)) return 0;
  return Math.min(1, Math.max(0, ratio as number));
}

export function resolveEffectiveUsageRatio(input: ContextRingInput): number | null {
  if (hasUsageData(input.usageRatio) && Number(input.usageRatio) > 0) {
    return input.usageRatio as number;
  }
  const tokenUsed = input.tokenUsed;
  const tokenLimit = input.tokenLimit;
  if (
    tokenUsed != null
    && tokenLimit != null
    && Number.isFinite(tokenUsed)
    && Number.isFinite(tokenLimit)
    && tokenUsed > 0
    && tokenLimit > 0
  ) {
    return tokenUsed / tokenLimit;
  }
  return hasUsageData(input.usageRatio) ? input.usageRatio as number : null;
}

export function resolveRingTone(level: string | null | undefined): ContextRingTone {
  switch (level) {
    case 'notice':
    case 'warning':
    case 'danger':
      return level;
    default:
      return 'normal';
  }
}

export function formatRingTokens(n: number | null | undefined): string {
  if (n == null || !Number.isFinite(n)) return '--';
  if (n < 1000) return `${n}`;
  return `${(n / 1000).toFixed(1)}k`;
}

export function formatRingPercent(ratio: number | null | undefined): string {
  if (!hasUsageData(ratio)) return '--';
  const clamped = clampUsageRatio(ratio);
  if (clamped > 0 && clamped < 0.01) {
    return '<1';
  }
  return `${Math.round(clamped * 100)}`;
}

export function computeRingGeometry(
  ratio: number | null | undefined,
  radius: number = RING_RADIUS,
): ContextRingGeometry {
  const circumference = 2 * Math.PI * radius;
  const dashOffset = circumference * (1 - clampUsageRatio(ratio));
  return { radius, circumference, dashOffset };
}

export function resolveRingView(
  input: ContextRingInput,
  radius: number = RING_RADIUS,
): ContextRingView {
  const effectiveUsageRatio = resolveEffectiveUsageRatio(input);
  const has = hasUsageData(effectiveUsageRatio);
  const clampedRatio = clampUsageRatio(effectiveUsageRatio);
  const percentText = formatRingPercent(effectiveUsageRatio);
  return {
    hasData: has,
    clampedRatio,
    percentText,
    labelText: has ? `${percentText}%` : percentText,
    tone: resolveRingTone(input.warningLevel ?? null),
    geometry: computeRingGeometry(effectiveUsageRatio, radius),
  };
}

type Translate = (key: string, params?: Record<string, string | number>) => string;

export function buildRingDetailItems(input: ContextRingInput, t: Translate): ContextRingDetailItem[] {
  if (!hasUsageData(resolveEffectiveUsageRatio(input))) {
    return [];
  }
  return [
    { key: 'usage', text: t('input.contextRing.usage', { value: formatRingTokens(input.tokenUsed) }) },
    {
      key: 'remaining',
      text: t('input.contextRing.remaining', { value: formatRingTokens(input.remainingTokens) }),
    },
    { key: 'limit', text: t('input.contextRing.limit', { value: formatRingTokens(input.tokenLimit) }) },
  ];
}

export function buildRingTooltip(input: ContextRingInput, t: Translate): string {
  const effectiveUsageRatio = resolveEffectiveUsageRatio(input);
  if (!hasUsageData(effectiveUsageRatio)) {
    return t('input.contextRing.empty');
  }
  const percent = formatRingPercent(effectiveUsageRatio);
  return [
    `${t('input.contextRing.label')} ${percent}%`,
    ...buildRingDetailItems(input, t).map((item) => item.text),
  ].join(' · ');
}
