import { mount, type Component } from 'svelte';
import App from './App.svelte';
import './styles/global.css';
import './styles/messages.css';
import { initMessageHandler, primeEventSeqTracking } from './lib/message-handler';
import { getState, initializeState, setCurrentSessionId } from './stores/messages.svelte';
import { i18n } from './stores/i18n.svelte';
import type { ClientBridge } from '../../shared/bridges/client-bridge';
import { notifyBridgeReady, setClientBridge } from '../../shared/bridges/bridge-runtime';

declare global {
  interface Window {
    __MAGI_WEBVIEW_BOOTED__?: boolean;
    __INITIAL_LOCALE__?: string;
  }
}

function installPasteDeduplication(): void {
  let lastPasteTime = 0;
  let lastPasteTarget: EventTarget | null = null;
  let lastPasteSignature = '';
  document.addEventListener('paste', (e) => {
    const now = Date.now();
    const clipboard = e.clipboardData;
    const target = e.target;
    const types = clipboard ? Array.from(clipboard.types || []) : [];
    const text = clipboard ? (clipboard.getData('text/plain') || '') : '';
    const signature = clipboard ? `${types.join('|')}::${text.slice(0, 200)}` : '';
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
}

function installClipboardShortcuts(): void {
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
    }
  }, true);
}

export function bootstrapApp(
  bridge: ClientBridge,
  RootComponent: Component = App,
): ReturnType<typeof mount> | undefined {
  let app: ReturnType<typeof mount> | undefined;
  if (window.__MAGI_WEBVIEW_BOOTED__) {
    console.warn('[Bootstrap] 应用已初始化，跳过重复挂载');
    return app;
  }

  window.__MAGI_WEBVIEW_BOOTED__ = true;
  setClientBridge(bridge);

  const initialSessionId = bridge.getInitialSessionId();
  const initialLocale = bridge.getInitialLocale();

  if (initialLocale) {
    i18n.setLocale(initialLocale);
  }
  if (initialSessionId) {
    setCurrentSessionId(initialSessionId);
    console.log('[Bootstrap] 初始 sessionId:', initialSessionId);
  }

  initializeState();
  const restoredState = getState();
  primeEventSeqTracking(
    restoredState.currentSessionId,
    restoredState.timelineProjection?.lastAppliedEventSeq || 0,
  );
  initMessageHandler(bridge);
  installPasteDeduplication();
  installClipboardShortcuts();

  app = mount(RootComponent, {
    target: document.getElementById('app')!,
  });

  notifyBridgeReady();
  return app;
}
