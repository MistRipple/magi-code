import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { withGoldenViteServer } from './golden-vite.mjs';

const desktopCapability = JSON.parse(await readFile(
  path.resolve('../apps/desktop/capabilities/default.json'),
  'utf8',
));

assert.deepEqual(
  desktopCapability.remote?.urls,
  ['http://127.0.0.1:*', 'http://localhost:*'],
  'Desktop 主窗口加载 daemon HTTP 页面时必须为本机来源启用 Tauri capability',
);

await withGoldenViteServer(async (server) => {
  const desktopDrop = await server.ssrLoadModule('/src/lib/desktop-file-drop.ts');
  const contextReferences = await server.ssrLoadModule('/src/lib/composer-context-references.ts');

  assert.deepEqual(
    desktopDrop.physicalToCssPoint({ x: 400, y: 240 }, 2),
    { x: 200, y: 120 },
    'Tauri 物理坐标必须按设备像素比转换为 CSS 坐标',
  );

  const zones = {
    sidebar: { left: 0, top: 0, right: 280, bottom: 800, width: 280, height: 800 },
    conversation: { left: 288, top: 56, right: 1200, bottom: 800, width: 912, height: 744 },
  };
  assert.equal(
    desktopDrop.resolveDesktopDropZone({ x: 120, y: 300 }, zones),
    'sidebar',
    '左侧区域必须路由为工作空间拖放',
  );
  assert.equal(
    desktopDrop.resolveDesktopDropZone({ x: 640, y: 300 }, zones),
    'conversation',
    '对话区域必须路由为上下文引用拖放',
  );
  assert.equal(
    desktopDrop.resolveDesktopDropZone({ x: 640, y: 20 }, zones),
    null,
    '顶部工具栏和其他区域不得处理拖放',
  );

  assert.deepEqual(
    desktopDrop.normalizeDesktopDropPaths(
      ['/tmp/a.txt', '/tmp/a.txt', ' /tmp/folder/ ', '/tmp/b.txt'],
      ['/tmp/b.txt'],
      2,
    ),
    ['/tmp/a.txt', '/tmp/folder'],
    '拖入路径必须去重、排除已有引用并遵守剩余数量上限',
  );
  assert.deepEqual(
    desktopDrop.normalizeDesktopDropPaths(['/tmp/a.txt'], [], 0),
    [],
    '剩余引用额度为零时不得再接收任何路径',
  );

  assert.deepEqual(
    desktopDrop.resolveDesktopDroppedPath('/tmp/folder', {
      path: '/private/tmp/folder',
      parent: '/private/tmp',
      entries: [],
    }),
    { kind: 'directory', path: '/private/tmp/folder', name: 'folder' },
    '目录拖放必须使用后端规范化后的目录路径',
  );
  assert.deepEqual(
    desktopDrop.resolveDesktopDroppedPath('/tmp/a.txt', {
      path: '/private/tmp',
      parent: '/private',
      entries: [],
      selectedPath: '/private/tmp/a.txt',
      selectedKind: 'file',
    }),
    { kind: 'file', path: '/private/tmp/a.txt', name: 'a.txt' },
    '文件拖放必须使用后端规范化后的文件路径',
  );

  let loaderCalls = 0;
  const inactiveStop = await desktopDrop.registerDesktopFileDropListener(() => {}, {
    isDesktopRuntime: () => false,
    loadCurrentWebview: async () => {
      loaderCalls += 1;
      throw new Error('非 Desktop 不得加载 Tauri API');
    },
  });
  inactiveStop();
  assert.equal(loaderCalls, 0, '浏览器和手机运行时不得注册原生拖放监听');

  let capturedHandler = null;
  let unlistenCalls = 0;
  const receivedEvents = [];
  const stop = await desktopDrop.registerDesktopFileDropListener((event) => {
    receivedEvents.push(event);
  }, {
    isDesktopRuntime: () => true,
    loadCurrentWebview: async () => ({
      onDragDropEvent: async (handler) => {
        capturedHandler = handler;
        return () => { unlistenCalls += 1; };
      },
    }),
  });
  assert.equal(typeof capturedHandler, 'function', 'Desktop 必须注册当前 Webview 的拖放监听');
  capturedHandler({ payload: { type: 'leave' } });
  assert.deepEqual(receivedEvents, [{ type: 'leave' }], '监听适配层必须解包 Tauri 事件 payload');
  stop();
  assert.equal(unlistenCalls, 1, '组件销毁时必须解除原生拖放监听');

  const windowsRoot = contextReferences.addComposerContextReference([], {
    kind: 'directory',
    path: 'C:\\',
    name: 'C:',
  });
  assert.equal(windowsRoot[0]?.path, 'C:\\', 'Windows 盘符根目录不得被错误裁剪为相对盘符');
});

const inputAreaSource = await readFile(
  new URL('../src/components/InputArea.svelte', import.meta.url),
  'utf8',
);
const shellSource = await readFile(
  new URL('../src/web/WebWorkbenchShell.svelte', import.meta.url),
  'utf8',
);
const threadPanelSource = await readFile(
  new URL('../src/components/ThreadPanel.svelte', import.meta.url),
  'utf8',
);

assert.match(
  threadPanelSource,
  /data-desktop-drop-zone="conversation"/,
  '对话面板必须提供明确的原生拖放命中边界',
);
assert.match(
  inputAreaSource,
  /window\.addEventListener\(DESKTOP_CONTEXT_DROP_EVENT, handleDesktopContextDrop/,
  '输入区必须接收工作台路由后的 Desktop 上下文拖放事件',
);
assert.match(
  inputAreaSource,
  /async function handleDesktopContextDrop[\s\S]*?browseAgentDirectory\(path\)[\s\S]*?resolveDesktopDroppedPath[\s\S]*?addContextReference/,
  '拖入对话区的路径必须复用后端路径校验与现有上下文引用状态',
);
assert.match(
  inputAreaSource,
  /const dropScopeKey =[\s\S]*?browseAgentDirectory\(path\)[\s\S]*?if \(currentComposerReferenceScopeKey\(\) !== dropScopeKey\) return;/,
  '异步路径解析完成前若用户切换会话或工作区，旧拖放结果不得串入新输入区',
);
assert.match(
  shellSource,
  /registerDesktopFileDropListener[\s\S]*?resolveDesktopDropZone/,
  '工作台必须作为唯一原生拖放监听拥有者并统一路由区域',
);
assert.match(
  shellSource,
  /handleDesktopWorkspaceDrop[\s\S]*?browseAgentDirectory[\s\S]*?kind !== 'directory'[\s\S]*?registerWorkspaceRoot\([^,]+, true\)/,
  '左侧拖放必须只接受目录，并通过共享注册流程进入草稿态',
);
assert.match(
  shellSource,
  /desktopDropIndicator[\s\S]*?desktop-drop-overlay/,
  '有效拖放区域必须提供轻量视觉反馈',
);

console.log('desktop file drop golden passed');
