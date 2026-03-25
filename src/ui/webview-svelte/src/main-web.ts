import { createWebClientBridge } from '../../shared/bridges/web-client-bridge';
import { bootstrapApp } from './bootstrap-app';
import WebWorkbenchShell from './web/WebWorkbenchShell.svelte';
import { installWebTheme } from './web/theme';

const bridge = createWebClientBridge();

installWebTheme();

export default bootstrapApp(bridge, WebWorkbenchShell);
