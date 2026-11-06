import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

await withGoldenViteServer(async (server) => {
  const { buildStoreItems, filterStoreItems } = await server.ssrLoadModule('/src/lib/plugin-store.ts');
  const text = (manifest, field) => manifest[field];
  const installed = (id, source) => ({
    manifest: { id, version: '1.0.0', name: id, description: '', permissions: [], contributions: {} },
    digest: 'd', grants: [], enabled: false, active: false,
    source: source ?? { kind: 'local', location: '/x.zip' },
  });
  const upstream = (over = {}) => ({
    ecosystem: 'dsh', package: 'dsh-x', version: '2.0.0', name: 'dsh-x', description: '', publisher: 'p',
    official: false, state: 'available', installable: true, ...over,
  });

  // 未安装的上游包用 生态+包名 作为临时身份，且不被文本过滤掉（registry 已按相关度匹配）。
  {
    const items = buildStoreItems([], [], [upstream()], text);
    assert.equal(items.length, 1);
    assert.equal(items[0].id, 'upstream:dsh:dsh-x');
    assert.equal(filterStoreItems(items, 'all', '完全无关的查询').length, 1);
  }
  // 搜索结果标注了已装插件 ID：并入已装条目，并按语义版本更新标记可更新。
  {
    const items = buildStoreItems(
      [installed('dsh.dsh-x', { kind: 'upstream', location: 'dsh:dsh-x@1.0.0' })], [],
      [upstream({ state: 'updatable', installedPluginId: 'dsh.dsh-x', installedVersion: '1.0.0' })], text,
    );
    assert.equal(items.length, 1);
    assert.equal(items[0].id, 'dsh.dsh-x');
    assert.equal(items[0].updatable, true);
    assert.ok(items[0].upstream);
  }
  // 刚安装完、上游搜索结果还没刷新（没有 installedPluginId）：按已装来源归并，不出现重复条目。
  {
    const items = buildStoreItems(
      [installed('dsh.dsh-x', { kind: 'upstream', location: 'dsh:dsh-x@2.0.0' })], [], [upstream()], text,
    );
    assert.equal(items.length, 1, '已装的上游包不能再以临时条目重复出现');
    assert.equal(items[0].id, 'dsh.dsh-x');
  }
  // 作用域包名：来源里的 @ 只取最后一个作为版本分隔。
  {
    const items = buildStoreItems(
      [installed('dsh.scope.pkg', { kind: 'upstream', location: 'dsh:@scope/pkg@3.1.0' })], [],
      [upstream({ package: '@scope/pkg', name: '@scope/pkg' })], text,
    );
    assert.equal(items.length, 1);
  }
  // 不同生态的同名包互不归并。
  {
    const items = buildStoreItems(
      [installed('dsh.dsh-x', { kind: 'upstream', location: 'dsh:dsh-x@1.0.0' })], [],
      [upstream({ ecosystem: 'pi' })], text,
    );
    assert.equal(items.length, 2);
  }
  console.log('plugin-store golden passed');
});
