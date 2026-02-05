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
    api.postMessage(sanitizeMessage(message));
  } else {
    console.log('[vscode-bridge] postMessage:', message);
  }
}

function sanitizeMessage(message: WebviewMessage): WebviewMessage {
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
    console.warn('[vscode-bridge] 消息序列化失败，可能包含不可克隆对象', error);
    return message;
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
  console.log('[vscode-bridge] 🔧 开始监听消息...');
  window.addEventListener('message', (event) => {
    const message = event.data as WebviewMessage;
    const msgType = message?.type;
    const msgId = (message as any)?.message?.id;
    // 🔧 调试日志：追踪所有收到的消息
    console.log(`[vscode-bridge] 收到消息: type=${msgType}, id=${msgId}, listeners=${listeners.size}`);
    if (msgType === 'unifiedMessage' || msgType === 'unifiedUpdate' || msgType === 'unifiedComplete') {
      const source = (message as any)?.message?.source;
      const agent = (message as any)?.message?.agent;
      // 🔧 增强调试：特别关注 Worker 消息
      if (source === 'worker') {
        console.log('[vscode-bridge] 🎯 WORKER 消息:', JSON.stringify({
          type: msgType,
          messageId: msgId,
          messageType: (message as any)?.message?.type,
          category: (message as any)?.message?.category,
          lifecycle: (message as any)?.message?.lifecycle,
          source,
          agent,
          blocksCount: (message as any)?.message?.blocks?.length ?? 0,
        }));
      } else {
        console.log('[vscode-bridge] 详细消息内容:', JSON.stringify({
          type: msgType,
          messageId: msgId,
          messageType: (message as any)?.message?.type,  // 方案 B：使用 MessageType
          category: (message as any)?.message?.category,
          lifecycle: (message as any)?.message?.lifecycle,
          source,
          agent,
          blocksCount: (message as any)?.message?.blocks?.length ?? 0,
          isPlaceholder: (message as any)?.message?.metadata?.isPlaceholder,
        }));
      }
    }
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
