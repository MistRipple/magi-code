import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';

const storeSource = await readFile(
  new URL('../src/stores/right-pane.svelte.ts', import.meta.url),
  'utf8',
);
const shellSource = await readFile(
  new URL('../src/web/WebWorkbenchShell.svelte', import.meta.url),
  'utf8',
);
const headerSource = await readFile(
  new URL('../src/components/Header.svelte', import.meta.url),
  'utf8',
);
const appSource = await readFile(
  new URL('../src/App.svelte', import.meta.url),
  'utf8',
);
const surfaceManagerSource = await readFile(
  new URL('../../apps/desktop/src/main/browser-surface-manager.ts', import.meta.url),
  'utf8',
);
const desktopControlSource = await readFile(
  new URL('../../apps/desktop/src/main/desktop-control-server.ts', import.meta.url),
  'utf8',
);
const preloadSource = await readFile(
  new URL('../../apps/desktop/src/preload/index.ts', import.meta.url),
  'utf8',
);
const mainSource = await readFile(
  new URL('../../apps/desktop/src/main/index.ts', import.meta.url),
  'utf8',
);
const windowManagerSource = await readFile(
  new URL('../../apps/desktop/src/main/window-manager.ts', import.meta.url),
  'utf8',
);
const desktopTypesSource = await readFile(
  new URL('../src/types/magi-desktop.d.ts', import.meta.url),
  'utf8',
);
const contractSource = await readFile(
  new URL('../../contracts/desktop-browser/src/index.ts', import.meta.url),
  'utf8',
);
const schemaSource = await readFile(
  new URL('../../contracts/desktop-browser/desktop-ipc.schema.json', import.meta.url),
  'utf8',
);

