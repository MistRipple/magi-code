import type { AgentExecutionStatsItem } from '../web/agent-api';
import { aggregatePromptCacheStats } from './usage-stats-aggregation';

export type SessionStatsScope = 'all' | 'orchestrator' | 'worker' | 'auxiliary';

/** A view projection of daemon totals; never estimates missing provider measurements. */
export function projectSessionStats(items: AgentExecutionStatsItem[], scope: SessionStatsScope) {
  const selected = scope === 'all' ? items : items.filter(item => item.role === scope);
  const sum = (field: 'llmCallCount' | 'inputTokens' | 'netOutputTokens') => selected.reduce((n, item) => n + (item[field] ?? 0), 0);
  const perfSum = (field: 'ttftSampleCount' | 'ttftTotalMs' | 'tpsSampleCount' | 'textOutputTokens' | 'contentDurationMs' | 'responseSampleCount' | 'responseOutputTokens' | 'responseDurationMs') => selected.reduce((n, item) => n + (item.performance?.[field] ?? 0), 0);
  const ttftCount = perfSum('ttftSampleCount');
  const tpsCount = perfSum('responseSampleCount');
  const duration = perfSum('responseDurationMs');
  const textDuration = perfSum('contentDurationMs');
  const last = selected.map(item => item.performance).filter(item => item && item.ttftSampleCount > 0)
    .sort((a, b) => (b?.lastObservedAt ?? 0) - (a?.lastObservedAt ?? 0))[0];
  return {
    tokenObserved: selected.reduce((n, item) => n + (item.tokenObservedCallCount ?? 0), 0),
    calls: sum('llmCallCount'), input: sum('inputTokens'), output: sum('netOutputTokens'),
    cache: aggregatePromptCacheStats(selected),
    writeObserved: selected.reduce((n, item) => n + (item.cache?.writeObservedCallCount ?? 0), 0),
    ttftCount, tpsCount, ttftMs: ttftCount ? perfSum('ttftTotalMs') / ttftCount : null,
    tps: duration > 0 ? perfSum('responseOutputTokens') * 1000 / duration : null,
    textTps: textDuration > 0 ? perfSum('textOutputTokens') * 1000 / textDuration : null,
    lastTtftMs: last?.lastTtftMs ?? null, lastTps: last?.lastResponseTps ?? null,
  };
}
