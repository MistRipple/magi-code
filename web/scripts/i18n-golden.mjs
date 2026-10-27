import assert from 'node:assert/strict';
import { readFile, readdir } from 'node:fs/promises';
import { withGoldenViteServer } from './golden-vite.mjs';
import { svelte } from '@sveltejs/vite-plugin-svelte';

const root = new URL('../src/', import.meta.url);
const dictionaries = {};
for (const locale of ['zh-CN', 'en-US']) {
  const text = await readFile(new URL(`i18n/${locale}.json`, root), 'utf8');
  const keys = [...text.matchAll(/^\s*"([^"\n]+)":/gm)].map(match => match[1]);
  assert.equal(new Set(keys).size, keys.length, `${locale} 不得有重复键`);
  dictionaries[locale] = JSON.parse(text);
}
const zh = dictionaries['zh-CN'];
const en = dictionaries['en-US'];
assert.deepEqual(Object.keys(zh).sort(), Object.keys(en).sort(), '中英文字典的键必须一致');
for (const key of Object.keys(zh)) {
  if (key !== 'settings.conversationDisplay.status.idle') {
    assert.ok(zh[key].trim() && en[key].trim(), `${key} 不得为空`);
  }
  assert.doesNotMatch(en[key], /[\u4e00-\u9fff]/u, `${key} 英文字典不得残留中文`);
  // 完成日期由同一组日期值生成，两种语言有意使用不同的补零字段。
  if (key === 'time.completedAt') continue;
  const variables = text => [...new Set(text.match(/\{\w+\}/g) ?? [])].sort();
  assert.deepEqual(variables(zh[key]), variables(en[key]), `${key} 插值变量必须一致`);
}
let sourceCount = 0;
async function checkSources(directory) {
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    if (['demos', 'acceptance', 'i18n'].includes(entry.name)) continue;
    const url = new URL(entry.name + (entry.isDirectory() ? '/' : ''), directory);
    if (entry.isDirectory()) { await checkSources(url); continue; }
    if (!/\.(?:svelte|ts)$/.test(entry.name)) continue;
    sourceCount++;
    const source = (await readFile(url, 'utf8')).replace(/\/\*[\s\S]*?\*\/|<!--[\s\S]*?-->/g, '');
    for (const match of source.matchAll(/i18n\.t\(\s*['"]([^'"\n]+)['"]\s*[,)]/g)) {
      assert.ok(Object.hasOwn(zh, match[1]), `${url.pathname} 引用了不存在的翻译键 ${match[1]}`);
    }
  }
}
await checkSources(root);

await withGoldenViteServer(async server => {
  const { i18n } = await server.ssrLoadModule('/src/stores/i18n.svelte.ts');
  const labels = await server.ssrLoadModule('/src/lib/task-labels.ts');
  const themes = await server.ssrLoadModule('/src/appearance/labels.ts');
  const appearance = await server.ssrLoadModule('/src/appearance/contract.ts');
  const { formatDuration } = await server.ssrLoadModule('/src/lib/utils.ts');
  try {
    for (const [locale, status, taskName, themeName] of [
      ['zh-CN', '执行中', '未命名任务', 'Magi 深色'],
      ['en-US', 'Running', 'Untitled task', 'Magi Dark'],
      ['zh-CN', '执行中', '未命名任务', 'Magi 深色'],
    ]) {
      i18n.setLocale(locale);
      assert.equal(labels.getTaskStatusLabel('running'), status, '切换语言必须即时更新状态');
      assert.equal(labels.getTaskDisplayTitle({ title: '' }), taskName);
      assert.equal(labels.getTaskDisplayTitle({ title: '用户的中文标题' }), '用户的中文标题', '用户内容保持原样');
      assert.equal(themes.themeDisplayName({ id: 'builtin.dark', name: 'Magi 深色' }), themeName);
      assert.equal(themes.themeDisplayName({ id: 'user.test', name: '我的主题' }), '我的主题', '自定义主题不改名');
      assert.equal(formatDuration(68_000, (key, vars) => i18n.t(key, vars)), locale === 'en-US' ? '1min8sec' : '1分8秒');
      assert.throws(() => appearance.normalizeAppearanceSnapshot(null),
        locale === 'en-US' ? /Invalid appearance state/ : /外观状态格式无效/);
      for (const kind of labels.USER_VISIBLE_TASK_KINDS) {
        const label = labels.getTaskKindLabel(kind);
        if (locale === 'en-US') assert.doesNotMatch(label, /[\u4e00-\u9fff]/u);
      }
      assert.equal(labels.getRunnerUserState('error'), 'finished', '本地化不得改变业务枚举');
    }
  } finally { i18n.setLocale('zh-CN'); }
}, { plugins: [svelte()] });
console.log(`i18n golden checks passed (${Object.keys(zh).length} keys, ${sourceCount} sources)`);
