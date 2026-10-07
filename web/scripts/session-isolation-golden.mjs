import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { withGoldenViteServer } from './golden-vite.mjs';

const read = (path) => readFile(new URL(path, import.meta.url), 'utf8');

await withGoldenViteServer(async (server) => {
  const merge = await server.ssrLoadModule('/src/lib/isolation-merge.ts');

  const entries = [
    { path: 'a.txt', action: 'modify', state: 'clean', contentKind: 'text', size: 1 },
    { path: 'b.txt', action: 'add', state: 'clean', contentKind: 'text', size: 1 },
    { path: 'c.txt', action: 'modify', state: 'already_applied', contentKind: 'text', size: 1 },
    { path: 'd.txt', action: 'modify', state: 'conflict', conflict: 'both_modified', contentKind: 'text', size: 1 },
    { path: 'e.txt', action: 'delete', state: 'conflict', conflict: 'modified_in_source', contentKind: 'text', size: 1 },
  ];
  assert.deepEqual(merge.summarizeMergePlan(entries), { clean: 2, alreadyApplied: 1, conflict: 2 });

  // 干净文件默认全部合并；没有给出处理方式的冲突不发给后端。
  assert.deepEqual(
    merge.buildMergeRequest(entries, { excluded: new Set(), resolutions: {} }),
    { paths: ['a.txt', 'b.txt', 'c.txt'], resolutions: {} },
  );
  // 取消勾选的文件不合并；选择「用副本的版本」的冲突才会合并，「保留主工作区」留在副本里。
  assert.deepEqual(
    merge.buildMergeRequest(entries, {
      excluded: new Set(['b.txt']),
      resolutions: { 'd.txt': 'use_session', 'e.txt': 'keep_source' },
    }),
    { paths: ['a.txt', 'c.txt', 'd.txt'], resolutions: { 'd.txt': 'use_session' } },
  );

  assert.equal(merge.conflictLabelKey('both_modified'), 'isolation.merge.conflict.bothModified');
  assert.equal(merge.conflictLabelKey('deleted_in_source'), 'isolation.merge.conflict.deletedInSource');
  assert.equal(merge.conflictLabelKey(undefined), 'isolation.merge.conflict.bothModified');
  assert.equal(merge.isolationOriginBlockingSession({ kind: 'manual' }), null);
  assert.equal(
    merge.isolationOriginBlockingSession({ kind: 'contention', blocking_session_id: 'other' }),
    'other',
  );

  // 接线：接口、事件、输入区芯片、变更面板横幅和合并面板都要接上。
  const api = await read('../src/web/agent-api.ts');
  for (const endpoint of [
    '/api/session/isolations',
    '/api/session/isolation',
    '/api/session/isolation/enable',
    '/api/session/isolation/discard',
    '/api/session/isolation/merge-plan',
    '/api/session/isolation/merge',
  ]) {
    assert.ok(api.includes(`'${endpoint}'`), `agent-api 缺少 ${endpoint}`);
  }
  const bridge = await read('../src/shared/bridges/web-client-bridge.ts');
  assert.match(bridge, /session\.isolation\.changed/);
  assert.match(bridge, /session\.isolation\.merged/);
  const input = await read('../src/components/InputArea.svelte');
  assert.match(input, /<SessionIsolationChip/);
  const edits = await read('../src/components/EditsPanel.svelte');
  assert.match(edits, /<IsolationBanner/);
  const app = await read('../src/App.svelte');
  assert.match(app, /<IsolationMergeDialog/);

  // 文案：组件里用到的 isolation.* / edits.isolated.* 键在中英文里都要有。
  const zh = JSON.parse(await read('../src/i18n/zh-CN.json'));
  const en = JSON.parse(await read('../src/i18n/en-US.json'));
  const used = new Set();
  for (const file of [
    '../src/components/SessionIsolationChip.svelte',
    '../src/components/IsolationBanner.svelte',
    '../src/components/IsolationMergeDialog.svelte',
    '../src/components/EditsPanel.svelte',
    '../src/web/WebWorkbenchShell.svelte',
    '../src/lib/isolation-merge.ts',
  ]) {
    const source = await read(file);
    for (const match of source.matchAll(/['"`]((?:isolation|edits\.isolated)\.[A-Za-z0-9_.]+)['"`]/g)) {
      used.add(match[1]);
    }
  }
  for (const action of ['add', 'modify', 'delete']) used.add(`isolation.merge.action.${action}`);
  assert.ok(used.size > 20, `应当扫描到足够多的文案键，实际 ${used.size}`);
  for (const key of used) {
    assert.ok(key in zh, `zh-CN 缺少 ${key}`);
    assert.ok(key in en, `en-US 缺少 ${key}`);
  }

  console.log('session isolation golden passed');
});
