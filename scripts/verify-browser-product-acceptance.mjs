import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(fileURLToPath(new URL('..', import.meta.url)));
const paths = {
  surface: 'apps/desktop/src/main/browser-surface-manager.ts',
  surfaceTests: 'apps/desktop/src/main/browser-surface-manager.test.ts',
  controlTests: 'apps/desktop/src/main/desktop-control-server.test.ts',
  window: 'apps/desktop/src/main/window-manager.ts',
  windowTests: 'apps/desktop/src/main/window-layout.test.ts',
  layout: 'apps/desktop/src/main/window-layout.ts',
  workbench: 'web/src/web/WebWorkbenchShell.svelte',
  rightPane: 'web/src/web/RightPane.svelte',
  browserTab: 'web/src/components/tabs/BrowserTabContent.svelte',
  inputArea: 'web/src/components/InputArea.svelte',
  worker: 'browser-automation-worker/src/runtime.ts',
  workerTests: 'browser-automation-worker/src/runtime.test.ts',
  browserRoutes: 'crates/magi-api/src/routes/browser.rs',
  browserRuntime: 'crates/magi-api/src/browser_tool_runtime.rs',
  sessionTurn: 'crates/magi-api/src/dto/session_turn.rs',
  appServer: 'crates/magi-api/src/app_server.rs',
  appServerSchema: 'contracts/app-server/app-server.schema.json',
  bridge: 'web/src/shared/bridges/web-client-bridge.ts',
};
const entries = await Promise.all(
  Object.entries(paths).map(async ([name, path]) => [name, await readFile(join(root, path), 'utf8')]),
);
const source = Object.fromEntries(entries);

const removedGeometry = /browserContentSlot|rendererGeometry|renderer_geometry|bindContentSurface|browserContentBounds/u;
for (const name of ['surface', 'window', 'layout', 'workbench', 'rightPane', 'browserTab']) {
  assert.doesNotMatch(source[name], removedGeometry, `${paths[name]} 保留了旧浏览器几何链路`);
}

