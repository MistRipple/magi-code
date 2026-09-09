import { timingSafeEqual } from "node:crypto";
import { existsSync, unlinkSync } from "node:fs";
import { createServer, type IncomingMessage, type Server } from "node:http";
import type { Socket } from "node:net";
import {
  DESKTOP_BROWSER_PROTOCOL_VERSION,
  normalizeOptionalDomNodeId,
  type BrowserAgentCursorAction,
  type BrowserCommandError,
  type BrowserCommandOutcome,
  type BrowserCommandResult,
  type BrowserHostCommand,
  type BrowserControlUpdate,
  type BrowserHostEvent,
  type BrowserHostEventEnvelope,
  type BrowserHostRequestEnvelope,
  type BrowserHostResponseEnvelope,
  type BrowserNodeSelection,
  type BrowserSurfaceBinding,
  type BrowserSurfaceIdentity,
  type DesktopBrowserHandshake,
} from "@magi/desktop-browser-contracts";
import {
  parseBrowserHostEventEnvelope,
  parseBrowserHostRequest,
  parseBrowserHostResponse,
} from "@magi/desktop-browser-contracts/validation";
import { WebSocket, WebSocketServer } from "ws";
import type { AutomationWorker } from "./automation-worker.js";
import type {
  BrowserSurfaceActivationInput,
  BrowserSurfaceEvent,
  BrowserSurfaceManager,
} from "./browser-surface-manager.js";

const HEARTBEAT_INTERVAL_MS = 2_000;
const MAX_MESSAGE_BYTES = 2 * 1024 * 1024;

interface DesktopControlConnection {
  websocket: WebSocket;
  active: Set<DesktopControlCommand>;
  closed: boolean;
  isAlive: boolean;
}

interface DesktopControlCommand {
  request: BrowserHostRequestEnvelope;
  controller: AbortController;
  connection: DesktopControlConnection;
  resourceKey: string | null;
  state: "pending" | "running" | "settled";
  cancellationRequested: boolean;
}

interface ResourceQueue {
  pending: DesktopControlCommand[];
  running: DesktopControlCommand | null;
}

type EnsureBrowserSurface = (
  input: BrowserSurfaceActivationInput,
) => Promise<unknown>;

interface AnnotationProjection {
  revision: number;
  annotations: unknown[];
}

export class DesktopControlServer {
  readonly #socketPath: string;
  readonly #token: string;
  readonly #surfaceManager: BrowserSurfaceManager;
  readonly #worker: AutomationWorker;
  readonly #waitForActiveWindow: (signal?: AbortSignal) => Promise<string>;
  readonly #ensureBrowserSurface: EnsureBrowserSurface;
  readonly #handshake: () => DesktopBrowserHandshake;
  readonly #onConnectionState: ((connected: boolean) => void) | undefined;
  readonly #queues = new Map<string, ResourceQueue>();
  readonly #active = new Map<string, DesktopControlCommand>();
  readonly #connections = new Set<DesktopControlConnection>();
  // Authority 中的标记是唯一持久化事实；这里仅缓存其最后一次已接收投影，
  // 用于 Chromium 文档导航或 Worker 重启后重放，不把标记状态写回 Desktop。
  readonly #annotationProjections = new Map<string, AnnotationProjection>();
  readonly #annotationProjectionLanes = new Map<string, Promise<void>>();
  // 标记是页面上的最新状态同步，不是需要占用 Tab 资源直到完成的交互命令。
  // 每个 Tab 只保留一个当前投影控制器；新状态或普通浏览器命令到来时，
  // 旧投影立即失效，迟到的 Chromium 结果不能阻塞或覆盖当前页面。
  readonly #annotationProjectionControllers = new Map<
    string,
    AbortController
  >();
  readonly #appliedAnnotationProjections = new Map<
    string,
    {
      revision: number;
      bindingKey: string;
    }
  >();
  // primary_changed 是物理 Chromium Surface 的生命周期边界。命令必须
  // 等待 Worker 完成同一代 binding 的 ACK，不能只依赖 Renderer 已收到
  // primary_surface_changed 事件。
  readonly #surfaceRebinds = new Map<string, Promise<void>>();
  #server: Server | null = null;
  #websocketServer: WebSocketServer | null = null;
  #client: WebSocket | null = null;
  #eventSequence = 0;
  #heartbeat: NodeJS.Timeout | null = null;
  #lifecycle: Promise<void> = Promise.resolve();

  constructor(input: {
    socketPath: string;
    token: string;
    surfaceManager: BrowserSurfaceManager;
    worker: AutomationWorker;
    waitForActiveWindow: (signal?: AbortSignal) => Promise<string>;
    ensureBrowserSurface: EnsureBrowserSurface;
    handshake: () => DesktopBrowserHandshake;
    onConnectionState?: (connected: boolean) => void;
  }) {
    this.#socketPath = input.socketPath;
    this.#token = input.token;
    this.#surfaceManager = input.surfaceManager;
    this.#worker = input.worker;
    this.#waitForActiveWindow = input.waitForActiveWindow;
    this.#ensureBrowserSurface = input.ensureBrowserSurface;
    this.#handshake = input.handshake;
    this.#onConnectionState = input.onConnectionState;
  }

  async start(): Promise<void> {
    return this.enqueueLifecycle(() => this.startInternal());
  }

