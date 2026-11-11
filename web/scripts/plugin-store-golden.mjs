import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

await withGoldenViteServer(async (server) => {
  const { buildStoreItems, filterStoreItems, installedUpstreamRequest, pluginImpactNotices } = await server.ssrLoadModule('/src/lib/plugin-store.ts');
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

  // 同一已声明事实供安装预览与激活使用；不按包名、优先级或当前启用状态推断能力。
  {
    const manifest = installed('arbitrary').manifest;
    const t = (key, vars) => `${key}${vars?.locations ? ':' + vars.locations : ''}`;
    assert.deepEqual(pluginImpactNotices(manifest, t), []);
    manifest.contributions = { views: [
      { id:'business', title:'Business', entry:'ui/page.html', placements:['main','rightPane'] },
      { id:'input', title:'Action', entry:'ui/action.html', placements:['composerLeft'] },
      { id:'duplicate', title:'Other action', entry:'ui/other.html', placements:['composerLeft'] },
    ] };
    const views = pluginImpactNotices(manifest, t);
    assert.equal(views.length, 1);
    assert.match(views[0].text, /impact.main.*impact.rightPane.*impact.composerLeft/);
    assert.equal(views[0].text.split('impact.composerLeft').length, 2, '重复入口不重复告知同一位置');
    manifest.extension = { hooks:[{name:'prompt.contribute',mode:'advise'},{name:'tool.call',mode:'guard'}] };
    manifest.permissions = [{kind:'contextRewrite',targets:['history']}];
    assert.deepEqual(pluginImpactNotices(manifest, t).map(notice => notice.id), ['ui','global-prompt','guard','context']);
  }

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
      [installed('dsh.dsh-x', { kind: 'upstream', location: 'dsh:dsh-x@1.0.0', github: { repository: 'owner/repo', commit: 'a'.repeat(40) } })], [],
      [upstream({ state: 'updatable' })], text,
    );
    assert.equal(items.length, 1);
    assert.equal(items[0].upstream, null, 'npm 搜索结果不替换已安装 GitHub 原包的来源');
    assert.equal(items[0].updatable, false, 'npm 版本不能当作仓库更新');
  }
  // 不同生态的同名包互不归并。
  {
    const items = buildStoreItems(
      [installed('dsh.dsh-x', { kind: 'upstream', location: 'dsh:dsh-x@1.0.0' })], [],
      [upstream({ ecosystem: 'pi' })], text,
    );
    assert.equal(items.length, 2);
  }
  // 搜索跨状态分组按匹配度：精确包名优先，其次作用域短名、前缀、描述与 registry 相关结果。
  {
    const names = ['unrelated-z', 'dsh-rea-agents', '@scope/dsh-rea', 'dsh-rea', 'unrelated-a'];
    const items = buildStoreItems([{
      ...installed('local-active'), active: true,
      manifest: { ...installed('local-active').manifest, description: 'dsh-rea related' },
    }], [], names.map((name) => upstream({ package: name, name })), text);
    assert.deepEqual(filterStoreItems(items, 'all', 'dsh-rea').map((i) => i.name),
      ['dsh-rea', '@scope/dsh-rea', 'dsh-rea-agents', 'local-active', 'unrelated-z', 'unrelated-a']);
    assert.equal(filterStoreItems(items, 'available', 'DSH-REA')[0].name, 'dsh-rea');
    assert.equal(filterStoreItems(items, 'installed', 'dsh-rea')[0].id, 'local-active');
  }
  // 刚安装后的条目尚未拿到上游刷新结果，也能用来源包名搜索并保持选择。
  {
    const items = buildStoreItems([installed('custom-title', { kind: 'upstream', location: 'dsh:dsh-rea@1.2.0' })], [], [], text);
    assert.equal(filterStoreItems(items, 'all', 'dsh-rea').length, 1);
  }
  // 运行组件更新不依赖 registry 搜索成功，也不把同版原包伪装成新版本。
  {
    const original = {
      ...installed('dsh.scope.pkg', { kind: 'upstream', location: 'dsh:@scope/pkg@1.0.0' }),
      translationRuntime: { state: 'updateAvailable', currentId: 'a'.repeat(64) },
    };
    const items = buildStoreItems([original], [], [], text);
    assert.equal(filterStoreItems(items, 'updatable', '').length, 1);
    assert.equal(items[0].version, '1.0.0');
    assert.deepEqual(installedUpstreamRequest(original), { ecosystem: 'dsh', package: '@scope/pkg', version: '1.0.0' });
    const github = { repository: 'owner/repo', reference: 'main', commit: 'b'.repeat(40) };
    assert.deepEqual(installedUpstreamRequest({ ...original, source: { ...original.source, github } }).github, github);
    const current = { ...original, translationRuntime: { state: 'current' } };
    assert.equal(filterStoreItems(buildStoreItems([current], [], [], text), 'updatable', '').length, 0);
    assert.equal(installedUpstreamRequest(installed('local')), null);
  }
  console.log('plugin-store golden passed');
});
