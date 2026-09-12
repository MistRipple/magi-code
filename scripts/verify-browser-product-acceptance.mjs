import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(fileURLToPath(new URL('..', import.meta.url)));
const paths = {
  surface: 'apps/desktop/src/main/browser-surface-manager.ts',
  surfaceTests: 'apps/desktop/src/main/browser-surface-manager.test.ts',
  main: 'apps/desktop/src/main/index.ts',
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
  routeRoot: 'crates/magi-api/src/routes/mod.rs',
  sessionRoutes: 'crates/magi-api/src/routes/sessions.rs',
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
assert.match(source.browserTab, /new ResizeObserver\(scheduleBrowserDisplaySizeSync\)/u);
assert.match(source.browserTab, /updateBrowserDisplaySize\(/u);
// 标记选择现在由真实 Chromium guest 返回归一化选区，Renderer 不再维护
// 覆盖 guest 的 annotationContentRect 透明层。
assert.match(source.browserTab, /annotationSelectionFromEvent\(/u);
assert.match(source.browserTab, /rect\.width <= 0[\s\S]*rect\.height <= 0/u);
assert.match(source.browserTab, /currentBrowserDisplaySize\(\)/u);
assert.match(source.browserTab, /displaySize/u);
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
assert.match(source.surface, /decideBrowserPopup\(details\)/u);
assert.match(source.surface, /reason: decision\.reason/u);
assert.match(source.browserTab, /event\.type === 'popup_blocked'[\s\S]*popupBlockedMessage/u);
assert.match(source.main, /configureAppRendererAuthentication\(controlToken\)/u);
assert.match(source.main, /X-Magi-Desktop-Renderer-Token/u);
assert.match(source.routeRoot, /browser_host_connection_config\(\)[\s\S]*config\.auth_token == request_token/u);
assert.match(source.browserRoutes, /Some\(BrowserClientPlatform::Desktop\)\) => observed/u);
assert.match(source.sessionTurn, /#\[serde\(skip\)\][\s\S]*desktop_browser_tools_allowed/u);
assert.match(source.sessionRoutes, /!request\.desktop_browser_tools_allowed[\s\S]*BrowserToolKind::ALL/u);
assert.match(source.appServer, /trusted_desktop_surface[\s\S]*capabilities\.desktop_browser_surface[\s\S]*capabilities\.browser_tools/u);
assert.match(source.surface, /Page\.captureScreenshot/u);
assert.match(source.surface, /DOM\.getNodeForLocation/u);
assert.match(source.surface, /Overlay\.highlightNode/u);
// 截图和设备指标必须共用当前 Surface 的持久 CDP session，避免 Electron
// 设备仿真与 clipped screenshot 争用并清除彼此状态。
assert.match(source.surface, /"Emulation\.setDeviceMetricsOverride"/u);
assert.match(source.surface, /"Emulation\.clearDeviceMetricsOverride"/u);
assert.match(source.surface, /fitBrowserViewportScale/u);
assert.doesNotMatch(source.surface, /\.enableDeviceEmulation\(|\.disableDeviceEmulation\(/u);
assert.doesNotMatch(source.surface, /new\s+WebContentsView\(|\.setBounds\(|\.addChildView\(/u);
assert.doesNotMatch(source.surface, /capturePage\(|startScreencast|drawImage\(/u);

assert.match(source.window, /new\s+BrowserWindow\(/u);
assert.match(source.window, /attachWindow\(windowId, window\.webContents\)/u);
assert.doesNotMatch(source.window, /\bBaseWindow\b|\bWebContentsView\b|\bappView\b|\.addChildView\(/u);
assert.doesNotMatch(source.window, /browserContentSlot|rendererGeometry|bindContentSurface/u);

for (const [name, titles] of Object.entries({
  surfaceTests: [
    'Desktop 只保留 BrowserWindow 壳层，浏览器显示由 Renderer webview 承载',
    'Webview 注册严格绑定当前窗口、Browser Session、导航代次和 partition',
    '右栏切换只隐藏非活动 Browser Tab，不卸载其 Chromium guest',
    '浏览器 popup 按单页支持矩阵导航或返回明确原因',
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
assert.match(source.browserRoutes, /crop_browser_screenshot/u);
assert.match(source.browserRuntime, /normalize_screenshot_clip/u);
assert.match(source.browserRuntime, /clip:\s*if clip\.is_some\(\) \{ None \} else \{ clip \}/u);
assert.match(source.browserRuntime, /crop_browser_screenshot/u);
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
