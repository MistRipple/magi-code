import { createServer } from "node:http";
import { execFile, spawn } from "node:child_process";
import { access, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createInterface } from "node:readline";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { setTimeout as sleep } from "node:timers/promises";
import { promisify } from "node:util";
import {
  connectAppPage,
  connectTarget,
  createReporter,
  launchApp,
  pageTools,
  waitFor,
} from "./lib/electron-harness.mjs";

/**
 * 打包 Electron 插件系统验收。
 *
 * 启动打包后的 Magi.app（独立状态根、独立端口、本地 OpenAI-compatible Provider），
 * 通过 App Renderer 的 CDP 驱动真实界面：安装 → 授权激活 → 设置表单 → 主页入口 →
 * 主内容区页面（隔离 iframe）→ 资源同步 → 视图选中内容引用 → 模型发起的插件工具调用 →
 * 右栏视图 → 升级排空 → 停用清理。不接管用户已有的 Electron/daemon 进程。
 *
 * 环境变量：MAGI_ELECTRON_PLUGINS_APP_EXECUTABLE（默认 target/electron-dist/mac-arm64/Magi.app/...）、
 * MAGI_ELECTRON_PLUGINS_CDP_PORT（9301）、MAGI_ELECTRON_PLUGINS_DAEMON_PORT（38931）。
 */

const execFileAsync = promisify(execFile);
const repositoryRoot = new URL("..", import.meta.url).pathname.replace(/\/$/u, "");
const appExecutable = process.env.MAGI_ELECTRON_PLUGINS_APP_EXECUTABLE?.trim()
  || join(repositoryRoot, "target/electron-dist/mac-arm64/Magi.app/Contents/MacOS/Magi");
const cdpPort = Number.parseInt(process.env.MAGI_ELECTRON_PLUGINS_CDP_PORT || "9301", 10);
const daemonPort = Number.parseInt(process.env.MAGI_ELECTRON_PLUGINS_DAEMON_PORT || "38931", 10);
const PLUGIN_ID = "example.board";

const { check, summary } = createReporter();

// ---- 本地 Provider：记录请求；遇到插件工具时发起一次真实工具调用 ----
function sse(content) {
  return [
    `data: ${JSON.stringify({ choices: [{ delta: { content }, finish_reason: null }] })}\n\n`,
    `data: ${JSON.stringify({ choices: [{ delta: {}, finish_reason: "stop" }] })}\n\n`,
    "data: [DONE]\n\n",
  ].join("");
}
function toolCall(name, args) {
  return [
    `data: ${JSON.stringify({ choices: [{ delta: { tool_calls: [{ index: 0, id: "plugin-call-1", type: "function", function: { name, arguments: JSON.stringify(args) } }] }, finish_reason: null }] })}\n\n`,
    `data: ${JSON.stringify({ choices: [{ delta: {}, finish_reason: "tool_calls" }] })}\n\n`,
    "data: [DONE]\n\n",
  ].join("");
}
function textOf(message) {
  const content = message?.content;
  if (typeof content === "string") return content;
  if (Array.isArray(content)) return content.map((part) => part?.text ?? "").join(" ");
  return "";
}

const providerRequests = [];
const rendererErrors = [];
const provider = createServer((request, response) => {
  let body = "";
  request.on("data", (chunk) => { body += chunk; });
  request.on("end", () => {
    if (request.url === "/v1/models") {
      response.writeHead(200, { "content-type": "application/json" });
      response.end(JSON.stringify({ object: "list", data: [{ id: "plugin-e2e-model", object: "model" }] }));
      return;
    }
    if (request.url !== "/v1/chat/completions") return void response.writeHead(404).end();
    const parsed = JSON.parse(body);
    providerRequests.push(parsed);
    const messages = parsed.messages ?? [];
    const lastUser = [...messages].reverse().find((message) => message.role === "user");
    const prompt = textOf(lastUser);
    // 发给模型的工具名经 bridge 编码，不再保留贡献 ID；按清单里的描述识别只读的“读取看板”。
    const summaryTool = (parsed.tools ?? []).find((tool) => tool.function?.description?.includes("返回看板当前的条目"))?.function?.name;
    const toolResult = messages.find((message) => message.role === "tool");
    response.writeHead(200, { "content-type": "text/event-stream" });
    if (prompt.includes("PLUGIN_TOOL_E2E") && summaryTool && !toolResult) {
      response.end(toolCall(summaryTool, {}));
    } else if (toolResult) {
      response.end(sse(`PLUGIN_TOOL_RESULT:${textOf(toolResult)}`));
    } else {
      response.end(sse("PLUGIN_E2E_OK"));
    }
  });
});
await new Promise((resolve) => provider.listen(0, "127.0.0.1", resolve));
const providerPort = provider.address().port;
const frameTrace = [];
const frameResponses = [];

/**
 * 沙箱 iframe（opaque origin）是独立的 OOPIF 目标，不在宿主页面的帧树里。
 * 这里按 CDP 目标列表单独连接它，以只读方式检查视图 DOM，不扩大插件的任何权限。
 */
async function inFrame(expression, selector = '.top-tab-pane.active iframe[data-testid="plugin-view"]') {
  const { root } = await page.call("DOM.getDocument");
  const { nodeId } = await page.call("DOM.querySelector", { nodeId: root.nodeId, selector });
  if (!nodeId) throw new Error(`缺少插件 iframe: ${selector}`);
  const { node } = await page.call("DOM.describeNode", { nodeId });
  const targets = (await (await fetch(`http://127.0.0.1:${cdpPort}/json/list`)).json())
    .filter((item) => item.type === "iframe" && item.url.includes(`/api/plugins/${PLUGIN_ID}/ui/`));
  frameTrace.push({ at: Date.now(), selector, frameId: node.frameId, attributes: node.attributes, targets: targets.map(({ id, url }) => ({ id, url })) });
  if (frameTrace.length > 80) frameTrace.shift();
  const frame = await connectTarget(cdpPort, (item) => item.id === node.frameId, "指定插件 iframe", rendererErrors);
  try {
    return await frame.evaluate(expression);
  } finally {
    frame.close();
  }
}

let tools = null;
const api = (_page, path, init) => tools.api(path, init);
const post = (_page, path, body) => tools.post(path, body);
const click = (_page, selector) => tools.click(selector);
const exists = (_page, selector) => tools.exists(selector);
const text = (_page, selector) => tools.text(selector);
const documentBodyIncludes = (value) => page.evaluate(`document.body.innerText.includes(${JSON.stringify(value)})`);

/** 设置页在显示时会重置/聚焦当前分类；重试点击直到目标分类真正成为当前页。 */
async function openSettingsTab(page, tabId) {
  const nav = `[data-testid="settings-nav-${tabId}"]`;
  await waitFor(async () => {
    // 设置视图创建后常驻 DOM、只切换显示，因此必须检查可见性而不是存在性。
    const visible = await page.evaluate(`(() => { const el = document.querySelector('[data-testid="settings-page"]'); return Boolean(el && el.getClientRects().length > 0); })()`);
    if (!visible) {
      await click(page, '[data-testid="sidebar-settings"]');
      // 刚打开时设置视图仍在初始化，分类选择会被随后的初始化覆盖；等其稳定后再选。
      await sleep(1500);
    }
    if (!(await exists(page, nav))) return false;
    await click(page, nav);
    await sleep(700);
    return (await page.evaluate(`document.querySelector(${JSON.stringify(nav)}).getAttribute('aria-current')`)) === "page";
  }, `设置分类 ${tabId} 成为当前页`, 15_000, 300);
}

/** 插件页左右分栏：左侧是概要列表，右侧是所选插件的详情；选中某一行即在同一页面显示它的详情与全部操作。 */
const PLUGIN_DETAIL = (id) => `[data-testid="plugin-card"][data-plugin-id="${id}"]`;
async function openPluginDetail(id = PLUGIN_ID) {
  await openSettingsTab(page, "plugins");
  if (!(await exists(page, PLUGIN_DETAIL(id)))) {
    await waitFor(() => exists(page, `[data-testid="plugin-row"][data-plugin-id="${id}"]`), "插件列表行出现");
    await click(page, `[data-testid="plugin-row"][data-plugin-id="${id}"]`);
  }
  await waitFor(() => exists(page, PLUGIN_DETAIL(id)), "插件详情面板出现");
}

// ---- 启动 ----
const workRoot = await mkdtemp(join(tmpdir(), "magi-electron-plugins-"));
const stateRoot = join(workRoot, "state");
const workspaceRoot = join(workRoot, "workspace");
await mkdir(workspaceRoot, { recursive: true });
await writeFile(join(workspaceRoot, "README.md"), "plugin e2e\n");
const archive = join(workRoot, "board-1.0.0.zip");
await execFileAsync("node", [join(repositoryRoot, "scripts/pack-plugin.mjs"), join(repositoryRoot, "examples/plugins/board"), archive]);

const app = launchApp({
  executable: appExecutable,
  cwd: repositoryRoot,
  cdpPort,
  env: {
    MAGI_DESKTOP_DAEMON_PORT: String(daemonPort),
    MAGI_STATE_ROOT: stateRoot,
    MAGI_OPEN_BROWSER: "0",
    MAGI_OPENAI_COMPAT_BASE_URL: `http://127.0.0.1:${providerPort}/v1`,
    MAGI_OPENAI_COMPAT_API_KEY: "plugin-e2e-key",
    MAGI_OPENAI_COMPAT_MODEL: "plugin-e2e-model",
    // 官方插件中心的默认地址在验收中指向本地服务，避免依赖外网。
    MAGI_PLUGIN_CENTER_DEFAULT_URL: `http://127.0.0.1:${daemonPort + 100}/official.json`,
  },
});
const electronLog = app.log;

