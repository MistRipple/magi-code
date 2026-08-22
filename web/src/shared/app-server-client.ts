import {
  APP_SERVER_METHOD_SIGNATURES,
  JSONRPC_VERSION,
  type AppServerNotificationMethod,
  type AppServerNotificationParams,
  type AppServerRequestMethod,
  type AppServerRequestParams,
  type AppServerRequestResult,
  type ClientCapabilities,
  type ClientInfo,
  type EventEnvelope,
  type EventNotificationParams,
  type EventStreamSnapshot,
  type EventSubscribeParams,
  type InitializeResult,
  type JsonRpcError,
  type JsonRpcRequestId,
  type JsonValue,
  type ServerNotification,
  type ServerRequest,
} from './app-server-protocol.generated';

export type AppServerConnectionState =
  | 'idle'
  | 'connecting'
  | 'ready'
  | 'reconnecting'
  | 'closed';

export interface AppServerError extends Error {
  code: number;
  retryable: boolean;
  data?: JsonValue;
}

export interface AppServerServerRequestContext {
  request: ServerRequest;
  respond(result: JsonValue): void;
  reject(error: JsonRpcError): void;
}

export interface AppServerClientOptions {
  endpoint: string;
  clientInfo: ClientInfo;
  capabilities?: ClientCapabilities;
  subscription?: EventSubscribeParams;
  reconnect?: boolean;
  requestTimeoutMs?: number;
  reconnectBaseDelayMs?: number;
  reconnectMaxDelayMs?: number;
  onStateChange?: (state: AppServerConnectionState) => void;
  onNotification?: (notification: ServerNotification) => void;
  onEvent?: (event: EventEnvelope) => void;
  onSnapshot?: (snapshot: EventStreamSnapshot) => void;
  onServerRequest?: (context: AppServerServerRequestContext) => void | Promise<void>;
}

interface PendingRequest {
  method: AppServerRequestMethod;
  resolve: (value: JsonValue) => void;
  reject: (error: AppServerError) => void;
  timeoutId: number | null;
  signal?: AbortSignal;
  abortListener?: () => void;
}

interface JsonRpcResponseLike {
  jsonrpc?: unknown;
  id?: unknown;
  result?: JsonValue;
  error?: JsonRpcError;
}

interface JsonRpcNotificationLike {
  jsonrpc?: unknown;
  method?: unknown;
  params?: JsonValue;
}

interface JsonRpcRequestLike extends JsonRpcNotificationLike {
  id?: unknown;
}

const DEFAULT_REQUEST_TIMEOUT_MS = 120_000;
const DEFAULT_RECONNECT_BASE_DELAY_MS = 250;
const DEFAULT_RECONNECT_MAX_DELAY_MS = 10_000;

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === 'object' && !Array.isArray(value);
}

function isEventEnvelope(value: unknown): value is EventEnvelope {
  return isRecord(value)
    && typeof value.event_id === 'string'
    && typeof value.event_type === 'string'
    && typeof value.category === 'string'
    && typeof value.occurred_at === 'number'
    && typeof value.sequence === 'number'
    && isRecord(value.payload);
}

function isEventStreamSnapshot(value: unknown): value is EventStreamSnapshot {
  return isRecord(value)
    && typeof value.next_sequence === 'number'
    && Array.isArray(value.recent_events)
    && value.recent_events.every(isEventEnvelope);
}

function isEventNotificationParams(value: unknown): value is EventNotificationParams {
  return isRecord(value)
    && typeof value.sequence === 'number'
    && isEventEnvelope(value.event);
}

function requestIdKey(value: JsonRpcRequestId): string {
  return typeof value === 'number' ? `number:${value}` : `string:${value}`;
}

function errorFromRpc(error: JsonRpcError): AppServerError {
  const result = new Error(error.message) as AppServerError;
  result.name = 'AppServerError';
  result.code = error.code;
  result.retryable = error.retryable === true;
  if (error.data !== undefined) result.data = error.data;
  return result;
}

function protocolError(message: string, code = -32600): AppServerError {
  const result = new Error(message) as AppServerError;
  result.name = 'AppServerProtocolError';
  result.code = code;
  result.retryable = false;
  return result;
}

