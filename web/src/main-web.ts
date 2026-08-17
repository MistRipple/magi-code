import { bootstrapApp } from './bootstrap-app';
import { initializeAppearanceRuntime } from './appearance/runtime';
import { createWebClientBridge } from './shared/bridges/web-client-bridge';
import type { Component } from 'svelte';

const desktopSurface = window.magiDesktop?.surface
  ?? new URLSearchParams(window.location.search).get('desktopSurface')
  ?? null;
if (desktopSurface === 'app' || desktopSurface === 'overlay') {
  // App Renderer 负责整窗壁纸与材质；Overlay Renderer 保留 surface 身份，
  // 由外观运行时禁用自己的壁纸层，透明区域才能透出下方的 App/网页 Surface。
  document.documentElement.dataset.magiDesktopSurface = desktopSurface;
}
const bridge = createWebClientBridge();

async function loadRootComponent(): Promise<Component> {
  if (desktopSurface === 'app') {
    return (await import('./DesktopAppShell.svelte')).default;
  }
  if (desktopSurface === 'overlay') {
    return (await import('./DesktopOverlayShell.svelte')).default;
  }
  return (await import('./web/WebWorkbenchShell.svelte')).default;
}

const app = Promise.all([
  initializeAppearanceRuntime().catch((error) => {
    console.error('[appearance] 初始化外观失败', error);
  }),
  loadRootComponent(),
]).then(([, RootComponent]) => bootstrapApp(bridge, RootComponent));

export default app;
