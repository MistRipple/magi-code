import { createWebClientBridge } from './shared/bridges/web-client-bridge';
import { bootstrapApp } from './bootstrap-app';
import WebWorkbenchShell from './web/WebWorkbenchShell.svelte';
import { initializeAppearanceRuntime } from './appearance/runtime';

const bridge = createWebClientBridge();

const app = initializeAppearanceRuntime()
  .catch((error) => {
    console.error('[appearance] 初始化外观失败', error);
  })
  .then(() => bootstrapApp(bridge, WebWorkbenchShell));

export default app;
