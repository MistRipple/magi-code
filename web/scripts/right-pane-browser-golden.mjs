import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

globalThis.$state = (value) => value;
globalThis.$derived = (value) => (typeof value === 'function' ? value() : value);
globalThis.$derived.by = (fn) => fn();
globalThis.$effect = (fn) => {
  try {
    fn();
  } catch {
    // 金标只验证右栏浏览器创建入口的行为，不重放组件 effect 的订阅副作用。
  }
};

class MemoryStorage {
  constructor() {
    this.values = new Map();
  }

  getItem(key) {
    return this.values.get(String(key)) ?? null;
  }

  setItem(key, value) {
    this.values.set(String(key), String(value));
  }

  removeItem(key) {
    this.values.delete(String(key));
  }
}

const storage = new MemoryStorage();

/** 与 session-navigation-golden 一致的浏览器窗口替身。 */
function webWindow(overrides = {}) {
  return {
    localStorage: storage,
    sessionStorage: new MemoryStorage(),
    setTimeout,
    clearTimeout,
    setInterval,
    clearInterval,
    addEventListener() {},
    removeEventListener() {},
    dispatchEvent() {
      return true;
    },
    location: { search: '', href: 'http://127.0.0.1:38123/web.html' },
    ...overrides,
  };
}

const originalFetch = globalThis.fetch;
const originalWindow = globalThis.window;

function jsonResponse(payload) {
  return new Response(JSON.stringify(payload), {
    status: 200,
    headers: { 'content-type': 'application/json' },
  });
}

// 每次创建的 Browser Session / Tab 必须拿到互不相同的身份，否则同 id
// 复用会掩盖「重复点击是否新建」这件事。序列跨桩调用递增，与真实 daemon
// 的单调 id 生成一致。
let daemonSequence = 0;

/** 用最小 daemon 桩记录浏览器资源的创建序列。 */
function installDaemonStub(overrides = {}) {
  const calls = [];
  globalThis.fetch = async (url, init = {}) => {
    const requestUrl = new URL(String(url));
    const path = requestUrl.pathname;
    const method = init.method ?? 'GET';
    calls.push({ method, path, url: requestUrl, body: init.body ? JSON.parse(String(init.body)) : null });
    if (overrides.handle) {
      const override = overrides.handle({ method, path, url: requestUrl, init });
      if (override) return override;
    }
    if (path === '/api/browser/capabilities') {
      return jsonResponse(overrides.capabilities ?? {
        revision: 1,
        inAppBrowserEnabled: true,
        browserUseEnabled: true,
        devtoolsEnabled: false,
        lanAccessEnabled: false,
        hostStatus: 'ready',
        hostProtocolCompatible: true,
        hostState: 'ready',
        lastErrorCode: null,
        platformCapabilities: {
          desktopBrowserSurface: true,
          browserRecords: true,
          browserAnnotations: true,
          browserRemoteSurface: true,
        },
      });
    }
    if (path === '/api/browser/sessions' && method === 'POST') {
      daemonSequence += 1;
      return jsonResponse({
        browserSessionId: `browser-session-golden-${daemonSequence}`,
        workspaceId: null,
        sessionId: 'session-golden',
        revision: 1,
        activeTabId: null,
        agentOccupied: false,
        tabs: [],
      });
    }
    if (path.endsWith('/tabs') && method === 'POST') {
      daemonSequence += 1;
      return jsonResponse({
        tabId: `browser-tab-golden-${daemonSequence}`,
        browserSessionId: 'browser-session-golden',
        lifecycle: 'creating',
        url: JSON.parse(String(init.body)).initialUrl,
        navigationRevision: 1,
        title: '',
      });
    }
    throw new Error(`unexpected daemon request: ${method} ${path}`);
  };
  return calls;
}

