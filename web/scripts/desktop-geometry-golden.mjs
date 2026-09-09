import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';

const read = (path) => readFile(new URL(path, import.meta.url), 'utf8');
const [shell, layout, rightPane, browserTab] = await Promise.all([
  read('../src/web/WebWorkbenchShell.svelte'),
  read('../../apps/desktop/src/main/window-layout.ts'),
  read('../src/web/RightPane.svelte'),
  read('../src/components/tabs/BrowserTabContent.svelte'),
]);

const removedGeometryProtocol = /browserContentSlot|rendererGeometry|renderer_geometry|bindContentSurface|browserContentBounds/u;
assert.doesNotMatch(shell, removedGeometryProtocol, 'Renderer 不得向 Main 上报浏览器几何');
assert.doesNotMatch(layout, removedGeometryProtocol, '窗口布局不得保存浏览器内容坐标');
assert.doesNotMatch(rightPane, removedGeometryProtocol, '右栏路由不得依赖浏览器几何协议');
assert.doesNotMatch(browserTab, removedGeometryProtocol, 'Browser Tab 不得发布或消费浏览器几何协议');

assert.match(shell, /grid-template-columns:[\s\S]*minmax\(var\(--workbench-min-content-width/u);
assert.match(shell, /var\(--desktop-right-pane-divider-width[\s\S]*var\(--desktop-right-pane-width/u);
assert.match(shell, /\.desktop-right-pane-column \{[\s\S]*width: 100%;[\s\S]*min-width: 0;[\s\S]*min-height: 0;/u);
assert.match(shell, /\.desktop-right-pane-column :global\(\.right-pane\) \{[\s\S]*width: 100%;[\s\S]*height: 100%;/u);

assert.match(layout, /rightPaneWidth: clampRightPaneWidth/u);
assert.match(layout, /rightPaneBounds = state\.rightPaneVisible/u);
assert.match(layout, /dividerBounds: state\.rightPaneVisible && sideBySide/u);
assert.match(rightPane, /class="right-pane-body"/u);
assert.match(rightPane, /class="right-pane-browser-tab-host"[\s\S]*hidden=\{/u);
assert.match(browserTab, /class="browser-surface-slot"[\s\S]*class="browser-webview"/u);
assert.match(browserTab, /\.browser-surface-slot \{[\s\S]*display: flex;[\s\S]*flex: 1;[\s\S]*min-width: 0;[\s\S]*min-height: 0;[\s\S]*overflow: hidden;/u);
assert.match(browserTab, /\.browser-webview \{[\s\S]*width: 100%;[\s\S]*height: 100%;[\s\S]*min-width: 0;[\s\S]*min-height: 0;/u);

console.log('desktop geometry golden passed');
