import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { parse } from 'svelte/compiler';
import { withGoldenViteServer } from './golden-vite.mjs';

await withGoldenViteServer(async server => {
  const { projectSessionStats } = await server.ssrLoadModule('/src/lib/session-stats.ts');
  const component = await readFile(new URL('../src/components/SessionStats.svelte', import.meta.url), 'utf8');
  const fixedSections = new Map();
  function inspect(node, conditions = []) {
    if (!node || typeof node !== 'object') return;
    if (node.type === 'IfBlock') {
      conditions = [...conditions, component.slice(node.expression.start, node.expression.end)];
    }
    const className = node.attributes?.find(attribute => attribute.name === 'class')?.value?.[0]?.data;
    if (className === 'metric-grid' || className === 'stats-details') fixedSections.set(className, conditions);
    for (const value of Object.values(node)) {
      for (const child of Array.isArray(value) ? value : [value]) inspect(child, conditions);
    }
  }
  inspect(parse(component).html);
  for (const section of ['metric-grid', 'stats-details']) {
    assert.deepEqual(fixedSections.get(section), ['open && trigger'],
      `${section} stays visible regardless of empty, loading or failed data`);
  }
  assert.match(component, /stats-empty--compact/u, 'loading and failed states retain a compact status row');
  assert.doesNotMatch(component, /input\.stats\.emptyHint/u, 'empty sessions do not add a redundant bottom explanation');
  assert.doesNotMatch(component, /input\.stats\.(?:tokenNote|performanceNote)/u, 'expanded details show fields without explanatory footnotes');
  assert.doesNotMatch(component, /stats-icon-button|input\.stats\.close|onmousedown=|onpointerup=|lastToggleAt/u,
    'statistics follows adjacent cards: no close button or duplicate pointer activation paths');

  const item = (role, count, tokens, duration, ttft, observed = true) => ({
    role, llmCallCount: count, tokenObservedCallCount: count, inputTokens: 1000, netOutputTokens: 200,
    cache: observed ? { observedCallCount: count, hitCallCount: 1, inputTokens: 1000, readTokens: 600, writeTokens: 0, tokenHitRate: .6, requestHitRate: 1 / count } : undefined,
    performance: { ttftSampleCount: count, ttftTotalMs: ttft, tpsSampleCount: 1, textOutputTokens: tokens, contentDurationMs: duration, lastObservedAt: duration, lastTtftMs: ttft / count, lastTps: tokens * 1000 / duration },
  });
  const rows = [item('orchestrator', 2, 20, 1000, 600), item('worker', 3, 180, 3000, 1900), item('auxiliary', 1, 0, 0, 0, false)];
  rows[2].performance = undefined;
  const all = projectSessionStats(rows, 'all');
  assert.equal(all.calls, 6);
  assert.equal(all.tokenObserved, 6);
  assert.equal(projectSessionStats([{ role: 'worker', llmCallCount: 1, netOutputTokens: 0 }], 'all').tokenObserved, 0, 'missing usage is unknown, not confirmed zero');
  assert.equal(all.tps, 50, 'TPS uses summed tokens / duration, not mean request speed');
  assert.equal(all.ttftMs, 500, 'TTFT uses number of measured streams');
  assert.equal(all.cache.tokenHitRate, .6, 'unobserved requests do not dilute cache rates');
  assert.equal(all.cache.observedCallCount, 5);
  assert.equal(all.lastTps, 60);
  assert.equal(projectSessionStats(rows, 'worker').calls, 3);
  assert.equal(projectSessionStats(rows, 'orchestrator').tps, 20);
  assert.equal(projectSessionStats(rows, 'auxiliary').cache.tokenHitRate, null);
  assert.equal(projectSessionStats([], 'all').tps, null);
  assert.equal(projectSessionStats([], 'all').ttftMs, null);
  const zero = item('orchestrator', 1, 0, 0, 0);
  zero.cache.readTokens = 0; zero.cache.hitCallCount = 0;
  assert.equal(projectSessionStats([zero], 'all').cache.tokenHitRate, 0, 'explicit zero remains zero');
  assert.equal(projectSessionStats([zero], 'all').tps, null, 'zero duration cannot produce infinite speed');

  // Multiple models/agents of one role must be combined before calculating ratios.
  const main = item('orchestrator', 2, 20, 1000, 600);
  const workerA = item('worker', 3, 180, 3000, 1900);
  const workerB = item('worker', 1, 10, 2000, 100);
  workerB.cache = { observedCallCount: 1, hitCallCount: 0, inputTokens: 9000, readTokens: 0, writeTokens: 500, writeObservedCallCount: 1 };
  workerB.performance.lastObservedAt = 4000;
  const missing = { role: 'worker', llmCallCount: 1, netOutputTokens: 0 };
  const image = { role: 'image_generation', llmCallCount: 1, tokenObservedCallCount: 0 };
  const mixed = [main, workerA, workerB, missing, image];
  const worker = projectSessionStats(mixed, 'worker');
  assert.equal(worker.calls, 5);
  assert.equal(worker.tokenObserved, 4, 'unreported worker usage stays outside token coverage');
  assert.equal(worker.output, 400);
  assert.equal(worker.ttftCount, 4);
  assert.equal(worker.ttftMs, 500);
  assert.equal(worker.tps, 38, 'combine worker body tokens and measured durations across models');
  assert.equal(worker.lastTps, 5, 'latest measurement is selected by timestamp, not speed or duration');
  assert.equal(worker.cache.tokenHitRate, .06, 'cache denominator is observed input, not calls or average percentages');
  assert.equal(worker.cache.requestHitRate, .25);
  assert.equal(worker.writeObserved, 1);
  assert.equal(worker.cache.writeTokens, 500);
  assert.equal(projectSessionStats(mixed, 'orchestrator').calls, 2, 'worker and image calls cannot leak into main stats');
  assert.equal(projectSessionStats(mixed, 'all').calls, 8, 'all roles includes non-text calls');
  console.log('session stats projection golden checks passed');

});