await withGoldenViteServer(async (server) => {
  // session-navigation 需要真实 bridge 才能派发导航消息；这里复用
  // session-navigation-golden 的装配方式，让物化会话那条路径可以收敛。
  const bridgeRuntime = await server.ssrLoadModule('/src/shared/bridges/bridge-runtime.ts');
  const rightPane = await server.ssrLoadModule('/src/stores/right-pane.svelte.ts');
  const browser = await server.ssrLoadModule('/src/web/right-pane-browser.ts');

  globalThis.window = webWindow();
  globalThis.localStorage = storage;
  const sessionNavigation = await server.ssrLoadModule('/src/shared/session-navigation.svelte.ts');
  let postedMessages = [];
  bridgeRuntime.setClientBridge({
    kind: 'web',
    postMessage(message) {
      postedMessages.push(message);
      // 主 Renderer 的导航确认：立刻按请求目标提交，模拟真实链路。
      if (message?.type === 'navigateSession' && message.requestId) {
        sessionNavigation.settleSessionNavigation(message.requestId, {
          kind: message.target,
          scope: message.scope,
          ...(message.workspaceId ? { workspaceId: message.workspaceId } : {}),
          ...(message.workspacePath ? { workspacePath: message.workspacePath } : {}),
          ...(message.sessionId ? { sessionId: message.sessionId } : {}),
        });
      }
    },
    onMessage() {
      return () => {};
    },
    getState() {
      return undefined;
    },
    setState() {},
    getInitialSessionId() {
      return '';
    },
    getInitialLocale() {
      return '';
    },
    notifyReady() {},
  });

  // 1. 非 Desktop：没有物理 BrowserSurface，链接必须交给系统浏览器。
  globalThis.window = webWindow();
  rightPane.activateRightPaneSession('workspace-golden', 'session-golden');
  assert.equal(
    await browser.openUrlInRightPaneBrowser('https://example.com/a'),
    'fallback-external',
    'Web / 手机端必须退回外部浏览器',
  );
  assert.equal(
    await browser.openHtmlFileInRightPaneBrowser({
      filepath: '/tmp/index.html',
      workspaceId: 'workspace-golden',
      workspacePath: '/tmp',
      sessionId: 'session-golden',
    }),
    'rejected',
    '非 Desktop 不能创建 HTML 预览页',
  );

  // 2. Desktop：链接在右栏创建顶级浏览器 Tab，并挂到当前 Magi 会话。
  globalThis.window = webWindow({
    location: { search: '?desktopSurface=app', href: 'http://127.0.0.1:38123/web.html?desktopSurface=app' },
    magiDesktop: { runtime: 'electron', surface: 'app' },
  });
  postedMessages = [];
  rightPane.activateRightPaneSession('workspace-golden', 'session-golden');
  let calls = installDaemonStub();
  assert.equal(
    await browser.openUrlInRightPaneBrowser('https://example.com/a'),
    'opened',
  );
  const pane = rightPane.getRightPaneState('workspace-golden\u0000session-golden');
  assert.equal(pane.openTabs.length, 1);
  assert.equal(pane.openTabs[0].kind, 'browser');
  assert.equal(pane.openTabs[0].payload.url, 'https://example.com/a');
  assert.equal(pane.openTabs[0].payload.workspaceId, 'workspace-golden');
  assert.equal(pane.openTabs[0].payload.sessionId, 'session-golden');
  assert.equal(pane.activeTabId, pane.openTabs[0].id);
  assert.equal(pane.collapsed, false, '点击链接必须展开右栏，而不是丢弃');
  assert.ok(calls.some((call) => call.path === '/api/browser/sessions'));
  assert.ok(calls.some((call) => call.path.endsWith('/tabs')));

  // 3. 同一 URL 再点一次：复用已有 Tab，不新建 Browser Session。
  calls = installDaemonStub();
  assert.equal(
    await browser.openUrlInRightPaneBrowser('https://example.com/a'),
    'opened',
  );
  assert.equal(
    rightPane.getRightPaneState('workspace-golden\u0000session-golden').openTabs.length,
    1,
    '同一 URL 必须复用已有 Tab',
  );
  assert.equal(calls.length, 1, '复用路径只查询能力，不再创建浏览器资源');

  // 4. 不同 URL：在同一会话里新建第二个顶级 Tab。
  calls = installDaemonStub();
  assert.equal(
    await browser.openUrlInRightPaneBrowser('https://example.com/b'),
    'opened',
  );
  assert.equal(
    rightPane.getRightPaneState('workspace-golden\u0000session-golden').openTabs.length,
    2,
  );

  // 5. 空白页是「再开一个浏览器」的显式意图，不参与同 URL 复用。
  rightPane.openBrowserTab('browser-session-blank', 'browser-tab-blank', {
    workspaceId: 'workspace-golden',
    sessionId: 'session-golden',
    label: 'about:blank',
    lifecycle: 'ready',
    url: 'about:blank',
    navigationRevision: 1,
  });
  calls = installDaemonStub();
  assert.equal(
    await browser.openUrlInRightPaneBrowser('about:blank'),
    'opened',
  );
  const blankTabs = rightPane
    .getRightPaneState('workspace-golden\u0000session-golden')
    .openTabs.filter((tab) => tab.payload.url === 'about:blank');
  assert.equal(blankTabs.length, 2, '空白页允许并存多个');

  // 6. 内置浏览器被设置关闭时：不退回系统浏览器，明确给出原因。
  calls = installDaemonStub({
    capabilities: {
      revision: 2,
      inAppBrowserEnabled: false,
      browserUseEnabled: true,
      devtoolsEnabled: false,
      lanAccessEnabled: false,
      hostStatus: 'ready',
      hostProtocolCompatible: true,
      hostState: 'ready',
      lastErrorCode: null,
      platformCapabilities: {
        desktopBrowserSurface: true,
        browserRecords: true,
        browserAnnotations: true,
        browserRemoteSurface: true,
      },
    },
  });
  assert.equal(
    await browser.openUrlInRightPaneBrowser('https://example.com/disabled'),
    'rejected',
  );

  // 7. 会话尚不存在时：先物化会话，再创建浏览器资源。
  rightPane.activateRightPaneSession('', '');
  calls = installDaemonStub({
    handle: ({ method, path, init, url }) => {
      if (path === '/api/session/materialize' && method === 'POST') {
        return jsonResponse({ sessionId: 'session-materialized', workspaceId: null });
      }
      if (path === '/api/browser/sessions' && method === 'POST') {
        return jsonResponse({
          browserSessionId: 'browser-session-materialized',
          workspaceId: null,
          sessionId: 'session-materialized',
          revision: 1,
          activeTabId: null,
          agentOccupied: false,
          tabs: [],
        });
      }
      if (path.endsWith('/tabs') && method === 'POST') {
        return jsonResponse({
          tabId: 'browser-tab-materialized',
          browserSessionId: 'browser-session-materialized',
          lifecycle: 'creating',
          url: JSON.parse(String(init.body)).initialUrl,
          navigationRevision: 1,
          title: '',
        });
      }
      void url;
      return null;
    },
  });
  assert.equal(
    await browser.openUrlInRightPaneBrowser('https://example.com/materialized'),
    'opened',
    '没有真实会话时必须先物化会话再创建浏览器资源',
  );
  assert.ok(
    calls.some((call) => call.path === '/api/session/materialize'),
    '必须先物化会话',
  );
  assert.ok(
    calls.some((call) => call.path === '/api/browser/sessions'),
    '物化后才能创建浏览器会话',
  );

  // 8. 工作区草稿（有 workspace、尚无会话）：物化后导航必须带 workspacePath，
  //    浏览器会话按工作区作用域创建。这是 Shell 以权威绑定传入的场景。
  const materializeHandlers = (sessionId, workspaceId) => ({
    handle: ({ method, path, init }) => {
      if (path === '/api/session/materialize' && method === 'POST') {
        return jsonResponse({ sessionId, workspaceId });
      }
      if (path === '/api/browser/sessions' && method === 'POST') {
        return jsonResponse({
          browserSessionId: `browser-session-${sessionId}`,
          workspaceId,
          sessionId,
          revision: 1,
          activeTabId: null,
          agentOccupied: false,
          tabs: [],
        });
      }
      if (path.endsWith('/tabs') && method === 'POST') {
        return jsonResponse({
          tabId: `browser-tab-${sessionId}`,
          browserSessionId: `browser-session-${sessionId}`,
          lifecycle: 'creating',
          url: JSON.parse(String(init.body)).initialUrl,
          navigationRevision: 1,
          title: '',
        });
      }
      return null;
    },
  });
  rightPane.activateRightPaneSession('workspace-draft', '');
  postedMessages = [];
  calls = installDaemonStub(materializeHandlers('session-ws-materialized', 'workspace-draft'));
  assert.equal(
    await browser.openUrlInRightPaneBrowser('https://example.com/ws-draft', {
      workspaceId: 'workspace-draft',
      workspacePath: '/tmp/workspace-draft',
      sessionId: '',
    }),
    'opened',
    '工作区草稿里点链接必须先物化会话再创建浏览器资源',
  );
  const workspaceNavigation = postedMessages.find((message) => message.type === 'navigateSession');
  assert.equal(workspaceNavigation?.scope, 'workspace');
  assert.equal(workspaceNavigation?.workspacePath, '/tmp/workspace-draft', '导航必须带 workspacePath');
  const workspaceBrowserSession = calls.find((call) => call.path === '/api/browser/sessions');
  assert.equal(workspaceBrowserSession?.body.scope, 'workspace');
  assert.equal(workspaceBrowserSession?.body.workspaceId, 'workspace-draft');
  assert.equal(workspaceBrowserSession?.body.workspacePath, '/tmp/workspace-draft');

  // 9. 工作区绑定缺 workspacePath：不能静默走到导航失败，也不能创建资源。
  rightPane.activateRightPaneSession('workspace-draft', '');
  postedMessages = [];
  calls = installDaemonStub(materializeHandlers('session-never', 'workspace-draft'));
  assert.equal(
    await browser.openUrlInRightPaneBrowser('https://example.com/no-path', {
      workspaceId: 'workspace-draft',
      workspacePath: '',
      sessionId: '',
    }),
    'rejected',
  );
  assert.equal(
    calls.some((call) => call.path === '/api/session/materialize' || call.path === '/api/browser/sessions'),
    false,
    '缺 workspacePath 时不应物化会话或创建浏览器资源',
  );

  // 10. 调用方显式传空（个人草稿）是权威的：不能回退到右栏残留的旧工作区作用域。
  rightPane.activateRightPaneSession('stale-workspace', 'stale-session');
  postedMessages = [];
  calls = installDaemonStub(materializeHandlers('session-personal-materialized', null));
  assert.equal(
    await browser.openUrlInRightPaneBrowser('https://example.com/personal', {
      workspaceId: '',
      workspacePath: '',
      sessionId: '',
    }),
    'opened',
  );
  const personalMaterialize = calls.find((call) => call.path === '/api/session/materialize');
  assert.equal(personalMaterialize?.body.scope, 'personal', '显式个人绑定必须按个人作用域物化');
  assert.equal(personalMaterialize?.body.workspaceId, null, '不得继承右栏残留的工作区');
  const personalBrowserSession = calls.find((call) => call.path === '/api/browser/sessions');
  assert.equal(personalBrowserSession?.body.scope, 'personal');

  // 11. GPT Web（应用级）视图处于激活态时，点链接新建的 Tab 必须成为内容槽当前项，
  //     而不是只进 Tab 条。
  rightPane.activateRightPaneSession('workspace-golden', 'session-golden');
  const gptHome = {
    tabId: 'browser-tab-web-model-home',
    lifecycle: 'ready',
    url: 'https://chatgpt.com/',
    navigationRevision: 1,
  };
  rightPane.openWebModelTab('browser-session-app-golden', gptHome);
  assert.equal(rightPane.rightPaneState.activeAppTabId, 'webSession:app');
  calls = installDaemonStub();
  assert.equal(
    await browser.openUrlInRightPaneBrowser('https://example.com/while-gpt-web', {
      workspaceId: 'workspace-golden',
      workspacePath: '/tmp',
      sessionId: 'session-golden',
    }),
    'opened',
  );
  assert.equal(
    rightPane.rightPaneState.activeAppTabId,
    null,
    'GPT Web 视图激活时点链接，必须离开应用级视图，新 Tab 才会显示',
  );
  const gptPane = rightPane.getRightPaneState('workspace-golden\u0000session-golden');
  assert.ok(gptPane.activeTabId?.startsWith('browser:'), '新建的浏览器 Tab 成为会话级当前项');
  assert.equal(
    rightPane.rightPaneState.appTabs.length,
    1,
    '离开应用级视图不销毁它：GPT Web 宿主仍挂载，后台推理不中断',
  );

  for (const binding of [
    { workspaceId: '', workspacePath: '', sessionId: 'personal-html' },
    { workspaceId: 'workspace-golden', workspacePath: '/tmp', sessionId: 'project-html' },
  ]) {
    calls = installDaemonStub();
    assert.equal(await browser.openHtmlFileInRightPaneBrowser({ filepath: 'index.html', ...binding }), 'opened');
    const created = calls.find(call => call.path.endsWith('/tabs'));
    const previewUrl = new URL(created.body.initialUrl);
    assert.equal(previewUrl.pathname, '/api/files/site-open');
    assert.equal(previewUrl.searchParams.get('sessionId'), binding.sessionId, 'HTML 必须携带来源会话，隔离副本和个人目录均按会话解析');
    assert.equal(previewUrl.searchParams.get('workspaceId') || '', binding.workspaceId);
  }

  globalThis.fetch = originalFetch;
  globalThis.window = originalWindow;
  console.log('right pane browser golden replay passed');
});
