/**
 * Agent 传输层
 *
 * 统一抽象前端与 Agent 服务之间的所有网络通信。
 * 业务层（agent-api.ts / web-client-bridge.ts）只依赖此模块的接口，
 * 完全不感知底层走的是浏览器直连还是 VS Code 宿主代理。
 *
 * 两种策略：
 * - DirectTransport：Web 模式，浏览器直接 fetch + EventSource
 * - HostProxyTransport：VS Code 模式，postMessage ↔ 宿主 Node.js ↔ Agent
 *
 * 模块初始化：
 *   initTransport()  — 由 createWebClientBridge() 在启动时调用一次
 *   getTransport()   — 返回已初始化的传输实例（业务层使用）
 */

// ============================================
// 公共接口
// ============================================

export interface SseHandlers {
  onOpen(): void;
  onMessage(data: string): void;
  onError(): void;
}

export interface SseConnection {
  close(): void;
}

export interface AgentTransport {
  /** 通用 HTTP 请求，签名与 fetch 对齐 */
  request(url: string, init?: RequestInit): Promise<Response>;
  /** SSE 事件流 */
  connectEventStream(url: string, handlers: SseHandlers): SseConnection;
}

// ============================================
// DirectTransport — Web / 浏览器直连
// ============================================

function createDirectTransport(): AgentTransport {
  return {
    request(url: string, init?: RequestInit): Promise<Response> {
      return fetch(url, init);
    },
    connectEventStream(url: string, handlers: SseHandlers): SseConnection {
      const stream = new EventSource(url);
      stream.onopen = () => handlers.onOpen();
      stream.onmessage = (event) => handlers.onMessage(event.data);
      stream.onerror = () => {
        stream.close();
        handlers.onError();
      };
      return { close: () => stream.close() };
    },
  };
}

// ============================================
// HostProxyTransport — VS Code / IDE 宿主代理
// ============================================

interface HostApi {
  postMessage(message: unknown): void;
}

const HOST_PROXY_REQUEST_TIMEOUT_MS = 30_000;