assert.match(source.browserTab, /<webview[\s\S]*class="browser-webview"/u);
assert.match(source.browserTab, /registerBrowserWebview\(/u);
assert.match(source.browserTab, /magi:browserScreenshotCaptured/u);
assert.match(source.browserTab, /magi:browserNodeSelected/u);
assert.match(source.browserTab, /VIEWPORT_DEVICE_MODES[\s\S]*id: 'wide'[\s\S]*id: 'narrow'/u);
assert.match(source.browserTab, /CUSTOM_VIEWPORT_DEBOUNCE_MILLIS/u);
assert.match(source.browserTab, /annotation-menu-number/u);
assert.match(source.rightPane, /class="right-pane-tabbar"[\s\S]*class="right-pane-body"/u);
assert.match(source.rightPane, /class="right-pane-browser-tab-host"[\s\S]*hidden=\{/u);
assert.match(source.rightPane, /<AgentTabContent/u);
assert.match(source.rightPane, /<TerminalTabContent/u);
assert.match(source.rightPane, /class="right-pane-image-wrap"/u);
assert.match(source.workbench, /desktop-right-pane-column/u);
assert.match(source.workbench, /desktop-right-pane-resize-handle/u);

assert.match(source.surface, /registerEmbeddedWebview\(/u);
assert.match(source.surface, /webContents\.fromId\(input\.webContentsId\)/u);
assert.match(source.surface, /guest\.hostWebContents !== host/u);
assert.match(
  source.surface,
  /session\.fromPartition\(record\.partitionId,\s*\{\s*cache:\s*false,?\s*\}\)/u,
);
assert.match(source.surface, /setWindowOpenHandler\(\(details\) =>/u);
assert.match(source.surface, /type: "popup_blocked"/u);
assert.match(source.surface, /Page\.captureScreenshot/u);
assert.match(source.surface, /DOM\.getNodeForLocation/u);
assert.match(source.surface, /Overlay\.highlightNode/u);
// 固定视口由 Electron WebContents 的原生设备仿真 API 提交。这里不再要求
// 旧的手工 CDP Emulation.* 旁路字符串，避免把已删除的控制路径重新引入。
assert.match(source.surface, /contents\.enableDeviceEmulation\(\{/u);
assert.match(source.surface, /contents\.disableDeviceEmulation\(\)/u);
assert.doesNotMatch(source.surface, /new\s+WebContentsView\(|\.setBounds\(|\.addChildView\(/u);
assert.doesNotMatch(source.surface, /capturePage\(|startScreencast|drawImage\(/u);

assert.match(source.window, /attachWindow\(windowId, appView\.webContents\)/u);
assert.match(source.window, /setViewBounds\(record\.appView, layout\.appBounds/u);
assert.doesNotMatch(source.window, /browserContentSlot|rendererGeometry|bindContentSurface/u);

for (const [name, titles] of Object.entries({
  surfaceTests: [
    '浏览器显示由 Renderer webview 承载，Main 不创建或调整浏览器 View',
    'Webview 注册严格绑定当前窗口、Browser Session、导航代次和 partition',
    '右栏切换只隐藏非活动 Browser Tab，不卸载其 Chromium guest',
    '浏览器 popup 永远复用当前一级 Browser Tab',
    '截图和 DOM 选择都来自同一真实 Chromium WebContents',
  ],
  controlTests: [
    '不同 Browser Tab 使用独立资源队列，可以并行执行而不互相阻塞',
    'Host 重新连接时重放当前 Primary Surface，重启后自动化仍有权威 Tab 身份',
    '持久化标记在页面刷新后按最后一次 Authority 投影重放',
  ],
  workerTests: [
    '浏览器截图收到快照根节点时必须捕获整页范围而不是把 root 当成 DOM ref',
    '浏览器截图必须拒绝互斥范围组合，并校验图片文件头',
    '浏览器自动化的响应式仿真必须通过 CDP 原生设备能力改变页面布局',
    '浏览器仿真 clear 会清理 UA 和额外请求头',
    'Accessibility 节点引用使用 Worker isolated world 的执行上下文',
  ],
  windowTests: [
    '右栏几何由父窗口布局唯一决定',
    '右栏拖动只改变父容器宽度并保持工作区最小宽度',
    '浏览器面板只保存逻辑 Surface 身份，非浏览器面板清除 Surface 身份',
  ],
})) {
  for (const title of titles) {
    assert.match(source[name], new RegExp(`test\\(["']${escapeRegExp(title)}["']`, 'u'), `${paths[name]} 缺少回归用例: ${title}`);
  }
}

assert.match(source.browserRoutes, /browser_platform_capabilities/u);
assert.match(source.browserRoutes, /persist_browser_annotation_screenshot/u);
assert.match(source.browserRoutes, /browser_annotation_artifact_path/u);
assert.match(source.browserRuntime, /normalize_screenshot_clip/u);
assert.match(source.sessionTurn, /browser_annotation_refs/u);
assert.match(source.bridge, /browserNodeSelections/u);
assert.match(source.bridge, /magi:sessionTurnSubmissionSettled/u);

for (const method of ['initialize', 'initialized', 'events/subscribe', 'browser/tools/list', 'browser/tool', 'approval/request', '$/cancelRequest']) {
  assert.match(source.appServerSchema, new RegExp(`"${escapeRegExp(method)}"`, 'u'), `App Server 缺少 ${method}`);
}
assert.match(source.appServer, /list_browser_tools[\s\S]*execute_browser_tool/u);
assert.match(source.appServer, /publish_browser_tool_item/u);
assert.match(source.inputArea, /submittedComposerDrafts[\s\S]*restoreComposerSubmissionDraft/u);

console.log('browser product acceptance passed');

function escapeRegExp(value) {
  return value.replace(/[.*+?^${}()|[\]\\]/gu, '\\$&');
}