assert.doesNotMatch(storeSource, /openRightPaneTab|forwardDesktopRightPaneIntent|DESKTOP_RIGHT_PANE_INTENT_VERSION/, '右栏 Store 不得再把本地 Tab 意图转发给 Main');
assert.match(storeSource, /upsertTab\([\s\S]*?kind,[\s\S]*?payload/, '右栏 Store 必须由本地 upsertTab 维护唯一面板状态');
assert.match(shellSource, /readyRightPane\(\)/, '桌面首次显示仍需要一次生命周期握手，但该调用不得携带右栏 Tab 意图');
assert.doesNotMatch(shellSource, /openRightPaneTab|forwardDesktopRightPaneIntent|onRightPaneIntent\(/, 'Workbench Shell 不得重新引入跨 Renderer 右栏 Tab 意图通道');
for (const [name, source] of [
  ['preload', preloadSource],
  ['main', mainSource],
  ['window manager', windowManagerSource],
  ['desktop types', desktopTypesSource],
  ['contract', contractSource],
  ['IPC schema', schemaSource],
]) {
  assert.doesNotMatch(
    source,
    /openRightPaneTab|DesktopRightPane|DESKTOP_RIGHT_PANE_INTENT_VERSION|parseRightPaneIntent|panelForRightPaneIntent/,
    `${name} 不得保留已删除的右栏双 Renderer 协议`,
  );
}
assert.match(preloadSource, /readyRightPane:\s*\(\) => ipcRenderer\.invoke\("magi-desktop:right-pane-ready"\)/, 'preload 必须保留首次窗口显示握手');
assert.match(mainSource, /ipcMain\.handle\("magi-desktop:right-pane-ready"[\s\S]*?manager\.handleRightPaneReady\(windowId\)/, 'Main 必须保留 readyRightPane 首次窗口显示握手');
assert.match(windowManagerSource, /handleRightPaneReady\(windowId: string\): void[\s\S]*?record\.window\.show\(\)/, 'WindowManager 的 ready 握手必须负责首次显示窗口');
assert.match(desktopTypesSource, /readyRightPane\(\): Promise<void>/, '桌面桥类型必须保留 readyRightPane 握手');
assert.match(schemaSource, /"magi-desktop:right-pane-ready"/, 'IPC schema 必须保留 readyRightPane 空载荷通道');
assert.match(shellSource, /desktop-right-pane-column[\s\S]*?desktopSurface=\{true\}/, '右栏多功能 UI 必须与主工作台位于同一 Renderer');
assert.doesNotMatch(shellSource, /onRightPaneIntent\(/, '统一 Renderer 不得保留旧的跨 Renderer 面板意图接收路径');
assert.match(surfaceManagerSource, /Emulation\.setDeviceMetricsOverride/, '固定视口必须使用 Chromium 设备仿真');
assert.doesNotMatch(
  surfaceManagerSource,
  /setPageScaleFactor|fixedViewportScale|scale:\s*scale/,
  '固定视口不得通过 pageScaleFactor 或按容器拟合缩放造成失真',
);
assert.match(
  surfaceManagerSource,
  /WebContentsView[\s\S]*?materialize[\s\S]*?Emulation\.setDeviceMetricsOverride/,
  '浏览器必须由 Electron Main 管理真实 Chromium WebContentsView，并保留 CDP 能力',
);
assert.doesNotMatch(
  surfaceManagerSource,
  /<webview|registerBrowserGuest|setSurfaceSlotBounds|setActiveSurface/,
  '浏览器不得保留旧的 Guest 注册或投影路径',
);
const materializeStart = surfaceManagerSource.indexOf('async materialize(');
const materializeEnd = surfaceManagerSource.indexOf('private async createSurface(', materializeStart);
const materializeSource = surfaceManagerSource.slice(materializeStart, materializeEnd);
const loadPageStart = surfaceManagerSource.indexOf('private async loadPage(');
const loadPageEnd = surfaceManagerSource.indexOf('private async waitForNavigation(', loadPageStart);
const loadPageSource = surfaceManagerSource.slice(loadPageStart, loadPageEnd);
assert.match(
  materializeSource,
  /const initialUrl = normalizeNavigableUrl\(input\.initialUrl\);[\s\S]*?const load = this\.startLoad\(record, initialUrl\)[\s\S]*?if \(input\.awaitPageLoad !== false\) await load/,
  'Surface materialize 必须先规范化 canonical URL，再交给唯一 loadPage 导航所有者；窗口激活可异步等待网络',
);
assert.doesNotMatch(
  materializeSource,
  /contents\.loadURL|void\s+record\.contents\.loadURL/,
  'Surface materialize 不得直接或 fire-and-forget 调用 loadURL',
);
assert.match(
  loadPageSource,
  /private async loadPage\([\s\S]*?record\.contents\.loadURL\(url\)/,
  '唯一 loadPage 导航所有者必须使用规范化后的 url 调用 WebContents.loadURL',
);
assert.equal(
  loadPageSource.match(/record\.contents\.loadURL\(url\)/g)?.length,
  1,
  'loadPage 内只能存在一个真实导航调用点',
);
assert.match(
  desktopControlSource,
  /case "create_page":[\s\S]*?case "restore_page":[\s\S]*?surfaceManager\.materialize\(/,
  'Worker 创建或恢复页面时必须先由 Main 物化真实 WebContentsView，不能返回无 Surface 的伪 page_state',
);
assert.doesNotMatch(
  desktopControlSource,
  /if \(!binding\)[\s\S]*?type: "page_state"[\s\S]*?return succeeded/,
  'Desktop Control 不得在缺少 Surface 时伪造页面已就绪',
);
assert.doesNotMatch(
  shellSource,
  /desktopVisibilityIntentKey/,
  '右栏可见性不得用未确认的意图 key 去重，否则后续同意图会被吞掉',
);
assert.match(
  shellSource,
  /desktopVisibilitySyncRequest[\s\S]*?desktopVisibilitySyncEpoch[\s\S]*?right_pane_visibility/,
  '右栏可见性必须存在可追踪的单请求同步状态机',
);
assert.match(
  shellSource,
  /if \(!snapshot\) return;[\s\S]*?if \(desktopRightPaneVisible === desiredVisible\)[\s\S]*?if \(desktopVisibilitySyncRequest\) return;/,
  '右栏可见性必须以 Main 快照确认作为收敛条件，并限制单条在途请求',
);
assert.match(
  shellSource,
  /if \(desktopRightPaneVisible === desiredVisible\) \{\s*return;\s*\}/,
  '快照确认不能提前清理仍未完成的可见性 IPC 请求',
);
assert.match(
  shellSource,
  /submitLayoutIntent\([\s\S]*?\.then\(\(nextSnapshot\)[\s\S]*?applyDesktopSnapshot\(nextSnapshot\)[\s\S]*?desktopVisibilitySyncRequest = null/,
  '右栏可见性请求完成后必须先应用 Main 快照，再清理在途状态',
);
assert.match(
  shellSource,
  /desktopVisibilitySyncFailure[\s\S]*?desktopSnapshotRevision[\s\S]*?用户下一次显式点击会清除该状态/,
  '右栏可见性失败必须有明确的失败态，禁止异常反馈式无限重试',
);
assert.doesNotMatch(
  headerSource,
  /getSnapshot\(\)[\s\S]*?submitLayoutIntent\(\{[\s\S]*?right_pane_visibility/,
  'Header 不得绕过 Workbench 状态直接写入桌面右栏可见性',
);
assert.match(
  appSource,
  /<Header[\s\S]*?onOpenSettings=\{openSettings\}[\s\S]*?>/,
  'App 不得向 Header 注入第二个桌面右栏可见性写入者',
);
assert.doesNotMatch(
  storeSource,
  /pane\.collapsed = true;[\s\S]*?return;/,
  'BrowserAuthority 空快照不得覆盖用户显式打开的空右栏',
);
assert.doesNotMatch(
  shellSource,
  /desktopVisibilityScopeKey === scopeKey/,
  'Desktop 右栏可见性不能只按作用域去重，否则同一会话新增 Browser Tab 无法展开右栏',
);

console.log('desktop right-pane intent golden passed');
