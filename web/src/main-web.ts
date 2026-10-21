import { bootstrapApp } from './bootstrap-app';
import { initializeAppearanceRuntime } from './appearance/runtime';
import { createWebClientBridge } from './shared/bridges/web-client-bridge';
import { preloadMarkdownRenderer } from './lib/markdown-renderer-loader';
import type { Component } from 'svelte';

const desktopSurface = window.magiDesktop?.surface
  ?? new URLSearchParams(window.location.search).get('desktopSurface')
  ?? null;
if (desktopSurface === 'app') {
  document.documentElement.dataset.magiDesktopSurface = desktopSurface;
}
const bridge = createWebClientBridge();

async function loadRootComponent(): Promise<Component> {
  if (desktopSurface === 'app') {
    return (await import('./DesktopAppShell.svelte')).default;
  }
  return (await import('./web/WebWorkbenchShell.svelte')).default;
}

const app = Promise.all([
  initializeAppearanceRuntime().catch((error) => {
    console.error('[appearance] 初始化外观失败', error);
  }),
  loadRootComponent(),
]).then(([, RootComponent]) => bootstrapApp(bridge, RootComponent));

// Keep the parser and KaTeX out of the initial entry, but warm the shared
// chunk once the shell has had a chance to paint so the first response does
// not pay the full network/parse cost.
app.then(() => {
  const schedule = window.requestIdleCallback ?? ((callback: IdleRequestCallback) => {
    window.setTimeout(() => callback({
      didTimeout: false,
      timeRemaining: () => 0,
    } as IdleDeadline), 250);
  });
  schedule(() => preloadMarkdownRenderer());
});

export default app;
