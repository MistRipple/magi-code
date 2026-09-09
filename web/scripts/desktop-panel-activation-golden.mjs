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
  const browserB = { ...browserA, tabId: 'browser-b' };
  const terminal = { scopeKey: browserA.scopeKey, kind: 'terminal', tabId: 'terminal-a' };
  const code = { scopeKey: browserA.scopeKey, kind: 'code', tabId: 'code-a' };
  const empty = { scopeKey: browserA.scopeKey, kind: null, tabId: null };
  const noPanel = { layout: { activePanelKind: null, activeTabId: null, activeSurfaceId: null } };
  const activeBrowserA = {
    layout: { activePanelKind: 'browser', activeTabId: 'browser-a', activeSurfaceId: 'surface-a' },
  };
  const activeTerminal = {
    layout: { activePanelKind: 'terminal', activeTabId: 'terminal-a', activeSurfaceId: null },
  };
  const activeCode = {
    layout: { activePanelKind: 'code', activeTabId: 'code-a', activeSurfaceId: null },
  };

  assert.equal(activation.decideDesktopPanelActivation(noPanel, browserA, false), 'dispatch');
  assert.equal(activation.decideDesktopPanelActivation(noPanel, browserA, true), 'wait_for_in_flight');
  assert.equal(activation.decideDesktopPanelActivation(activeBrowserA, browserA, true), 'acknowledged');
  assert.equal(activation.decideDesktopPanelActivation(activeBrowserA, browserB, false), 'dispatch');
  assert.equal(activation.decideDesktopPanelActivation(activeBrowserA, terminal, true), 'wait_for_in_flight');
  assert.equal(activation.decideDesktopPanelActivation(activeBrowserA, code, false), 'dispatch');
  assert.equal(activation.decideDesktopPanelActivation(activeCode, code, false), 'acknowledged');
  assert.equal(activation.decideDesktopPanelActivation(activeTerminal, terminal, false), 'acknowledged');
  assert.equal(activation.decideDesktopPanelActivation(noPanel, empty, false), 'acknowledged');
  assert.equal(
    activation.desktopPanelTargetAcknowledged(
      { layout: { ...activeBrowserA.layout, activeSurfaceId: null } },
      browserA,
    ),
    false,
  );
  assert.equal(activation.sameDesktopPanelTarget(browserA, browserB), false);
});

console.log('desktop panel activation golden passed');
