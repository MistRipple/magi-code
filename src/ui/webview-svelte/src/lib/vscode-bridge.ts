/**
 * VS Code API 桥接层
 * 封装与 VS Code 扩展宿主的通信
 */

// 消息类型定义
export interface WebviewMessage {
  type: string;
  [key: string]: unknown;
}

// VS Code API 实例（只能获取一次）
let vsCodeApi: VsCodeApi | null = null;

/**
 * 获取 VS Code API 实例
 */
function getVsCodeApi(): VsCodeApi | null {
  if (vsCodeApi) {
    return vsCodeApi;
  }
  
  // 检查是否在 VS Code webview 环境中
  if (typeof acquireVsCodeApi === 'function') {
    vsCodeApi = acquireVsCodeApi();
    return vsCodeApi;
  }
  
  // 开发环境模拟
  console.warn('[vscode-bridge] 不在 VS Code 环境中，使用模拟 API');
  return null;
}

/**
 * 发送消息到扩展宿主
 */
export function postMessage(message: WebviewMessage): void {
  const api = getVsCodeApi();
  if (api) {
    api.postMessage(message);
  } else {
    console.log('[vscode-bridge] postMessage:', message);
  }
}

/**
 * 获取持久化状态
 */
export function getState<T>(): T | undefined {
  const api = getVsCodeApi();
  if (api) {
    return api.getState() as T | undefined;
  }
  // 开发环境使用 localStorage
  const stored = localStorage.getItem('webview-state');
  return stored ? JSON.parse(stored) : undefined;
}

/**
 * 设置持久化状态
 */
export function setState<T>(state: T): void {
  const api = getVsCodeApi();
  if (api) {
    api.setState(state);
  } else {
    localStorage.setItem('webview-state', JSON.stringify(state));
  }
}

// 消息监听器类型
type MessageListener = (message: WebviewMessage) => void;
const listeners: Set<MessageListener> = new Set();

/**
 * 注册消息监听器
 */
export function onMessage(listener: MessageListener): () => void {
  listeners.add(listener);
  
  // 返回取消订阅函数
  return () => {
    listeners.delete(listener);
  };
}

// 全局消息监听
if (typeof window !== 'undefined') {
  window.addEventListener('message', (event) => {
    const message = event.data as WebviewMessage;
    listeners.forEach((listener) => {
      try {
        listener(message);
      } catch (error) {
        console.error('[vscode-bridge] 消息处理错误:', error);
      }
    });
  });
}

// 导出便捷方法
export const vscode = {
  postMessage,
  getState,
  setState,
  onMessage,
};

/**
 * 获取初始 sessionId（由扩展宿主注入）
 */
export function getInitialSessionId(): string {
  if (typeof window !== 'undefined') {
    return (window as unknown as { __INITIAL_SESSION_ID__?: string }).__INITIAL_SESSION_ID__ || '';
  }
  return '';
}

