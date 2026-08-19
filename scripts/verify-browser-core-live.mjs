import assert from "node:assert/strict";

const baseUrl = (process.env.MAGI_ACCEPTANCE_BASE_URL || "http://127.0.0.1:38123").replace(/\/$/u, "");
const args = new Map();
for (let index = 2; index < process.argv.length; index += 1) {
  const value = process.argv[index];
  if (!value.startsWith("--")) continue;
  const key = value.slice(2);
  const next = process.argv[index + 1];
  if (next && !next.startsWith("--")) {
    args.set(key, next);
    index += 1;
  } else {
    args.set(key, true);
  }
}

const sessionId = String(args.get("session-id") || process.env.MAGI_ACCEPTANCE_SESSION_ID || "").trim();
const requestedTabId = String(args.get("tab-id") || process.env.MAGI_ACCEPTANCE_TAB_ID || "").trim();
const writeAnnotation = args.has("write-annotation");
const lifecycleRegression = args.has("lifecycle-regression");
const checks = [];
const failures = [];

function record(name, passed, detail = "") {
  const line = `${passed ? "通过" : "失败"} ${name}${detail ? `: ${detail}` : ""}`;
  checks.push(line);
  if (!passed) failures.push(line);
}

async function readResponse(path, init = {}) {
  const response = await fetch(`${baseUrl}${path}`, {
    ...init,
    headers: {
      ...(init.headers || {}),
      "content-type": "application/json",
    },
  });
  const bytes = Buffer.from(await response.arrayBuffer());
  return { response, bytes };
}

async function readJson(path, init = {}) {
  const { response, bytes } = await readResponse(path, init);
  let body = null;
  try {
    body = JSON.parse(bytes.toString("utf8"));
  } catch {
    body = null;
  }
  return { response, body, bytes };
}

function pngDimensions(bytes) {
  assert.equal(bytes.subarray(0, 8).toString("hex"), "89504e470d0a1a0a", "响应不是 PNG");
  return {
    width: bytes.readUInt32BE(16),
    height: bytes.readUInt32BE(20),
  };
}

function jsonBody(value) {
  return JSON.stringify(value);
}

function sessionFromResponse(body) {
  return body?.session ?? body;
}

function tabFromResponse(body, tabId) {
  return sessionFromResponse(body)?.tabs?.find((candidate) => candidate.tabId === tabId) ?? null;
}

async function activateAndReadTab(tabId) {
  const activated = await readJson(`/api/browser/tabs/${encodeURIComponent(tabId)}/activate`, {
    method: "POST",
    body: jsonBody({}),
  });
  record(`真实浏览器 Tab ${tabId} 激活`, activated.response.ok, `HTTP ${activated.response.status}`);
  return {
    response: activated.response,
    session: sessionFromResponse(activated.body),
    tab: tabFromResponse(activated.body, tabId),
  };
}

if (!sessionId) {
  throw new Error("必须提供 --session-id，示例：--session-id session-...");
}

const health = await readJson("/health");
record("daemon health", health.response.ok, `HTTP ${health.response.status}`);

const connection = await readJson("/api/browser/desktop/connection");
const connectionReady = connection.response.ok
  && connection.body?.hostStatus === "ready"
  && connection.body?.hostProtocolCompatible === true;
record(
  "真实 Electron 浏览器 Host 连接",
  connectionReady,
  `hostStatus=${connection.body?.hostStatus ?? "unknown"}, protocol=${String(connection.body?.hostProtocolCompatible)}`,
);

const sessionPath = `/api/browser/sessions/current?scope=personal&sessionId=${encodeURIComponent(sessionId)}`;
const current = await readJson(sessionPath);
let session = current.body?.session;
record("浏览器会话可读取", current.response.ok && Boolean(session), `HTTP ${current.response.status}`);

