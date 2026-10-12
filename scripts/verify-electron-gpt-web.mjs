import { createServer } from "node:http";
import { cp, mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import { setTimeout as sleep } from "node:timers/promises";
import { connectAppPage, createReporter, launchApp, pageTools, waitFor } from "./lib/electron-harness.mjs";

/**
 * GPT Web 第一方插件的真实账号验收（打包 Electron）。
 *
 * 需要本机已登录 ChatGPT：脚本只把 `persist:magi-web-model` 登录分区复制到一次性的独立状态根，
 * 不触碰用户正在使用的 ~/.magi，结束后删除副本。验证：未安装时无入口 → 安装并授权第一方插件 →
 * 引擎入口出现 → 临时对话发送一条消息并读回回复 → 停用后入口与槽位释放。
 *
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
const PLUGIN_ID = "openai.chatgpt-web";
const REPLY_TOKEN = "MAGI_PLUGIN_E2E_OK";

const { check, summary } = createReporter();
const rendererErrors = [];
let lastProbe = "";

// MAGI_GPT_WEB_FRESH=1：不复制登录分区，验证全新环境下 surface 能挂载且探测给出“需要登录”。
const fresh = process.env.MAGI_GPT_WEB_FRESH === "1";
if (!fresh && !existsSync(join(sourceUserData, "Partitions", "magi-web-model"))) {
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
if (!fresh) {
  await cp(join(sourceUserData, "Partitions", "magi-web-model"), join(stateRoot, "desktop", "Partitions", "magi-web-model"), { recursive: true });
  await cp(join(sourceUserData, "Local State"), join(stateRoot, "desktop", "Local State")).catch(() => {});
  await cp(join(sourceUserData, "browser-partitions.json"), join(stateRoot, "desktop", "browser-partitions.json")).catch(() => {});
}

const app = launchApp({
  executable: appExecutable,
  cwd: repositoryRoot,
  cdpPort,
  env: {
    MAGI_DESKTOP_DAEMON_PORT: String(daemonPort),
    MAGI_STATE_ROOT: stateRoot,
    MAGI_OPEN_BROWSER: "0",
    MAGI_OPENAI_COMPAT_BASE_URL: `http://127.0.0.1:${providerPort}/v1`,
    MAGI_OPENAI_COMPAT_API_KEY: "gpt-web-e2e-key",
    MAGI_OPENAI_COMPAT_MODEL: "gpt-web-e2e-model",
  },
});

let page = null;
let tools = null;
let failure = null;
let shot = 0;
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
  await waitFor(() => tools.exists('[data-web-model-mode-option="temporary"]'), "对话方式选择出现", 10_000);
  await tools.click('[data-web-model-mode-option="temporary"]');
  await sleep(500);
  check("会话选择临时对话方式", (await page.evaluate(`document.querySelector('[data-web-model-mode]')?.getAttribute('data-web-model-mode')`)) === "temporary");
  await page.evaluate(`document.querySelector('.ia-reasoning-title, .ia-model-wrap')?.blur?.()`);
  await page.evaluate(`document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))`);
  await sleep(400);

  // 发送一条真实消息：用户原文原样进入网页对话，回复读回 Magi
  await page.evaluate(`(() => {
    const el = document.querySelector('[data-testid="input-textarea"]');
    el.focus(); el.textContent = '请计算 12+30，只回复 ANS= 加上结果，不要其他文字。';
    el.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertText', data: el.textContent }));
  })()`);
  await waitFor(async () => !(await page.evaluate(`document.querySelector('[data-testid="input-send-button"]')?.disabled ?? true`)), "发送按钮可用", 10_000);
  await tools.click('[data-testid="input-send-button"]');
  await waitFor(async () => (await page.evaluate(`document.body.innerText`)).includes("ANS=42"), "GPT Web 回复读回 Magi（ANS=42）", 150_000, 1_500);
  check("真实 ChatGPT 网页回复读回 Magi 对话", true);
  await snap("reply");

  // 停用：插件全局生效，一次停用后入口、接口与右栏视图全部消失。
  const offApp = await tools.post(`/api/plugins/${PLUGIN_ID}/deactivate`, { scope: "application" });
  check("应用级停用成功并释放宿主资源", offApp.status === 200, offApp.body.slice(0, 200));
  const afterStop = await tools.api("/api/browser/web-models/runtime");
  check("停用后 GPT Web 接口被拒绝（不靠隐藏入口）", afterStop.status === 403, `${afterStop.status}`);
  await waitFor(async () => !(await tools.exists('[data-tab-id^="webSession"]')), "停用后右栏 GPT Web 视图消失", 20_000);
  check("停用后右栏 GPT Web 视图消失", true);
  }
  void sleep;
} catch (error) {
  failure = error;
  if (lastProbe) process.stderr.write(`\n[最后一次探测] ${lastProbe}\n`);
  if (page) {
    const body = await page.evaluate(`document.body.innerText.slice(0, 1200)`).catch(() => "");
    process.stderr.write(`\n[失败时页面文本]\n${body}\n[Renderer 错误]\n${[...new Set(rendererErrors)].slice(-6).join("\n---\n")}\n`);
    await snap("failure").catch(() => {});
  }
  if (process.env.MAGI_GPT_WEB_LOG) await writeFile(process.env.MAGI_GPT_WEB_LOG, app.log.join(""));
  process.stderr.write(`\n[Electron 日志尾部]\n${app.log.join("").slice(-6000)}\n`);
} finally {
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
