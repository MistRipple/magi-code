import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(fileURLToPath(new URL("..", import.meta.url)));

const read = (path) => readFile(join(root, path), "utf8");
const [workerSource, tabSource, inputSource, messageSource, browserRoutes, browserTools, controlSchema, overlayShell, overlayManager, desktopIndex, surfaceManager, windowManager, desktopControlServer] =
  await Promise.all([
    read("browser-automation-worker/src/runtime.ts"),
    read("web/src/components/tabs/BrowserTabContent.svelte"),
    read("web/src/components/InputArea.svelte"),
    read("web/src/components/MessageItem.svelte"),
    read("crates/magi-api/src/routes/browser.rs"),
    read("crates/magi-api/src/browser_tool_runtime.rs"),
    read("contracts/desktop-browser/desktop-control.schema.json"),
    read("web/src/DesktopOverlayShell.svelte"),
    read("apps/desktop/src/main/desktop-overlay-manager.ts"),
    read("apps/desktop/src/main/index.ts"),
    read("apps/desktop/src/main/browser-surface-manager.ts"),
    read("apps/desktop/src/main/window-manager.ts"),
    read("apps/desktop/src/main/desktop-control-server.ts"),
  ]);
const workbenchShell = await read("web/src/web/WebWorkbenchShell.svelte");
const rightPaneSource = await read("web/src/web/RightPane.svelte");

function section(source, startMarker, endMarker, label) {
  const start = source.indexOf(startMarker);
  assert.notEqual(start, -1, `${label} 缺少起点 ${startMarker}`);
  const end = source.indexOf(endMarker, start + startMarker.length);
  assert.notEqual(end, -1, `${label} 缺少终点 ${endMarker}`);
  return source.slice(start, end);
}

function ordered(source, markers, label) {
  let cursor = -1;
  for (const marker of markers) {
    const next = source.indexOf(marker, cursor + 1);
    assert.notEqual(next, -1, `${label} 缺少或顺序错误: ${marker}`);
    cursor = next;
  }
}

const materializeSection = section(
  surfaceManager,
  "async materialize(",
  "private createSurface(",
  "BrowserSurfaceManager.materialize",
);
ordered(
  materializeSection,
  [
    "let record = this.surfaceForTab(input.tabId, input.windowId);",
    "if (!record) record = this.createSurface(input);",
    "const load = this.startLoad(record, initialUrl);",
    "if (input.awaitPageLoad === true) await load;",
    "else void load.catch(() => undefined);",
    "return this.binding(record);",
  ],
  "首次物化非阻塞导航",
);
assert.equal(
  [...materializeSection.matchAll(/this\.createSurface\(input\)/gu)].length,
  1,
  "同一 Tab 复用检查要求 materialize 只有一个受条件保护的创建点",
);
assert.match(windowManager, /awaitPageLoad: false/u);

const startLoadSection = section(
  surfaceManager,
  "private startLoad(",
  "private waitForNavigationOperation(",
  "BrowserSurfaceManager.startLoad",
);
assert.match(
  startLoadSection,
  /record\.loadPromise[\s\S]*?sameNavigationUrl\(record\.navigationOperation\.targetUrl \?\? "about:blank", url\)[\s\S]*?return record\.loadPromise;/u,
  "同一 URL 的并发初始导航必须复用，目标 URL 变化才启动新导航",
);