let tab = null;
if (session) {
  tab = requestedTabId
    ? session.tabs.find((candidate) => candidate.tabId === requestedTabId)
    : session.tabs.find((candidate) => candidate.lifecycle === "ready" && candidate.surfaceId);
}
if (tab && (!tab.surfaceId || tab.lifecycle !== "ready")) {
  const activated = await readJson(`/api/browser/tabs/${encodeURIComponent(tab.tabId)}/activate`, {
    method: "POST",
    body: jsonBody({}),
  });
  record("真实浏览器 Tab 可重新激活", activated.response.ok, `HTTP ${activated.response.status}`);
  const refreshed = await readJson(sessionPath);
  session = refreshed.body?.session;
  tab = session?.tabs.find((candidate) => candidate.tabId === tab.tabId) ?? tab;
}
record(
  "当前 Tab 具有真实 Surface",
  Boolean(tab?.surfaceId),
  tab ? `${tab.tabId} surface=${tab.surfaceId ?? "none"}` : "未找到 ready Tab",
);
record(
  "运行态响应式视口不进入会话持久化",
  Boolean(tab) && !Object.prototype.hasOwnProperty.call(tab, "viewport"),
  tab ? "会话 Tab 响应未包含 viewport 字段" : "未找到 Tab",
);

let temporaryLifecycleTabId = null;
if (lifecycleRegression) {
  try {
    if (!connectionReady || !session) {
      record("浏览器 Surface 生命周期回归前置条件", false, "需要 ready Host 和可读取的浏览器会话");
    } else {
      let firstTabId = tab?.tabId || requestedTabId || "";
      if (!firstTabId) {
        const candidate = session.tabs.find((item) => item.lifecycle !== "closed");
        firstTabId = candidate?.tabId || "";
      }
      let secondTab = session.tabs.find(
        (candidate) => candidate.lifecycle !== "closed" && candidate.tabId !== firstTabId,
      );
      if (!secondTab) {
        const created = await readJson(
          `/api/browser/sessions/${encodeURIComponent(session.browserSessionId)}/tabs`,
          {
            method: "POST",
            body: jsonBody({ initialUrl: "about:blank", clientPlatform: "desktop" }),
          },
        );
        temporaryLifecycleTabId = created.body?.tabId || null;
        record(
          "生命周期回归创建第二个 Browser Tab",
          created.response.status === 201 && Boolean(temporaryLifecycleTabId),
          `HTTP ${created.response.status}`,
        );
        if (temporaryLifecycleTabId) {
          secondTab = { tabId: temporaryLifecycleTabId };
        }
      }

      const beforeConnection = await readJson("/api/browser/desktop/connection");
      const first = firstTabId ? await activateAndReadTab(firstTabId) : null;
      const second = secondTab?.tabId ? await activateAndReadTab(secondTab.tabId) : null;
      const firstAgain = firstTabId ? await activateAndReadTab(firstTabId) : null;
      const afterConnection = await readJson("/api/browser/desktop/connection");

      const firstSurfaceId = first?.tab?.surfaceId || null;
      const firstAgainSurfaceId = firstAgain?.tab?.surfaceId || null;
      record(
        "同一 Tab 重复激活复用 Surface",
        Boolean(firstSurfaceId) && firstSurfaceId === firstAgainSurfaceId,
        `first=${firstSurfaceId || "none"}, repeated=${firstAgainSurfaceId || "none"}`,
      );
      record(
        "不同 Tab 激活不重复建立 Desktop Host 连接",
        beforeConnection.response.ok
          && afterConnection.response.ok
          && Number.isSafeInteger(beforeConnection.body?.desktopConnectionGeneration)
          && beforeConnection.body.desktopConnectionGeneration === afterConnection.body?.desktopConnectionGeneration,
        `before=${beforeConnection.body?.desktopConnectionGeneration ?? "unknown"}, after=${afterConnection.body?.desktopConnectionGeneration ?? "unknown"}`,
      );
      record(
        "第二个 Tab 激活返回独立 Surface",
        Boolean(second?.tab?.surfaceId) && second.tab.surfaceId !== firstSurfaceId,
        `first=${firstSurfaceId || "none"}, second=${second?.tab?.surfaceId || "none"}`,
      );
    }
  } finally {
    if (temporaryLifecycleTabId) {
      const closed = await readJson(`/api/browser/tabs/${encodeURIComponent(temporaryLifecycleTabId)}`, {
        method: "DELETE",
        body: jsonBody({}),
      });
      record("生命周期回归清理临时 Browser Tab", closed.response.ok, `HTTP ${closed.response.status}`);
      temporaryLifecycleTabId = null;
    }
  }
}

