import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';

const storeSource = await readFile(
  new URL('../src/stores/right-pane.svelte.ts', import.meta.url),
  'utf8',
);
const shellSource = await readFile(
  new URL('../src/DesktopRightPaneShell.svelte', import.meta.url),
  'utf8',
);
const bridgeSource = await readFile(
  new URL('../src/types/magi-desktop.d.ts', import.meta.url),
  'utf8',
);
const contractSource = await readFile(
  new URL('../../contracts/desktop-browser/src/index.ts', import.meta.url),
  'utf8',
);
const surfaceManagerSource = await readFile(
  new URL('../../apps/desktop/src/main/browser-surface-manager.ts', import.meta.url),
  'utf8',
);

assert.match(contractSource, /DESKTOP_RIGHT_PANE_INTENT_VERSION\s*=\s*1/, '右栏 IPC 必须有明确的协议版本');
assert.match(contractSource, /DesktopRightPaneAgentIntent/, '右栏 IPC 必须覆盖 Agent 面板');
assert.match(contractSource, /DesktopRightPaneCodeIntent/, '右栏 IPC 必须覆盖代码和图片面板');
assert.match(contractSource, /DesktopRightPaneTerminalIntent/, '右栏 IPC 必须覆盖终端面板');
assert.match(bridgeSource, /openRightPaneTab\(request:/, '桌面桥必须暴露打开右栏面板意图');
assert.match(bridgeSource, /readyRightPane\(\): Promise<void>/, '右栏 Renderer 必须显式声明就绪，避免启动竞态');
assert.match(bridgeSource, /onRightPaneIntent\(listener:/, '右栏 Renderer 必须订阅 Main 转发的面板意图');
assert.match(storeSource, /surface !== 'app'/, '只有 App Renderer 可以向 Main 发布跨 Renderer 面板意图');
assert.match(storeSource, /DESKTOP_RIGHT_PANE_INTENT_VERSION/, '面板意图必须携带统一协议版本');
assert.match(storeSource, /kind: 'agent'/, 'App 侧必须转发 Agent 面板意图');
assert.match(storeSource, /kind: 'code'/, 'App 侧必须转发代码和图片面板意图');
assert.match(storeSource, /kind: 'terminal'/, 'App 侧必须转发终端面板意图');
assert.match(shellSource, /onRightPaneIntent\(/, 'Right-Pane Renderer 必须接收跨 Renderer 面板意图');
assert.match(shellSource, /readyRightPane\(\)/, 'Right-Pane Renderer 必须在订阅后通知 Main 已就绪');
assert.match(shellSource, /case 'agent':[\s\S]*?openAgentTab/, '右栏必须在本地 Renderer 创建 Agent Tab');
assert.match(shellSource, /case 'code':[\s\S]*?openCodeTab/, '右栏必须在本地 Renderer 创建代码或图片 Tab');
assert.match(shellSource, /case 'terminal':[\s\S]*?openTerminalTab/, '右栏必须在本地 Renderer 创建终端 Tab');
assert.match(surfaceManagerSource, /Emulation\.setDeviceMetricsOverride/, '固定视口必须使用 Chromium 设备仿真');
assert.doesNotMatch(
  surfaceManagerSource,
  /setPageScaleFactor|fixedViewportScale|scale:\s*scale/,
  '固定视口不得通过 pageScaleFactor 或按容器拟合缩放造成失真',
);
assert.match(
  surfaceManagerSource,
  /The native parent owns geometry/,
  '右栏拖拽必须由原生父窗口 bounds 管理，不能重放 viewport',
);

console.log('desktop right-pane intent golden passed');