const activationSection = section(
  windowManager,
  "async activateBrowser(",
  "\n  activatePanel(",
  "WindowManager.activateBrowser",
);
assert.doesNotMatch(activationSection, /registerDesktopBrowserConnection|connect_desktop_socket|new WebSocket/u);
assert.doesNotMatch(activationSection, /closeTab\(|createSurface\(|loadURL\(/u);

const desktopRegistrationCount = [...desktopIndex.matchAll(/await registerDesktopBrowserConnection\(\);/gu)].length;
assert.equal(desktopRegistrationCount, 2, "Desktop Host 只允许在初始 ready 和 Worker 恢复时注册");
assert.match(
  desktopIndex,
  /function handleDaemonReady\(\)[\s\S]*?await registerDesktopBrowserConnection\(\);[\s\S]*?restoreAfterDaemonReady/u,
);
assert.match(
  desktopControlServer,
  /if \(this\.#client && this\.#client\.readyState !== WebSocket\.CLOSED\)[\s\S]*?409 Conflict/u,
);

const surfaceSlotUpdate = section(
  surfaceManager,
  "  bindContentSurface(",
  "\n  bindingForTabInWindow(",
  "BrowserSurfaceManager.bindContentSurface",
);
assert.match(
  surfaceSlotUpdate,
  /bindContentSurface\([\s\S]*?bounds: Rectangle \| null,[\s\S]*?\): void \{/u,
  "Browser Surface 绑定接口只接收当前内容槽",
);
assert.doesNotMatch(surfaceSlotUpdate, /clipBounds|intersectRect|leaseBounds/u);
assert.match(
  surfaceSlotUpdate,
  /const target = records\.find\(\(record\) => \([\s\S]*?record\.tabId === tabId && record\.surfaceId === surfaceId[\s\S]*?\) \?\? null[\s\S]*?this\.applySlot\(target, bounds, window\);[\s\S]*?if \(record !== target\) this\.unmountSurface\(record, window\);/u,
  "Browser Tab 切换必须先挂载目标 Surface，再卸载旧 Surface，不能制造空槽",
);
assert.doesNotMatch(surfaceSlotUpdate, /createSurface\(|startLoad\(|loadPage\(|loadURL\(|attachDebugger\(|setViewport\(/u);

assert.match(
  windowManager,
  /bindContentSurface\(\s*record\.windowId,\s*layout\.activeTabId,\s*layout\.activeSurfaceId,\s*currentBrowserContentBounds,\s*currentBrowserParentBounds,\s*\)/u,
  "Main 必须把 Renderer 上报的完整窗口坐标内容槽传给原生 Surface",
);
assert.doesNotMatch(
  windowManager,
  /x:\s*0,\s*y:\s*0,\s*width:\s*currentBrowserContentBounds\.width/u,
  "Main 不得将窗口坐标内容槽重置为根坐标",
);

const windowSlotStart = windowManager.indexOf(
  "  private applyLayout(record: DesktopWindowRecord): DesktopWindowSnapshot",
);
const windowSlotEnd = windowManager.indexOf(
  "\n  private requireWindow(",
  windowSlotStart,
);
assert.notEqual(windowSlotStart, -1, "WindowManager.applyLayout 缺少起点");
assert.notEqual(windowSlotEnd, -1, "WindowManager.applyLayout 缺少终点");
const windowSlotUpdate = windowManager.slice(windowSlotStart, windowSlotEnd);
assert.match(windowSlotUpdate, /const rendererGeometryForActiveSurface = browserSurfaceActive[\s\S]*?browserContentSlot\?\.tabId === layout\.activeTabId/u);
assert.match(windowSlotUpdate, /const currentBrowserContentBounds = browserSurfaceActive && rendererGeometryForActiveSurface\s*\?\s*browserContentBounds\(layout\)\s*:\s*null/u);
assert.match(windowSlotUpdate, /this\.#surfaceManager\.bindContentSurface\(/u);
assert.doesNotMatch(windowSlotUpdate, /materialize\(|activateBrowser\(|loadURL\(|setBrowserViewport\(/u);
assert.doesNotMatch(tabSource, /updateBrowserSlot|bindContentSurface/u);
assert.doesNotMatch(tabSource, /transform:\s*scale\(|object-fit:\s*(fill|cover)|surfaceWidth|surfaceHeight/u);
assert.match(windowManager, /browserContentBounds\(layout\)[\s\S]*?bindContentSurface\(/u);
assert.match(workbenchShell, /ResizeObserver/u);
assert.match(workbenchShell, /type:\s*['"]renderer_geometry['"]/u);
assert.match(
  windowManager,
  /setViewBounds\(record\.appView, layout\.appBounds(?: as Rectangle)?\)/u,
  "主 Renderer 必须通过统一的原生边界事务设置范围",
);
assert.match(workbenchShell, /desktop-right-pane-column--overlay[\s\S]*?box-shadow:\s*inset 1px 0 var\(--border\)/u);
assert.doesNotMatch(workbenchShell, /desktop-right-pane-column--overlay[\s\S]*?border-left:/u);
assert.match(
  surfaceManager,
  /const contentRoot = this\.#contentRoots\.get\(input\.windowId\)[\s\S]*?contentRoot\.addChildView\(record\.view, 1\)[\s\S]*?record\.view\.setBounds\(effectiveBounds\)/u,
  "Browser Surface 必须直接挂载到窗口根 contentView 并使用窗口内容槽坐标",
);
assert.match(
  surfaceManager,
  /Emulation\.setDeviceMetricsOverride[\s\S]*?deviceScaleFactor: viewport\.device_scale_factor_millis \/ 1_000[\s\S]*?scale,[\s\S]*?screenWidth: width[\s\S]*?screenHeight: height/u,
  "固定响应式视口必须使用 Chromium 原生设备指标和内容槽内 compositor scale",
);
assert.match(
  surfaceManager,
  /const ALLOWED_WORKER_CDP_METHODS = new Set\(\[[\s\S]*?"Emulation\.setTouchEmulationEnabled"/u,
  "Lighthouse 触控仿真命令必须经过 Worker CDP 白名单",
);
const viewportMethods = section(
  surfaceManager,
  "private async applyViewport(",
  "private async setAgentCursor(",
  "BrowserSurfaceManager.applyViewport",
);
assert.doesNotMatch(
  viewportMethods,
  /Emulation\.setTouchEmulationEnabled/u,
  "桌面 viewport 应用不能直接调用触控仿真命令",
);
assert.match(
  surfaceManager,
  /focusOnNavigation:\s*false/u,
  "浏览器导航不得自动抢占 App Renderer 焦点",
);
assert.match(
  surfaceManager,
  /record\.viewport\.mode === "auto"[\s\S]*?Emulation\.clearDeviceMetricsOverride/u,
  "auto 视口必须清理固定设备仿真，固定视口才使用 Tab 级设备指标",
);
assert.match(
  surfaceManager,
  /const viewport = commit\.viewport;[\s\S]*?if \(viewport\.mode !== "fixed"\) return;/u,
  "固定视口必须只读取当前 Browser Tab 的逻辑视口配置",
);
const applySlotSection = section(
  surfaceManager,
  "private applySlot(",
  "private async loadPage(",
  "BrowserSurfaceManager.applySlot",
);
assert.match(
  applySlotSection,
  /内容槽只管理原生 View 的物理承载范围[\s\S]*?applyViewport[\s\S]*?禁止 CSS transform、截图/u,
  "右栏尺寸变化只能更新原生内容槽，viewport 由 Chromium 的唯一视口状态管理",
);
assert.match(
  applySlotSection,
  /const boundsChanged = !sameBounds\(record\.view\.getBounds\(\), effectiveBounds\)[\s\S]*?if \(boundsChanged\) record\.view\.setBounds\(effectiveBounds\)[\s\S]*?scheduleViewportCommit\(record\)/u,
  "内容槽变化只更新原生 View bounds 并提交 viewport 生命周期，auto 由原生 WebContentsView 自然响应",
);
assert.match(
  applySlotSection,
  /record\.view\.setVisible\(true\)[\s\S]*?this\.scheduleViewportCommit\(record\)/u,
  "固定视口的 compositor scale 由统一 viewport commit 在内容槽更新后处理",
);
assert.match(
  surfaceManager,
  /this\.#contentRoots\.get\(record\.windowId\)\?\.removeChildView\(record\.view\)/u,
  "非当前 Browser Surface 必须从原生命中树解绑但保留 WebContents",
);

assert.match(
  workerSource,
  /input\.target && input\.target\.element_ref !== "root"[\s\S]*?else if \(input\.clip\)/u,
  "截图必须把快照合成根节点视为页面范围，而不是交给 DOM ref 解析",
);
assert.match(
  workerSource,
  /const viewport = await this\.pageViewport\(binding\)[\s\S]*?input\.clip\.x \* viewport\.width[\s\S]*?input\.clip\.height \* viewport\.height/u,
  "区域截图必须基于当前页面运行时视口换算真实 CDP clip",
);
assert.match(
  workerSource,
  /Emulation\.setUserAgentOverride[\s\S]*?Emulation\.setEmulatedMedia[\s\S]*?Network\.emulateNetworkConditions/u,
  "浏览器仿真工具必须调用 Chromium CDP 原生仿真能力",
);
assert.match(
  workerSource,
  /case "clear":[\s\S]*?Emulation\.setUserAgentOverride[\s\S]*?Network\.setExtraHTTPHeaders/u,
  "清理浏览器仿真必须同时清除 UA 和额外请求头",
);

assert.match(
  tabSource,
  /desktop\.setBrowserViewport\([\s\S]*?mode === 'auto'[\s\S]*?deviceScaleFactorMillis: 1_000/u,
  "桌面端响应式视口必须通过统一 Desktop IPC 设置，并保留设备参数",
);
assert.match(tabSource, /CUSTOM_VIEWPORT_DEBOUNCE_MILLIS = 180/u);
assert.match(tabSource, /useAutomaticViewport\(\)[\s\S]*?updateLogicalViewport\('auto'\)/u);
assert.match(tabSource, /VIEWPORT_DEVICE_MODES = \[[\s\S]*?id: 'wide'[\s\S]*?id: 'narrow'/u);
assert.match(tabSource, /class="viewport-menu"[\s\S]*?type="number"[\s\S]*?type="number"/u);
assert.match(tabSource, /class="annotation-menu"[\s\S]*?annotation-menu-number/u);
// Desktop 的浮层统一走原生 Overlay，Web 模式由同一组件的 DOM 浮层承载；
// 菜单展开期间不能撤下网页，否则会重新出现“页面像被切换”的体验。
assert.match(tabSource, /\{#if viewportMenuOpen && !desktopRuntime\}[\s\S]*?class="viewport-popover"/u);
assert.match(tabSource, /\{#if annotationMenuOpen && !desktopRuntime\}[\s\S]*?class="annotation-history-popover"/u);
assert.match(tabSource, /openDesktopViewportMenu\(\)/u);
assert.match(tabSource, /openDesktopAnnotationHistory\(\)/u);
assert.doesNotMatch(tabSource, /setDesktopBlockingOverlay\(key, open\)/u);
assert.match(rightPaneSource, /openDesktopAddPaneMenu\(\)/u);
assert.doesNotMatch(rightPaneSource, /setDesktopBlockingOverlay\(key, addPaneMenuOpen\)/u);
assert.match(overlayManager, /export type DesktopOverlayKind = "menu" \| "annotation";/u);
assert.match(overlayManager, /"right-pane-add"[\s\S]*?"browser-viewport"[\s\S]*?"browser-annotations"/u);
assert.doesNotMatch(overlayManager, /overlayAnchors|widthLimit|itemHeight/u);
assert.match(
  browserTools,
  /if action != "set"[\s\S]*?let Some\(mode\) = optional_string[\s\S]*?mode == "auto"[\s\S]*?BrowserLogicalViewport::Auto/u,
  "LLM 必须能通过 browser_viewport 的 auto 模式恢复跟随内容槽",
);
assert.match(
  browserTools,
  /device_scale_factor_millis/u,
  "LLM 浏览器视口工具必须暴露设备像素比控制",
);
assert.match(
  tabSource,
  /onblur=\{\(\) => \{ addressEditing = false; \}\}/u,
  "地址栏失焦只能结束编辑状态，不能覆盖用户尚未提交的 URL",
);
assert.doesNotMatch(
  tabSource,
  /onblur=\{[\s\S]*?activeTab\?\.url[\s\S]*?address = activeTab\.url[\s\S]*?\}/u,
  "地址栏失焦不得把用户输入恢复成旧页面地址",
);
assert.match(
  desktopIndex,
  /magi-desktop:close-overlay[\s\S]*?const role = manager\.rendererRoleForWebContents\(event\.sender\.id\)[\s\S]*?role !== "app" && role !== "overlay"/u,
  "原生 Overlay 必须能够自行关闭并把焦点交还主 Renderer",
);
assert.match(
  overlayShell,
  /handleAnnotationPointerDown[\s\S]*?handleAnnotationPointerMove[\s\S]*?handleAnnotationPointerUp/u,
  "桌面标记层必须保留完整的按下、移动、抬起选择链路",
);
assert.match(
  overlayManager,
  /const knownAction = state\.fields\.some\([\s\S]*?\["selection", "save", "cancel"\]/u,
  "Main Overlay 必须允许标记选择事件进入主 Renderer，而不是把它当成未知动作丢弃",
);
assert.match(
  tabSource,
  /action\.id === 'selection'[\s\S]*?openAnnotationCommentOverlay\(\)/u,
  "标记选择成功后必须切换到备注编辑层",
);
assert.match(
  workbenchShell,
  /function isClosedBrowserTabError[\s\S]*?resyncAfterClosedBrowserTab/u,
  "已关闭 Browser Tab 必须由唯一 Shell 控制器通过权威快照收敛，不能进入激活重试循环",
);
assert.doesNotMatch(
  rightPaneSource,
  /function isClosedBrowserTabError|resyncAfterClosedBrowserTab/u,
  "RightPane 不得重新成为 Browser Surface 激活或关闭态收敛的旁路写入者",
);

assert.match(
  browserRoutes,
  /let screenshot_clip = match &anchor[\s\S]*?persist_browser_annotation_screenshot\([\s\S]*?screenshot_clip/u,
  "浏览器标记截图必须使用元素或区域锚点的裁剪范围，不能退化为整页截图",
);
assert.match(
  browserRoutes,
  /browser_annotation_artifact_path\([\s\S]*?std::fs::read\(&path\)[\s\S]*?\("content-type", "image\/png"\)/u,
  "标记 artifact 必须通过权威 artifact 路径读取并以 PNG 响应返回",
);
assert.match(
  browserTools,
  /Some\("root"\) => Ok\(None\)/u,
  "浏览器截图工具必须把 root 作为整页范围处理，避免 root ref 不存在错误",
);
assert.match(controlSchema, /"set_logical_viewport"/u);
assert.match(controlSchema, /"get_logical_viewport"/u);

assert.match(
  inputSource,
  /browserAnnotationRefs:[\s\S]*?browserNodeSelections:/u,
  "标记必须以稳定 ID 进入标准 Turn 发送载荷，并与节点选择共用同一上下文入口",
);
assert.match(
  messageSource,
  /messageBrowserAnnotationRefs\.length > 0[\s\S]*?annotation\.sequence \?\? annotationIndex \+ 1/u,
  "对话区域必须展示标记序号和备注，并按 artifact 可用性决定是否可预览",
);
assert.match(
  messageSource,
  /browserAnnotationPreviewError = \$state\(''\)[\s\S]*?browser\.annotation\.previewFailed[\s\S]*?role="status"/u,
  "标记截图 artifact 加载失败必须在对话区域显示可见状态",
);

process.stdout.write("浏览器核心验收契约通过：Surface 非阻塞物化、Tab Surface 复用、单一 Host 连接、右栏 bounds-only 更新、截图裁剪、root 截图、CDP 仿真、标记消息链路、artifact、响应式视口均已覆盖。\n");
