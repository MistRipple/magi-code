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

  // 剪贴板快捷键支持（VS Code Webview 中非输入框元素的复制/剪切/全选）
  document.addEventListener('keydown', (e) => {
    const meta = e.metaKey || e.ctrlKey;
    if (!meta) return;

    // 焦点在 input/textarea 中时，浏览器原生处理即可，避免重复
    const active = document.activeElement;
    const isEditable = active instanceof HTMLInputElement || active instanceof HTMLTextAreaElement || (active as HTMLElement)?.isContentEditable;
    if (isEditable) return;

    if (e.key === 'c') {
      document.execCommand('copy');
    } else if (e.key === 'x') {
      document.execCommand('cut');
    } else if (e.key === 'a') {
      e.preventDefault();
      document.execCommand('selectAll');
    }
  });

  // 挂载 Svelte 应用
  app = mount(App, {
    target: document.getElementById('app')!,
  });

  // 通知扩展宿主 webview 已就绪
  vscode.postMessage({ type: 'webviewReady' });
}

export default app;
