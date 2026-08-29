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
  /** 连接收到有效响应或通知时触发，用于上层更新传输活跃时间。 */
  onActivity?: () => void;
  onNotification?: (notification: ServerNotification) => void;
  onEvent?: (event: EventEnvelope) => void;
  onSnapshot?: (snapshot: EventStreamSnapshot) => void;
  onServerRequest?: (context: AppServerServerRequestContext) => void | Promise<void>;
}

interface PendingRequest {
  id: JsonRpcRequestId;
  method: AppServerRequestMethod;
  connection: ConnectionRef;
  resolve: (value: JsonValue) => void;
  reject: (error: AppServerError) => void;
  timeoutId: number | null;
  signal?: AbortSignal;
  abortListener?: () => void;
}

interface ConnectionRef {
  socket: WebSocket;
  generation: number;
}

interface OpenWait {
  connection: ConnectionRef;
  resolve: () => void;
  reject: (error: AppServerError) => void;
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
const APP_SERVER_HEARTBEAT_INTERVAL_MS = 10_000;
const APP_SERVER_HEARTBEAT_TIMEOUT_MS = 5_000;

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
  #socketGeneration = 0;
  #nextSocketGeneration = 0;
  #state: AppServerConnectionState = 'idle';
  #connectPromise: Promise<InitializeResult> | null = null;
  #openWait: OpenWait | null = null;
  #nextRequestId = 0;
  #reconnectAttempt = 0;
  #reconnectTimer: number | null = null;
  #closedByClient = false;
  #initializeResult: InitializeResult | null = null;
  #subscription: EventSubscribeParams | null;
  #lastSequence = 0;
  #heartbeatTimer: number | null = null;
  #heartbeatInFlight = false;

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
    if (this.#reconnectTimer !== null) {
      window.clearTimeout(this.#reconnectTimer);
      this.#reconnectTimer = null;
    }
    const connectPromise = this.connectOnce();
    const trackedPromise = connectPromise.finally(() => {
      if (this.#connectPromise === trackedPromise) {
        this.#connectPromise = null;
      }
    });
    this.#connectPromise = trackedPromise;
    return trackedPromise;
  }