let page = null;
let failure = null;
try {
  page = await waitFor(async () => {
    const candidate = await connectAppPage(cdpPort, daemonPort, rendererErrors).catch(() => null);
    if (!candidate) return null;
    const ready = await candidate.evaluate(`Boolean(document.querySelector('[data-testid="input-textarea"]'))`).catch(() => false);
    if (!ready) { candidate.close(); return null; }
    return candidate;
  }, "打包 Electron App Renderer 就绪", 60_000, 500);
  tools = pageTools(page);
  await page.call("Network.enable");
  page.on("Network.responseReceived", ({ frameId, response }) => {
    if (response.url.includes(`/api/plugins/${PLUGIN_ID}/ui/`)) {
      frameResponses.push({ frameId, url: response.url, status: response.status });
    }
  });
  check("打包 Electron 启动并渲染工作台", true);

  // 1. 安装（本地包）+ 工作区
  const installed = await post(page, "/api/plugins/install", { source: `local:${archive}` });
  check("本地包安装成功", installed.status === 200, installed.body.slice(0, 200));
  const registered = await post(page, "/api/workspaces/register", { path: workspaceRoot });
  const workspaceId = JSON.parse(registered.body).workspaceId;
  check("工作区注册成功", Boolean(workspaceId), registered.body);
  const scope = "application";
  const workspaceEnable = await post(page, `/api/plugins/${PLUGIN_ID}/enable`, { scope: `workspace:${workspaceId}` });
  check("插件全局生效：工作区作用域键被拒绝", workspaceEnable.status === 409, `${workspaceEnable.status} ${workspaceEnable.body.slice(0, 120)}`);
  await page.call("Page.reload", { ignoreCache: true });
  await waitFor(() => exists(page, `[data-workspace-id="${workspaceId}"]`), "工作区进入侧栏");
  await page.evaluate(`(() => {
    const row = document.querySelector('[data-workspace-id="${workspaceId}"]');
    row.click();
    const create = row.closest('.workspace-row')?.querySelector('button.row-action[aria-label]');
    if (!create) throw new Error('缺少新建会话按钮');
    create.click();
  })()`);
  await waitFor(async () => exists(page, '[data-testid="input-textarea"]'), "工作区会话输入区");

  // 2. 设置页：插件卡片、来源与摘要、授权激活
  await openSettingsTab(page, "plugins");
  const rowSelector = `[data-testid="plugin-row"][data-plugin-id="${PLUGIN_ID}"]`;
  await waitFor(() => exists(page, rowSelector), "插件列表行出现");
  check("插件页左侧列表只展示概要行，状态为已安装", (await page.evaluate(`document.querySelector(${JSON.stringify(rowSelector)}).dataset.status`)) === "installed");
  await openPluginDetail();
  const card = PLUGIN_DETAIL(PLUGIN_ID);
  const source = await text(page, `${card} [data-testid="plugin-source"]`);
  check("详情展示本地来源与摘要", source.includes("本地安装包") && (await text(page, card)).includes("摘要"), source);
  const currentRow = `${card} [data-testid="plugin-scope-row"][data-scope="${scope}"]`;
  await waitFor(() => exists(page, currentRow), "当前作用域行");
  check("激活前当前作用域为未启用", (await page.evaluate(`document.querySelector(${JSON.stringify(currentRow)}).dataset.state`)) === "off");
  await click(page, `${currentRow} [data-action="activate"]`);
  await waitFor(async () => (await page.evaluate(`document.querySelector(${JSON.stringify(currentRow)})?.dataset.state`)) === "active", "授权并激活后状态为已激活");
  check("授权并激活成功", true);
  check("激活后列表行状态为已启用", (await page.evaluate(`document.querySelector(${JSON.stringify(rowSelector)}).dataset.status`)) === "active");
  const quickSettings = await exists(page, `${card} [data-testid="plugin-open-settings"]`);
  check("有设置项的已启用插件在详情面板提供「打开设置」快捷入口", quickSettings);
  await click(page, `${card} [data-testid="plugin-open-settings"]`);
  const settingsNav = `[data-testid="settings-nav-plugin:${PLUGIN_ID}:settings"]`;
  await waitFor(async () => (await page.evaluate(`document.querySelector(${JSON.stringify(settingsNav)})?.getAttribute('aria-current')`)) === "page", "快捷入口跳到该插件的设置分类", 20_000);
  check("点击「打开设置」直接进入插件的设置分类", true);
  await openSettingsTab(page, "plugins");

  // 3. 动态设置表单
  await waitFor(() => exists(page, `[data-testid="settings-nav-plugin:${PLUGIN_ID}:settings"]`), "插件设置 Tab 出现");
  await openSettingsTab(page, `plugin:${PLUGIN_ID}:settings`);
  await waitFor(() => exists(page, ".plugin-settings-form input.form-input"), "设置表单渲染");
  const titleInput = await page.evaluate(`document.getElementById('plugin-setting-example.board-title').value`);
  check("设置表单展示 schema 默认值", titleInput === "我的看板", titleInput);
  await page.evaluate(`(() => {
    const input = document.getElementById('plugin-setting-example.board-title');
    input.value = '验收看板';
    input.dispatchEvent(new Event('input', { bubbles: true }));
    document.querySelector('.plugin-settings-actions button').click();
  })()`);
  await waitFor(async () => {
    const saved = JSON.parse((await api(page, `/api/plugins/${PLUGIN_ID}/settings?scope=${encodeURIComponent(scope)}`)).body);
    return saved.value?.title === "验收看板";
  }, "设置经 daemon 校验后保存");
  check("设置保存到唯一设置存储", true);

  // 4. 主页入口 → 主内容区页面
  await click(page, '[data-testid="settings-back"]');
  await waitFor(() => exists(page, '[data-testid="top-tab-plugin"]'), "顶部标签出现插件页面入口");
  check("顶部标签只展示入口，不自动打开页面", !(await exists(page, '[data-testid="plugin-view"]')));
  const homeEntries = await page.evaluate(`document.querySelectorAll('[data-testid="plugin-home-entry"]').length`);
  check("空会话主页展示插件入口", homeEntries === 1, String(homeEntries));
  await waitFor(() => exists(page, '[data-testid="plugin-home-card"] iframe[data-testid="plugin-card-view"]'), "主页出现声明的插件卡片", 30_000);
  const cardFrame = await page.evaluate(`(() => { const f = document.querySelector('[data-testid="plugin-home-card"] iframe'); return { sandbox: f.getAttribute('sandbox'), src: f.getAttribute('src') }; })()`);
  check("主页卡片是隔离 iframe 且不占用主视图标识", cardFrame.sandbox === "allow-scripts" && cardFrame.src.includes(`/api/plugins/${PLUGIN_ID}/ui/index.html`) && !(await exists(page, '[data-testid="plugin-view"]')), JSON.stringify(cardFrame));
  await click(page, '[data-testid="plugin-home-entry"]');
  await waitFor(() => exists(page, '[data-testid="plugin-view"]'), "主内容区插件页面 iframe");
  const frameAttrs = await page.evaluate(`(() => { const f = document.querySelector('[data-testid="plugin-view"]'); return { sandbox: f.getAttribute('sandbox'), src: f.getAttribute('src') }; })()`);
  check("视图运行在 sandbox=allow-scripts 的隔离 iframe", frameAttrs.sandbox === "allow-scripts" && frameAttrs.src.includes(`/api/plugins/${PLUGIN_ID}/ui/index.html`), JSON.stringify(frameAttrs));

  // 5. 视图经消息桥读取设置、资源，并随资源变化同步
  await waitFor(async () => (await inFrame(`document.getElementById('title').textContent`)) === "验收看板", "视图读取并展示保存的设置");
  check("视图经消息桥读取设置", true);
  const write = (value, expected) => api(page, `/api/plugins/${PLUGIN_ID}/resources/board?scope=${encodeURIComponent(scope)}`, {
    method: "PUT", headers: { "content-type": "application/json" }, body: JSON.stringify({ expectedVersion: expected, value }),
  });
  const first = await write({ items: [{ text: "写测试" }, { text: "修缺陷" }] }, 0);
  check("资源写入成功并返回版本 1", first.status === 200 && JSON.parse(first.body).version === 1, first.body);
  await waitFor(async () => (await inFrame(`document.querySelectorAll('#items li').length`)) === 2, "视图收到订阅推送并渲染 2 条");
  check("视图订阅实时同步资源", true);
  const conflict = await write({ items: [] }, 0);
  check("过期版本写入明确返回冲突", conflict.status === 409, `${conflict.status} ${conflict.body}`);

  // 6. 视图选中内容 → 输入区结构化引用
  await inFrame(`document.querySelector('#items li button').click()`);
  await waitFor(() => exists(page, '[data-testid="plugin-selection-chip"]'), "输入区出现插件选择 chip");
  check("视图选中内容形成输入区引用", (await text(page, '[data-testid="plugin-selection-chip"]')).includes("看板条目 1"));

  check("主内容区页面选中时提示引用已加入输入区", await waitFor(async () => (await page.evaluate(`document.body.innerText`)).includes("加入输入区"), "加入提示", 5_000));
  await click(page, ".tab-bar--top .tab-item"); // 回到“对话”标签，输入区才可见

  // 命令与结构化引用互斥：用户先选命令再从插件视图选引用时，命令必须让位并说明，不能提交非法组合。
  await page.evaluate(`(() => {
    const chip = document.querySelector('[data-testid="plugin-selection-chip"]');
    // 主内容区切回“对话”时输入区可能尚未重新挂载；此时没有旧 chip 也不影响
    // 后续“命令 → 引用”的互斥行为验证。
    chip?.querySelector('button')?.click();
  })()`);
  await click(page, '[aria-label="添加上下文与能力"]');
  await waitFor(() => exists(page, '.ia-add-item[role="menuitem"]'), "输入区添加菜单展开");
  await page.evaluate(`(() => {
    const item = [...document.querySelectorAll('.ia-add-item')].find((el) => el.textContent.includes('清空看板'));
    if (!item) throw new Error('添加菜单缺少清空看板命令');
    item.click();
  })()`);
  await waitFor(() => documentBodyIncludes('清空看板'), "插件命令选中后显示在输入区");
  await click(page, '[data-testid="top-tab-plugin"]');
  await waitFor(() => exists(page, '[data-testid="plugin-view"]'), "回到插件视图");
  await waitFor(async () => {
    try {
      return await inFrame("Boolean(document.querySelector('#items li:nth-child(1) button'))");
    } catch {
      return false;
    }
  }, "插件视图条目重新挂载", 10_000);
  await inFrame(`document.querySelector('#items li:nth-child(1) button').click()`);
  await click(page, ".tab-bar--top .tab-item"); // 回到“对话”标签，输入区才可见
  check(
    "选择插件引用会取消已选插件命令并保留引用",
    await page.evaluate(`(() => ({
      selection: Boolean(document.querySelector('[data-testid="plugin-selection-chip"]')),
      command: Boolean([...document.querySelectorAll('.ia-reference-chip')].some((el) => el.textContent.includes('清空看板'))),
    }))()`).then((state) => state.selection && !state.command),
  );

  // 7. 带引用提交一轮，并让模型调用真实插件工具
  await page.evaluate(`(() => {
    const el = document.querySelector('[data-testid="input-textarea"]');
    el.focus(); el.textContent = 'PLUGIN_TOOL_E2E 请总结我选中的条目';
    el.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertText', data: el.textContent }));
  })()`);
  await waitFor(async () => !(await page.evaluate(`document.querySelector('[data-testid="input-send-button"]')?.disabled ?? true`)), "发送按钮可用");
  await click(page, '[data-testid="input-send-button"]');
  await waitFor(() => providerRequests.some((request) => (request.messages ?? []).some((message) => message.role === "tool")), "模型收到插件工具结果", 60_000);
  const firstRequest = providerRequests.find((request) => textOf((request.messages ?? []).at(-1)).includes("PLUGIN_TOOL_E2E"));
  const promptText = (firstRequest?.messages ?? []).map(textOf).join("\n");
  check("模型上下文包含插件选择且标注为不可信", promptText.includes("插件视图中选中") && promptText.includes("修缺陷") === false && promptText.includes("写测试") && promptText.includes("不可信"), promptText.slice(-600));
  const toolNames = (firstRequest?.tools ?? []).map((tool) => tool.function?.name);
  const exposed = toolNames.filter((name) => name?.includes("plugin_example_board"));
  check("模型只看到合法且已授权的插件工具名", exposed.length === 2 && exposed.every((name) => /^[A-Za-z0-9_-]{1,64}$/u.test(name)), JSON.stringify(exposed));
  const toolMessage = providerRequests.flatMap((request) => request.messages ?? []).find((message) => message.role === "tool");
  const toolPayload = textOf(toolMessage);
  check("插件工具经真实 Worker 执行并返回结构化结果", toolPayload.includes('"version":1') && toolPayload.includes("写测试"), toolPayload);
  await waitFor(async () => (await page.evaluate(`document.body.innerText`)).includes("PLUGIN_TOOL_RESULT"), "回答写入对话", 30_000);
  check("工具结果进入模型最终回答", true);
  check("提交后输入区引用已清空", !(await exists(page, '[data-testid="plugin-selection-chip"]')));

  // 8. 命令经同一 Turn 链路写回资源，视图同步
  const session = await page.evaluate(`window.location.href`);
  const commandTurn = await post(page, "/api/session/turn", {
    scope: "workspace", workspaceId, workspacePath: workspaceRoot, text: "清空",
    pluginCommandId: `plugin/${PLUGIN_ID}/clear`, requestId: "cmd-1", userMessageId: "cmd-u1", placeholderMessageId: "cmd-p1",
  });
  check("插件命令作为 Turn 被接纳", commandTurn.status === 200, commandTurn.body.slice(0, 200));
  await waitFor(async () => (await api(page, `/api/plugins/${PLUGIN_ID}/resources/board?scope=${encodeURIComponent(scope)}`).then((r) => JSON.parse(r.body))).version === 2, "命令写入资源（版本 2）", 30_000);
  await click(page, '[data-testid="top-tab-plugin"]'); // 回到插件页面：视图重新挂载并读取最新快照
  await waitFor(async () => (await inFrame(`document.querySelectorAll('#items li').length`)) === 0, "视图展示命令清空后的资源", 30_000);
  check("命令经 Worker 写入资源并推送到视图", true);
  void session;

  // 9. 右栏视图 + 停用清理
  await waitFor(async () => {
    await page.evaluate(`document.querySelector('[aria-label="展开右侧面板"]')?.click()`);
    return exists(page, '[aria-label="新增面板"]');
  }, "右栏新增面板按钮", 30_000, 500);
  await click(page, '[aria-label="新增面板"]');
  await waitFor(() => exists(page, '.right-pane-add-menu button[data-kind="plugin"]'), "右栏新增菜单出现插件视图");
  await click(page, '.right-pane-add-menu button[data-kind="plugin"]');
  await waitFor(() => exists(page, '.right-pane iframe[data-testid="plugin-view"]'), "右栏插件视图 iframe");
  const rightFrameUrl = await page.evaluate(`document.querySelector('.right-pane iframe[data-testid="plugin-view"]').src`);
  await waitFor(async () => (await inFrame(`document.getElementById('title')?.textContent`, '.right-pane iframe[data-testid="plugin-view"]')) === "验收看板", "右栏视图与主页面共用同一设置与资源");
  const rightFrameId = frameTrace.at(-1).frameId;
  check("右栏插件视图使用唯一应用作用域并成功加载", new URL(rightFrameUrl).searchParams.get("scope") === "application"
    && await waitFor(() => frameResponses.some((response) => response.frameId === rightFrameId && response.url === rightFrameUrl && response.status === 200), "右栏插件页面 HTTP 200"));
  check("右栏可打开同一插件视图并读取同一数据源", true);
  for (let cycle = 0; cycle < 6; cycle++) {
    await click(page, '.right-pane-tab.active .right-pane-tab-close');
    await waitFor(async () => !(await exists(page, '.right-pane iframe[data-testid="plugin-view"]')), "右栏插件视图关闭");
    await waitFor(async () => {
      await page.evaluate(`document.querySelector('[aria-label="展开右侧面板"]')?.click()`);
      return exists(page, '[aria-label="新增面板"]');
    }, "关闭后重新展开右栏");
    await click(page, '[aria-label="新增面板"]');
    await click(page, '.right-pane-add-menu button[data-kind="plugin"]');
    await waitFor(async () => (await inFrame(`document.getElementById('title')?.textContent`, '.right-pane iframe[data-testid="plugin-view"]')) === "验收看板", "重新挂载的右栏插件视图读取设置");
  }
  check("右栏连续关闭重开六次仍读取同一设置且主页面保持独立", (await inFrame(`document.getElementById('title').textContent`)) === "验收看板");

  // 添加菜单由插件投影驱动：插件视图项带稳定身份，末尾固定「管理插件」直达设置页插件分类。
  await click(page, '[aria-label="新增面板"]');
  await waitFor(() => exists(page, '.right-pane-add-menu button[data-kind="managePlugins"]'), "添加菜单出现管理插件入口");
  const pluginItem = await page.evaluate(`(() => { const el = document.querySelector('.right-pane-add-menu button[data-kind="plugin"]'); return el ? { pluginId: el.dataset.pluginId, contributionId: el.dataset.contributionId, disabled: el.disabled } : null; })()`);
  check("添加菜单的插件视图项携带插件与贡献身份且可用", pluginItem?.pluginId === PLUGIN_ID && Boolean(pluginItem.contributionId) && pluginItem.disabled === false, JSON.stringify(pluginItem));
  check("未安装 GPT Web 插件时添加菜单不出现其入口（不是置灰）", !(await exists(page, '.right-pane-add-menu button[data-kind="webSession"]')));
  await click(page, '.right-pane-add-menu button[data-kind="managePlugins"]');
  await waitFor(() => exists(page, '[data-testid="plugin-list"]'), "管理插件直达设置页插件分类", 30_000);
  check("添加菜单「管理插件」直达设置页插件分类", true);
  await click(page, '[data-testid="plugin-guide-toggle"]');
  await waitFor(() => exists(page, '[data-testid="plugin-guide"]'), "插件中心展开开发规范", 30_000);
  check("插件开发规范以弹窗展示", ((await text(page, '[data-testid="plugin-guide"]')) ?? "").includes("Magi 插件作者指南"));
  check("弹窗提供复制与导出 Markdown", (await exists(page, '[data-testid="plugin-guide-copy"]')) && (await exists(page, '[data-testid="plugin-guide-export"]')));
  await click(page, '[data-testid="plugin-guide-copy"]');
  await waitFor(async () => ((await text(page, '[data-testid="plugin-guide-copy"]')) ?? "").includes("已复制"), "复制后按钮提示已复制", 10_000);
  check("复制后给出已复制反馈", true);
  await click(page, '[data-testid="plugin-guide-close"]');
  await waitFor(async () => !(await exists(page, '[data-testid="plugin-guide"]')), "关闭规范弹窗", 10_000);
  check("未安装 GPT Web 插件时设置导航没有它的独立分类", !(await exists(page, '[data-testid="settings-nav-webModel"]')));

  await openPluginDetail();
  await waitFor(() => exists(page, currentRow), "设置页当前作用域行");
  await click(page, `${currentRow} [data-action="deactivate"]`);
  await waitFor(async () => (await page.evaluate(`document.querySelector(${JSON.stringify(currentRow)})?.dataset.state`)) === "enabled", "停用后状态回到已启用", 45_000);
  check("停用成功且保留启用意愿", true);
  await click(page, '[data-testid="settings-back"]');
  await waitFor(async () => !(await exists(page, '[data-testid="top-tab-plugin"]')), "停用后顶部入口消失");
  await waitFor(async () => !(await exists(page, '[data-testid="plugin-view"]')), "停用后已打开的插件视图被关闭");
  check("停用后入口、主内容区页面和右栏视图全部消失", true);
  check("停用后主页卡片同时消失", !(await exists(page, '[data-testid="plugin-home-card"]')));
  const afterDeactivate = await api(page, `/api/plugins/${PLUGIN_ID}/resources/board?scope=${encodeURIComponent(scope)}`);
  check("停用后直接调用资源 API 被拒绝（不靠隐藏入口）", afterDeactivate.status === 403, String(afterDeactivate.status));

  // 10. 重新激活 → 升级：排空全部作用域，授权清空、启用保留、数据保留
  await openPluginDetail();
  await waitFor(() => exists(page, currentRow), "设置页行");
  await click(page, `${currentRow} [data-action="activate"]`);
  await waitFor(async () => (await page.evaluate(`document.querySelector(${JSON.stringify(currentRow)})?.dataset.state`)) === "active", "重新激活");
  const manifest = JSON.parse(await (await import("node:fs/promises")).readFile(join(repositoryRoot, "examples/plugins/board/manifest.json"), "utf8"));
  const v2 = join(workRoot, "board-v2");
  await mkdir(join(v2, "ui"), { recursive: true });
  manifest.version = "1.1.0";
  await writeFile(join(v2, "manifest.json"), JSON.stringify(manifest));
  const fs = await import("node:fs/promises");
  await fs.copyFile(join(repositoryRoot, "examples/plugins/board/plugin.mjs"), join(v2, "plugin.mjs"));
  await fs.copyFile(join(repositoryRoot, "examples/plugins/board/ui/index.html"), join(v2, "ui/index.html"));
  const archive2 = join(workRoot, "board-1.1.0.zip");
  await execFileAsync("node", [join(repositoryRoot, "scripts/pack-plugin.mjs"), v2, archive2]);
  // 准备阶段：预览不写入状态，只给出版本与权限差异；摘要不符的确认请求被拒绝。
  const preview = await post(page, "/api/plugins/preview", { source: `local:${archive2}` });
  const previewBody = JSON.parse(preview.body);
  check("升级候选包预览给出升级操作与权限差异", preview.status === 200 && previewBody.operation === "upgrade"
    && previewBody.installedVersion === "1.0.0" && previewBody.addedPermissions.length === 0, preview.body.slice(0, 240));
  const stillOld = JSON.parse((await api(page, "/api/plugins")).body).plugins.find((entry) => entry.manifest.id === PLUGIN_ID);
  check("预览不改变已安装版本", stillOld.manifest.version === "1.0.0");
  const mismatched = await post(page, "/api/plugins/upgrade", { source: `local:${archive2}`, expectedDigest: "0".repeat(64) });
  check("摘要与预览不一致的升级被拒绝", mismatched.status === 409, `${mismatched.status} ${mismatched.body.slice(0, 160)}`);

  // 经设置页走完整的“预览 → 确认 → 排空切换”流程。
  await openSettingsTab(page, "plugins");
  await click(page, '[data-testid="plugin-sources-open"]');
  await waitFor(() => exists(page, '[data-testid="plugin-address-input"]'), "安装来源弹窗中的地址输入框");
  check("地址输入框明确只接受远程地址", (await page.evaluate(`document.querySelector('[data-testid="plugin-address-input"]')?.getAttribute('placeholder')`)) === "远程插件包地址（HTTP(S)），本地 ZIP 请使用下方选择文件");
  await page.evaluate(`(() => { const input = document.querySelector('[data-testid="plugin-address-input"]'); input.value = ${JSON.stringify(`local:${archive2}`)}; input.dispatchEvent(new Event('input', { bubbles: true })); })()`);
  await click(page, '[data-testid="plugin-address-install"]');
  await waitFor(() => exists(page, '[data-testid="plugin-candidate"][data-operation="upgrade"]'), "升级确认面板出现");
  check("确认面板展示权限差异与版本变化", (await text(page, '[data-testid="plugin-candidate"]')).includes("1.1.0"));
  await click(page, '[data-action="confirm-candidate"]');
  await waitFor(async () => !(await exists(page, '[data-testid="plugin-candidate"]')), "确认后面板关闭", 60_000);
  await openPluginDetail();
  const upgraded = await api(page, "/api/plugins");
  check("升级经全作用域排空后切换", upgraded.status === 200 && JSON.parse(upgraded.body).plugins.find((entry) => entry.manifest.id === PLUGIN_ID)?.manifest.version === "1.1.0", upgraded.body.slice(0, 200));
  await page.evaluate(`window.dispatchEvent(new Event('focus'))`);
  await waitFor(async () => (await page.evaluate(`document.querySelector(${JSON.stringify(currentRow)})?.dataset.state`)) === "enabled", "升级后界面显示待重新授权", 15_000);
  check("升级后启用意愿保留、激活与授权清空", true);
  const data = await api(page, `/api/plugins/${PLUGIN_ID}/resources/board?scope=${encodeURIComponent(scope)}`);
  check("升级后未授权前资源 API 仍被拒绝", data.status === 403, String(data.status));
  await click(page, `${currentRow} [data-action="activate"]`);
  await waitFor(async () => (await page.evaluate(`document.querySelector(${JSON.stringify(currentRow)})?.dataset.state`)) === "active", "对新包重新授权激活");
  const preserved = JSON.parse((await api(page, `/api/plugins/${PLUGIN_ID}/resources/board?scope=${encodeURIComponent(scope)}`)).body);
  check("升级保留用户数据（命令清空后的版本 2）", preserved.version === 2, JSON.stringify(preserved));

  // 10b. 数据格式升级：新版提高 dataSchemaVersion 并声明转换，宿主在 Worker 中用新版入口转换已有数据
  const manifest3 = { ...manifest, version: "2.0.0", dataSchemaVersion: 2, dataMigration: { fromVersions: [1] } };
  const v3 = join(workRoot, "board-v3");
  await mkdir(join(v3, "ui"), { recursive: true });
  await writeFile(join(v3, "manifest.json"), JSON.stringify(manifest3));
  await fs.copyFile(join(repositoryRoot, "examples/plugins/board/plugin.mjs"), join(v3, "plugin.mjs"));
  await fs.copyFile(join(repositoryRoot, "examples/plugins/board/ui/index.html"), join(v3, "ui/index.html"));
  const archive3 = join(workRoot, "board-2.0.0.zip");
  await execFileAsync("node", [join(repositoryRoot, "scripts/pack-plugin.mjs"), v3, archive3]);
  // 先写入一条数据，使“已有数据 + 数据格式变化”成立。
  const beforeMigration = JSON.parse((await api(page, `/api/plugins/${PLUGIN_ID}/resources/board?scope=${encodeURIComponent(scope)}`)).body);
  const seeded = await write({ items: [{ text: "迁移前" }] }, beforeMigration.version);
  check("数据格式升级前写入一条数据", seeded.status === 200, seeded.body.slice(0, 160));
  const preview3 = JSON.parse((await post(page, "/api/plugins/preview", { source: `local:${archive3}` })).body);
  check("预览提示将自动转换已有数据", preview3.dataConversion === true && preview3.dataSchemaBlocked === false, JSON.stringify(preview3).slice(0, 200));
  const upgraded3 = await post(page, "/api/plugins/upgrade", { source: `local:${archive3}`, expectedDigest: preview3.digest });
  check("数据格式升级经 Worker 转换后切换成功", upgraded3.status === 200 && JSON.parse(upgraded3.body).plugins[0].manifest.version === "2.0.0", upgraded3.body.slice(0, 200));
  await page.evaluate(`window.dispatchEvent(new Event('focus'))`);
  await waitFor(async () => (await page.evaluate(`document.querySelector(${JSON.stringify(currentRow)})?.dataset.state`)) === "enabled", "数据升级后等待重新授权", 15_000);
  await click(page, `${currentRow} [data-action="activate"]`);
  await waitFor(async () => (await page.evaluate(`document.querySelector(${JSON.stringify(currentRow)})?.dataset.state`)) === "active", "数据升级后重新授权激活");
  const migrated = JSON.parse((await api(page, `/api/plugins/${PLUGIN_ID}/resources/board?scope=${encodeURIComponent(scope)}`)).body);
  check("转换后的数据生效且版本继续递增", migrated.version === JSON.parse(seeded.body).version + 1
    && migrated.value.schema === 2 && migrated.value.items[0].text === "迁移前" && migrated.value.items[0].migratedFrom === 1, JSON.stringify(migrated));

  // 10c. 插件中心：用发布工具签名目录，由真实 daemon 校验签名与摘要后安装
  const centerDir = join(workRoot, "center");
  const centerPlugin = join(centerDir, "src");
  await mkdir(centerPlugin, { recursive: true });
  await writeFile(join(centerPlugin, "manifest.json"), JSON.stringify({
    sdkVersion: 1, id: "example.centered", version: "1.0.0", name: "中心示例", description: "经插件中心发布的最小插件",
    backend: "plugin.mjs", permissions: [], dataSchemaVersion: 1,
    settingsSchema: { type: "object", additionalProperties: false }, contributions: { commands: [{ id: "ping", title: "Ping", description: "" }] },
  }));
  await writeFile(join(centerPlugin, "plugin.mjs"), "export default () => ({ content: 'pong' })");
  const centerArchive = join(centerDir, "example.centered-1.0.0.zip");
  await execFileAsync("node", [join(repositoryRoot, "scripts/pack-plugin.mjs"), centerPlugin, centerArchive]);
  const keyFile = join(centerDir, "center-key.pem");
  const keygen = await execFileAsync("node", [join(repositoryRoot, "scripts/plugin-center.mjs"), "keygen", keyFile, "--key-id", "e2e-key"]);
  const publicKey = /publicKey:\s+(\S+)/u.exec(keygen.stdout)?.[1];
  const centerServer = createServer(async (request, response) => {
    const name = decodeURIComponent((request.url ?? "/").split("/").pop() ?? "");
    const fsp = await import("node:fs/promises");
    try {
      const body = await fsp.readFile(join(centerDir, name));
      response.writeHead(200, { "content-type": name.endsWith(".json") ? "application/json" : "application/zip" });
      response.end(body);
    } catch { response.writeHead(404).end(); }
  });
  await new Promise((resolveListen) => centerServer.listen(daemonPort + 100, "127.0.0.1", resolveListen));
  const centerBase = `http://127.0.0.1:${centerServer.address().port}`;
  try {
    await execFileAsync("node", [join(repositoryRoot, "scripts/plugin-center.mjs"), "publish", "--key", keyFile, "--key-id", "e2e-key", "--base-url", centerBase, "--out", join(centerDir, "catalog.json"), centerArchive]);
    const put = (trustedKeys, file = "catalog.json") => api(page, "/api/plugins/center/config", {
      method: "PUT", headers: { "content-type": "application/json" }, body: JSON.stringify({ url: `${centerBase}/${file}`, trustedKeys }),
    });
    await put([{ keyId: "e2e-key", publicKey: Buffer.alloc(32, 7).toString("base64") }]);
    const untrusted = JSON.parse((await api(page, "/api/plugins/center")).body);
    check("目录签名与受信公钥不符时不展示任何条目", (untrusted.entries ?? []).length === 0 && Boolean(untrusted.error), JSON.stringify(untrusted).slice(0, 200));
    await put([{ keyId: "e2e-key", publicKey }]);
    const trusted = JSON.parse((await api(page, "/api/plugins/center")).body);
    check("发布工具签名的目录通过 daemon 验签并展示条目", trusted.entries?.some((entry) => entry.id === "example.centered" && entry.state === "available"), JSON.stringify(trusted).slice(0, 240));
    const centerInstall = await post(page, "/api/plugins/center/install", { id: "example.centered", version: "1.0.0" });
    const centered = JSON.parse(centerInstall.body).plugins?.find((entry) => entry.manifest.id === "example.centered");
    check("经插件中心安装且来源标识为 center", centerInstall.status === 200 && centered?.source.kind === "center", centerInstall.body.slice(0, 200));
    check("配置公钥且验签通过的目录标记为已签名", trusted.verified === true);
    await post(page, "/api/plugins/example.centered/uninstall", {});

    // 只填地址、不配公钥：普通目录（相对包地址）即可浏览和安装，按指定地址的信任级别对待。
    await execFileAsync("node", [join(repositoryRoot, "scripts/plugin-center.mjs"), "publish", "--out", join(centerDir, "plain.json"), centerArchive]);
    await execFileAsync("node", [join(repositoryRoot, "scripts/plugin-center.mjs"), "publish", "--out", join(centerDir, "official.json"), centerArchive]);
    await put([]);
    await openSettingsTab(page, "plugins");
    await click(page, '[data-testid="plugin-sources-open"]');
    await waitFor(() => exists(page, '[data-testid="plugin-center-url"]'), "设置页插件中心地址输入框");
    await page.evaluate(`(() => { const input = document.querySelector('[data-testid="plugin-center-url"]'); input.value = ${JSON.stringify(`${centerBase}/plain.json`)}; input.dispatchEvent(new Event('input', { bubbles: true })); })()`);
    await click(page, '[data-testid="plugin-center-save"]');
    await waitFor(() => exists(page, '[data-testid="plugin-center-unsigned"]'), "未签名目录的提示出现", 30_000);
    check("只填地址即可浏览插件中心，并提示目录未签名", true);
    const plain = JSON.parse((await api(page, "/api/plugins/center")).body);
    check("普通目录无需公钥，相对包地址按目录位置解析", plain.verified === false && plain.entries.some((entry) => entry.id === "example.centered"), JSON.stringify(plain).slice(0, 240));
    const plainInstall = await post(page, "/api/plugins/center/install", { id: "example.centered", version: "1.0.0" });
    const plainInstalled = JSON.parse(plainInstall.body).plugins?.find((entry) => entry.manifest.id === "example.centered");
    check("未签名目录安装的插件来源标识为指定地址", plainInstall.status === 200 && plainInstalled?.source.kind === "address" && plainInstalled.source.location.startsWith(centerBase), plainInstall.body.slice(0, 200));
    await post(page, "/api/plugins/example.centered/uninstall", {});
    // 大量插件：目录有 45 个条目时列表分页（每页 20 个），可搜索与过滤。
    const bulk = { format: 1, plugins: Array.from({ length: 45 }, (_, index) => ({
      id: `bulk.plugin-${String(index).padStart(2, "0")}`, name: `批量插件 ${String(index).padStart(2, "0")}`, description: `用于验证分页的第 ${index} 个插件`,
      version: "1.0.0", url: `packages/none-${index}.zip`, sha256: "a".repeat(64), size: 1024 })) };
    await (await import("node:fs/promises")).writeFile(join(centerDir, "bulk.json"), JSON.stringify(bulk));
    await put([], "bulk.json");
    await page.evaluate(`window.dispatchEvent(new Event('focus'))`);
    await openSettingsTab(page, "plugins");
    if (await exists(page, '[data-testid="plugin-sources-close"]')) await click(page, '[data-testid="plugin-sources-close"]');
    await waitFor(async () => (await page.evaluate(`document.querySelectorAll('[data-testid="plugin-row"]').length`)) === 20, "第一页只展示 20 个插件", 30_000);
    check("目录超过一页时分页展示（每页 20 个）", await exists(page, '[data-testid="plugin-pager"]'));
    // 左右分栏：右侧默认展示列表第一项，点击其他项在同一页面切换，不弹窗。
    const firstId = await page.evaluate(`document.querySelector('[data-testid="plugin-row"]').dataset.pluginId`);
    check("右侧详情默认展示列表第一个插件", await exists(page, PLUGIN_DETAIL(firstId)) && (await page.evaluate(`document.querySelector('[data-testid="plugin-row"]').getAttribute('aria-current')`)) === "true");
    const secondId = await page.evaluate(`document.querySelector('[data-testid="plugin-row"][data-status="available"]').dataset.pluginId`);
    const dialogsBefore = await page.evaluate(`document.querySelectorAll('[role="dialog"]').length`);
    await click(page, `[data-testid="plugin-row"][data-plugin-id="${secondId}"]`);
    await waitFor(() => exists(page, PLUGIN_DETAIL(secondId)), "点击另一行后右侧切换到该插件");
    check("点击列表行在同一页面切换详情且没有新弹窗", (await page.evaluate(`document.querySelectorAll('[role="dialog"]').length`)) === dialogsBefore);
    check("未安装的插件在详情面板给出醒目的安装按钮", await exists(page, `${PLUGIN_DETAIL(secondId)} [data-testid="plugin-detail-install"]`));
    await click(page, '[data-testid="plugin-page-next"]');
    await waitFor(async () => (await page.evaluate(`document.querySelectorAll('[data-testid="plugin-row"]').length`)) > 0, "第二页有内容");
    check("可翻到下一页，上一页按钮可用", await page.evaluate(`!document.querySelector('[data-testid="plugin-page-prev"]').disabled`));
    await page.evaluate(`(() => { const input = document.querySelector('[data-testid="plugin-search"]'); input.value = '批量插件 07'; input.dispatchEvent(new Event('input', { bubbles: true })); })()`);
    await waitFor(async () => (await page.evaluate(`document.querySelectorAll('[data-testid="plugin-row"]').length`)) === 1, "搜索后只剩匹配项", 10_000);
    check("搜索过滤后回到第一页且不再分页", !(await exists(page, '[data-testid="plugin-pager"]')));
    await page.evaluate(`(() => { const input = document.querySelector('[data-testid="plugin-search"]'); input.value = ''; input.dispatchEvent(new Event('input', { bubbles: true })); })()`);
    await click(page, '[data-testid="plugin-filter-installed"]');
    await waitFor(async () => (await page.evaluate(`[...document.querySelectorAll('[data-testid="plugin-row"]')].every((row) => row.dataset.status !== 'available')`)), "「已安装」过滤只含已安装插件", 10_000);
    check("过滤「已安装」只展示已安装的插件", true);
    await click(page, '[data-testid="plugin-filter-all"]');

    // 官方插件中心：从未配置（或清空地址）时使用默认地址，来自官方地址的目录算插件中心来源。
    const restored = JSON.parse((await api(page, "/api/plugins/center/config", {
      method: "PUT", headers: { "content-type": "application/json" }, body: JSON.stringify({ url: "", trustedKeys: [] }),
    })).body);
    check("清空地址恢复官方插件中心", restored.url === `${centerBase}/official.json`, JSON.stringify(restored));
    const official = JSON.parse((await api(page, "/api/plugins/center")).body);
    check("官方插件中心无需任何配置且标记为可信", official.verified === true && official.entries.some((entry) => entry.id === "example.centered"), JSON.stringify(official).slice(0, 240));
    const officialInstall = await post(page, "/api/plugins/center/install", { id: "example.centered", version: "1.0.0" });
    const officialInstalled = JSON.parse(officialInstall.body).plugins?.find((entry) => entry.manifest.id === "example.centered");
    check("官方插件中心安装的插件来源标识为 center", officialInstall.status === 200 && officialInstalled?.source.kind === "center", officialInstall.body.slice(0, 200));
    await post(page, "/api/plugins/example.centered/uninstall", {});
    // 配置了公钥后，未签名目录被拒绝。
    await put([{ keyId: "e2e-key", publicKey }], "plain.json");
    const gated = JSON.parse((await api(page, "/api/plugins/center")).body);
    check("配置公钥后拒绝未签名目录", gated.entries.length === 0 && Boolean(gated.error), JSON.stringify(gated).slice(0, 200));
  } finally {
    centerServer.close();
  }

  // 10d. MCP 服务插件壳：未激活时管理面拒绝且不监听；激活后可用；停用即释放；重新激活按原配置恢复
  const MCP_ID = "magi.mcp-server";
  const before = await api(page, "/api/mcp-server/status");
  check("未安装 MCP 服务插件时管理面被拒绝", before.status === 403, String(before.status));
  check("未安装时设置导航没有 MCP 服务分类", !(await exists(page, '[data-testid="settings-nav-mcpServer"]')));
  const mcpInstalled = await post(page, `/api/plugins/bundled/${MCP_ID}/install`, {});
  const mcpManifest = JSON.parse(mcpInstalled.body).plugins.find((entry) => entry.manifest.id === MCP_ID)?.manifest;
  check("内置 MCP 服务插件安装成功", mcpInstalled.status === 200 && Boolean(mcpManifest), mcpInstalled.body.slice(0, 160));
  await post(page, `/api/plugins/${MCP_ID}/enable`, { scope: "application" });
  await post(page, `/api/plugins/${MCP_ID}/authorize`, { scope: "application", grants: mcpManifest.permissions });
  const mcpActivated = await post(page, `/api/plugins/${MCP_ID}/activate`, { scope: "application" });
  check("MCP 服务插件应用级激活成功", mcpActivated.status === 200, mcpActivated.body.slice(0, 160));
  const mcpEnabled = JSON.parse((await post(page, "/api/mcp-server/enabled", { enabled: true })).body);
  check("激活后可开启服务并监听本机端口", mcpEnabled.running === true && /^http:\/\/127\.0\.0\.1:\d+/.test(mcpEnabled.url ?? ""), JSON.stringify(mcpEnabled).slice(0, 200));
  const reachable = async (url) => fetch(url, { method: "POST", headers: { "content-type": "application/json" }, body: "{}" }).then((response) => response.status).catch(() => null);
  check("监听端口要求令牌（无令牌 401）", (await reachable(mcpEnabled.url)) === 401);
  const mcpOff = await post(page, `/api/plugins/${MCP_ID}/deactivate`, { scope: "application" });
  check("停用 MCP 服务插件成功", mcpOff.status === 200, mcpOff.body.slice(0, 160));
  check("停用后监听端口立即释放", (await reachable(mcpEnabled.url)) === null);
  check("停用后管理面被拒绝", (await api(page, "/api/mcp-server/status")).status === 403);
  await post(page, `/api/plugins/${MCP_ID}/authorize`, { scope: "application", grants: mcpManifest.permissions });
  await post(page, `/api/plugins/${MCP_ID}/activate`, { scope: "application" });
  // MCP 客户端设置：工具目录固定完整开放，只选择逐次询问或开放全部权限。
  await page.call("Page.reload", { ignoreCache: true });
  await waitFor(() => exists(page, '[data-testid="input-textarea"]'), "MCP 激活后工作台重新加载");
  await openSettingsTab(page, "mcpServer");
  await click(page, '[data-mcp-tab="clients"]');
  await waitFor(() => exists(page, '[data-mcp-create-open="1"]'), "MCP 客户端设置出现");
  await click(page, '[data-mcp-create-open="1"]');
  await waitFor(() => exists(page, '[data-mcp-approval-mode="1"]'), "MCP 客户端授权方式选择器出现");
  check("新令牌默认完整工具目录并逐次询问", await page.evaluate(`(() => ({
    hasProfile: Boolean(document.querySelector('[data-mcp-profile="1"]')),
    approval: document.querySelector('[data-mcp-approval-mode="1"]')?.value,
    hasDeny: [...(document.querySelector('[data-mcp-approval-mode="1"]')?.options ?? [])].some((option) => option.value === 'deny'),
  }))()`).then((state) => !state.hasProfile && state.approval === 'ask' && !state.hasDeny));
  await page.evaluate(`(() => {
    const select = document.querySelector('[data-mcp-approval-mode="1"]');
    select.value = 'always';
    select.dispatchEvent(new Event('change', { bubbles: true }));
  })()`);
  check("开放权限无需第二个风险确认，网络开关仍独立可选", !(await exists(page, '[data-mcp-confirm-risk="1"]')) && await exists(page, '[data-mcp-allow-network="1"]'));
  await page.evaluate(`(() => [...document.querySelectorAll('form.create-form')].at(-1)?.querySelector('button[type="button"]')?.click())()`);
  const restoredUrl = await waitFor(async () => {
    const status = JSON.parse((await api(page, "/api/mcp-server/status")).body);
    return status.running ? status.url : null;
  }, "重新激活后服务按已保存的配置恢复", 30_000);
  check("重新激活后按原配置恢复监听（端口不变）", restoredUrl === mcpEnabled.url, `${restoredUrl} vs ${mcpEnabled.url}`);
  // 10e. 外部会话：外部 agent 通过 MCP 调用本地工具的过程，是对应工作区下的一个真正的（只读）会话，
  // 调用写进它的 canonical 时间线，在主对话区里和内部对话同一套展示
  await writeFile(join(workspaceRoot, "external-read.txt"), "hello from an external agent");
  const autoIssued = JSON.parse((await post(page, "/api/mcp-server/tokens", {
    clientName: "e2e-auto-agent",
    workspaceId,
    approvalMode: "always",
    ttlDays: 1,
  })).body);
  const autoCall = (name, args_) => fetch(restoredUrl, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${autoIssued.secret}` },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name, arguments: args_ } }),
  }).then((response) => response.json());
  const autoWrite = await autoCall("magi.fs.write", { path: "auto-mode.txt", content: "auto-approved\n" });
  const pendingAfterAutoWrite = JSON.parse((await api(page, "/api/mcp-server/approvals")).body).approvals ?? [];
  check("始终授权令牌的普通写入无需 Magi 弹窗", autoWrite.result?.isError !== true && pendingAfterAutoWrite.length === 0, JSON.stringify(autoWrite).slice(0, 200));
  await writeFile(join(workspaceRoot, "auto-delete.txt"), "remove without prompt");
  const autoRemove = await autoCall("magi.fs.remove", { path: "auto-delete.txt" });
  const removedFileStillExists = await access(join(workspaceRoot, "auto-delete.txt")).then(() => true).catch(() => false);
  check("开放权限也自动执行删除类 MCP 工具", autoRemove.result?.isError !== true && !removedFileStillExists, JSON.stringify(autoRemove).slice(0, 200));
  const autoExec = await autoCall("magi.shell.exec", { command: "printf open-mode" });
  check("开放权限也自动执行命令工具", autoExec.result?.isError !== true && JSON.stringify(autoExec).includes("open-mode"), JSON.stringify(autoExec).slice(0, 200));
  // 使用设置页实际生成的配置，验证发行包自己的 stdio 中继，避免维护另一套命令与权限合同。
  const snippets = JSON.parse((await api(page, `/api/mcp-server/config-snippets?tokenId=${autoIssued.token.tokenId}`)).body);
  const relayConfig = Object.values(snippets.stdioJson.mcpServers)[0];
  const relay = spawn(relayConfig.command, relayConfig.args, {
    env: { ...process.env, ...relayConfig.env }, stdio: ["pipe", "pipe", "pipe"],
  });
  const replies = new Map();
  const lines = createInterface({ input: relay.stdout });
  let relayError = null;
  relay.on("error", (error) => { relayError = error; });
  relay.stderr.on("data", () => {});
  lines.on("line", (line) => { const reply = JSON.parse(line); replies.set(reply.id, reply); });
  let relayId = 0;
  const send = async (method, params) => {
    const id = ++relayId;
    relay.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`);
    return waitFor(() => {
      if (relayError) throw relayError;
      if (relay.exitCode !== null && !replies.has(id)) throw new Error("stdio 中继提前退出");
      return replies.get(id);
    }, `stdio ${method}`, 10_000);
  };
  try {
    const initialized = await send("initialize", { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "plugin-e2e-stdio", version: "1" } });
    check("生成的客户端配置可通过发行包 stdio 中继握手", initialized.result?.serverInfo?.name === "magi");
    const listed = await send("tools/list", {});
    const names = (listed.result?.tools ?? []).map((tool) => tool.name);
    check("stdio 目录统一包含读取、写入、删除和命令", ["magi.fs.read", "magi.fs.write", "magi.fs.remove", "magi.shell.exec"].every((name) => names.includes(name)));
    const read = await send("tools/call", { name: "magi.fs.read", arguments: { path: "external-read.txt" } });
    check("stdio 读取真实工作区内容", read.result?.isError === false && JSON.stringify(read).includes("hello from an external agent"));
    const write = await send("tools/call", { name: "magi.fs.write", arguments: { path: "stdio-write.txt", content: "via packaged stdio" } });
    check("stdio 开放权限写入实际文件", write.result?.isError === false && (await readFile(join(workspaceRoot, "stdio-write.txt"), "utf8")) === "via packaged stdio");
    const escape = await send("tools/call", { name: "magi.fs.read", arguments: { path: "../outside.txt" } });
    check("stdio 越出工作区拒绝", escape.result?.isError === true);
    const cancelId = relayId + 1;
    const pendingShell = send("tools/call", { name: "magi.shell.exec", arguments: {
      command: "printf started > stdio-cancel-started.txt; sleep 30; printf finished > stdio-cancel-finished.txt",
    } });
    await waitFor(() => access(join(workspaceRoot, "stdio-cancel-started.txt")).then(() => true).catch(() => false), "stdio 命令实际开始");
    const concurrentRead = await send("tools/call", { name: "magi.fs.read", arguments: { path: "external-read.txt" } });
    relay.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", method: "notifications/cancelled", params: { requestId: cancelId } })}\n`);
    const cancelledShell = await pendingShell;
    check("stdio 取消通知终止指定命令并保留另一条读取结果", cancelledShell.result?.isError === true
      && JSON.stringify(cancelledShell).includes("cancelled") && concurrentRead.result?.isError === false
      && !(await access(join(workspaceRoot, "stdio-cancel-finished.txt")).then(() => true).catch(() => false)), JSON.stringify(cancelledShell).slice(0, 250));
  } finally {
    lines.close();
    relay.stdin.end();
    relay.kill();
  }
  const invalidRelay = await execFileAsync(relayConfig.command, relayConfig.args, {
    env: { ...process.env, ...relayConfig.env, MAGI_MCP_TOKEN: "magi_mcp_wrong" }, timeout: 5_000,
  }).then((output) => output, (error) => error);
  check("stdio 错误令牌被拒绝并断开", /unauthorized/u.test(invalidRelay.stdout ?? ""));
  const issued = JSON.parse((await post(page, "/api/mcp-server/tokens", { clientName: "e2e-agent", workspaceId, ttlDays: 1 })).body);
  const mcpCall = (name, args_) => fetch(restoredUrl, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${issued.secret}` },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name, arguments: args_ } }),
  }).then((response) => response.json());
  const externalReadPending = mcpCall("magi.fs.read", { path: "external-read.txt" });
  const readApproval = await waitFor(async () => {
    const approvals = JSON.parse((await api(page, "/api/mcp-server/approvals")).body).approvals ?? [];
    return approvals.find((approval) => approval.tokenId === issued.token.tokenId) ?? null;
  }, "每次询问模式下读取操作也请求确认");
  const readApprovalResult = await post(page, "/api/mcp-server/approvals/resolve", {
    approvalId: readApproval.approvalId,
    decision: "allow_once",
  });
  check("可单次允许 MCP 读取操作", readApprovalResult.status === 200, readApprovalResult.body.slice(0, 200));
  const externalRead = await externalReadPending;
  check("外部 agent 经 MCP 读取本地文件成功", externalRead.result?.isError !== true && JSON.stringify(externalRead).includes("hello from an external agent"), JSON.stringify(externalRead).slice(0, 200));
  const externalDenied = await mcpCall("magi.fs.read", { path: "../escape.txt" });
  check("越出工作区的调用被拒绝", externalDenied.result?.isError === true);
  const sessionsList = JSON.parse((await api(page, "/api/mcp-server/sessions")).body).sessions;
  const externalSession = sessionsList.find((entry) => entry.tokenId === issued.token.tokenId);
  check("daemon 为外部客户端建立了外部会话，归属令牌绑定的工作区", externalSession?.workspaceId === workspaceId && externalSession.sessionId.startsWith("external-tool-"), JSON.stringify(sessionsList).slice(0, 200));
  await page.evaluate(`window.dispatchEvent(new Event('focus'))`);
  const sessionItem = '[data-testid="external-session-item"]';
  await page.evaluate(`(() => { const header = document.querySelector('[data-workspace-id="${workspaceId}"]'); if (header && header.getAttribute('aria-expanded') !== 'true') header.click(); })()`);
  await waitFor(() => exists(page, sessionItem), "工作区下出现外部客户端会话", 30_000);
  check("外部会话进入工作区的统一会话列表", await exists(page, `.session-item[data-session-id="${externalSession.sessionId}"]`));
  check("外部会话按最近活动时间排在普通会话中", await page.evaluate(`(() => {
    const item = document.querySelector('[data-testid="external-session-item"]');
    const list = item?.closest('.session-list');
    const times = list ? [...list.querySelectorAll('.session-item')].map((entry) => Number(entry.dataset.sessionUpdatedAt ?? 0)) : [];
    return Boolean(item && list && !item.closest('.external-list') && times.every((time, index) => index === 0 || times[index - 1] >= time));
  })()`));
  check("侧栏的外部会话以客户端名称标识并标注「外部」", await page.evaluate(`(() => { const item = document.querySelector('[data-testid="external-session-item"]'); return Boolean(item?.closest('.workspace-children')) && item.innerText.includes('e2e-agent') && item.innerText.includes('外部'); })()`));
  if (await exists(page, '[data-testid="settings-back"]')) await click(page, '[data-testid="settings-back"]').catch(() => {});
  // 停在「变更」标签时点外部会话，也要回到「对话」标签看到它。
  await page.evaluate(`document.querySelectorAll('.tab-bar--top .tab-item')[1]?.click()`);
  await sleep(300);
  await click(page, sessionItem);
  await waitFor(() => exists(page, '[data-testid="external-session-bar"]'), "打开外部会话后输入区换成只读提示", 30_000);
  check("点外部会话后顶部标签回到「对话」", await page.evaluate(`document.querySelector('.tab-bar--top .tab-item')?.classList.contains('active') === true`));
  check("外部会话在主对话区打开，没有对话输入框", !(await exists(page, '[data-testid="input-textarea"]')));
  check("打开外部会话后仍保留在统一会话列表", await exists(page, `.session-item[data-session-id="${externalSession.sessionId}"]`));
  await waitFor(async () => (await page.evaluate(`document.body.innerText`)).includes("external-read.txt"), "时间线里出现外部调用的工具卡片", 30_000).catch(async (error) => {
    const dom = await page.evaluate(`JSON.stringify({ thread: document.querySelector('.thread-panel')?.innerText?.slice(0, 300), html: document.querySelector('.main-content')?.innerHTML?.replace(/<style[\\s\\S]*?<\\/style>/g, '').slice(0, 1200), bar: Boolean(document.querySelector('[data-testid="external-session-bar"]')), settings: Boolean(document.querySelector('[data-testid="settings-page"]')?.getClientRects().length), vis: document.visibilityState })`);
    const api_ = (await api(page, `/api/messages?scope=workspace&workspaceId=${encodeURIComponent(workspaceId)}&sessionId=${encodeURIComponent(externalSession.sessionId)}`)).body;
    throw new Error(`${error.message}；${dom}；API=${api_.slice(0, 900)}`);
  });
  check("外部调用在时间线里按工具卡片展示（读取的文件名）", true);
  // 写入需要在 Magi 中确认：确认出现在会话自己的授权托盘里（和内部对话同一个组件），允许一次后写入成功。
  // 窗口不在前台时，新的待确认发系统通知（验收里用桩捕获 Notification 调用）。
  await page.evaluate(`window.__notes = []; document.hasFocus = () => false; window.Notification = class { static permission = 'granted'; constructor(title, options) { window.__notes.push({ title, body: options?.body ?? '' }); } close() {} };`);
  const writing = mcpCall("magi.fs.write", { path: "ext-write.txt", content: "written by external\n" });
  writing.catch(() => {});
  await waitFor(() => exists(page, '[data-approval-id]'), "外部写入的待确认出现在会话的授权托盘里", 30_000);
  check("等待确认的写入出现在外部会话自己的授权托盘里", true);
  const notes = await waitFor(async () => { const list = JSON.parse(await page.evaluate(`JSON.stringify(window.__notes)`)); return list.length > 0 ? list : null; }, "窗口不在前台时发出系统通知", 20_000);
  check("待确认在窗口不在前台时发系统通知（带客户端名称与操作摘要）", notes[0].body.includes("e2e-agent") && notes[0].body.includes("ext-write.txt"), JSON.stringify(notes).slice(0, 200));
  await page.evaluate(`(() => { const button = [...document.querySelectorAll('[data-approval-id] button')].find((el) => el.innerText.includes('允许一次') || el.innerText.includes('Allow once')); if (!button) throw new Error('缺少单次授权按钮'); button.click(); })()`);
  const written = await writing;
  check("在会话里批准后外部写入成功", written.result?.isError !== true, JSON.stringify(written).slice(0, 200));
  await waitFor(async () => (await page.evaluate(`document.body.innerText`)).includes("ext-write.txt"), "写入展示为带文件名的卡片", 30_000);
  check("外部写入在会话里展示为带文件名的卡片", true);
  const writingAgain = mcpCall("magi.fs.write", { path: "ext-write-again.txt", content: "written by external after another prompt\n" });
  const secondApprovalAppeared = await waitFor(() => exists(page, '[data-approval-id]'), "下一次写入再次出现单次确认", 30_000).then(() => true).catch(() => false);
  if (secondApprovalAppeared) {
    await page.evaluate(`(() => { const button = [...document.querySelectorAll('[data-approval-id] button')].find((el) => el.innerText.includes('允许一次') || el.innerText.includes('Allow once')); if (!button) throw new Error('缺少第二次单次授权按钮'); button.click(); })()`);
  }
  const writtenAgain = await writingAgain;
  check("每次询问模式对下一次写入再次确认", secondApprovalAppeared && writtenAgain.result?.isError !== true, JSON.stringify(writtenAgain).slice(0, 200));
  const deniedWritePending = mcpCall("magi.fs.write", { path: "denied-write.txt", content: "must not be written" });
  const deniedApproval = await waitFor(async () => JSON.parse((await api(page, "/api/mcp-server/approvals")).body).approvals.find((approval) => approval.tokenId === issued.token.tokenId), "拒绝写入的待确认");
  await post(page, "/api/mcp-server/approvals/resolve", { approvalId: deniedApproval.approvalId, decision: "deny" });
  const deniedWrite = await deniedWritePending;
  check("拒绝当前 MCP 写入不产生文件", deniedWrite.result?.isError === true && !(await access(join(workspaceRoot, "denied-write.txt")).then(() => true).catch(() => false)));
  const cancelledWritePending = mcpCall("magi.fs.write", { path: "cancelled-write.txt", content: "must not be written" });
  await waitFor(() => page.evaluate(`[...document.querySelectorAll('[data-approval-id]')].some((tray) => tray.innerText.includes('cancelled-write.txt'))`), "取消前出现对应写入的授权托盘");
  const cancelledNotification = await fetch(restoredUrl, {
    method: "POST", headers: { "content-type": "application/json", authorization: `Bearer ${issued.secret}` },
    body: JSON.stringify({ jsonrpc: "2.0", method: "notifications/cancelled", params: { requestId: 1 } }),
  });
  const cancelledWrite = await cancelledWritePending;
  check("HTTP 取消待审批调用不产生文件", cancelledNotification.status === 202 && cancelledWrite.result?.isError === true
    && JSON.stringify(cancelledWrite).includes("cancelled")
    && !(await access(join(workspaceRoot, "cancelled-write.txt")).then(() => true).catch(() => false)));
  await waitFor(async () => !(await exists(page, '[data-approval-id]')), "取消后授权托盘消失");
  const cancelledCard = await waitFor(() => page.evaluate(`(() => { const card = [...document.querySelectorAll('.tool-call')].find((card) => card.innerText.includes('cancelled-write.txt') && card.querySelector('.status-cancelled')); return card ? card.innerText + ' ' + card.querySelector('.status-cancelled').getAttribute('aria-label') : ''; })()`), "取消写入的工具卡片显示已取消");
  check("取消后的界面没有遗留审批且工具卡片标记已取消", /已取消|Cancelled/iu.test(cancelledCard), cancelledCard.slice(0, 200));
  const changes = await autoCall("magi.changes.list", {});
  check("MCP 变更账本包含真实写入", changes.result?.isError === false && JSON.stringify(changes).includes("auto-mode.txt"));
  const audit = await api(page, "/api/mcp-server/audit?limit=20");
  check("MCP 审计保留记录且不回显令牌", JSON.parse(audit.body).entries.length > 0 && !audit.body.includes(issued.secret) && !audit.body.includes(autoIssued.secret));
  // 不保存调用内容：读到的内容不落库，动作与路径保留。
  const retention = await fetch(`http://127.0.0.1:${daemonPort}/api/mcp-server/external`, { method: "PUT", headers: { "content-type": "application/json" }, body: JSON.stringify({ saveContent: false, retentionDays: 30 }) });
  check("可关闭保存调用内容", retention.status === 200);
  await writeFile(join(workspaceRoot, "private.txt"), "very private body");
  const privateReadPending = mcpCall("magi.fs.read", { path: "private.txt" });
  const privateReadApproval = await waitFor(async () => {
    const approvals = JSON.parse((await api(page, "/api/mcp-server/approvals")).body).approvals ?? [];
    return approvals.find((approval) => approval.tokenId === issued.token.tokenId) ?? null;
  }, "每次询问模式下的隐私读取也请求确认");
  check("每次询问模式下的隐私读取也请求确认", true);
  await post(page, "/api/mcp-server/approvals/resolve", {
    approvalId: privateReadApproval.approvalId,
    decision: "allow_once",
  });
  const privateRead = await privateReadPending;
  check("关闭保存后外部调用本身仍然成功", privateRead.result?.isError !== true);
  const turnsText = JSON.stringify(JSON.parse((await api(page, `/api/messages?scope=workspace&workspaceId=${encodeURIComponent(workspaceId)}&sessionId=${encodeURIComponent(externalSession.sessionId)}`)).body));
  check("关闭保存后读到的内容不落库、动作与路径保留", !turnsText.includes("very private body") && turnsText.includes("private.txt"), turnsText.slice(0, 200));
  await waitFor(async () => {
    const history = JSON.parse((await api(page, `/api/messages?scope=workspace&workspaceId=${encodeURIComponent(workspaceId)}&sessionId=${encodeURIComponent(externalSession.sessionId)}`)).body);
    return history.canonicalTurns.at(-1)?.status === 'completed';
  }, "外部调用空闲后 daemon 收口活动段", 45_000);
  await waitFor(() => page.evaluate(`!document.querySelector('.runtime-diagnostics-slot')?.innerText.includes('运行中')
    && !document.querySelector('.turn-runtime-indicator')`), "外部活动段完成后界面不再显示运行中");
  check("外部活动段自动收口且实时清理运行状态，无需刷新", true);
  await fetch(`http://127.0.0.1:${daemonPort}/api/mcp-server/external`, { method: "PUT", headers: { "content-type": "application/json" }, body: JSON.stringify({ saveContent: true, retentionDays: 30 }) });

  const revokedShellPending = autoCall("magi.shell.exec", { command: "printf started > revoke-shell-started.txt; sleep 2; printf finished > revoke-shell-finished.txt" });
  await waitFor(() => access(join(workspaceRoot, "revoke-shell-started.txt")).then(() => true).catch(() => false), "吊销前命令实际开始");
  await api(page, `/api/mcp-server/tokens/${autoIssued.token.tokenId}`, { method: "DELETE" });
  const revokedShell = await revokedShellPending;
  check("吊销令牌停止在途命令且不产生后续写入", revokedShell.result?.isError === true
    && JSON.parse(revokedShell.result.content[0].text).cancelled === true
    && !(await access(join(workspaceRoot, "revoke-shell-finished.txt")).then(() => true).catch(() => false)), JSON.stringify(revokedShell).slice(0, 200));
  const revoked = await fetch(restoredUrl, { method: "POST", headers: { "content-type": "application/json", authorization: `Bearer ${autoIssued.secret}` }, body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "ping", params: {} }) });
  check("吊销客户端令牌后 HTTP 立即拒绝", revoked.status === 401);
  const revokedSession = JSON.parse((await api(page, "/api/mcp-server/sessions")).body).sessions.find((session) => session.tokenId === autoIssued.token.tokenId);
  await click(page, `.session-item[data-session-id="${revokedSession.sessionId}"]`);
  await waitFor(() => page.evaluate(`(() => {
    const card = [...document.querySelectorAll('.tool-call')].find((card) => card.innerText.includes('revoke-shell-started.txt'));
    return Boolean(card?.querySelector('.status-cancelled'));
  })()`), "吊销命令的会话卡片显示已取消");
  check("吊销后会话卡片显示实际取消状态", true);

  await post(page, "/api/mcp-server/enabled", { enabled: false });
  await post(page, `/api/plugins/${MCP_ID}/deactivate`, { scope: "application" });

  // 11. Renderer 无未处理异常
  const logText = electronLog.join("");
  check("历史快照与实时事件无 canonical 事实冲突", !rendererErrors.some((error) => error.includes('[canonical-turn-store]')), rendererErrors.slice(-4).join('\n'));
  check("Electron 日志没有 Desktop IPC 协议拒绝", !/IPC.*(validation|拒绝)/iu.test(logText));
  check("插件 iframe 没有 sandbox 初始化异常或串入应用消息协议", !/sandboxed_renderer.bundle.js script failed|binding.startupData|未知消息类型: request/u.test([...rendererErrors, logText].join("\n")));
} catch (error) {
  failure = error;
  if (page) {
    const body = await page.evaluate(`document.body.innerText.slice(0, 1500)`).catch(() => "");
    process.stderr.write(`\n[失败时页面文本]\n${body}\n`);
    process.stderr.write(`\n[Renderer 错误]\n${[...new Set(rendererErrors)].slice(-8).join("\n---\n")}\n`);
    process.stderr.write(`\n[Provider 请求]\n${providerRequests.map((request, index) => `#${index} tools=${(request.tools ?? []).map((tool) => tool.function?.name).filter((name) => name?.includes("board")).join(",")} last=${textOf((request.messages ?? []).at(-1)).slice(0, 160).replace(/\n/g, " ")}`).join("\n")}\n`);
    const nav = await page.evaluate(`[...document.querySelectorAll('[data-testid^="settings-nav-"]')].map((el) => el.dataset.testid + '=' + el.getAttribute('aria-current')).join(', ')`).catch(() => "");
    process.stderr.write(`\n[设置导航]\n${nav}\n`);
  }
  process.stderr.write(`\n[Electron 日志尾部]\n${electronLog.join("").slice(-2500)}\n`);
} finally {
  if (failure || process.env.MAGI_ELECTRON_PLUGINS_DIAGNOSTICS_DIR) {
    const diagnostics = process.env.MAGI_ELECTRON_PLUGINS_DIAGNOSTICS_DIR || workRoot;
    await mkdir(diagnostics, { recursive: true });
    await writeFile(join(diagnostics, "frames.json"), JSON.stringify({ failure: failure?.message, frameTrace, frameResponses, rendererErrors }, null, 2));
    await writeFile(join(diagnostics, "electron.log"), electronLog.join(""));
    if (page) {
      const screenshot = await page.call("Page.captureScreenshot", { format: "png" }).catch(() => null);
      if (screenshot) await writeFile(join(diagnostics, "screen.png"), Buffer.from(screenshot.data, "base64"));
    }
    process.stdout.write(`验收现场已保存: ${diagnostics}\n`);
  }
  page?.close();
  await app.stop();
  provider.closeAllConnections?.();
  provider.close();
  if (!failure) await rm(workRoot, { recursive: true, force: true }).catch(() => {});
}

process.stdout.write(`\n${summary()}\n`);
if (failure) {
  process.stderr.write(`${failure.message}\n`);
  process.exit(1);
}
