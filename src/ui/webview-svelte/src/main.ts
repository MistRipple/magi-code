import { mount } from 'svelte';
import App from './App.svelte';
import './styles/global.css';
import { initMessageHandler } from './lib/message-handler';
import { getInitialSessionId, vscode } from './lib/vscode-bridge';
import { setCurrentSessionId } from './stores/messages.svelte';

// 初始化 sessionId（从扩展宿主注入的值）
const initialSessionId = getInitialSessionId();
if (initialSessionId) {
  setCurrentSessionId(initialSessionId);
  console.log('[Main] 初始 sessionId:', initialSessionId);
}

// 初始化消息处理器
initMessageHandler();

// 挂载 Svelte 应用
const app = mount(App, {
  target: document.getElementById('app')!,
});

// 通知扩展宿主 webview 已就绪
vscode.postMessage({ type: 'webviewReady' });

export default app;