  close(): void {
    this.#closedByClient = true;
    this.stopHeartbeat();
    if (this.#reconnectTimer !== null) {
      window.clearTimeout(this.#reconnectTimer);
      this.#reconnectTimer = null;
    }
    this.#reconnectAttempt = 0;
    const error = protocolError('App Server 连接已关闭', -32000);
    const connection = this.currentConnection();
    if (connection) {
      this.invalidateConnection(connection, error);
      try {
        connection.socket.close();
      } catch {
        // close 失败不影响本地状态已经收敛。
      }
    } else {
      this.rejectOpen(error);
      this.rejectPending(error);
      this.#initializeResult = null;
    }
    this.setState('closed');
  }

  async request<M extends AppServerRequestMethod>(
    method: M,
    params: AppServerRequestParams<M>,
    options: { timeoutMs?: number; signal?: AbortSignal } = {},
  ): Promise<AppServerRequestResult<M>> {
    await this.connect();
    const id = this.nextRequestId();
    const connection = this.currentConnection();
    if (!connection) {
      throw protocolError('App Server 尚未连接', -32000);
    }
    const result = await this.requestRaw(id, method, params, options, connection);
    return result;
  }

  private async connectOnce(): Promise<InitializeResult> {
    const generation = ++this.#nextSocketGeneration;
    const socket = new WebSocket(endpointForWebSocket(this.#options.endpoint));
    const connection: ConnectionRef = { socket, generation };
    this.#socket = socket;
    this.#socketGeneration = generation;
    this.#initializeResult = null;
    this.setState(this.#reconnectAttempt > 0 ? 'reconnecting' : 'connecting');
    const open = this.waitForOpen(connection);
    socket.addEventListener('open', () => this.handleOpen(connection));
    socket.addEventListener('message', (event) => this.handleMessage(connection, event.data));
    socket.addEventListener('error', () => this.handleError(connection));
    socket.addEventListener('close', () => this.handleClose(connection));

    try {
      await open;
      this.assertCurrentConnection(connection);
      const initialize = await this.requestRaw(
        this.nextRequestId(),
        'initialize',
        {
          clientInfo: this.#options.clientInfo,
          protocol: { major: 1, minor: 0 },
          capabilities: this.#options.capabilities ?? {},
        },
        { timeoutMs: this.#options.requestTimeoutMs },
        connection,
      );
      this.assertCurrentConnection(connection);
      this.#initializeResult = initialize;
      this.sendNotification('initialized', {}, connection);
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
          connection,
        );
      }
      this.assertCurrentConnection(connection);
      this.#reconnectAttempt = 0;
      this.setState('ready');
      this.startHeartbeat();
      return initialize;
    } catch (error) {
      const normalized = error instanceof Error && 'code' in error
        ? error as AppServerError
        : protocolError(error instanceof Error ? error.message : String(error));
      if (this.isCurrentConnection(connection)) {
        this.failConnection(connection, normalized);
      }
      throw normalized;
    }
  }

  private currentConnection(): ConnectionRef | null {
    return this.#socket
      ? { socket: this.#socket, generation: this.#socketGeneration }
      : null;
  }

  private isCurrentConnection(connection: ConnectionRef): boolean {
    return this.#socket === connection.socket && this.#socketGeneration === connection.generation;
  }

  private assertCurrentConnection(connection: ConnectionRef): void {
    if (!this.isCurrentConnection(connection) || connection.socket.readyState !== WebSocket.OPEN) {
      throw protocolError('App Server WebSocket 连接已失效', -32000);
    }
  }

  private waitForOpen(connection: ConnectionRef): Promise<void> {
    const open = new Promise<void>((resolve, reject) => {
      this.#openWait = { connection, resolve, reject };
    });
    open.catch(() => undefined);
    return open;
  }

  private handleOpen(connection: ConnectionRef): void {
    if (!this.isCurrentConnection(connection)) return;
    const wait = this.#openWait;
    if (!wait || !this.sameConnection(wait.connection, connection)) return;
    this.#openWait = null;
    wait.resolve();
  }

  private handleError(connection: ConnectionRef): void {
    if (!this.isCurrentConnection(connection)) return;
    this.failConnection(connection, protocolError('App Server WebSocket 连接失败', -32000));
  }

  private handleClose(connection: ConnectionRef): void {
    if (!this.isCurrentConnection(connection)) return;
    this.failConnection(connection, protocolError('App Server WebSocket 已断开', -32000));
  }

  private sameConnection(left: ConnectionRef, right: ConnectionRef): boolean {
    return left.socket === right.socket && left.generation === right.generation;
  }

  private failConnection(connection: ConnectionRef, error: AppServerError): void {
    if (!this.isCurrentConnection(connection)) return;
    this.stopHeartbeat();
    this.invalidateConnection(connection, error);
    if (this.#closedByClient) {
      this.setState('closed');
    } else if (this.#options.reconnect) {
      this.setState('reconnecting');
      this.scheduleReconnect();
    } else {
      this.setState('closed');
    }
    try {
      connection.socket.close();
    } catch {
      // 连接已经从客户端状态移除，底层 close 失败无需再次改变状态。
    }
  }

  private invalidateConnection(connection: ConnectionRef, error: AppServerError): void {
    if (!this.isCurrentConnection(connection)) return;
    this.#socket = null;
    this.#socketGeneration = 0;
    this.#initializeResult = null;
    this.rejectOpen(error, connection);
    this.rejectPending(error, connection);
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
      void this.connect().catch(() => undefined);
    }, delay);
  }

  private startHeartbeat(): void {
    this.stopHeartbeat();
    this.#heartbeatTimer = window.setInterval(() => {
      const connection = this.currentConnection();
      if (
        this.#state !== 'ready'
        || this.#heartbeatInFlight
        || !connection
        || connection.socket.readyState !== WebSocket.OPEN
      ) {
        return;
      }
      this.#heartbeatInFlight = true;
      const generation = connection.generation;
      void this.requestRaw(
        this.nextRequestId(),
        'ping',
        {},
        { timeoutMs: APP_SERVER_HEARTBEAT_TIMEOUT_MS },
        connection,
      )
        .then(() => {
          this.#options.onActivity?.();
        })
        .catch(() => {
          // WebSocket 可能仍显示 OPEN，但已经无法可靠收发数据。主动关闭
          // 交给统一 close/reconnect 链路处理，避免上层空闲检测误判为 SSE 断流。
          if (this.isCurrentConnection(connection) && !this.#closedByClient) {
            this.failConnection(connection, protocolError('App Server 心跳失败', -32000));
          }
        })
        .finally(() => {
          if (this.#socketGeneration === generation) {
            this.#heartbeatInFlight = false;
          }
        });
    }, APP_SERVER_HEARTBEAT_INTERVAL_MS);
  }

