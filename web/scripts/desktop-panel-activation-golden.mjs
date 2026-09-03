import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

await withGoldenViteServer(async (server) => {
  const activation = await server.ssrLoadModule('/src/web/desktop-panel-activation.ts');

  const browserA = {
    scopeKey: 'workspace\u0000session',
    kind: 'browser',
    tabId: 'browser-a',
    browserSessionId: 'browser-session',
  };
  const browserB = {
    ...browserA,
    tabId: 'browser-b',
  };
  const terminal = {
    scopeKey: 'workspace\u0000session',
    kind: 'terminal',
    tabId: 'terminal-a',
  };
  const code = {
    scopeKey: 'workspace\u0000session',
    kind: 'code',
    tabId: 'code-a',
  };
  const empty = {
    scopeKey: 'workspace\u0000session',
    kind: null,
    tabId: null,
  };
  const noPanel = {
    layout: {
      activePanelKind: null,
      activeTabId: null,
      activeSurfaceId: null,
    },
  };
  const activeBrowserA = {
    layout: {
      activePanelKind: 'browser',
      activeTabId: 'browser-a',
      activeSurfaceId: 'surface-a',
      rendererGeometry: {
        rightPaneBounds: { x: 800, y: 0, width: 480, height: 900 },
        browserContentSlot: {
          tabId: 'browser-a',
          bounds: { x: 800, y: 40, width: 480, height: 860 },
        },
      },
    },
  };
  const activeTerminal = {
    layout: {
      activePanelKind: 'terminal',
      activeTabId: 'terminal-a',
      activeSurfaceId: null,
    },
  };
  const activeCode = {
    layout: {
      activePanelKind: 'code',
      activeTabId: 'code-a',
      activeSurfaceId: null,
    },
  };

  assert.equal(
    activation.decideDesktopPanelActivation(noPanel, browserA, false),
    'dispatch',
    '首次打开 Browser Tab 必须派发一次 Main 激活请求',
  );
  assert.equal(
    activation.decideDesktopPanelActivation(noPanel, browserA, true),
    'wait_for_in_flight',
    'Browser 激活在途时不得并发第二条 IPC',
  );
  assert.equal(
    activation.decideDesktopPanelActivation(activeBrowserA, browserA, true),
    'acknowledged',
    'Main 快照已确认 Browser Surface 和当前内容槽时不得重复物化',
  );
  assert.equal(
    activation.decideDesktopPanelActivation({
      layout: { ...activeBrowserA.layout, rendererGeometry: null },
    }, browserA, false, true),
    'wait_for_geometry',
    'Surface 已物化但 Renderer 内容槽尚未确认时只能等待真实几何事件',
  );
  assert.equal(
    activation.decideDesktopPanelActivation(activeBrowserA, browserB, false),
    'dispatch',
    '旧 Browser Tab 快照不能确认新 Browser Tab',
  );

  // Browser A 请求仍在 Main 中时，用户连续切 Terminal -> Code。旧快照只能
  // 收口旧请求；下一次派发必须以最新 Code Tab 为准，不能留下旧 Surface。
  assert.equal(
    activation.decideDesktopPanelActivation(activeBrowserA, terminal, true),
    'wait_for_in_flight',
    'Browser -> Terminal 切换等待旧请求收口',
  );
  assert.equal(
    activation.decideDesktopPanelActivation(activeBrowserA, code, false),
    'dispatch',
    '旧 Browser 响应收口后必须派发最新 Code，而不是回退 Terminal 或 Browser',
  );
  assert.equal(
    activation.decideDesktopPanelActivation(activeCode, code, false),
    'acknowledged',
    'Code 确认时 Browser Surface 必须已清空',
  );
  assert.equal(
    activation.decideDesktopPanelActivation(activeTerminal, terminal, false),
    'acknowledged',
    'Terminal 确认时 Browser Surface 必须已清空',
  );
  assert.equal(
    activation.decideDesktopPanelActivation(noPanel, empty, false),
    'acknowledged',
    '关闭最后一个 Tab 后空面板必须由 Main 明确确认',
  );
  assert.equal(
    activation.desktopPanelTargetAcknowledged({
      layout: { ...activeBrowserA.layout, activeSurfaceId: null },
    }, browserA),
    false,
    'Browser 没有真实 Surface 时不能被误判为已激活',
  );
  assert.equal(
    activation.desktopPanelTargetAcknowledged({
      layout: { ...activeBrowserA.layout, rendererGeometry: {
        ...activeBrowserA.layout.rendererGeometry,
        browserContentSlot: { ...activeBrowserA.layout.rendererGeometry.browserContentSlot, tabId: 'browser-b' },
      } },
    }, browserA),
    false,
    '其他 Browser Tab 的内容槽不能确认当前激活目标',
  );
  assert.equal(
    activation.sameDesktopPanelTarget(browserA, browserB),
    false,
    '不同 Browser Tab 必须是不同激活目标',
  );
});

console.log('desktop panel activation golden passed');
