import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { withGoldenViteServer } from './golden-vite.mjs';

await withGoldenViteServer(async (server) => {
  const module = await server.ssrLoadModule('/src/lib/relative-time.ts');
  const now = Date.parse('2026-09-19T12:00:00.000Z');

  assert.equal(module.formatRelativeTime(now - 30_000, now, 'zh-CN'), '刚刚');
  assert.equal(module.formatRelativeTime(now - 5 * 60_000, now, 'zh-CN'), '5 分钟');
  assert.equal(module.formatRelativeTime(now - 2 * 3600_000, now, 'zh-CN'), '2 小时');
  assert.equal(module.formatRelativeTime(now - 3 * 86400_000, now, 'zh-CN'), '3 天');
  assert.equal(module.formatRelativeTime(now - 31 * 86400_000, now, 'en-US'), 'Aug 19');
  // 取整过的「当前时间」会比正在更新的会话旧几秒：这不是未来，仍显示「刚刚」。
  assert.equal(module.formatRelativeTime(now + 14_000, now, 'zh-CN'), '刚刚');
  assert.equal(module.formatRelativeTime(now + 60_000, now, 'en-US'), 'just now');
  // 明显超前才是异常，退回绝对日期。
  assert.equal(module.formatRelativeTime(now + 120_000, now, 'en-US'), 'Sep 19');

  const { formatDuration, formatCompletedTime, formatTraceableTime } = await server.ssrLoadModule('/src/lib/utils.ts');
  const completedAt = new Date(2026, 9, 10, 6, 20).getTime();
  for (const [locale, expectedDuration, expectedDate] of [
    ['zh-CN', '1分8秒', '2026年10月10日 6时20分'],
    ['en-US', '1min8sec', '2026-10-10 06:20'],
  ]) {
    const dictionary = JSON.parse(await readFile(new URL(`../src/i18n/${locale}.json`, import.meta.url), 'utf8'));
    const translate = (key, vars = {}) => Object.entries(vars).reduce(
      (text, [name, value]) => text.replaceAll(`{${name}}`, String(value)), dictionary[key] ?? key,
    );
    assert.equal(formatDuration(68_000, translate), expectedDuration);
    assert.equal(formatCompletedTime(completedAt, translate), expectedDate);
    assert.equal(formatDuration(0, translate), locale === 'zh-CN' ? '0秒' : '0sec');
    assert.equal(formatDuration(999, translate), locale === 'zh-CN' ? '不足1秒' : '<1sec');
    assert.equal(formatDuration(3_668_000, translate), locale === 'zh-CN' ? '1小时1分8秒' : '1hr1min8sec');
    assert.equal(formatDuration(Number.NaN, translate), locale === 'zh-CN' ? '0秒' : '0sec');
    assert.equal(formatCompletedTime(Number.NaN, translate), '--');
    assert.equal(formatCompletedTime(new Date(2026, 0, 2, 0, 5).getTime(), translate),
      locale === 'zh-CN' ? '2026年1月2日 0时05分' : '2026-01-02 00:05');
  }
  assert.equal(formatTraceableTime(completedAt, completedAt), '06:20', '用户消息原有当天时间规则保持不变');
});

console.log('relative time golden checks passed');