let fullDimensions = null;
if (!lifecycleRegression && tab?.tabId && connectionReady) {
  const full = await readResponse(`/api/browser/tabs/${encodeURIComponent(tab.tabId)}/screenshot`, {
    method: "POST",
    body: jsonBody({ fullPage: false, clientPlatform: "desktop" }),
  });
  try {
    fullDimensions = pngDimensions(full.bytes);
    record("浏览器截图工具调用", full.response.ok && full.response.headers.get("content-type") === "image/png", `${fullDimensions.width}x${fullDimensions.height}`);
  } catch (error) {
    record("浏览器截图工具调用", false, `HTTP ${full.response.status}: ${error.message}`);
  }
}

let createdAnnotation = null;
try {
  if (!lifecycleRegression && writeAnnotation && tab?.tabId && connectionReady) {
    const rect = { x: 0.1, y: 0.1, width: 0.2, height: 0.2 };
    const created = await readJson(`/api/browser/tabs/${encodeURIComponent(tab.tabId)}/annotations`, {
      method: "POST",
      body: jsonBody({
        selection: {
          kind: "region",
          navigationRevision: tab.navigationRevision,
          rect,
        },
        comment: `browser-core-live-${Date.now()}`,
      }),
    });
    createdAnnotation = created.body;
    record("标记创建并写入权威状态", created.response.status === 201 && Boolean(createdAnnotation?.annotationId), `HTTP ${created.response.status}`);

    const listed = await readJson(`/api/browser/tabs/${encodeURIComponent(tab.tabId)}/annotations`);
    const listedAnnotation = listed.body?.find((candidate) => candidate.annotationId === createdAnnotation?.annotationId);
    record("标记持久化后可重新读取", listed.response.ok && listedAnnotation?.comment === createdAnnotation?.comment, createdAnnotation?.annotationId || "无标记 ID");

    const artifact = await readResponse(`/api/browser/annotations/${encodeURIComponent(createdAnnotation.annotationId)}/artifact?sessionId=${encodeURIComponent(sessionId)}`);
    let artifactDimensions = null;
    try {
      artifactDimensions = pngDimensions(artifact.bytes);
      record("标记截图 artifact 可读取", artifact.response.ok && artifact.response.headers.get("content-type") === "image/png", `${artifactDimensions.width}x${artifactDimensions.height}`);
    } catch (error) {
      record("标记截图 artifact 可读取", false, `HTTP ${artifact.response.status}: ${error.message}`);
    }

    if (fullDimensions && artifactDimensions) {
      const cropped = artifactDimensions.width < fullDimensions.width
        && artifactDimensions.height < fullDimensions.height;
      record(
        "标记截图按选区裁剪而非整页",
        cropped,
        `full=${fullDimensions.width}x${fullDimensions.height}, artifact=${artifactDimensions.width}x${artifactDimensions.height}, ratio=${(artifactDimensions.width / fullDimensions.width).toFixed(3)}x${(artifactDimensions.height / fullDimensions.height).toFixed(3)}`,
      );
    }

    const updated = await readJson(`/api/browser/annotations/${encodeURIComponent(createdAnnotation.annotationId)}`, {
      method: "POST",
      body: jsonBody({ comment: `${createdAnnotation.comment}-updated` }),
    });
    record("标记备注可编辑并持久化", updated.response.ok && updated.body?.comment.endsWith("-updated"), `HTTP ${updated.response.status}`);
  } else if (!lifecycleRegression) {
    record("标记持久化/裁剪 live 检查", false, "需要 --write-annotation 且当前 Tab 必须有真实 Surface");
  }
} finally {
  if (createdAnnotation?.annotationId) {
    await readJson(`/api/browser/annotations/${encodeURIComponent(createdAnnotation.annotationId)}/status`, {
      method: "POST",
      body: jsonBody({ status: "deleted" }),
    });
  }
}

for (const check of checks) process.stdout.write(`${check}\n`);
if (failures.length) {
  process.stderr.write(`\n浏览器核心 live 验收失败 ${failures.length} 项。\n`);
  process.exitCode = 1;
} else {
  process.stdout.write("浏览器核心 live 验收通过。\n");
}