function invalidParams(message: string): AppServerError {
  return protocolError(message, -32602);
}

function nonEmptyString(value: unknown): value is string {
  return typeof value === 'string' && value.trim().length > 0;
}

function jsonRpcId(value: unknown): value is JsonRpcRequestId {
  return (typeof value === 'string' && value.length > 0) ||
    (typeof value === 'number' && Number.isFinite(value));
}

function knownRequestMethod(method: string): method is AppServerRequestMethod {
  return Object.hasOwn(APP_SERVER_METHOD_SIGNATURES, method)
    && APP_SERVER_METHOD_SIGNATURES[method as keyof typeof APP_SERVER_METHOD_SIGNATURES].kind === 'request';
}

function knownNotificationMethod(method: string): method is AppServerNotificationMethod {
  return Object.hasOwn(APP_SERVER_METHOD_SIGNATURES, method)
    && APP_SERVER_METHOD_SIGNATURES[method as keyof typeof APP_SERVER_METHOD_SIGNATURES].kind === 'notification';
}

function assertRequestParams(method: AppServerRequestMethod, params: unknown): void {
  if (!isRecord(params)) throw invalidParams(`${method} params 必须是对象`);
  switch (method) {
    case 'initialize':
      if (!isRecord(params.clientInfo) || !nonEmptyString(params.clientInfo.name)) {
        throw invalidParams('initialize.clientInfo.name 必须是非空字符串');
      }
      break;
    case 'session/read':
      if (!nonEmptyString(params.sessionId)) throw invalidParams('session/read.sessionId 必须是非空字符串');
      break;
    case 'turn/start':
      if (params.scope !== 'personal' && params.scope !== 'workspace') {
        throw invalidParams('turn/start.scope 必须是 personal 或 workspace');
      }
      break;
    case 'browser/tool':
      if (!nonEmptyString(params.sessionId) || !nonEmptyString(params.tool) || !isRecord(params.arguments)) {
        throw invalidParams('browser/tool 必须包含 sessionId、tool 和 arguments 对象');
      }
      break;
    case 'approval/request':
      if (!nonEmptyString(params.sessionId) || !nonEmptyString(params.reason)) {
        throw invalidParams('approval/request 必须包含 sessionId 和 reason');
      }
      break;
    case 'ping':
    case 'session/list':
    case 'events/subscribe':
    case 'browser/tools/list':
      break;
  }
}

function assertServerRequest(method: string, params: unknown): void {
  if (method !== 'approval/request' || !isRecord(params)) {
    throw protocolError(`客户端不支持服务端请求: ${method}`);
  }
  assertRequestParams('approval/request', params);
}

function errorFromThrown(error: unknown): JsonRpcError {
  if (error && typeof error === 'object' && 'code' in error && 'message' in error &&
    typeof error.code === 'number' && typeof error.message === 'string') {
    return {
      code: error.code,
      message: error.message,
      retryable: 'retryable' in error && error.retryable === true,
    };
  }
  return { code: -32600, message: error instanceof Error ? error.message : String(error), retryable: false };
}

