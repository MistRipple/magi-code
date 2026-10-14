import { createServer } from "node:http";
import { cp, mkdir, mkdtemp, readdir, readFile, rm, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import { setTimeout as sleep } from "node:timers/promises";
import { connectAppPage, connectTarget, createReporter, launchApp, pageTools, waitFor } from "./lib/electron-harness.mjs";

/**
 * GPT Web 第一方插件的真实账号验收（打包 Electron）。
 *
 * 需要本机已登录 ChatGPT：脚本只把 `persist:magi-web-model` 登录分区复制到一次性的独立状态根，
 * 不触碰用户正在使用的 ~/.magi，结束后删除副本。验证：未安装时无入口 → 安装并授权第一方插件 →
 * 引擎入口出现 → 临时对话发送一条消息并读回回复 → 粘贴红 / 蓝两张纯色图发送并读回颜色
 * （验证图片经页面文件输入送达网页，且 daemon 的 web-uploads 暂存目录已清空）→ 停用后入口与槽位释放。
 *
 * MAGI_GPT_WEB_QUESTION_FIXTURE=1：不用账号，验证受控 DIL 多阶段卡片（不是远端模型验收）。
 * MAGI_GPT_WEB_REAL_QUESTION=1：额外验证真账号生成的原生卡片和作答（需账号支持，不满足时显式失败）。
 * 环境变量：MAGI_GPT_WEB_SOURCE_USERDATA（默认 ~/.magi/desktop）、MAGI_GPT_WEB_APP_EXECUTABLE、
 * MAGI_GPT_WEB_CDP_PORT（9311）、MAGI_GPT_WEB_DAEMON_PORT（38941）、MAGI_GPT_WEB_SCREENSHOTS（截图目录）。
 */

const repositoryRoot = new URL("..", import.meta.url).pathname.replace(/\/$/u, "");
const appExecutable = process.env.MAGI_GPT_WEB_APP_EXECUTABLE?.trim()
  || join(repositoryRoot, "target/electron-dist/mac-arm64/Magi.app/Contents/MacOS/Magi");
const sourceUserData = process.env.MAGI_GPT_WEB_SOURCE_USERDATA?.trim() || join(homedir(), ".magi", "desktop");
const cdpPort = Number.parseInt(process.env.MAGI_GPT_WEB_CDP_PORT || "9311", 10);
const daemonPort = Number.parseInt(process.env.MAGI_GPT_WEB_DAEMON_PORT || "38941", 10);
const screenshotDir = process.env.MAGI_GPT_WEB_SCREENSHOTS?.trim() || "";
const conversationMode = process.env.MAGI_GPT_WEB_MODE === "saved" ? "saved" : "temporary";
const PLUGIN_ID = "openai.chatgpt-web";
const REPLY_TOKEN = "MAGI_PLUGIN_E2E_OK";

const { check, summary } = createReporter();
const rendererErrors = [];
let lastProbe = "";

// MAGI_GPT_WEB_FRESH=1：不复制登录分区，验证全新环境下 surface 能挂载且探测给出“需要登录”。
const questionFixture = process.env.MAGI_GPT_WEB_QUESTION_FIXTURE === "1";
const fresh = !questionFixture && process.env.MAGI_GPT_WEB_FRESH === "1";
if (!fresh && !questionFixture && !existsSync(join(sourceUserData, "Partitions", "magi-web-model"))) {
  process.stdout.write("跳过：未找到已登录的 GPT Web 分区\n");
  process.exit(0);
}

// 本地 Provider 只用于让模型选择器有可列出的普通模型；GPT Web 的对话走真实网页，不经过它。
const provider = createServer((request, response) => {
  if (request.url === "/v1/models") {
    response.writeHead(200, { "content-type": "application/json" });
    response.end(JSON.stringify({ object: "list", data: [{ id: "gpt-web-e2e-model", object: "model" }] }));
  } else {
    response.writeHead(404).end();
  }
});
await new Promise((resolve) => provider.listen(0, "127.0.0.1", resolve));
const providerPort = provider.address().port;

const workRoot = await mkdtemp(join(tmpdir(), "magi-electron-gpt-web-"));
const stateRoot = join(workRoot, "state");
await mkdir(join(stateRoot, "desktop", "Partitions"), { recursive: true });
if (!fresh && !questionFixture) {
  await cp(join(sourceUserData, "Partitions", "magi-web-model"), join(stateRoot, "desktop", "Partitions", "magi-web-model"), { recursive: true });
  await cp(join(sourceUserData, "Local State"), join(stateRoot, "desktop", "Local State")).catch(() => {});
  await cp(join(sourceUserData, "browser-partitions.json"), join(stateRoot, "desktop", "browser-partitions.json")).catch(() => {});
}

const app = launchApp({
  executable: appExecutable,
  cwd: repositoryRoot,
  cdpPort,
  inspectPort: process.env.MAGI_GPT_WEB_LAYOUT_CHECK === "1" ? cdpPort + 17 : null,
  env: {
    MAGI_DESKTOP_DAEMON_PORT: String(daemonPort),
    MAGI_STATE_ROOT: stateRoot,
    MAGI_OPEN_BROWSER: "0",
    MAGI_OPENAI_COMPAT_BASE_URL: `http://127.0.0.1:${providerPort}/v1`,
    MAGI_OPENAI_COMPAT_API_KEY: "gpt-web-e2e-key",
    MAGI_OPENAI_COMPAT_MODEL: "gpt-web-e2e-model",
  },
});

let fixtureWeb = null;
let page = null;
let tools = null;
let failure = null;
let shot = 0;
async function sendComposerMessage(text) {
  await page.evaluate(`(() => {
    const el = document.querySelector('[data-testid="input-textarea"]');
    el.focus(); el.textContent = ${JSON.stringify(text)};
    el.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertText', data: el.textContent }));
  })()`);
  await waitFor(async () => !(await page.evaluate(`document.querySelector('[data-testid="input-send-button"]')?.disabled ?? true`)), "发送按钮可用", 10_000);
  await tools.click('[data-testid="input-send-button"]');
}

async function snap(label) {
  if (!screenshotDir || !page) return;
  await mkdir(screenshotDir, { recursive: true });
  // 截图只是诊断辅助：GPU 关闭时 Chromium 可能不响应，不能让它掩盖真正的验收结论。
  const data = await Promise.race([tools.screenshot(), sleep(8_000).then(() => null)]).catch(() => null);
  if (!data) return;
  await writeFile(join(screenshotDir, `${String(++shot).padStart(2, "0")}-${label}.png`), Buffer.from(data, "base64"));
}

try {
  page = await waitFor(async () => {
    const candidate = await connectAppPage(cdpPort, daemonPort, rendererErrors).catch(() => null);
    if (!candidate) return null;
    const ready = await candidate.evaluate(`Boolean(document.querySelector('[data-testid="input-textarea"]'))`).catch(() => false);
    if (!ready) { candidate.close(); return null; }
    return candidate;
  }, "打包 Electron App Renderer 就绪", 60_000, 500);
  tools = pageTools(page);
  check("打包 Electron 启动并渲染工作台", true);

  // 未安装时：运行态读取不报错、不出现 GPT Web 入口
  const before = await tools.api("/api/browser/web-models/runtime");
  check("未安装时 GPT Web 接口明确拒绝", before.status !== 200, `${before.status}`);

  // 安装并授权第一方插件（与用户 ZIP 同一条校验/生命周期链路）
  const installed = await tools.post(`/api/plugins/bundled/${PLUGIN_ID}/install`, {});
  check("第一方 GPT Web 插件安装成功", installed.status === 200, installed.body.slice(0, 200));
  const manifest = JSON.parse(installed.body).plugins.find((item) => item.manifest.id === PLUGIN_ID).manifest;
  check("清单声明了 gpt-web 原生 Browser surface", manifest.contributions.browserSurfaces?.some((surface) => surface.id === "gpt-web"));
  const grants = manifest.permissions.map((permission) => ({ ...permission }));
  await tools.post(`/api/plugins/${PLUGIN_ID}/enable`, { scope: "application" });
  await tools.post(`/api/plugins/${PLUGIN_ID}/authorize`, { scope: "application", grants });
  const activated = await tools.post(`/api/plugins/${PLUGIN_ID}/activate`, { scope: "application" });
  check("应用级激活成功", activated.status === 200, activated.body.slice(0, 200));
  await page.call("Page.reload", { ignoreCache: true });
  await waitFor(() => tools.exists('[data-testid="input-textarea"]'), "重载后工作台", 30_000);
  await snap("after-activate");

  // 与真实使用一致：在工作区会话里打开 GPT Web（草稿/个人空状态下宿主没有会话上下文）。
  const workspaceRoot = join(workRoot, "workspace");
  await mkdir(workspaceRoot, { recursive: true });
  const registered = await tools.post("/api/workspaces/register", { path: workspaceRoot });
  const workspaceId = JSON.parse(registered.body).workspaceId;
  await page.call("Page.reload", { ignoreCache: true });
  await waitFor(() => tools.exists(`[data-workspace-id="${workspaceId}"]`), "工作区进入侧栏", 30_000);
  await page.evaluate(`(() => {
    const row = document.querySelector('[data-workspace-id="${workspaceId}"]');
    row.click();
    row.closest('.workspace-row')?.querySelector('button.row-action[aria-label]')?.click();
  })()`);
  await waitFor(() => tools.exists('[data-testid="input-textarea"]'), "工作区会话输入区", 30_000);

  // 与用户一致的入口：设置 → GPT Web（独立分类）→“连接/检查可用性”（投影应用级会话、挂载 guest、探测）
  await waitFor(async () => {
    const visible = await page.evaluate(`Boolean(document.querySelector('[data-testid="settings-page"]')?.getClientRects().length)`);
    if (!visible) await tools.click('[data-testid="sidebar-settings"]');
    await sleep(1200);
    await tools.click('[data-testid="settings-nav-webModel"]').catch(() => {});
    await sleep(600);
    return page.evaluate(`Boolean(document.getElementById('web-model-title'))`);
  }, "设置页出现独立的 GPT Web 分类", 30_000, 500);
  await page.evaluate(`document.querySelector('[data-web-model-connect]').click()`);
  await waitFor(() => tools.exists("webview"), "GPT Web webview 挂载", 45_000);
  check("连接 GPT Web 后右栏挂载原生 webview", true);
  if (questionFixture) {
    // 只在验收副本中拦截导航，原生 webview、Worker、daemon 和 UI 仍走真实链路。
    const html = await readFile(new URL("./fixtures/chatgpt-dil-question.html", import.meta.url), "utf8");
    fixtureWeb = await waitFor(() => connectTarget(cdpPort, (target) => target.type === "webview", "GPT Web guest").catch(() => null), "fixture guest", 30_000);
    fixtureWeb.on("Fetch.requestPaused", (event) => {
      const document = event.resourceType === "Document";
      const body = document ? html : JSON.stringify({ user: { name: "fixture" } });
      void fixtureWeb.call("Fetch.fulfillRequest", {
        requestId: event.requestId, responseCode: 200,
        responseHeaders: [{ name: "Content-Type", value: document ? "text/html; charset=utf-8" : "application/json" }],
        body: Buffer.from(body).toString("base64"),
      }).catch((error) => rendererErrors.push(String(error)));
    });
    await fixtureWeb.call("Fetch.enable", { patterns: [{ urlPattern: "https://chatgpt.com/*", requestStage: "Request" }] });
    await fixtureWeb.call("Page.navigate", { url: "https://chatgpt.com/" });
    await waitFor(() => fixtureWeb.evaluate(`document.title === 'DIL question fixture'`), "受控问题页面加载", 30_000);
  }


  // 回到工作台；GPT Web 标签就是右栏当前内容，点“检查连接”走用户的真实对齐路径。
  await tools.click('[data-testid="settings-back"]');
  await waitFor(async () => {
    await page.evaluate(`document.querySelector('[aria-label="展开右侧面板"]')?.click()`);
    return page.evaluate(`[...document.querySelectorAll('button')].some((b) => b.innerText.trim() === '检查连接')`);
  }, "GPT Web 标签出现“检查连接”", 20_000, 500);
  await page.evaluate(`[...document.querySelectorAll('button')].find((b) => b.innerText.trim() === '检查连接').click()`);
  // 登录态：复制来的持久分区应仍可用
  const probeResult = await waitFor(async () => {
    const result = await tools.post("/api/browser/web-models/probe", { clientPlatform: "desktop" });
    lastProbe = `${result.status} ${result.body.slice(0, 300)}`;
    const body = JSON.parse(result.body);
    return result.status === 200 && body.status !== "failed" ? body : null;
  }, "GPT Web 探测给出结论", 90_000, 3_000);
  check(fresh ? "全新环境探测结论为需要登录" : "探测结论为已登录可用（ok）", probeResult.status === (fresh ? "login_required" : "ok"), `status=${probeResult.status} reason=${probeResult.reason ?? ""}`);

  loggedInFlow: {
  if (fresh) break loggedInFlow; // 全新环境只验证到“需要登录”
  // 输入区模型选择器：登录探测通过后应出现固定的 GPT Web 入口
  await tools.click('.ia-model-btn');
  await sleep(800);
  await waitFor(() => page.evaluate(`[...document.querySelectorAll('.ia-model-wrap *')].some((el) => el.children.length === 0 && el.textContent.trim() === 'GPT Web')`), "选择器出现 GPT Web 入口", 15_000);
  check("探测通过后选择器出现固定的 GPT Web 入口", true);
  await page.evaluate(`(() => { const label = [...document.querySelectorAll('.ia-model-wrap *')].find((el) => el.children.length === 0 && el.textContent.trim() === 'GPT Web'); (label.closest('button, [role=option], [role=menuitem], li') || label).click(); })()`);
  await sleep(1200);
  // 对话方式：临时对话（不在 ChatGPT 侧留下历史）
  await tools.click(".ia-model-btn");
  await waitFor(() => tools.exists(`[data-web-model-mode-option="${conversationMode}"]`), "对话方式选择出现", 10_000);
  await tools.click(`[data-web-model-mode-option="${conversationMode}"]`);
  await sleep(500);
  check(`会话选择 ${conversationMode} 对话方式`, (await page.evaluate(`document.querySelector('[data-web-model-mode]')?.getAttribute('data-web-model-mode')`)) === conversationMode);
  await page.evaluate(`document.querySelector('.ia-reasoning-title, .ia-model-wrap')?.blur?.()`);
  await page.evaluate(`document.querySelector('.ia-popover-backdrop')?.click()`);
  await sleep(400);

  if (questionFixture) {
  await sendComposerMessage('问我晚上想吃什么，从先选主食，再选吃法');
    await waitFor(() => tools.exists('[data-user-question-id]'), "Magi 显示第一个网页问题", 30_000);
    check("网页六个选项投影到现有问题面板", await page.evaluate(`document.querySelectorAll('[data-user-question-id] .uq-option[role=radio]').length === 6`));
    check("页面没有的自定义输入与跳过不会出现在 Magi", await page.evaluate(`!document.querySelector('[data-user-question-id] .uq-other') && ![...document.querySelectorAll('[data-user-question-id] button')].some(b => b.innerText.trim() === '跳过')`));
    if (process.env.MAGI_GPT_WEB_LAYOUT_CHECK === "1") {
      for (const width of [960, 1100, 1440]) {
        await page.call("Emulation.setDeviceMetricsOverride", { width, height: 900, deviceScaleFactor: 1, mobile: false });
        await sleep(400);
        const layout = await page.evaluate(`(() => {
          const elements = [...document.querySelectorAll('[data-user-question-id], .markdown-content')];
          return elements.filter(e => e.getClientRects().length).map(e => ({ name:e.className, width:e.clientWidth, scroll:e.scrollWidth }));
        })()`);
        check(`窗口 ${width}px 下问题和正文无整体横向溢出`, layout.length > 0 && layout.every(item => item.width > 0 && item.scroll <= item.width + 1), JSON.stringify(layout));
        await snap(`narrow-question-${width}`);
      }
      // App Renderer 的设备模拟会影响嵌入目标；清除后再检验真实 guest。
      await page.call("Emulation.clearDeviceMetricsOverride");
      // Electron 不提供 Browser.getWindowForTarget；验收通过 Main inspector 调整真正的 BrowserWindow。
      const main = await connectTarget(cdpPort + 17, () => true, "Electron Main inspector");
      const mainWindow = `process.getBuiltinModule('module')._load('electron').BrowserWindow.getAllWindows()[0]`;
      const originalBounds = await main.evaluate(`${mainWindow}.getBounds()`);
      try {
        for (const width of [960, 1440]) {
          await main.evaluate(`${mainWindow}.setSize(${width}, 900)`);
          await sleep(1000);
          const surface = await page.evaluate(`(() => { const view = document.querySelector('webview'); return { width:view?.clientWidth, height:view?.clientHeight }; })()`);
          const viewport = await fixtureWeb.evaluate(`({width:innerWidth,height:innerHeight})`);
          check(`真实窗口 ${width}px 下 GPT Web 自动视口宽度匹配内容槽`, Math.abs(surface.width - viewport.width) <= 1, JSON.stringify({surface, viewport}));
        }
      } finally {
        await main.evaluate(`${mainWindow}.setBounds(${JSON.stringify(originalBounds)})`);
        main.close();
      }
      // 后台离屏 guest 保留推理页面；显示自适应需在用户真正打开的内容槽中验收。
      await tools.click('.right-pane-add-tab');
      await waitFor(() => tools.exists('.right-pane-add-menu-item[data-kind="webSession"]:not([disabled])'), "GPT Web 可显示入口", 15_000);
      await tools.click('.right-pane-add-menu-item[data-kind="webSession"]');
      await waitFor(() => page.evaluate(`document.querySelector('.right-pane-tabbar [data-tab-id="webSession:app"]')?.classList.contains('active')`), "GPT Web 标签处于激活态", 20_000);
      await sleep(1000);
      for (const width of [320, 600]) {
        await page.evaluate(`window.magiDesktop.submitLayoutIntent({ type: 'right_pane_width', width: ${width} })`);
        const sizes = await waitFor(async () => {
          const surfaceWidth = await page.evaluate(`document.querySelector('webview')?.clientWidth`);
          const viewportWidth = await fixtureWeb.evaluate(`innerWidth`);
          if (Math.abs(surfaceWidth - width) > 1 || Math.abs(surfaceWidth - viewportWidth) > 1) {
            throw new Error(JSON.stringify({ requestedWidth: width, surfaceWidth, viewportWidth }));
          }
          return {surfaceWidth, viewportWidth};
        }, `GPT Web 随右栏 ${width}px 自适应`, 15_000);
        check(`右栏 ${width}px 时网页视口随面板变化`, Boolean(sizes), JSON.stringify(sizes));
      }
      await page.evaluate(`window.magiDesktop.submitLayoutIntent({ type: 'right_pane_reset_width' })`);
    }
    const firstId = await page.evaluate(`document.querySelector('[data-user-question-id]').getAttribute('data-user-question-id')`);
    await snap("question-stage1");
    await page.evaluate(`[...document.querySelectorAll('[data-user-question-id] .uq-option')].find(b => b.innerText.includes('面条')).click()`);
    await tools.click('[data-user-question-id] .dock-btn--primary');
    await waitFor(async () => {
      const id = await page.evaluate(`document.querySelector('[data-user-question-id]')?.getAttribute('data-user-question-id')`);
      return id && id !== firstId;
    }, "回答后进入下一阶段问题", 20_000);
    check("回写网页后同一回合继续显示第二阶段", await page.evaluate(`document.querySelector('[data-user-question-id] .uq-question')?.innerText.includes('吃法')`));
    await snap("question-stage2");
    await page.evaluate(`[...document.querySelectorAll('[data-user-question-id] .uq-option')].find(b => b.innerText.includes('汤面')).click()`);
    await tools.click('[data-user-question-id] .dock-btn--primary');
    await waitFor(() => tools.exists('[data-user-question-id] .uq-other'), "网页自定义答案投影", 20_000);
    await tools.click('[data-user-question-id] .uq-other-toggle');
    await page.evaluate(`(() => { const input = document.querySelector('[data-user-question-id] .uq-other-input'); input.value = '少盐不辣'; input.dispatchEvent(new Event('input', {bubbles:true})); })()`);
    await tools.click('[data-user-question-id] .dock-btn--primary');
    await waitFor(() => page.evaluate(`document.querySelector('[data-user-question-id] .uq-question')?.innerText.includes('配菜')`), "自定义回答后继续下一阶段", 20_000);
    check("自定义输入撤销网页默认选项且回读确认", true);
    await page.evaluate(`[...document.querySelectorAll('[data-user-question-id] button')].find(b => b.innerText.trim() === '跳过').click()`);
    await waitFor(async () => (await page.evaluate(`document.body.innerText`)).includes('MAGI_QUESTION_E2E_OK:面条:汤面:少盐不辣:skipped'), "作答读回 Magi 最终回复", 20_000);
    check("网页支持跳过时由 Magi 回写跳过", true);
    await waitFor(async () => !(await tools.exists('[data-testid="input-stop-button"]')), "网页回合成功结束", 20_000);
    check("多阶段选择经站点适配器完成并读回最终回复", true);
    check("回答后无遗留问题面板", !(await tools.exists('[data-user-question-id]')));
    if (process.env.MAGI_GPT_WEB_LAYOUT_CHECK === "1") {
      for (const width of [960, 1100, 1440]) {
        await page.call("Emulation.setDeviceMetricsOverride", { width, height: 900, deviceScaleFactor: 1, mobile: false });
        await sleep(400);
        const layout = await page.evaluate(`(() => {
          const blocks = [...document.querySelectorAll('.markdown-content')].filter(e => e.getClientRects().length);
          return { page: { width: innerWidth, scroll:document.documentElement.scrollWidth }, blocks: blocks.map(e => ({width:e.clientWidth,scroll:e.scrollWidth})), tables: document.querySelectorAll('.markdown-content table').length, code: document.querySelectorAll('.markdown-content .code-pre').length };
        })()`);
        check(`窗口 ${width}px 下长链接、表格和代码不撑宽正文`, layout.tables > 0 && layout.code > 0 && layout.blocks.every(item => item.scroll <= item.width + 1), JSON.stringify(layout));
        check(`窗口 ${width}px 下工作台无横向滚动`, layout.page.scroll <= layout.page.width + 1, JSON.stringify(layout.page));
      }
      await page.call("Emulation.clearDeviceMetricsOverride");
    }

    await snap("question-final");
  } else {
  if (process.env.MAGI_GPT_WEB_ONLY_QUESTION !== "1") {
  // 发送一条真实消息：用户原文原样进入网页对话，回复读回 Magi
  await sendComposerMessage('请计算 12+30，只回复 ANS= 加上结果，不要其他文字。');
  await waitFor(async () => (await page.evaluate(`document.body.innerText`)).includes("ANS=42"), "GPT Web 回复读回 Magi（ANS=42）", 150_000, 1_500);
  check("真实 ChatGPT 网页回复读回 Magi 对话", true);
  await waitFor(async () => !(await tools.exists('[data-testid="input-stop-button"]')), "文字回合成功结束", 20_000);
  await snap("reply");

  // 多图：在 Magi 输入框粘贴红、蓝两张纯色 PNG，只让网页回答颜色——回复里同时出现两种颜色即证明两张图都送达。
  await page.evaluate(`(async () => {
    const solid = (color) => new Promise((resolve) => {
      const canvas = document.createElement('canvas');
      canvas.width = 96; canvas.height = 96;
      const context = canvas.getContext('2d');
      context.fillStyle = color; context.fillRect(0, 0, 96, 96);
      canvas.toBlob((blob) => resolve(blob), 'image/png');
    });
    const transfer = new DataTransfer();
    transfer.items.add(new File([await solid('#e00000')], 'red.png', { type: 'image/png' }));
    transfer.items.add(new File([await solid('#0000e0')], 'blue.png', { type: 'image/png' }));
    const el = document.querySelector('[data-testid="input-textarea"]');
    el.focus();
    el.dispatchEvent(new ClipboardEvent('paste', { clipboardData: transfer, bubbles: true, cancelable: true }));
  })()`);
  await waitFor(() => page.evaluate(`document.querySelectorAll('img[src^="data:image/png"]').length >= 2`), "输入区出现两张待发送图片", 10_000);
  await sendComposerMessage('这两张纯色图片的颜色分别是什么？只回复 COLORS= 加上两种颜色的中文名，用逗号分隔，按附件顺序，不要其他文字。');
  await waitFor(async () => /COLORS=\s*(红[^\n]{0,4}蓝|蓝[^\n]{0,4}红)/u.test(await page.evaluate(`document.body.innerText`)), "GPT Web 同时认出红、蓝两张图", 180_000, 1_500);
  check("Magi 粘贴的多张图片送达 GPT Web 并被识别", true);
  await waitFor(async () => !(await tools.exists('[data-testid="input-stop-button"]')), "图片回合成功结束", 20_000);
  await snap("images-reply");
  const leftovers = await readdir(join(stateRoot, "web-uploads")).catch(() => []);
  check("图片暂存目录在发送后已清空", leftovers.length === 0, leftovers.join(","));
  }
  if (process.env.MAGI_GPT_WEB_REAL_QUESTION === "1" || process.env.MAGI_GPT_WEB_ONLY_QUESTION === "1") {
  await sendComposerMessage('问我晚上想吃什么，给出几个选项');
    await waitFor(() => tools.exists('[data-user-question-id]'), "真实 ChatGPT 生成可映射的原生选项卡片", 240_000, 1000);
    check("真账号原生选项映射到 Magi 问题面板", true);
    let answered = 0;
    const seen = new Set();
    for (let stage = 0; stage < 8; stage += 1) {
      const id = await page.evaluate(`document.querySelector('[data-user-question-id]')?.getAttribute('data-user-question-id')`);
      if (id) {
        if (seen.has(id)) throw new Error('真实卡片重复出现，回写未被确认');
        seen.add(id);
        await snap(`real-question-${stage + 1}`);
        await tools.click('[data-user-question-id] .uq-option');
        await tools.click('[data-user-question-id] .dock-btn--primary');
        answered += 1;
      }
      const next = await waitFor(async () => {
        const current = await page.evaluate(`document.querySelector('[data-user-question-id]')?.getAttribute('data-user-question-id')`);
        if (current && current !== id) return 'question';
        if (!current && !(await tools.exists('[data-testid="input-stop-button"]'))) return 'complete';
        return null;
      }, '真实网页作答后继续或完成', 120_000, 1000);
      if (next === 'complete') break;
    }
    check("真账号选项作答回写网页后回合成功结束", answered > 0 && !(await tools.exists('[data-testid="input-stop-button"]')));
  }


  }

  if (questionFixture) {
    // 页面在等待作答时停止：覆盖项目归属、canonical 终态与 Web 槽位结算。
    await sendComposerMessage('停止验收：问我晚餐想吃什么并等待我的回答');
    await waitFor(() => tools.exists('[data-user-question-id]'), "停止前网页问题处于等待回答");
    const runtime = await tools.api('/api/browser/web-models/runtime');
    const owner = JSON.parse(runtime.body).entries.find(entry => entry.active);
    const activeHistory = await tools.api('/api/messages?' + new URLSearchParams({ scope: 'workspace', sessionId: owner.sessionId, workspaceId }));
    const turnId = JSON.parse(activeHistory.body).canonicalTurns.at(-1)?.turnId;
    check("停止前 GPT Web 活跃会话绑定到项目", owner?.workspaceId === workspaceId && Boolean(turnId), runtime.body);
    await tools.click('[data-testid="sidebar-settings"]');
    await waitFor(() => tools.exists('[data-testid="settings-nav-webModel"]'), "停止验收设置入口");
    await tools.click('[data-testid="settings-nav-webModel"]');
    await waitFor(() => page.evaluate(`Boolean(document.querySelector('[data-web-model-stop]:not([disabled])'))`), "设置停止按钮可用");
    await tools.click('[data-web-model-stop]');
    await waitFor(() => tools.exists('.confirm-btn--danger'), "停止活跃推理的确认框");
    await tools.click('.confirm-btn--danger');
    await waitFor(async () => {
      const status = await tools.api('/api/browser/web-models/runtime');
      return status.status === 200 && JSON.parse(status.body).entries.length === 0;
    }, "设置停止释放 Web 槽位", 30_000);
    const history = await tools.api('/api/messages?' + new URLSearchParams({ scope: 'workspace', sessionId: owner.sessionId, workspaceId }));
    const turn = JSON.parse(history.body).canonicalTurns.find(turn => turn.turnId === turnId);
    check("设置停止使项目会话的 canonical Turn 进入 cancelled", turn?.status === 'cancelled', JSON.stringify(turn));
    await tools.click('[data-testid="settings-back"]');
    await waitFor(async () => !(await tools.exists('[data-testid="input-stop-button"]')) && !(await tools.exists('[data-user-question-id]')), "停止后输入恢复且无遗留问题面板");
    check("停止后输入恢复且问题面板清理", true);
    await snap('stopped-workspace-turn');
  }

  // 停用：插件全局生效，一次停用后入口、接口与右栏视图全部消失。
  const offApp = await tools.post(`/api/plugins/${PLUGIN_ID}/deactivate`, { scope: "application" });
  check("应用级停用成功并释放宿主资源", offApp.status === 200, offApp.body.slice(0, 200));
  const afterStop = await tools.api("/api/browser/web-models/runtime");
  check("停用后 GPT Web 接口被拒绝（不靠隐藏入口）", afterStop.status === 403, `${afterStop.status}`);
  await waitFor(async () => !(await tools.exists('[data-tab-id^="webSession"]')), "停用后右栏 GPT Web 视图消失", 20_000);
  check("停用后右栏 GPT Web 视图消失", true);
  }
  void sleep;
  check("验收期间 Renderer 无 canonical 事实冲突或运行时异常", rendererErrors.length === 0, rendererErrors.slice(-4).join('\n'));
} catch (error) {
  failure = error;
  if (lastProbe) process.stderr.write(`\n[最后一次探测] ${lastProbe}\n`);
  if (fixtureWeb) process.stderr.write(`\n[夹具页面] ${JSON.stringify(await fixtureWeb.evaluate(`({url:location.href,title:document.title,text:document.body.innerText.slice(0,800),composer:!!document.querySelector('#prompt-textarea'),events:window.__fixtureEvents})`).catch(() => null))}\n`);
  if (!fixtureWeb && page) {
    const guest = await connectTarget(cdpPort, (target) => target.type === "webview" && /chatgpt\.com/u.test(target.url), "ChatGPT guest").catch(() => null);
    if (guest) {
      const diagnostic = await guest.evaluate(`({url:location.href,title:document.title,composer:[...document.querySelectorAll('[contenteditable=true],textarea')].map(e=>({id:e.id,text:(e.innerText||e.value||'').slice(0,100)})),buttons:[...document.querySelectorAll('form button')].map(b=>({label:b.getAttribute('aria-label'),disabled:b.disabled})),text:document.body.innerText.slice(-2500),dil:[...document.querySelectorAll('[data-dil-message-id]')].map(e=>e.outerHTML.slice(0,8000)),groups:[...document.querySelectorAll('[role=radiogroup], [data-above-composer-portal]')].map(e=>e.outerHTML.slice(0,6000)),iframes:[...document.querySelectorAll('iframe')].map(e=>({src:e.src,title:e.title}))})`).catch(() => null);
      process.stderr.write(`\n[真实网页诊断] ${JSON.stringify(diagnostic)}\n`);
      guest.close();
    }
  }
  if (page) {
    const body = await page.evaluate(`document.body.innerText.slice(0, 1200)`).catch(() => "");
    process.stderr.write(`\n[失败时页面文本]\n${body}\n[Renderer 错误]\n${[...new Set(rendererErrors)].slice(-6).join("\n---\n")}\n`);
    await snap("failure").catch(() => {});
  }
  if (process.env.MAGI_GPT_WEB_LOG) await writeFile(process.env.MAGI_GPT_WEB_LOG, app.log.join(""));
  process.stderr.write(`\n[Electron 日志尾部]\n${app.log.join("").slice(-6000)}\n`);
} finally {
  fixtureWeb?.close();
  page?.close();
  await app.stop();
  provider.closeAllConnections?.();
  provider.close();
  await rm(workRoot, { recursive: true, force: true }).catch(() => {});
}

process.stdout.write(`\n${summary()}\n`);
if (failure) {
  process.stderr.write(`${failure.message}\n`);
  process.exit(1);
}
void REPLY_TOKEN;
