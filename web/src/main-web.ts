import { bootstrapApp } from './bootstrap-app';
import { initializeAppearanceRuntime } from './appearance/runtime';
import { createWebClientBridge } from './shared/bridges/web-client-bridge';
import type { Component } from 'svelte';

const desktopSurface = window.magiDesktop?.surface
  ?? new URLSearchParams(window.location.search).get('desktopSurface')
  ?? null;
const bridge = createWebClientBridge();

async function loadRootComponent(): Promise<Component> {
  if (desktopSurface === 'app') {
    return (await import('./DesktopAppShell.svelte')).default;
  }
  if (desktopSurface === 'right-pane') {
    return (await import('./DesktopRightPaneShell.svelte')).default;
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