function createHostProxyTransport(hostApi: HostApi): AgentTransport {
  let requestIdCounter = 0;
  let sseSubscriptionCounter = 0;
  const pendingRequests = new Map<string, {
    resolve: (response: Response) => void;
    reject: (error: Error) => void;
    timeoutId: number | null;
  }>();

  // 统一 window message 监听：处理 API 代理响应 + SSE 事件
  let sseMessageHandler: ((data: string) => void) | null = null;
  let sseOpenHandler: (() => void) | null = null;
  let sseErrorHandler: (() => void) | null = null;
  let activeSseSubscriptionId: string | null = null;

  if (typeof window !== 'undefined') {
    window.addEventListener('message', (event) => {
      const msg = event.data;
      if (!msg || typeof msg !== 'object' || !msg.type) return;

      switch (msg.type) {
        case 'agentApiProxyResponse': {
          const pending = pendingRequests.get(msg.requestId);
          if (pending) {
            pendingRequests.delete(msg.requestId);
            if (pending.timeoutId !== null) {
              window.clearTimeout(pending.timeoutId);
            }
            const headers = new Headers(msg.headers || {});
            pending.resolve(new Response(msg.body ?? '', { status: msg.status, headers }));
          }
          break;
        }
        case 'agentSseEvent':
          if (
            typeof msg.subscriptionId === 'string'
            && msg.subscriptionId === activeSseSubscriptionId
          ) {
            sseMessageHandler?.(msg.data);
          }
          break;
        case 'agentSseStatus':
          if (
            typeof msg.subscriptionId === 'string'
            && msg.subscriptionId === activeSseSubscriptionId
          ) {
            if (msg.status === 'open') {
              sseOpenHandler?.();
            } else if (msg.status === 'error') {
              sseErrorHandler?.();
            }
          }
          break;
      }
    });
  }

  return {
    request(url: string, init?: RequestInit): Promise<Response> {
      const requestId = `proxy-${++requestIdCounter}-${Date.now()}`;
      const headersObj: Record<string, string> = {};
      if (init?.headers) {
        if (init.headers instanceof Headers) {
          init.headers.forEach((v, k) => { headersObj[k] = v; });
        } else if (Array.isArray(init.headers)) {
          for (const [k, v] of init.headers) { headersObj[k] = v; }
        } else {
          Object.assign(headersObj, init.headers);
        }
      }
      return new Promise<Response>((resolve, reject) => {
        const timeoutId = window.setTimeout(() => {
          const pending = pendingRequests.get(requestId);
          if (!pending) {
            return;
          }
          pendingRequests.delete(requestId);
          pending.reject(new Error('agent proxy request timeout'));
        }, HOST_PROXY_REQUEST_TIMEOUT_MS);
        pendingRequests.set(requestId, { resolve, reject, timeoutId });
        hostApi.postMessage({
          type: 'agentApiProxy',
          requestId,
          method: init?.method || 'GET',
          url,
          body: typeof init?.body === 'string' ? init.body : undefined,
          headers: headersObj,
        });
      });
    },
    connectEventStream(url: string, handlers: SseHandlers): SseConnection {
      // 从完整 URL 中提取 queryString 给宿主
      const queryString = url.includes('?') ? url.split('?')[1] : '';
      const subscriptionId = `sse-${Date.now()}-${++sseSubscriptionCounter}`;
      activeSseSubscriptionId = subscriptionId;
      sseOpenHandler = () => handlers.onOpen();
      sseMessageHandler = (data: string) => handlers.onMessage(data);
      sseErrorHandler = () => handlers.onError();
      hostApi.postMessage({ type: 'agentSseSubscribe', queryString, subscriptionId });
      return {
        close() {
          if (activeSseSubscriptionId === subscriptionId) {
            activeSseSubscriptionId = null;
            sseOpenHandler = null;
            sseMessageHandler = null;
            sseErrorHandler = null;
          }
          hostApi.postMessage({ type: 'agentSseUnsubscribe', subscriptionId });
        },
      };
    },
  };
}

// ============================================
// 模块级单例
// ============================================

declare const acquireVsCodeApi: undefined | (() => HostApi);

let transport: AgentTransport | null = null;
let hostApiHandle: HostApi | null = null;

/**
 * 初始化传输层。在应用启动时调用一次。
 * 自动检测运行环境并选择对应的传输策略。
 *
 * 重要：acquireVsCodeApi() 在 VS Code WebView 中只能调用一次，
 * 传输层是它的唯一调用者。其他模块通过 getHostApi() 获取 handle。
 */
export function initTransport(): void {
  if (transport) return;
  if (typeof acquireVsCodeApi === 'function') {
    hostApiHandle = acquireVsCodeApi();
    transport = createHostProxyTransport(hostApiHandle);
  } else {
    transport = createDirectTransport();
  }
}

/**
 * 获取已初始化的传输实例。
 * 如果尚未初始化，按 Web 直连模式兜底（确保 agent-api.ts 中的顶层调用不崩溃）。
 */
export function getTransport(): AgentTransport {
  if (!transport) {
    transport = createDirectTransport();
  }
  return transport;
}

/**
 * 获取 IDE 宿主 API handle（postMessage 入口）。
 * - VS Code 模式：返回 acquireVsCodeApi() 得到的 handle
 * - Web 模式：返回 null
 *
 * 供 bridge 层转发 IDE 原生能力消息（openFile、viewDiff 等）。
 */
export function getHostApi(): HostApi | null {
  return hostApiHandle;
}

/**
 * 是否运行在 IDE 宿主代理模式下（VS Code / 未来 IDEA 等）。
 * 用于个别需要区分环境的场景（如 IDE 原生能力转发）。
 */
export function isHostedTransport(): boolean {
  return hostApiHandle !== null;
}