  private stopHeartbeat(): void {
    if (this.#heartbeatTimer !== null) {
      window.clearInterval(this.#heartbeatTimer);
      this.#heartbeatTimer = null;
    }
    this.#heartbeatInFlight = false;
  }

  private requestRaw<M extends AppServerRequestMethod>(
    id: JsonRpcRequestId,
    method: M,
    params: AppServerRequestParams<M>,
    options: { timeoutMs?: number; signal?: AbortSignal },
    connection: ConnectionRef,
  ): Promise<AppServerRequestResult<M>> {
    if (!knownRequestMethod(method)) throw protocolError(`未知 App Server 请求方法: ${method}`);
    assertRequestParams(method, params);
    if (!this.isCurrentConnection(connection) || connection.socket.readyState !== WebSocket.OPEN) {
      throw protocolError('App Server 尚未连接', -32000);
    }
    const timeoutMs = options.timeoutMs ?? this.#options.requestTimeoutMs;
    if (!Number.isInteger(timeoutMs) || timeoutMs < 100 || timeoutMs > 600_000) {
      throw invalidParams('requestTimeoutMs 必须在 100 到 600000 毫秒之间');
    }
    const key = requestIdKey(id);
    if (this.#pending.has(key)) {
      throw protocolError(`App Server 请求 ID 重复: ${String(id)}`);
    }
    return new Promise<JsonValue>((resolve, reject) => {
      const pending: PendingRequest = {
        id,
        method,
        connection,
        resolve,
        reject,
        timeoutId: null,
        signal: options.signal,
      };
      const abortListener = options.signal
        ? () => {
          if (!this.removePending(key, pending)) return;
          this.sendCancel(pending);
          pending.reject(protocolError('App Server 请求已取消', -32800));
        }
        : undefined;
      pending.abortListener = abortListener;
      this.#pending.set(key, pending);
      pending.timeoutId = window.setTimeout(() => {
        if (!this.removePending(key, pending)) return;
        this.sendCancel(pending);
        pending.reject(protocolError('App Server 请求超时', -32801));
      }, timeoutMs);
      if (options.signal?.aborted) {
        abortListener?.();
        return;
      }
      options.signal?.addEventListener('abort', abortListener!, { once: true });
      try {
        connection.socket.send(JSON.stringify({
          jsonrpc: JSONRPC_VERSION,
          id,
          method,
          requestTimeoutMs: timeoutMs,
          ...(params !== undefined ? { params } : {}),
        }));
      } catch (error) {
        if (this.removePending(key, pending)) {
          const sendError = protocolError(
            error instanceof Error ? error.message : 'App Server 请求发送失败',
            -32000,
          );
          pending.reject(sendError);
          this.failConnection(connection, sendError);
        }
      }
    }) as Promise<AppServerRequestResult<M>>;
  }

  private removePending(key: string, pending: PendingRequest): boolean {
    if (this.#pending.get(key) !== pending) return false;
    this.#pending.delete(key);
    this.clearPending(pending);
    return true;
  }

  private sendNotification<M extends AppServerNotificationMethod>(
    method: M,
    params: AppServerNotificationParams<M>,
    connection: ConnectionRef | null = this.currentConnection(),
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
    if (!connection || !this.isCurrentConnection(connection) || connection.socket.readyState !== WebSocket.OPEN) return;
    connection.socket.send(JSON.stringify({ jsonrpc: JSONRPC_VERSION, method, params }));
  }

  private sendCancel(pending: PendingRequest): void {
    try {
      this.sendNotification('$/cancelRequest', { id: pending.id }, pending.connection);
    } catch {
      // 请求已经结束；取消通知发送失败不能再次改变调用方结果。
    }
  }

  private nextRequestId(): string {
    this.#nextRequestId += 1;
    return `app-server-${Date.now()}-${this.#nextRequestId}`;
  }

  private handleMessage(connection: ConnectionRef, raw: unknown): void {
    if (!this.isCurrentConnection(connection)) return;
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
      this.handleResponse(connection, value as JsonRpcResponseLike);
      return;
    }
    if (isServerRequest(value)) {
      this.handleServerRequest(connection, value as JsonRpcRequestLike);
      return;
    }
    if (isNotification(value)) {
      this.handleNotification(connection, value as JsonRpcNotificationLike);
    }
  }

  private handleResponse(connection: ConnectionRef, value: JsonRpcResponseLike): void {
    if (typeof value.id !== 'string' && typeof value.id !== 'number') return;
    const pending = this.#pending.get(requestIdKey(value.id));
    if (!pending || !this.sameConnection(pending.connection, connection)) return;
    if (!this.removePending(requestIdKey(value.id), pending)) return;
    this.#options.onActivity?.();
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

  private handleNotification(connection: ConnectionRef, value: JsonRpcNotificationLike): void {
    if (!this.isCurrentConnection(connection)) return;
    const method = typeof value.method === 'string' ? value.method : '';
    const params = value.params;
    this.#options.onActivity?.();
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

  private handleServerRequest(connection: ConnectionRef, value: JsonRpcRequestLike): void {
    if (!this.isCurrentConnection(connection)) return;
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
      this.sendResponse(connection, request.id, undefined, errorFromThrown(error));
      return;
    }
    let settled = false;
    const respond = (result: JsonValue) => {
      if (settled) return;
      if (!isRecord(result) || typeof result.approved !== 'boolean') {
        settled = true;
        this.sendResponse(connection, request.id, undefined, {
          code: -32602,
          message: 'approval/request 响应必须包含 approved 布尔值',
          retryable: false,
        });
        return;
      }
      settled = true;
      this.sendResponse(connection, request.id, result, undefined);
    };
    const reject = (error: JsonRpcError) => {
      if (settled) return;
      settled = true;
      this.sendResponse(connection, request.id, undefined, error);
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

  private sendResponse(
    connection: ConnectionRef,
    id: JsonRpcRequestId,
    result: JsonValue | undefined,
    error: JsonRpcError | undefined,
  ): void {
    if (!this.isCurrentConnection(connection) || connection.socket.readyState !== WebSocket.OPEN) return;
    try {
      connection.socket.send(JSON.stringify({
        jsonrpc: JSONRPC_VERSION,
        id,
        ...(error ? { error } : { result: result ?? null }),
      }));
    } catch (sendError) {
      this.failConnection(connection, protocolError(
        sendError instanceof Error ? sendError.message : 'App Server 响应发送失败',
        -32000,
      ));
    }
  }

  private clearPending(pending: PendingRequest): void {
    if (pending.timeoutId !== null) window.clearTimeout(pending.timeoutId);
    if (pending.signal && pending.abortListener) {
      pending.signal.removeEventListener('abort', pending.abortListener);
    }
  }

  private rejectPending(error: AppServerError, connection?: ConnectionRef): void {
    for (const [key, pending] of this.#pending) {
      if (connection && !this.sameConnection(pending.connection, connection)) continue;
      if (this.removePending(key, pending)) {
        pending.reject(error);
      }
    }
  }

  private rejectOpen(error: AppServerError, connection?: ConnectionRef): void {
    const wait = this.#openWait;
    if (!wait || (connection && !this.sameConnection(wait.connection, connection))) return;
    this.#openWait = null;
    wait.reject(error);
  }

  private setState(state: AppServerConnectionState): void {
    if (this.#state === state) return;
    this.#state = state;
    this.#options.onStateChange?.(state);
  }
}
