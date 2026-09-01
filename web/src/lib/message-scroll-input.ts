export type MessageScrollDirection = 'up' | 'down' | 'none';

export interface VerticalScrollMetrics {
  scrollTop: number;
  scrollHeight: number;
  clientHeight: number;
}

export function deriveMessageScrollDirection(
  nextScrollTop: number,
  previousScrollTop: number,
  thresholdPx = 1,
): MessageScrollDirection {
  const delta = nextScrollTop - previousScrollTop;
  if (!Number.isFinite(delta) || Math.abs(delta) <= thresholdPx) {
    return 'none';
  }
  return delta < 0 ? 'up' : 'down';
}

export function canScrollableElementConsumeWheel(
  deltaY: number,
  metrics: VerticalScrollMetrics,
  overflowY: string,
): boolean {
  if (!Number.isFinite(deltaY) || deltaY === 0) return false;
  if (overflowY !== 'auto' && overflowY !== 'scroll' && overflowY !== 'overlay') {
    return false;
  }
  if (
    !Number.isFinite(metrics.scrollTop)
    || !Number.isFinite(metrics.scrollHeight)
    || !Number.isFinite(metrics.clientHeight)
    || metrics.scrollHeight <= metrics.clientHeight + 1
  ) {
    return false;
  }
  return deltaY < 0
    ? metrics.scrollTop > 1
    : metrics.scrollTop + metrics.clientHeight < metrics.scrollHeight - 1;
}

export function isMainMessageWheelInput(input: {
  deltaY: number;
  ctrlKey: boolean;
  metaKey: boolean;
  nestedScrollerCanConsume: boolean;
}): boolean {
  return Number.isFinite(input.deltaY)
    && input.deltaY !== 0
    && !input.ctrlKey
    && !input.metaKey
    && !input.nestedScrollerCanConsume;
}