  private async startInternal(): Promise<void> {
    if (this.#server) return;
    if (process.platform !== "win32" && existsSync(this.#socketPath))
      unlinkSync(this.#socketPath);
    const server = createServer((request, response) => {
      if (request.url !== "/health" || !authorized(request, this.#token)) {
        response.writeHead(404).end();
        return;
      }
      response.writeHead(200, { "content-type": "application/json" }).end(
        JSON.stringify({
          status: "ready",
          protocol_version: DESKTOP_BROWSER_PROTOCOL_VERSION,
        }),
      );
    });
    const websocketServer = new WebSocketServer({
      noServer: true,
      maxPayload: MAX_MESSAGE_BYTES,
      perMessageDeflate: false,
    });
    server.on(
      "upgrade",
      (request: IncomingMessage, socket: Socket, head: Buffer) => {
        if (request.url !== "/control" || !authorized(request, this.#token)) {
          socket.write(
            "HTTP/1.1 401 Unauthorized\r\nConnection: close\r\n\r\n",
          );
          socket.destroy();
          return;
        }
        if (this.#client && this.#client.readyState !== WebSocket.CLOSED) {
          // terminate() changes the old socket to CLOSING before its asynchronous
          // close event arrives. Treat that state as already released so a daemon
          // reconnect during Electron restart is not rejected by a stale client.
          if (this.#client.readyState === WebSocket.CLOSING) {
            const connection = this.connectionFor(this.#client);
            if (connection) this.releaseConnection(connection);
            this.#client = null;
          } else {
            socket.write("HTTP/1.1 409 Conflict\r\nConnection: close\r\n\r\n");
            socket.destroy();
            return;
          }
        }
        websocketServer.handleUpgrade(request, socket, head, (websocket) => {
          websocketServer.emit("connection", websocket, request);
        });
      },
    );
    websocketServer.on("connection", (websocket) =>
      this.acceptClient(websocket),
    );
    this.#server = server;
    this.#websocketServer = websocketServer;
    try {
      await new Promise<void>((resolve, reject) => {
        server.once("error", reject);
        server.listen(this.#socketPath, () => {
          server.off("error", reject);
          resolve();
        });
      });
      this.#heartbeat = setInterval(
        () => this.heartbeat(),
        HEARTBEAT_INTERVAL_MS,
      );
      this.#heartbeat.unref();
    } catch (cause) {
      this.#server = null;
      this.#websocketServer = null;
      await this.closeWebSocketServer(websocketServer);
      await closeHttpServer(server);
      if (process.platform !== "win32" && existsSync(this.#socketPath))
        unlinkSync(this.#socketPath);
      throw cause;
    }
  }

  handleSurfaceEvent(event: BrowserSurfaceEvent): void {
    const forwarded = this.#worker.forwardSurfaceEvent(event);
    if (event.type === "primary_changed") {
      this.#surfaceRebinds.set(event.binding.tab_id, forwarded);
      forwarded.then(
        () => this.clearSurfaceRebind(event.binding.tab_id, forwarded),
        () => this.clearSurfaceRebind(event.binding.tab_id, forwarded),
      );
      // Surface 事件来源是 Chromium 主线程回调，不能让没有等待者的
      // 异步重绑失败变成未处理 Promise；真正的浏览器命令仍会观察到
      // 原始失败并停止执行。
      void forwarded.catch(() => undefined);
    }
    // 一个逻辑 Tab 可以在多个窗口拥有 WebContents，但 Host 只接受其
    // 当前 Primary 的页面事实。否则后台窗口的旧导航、加载和弹窗事件
    // 会覆盖当前 Surface，表现为地址、状态和工具结果来回跳变。
    if (
      event.type !== "primary_changed" &&
      event.type !== "cdp_event" &&
      !this.#surfaceManager.isPrimary(event.binding)
    ) {
      return;
    }
    switch (event.type) {
      case "primary_changed":
        this.emit({
          type: "primary_surface_changed",
          payload: { binding: event.binding },
        });
        this.scheduleAnnotationProjection(event.binding.tab_id);
        break;
      case "page_updated":
        if (this.#surfaceManager.isPrimary(event.binding)) {
          this.#appliedAnnotationProjections.delete(event.binding.tab_id);
          this.emit({
            type: "page_updated",
            payload: { binding: event.binding, page_state: event.page },
          });
          this.scheduleAnnotationProjection(event.binding.tab_id);
        }
        break;
      case "page_crashed":
        if (this.#surfaceManager.isPrimary(event.binding)) {
          this.emit({
            type: "page_crashed",
            payload: { binding: event.binding, diagnostic: event.reason },
          });
        }
        break;
      case "popup_blocked":
        this.emit({
          type: "popup_blocked",
          payload: { binding: event.binding, url: event.url },
        });
        break;
      case "user_takeover":
        this.emit({
          type: "user_takeover",
          payload: { binding: event.binding },
        });
        break;
      case "agent_cursor":
        this.emit({
          type: "agent_cursor",
          payload: {
            tab_id: event.binding.tab_id,
            visible: event.visible,
            x: event.x,
            y: event.y,
            action: agentCursorAction(event.action),
          },
        });
        break;
      case "loading_changed":
        this.emit({
          type: "loading_changed",
          payload: { binding: event.binding, loading: event.loading },
        });
        break;
      case "page_failed":
        this.emit({
          type: "page_failed",
          payload: { binding: event.binding, reason: event.reason },
        });
        break;
      case "download":
        this.emit({
          type: "download",
          payload: {
            tab_id: event.binding.tab_id,
            download_id: event.downloadId,
            suggested_filename: event.suggestedFilename,
            state: event.state,
            received_bytes: event.receivedBytes,
            total_bytes: event.totalBytes,
            ...(event.byteLength !== undefined
              ? { byte_length: event.byteLength }
              : {}),
            ...(event.error ? { error: event.error } : {}),
          },
        });
        break;
      case "cdp_event":
        break;
      case "node_inspected": {
        // 节点选择必须和 page/surface 的 Primary 身份一起校验。即使
        // Main 已经在异步采集期间切换了 Primary，也不能把旧 Surface 的
        // DOM 上下文发送给 Host，避免 LLM 收到另一页的节点。
        if (!this.#surfaceManager.isPrimary(event.binding)) break;
        const selection = nodeSelectionFromEvent(event);
        if (selection)
          this.emit({ type: "node_selection", payload: selection });
        break;
      }
    }
  }

  handleSurfaceContentReady(binding: BrowserSurfaceBinding): void {
    if (this.#surfaceManager.isPrimary(binding)) {
      this.scheduleAnnotationProjection(binding.tab_id);
    }
  }

  handleSurfaceDocumentReady(binding: BrowserSurfaceBinding): void {
    if (this.#surfaceManager.isPrimary(binding)) {
      this.scheduleAnnotationProjection(binding.tab_id);
    }
  }

  async close(): Promise<void> {
    return this.enqueueLifecycle(() => this.closeInternal());
  }

  private async closeInternal(): Promise<void> {
    if (this.#heartbeat) clearInterval(this.#heartbeat);
    this.#heartbeat = null;
    for (const controller of this.#annotationProjectionControllers.values())
      controller.abort();
    this.#annotationProjectionControllers.clear();
    for (const connection of [...this.#connections]) {
      this.releaseConnection(connection);
      connection.websocket.terminate();
    }
    this.#client = null;
    const websocketServer = this.#websocketServer;
    this.#websocketServer = null;
    if (websocketServer) await this.closeWebSocketServer(websocketServer);
    const server = this.#server;
    this.#server = null;
    if (server) await closeHttpServer(server);
    if (process.platform !== "win32" && existsSync(this.#socketPath))
      unlinkSync(this.#socketPath);
  }

  private enqueueLifecycle<T>(operation: () => Promise<T>): Promise<T> {
    const next = this.#lifecycle.then(operation, operation);
    this.#lifecycle = next.then(
      () => undefined,
      () => undefined,
    );
    return next;
  }

  private heartbeat(): void {
    for (const connection of [...this.#connections]) {
      if (connection.closed) continue;
      if (!connection.isAlive) {
        this.releaseConnection(connection);
        connection.websocket.terminate();
        continue;
      }
      connection.isAlive = false;
      if (connection.websocket.readyState === WebSocket.OPEN) {
        try {
          connection.websocket.ping();
        } catch {
          this.releaseConnection(connection);
          connection.websocket.terminate();
        }
      }
    }
    this.emit({
      type: "heartbeat",
      payload: { monotonic_millis: Math.floor(performance.now()) },
    });
  }

  private closeWebSocketServer(
    websocketServer: WebSocketServer,
  ): Promise<void> {
    return new Promise((resolve) => {
      websocketServer.close(() => resolve());
    });
  }

  /** Worker 恢复后调度所有仍在当前 Primary Surface 上的标记投影。 */
  async replayCachedAnnotations(): Promise<void> {
    // 标记重放属于最终一致性的 UI 同步，不能成为 Worker ready 或普通浏览器
    // 命令的前置条件。具体投影失败会由 scheduleAnnotationProjection 记录，
    // 下一次 Surface 生命周期事件仍可按 Authority 快照重试。
    for (const tabId of this.#annotationProjections.keys())
      this.scheduleAnnotationProjection(tabId);
  }

  private acceptClient(websocket: WebSocket): void {
    const connection: DesktopControlConnection = {
      websocket,
      active: new Set(),
      closed: false,
      isAlive: true,
    };
    this.#connections.add(connection);
    this.#client = websocket;
    websocket.on("pong", () => {
      if (!connection.closed) connection.isAlive = true;
    });
    console.info("[DesktopControlServer] Browser Host connected", {
      readyState: websocket.readyState,
    });
    this.emit({ type: "ready", payload: this.#handshake() });
    // daemon 可能在 Electron 已经创建并提升 Browser Surface 之后才建立
    // 控制连接。连接握手只发送 ready 会丢失既有 Primary，导致 daemon
    // 认为逻辑 Tab 没有 Surface，随后截图、标记和自动化都会拿到空绑定。
    // 连接建立后立即重放当前 Primary，使远端 Authority 与 Main 的真实
    // Surface 注册状态收敛到同一代次。
    for (const binding of this.#surfaceManager.bindings()) {
      if (this.#surfaceManager.isPrimary(binding)) {
        this.emit({ type: "primary_surface_changed", payload: { binding } });
        this.scheduleAnnotationProjection(binding.tab_id);
      }
    }
    // 只有 WebSocket 已经被 Desktop Control Server 接受，Host 才真正具备
    // 接收浏览器命令的能力。daemon 的 HTTP 状态表示注册完成，不代表这条
    // 控制通道已经建立，不能用它提前触发布局恢复。
    this.#onConnectionState?.(true);
    websocket.on("message", (data, binary) => {
      if (binary) {
        websocket.close(1003, "binary requests are not supported");
        return;
      }
      if (connection.closed) return;
      let request: BrowserHostRequestEnvelope;
      try {
        request = parseRequest(data.toString());
      } catch (cause) {
        console.error("[DesktopControlServer] Invalid Browser Host request", {
          error: cause instanceof Error ? cause.message : String(cause),
        });
        this.sendEnvelope(
          connection,
          failedResponse("invalid-request", normalizeError(cause)),
        );
        return;
      }
      console.info("[DesktopControlServer] Browser command received", {
        requestId: request.request_id,
        command: request.command.type,
        tabId: commandTabId(request.command),
      });
      if (request.command.type === "cancel") {
        const active = this.#active.get(request.command.payload.request_id);
        if (active?.connection === connection) this.cancelCommand(active);
        return;
      }
      if (this.#active.has(request.request_id)) {
        this.sendEnvelope(
          connection,
          failedResponse(request.request_id, {
            code: "browser_request_id_in_use",
            message: "浏览器请求 ID 已在使用中",
            recoverable: true,
            side_effect_started: false,
            diagnostic: null,
          }),
        );
        return;
      }
      const controller = new AbortController();
      const command: DesktopControlCommand = {
        request,
        controller,
        connection,
        resourceKey: commandTabId(request.command),
        state: "pending",
        cancellationRequested: false,
      };
      this.#active.set(request.request_id, command);
      connection.active.add(command);
      this.enqueueCommand(command);
    });
    websocket.on("error", (error) => {
      console.error("[DesktopControlServer] Browser Host websocket error", {
        error: error instanceof Error ? error.message : String(error),
      });
      // error 不保证后续 close 事件及时到达。连接失效必须在这里完成
      // 同一套释放流程，避免旧连接继续占用资源队列。
      this.releaseConnection(connection);
    });
    websocket.on("close", (code, reason) => {
      console.warn("[DesktopControlServer] Browser Host websocket closed", {
        code,
        reason: reason.toString(),
      });
      this.releaseConnection(connection);
    });
  }

  private enqueueCommand(command: DesktopControlCommand): void {
    // 停止导航是当前 Tab 的唯一高优先级中断命令。它必须与正在等待
    // Chromium 网络事件的 navigate 并行进入同一个 SurfaceManager，不能
    // 排在普通资源队列末尾，否则工具栏的 Stop 永远无法真正停止页面。
    if (isNavigationInterruptCommand(command.request.command)) {
      void this.runCommand(command);
      return;
    }
    if (!command.resourceKey) {
      void this.runCommand(command);
      return;
    }
    const queue = this.#queues.get(command.resourceKey) ?? {
      pending: [],
      running: null,
    };
    queue.pending.push(command);
    this.#queues.set(command.resourceKey, queue);
    this.pumpQueue(command.resourceKey, queue);
  }

  private pumpQueue(resourceKey: string, queue: ResourceQueue): void {
    if (queue.running) return;
    while (queue.pending.length > 0) {
      const command = queue.pending.shift();
      if (!command) return;
      if (command.connection.closed || command.cancellationRequested) {
        this.finishCommand(command);
        continue;
      }
      queue.running = command;
      void this.runCommand(command).finally(() => {
        if (queue.running === command) queue.running = null;
        if (this.#queues.get(resourceKey) === queue)
          this.pumpQueue(resourceKey, queue);
      });
      return;
    }
    if (!queue.running && this.#queues.get(resourceKey) === queue)
      this.#queues.delete(resourceKey);
  }

  private async runCommand(command: DesktopControlCommand): Promise<void> {
    if (command.state === "settled") return;
    if (command.connection.closed || command.cancellationRequested) {
      this.finishCommand(command);
      return;
    }
    command.state = "running";
    const startedAt = performance.now();
    try {
      await this.executeWithCancellation(command);
      console.info("[DesktopControlServer] Browser command settled", {
        requestId: command.request.request_id,
        command: command.request.command.type,
        durationMs: Math.round(performance.now() - startedAt),
      });
    } catch (cause) {
      if (!command.connection.closed && !command.cancellationRequested) {
        this.sendEnvelope(
          command.connection,
          failedResponse(command.request.request_id, normalizeError(cause)),
        );
      }
    } finally {
      this.finishCommand(command);
    }
  }

  private cancelCommand(command: DesktopControlCommand): void {
    if (command.state === "settled") return;
    command.cancellationRequested = true;
    command.controller.abort();
    if (command.state !== "pending") return;
    this.removePendingCommand(command);
    this.sendEnvelope(
      command.connection,
      cancelledResponse(command.request.request_id),
    );
    this.finishCommand(command);
  }

  private removePendingCommand(command: DesktopControlCommand): void {
    const resourceKey = command.resourceKey;
    if (!resourceKey) return;
    const queue = this.#queues.get(resourceKey);
    if (!queue) return;
    const index = queue.pending.indexOf(command);
    if (index >= 0) queue.pending.splice(index, 1);
    if (!queue.running) this.pumpQueue(resourceKey, queue);
  }

  private finishCommand(command: DesktopControlCommand): void {
    if (command.state === "settled") return;
    command.state = "settled";
    if (this.#active.get(command.request.request_id) === command) {
      this.#active.delete(command.request.request_id);
    }
    command.connection.active.delete(command);
  }

  private releaseConnection(connection: DesktopControlConnection): void {
    if (connection.closed) return;
    connection.closed = true;
    for (const command of [...connection.active]) {
      command.cancellationRequested = true;
      command.controller.abort();
      // A pending command has not touched Chromium and can leave the queue
      // immediately. A running command remains the queue owner until its
      // underlying operation settles, even though this connection is gone.
      if (command.state === "pending") this.finishCommand(command);
    }
    this.#connections.delete(connection);
    if (this.#client === connection.websocket) {
      this.#client = null;
      this.#surfaceManager.releaseDisconnectedHostControl();
      this.#onConnectionState?.(false);
    }
    for (const [resourceKey, queue] of this.#queues) {
      queue.pending = queue.pending.filter((command) => {
        if (command.connection !== connection) return true;
        this.finishCommand(command);
        return false;
      });
      if (!queue.running && queue.pending.length === 0) {
        this.#queues.delete(resourceKey);
      } else if (!queue.running) {
        this.pumpQueue(resourceKey, queue);
      }
    }
  }

  private connectionFor(websocket: WebSocket): DesktopControlConnection | null {
    for (const connection of this.#connections) {
      if (connection.websocket === websocket) return connection;
    }
    return null;
  }

  private sendEnvelope(
    connection: DesktopControlConnection,
    envelope: BrowserHostResponseEnvelope,
  ): void {
    if (connection.closed || connection.websocket.readyState !== WebSocket.OPEN)
      return;
    try {
      const validated = parseBrowserHostResponse(envelope);
      assertProtocolVersion(validated.protocol_version);
      connection.websocket.send(JSON.stringify(validated));
    } catch (cause) {
      console.error(
        "[DesktopControlServer] Browser response validation/send failed",
        {
          error: cause instanceof Error ? cause.message : String(cause),
        },
      );
      this.releaseConnection(connection);
    }
  }

  private sendBinary(
    connection: DesktopControlConnection,
    payload: Buffer,
  ): void {
    if (connection.closed || connection.websocket.readyState !== WebSocket.OPEN)
      return;
    try {
      connection.websocket.send(payload, { binary: true });
    } catch (cause) {
      console.error(
        "[DesktopControlServer] Browser binary response send failed",
        {
          error: cause instanceof Error ? cause.message : String(cause),
        },
      );
      this.releaseConnection(connection);
    }
  }

  private async execute(
    request: BrowserHostRequestEnvelope,
    signal?: AbortSignal,
  ): Promise<{
    envelope: BrowserHostResponseEnvelope;
    binary?: Buffer;
  }> {
    try {
      const executed = await this.executeCommand(request.command, signal);
      return {
        envelope: {
          request_id: request.request_id,
          protocol_version: DESKTOP_BROWSER_PROTOCOL_VERSION,
          outcome: executed.outcome,
        },
        ...(executed.binary ? { binary: executed.binary } : {}),
      };
    } catch (cause) {
      return {
        envelope: failedResponse(request.request_id, normalizeError(cause)),
      };
    }
  }

  private async executeWithCancellation(
    command: DesktopControlCommand,
  ): Promise<void> {
    const { request, controller, connection } = command;
    const { signal } = controller;
    if (signal.aborted) {
      return;
    }
    const operation = this.execute(request, signal);
    const cancellation = waitForAbort(signal);
    const result = await Promise.race([
      operation.then((response) => ({ kind: "completed" as const, response })),
      cancellation.then(() => ({ kind: "cancelled" as const })),
    ]);
    if (result.kind === "cancelled") {
      this.sendEnvelope(connection, indeterminateResponse(request.request_id));
      // 普通命令的资源仍由底层操作持有，直到它真正收口。否则旧点击、输入
      // 或导航尚未结束时，新连接会同时操作同一个 WebContents。只有标记
      // 投影通过独立的最新状态链绕开这里，不会进入普通命令资源队列。
      await operation.catch(() => undefined);
      return;
    }
    // Cancellation may win immediately after the operation promise resolves.
    // Re-check both the signal and the command ownership before writing; this
    // is the response fence that prevents a late success from an old request.
    if (signal.aborted || command.cancellationRequested || connection.closed) {
      if (command.cancellationRequested) {
        this.sendEnvelope(
          connection,
          indeterminateResponse(request.request_id),
        );
      }
      await operation.catch(() => undefined);
      return;
    }
    this.sendEnvelope(connection, result.response.envelope);
    if (result.response.binary)
      this.sendBinary(connection, result.response.binary);
  }

  private async executeCommand(
    command: BrowserHostCommand,
    signal?: AbortSignal,
  ): Promise<{
    outcome: BrowserCommandOutcome;
    binary?: Buffer;
  }> {
    const tabId = commandTabId(command);
    if (tabId && command.type !== "set_annotations") {
      // 普通浏览器命令优先于页面标记重放。标记事实已经由 Authority
      // 持久化，当前页面换代或用户开始操作时，旧投影没有继续占用资源的
      // 价值；重新绑定后会按最新快照再次投影。
      this.cancelAnnotationProjection(tabId);
    }
    switch (command.type) {
      case "ping":
        return succeeded({
          type: "pong",
          payload: { monotonic_millis: Math.floor(performance.now()) },
        });
      case "create_page":
      case "restore_page": {
        const current = this.#surfaceManager.primaryBindingForTab(
          command.payload.tab_id,
        );
        // 创建/恢复是资源物化阶段，不能等待 Renderer 内容槽。新建 Tab 的
        // Renderer 记录只有在这个响应返回后才会出现；如果这里调用
        // ensureBrowserSurface，就会形成“等待内容槽 -> 内容槽等待创建响应”
        // 的环路，并让新 Tab 无故卡住一整个内容槽超时周期。
        const windowId =
          current?.window_id ?? (await this.#waitForActiveWindow(signal));
        const binding = await this.#surfaceManager.materialize({
          windowId,
          tabId: command.payload.tab_id,
          browserSessionId: command.payload.browser_session_id,
          initialUrl: command.payload.initial_url,
          navigationRevision: command.payload.navigation_revision,
          viewport: command.payload.logical_viewport,
          awaitPageLoad: false,
          reannouncePrimary: true,
        });
        // create_page 只负责登记逻辑 Browser Tab。真正的 guest 由右栏
        // <webview> 随后注册；此阶段不能等待或伪造 WebContents binding。
        const contents = binding
          ? this.#surfaceManager.recordForBinding(binding)
          : null;
        const url = contents?.getURL() || command.payload.initial_url;
        return succeeded({
          type: "page_state",
          payload: {
            tab_id: command.payload.tab_id,
            url: url || "about:blank",
            origin: safeOrigin(url),
            title: contents?.getTitle() || "",
            // materialize 可能在绑定真实 guest 后立即启动初始导航并推进
            // revision。响应必须返回物理 Surface 的当前代次，否则 Authority
            // 会把旧代次发布给 Renderer，下一次 webview 注册必然被拒绝。
            navigation_revision:
              binding?.navigation_revision ??
              command.payload.navigation_revision,
          },
        });
      }
      case "ensure_surface": {
        const binding = await this.requireRenderablePrimaryBinding(
          command.payload.tab_id,
        );
        return succeeded({
          type: "surface_binding",
          payload: binding,
        });
      }
      case "close_page":
        await this.#surfaceManager.closeTab(command.payload.tab_id);
        return succeeded({ type: "empty" });
      case "navigate": {
        const binding = await this.requireRenderablePrimaryBinding(
          command.payload.tab_id,
        );
        const page = await this.#surfaceManager.navigate(
          binding,
          command.payload.navigation,
        );
        return succeeded({ type: "page_state", payload: page });
      }
      case "stop_navigation": {
        const binding = await this.requireRenderablePrimaryBinding(
          command.payload.tab_id,
        );
        const page = await this.#surfaceManager.stopNavigation(binding);
        return succeeded({ type: "page_state", payload: page });
      }
      case "set_logical_viewport": {
        const binding = await this.requireRenderablePrimaryBinding(
          command.payload.tab_id,
        );
        await this.#surfaceManager.setViewport(
          binding,
          command.payload.viewport,
        );
        return succeeded({ type: "empty" });
      }
      case "get_logical_viewport": {
        const binding = await this.requireRenderablePrimaryBinding(
          command.payload.tab_id,
        );
        const state = this.#surfaceManager.viewportStateForSurface(
          binding.surface_id,
        );
        if (!state) throw new Error("browser_surface_not_found");
        return succeeded({
          type: "json",
          payload: {
            value: {
              tab_id: binding.tab_id,
              viewport: state.viewport,
            },
          },
        });
      }
      case "set_annotations": {
        this.recordAnnotationProjection(
          command.payload.tab_id,
          command.payload.annotations,
        );
        this.scheduleAnnotationProjection(command.payload.tab_id);
        // set_annotations 的成功语义是 Desktop 已接受最新 Authority 快照。
        // 页面投影是可重放的最终一致性同步，不能让标记请求反向阻塞导航、
        // 刷新、输入或 Worker ready。
        return succeeded({ type: "empty" });
      }
      case "inspect_start":
      case "inspect_stop": {
        const binding = await this.requireRenderablePrimaryBindingForIdentity(
          command.payload,
        );
        if (command.type === "inspect_start") {
          await this.#surfaceManager.startInspect(binding);
        } else {
          await this.#surfaceManager.stopInspect(binding);
        }
        return succeeded({ type: "empty" });
      }
      case "update_control":
        await this.requireRenderablePrimaryBinding(command.payload.tab_id);
        await this.#surfaceManager.updateControl(
          command.payload.tab_id,
          command.payload.surface_id,
          command.payload.control as BrowserControlUpdate,
        );
        return succeeded({ type: "empty" });
      case "shutdown":
        return succeeded({ type: "empty" });
      default: {
        const tabId = commandTabId(command);
        if (!tabId) throw new Error("browser_tab_id_missing");
        const binding = await this.requireRenderablePrimaryBinding(tabId);
        const executed = await this.#worker.execute(binding, command, signal);
        // 交互命令在 Worker 内可能由多个 CDP 输入事件组成。动作中的
        // keyDown/click 可能已经触发导航，因此不能把 Worker 发送前的
        // binding 当作动作结果的页面状态。由 Main 在动作完成后读取同一
        // WebContents 的最新地址、标题和 navigation revision，统一满足
        // Rust 工具层的 PageState 契约。
        if (
          executed.outcome.status === "succeeded" &&
          isPageStateInteraction(command)
        ) {
          const currentBinding =
            await this.requireRenderablePrimaryBinding(tabId);
          const contents =
            this.#surfaceManager.recordForBinding(currentBinding);
          return {
            outcome: {
              status: "succeeded",
              payload: {
                type: "page_state",
                payload: {
                  tab_id: currentBinding.tab_id,
                  url: contents.getURL() || "about:blank",
                  origin: safeOrigin(contents.getURL()),
                  title: contents.getTitle() || "",
                  navigation_revision: currentBinding.navigation_revision,
                },
              },
            },
            ...(executed.binary ? { binary: executed.binary } : {}),
          };
        }
        return executed;
      }
    }
  }

  private recordAnnotationProjection(
    tabId: string,
    annotations: unknown[],
  ): void {
    this.cancelAnnotationProjection(tabId);
    const previous = this.#annotationProjections.get(tabId);
    this.#annotationProjections.set(tabId, {
      revision: (previous?.revision ?? 0) + 1,
      // 控制协议已完成结构化校验。复制数组防止调用方在异步重放期间改变
      // 这一轮权威快照的成员顺序；对象会由 Worker 再次 JSON 序列化。
      annotations: [...annotations],
    });
  }

  private scheduleAnnotationProjection(tabId: string): void {
    if (!this.#annotationProjections.has(tabId)) return;
    void this.enqueueAnnotationProjection(tabId)
      .then((result) => {
        if (result.outcome.status !== "succeeded") {
          console.warn("[DesktopControlServer] 浏览器标记投影未能重放", {
            tabId,
            code:
              result.outcome.status === "failed" ||
              result.outcome.status === "indeterminate"
                ? result.outcome.payload.code
                : result.outcome.status,
          });
        }
      })
      .catch((cause) => {
        // 导航与窗口切换会让旧 Surface 失效。下一次 Primary/page_updated
        // 事件会以同一 Authority 快照重放，无需把暂态失败写回持久化状态。
        console.debug(
          "[DesktopControlServer] 浏览器标记投影等待下一轮 Surface 事件",
          {
            tabId,
            error: cause instanceof Error ? cause.message : String(cause),
          },
        );
      });
  }

  private enqueueAnnotationProjection(
    tabId: string,
  ): Promise<{ outcome: BrowserCommandOutcome; binary?: Buffer }> {
    const previous =
      this.#annotationProjectionLanes.get(tabId) ?? Promise.resolve();
    const controller = new AbortController();
    this.#annotationProjectionControllers.set(tabId, controller);
    const run = previous
      .catch(() => undefined)
      .then(async () => {
        if (
          this.#annotationProjectionControllers.get(tabId) !== controller ||
          controller.signal.aborted
        ) {
          return succeeded({ type: "empty" });
        }
        let latestResult: {
          outcome: BrowserCommandOutcome;
          binary?: Buffer;
        } | null = null;
        while (true) {
          if (controller.signal.aborted) return succeeded({ type: "empty" });
          const projection = this.#annotationProjections.get(tabId);
          if (!projection)
            throw new Error("browser_annotation_projection_missing");
          // 每次真正执行前重新读取 Primary，不能把导航前的 binding 用于
          // 新文档，否则旧 CDP 执行上下文会把标记错误写入下一页或直接超时。
          const binding = requirePrimaryBinding(this.#surfaceManager, tabId);
          const bindingKey = annotationBindingKey(binding);
          const applied = this.#appliedAnnotationProjections.get(tabId);
          if (
            applied?.revision === projection.revision &&
            applied.bindingKey === bindingKey
          )
            return latestResult ?? succeeded({ type: "empty" });
          // 标记事实已经在 Authority 持久化。后台 Surface 没有 Chromium
          // compositor viewport，先保留投影快照，等它真正绑定内容槽时由
          // handleSurfaceContentReady 触发重放，不把暂态不可见误报成失败。
          if (!this.#surfaceManager.isContentSlotBoundBinding(binding)) {
            return succeeded({ type: "empty" });
          }
          const operation = this.#worker.execute(
            binding,
            {
              type: "set_annotations",
              payload: {
                tab_id: tabId,
                annotations: projection.annotations,
              },
            },
            controller.signal,
          );
          // 即使底层 Chromium Promise 无法取消，投影控制器也必须能够
          // 立即结束这条“最新状态同步”链。底层 Promise 由 Worker 的
          // cancellation/代次围栏继续收口，绝不在这里等待它。
          void operation.catch(() => undefined);
          let result: { outcome: BrowserCommandOutcome; binary?: Buffer };
          try {
            result = await Promise.race([
              operation,
              waitForAbort(controller.signal).then(() => {
                throw new Error("browser_command_cancelled");
              }),
            ]);
          } catch (cause) {
            if (controller.signal.aborted) return succeeded({ type: "empty" });
            throw cause;
          }
          if (controller.signal.aborted) return succeeded({ type: "empty" });
          latestResult = result;
          if (result.outcome.status === "succeeded") {
            this.#appliedAnnotationProjections.set(tabId, {
              revision: projection.revision,
              bindingKey,
            });
          }
          if (
            this.#annotationProjections.get(tabId)?.revision ===
            projection.revision
          ) {
            return result;
          }
        }
      });
    const settled = run.then(
      () => undefined,
      () => undefined,
    );
    this.#annotationProjectionLanes.set(tabId, settled);
    void settled.finally(() => {
      if (this.#annotationProjectionLanes.get(tabId) === settled) {
        this.#annotationProjectionLanes.delete(tabId);
      }
      if (this.#annotationProjectionControllers.get(tabId) === controller) {
        this.#annotationProjectionControllers.delete(tabId);
      }
    });
    return run;
  }

  private cancelAnnotationProjection(tabId: string): void {
    this.#annotationProjectionControllers.get(tabId)?.abort();
    this.#annotationProjectionControllers.delete(tabId);
  }

  private async requireRenderablePrimaryBinding(
    tabId: string,
  ): Promise<BrowserSurfaceBinding> {
    const activation = this.#surfaceManager.activationInputForTab(tabId);
    if (!activation) throw new Error("browser_surface_not_found");
    await this.#ensureBrowserSurface(activation);
    await this.waitForSurfaceRebind(tabId);
    const binding = requirePrimaryBinding(this.#surfaceManager, tabId);
    if (!this.#surfaceManager.isContentSlotBoundBinding(binding)) {
      throw new Error("browser_surface_content_slot_unavailable");
    }
    return binding;
  }

  private async waitForSurfaceRebind(tabId: string): Promise<void> {
    await this.#surfaceRebinds.get(tabId);
  }

  private clearSurfaceRebind(tabId: string, rebind: Promise<void>): void {
    if (this.#surfaceRebinds.get(tabId) === rebind) {
      this.#surfaceRebinds.delete(tabId);
    }
  }

  private async requireRenderablePrimaryBindingForIdentity(
    identity: BrowserSurfaceIdentity,
  ): Promise<BrowserSurfaceBinding> {
    const binding = await this.requireRenderablePrimaryBinding(identity.tab_id);
    if (
      binding.surface_id !== identity.surface_id ||
      binding.navigation_revision !== identity.navigation_revision
    ) {
      throw new Error("browser_surface_stale");
    }
    return binding;
  }

  private emit(event: BrowserHostEvent): void {
    const client = this.#client;
    if (!client || client.readyState !== WebSocket.OPEN) return;
    const envelope: BrowserHostEventEnvelope = {
      protocol_version: DESKTOP_BROWSER_PROTOCOL_VERSION,
      sequence: ++this.#eventSequence,
      event,
    };
    try {
      const validated = parseBrowserHostEventEnvelope(envelope);
      assertProtocolVersion(validated.protocol_version);
      if (validated.event.type === "ready") {
        assertProtocolVersion(validated.event.payload.protocol_version);
      }
      client.send(JSON.stringify(validated));
    } catch (cause) {
      const connection = this.connectionFor(client);
      // 事件协议失败后不能只在 Host 侧丢弃连接。否则 daemon 仍保有一个
      // 已打开但再也不会收到心跳的 socket，最终表现为延迟的 heartbeat_timeout。
      try {
        client.close(1011, "browser host event protocol failure");
      } finally {
        if (connection) this.releaseConnection(connection);
      }
      console.error("[DesktopControlServer] Browser event send failed", {
        error: cause instanceof Error ? cause.message : String(cause),
      });
    }
  }
}

function parseRequest(value: string): BrowserHostRequestEnvelope {
  if (Buffer.byteLength(value, "utf8") > MAX_MESSAGE_BYTES)
    throw new Error("browser_command_too_large");
  let decoded: unknown;
  try {
    decoded = JSON.parse(value);
  } catch {
    throw new Error("browser_protocol_invalid:invalid_json");
  }
  const request = parseBrowserHostRequest(decoded);
  assertProtocolVersion(request.protocol_version);
  return request;
}

function assertProtocolVersion(version: {
  major: number;
  minor: number;
}): void {
  if (
    version.major !== DESKTOP_BROWSER_PROTOCOL_VERSION.major ||
    version.minor !== DESKTOP_BROWSER_PROTOCOL_VERSION.minor
  ) {
    throw new Error("browser_protocol_incompatible");
  }
}

function commandTabId(command: BrowserHostCommand): string | null {
  return "payload" in command && "tab_id" in command.payload
    ? String(command.payload.tab_id)
    : null;
}

function isNavigationInterruptCommand(command: BrowserHostCommand): boolean {
  return command.type === "stop_navigation";
}

function isPageStateInteraction(command: BrowserHostCommand): boolean {
  return (
    command.type === "click" ||
    command.type === "type" ||
    command.type === "press" ||
    command.type === "scroll"
  );
}

function requirePrimaryBinding(manager: BrowserSurfaceManager, tabId: string) {
  const binding = manager.primaryBindingForTab(tabId);
  if (!binding) throw new Error("browser_surface_not_found");
  return binding;
}

function annotationBindingKey(binding: BrowserSurfaceBinding): string {
  return [
    binding.desktop_epoch,
    binding.window_id,
    binding.surface_id,
    binding.surface_revision,
    binding.web_contents_id,
    binding.target_id,
    binding.browser_context_id,
    binding.navigation_revision,
  ].join("\u001f");
}

function nodeSelectionFromEvent(
  event: Extract<BrowserSurfaceEvent, { type: "node_inspected" }>,
): BrowserNodeSelection | null {
  const { node } = event;
  const backendDomNodeId = normalizeOptionalDomNodeId(node.backend_node_id);
  const domNodeId = normalizeOptionalDomNodeId(node.node_id);
  const domNodeIdIsValid =
    node.node_id === null ||
    node.node_id === undefined ||
    node.node_id === 0 ||
    domNodeId !== null;
  // Chromium 节点可能没有可解析的 DOM id、frame 或 box model（例如文本
  // 节点、Shadow DOM 边界或导航竞态）。这些字段在协议中明确可空，不能
  // 因为缺少展示信息而丢弃合法的节点选择；只有身份和节点名称必须完整。
  if (
    !node.browser_session_id.trim() ||
    backendDomNodeId === null ||
    !domNodeIdIsValid
  ) {
    console.warn("[DesktopControlServer] 忽略无效的 Chromium 节点选择身份", {
      surfaceId: event.binding.surface_id,
      hasBrowserSessionId: Boolean(node.browser_session_id.trim()),
      backendNodeId: node.backend_node_id,
    });
    return null;
  }
  const textExcerpt =
    node.node_value.trim() ||
    node.attributes["aria-label"]?.trim() ||
    node.attributes.title?.trim() ||
    "";
  return {
    tab_id: event.binding.tab_id,
    surface_id: event.binding.surface_id,
    navigation_revision: event.binding.navigation_revision,
    browser_session_id: node.browser_session_id,
    url: node.page_url,
    title: node.page_title,
    frame_id: node.frame_id,
    backend_dom_node_id: backendDomNodeId,
    dom_node_id: domNodeId,
    node_name: node.node_name,
    attributes: node.attributes,
    text_excerpt: textExcerpt,
    outer_html: node.outer_html,
    outer_html_truncated: node.outer_html_truncated,
    aria_role: node.attributes.role?.trim() || null,
    aria_name: node.attributes["aria-label"]?.trim() || null,
    bounds: node.bounds,
  };
}

function succeeded(result: BrowserCommandResult): {
  outcome: BrowserCommandOutcome;
} {
  return { outcome: { status: "succeeded", payload: result } };
}

function failedResponse(
  requestId: string,
  error: BrowserCommandError,
): BrowserHostResponseEnvelope {
  return {
    request_id: requestId,
    protocol_version: DESKTOP_BROWSER_PROTOCOL_VERSION,
    outcome: { status: "failed", payload: error },
  };
}

function indeterminateResponse(requestId: string): BrowserHostResponseEnvelope {
  return {
    request_id: requestId,
    protocol_version: DESKTOP_BROWSER_PROTOCOL_VERSION,
    outcome: {
      status: "indeterminate",
      payload: {
        code: "browser_command_cancelled",
        message: "浏览器命令已取消，但 Chromium 操作可能已经产生副作用",
        recoverable: false,
        side_effect_started: true,
        diagnostic: null,
      },
    },
  };
}

function cancelledResponse(requestId: string): BrowserHostResponseEnvelope {
  return {
    request_id: requestId,
    protocol_version: DESKTOP_BROWSER_PROTOCOL_VERSION,
    outcome: { status: "cancelled" },
  };
}

function waitForAbort(signal: AbortSignal): Promise<void> {
  if (signal.aborted) return Promise.resolve();
  return new Promise((resolve) => {
    const onAbort = () => {
      signal.removeEventListener("abort", onAbort);
      resolve();
    };
    signal.addEventListener("abort", onAbort, { once: true });
  });
}

function closeHttpServer(server: Server): Promise<void> {
  if (!server.listening) return Promise.resolve();
  server.closeAllConnections();
  return new Promise((resolve) => {
    server.close(() => resolve());
  });
}

function normalizeError(cause: unknown): BrowserCommandError {
  const source = cause instanceof Error ? cause : new Error(String(cause));
  const code = source.message.split(":", 1)[0];
  return {
    code:
      code?.startsWith("browser_") || code?.startsWith("desktop_")
        ? code
        : "browser_desktop_control_failed",
    message: source.message,
    recoverable: true,
    side_effect_started: false,
    diagnostic: source.stack ?? null,
  };
}

function authorized(request: IncomingMessage, token: string): boolean {
  const header = request.headers.authorization;
  if (!header?.startsWith("Bearer ")) return false;
  const received = Buffer.from(header.slice("Bearer ".length), "utf8");
  const expected = Buffer.from(token, "utf8");
  return (
    received.length === expected.length && timingSafeEqual(received, expected)
  );
}

function safeOrigin(value: string): string | null {
  try {
    const origin = new URL(value).origin;
    return origin === "null" ? null : origin;
  } catch {
    return null;
  }
}

function agentCursorAction(
  value: string | null,
): BrowserAgentCursorAction | null {
  switch (value) {
    case "move":
    case "click":
    case "drag":
    case "type":
    case "scroll":
      return value;
    default:
      return null;
  }
}
