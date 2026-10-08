/**
 * 真实 Electron / Chromium 里验证浏览器 guest 的网络边界（由 verify-electron-browser-network-policy.mjs 启动）。
 *
 * 钩子与 BrowserSurfaceManager.configurePartition 里的 session.webRequest.onBeforeRequest 完全一致，
 * 验证页面内的 iframe、fetch 与重定向无法绕过只检查导航入口的规则，并且 *.localhost 预览来源
 * 能被 Chromium 解析到本机、带着自己的 Host 到达服务器。
 */
const { app, BrowserWindow, session } = require("electron");
const http = require("node:http");
// 由 verify-electron-browser-network-policy.mjs 打包并通过环境变量传入的求值器。
const { evaluateBrowserRequestTarget } = require(process.env.MAGI_NETWORK_POLICY_BUNDLE);

app.dock?.hide();
app.disableHardwareAcceleration();
const results = [];
const record = (name, ok, detail = "") => results.push({ name, ok, detail });

function listen(handler) {
  return new Promise((resolve) => {
    const server = http.createServer(handler);
    server.listen(0, "127.0.0.1", () => resolve({ server, port: server.address().port }));
  });
}

app.whenReady().then(async () => {
  const selfHits = [];
  const self = await listen((req, res) => {
    selfHits.push({ host: req.headers.host, url: req.url });
    res.setHeader("access-control-allow-origin", "*");
    res.end("SELF-SECRET");
  });
  let selfPort = self.port;
  const web = await listen((req, res) => {
    if (req.url === "/redirect-self") {
      res.writeHead(302, { location: `http://127.0.0.1:${selfPort}/api/session/tool-approvals` });
      return res.end();
    }
    if (req.url === "/redirect-metadata") {
      res.writeHead(302, { location: "http://169.254.169.254/latest/meta-data" });
      return res.end();
    }
    if (req.url === "/ok") return res.end("OK");
    res.setHeader("content-type", "text/html");
    res.end(`<!doctype html><title>t</title><iframe id=f src="http://127.0.0.1:${selfPort}/api/frame"></iframe>`);
  });

  const config = { selfPorts: new Set([selfPort]), allowPrivateNetwork: true };
  const ses = session.fromPartition("magi-browser-netpolicy-smoke");
  // 与 BrowserSurfaceManager.configurePartition 里的钩子完全一致。
  ses.webRequest.onBeforeRequest(
    { urls: ["http://*/*", "https://*/*", "ws://*/*", "wss://*/*"] },
    (details, callback) => {
      try {
        const verdict = evaluateBrowserRequestTarget(new URL(details.url), config);
        callback({ cancel: !verdict.allowed });
      } catch {
        callback({ cancel: true });
      }
    },
  );

  const win = new BrowserWindow({ show: false, webPreferences: { session: ses, sandbox: true, contextIsolation: true } });
  const wc = win.webContents;

  // 1) 页面（对照）：同一来源之外的本机端口可以加载，页面里的 iframe 指向自身端口应被拦截。
  await wc.loadURL(`http://127.0.0.1:${web.port}/page`);
  record("普通本机页面可加载（对照）", wc.getURL().includes("/page"));
  await new Promise((r) => setTimeout(r, 400));
  record("iframe 指向 Magi 自身端口被拦截（服务器零命中）", !selfHits.some((hit) => hit.url.startsWith("/api/frame")), JSON.stringify(selfHits));

  // 2) fetch / XHR 指向自身端口
  const fetched = await wc.executeJavaScript(
    `fetch("http://127.0.0.1:${selfPort}/api/x", {mode:"no-cors"}).then(()=> "reached", e => "blocked:"+e.name)`,
  );
  record("fetch 指向自身端口被拦截", fetched.startsWith("blocked"), fetched);
  const control = await wc.executeJavaScript(
    `fetch("http://127.0.0.1:${web.port}/ok").then(r=>r.text(), e => "err:"+e.name)`,
  );
  record("fetch 其他本机端口放行（对照）", control === "OK", control);

  // 3) 重定向到自身端口 / 云元数据
  for (const [name, path] of [["重定向到 Magi 自身端口", "/redirect-self"], ["重定向到云元数据地址", "/redirect-metadata"]]) {
    const failure = await new Promise((resolve) => {
      wc.once("did-fail-load", (_e, code, desc) => resolve(`${code}:${desc}`));
      wc.loadURL(`http://127.0.0.1:${web.port}${path}`).catch(() => {});
    });
    record(`${name}被拦截`, failure.startsWith("-20:"), failure);
  }
  record("重定向后自身端口服务器仍然零命中", !selfHits.some((hit) => hit.url.startsWith("/api/session")), JSON.stringify(selfHits));

  // 4) 预览来源：site.localhost 解析到本机并且被放行
  const events = [];
  const onFail = (_e, code, desc, url) => events.push(`fail:${code}:${desc}:${url}`);
  wc.on("did-fail-load", onFail);
  const before = selfHits.length;
  await wc.loadURL(`http://site.localhost:${selfPort}/api/files/site/t/ws/index.html`).catch((e) => events.push("loadURL-rejected:" + e.message));
  await new Promise((r) => setTimeout(r, 500));
  wc.off("did-fail-load", onFail);
  record("预览来源 site.localhost 无加载失败", events.length === 0, events.join(" | "));
  record("预览请求带着 site.localhost 的 Host 到达服务器", selfHits.slice(before).some((hit) => (hit.host || "").startsWith("site.localhost:")), JSON.stringify(selfHits.slice(before)));
  // 对照：不经过本钩子，系统/Chromium 对 site.localhost 的解析本身是否可用
  const rawSes = session.fromPartition("magi-browser-netpolicy-raw");
  const raw = new BrowserWindow({ show: false, webPreferences: { session: rawSes } });
  const rawEvents = [];
  raw.webContents.on("did-fail-load", (_e, code, desc) => rawEvents.push(`${code}:${desc}`));
  await raw.webContents.loadURL(`http://site.localhost:${selfPort}/raw`).catch((e) => rawEvents.push("rejected:" + e.message));
  record("对照：无任何钩子时 site.localhost 也能解析（Chromium 内置 *.localhost 回环）", rawEvents.length === 0, rawEvents.join(" | "));

  for (const r of results) console.log(`${r.ok ? "PASS" : "FAIL"}  ${r.name}${r.ok ? "" : "  -> " + r.detail}`);
  const failed = results.filter((r) => !r.ok).length;
  console.log(failed === 0 ? "ALL PASSED" : `${failed} FAILED`);
  self.server.close(); web.server.close();
  app.exit(failed === 0 ? 0 : 1);
});
