import { createVsCodeClientBridge } from '../../shared/bridges/vscode-client-bridge';
import { createWebClientBridge } from '../../shared/bridges/web-client-bridge';
import { bootstrapApp } from './bootstrap-app';

// 如果宿主注入了 Agent baseUrl，插件面板走 Agent HTTP/SSE 单链（与 Web 端统一）。
// 否则回退到 vscode-client-bridge（宿主内 runtime）。
const agentBaseUrl = typeof window !== 'undefined'
  ? (window as unknown as { __AGENT_BASE_URL__?: string }).__AGENT_BASE_URL__?.trim() || ''
  : '';

const bridge = agentBaseUrl
  ? createWebClientBridge()
  : createVsCodeClientBridge();

export default bootstrapApp(bridge);
