import { createWebClientBridge } from '../../shared/bridges/web-client-bridge';
import { bootstrapApp } from './bootstrap-app';

const agentBaseUrl = typeof window !== 'undefined'
  ? (window as unknown as { __AGENT_BASE_URL__?: string }).__AGENT_BASE_URL__?.trim() || ''
  : '';
const workspacePath = typeof window !== 'undefined'
  ? (window as unknown as { __INITIAL_WORKSPACE_PATH__?: string }).__INITIAL_WORKSPACE_PATH__?.trim() || ''
  : '';

if (!agentBaseUrl) {
  throw new Error('[main-vscode] 缺少 __AGENT_BASE_URL__，插件面板不允许回退到宿主链路。');
}

if (!workspacePath) {
  throw new Error('[main-vscode] 缺少 __INITIAL_WORKSPACE_PATH__，插件面板不允许在未绑定工作区时启动。');
}

const bridge = createWebClientBridge();

export default bootstrapApp(bridge);
