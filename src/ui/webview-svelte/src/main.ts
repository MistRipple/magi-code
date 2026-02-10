import { mount } from 'svelte';
import App from './App.svelte';
import './styles/global.css';
import './styles/messages.css';
import { initMessageHandler } from './lib/message-handler';
import { getInitialSessionId, vscode } from './lib/vscode-bridge';
import { setCurrentSessionId } from './stores/messages.svelte';

declare global {
  interface Window {
    __MAGI_WEBVIEW_BOOTED__?: boolean;
  }
}

let app: ReturnType<typeof mount> | undefined;

if (window.__MAGI_WEBVIEW_BOOTED__) {
  console.warn('[Main] webview 已初始化，跳过重复挂载');
} else {
  window.__MAGI_WEBVIEW_BOOTED__ = true;

  // 初始化 sessionId（从扩展宿主注入的值）
  const initialSessionId = getInitialSessionId();
  if (initialSessionId) {
    setCurrentSessionId(initialSessionId);
    console.log('[Main] 初始 sessionId:', initialSessionId);
  }

  // 初始化消息处理器
  initMessageHandler();

  // 挂载 Svelte 应用
  app = mount(App, {
    target: document.getElementById('app')!,
  });

  // 通知扩展宿主 webview 已就绪
  vscode.postMessage({ type: 'webviewReady' });
}

export default app;
