import { readFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(fileURLToPath(new URL("..", import.meta.url)));
const sourcePaths = {
  surfaceManager: "apps/desktop/src/main/browser-surface-manager.ts",
  surfaceTests: "apps/desktop/src/main/browser-surface-manager.test.ts",
  windowManager: "apps/desktop/src/main/window-manager.ts",
  windowLayout: "apps/desktop/src/main/window-layout.ts",
  windowTests: "apps/desktop/src/main/window-layout.test.ts",
  controlTests: "apps/desktop/src/main/desktop-control-server.test.ts",
  workbench: "web/src/web/WebWorkbenchShell.svelte",
  rightPane: "web/src/web/RightPane.svelte",
  browserTab: "web/src/components/tabs/BrowserTabContent.svelte",
  inputArea: "web/src/components/InputArea.svelte",
  worker: "browser-automation-worker/src/runtime.ts",
  workerTests: "browser-automation-worker/src/runtime.test.ts",
  appServerClientGolden: "web/scripts/app-server-client-golden.mjs",
  sessionNavigationGolden: "web/scripts/session-navigation-golden.mjs",
  canonicalGolden: "web/scripts/canonical-turn-golden.mjs",
  bridgeGolden: "web/scripts/web-client-bridge-golden.mjs",
  bridgeSource: "web/src/shared/bridges/web-client-bridge.ts",
  appServerSchema: "contracts/app-server/app-server.schema.json",
  appServer: "crates/magi-api/src/app_server.rs",
  browserRoutes: "crates/magi-api/src/routes/browser.rs",
  sessionTurn: "crates/magi-api/src/dto/session_turn.rs",
  liveAcceptance: "scripts/verify-browser-core-live.mjs",
};

const entries = await Promise.all(
  Object.entries(sourcePaths).map(async ([name, path]) => [name, await readFile(join(root, path), "utf8")]),
);
const sources = Object.fromEntries(entries);
const checks = [];
const failures = [];

function record(name, passed, detail = "") {
  const line = `${passed ? "通过" : "失败"} ${name}${detail ? `: ${detail}` : ""}`;
  checks.push(line);
  if (!passed) failures.push(line);
}

function includes(name, needle, detail = "") {
  const source = sources[name];
  const passed = typeof source === "string" && source.includes(needle);
  record(`${sourcePaths[name]} 包含 ${needle}`, passed, detail);
}

function matches(name, pattern, detail = "") {
  const source = sources[name];
  const passed = typeof source === "string" && pattern.test(source);
  record(`${sourcePaths[name]} 匹配 ${pattern}`, passed, detail);
}

function ordered(name, markers, detail = "") {
  const source = sources[name];
  let cursor = -1;
  let passed = typeof source === "string";
  for (const marker of markers) {
    const next = passed ? source.indexOf(marker, cursor + 1) : -1;
    if (next === -1) {
      passed = false;
      break;
    }
    cursor = next;
  }
  record(`${sourcePaths[name]} 保持验收顺序 ${markers.join(" -> ")}`, passed, detail);
}

function escapeRegExp(value) {
  return value.replace(/[.*+?^${}()|[\]\\]/gu, "\\$&");
}

function hasTest(name, title) {
  const source = sources[name];
  const jsTest = typeof source === "string"
    && (source.includes(`test("${title}"`) || source.includes(`test('${title}'`)
      || source.includes(`it("${title}"`) || source.includes(`it('${title}'`));
  const rustTest = typeof source === "string"
    && new RegExp(
      `#\\[(?:tokio::)?test(?:\\([^\\]]*\\))?\\][\\s\\r\\n]*(?:pub\\s+)?fn\\s+${escapeRegExp(title)}\\s*\\(`,
      "u",
    ).test(source);
  const passed = jsTest || rustTest;
  record(`${sourcePaths[name]} 包含动态回归用例 ${title}`, passed);
}

record("验收脚本为只读模式", true, "只读取源码、协议和测试文件，不启动服务、不写入文件");

record(
  "右栏是统一 Renderer 容器，Browser Surface 只进入内容槽",
  /browserContentSlot/u.test(sources.windowLayout)
    && /browser-content-slot|renderer_geometry/u.test(sources.workbench)
    && /bindContentSurface\(/u.test(sources.surfaceManager),
);
matches(
  "surfaceManager",
  /const contentRoot = this\.#contentRoots\.get\(record\.windowId\)[\s\S]*?contentRoot\.addChildView\(record\.view, 1\)[\s\S]*?record\.view\.setBounds\(effectiveBounds\)/u,
  "原生浏览器 View 直接挂载窗口根 contentView 并使用窗口内容槽坐标",
);
matches(
  "windowManager",
  /window\.contentView\.addChildView\(appView, 0\)[\s\S]*?#surfaceManager\.attachWindow\(windowId, window, window\.contentView\)[\s\S]*?#overlayManager\.create\(windowId, window, window\.contentView\)/u,
  "App Renderer 固定为根层，Browser Surface 与 Overlay 按需作为 contentView 的直接子视图挂载",
);
matches("workbench", /ResizeObserver/u, "右栏尺寸由 Renderer 几何变化驱动");
matches("workbench", /type:\s*['"]renderer_geometry['"]/u, "Renderer 几何协议存在");
matches("browserTab", /setBrowserViewport\(/u, "浏览器 Tab 视口通过 Desktop IPC 设置");
record(
  "浏览器 Tab 不通过 CSS 投影或坐标变换伪装嵌入",
  !/transform:\s*scale\(|object-fit:\s*(?:fill|cover)|surfaceWidth|surfaceHeight/u.test(sources.browserTab),
);
hasTest("surfaceTests", "Browser Surface 只接受 Main 布局计算出的内容槽");
hasTest("surfaceTests", "右栏非浏览器面板不依赖 Browser Surface");
hasTest("windowTests", "内容槽尺寸变化不反向修改右栏父容器尺寸");

hasTest("surfaceTests", "切换 Browser Tab 保留各自 WebContents，非当前 Surface 不参与命中测试");
hasTest("controlTests", "不同 Browser Tab 使用独立资源队列，可以并行执行而不互相阻塞");
hasTest("controlTests", "Host 重新连接时重放当前 Primary Surface，重启后自动化仍有权威 Tab 身份");
hasTest("workerTests", "不同 Browser Surface 使用独立的页面运行态和 CDP binding，不串用 Tab 数据");
matches("surfaceManager", /this\.#contentRoots\.get\(record\.windowId\)\?\.removeChildView\(record\.view\)/u, "隐藏 Tab 只解绑原生 View，不销毁 WebContents");

hasTest("surfaceTests", "页面导航期间保持原生页面可见，只有渲染进程崩溃才恢复 Surface");
hasTest("workerTests", "导航 revision 变化后不会复用上一文档的 Console 和 Network 记录");
matches("surfaceManager", /did-start-navigation[\s\S]*?did-finish-load/u, "导航生命周期存在开始和完成边界");
matches("surfaceManager", /reloadIgnoringCache\(\)/u, "刷新和清理缓存走 Chromium WebContents 原生 reload");
matches("liveAcceptance", /activateAndReadTab[\s\S]*?focusBrowserTab/u, "live 脚本覆盖 Tab 激活与焦点同步");

hasTest("surfaceTests", "原生浏览器页面接管时关闭同一 Browser Tab 的菜单 Overlay");
hasTest("surfaceTests", "Browser Surface 导航不自动抢占 App Renderer 焦点");
hasTest("surfaceTests", "Overlay Manager 只分发动作，业务 Renderer 完成处理后再关闭");
matches("rightPane", /openDesktopAddPaneMenu\(\)/u, "Desktop 新增面板菜单必须绑定原生 Overlay");
matches("browserTab", /openDesktopViewportMenu\(\)/u, "视口菜单必须绑定原生 Overlay");
matches("browserTab", /\{#if viewportMenuOpen && !desktopRuntime\}/u, "Web 模式继续保留 DOM 浮层，Desktop 不再撤下页面");
matches("browserTab", /\{#if annotationMenuOpen && !desktopRuntime\}/u, "Web 模式继续保留 DOM 浮层，Desktop 不再撤下页面");
matches("surfaceManager", /focusOnNavigation:\s*false/u, "导航不会强制抢占对话输入焦点");

hasTest("workerTests", "浏览器截图收到快照根节点时必须捕获整页范围而不是把 root 当成 DOM ref");
hasTest("workerTests", "浏览器截图必须拒绝互斥范围组合，并校验图片文件头");
hasTest("workerTests", "元素截图先滚动到元素并重新读取最终 bounds");
matches(
  "surfaceManager",
  /waitForViewportCommit\(record\)[\s\S]*?method === "Page\.captureScreenshot"[\s\S]*?waitForScreenshotReadiness\(record\)[\s\S]*?fromSurface: true/u,
  "截图通过 Chromium Page domain 从真实 WebContents compositor 读取，宿主不参与坐标映射",
);
if (/capturePage\(|capturePageRect|mapBrowserCaptureClipToNativeRect/u.test(sources.surfaceManager)) {
  record("apps/desktop/src/main/browser-surface-manager.ts 不保留 native capturePage 双实现", false);
} else {
  record("apps/desktop/src/main/browser-surface-manager.ts 不保留 native capturePage 双实现", true);
}
matches("browserRoutes", /persist_browser_annotation_screenshot\([\s\S]*?screenshot_clip/u, "标记截图使用区域锚点裁剪");
matches("browserRoutes", /browser_annotation_artifact_path\([\s\S]*?image\/png/u, "标记截图通过持久化 artifact 以 PNG 返回");
matches("canonicalGolden", /browserAnnotationRefs[\s\S]*?browser annotation metadata/u, "标记 ID 进入 canonical 消息链路");
matches(
  "bridgeSource",
  /const browserNodeSelections = Array\.isArray\(input\.browserNodeSelections\)[\s\S]*?typeof selection\.backendDomNodeId === 'number'[\s\S]*?typeof selection\.outerHtmlTruncated === 'boolean'[\s\S]*?browserNodeSelections,\s*accessProfile/u,
  "真实 Renderer bridge 校验并转发结构化节点选择，不依赖 golden 文本占位",
);
matches(
  "bridgeSource",
  /canonicalTurnSeqFromResult\([\s\S]*?updateRequestBinding\(requestId,[\s\S]*?turnSeq/u,
  "accepted canonical turn 序号只在服务端确认后绑定，不与输入节点选择混用",
);
matches(
  "bridgeGolden",
  /async function verifyBrowserNodeSelectionTurn[\s\S]*?turnBody\.browserNodeSelections[\s\S]*?findArtifactByRequestId\([\s\S]*?canonicalTimelineProjection[\s\S]*?requestId[\s\S]*?acceptedResponse\.resolve/u,
  "golden 真实执行节点选择提交、规范化、乐观消息保留和 accepted 清理",
);

hasTest("workerTests", "Accessibility 节点引用使用 Worker isolated world 的执行上下文");
hasTest("surfaceTests", "节点检查使用真实鼠标坐标、Chromium DOM 命中和原生高亮");
hasTest("surfaceTests", "节点选择只向 Host 发送当前 Primary 的完整结构化上下文");
matches("surfaceManager", /before-mouse-event[\s\S]*?DOM\.getNodeForLocation[\s\S]*?Overlay\.highlightNode/u, "节点选择拦截真实鼠标并使用 Chromium DOM 命中与原生高亮");
matches("sessionTurn", /validate_browser_node_selections\(/u, "节点选择在进入会话前进行结构化校验");
matches("sessionTurn", /pub outer_html_truncated: bool/u, "节点 HTML 截断状态在 App Server DTO 中为必填协议字段");

hasTest("workerTests", "浏览器自动化的响应式仿真必须通过 CDP 原生设备能力改变页面布局");
hasTest("workerTests", "浏览器仿真 clear 会清理 UA 和额外请求头");
hasTest("windowTests", "过期 frame 不得替换最后一次已确认的完整几何");
matches("surfaceManager", /Emulation\.setDeviceMetricsOverride[\s\S]*?deviceScaleFactor[\s\S]*?scale,/u, "固定设备视口走 Chromium CDP 设备指标与原生 compositor scale");
matches("surfaceManager", /Emulation\.clearDeviceMetricsOverride/u, "auto 视口清理固定设备仿真");
matches("browserTab", /VIEWPORT_DEVICE_MODES[\s\S]*?id: 'wide'[\s\S]*?id: 'narrow'/u, "产品只提供宽屏和窄屏预设，并允许自定义");
matches("browserTab", /CUSTOM_VIEWPORT_DEBOUNCE_MILLIS/u, "自定义视口输入有明确的动态更新节流边界");

hasTest("browserRoutes", "platform_capabilities_are_explicit_for_each_client");
hasTest("browserRoutes", "real_browser_operations_are_unavailable_to_web_clients");
hasTest("browserRoutes", "explicit_non_desktop_platform_cannot_be_overridden_by_a_desktop_user_agent");
matches("browserRoutes", /validate_session_scope\([\s\S]*?requested_workspace_id[\s\S]*?requested_workspace_path/u, "workspace 和 personal scope 在路由边界校验");
matches("browserRoutes", /browser_annotation_artifact_path[\s\S]*?session_id/u, "无 workspace 会话也使用 session 级浏览器资源边界");

hasTest("surfaceTests", "Surface 重启恢复会让 Worker 清理旧页面运行态");
hasTest("workerTests", "Renderer 重启清理 Worker 保存的 CDP 运行态，避免复用失效会话");
matches("surfaceManager", /render-process-gone[\s\S]*?invalidateAndRecover/u, "只有渲染进程崩溃触发恢复");
matches("appServer", /runtime_epoch\(\)[\s\S]*?browser\/tool/u, "服务端结果携带 runtime epoch");
matches("surfaceManager", /desktop_epoch/u, "重启后的 Surface 身份包含 desktop epoch");

for (const method of ["initialize", "initialized", "events/subscribe", "browser/tools/list", "browser/tool", "approval/request", "$/cancelRequest"]) {
  includes("appServerSchema", `"${method}"`, `App Server 协议包含 ${method}`);
}
matches("appServerClientGolden", /browser\/tools\/list[\s\S]*?browser\/tool[\s\S]*?runtimeEpoch/u, "LLM 浏览器工具动态 golden 覆盖 list、执行和结果身份");
matches("appServerClientGolden", /assert\.deepEqual\(toolRequest\.params, toolParams/u, "LLM 工具请求断言 sessionId、tool、arguments 完整传输");
matches("appServer", /list_browser_tools\([\s\S]*?execute_browser_tool\(/u, "服务端提供浏览器工具目录和执行入口");
matches("appServer", /publish_browser_tool_item\([\s\S]*?session\.turn\.item\.upserted/u, "浏览器工具结果写入 canonical turn item 事件");
matches("sessionNavigationGolden", /pending[\s\S]*?settleSessionNavigation/u, "会话切换有可观察的 pending/settle 生命周期");
matches("liveAcceptance", /screenshot[\s\S]*?lifecycleRegression/u, "live 验收支持截图与重启生命周期回归参数");
matches(
  "inputArea",
  /submittedComposerDrafts[\s\S]*?restoreComposerSubmissionDraft[\s\S]*?magi:sessionTurnSubmissionSettled/u,
  "发送失败时输入区只按 requestId 恢复未被新输入覆盖的完整草稿",
);
matches(
  "bridgeSource",
  /emitSessionTurnSubmissionSettled\(requestId, 'accepted'\)[\s\S]*?emitSessionTurnSubmissionSettled\(requestId, 'failed'\)/u,
  "Bridge 为提交成功和失败提供同一条 request lifecycle 终态事件",
);

if (failures.length > 0) {
  process.stderr.write(`浏览器产品验收契约失败 ${failures.length} 项。\n${failures.join("\n")}\n`);
  process.exitCode = 1;
} else {
  process.stdout.write(`浏览器产品验收契约通过，共 ${checks.length} 项。\n`);
}
