/**
 * Electron Renderer 与 Web 工作台共用的 daemon 传输层。
 * 业务层通过同一 fetch / EventSource 入口发送请求和订阅服务端事实。
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
      return fetch(withPublicTunnelToken(url), init);
    },
    connectEventStream(url: string, handlers: SseHandlers): SseConnection {
      let closedByClient = false;
      const stream = new EventSource(withPublicTunnelToken(url));
      stream.onopen = () => handlers.onOpen();
      stream.onmessage = (event) => handlers.onMessage(event.data);
      stream.onerror = () => {
        if (!closedByClient) {
          handlers.onError();
        }
      };
      return {
        close: () => {
          closedByClient = true;
          stream.close();
        },
      };
    },
  };
}

function withPublicTunnelToken(url: string): string {
  if (typeof window === 'undefined') return url;
  const pageUrl = new URL(window.location.href);
  const token = pageUrl.searchParams.get('tunnel_token');
  if (!token) return url;
  const requestUrl = new URL(url, pageUrl.origin);
  if (requestUrl.origin !== pageUrl.origin) return requestUrl.toString();
  if (!requestUrl.searchParams.has('tunnel_token')) {
    requestUrl.searchParams.set('tunnel_token', token);
  }
  return requestUrl.toString();
}

// Electron Renderer 与 Web 共用同一条 HTTP/SSE 通道。
let transport: AgentTransport | null = null;

export function initTransport(): void {
  transport ??= createDirectTransport();
}

export function getTransport(): AgentTransport {
  initTransport();
  return transport!;
}
