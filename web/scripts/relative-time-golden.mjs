import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

await withGoldenViteServer(async (server) => {
  const module = await server.ssrLoadModule('/src/lib/relative-time.ts');
  const now = Date.parse('2026-09-19T12:00:00.000Z');

  assert.equal(module.formatRelativeTime(now - 30_000, now, 'zh-CN'), '刚刚');
  assert.equal(module.formatRelativeTime(now - 5 * 60_000, now, 'zh-CN'), '5 分钟');
  assert.equal(module.formatRelativeTime(now - 2 * 3600_000, now, 'zh-CN'), '2 小时');
  assert.equal(module.formatRelativeTime(now - 3 * 86400_000, now, 'zh-CN'), '3 天');
  assert.equal(module.formatRelativeTime(now - 31 * 86400_000, now, 'en-US'), 'Aug 19');
  assert.equal(module.formatRelativeTime(now + 60_000, now, 'en-US'), 'Sep 19');
});

console.log('relative time golden checks passed');