function assertResult(method: AppServerRequestMethod, value: unknown): void {
  if (!isRecord(value)) throw protocolError(`${method} 返回结果必须是对象`);
  if (method === 'initialize' || method === 'ping' || method.endsWith('/list') || method.endsWith('/read')) {
    if (!nonEmptyString(value.runtimeEpoch)) throw protocolError(`${method} 缺少 runtimeEpoch`);
  }
  switch (method) {
    case 'initialize':
      if (!isRecord(value.serverInfo) || !isRecord(value.protocol) || !isRecord(value.capabilities)) {
        throw protocolError('initialize 返回结果结构无效');
      }
      break;
    case 'ping':
      break;
    case 'session/list':
      if (!Array.isArray(value.sessions)) throw protocolError('session/list.sessions 必须是数组');
      break;
    case 'session/read':
      if (!isRecord(value.session) || !Array.isArray(value.turns)) throw protocolError('session/read 返回结果结构无效');
      break;
    case 'turn/start':
      if (!nonEmptyString(value.sessionId) || !nonEmptyString(value.entryId) || !nonEmptyString(value.eventId)) {
        throw protocolError('turn/start 返回结果结构无效');
      }
      break;
    case 'events/subscribe':
      if (typeof value.subscribed !== 'boolean' || typeof value.nextSequence !== 'number' || typeof value.resyncRequired !== 'boolean') {
        throw protocolError('events/subscribe 返回结果结构无效');
      }
      break;
    case 'browser/tools/list':
      if (!Array.isArray(value.tools) || !isRecord(value.capabilities)) throw protocolError('browser/tools/list 返回结果结构无效');
      break;
    case 'browser/tool':
      if (!nonEmptyString(value.tool) || !nonEmptyString(value.itemId) || !nonEmptyString(value.turnId) ||
        !nonEmptyString(value.runtimeEpoch) || !['completed', 'blocked', 'failed', 'cancelled', 'indeterminate'].includes(String(value.status))) {
        throw protocolError('browser/tool 返回结果结构无效');
      }
      break;
    case 'approval/request':
      if (typeof value.approved !== 'boolean') throw protocolError('approval/request 返回结果结构无效');
      break;
  }
}

function endpointForWebSocket(endpoint: string): string {
  const url = new URL(endpoint, typeof window === 'undefined' ? undefined : window.location.href);
  if (url.protocol === 'http:') url.protocol = 'ws:';
  if (url.protocol === 'https:') url.protocol = 'wss:';
  return url.toString();
}

function isResponse(value: Record<string, unknown>): boolean {
  return 'id' in value && !('method' in value) && ('result' in value || 'error' in value);
}

function isNotification(value: Record<string, unknown>): boolean {
  return typeof value.method === 'string' && !('id' in value);
}

function isServerRequest(value: Record<string, unknown>): boolean {
  return typeof value.method === 'string' && 'id' in value;
}

function asJsonRpcError(value: unknown): JsonRpcError {
  if (!isRecord(value) || typeof value.code !== 'number' || typeof value.message !== 'string') {
    return { code: -32600, message: '服务端返回了无效错误对象', retryable: false };
  }
  return {
    code: value.code,
    message: value.message,
    ...(value.data !== undefined ? { data: value.data } : {}),
    ...(typeof value.retryable === 'boolean' ? { retryable: value.retryable } : {}),
  };
}

export class AppServerClient {
  readonly #options: Required<Pick<
    AppServerClientOptions,
    'reconnect' | 'requestTimeoutMs' | 'reconnectBaseDelayMs' | 'reconnectMaxDelayMs'
  >> & AppServerClientOptions;
  readonly #pending = new Map<string, PendingRequest>();
  #socket: WebSocket | null = null;
  #state: AppServerConnectionState = 'idle';
  #connectPromise: Promise<InitializeResult> | null = null;
  #openResolve: (() => void) | null = null;
  #openReject: ((error: AppServerError) => void) | null = null;
  #nextRequestId = 0;
  #reconnectAttempt = 0;
  #reconnectTimer: number | null = null;
  #closedByClient = false;
  #initializeResult: InitializeResult | null = null;
  #subscription: EventSubscribeParams | null;
  #lastSequence = 0;

