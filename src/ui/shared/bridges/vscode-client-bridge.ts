import type { ClientBridge, ClientBridgeMessage, SupportedLocale } from './client-bridge';

let vsCodeApi: VsCodeApi | null = null;
const listeners: Set<(message: ClientBridgeMessage) => void> = new Set();
let bridgeListenerRegistered = false;

function getVsCodeApi(): VsCodeApi | null {
  if (vsCodeApi) {
    return vsCodeApi;
  }
  if (typeof acquireVsCodeApi === 'function') {
    vsCodeApi = acquireVsCodeApi();
    return vsCodeApi;
  }
  console.warn('[vscode-client-bridge] 不在 VS Code 环境中，使用模拟 API');
  return null;
}

function sanitizeMessage(message: ClientBridgeMessage): ClientBridgeMessage {
  try {
    if (typeof structuredClone === 'function') {
      return structuredClone(message);
    }
  } catch {
    // fall through to JSON clone
  }
  try {
    return JSON.parse(JSON.stringify(message));
  } catch (error) {
    console.warn('[vscode-client-bridge] 消息序列化失败，可能包含不可克隆对象', error);
    return message;
  }
}

function ensureWindowListener(): void {
  if (bridgeListenerRegistered || typeof window === 'undefined') {
    return;
  }
  bridgeListenerRegistered = true;
  console.log('[vscode-client-bridge] 开始监听消息...');
  window.addEventListener('message', (event) => {
    const message = event.data as ClientBridgeMessage;
    const msgType = message?.type;
    const msgId = (message as any)?.message?.id;
    console.log(`[vscode-client-bridge] 收到消息: type=${msgType}, id=${msgId}, listeners=${listeners.size}`);
    listeners.forEach((listener) => {
      try {
        listener(message);
      } catch (error) {
        console.error('[vscode-client-bridge] 消息处理错误:', error);
      }
    });
  });
}

export function createVsCodeClientBridge(): ClientBridge {
  ensureWindowListener();

  return {
    kind: 'vscode',
    postMessage(message: ClientBridgeMessage): void {
      const api = getVsCodeApi();
      if (api) {
        api.postMessage(sanitizeMessage(message));
      } else {
        console.log('[vscode-client-bridge] postMessage:', message);
      }
    },
    onMessage(listener: (message: ClientBridgeMessage) => void): () => void {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    getState<T>(): T | undefined {
      const api = getVsCodeApi();
      if (api) {
        return api.getState() as T | undefined;
      }
      const stored = localStorage.getItem('webview-state');
      return stored ? JSON.parse(stored) : undefined;
    },
    setState<T>(state: T): void {
      const api = getVsCodeApi();
      if (api) {
        api.setState(state);
      } else {
        localStorage.setItem('webview-state', JSON.stringify(state));
      }
    },
    getInitialSessionId(): string {
      if (typeof window !== 'undefined') {
        return (window as unknown as { __INITIAL_SESSION_ID__?: string }).__INITIAL_SESSION_ID__ || '';
      }
      return '';
    },
    getInitialLocale(): SupportedLocale {
      if (typeof window !== 'undefined') {
        const locale = (window as unknown as { __INITIAL_LOCALE__?: string }).__INITIAL_LOCALE__;
        if (locale === 'zh-CN' || locale === 'en-US') {
          return locale;
        }
      }
      return '';
    },
    notifyReady(): void {
      this.postMessage({ type: 'webviewReady' });
    },
  };
}
