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
  "private async waitForNavigation(",
  "BrowserSurfaceManager.startLoad",
);
assert.match(startLoadSection, /if \(record\.loadPromise\) return record\.loadPromise;/u);

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
  "bindContentSurface(windowId: string, tabId: string, bounds: Rectangle | null)",
  "bindingForTabInWindow(",
  "BrowserSurfaceManager.bindContentSurface",
);
assert.match(surfaceSlotUpdate, /if \(record\.tabId === tabId\) \{\s*this\.applySlot\(record, bounds, window\);/u);
assert.doesNotMatch(surfaceSlotUpdate, /createSurface\(|startLoad\(|loadPage\(|loadURL\(|attachDebugger\(|setViewport\(/u);

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
assert.match(windowSlotUpdate, /browserContentBounds\(layout\)/u);
assert.match(windowSlotUpdate, /this\.#surfaceManager\.bindContentSurface\(/u);
assert.doesNotMatch(windowSlotUpdate, /materialize\(|activateBrowser\(|loadURL\(|setBrowserViewport\(/u);
assert.doesNotMatch(tabSource, /ResizeObserver|browserSurfaceSlot/u);
assert.doesNotMatch(tabSource, /updateBrowserSlot|bindContentSurface/u);
assert.doesNotMatch(tabSource, /transform:\s*scale\(|object-fit:\s*(fill|cover)|surfaceWidth|surfaceHeight/u);
assert.match(windowManager, /browserContentBounds\(layout\)[\s\S]*?bindContentSurface\(/u);
assert.match(windowManager, /record\.appLayer\.setBounds\(layout\.appBounds[\s\S]*?record\.appView\.setBounds\(layout\.appBounds/u);
assert.match(workbenchShell, /desktop-right-pane-column--overlay[\s\S]*?box-shadow:\s*inset 1px 0 var\(--border\)/u);
assert.doesNotMatch(workbenchShell, /desktop-right-pane-column--overlay[\s\S]*?border-left:/u);
assert.match(
  surfaceManager,
  /record\.host\.addChildView\(record\.view, 0\)[\s\S]*?const localBounds = \{ x: 0, y: 0, width: bounds\.width, height: bounds\.height \}[\s\S]*?record\.view\.setBounds\(localBounds\)/u,
  "Browser Surface 必须挂载到当前内容槽宿主并使用局部内容槽坐标",
);
assert.match(
  surfaceManager,
  /Emulation\.setDeviceMetricsOverride[\s\S]*?deviceScaleFactor: viewport\.device_scale_factor_millis \/ 1_000[\s\S]*?screenWidth: width[\s\S]*?screenHeight: height/u,
  "固定响应式视口必须使用 Chromium 原生设备指标和设备像素比",
);
assert.doesNotMatch(
  surfaceManager,
  /Emulation\.setTouchEmulationEnabled/u,
  "桌面 WebContentsView 不应调用会阻塞 CDP lane 的触控仿真命令",
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
  /const viewport = record\.viewport;[\s\S]*?if \(viewport\.mode !== "fixed"\) return;/u,
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
  /内容槽只管理原生 View 的物理承载范围[\s\S]*?不能重新提交[\s\S]*?fixed/u,
  "右栏尺寸变化只能更新原生内容槽，不得改写 Tab 级 CSS viewport",
);
assert.doesNotMatch(
  applySlotSection,
  /scheduleViewportApply\(record\)/u,
  "右栏尺寸变化不得重新提交 viewport",
);
assert.match(
  surfaceManager,
  /record\.host\.removeChildView\(record\.view\)/u,
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
assert.match(tabSource, /fields:[\s\S]*?id: 'width'[\s\S]*?id: 'height'/u);
assert.match(
  browserTools,
  /if action != "set"[\s\S]*?let mode = optional_string[\s\S]*?mode == "auto"[\s\S]*?BrowserLogicalViewport::Auto/u,
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
  /state\.kind === "annotation" && \["selection", "save", "cancel"\]/u,
  "Main Overlay 必须允许标记选择事件进入主 Renderer，而不是把它当成未知动作丢弃",
);
assert.match(
  tabSource,
  /action\.id === 'selection'[\s\S]*?openAnnotationCommentOverlay\(\)/u,
  "标记选择成功后必须切换到备注编辑层",
);
assert.match(
  rightPaneSource,
  /function isClosedBrowserTabError[\s\S]*?resyncAfterClosedBrowserTab/u,
  "已关闭 Browser Tab 必须通过权威快照收敛，不能进入激活重试循环",
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
  /browserAnnotationRefs:[\s\S]*?browserAnnotationSnapshots:/u,
  "标记必须同时以稳定 ID 和快照进入消息发送载荷",
);
assert.match(
  messageSource,
  /messageBrowserAnnotationRefs\.length > 0[\s\S]*?annotation\.sequence \?\? annotationIndex \+ 1/u,
  "对话区域必须展示标记序号和备注，并按 artifact 可用性决定是否可预览",
);

process.stdout.write("浏览器核心验收契约通过：Surface 非阻塞物化、Tab Surface 复用、单一 Host 连接、右栏 bounds-only 更新、截图裁剪、root 截图、CDP 仿真、标记消息链路、artifact、响应式视口均已覆盖。\n");