  constructor(options: AppServerClientOptions) {
    this.#options = {
      ...options,
      reconnect: options.reconnect !== false,
      requestTimeoutMs: options.requestTimeoutMs ?? DEFAULT_REQUEST_TIMEOUT_MS,
      reconnectBaseDelayMs: options.reconnectBaseDelayMs ?? DEFAULT_RECONNECT_BASE_DELAY_MS,
      reconnectMaxDelayMs: options.reconnectMaxDelayMs ?? DEFAULT_RECONNECT_MAX_DELAY_MS,
    };
    this.#subscription = options.subscription ? { ...options.subscription } : null;
  }

  get state(): AppServerConnectionState {
    return this.#state;
  }

  get initializeResult(): InitializeResult | null {
    return this.#initializeResult;
  }

  get lastSequence(): number {
    return this.#lastSequence;
  }

  setSubscription(subscription: EventSubscribeParams | null): void {
    this.#subscription = subscription ? { ...subscription } : null;
  }

  async connect(): Promise<InitializeResult> {
    if (this.#state === 'ready' && this.#initializeResult) return this.#initializeResult;
    if (this.#connectPromise) return this.#connectPromise;
    this.#closedByClient = false;
    this.#connectPromise = this.connectOnce().finally(() => {
      this.#connectPromise = null;
    });
    return this.#connectPromise;
  }

  close(): void {
    this.#closedByClient = true;
    if (this.#reconnectTimer !== null) {
      window.clearTimeout(this.#reconnectTimer);
      this.#reconnectTimer = null;
    }
    this.rejectOpen(protocolError('App Server 连接已关闭', -32000));
    this.rejectPending(protocolError('App Server 连接已关闭', -32000));
    this.#socket?.close();
    this.#socket = null;
    this.setState('closed');
  }

  async request<M extends AppServerRequestMethod>(
    method: M,
    params: AppServerRequestParams<M>,
    options: { timeoutMs?: number; signal?: AbortSignal } = {},
  ): Promise<AppServerRequestResult<M>> {
    await this.connect();
    const id = this.nextRequestId();
    const result = await this.requestRaw(id, method, params, options);
    return result;
  }

  private async connectOnce(): Promise<InitializeResult> {
    this.setState(this.#reconnectAttempt > 0 ? 'reconnecting' : 'connecting');
    const socket = new WebSocket(endpointForWebSocket(this.#options.endpoint));
    this.#socket = socket;
    const open = new Promise<void>((resolve, reject) => {
      this.#openResolve = resolve;
      this.#openReject = reject;
    });
    socket.addEventListener('open', () => this.handleOpen());
    socket.addEventListener('message', (event) => this.handleMessage(event.data));
    socket.addEventListener('error', () => {
      this.#openReject?.(protocolError('App Server WebSocket 连接失败'));
    });
    socket.addEventListener('close', () => this.handleClose(socket));
    await open;
    const initialize = await this.requestRaw(
      this.nextRequestId(),
      'initialize',
      {
        clientInfo: this.#options.clientInfo,
        protocol: { major: 1, minor: 0 },
        capabilities: this.#options.capabilities ?? {},
      },
      { timeoutMs: this.#options.requestTimeoutMs },
    );
    this.#initializeResult = initialize;
    this.sendNotification('initialized', {});
    if (this.#subscription) {
      const subscription = {
        ...this.#subscription,
        afterSequence: Math.max(this.#subscription.afterSequence ?? 0, this.#lastSequence),
      };
      await this.requestRaw(
        this.nextRequestId(),
        'events/subscribe',
        subscription,
        { timeoutMs: this.#options.requestTimeoutMs },
      );
    }
    this.#reconnectAttempt = 0;
    this.setState('ready');
    return initialize;
  }

  private handleOpen(): void {
    this.#openResolve?.();
    this.#openResolve = null;
    this.#openReject = null;
  }

  private handleClose(socket: WebSocket): void {
    if (this.#socket !== socket) return;
    this.#socket = null;
    this.#initializeResult = null;
    this.rejectPending(protocolError('App Server WebSocket 已断开', -32000));
    if (this.#closedByClient) {
      this.setState('closed');
      return;
    }
    this.setState('reconnecting');
    if (this.#options.reconnect) this.scheduleReconnect();
  }

  private scheduleReconnect(): void {
    if (this.#reconnectTimer !== null || this.#closedByClient) return;
    const delay = Math.min(
      this.#options.reconnectMaxDelayMs,
      this.#options.reconnectBaseDelayMs * (2 ** Math.min(this.#reconnectAttempt, 6)),
    );
    this.#reconnectAttempt += 1;
    this.#reconnectTimer = window.setTimeout(() => {
      this.#reconnectTimer = null;
      void this.connect().catch(() => this.scheduleReconnect());
    }, delay);
  }

  private async requestRaw<M extends AppServerRequestMethod>(
    id: JsonRpcRequestId,
    method: M,
    params: AppServerRequestParams<M>,
    options: { timeoutMs?: number; signal?: AbortSignal },
  ): Promise<AppServerRequestResult<M>> {
    if (!knownRequestMethod(method)) throw protocolError(`未知 App Server 请求方法: ${method}`);
    assertRequestParams(method, params);
    const socket = this.#socket;
    if (!socket || socket.readyState !== WebSocket.OPEN) {
      throw protocolError('App Server 尚未连接', -32000);
    }
    const timeoutMs = options.timeoutMs ?? this.#options.requestTimeoutMs;
    if (!Number.isInteger(timeoutMs) || timeoutMs < 100 || timeoutMs > 600_000) {
      throw invalidParams('requestTimeoutMs 必须在 100 到 600000 毫秒之间');
    }
    const key = requestIdKey(id);
    return new Promise<JsonValue>((resolve, reject) => {
      const abortListener = options.signal
        ? () => {
          this.#pending.delete(key);
          this.sendCancel(id);
          reject(protocolError('App Server 请求已取消', -32800));
        }
        : undefined;
      const timeoutId = window.setTimeout(() => {
        this.#pending.delete(key);
        this.sendCancel(id);
        reject(protocolError('App Server 请求超时', -32801));
      }, timeoutMs);
      this.#pending.set(key, {
        method,
        resolve,
        reject,
        timeoutId,
        signal: options.signal,
        abortListener,
      });
      if (options.signal?.aborted) {
        abortListener?.();
        return;
      }
      options.signal?.addEventListener('abort', abortListener!, { once: true });
      socket.send(JSON.stringify({
        jsonrpc: JSONRPC_VERSION,
        id,
        method,
        requestTimeoutMs: timeoutMs,
        ...(params !== undefined ? { params } : {}),
      }));
    }) as Promise<AppServerRequestResult<M>>;
  }

  private sendNotification<M extends AppServerNotificationMethod>(
    method: M,
    params: AppServerNotificationParams<M>,
  ): void {
    if (!knownNotificationMethod(method)) {
      this.#options.onNotification?.({
        jsonrpc: JSONRPC_VERSION,
        method: 'protocol/error',
        params: { code: -32601, message: `未知 App Server 通知方法: ${method}` },
      });
      return;
    }
    if (method === '$/cancelRequest') {
      if (!isRecord(params) || !jsonRpcId(params.id)) {
        throw invalidParams('$/cancelRequest.id 必须是有效请求 ID');
      }
    }
    if (this.#socket?.readyState !== WebSocket.OPEN) return;
    this.#socket.send(JSON.stringify({ jsonrpc: JSONRPC_VERSION, method, params }));
  }

  private sendCancel(id: JsonRpcRequestId): void {
    this.sendNotification('$/cancelRequest', { id });
  }

  private nextRequestId(): string {
    this.#nextRequestId += 1;
    return `app-server-${Date.now()}-${this.#nextRequestId}`;
  }

  private handleMessage(raw: unknown): void {
    let value: unknown;
    try {
      value = typeof raw === 'string' ? JSON.parse(raw) : raw;
    } catch {
      this.#options.onNotification?.({
        jsonrpc: JSONRPC_VERSION,
        method: 'protocol/error',
        params: { code: -32600, message: 'App Server 返回了无效 JSON' },
      });
      return;
    }
    if (!isRecord(value) || value.jsonrpc !== JSONRPC_VERSION) return;
    if (isResponse(value)) {
      this.handleResponse(value as JsonRpcResponseLike);
      return;
    }
    if (isServerRequest(value)) {
      this.handleServerRequest(value as JsonRpcRequestLike);
      return;
    }
    if (isNotification(value)) {
      this.handleNotification(value as JsonRpcNotificationLike);
    }
  }

  private handleResponse(value: JsonRpcResponseLike): void {
    if (typeof value.id !== 'string' && typeof value.id !== 'number') return;
    const pending = this.#pending.get(requestIdKey(value.id));
    if (!pending) return;
    this.#pending.delete(requestIdKey(value.id));
    this.clearPending(pending);
    if (value.error !== undefined) {
      pending.reject(errorFromRpc(asJsonRpcError(value.error)));
      return;
    }
    try {
      assertResult(pending.method, value.result ?? null);
      pending.resolve(value.result ?? null);
    } catch (error) {
      pending.reject(error instanceof Error ? error as AppServerError : protocolError(String(error)));
    }
  }

  private handleNotification(value: JsonRpcNotificationLike): void {
    const method = typeof value.method === 'string' ? value.method : '';
    const params = value.params;
    if (method === 'events/snapshot' && isEventStreamSnapshot(params)) {
      const snapshot = params;
      this.#lastSequence = Math.max(this.#lastSequence, Math.max(0, snapshot.next_sequence - 1));
      this.#options.onSnapshot?.(snapshot);
    } else if (method === 'events/resyncRequired' && isRecord(params)) {
      const snapshot = isEventStreamSnapshot(params.snapshot) ? params.snapshot : null;
      if (snapshot) {
        this.#lastSequence = Math.max(this.#lastSequence, Math.max(0, snapshot.next_sequence - 1));
        this.#options.onSnapshot?.(snapshot);
      }
    } else if (method.startsWith('event/') && isEventNotificationParams(params)) {
      this.#lastSequence = Math.max(this.#lastSequence, params.sequence);
      this.#options.onEvent?.(params.event);
    }
    this.#options.onNotification?.({
      jsonrpc: JSONRPC_VERSION,
      method,
      ...(params !== undefined ? { params } : {}),
    });
  }

  private handleServerRequest(value: JsonRpcRequestLike): void {
    if (typeof value.id !== 'string' && typeof value.id !== 'number' || typeof value.method !== 'string') {
      return;
    }
    const request: ServerRequest = {
      jsonrpc: JSONRPC_VERSION,
      id: value.id,
      method: value.method,
      ...(value.params !== undefined ? { params: value.params } : {}),
    };
    try {
      assertServerRequest(request.method, request.params);
    } catch (error) {
      this.sendResponse(request.id, undefined, errorFromThrown(error));
      return;
    }
    let settled = false;
    const respond = (result: JsonValue) => {
      if (settled) return;
      if (!isRecord(result) || typeof result.approved !== 'boolean') {
        settled = true;
        this.sendResponse(request.id, undefined, {
          code: -32602,
          message: 'approval/request 响应必须包含 approved 布尔值',
          retryable: false,
        });
        return;
      }
      settled = true;
      this.sendResponse(request.id, result, undefined);
    };
    const reject = (error: JsonRpcError) => {
      if (settled) return;
      settled = true;
      this.sendResponse(request.id, undefined, error);
    };
    const context = { request, respond, reject } satisfies AppServerServerRequestContext;
    const callback = this.#options.onServerRequest;
    if (!callback) {
      reject({ code: -32601, message: `客户端未处理服务端请求: ${request.method}`, retryable: false });
      return;
    }
    void Promise.resolve(callback(context)).catch((error: unknown) => {
      reject({
        code: -32603,
        message: error instanceof Error ? error.message : String(error),
        retryable: false,
      });
    });
  }

  private sendResponse(id: JsonRpcRequestId, result: JsonValue | undefined, error: JsonRpcError | undefined): void {
    if (this.#socket?.readyState !== WebSocket.OPEN) return;
    this.#socket.send(JSON.stringify({
      jsonrpc: JSONRPC_VERSION,
      id,
      ...(error ? { error } : { result: result ?? null }),
    }));
  }

  private clearPending(pending: PendingRequest): void {
    if (pending.timeoutId !== null) window.clearTimeout(pending.timeoutId);
    if (pending.signal && pending.abortListener) {
      pending.signal.removeEventListener('abort', pending.abortListener);
    }
  }

  private rejectPending(error: AppServerError): void {
    for (const [key, pending] of this.#pending) {
      this.#pending.delete(key);
      this.clearPending(pending);
      pending.reject(error);
    }
  }

  private rejectOpen(error: AppServerError): void {
    this.#openReject?.(error);
    this.#openResolve = null;
    this.#openReject = null;
  }

  private setState(state: AppServerConnectionState): void {
    if (this.#state === state) return;
    this.#state = state;
    this.#options.onStateChange?.(state);
  }
}
