import { mount } from 'svelte';
import App from './App.svelte';
import './styles/global.css';
import './styles/messages.css';
import { initMessageHandler } from './lib/message-handler';
import { getInitialLocale, getInitialSessionId, vscode } from './lib/vscode-bridge';
import { setCurrentSessionId } from './stores/messages.svelte';
import { i18n } from './stores/i18n.svelte';

declare global {
  interface Window {
    __MAGI_WEBVIEW_BOOTED__?: boolean;
    __INITIAL_LOCALE__?: string;
  }
}

let app: ReturnType<typeof mount> | undefined;

if (window.__MAGI_WEBVIEW_BOOTED__) {
  console.warn('[Main] webview 已初始化，跳过重复挂载');
} else {
  window.__MAGI_WEBVIEW_BOOTED__ = true;

  // 初始化 sessionId（从扩展宿主注入的值）
  const initialSessionId = getInitialSessionId();
  const initialLocale = getInitialLocale();
  if (initialLocale) {
    i18n.setLocale(initialLocale);
  }
  if (initialSessionId) {
    setCurrentSessionId(initialSessionId);
    console.log('[Main] 初始 sessionId:', initialSessionId);
  }

  // 初始化消息处理器
  initMessageHandler();

  // 粘贴去重：VS Code Webview 中 paste 可能被浏览器引擎和宿主各触发一次，
  // 在 capture 阶段拦截短时间内的重复 paste 事件，避免粘贴翻倍
  let lastPasteTime = 0;
  let lastPasteTarget: EventTarget | null = null;
  let lastPasteSignature = '';
  document.addEventListener('paste', (e) => {
    const now = Date.now();
    const clipboard = e.clipboardData;
    const target = e.target;
    const types = clipboard ? Array.from(clipboard.types || []) : [];
    const text = clipboard ? (clipboard.getData('text/plain') || '') : '';
    const signature = clipboard
      ? `${types.join('|')}::${text.slice(0, 200)}`
      : '';
    const isDuplicate = signature
      && now - lastPasteTime < 80
      && target === lastPasteTarget
      && signature === lastPasteSignature;

    if (isDuplicate) {
      e.preventDefault();
      e.stopImmediatePropagation();
      return;
    }
    lastPasteTime = now;
    lastPasteTarget = target;
    lastPasteSignature = signature;
  }, true);

  // 剪贴板快捷键支持
  // VS Code Sidebar WebviewView 中，Ctrl/Cmd+C/X/A 会被 VS Code 宿主拦截，
  // 不会传递给 iframe 内的 input/textarea，需要手动 execCommand 来恢复功能。
  // 粘贴(V)不做手动处理：execCommand('paste') 已被现代浏览器禁用，
  // 依赖 VS Code 宿主通过 paste 事件注入 + 上方的 paste 去重机制。
  document.addEventListener('keydown', (e) => {
    const meta = e.metaKey || e.ctrlKey;
    if (!meta || e.altKey || e.shiftKey || e.defaultPrevented || e.isComposing) return;
    const key = e.key.toLowerCase();
    const code = e.code;

    const isCopy = code === 'KeyC' || key === 'c';
    const isCut = code === 'KeyX' || key === 'x';
    const isSelectAll = code === 'KeyA' || key === 'a';
    const isPaste = code === 'KeyV' || key === 'v';
    if (!isCopy && !isCut && !isSelectAll && !isPaste) return;

    if (isCopy) {
      e.preventDefault();
      e.stopPropagation();
      document.execCommand('copy');
      return;
    }
    if (isCut) {
      e.preventDefault();
      e.stopPropagation();
      document.execCommand('cut');
      return;
    }
    if (isSelectAll) {
      e.preventDefault();
      e.stopPropagation();
      const target = e.target;
      if (target instanceof HTMLTextAreaElement || target instanceof HTMLInputElement) {
        target.select();
        return;
      }
      document.execCommand('selectAll');
      return;
    }
    // 粘贴交给浏览器/宿主默认链路，避免因剪贴板权限限制导致粘贴失效
    return;
  }, true);

  // 挂载 Svelte 应用
  app = mount(App, {
    target: document.getElementById('app')!,
  });

  // 通知扩展宿主 webview 已就绪
  vscode.postMessage({ type: 'webviewReady' });
}

export default app;
