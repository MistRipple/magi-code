import { timingSafeEqual } from "node:crypto";
import { existsSync, unlinkSync } from "node:fs";
import { createServer, type IncomingMessage, type Server } from "node:http";
import type { Socket } from "node:net";
import {
  DESKTOP_BROWSER_PROTOCOL_VERSION,
  type BrowserCommandError,
  type BrowserCommandOutcome,
  type BrowserCommandResult,
  type BrowserHostCommand,
  type BrowserControlUpdate,
  type BrowserHostEvent,
  type BrowserHostEventEnvelope,
  type BrowserHostRequestEnvelope,
  type BrowserHostResponseEnvelope,
  type DesktopBrowserHandshake,
} from "@magi/desktop-browser-contracts";
import { WebSocket, WebSocketServer } from "ws";
import type { AutomationWorker } from "./automation-worker.js";
import type { BrowserSurfaceEvent, BrowserSurfaceManager } from "./browser-surface-manager.js";

const HEARTBEAT_INTERVAL_MS = 2_000;
const MAX_MESSAGE_BYTES = 2 * 1024 * 1024;

export class DesktopControlServer {
  readonly #socketPath: string;
  readonly #token: string;
  readonly #surfaceManager: BrowserSurfaceManager;
  readonly #worker: AutomationWorker;
  readonly #activeWindowId: () => string;
  readonly #handshake: () => DesktopBrowserHandshake;
  readonly #queues = new Map<string, Promise<void>>();
  readonly #active = new Map<string, AbortController>();
  #server: Server | null = null;
  #websocketServer: WebSocketServer | null = null;
  #client: WebSocket | null = null;
  #eventSequence = 0;
  #heartbeat: NodeJS.Timeout | null = null;

  constructor(input: {
    socketPath: string;
    token: string;
    surfaceManager: BrowserSurfaceManager;
    worker: AutomationWorker;
    activeWindowId: () => string;
    handshake: () => DesktopBrowserHandshake;
  }) {
    this.#socketPath = input.socketPath;
    this.#token = input.token;
    this.#surfaceManager = input.surfaceManager;
    this.#worker = input.worker;
    this.#activeWindowId = input.activeWindowId;
    this.#handshake = input.handshake;
  }

  async start(): Promise<void> {
    if (this.#server) return;
    if (process.platform !== "win32" && existsSync(this.#socketPath)) unlinkSync(this.#socketPath);
    const server = createServer((request, response) => {
      if (request.url !== "/health" || !authorized(request, this.#token)) {
        response.writeHead(404).end();
        return;
      }
      response.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify({
        status: "ready",
        protocol_version: DESKTOP_BROWSER_PROTOCOL_VERSION,
      }));
    });
    const websocketServer = new WebSocketServer({
      noServer: true,
      maxPayload: MAX_MESSAGE_BYTES,
      perMessageDeflate: false,
    });
    server.on("upgrade", (request: IncomingMessage, socket: Socket, head: Buffer) => {
      if (request.url !== "/control" || !authorized(request, this.#token)) {
        socket.write("HTTP/1.1 401 Unauthorized\r\nConnection: close\r\n\r\n");
        socket.destroy();
        return;
      }
      if (this.#client && this.#client.readyState !== WebSocket.CLOSED) {
        socket.write("HTTP/1.1 409 Conflict\r\nConnection: close\r\n\r\n");
        socket.destroy();
        return;
      }
      websocketServer.handleUpgrade(request, socket, head, (websocket) => {
        websocketServer.emit("connection", websocket, request);
      });
    });
    websocketServer.on("connection", (websocket) => this.acceptClient(websocket));
    await new Promise<void>((resolve, reject) => {
      server.once("error", reject);
      server.listen(this.#socketPath, () => {
        server.off("error", reject);
        resolve();
      });
    });
    this.#server = server;
    this.#websocketServer = websocketServer;
    this.#heartbeat = setInterval(() => {
      this.emit({ type: "heartbeat", payload: { monotonic_millis: Math.floor(performance.now()) } });
    }, HEARTBEAT_INTERVAL_MS);
    this.#heartbeat.unref();
  }

  handleSurfaceEvent(event: BrowserSurfaceEvent): void {
    this.#worker.forwardSurfaceEvent(event);
    switch (event.type) {
      case "primary_changed":
        this.emit({ type: "primary_surface_changed", payload: { binding: event.binding } });
        break;
      case "page_updated":
        if (this.#surfaceManager.isPrimary(event.binding)) {
          this.emit({
            type: "page_updated",
            payload: { binding: event.binding, page_state: event.page },
          });
        }
        break;
      case "page_crashed":
        if (this.#surfaceManager.isPrimary(event.binding)) {
          this.emit({ type: "page_crashed", payload: { binding: event.binding, diagnostic: event.reason } });
        }
        break;
      case "popup_blocked":
        this.emit({ type: "popup_blocked", payload: { binding: event.binding, url: event.url } });
        break;
      case "user_takeover":
        this.emit({ type: "user_takeover", payload: { binding: event.binding } });
        break;
      case "agent_cursor":
        this.emit({
          type: "agent_cursor",
          payload: {
            tab_id: event.binding.tab_id,
            visible: event.visible,
            x: event.x,
            y: event.y,
            action: event.action,
          },
        });
        break;
      case "loading_changed":
        this.emit({ type: "loading_changed", payload: { binding: event.binding, loading: event.loading } });
        break;
      case "page_failed":
        this.emit({ type: "page_failed", payload: { binding: event.binding, reason: event.reason } });
        break;
      case "download":
        this.emit({
          type: "download",
          payload: {
            tab_id: event.binding.tab_id,
            suggested_filename: event.suggestedFilename,
            state: event.state,
            ...(event.byteLength !== undefined ? { byte_length: event.byteLength } : {}),
            ...(event.error ? { error: event.error } : {}),
          },
        });
        break;
      case "cdp_event":
        break;
    }
  }

  async close(): Promise<void> {
    if (this.#heartbeat) clearInterval(this.#heartbeat);
    this.#heartbeat = null;
    for (const controller of this.#active.values()) controller.abort();
    this.#active.clear();
    this.#client?.terminate();
    this.#client = null;
    this.#websocketServer?.close();
    this.#websocketServer = null;
    const server = this.#server;
    this.#server = null;
    if (server) {
      server.closeAllConnections();
      await new Promise<void>((resolve) => server.close(() => resolve()));
    }
    if (process.platform !== "win32" && existsSync(this.#socketPath)) unlinkSync(this.#socketPath);
  }

  private acceptClient(websocket: WebSocket): void {
    this.#client = websocket;
    this.emit({ type: "ready", payload: this.#handshake() });
    // daemon 可能在 Electron 已经创建并提升 Browser Surface 之后才建立
    // 控制连接。连接握手只发送 ready 会丢失既有 Primary，导致 daemon
    // 认为逻辑 Tab 没有 Surface，随后截图、标记和自动化都会拿到空绑定。
    // 连接建立后立即重放当前 Primary，使远端 Authority 与 Main 的真实
    // Surface 注册状态收敛到同一代次。
    for (const binding of this.#surfaceManager.bindings()) {
      if (this.#surfaceManager.isPrimary(binding)) {
        this.emit({ type: "primary_surface_changed", payload: { binding } });
      }
    }
    websocket.on("message", (data, binary) => {
      if (binary) {
        websocket.close(1003, "binary requests are not supported");
        return;
      }
      let request: BrowserHostRequestEnvelope;
      try {
        request = parseRequest(data.toString());
      } catch (cause) {
        websocket.send(JSON.stringify(failedResponse("invalid-request", normalizeError(cause))));
        return;
      }
      if (request.command.type === "cancel") {
        this.#active.get(request.command.payload.request_id)?.abort();
        return;
      }
      const controller = new AbortController();
      this.#active.set(request.request_id, controller);
      const run = async () => {
        try {
          await this.executeWithCancellation(request, controller.signal, websocket);
        } finally {
          if (this.#active.get(request.request_id) === controller) {
            this.#active.delete(request.request_id);
          }
        }
      };
      const queueKey = commandTabId(request.command);
      if (!queueKey) {
        void run();
        return;
      }
      const previous = this.#queues.get(queueKey) ?? Promise.resolve();
      const next = previous.then(run, run);
      // Queue 的尾部必须始终是 fulfilled Promise。这样某条命令因窗口销毁
      // 或发送失败而 reject 时，后续同 Tab 命令仍能按顺序执行，不会把错误
      // Promise 永久留在队列里；这里不做重试，只收敛队列生命周期。
      const tail = next.then(
        () => undefined,
        (error) => {
          console.error("[DesktopControlServer] Browser command queue failed", {
            tabId: queueKey,
            error: error instanceof Error ? error.message : String(error),
          });
        },
      );
      this.#queues.set(queueKey, tail);
      void tail.then(() => {
        if (this.#queues.get(queueKey) === tail) this.#queues.delete(queueKey);
      });
    });
    websocket.on("close", () => {
      if (this.#client === websocket) this.#client = null;
    });
  }

  private async execute(request: BrowserHostRequestEnvelope): Promise<{
    envelope: BrowserHostResponseEnvelope;
    binary?: Buffer;
  }> {
    try {
      const executed = await this.executeCommand(request.command);
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
    request: BrowserHostRequestEnvelope,
    signal: AbortSignal,
    websocket: WebSocket,
  ): Promise<void> {
    if (signal.aborted) {
      if (websocket.readyState === WebSocket.OPEN) {
        websocket.send(JSON.stringify(cancelledResponse(request.request_id)));
      }
      return;
    }
    const operation = this.execute(request);
    const cancellation = waitForAbort(signal);
    const result = await Promise.race([
      operation.then((response) => ({ kind: "completed" as const, response })),
      cancellation.then(() => ({ kind: "cancelled" as const })),
    ]);
    if (result.kind === "cancelled") {
      if (websocket.readyState === WebSocket.OPEN) {
        websocket.send(JSON.stringify(indeterminateResponse(request.request_id)));
      }
      // Preserve the per-tab queue until the real Chromium/Worker operation
      // settles. The response is already indeterminate, so the next command
      // cannot race the unknown side effect on the same page.
      await operation;
      return;
    }
    if (websocket.readyState !== WebSocket.OPEN) return;
    websocket.send(JSON.stringify(result.response.envelope));
    if (result.response.binary) websocket.send(result.response.binary, { binary: true });
  }

  private async executeCommand(command: BrowserHostCommand): Promise<{
    outcome: BrowserCommandOutcome;
    binary?: Buffer;
  }> {
    switch (command.type) {
      case "ping":
        return succeeded({ type: "pong", payload: { monotonic_millis: Math.floor(performance.now()) } });
      case "create_page":
      case "restore_page": {
        const current = this.#surfaceManager.primaryBindingForTab(command.payload.tab_id);
        // 已物化的逻辑 Tab 始终复用 Primary Surface。只有首个物理 Surface
        // 尚不存在时，才以当前窗口作为初始放置入口；后续命令不再读取焦点窗口。
        const windowId = current?.window_id ?? this.#activeWindowId();
        const binding = await this.#surfaceManager.materialize({
          windowId,
          tabId: command.payload.tab_id,
          browserSessionId: command.payload.browser_session_id,
          initialUrl: command.payload.initial_url,
          navigationRevision: command.payload.navigation_revision,
          viewport: command.payload.logical_viewport,
          // Host RestorePage/CreatePage 只负责把真实 WebContentsView 物化
          // 并返回当前页面状态，不能把网络导航放在右栏激活的关键路径上。
          // Chromium 页面继续在原生视图中加载，页面加载状态由 Surface 事件
          // 单独上报，避免用户看到“正在连接浏览器”而不是实际加载过程。
          awaitPageLoad: false,
        });
        const contents = this.#surfaceManager.recordForBinding(binding);
        return succeeded({
          type: "page_state",
          payload: {
            tab_id: binding.tab_id,
            url: contents.getURL() || "about:blank",
            origin: safeOrigin(contents.getURL()),
            title: contents.getTitle(),
            navigation_revision: binding.navigation_revision,
          },
        });
      }
      case "close_page":
        await this.#surfaceManager.closeTab(command.payload.tab_id);
        return succeeded({ type: "empty" });
      case "navigate": {
        const binding = requirePrimaryBinding(this.#surfaceManager, command.payload.tab_id);
        const page = await this.#surfaceManager.navigate(binding, command.payload.navigation);
        return succeeded({ type: "page_state", payload: page });
      }
      case "set_logical_viewport": {
        const binding = requirePrimaryBinding(this.#surfaceManager, command.payload.tab_id);
        await this.#surfaceManager.setViewport(binding, command.payload.viewport);
        return succeeded({ type: "empty" });
      }
      case "get_logical_viewport": {
        const binding = requirePrimaryBinding(this.#surfaceManager, command.payload.tab_id);
        const state = this.#surfaceManager.viewportStateForSurface(binding.surface_id);
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
      case "update_control":
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
        const binding = requirePrimaryBinding(this.#surfaceManager, tabId);
        const executed = await this.#worker.execute(binding, command);
        // 交互命令在 Worker 内可能由多个 CDP 输入事件组成。动作中的
        // keyDown/click 可能已经触发导航，因此不能把 Worker 发送前的
        // binding 当作动作结果的页面状态。由 Main 在动作完成后读取同一
        // WebContents 的最新地址、标题和 navigation revision，统一满足
        // Rust 工具层的 PageState 契约。
        if (executed.outcome.status === "succeeded" && isPageStateInteraction(command)) {
          const currentBinding = requirePrimaryBinding(this.#surfaceManager, tabId);
          const contents = this.#surfaceManager.recordForBinding(currentBinding);
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

  private emit(event: BrowserHostEvent): void {
    const client = this.#client;
    if (!client || client.readyState !== WebSocket.OPEN) return;
    const envelope: BrowserHostEventEnvelope = {
      protocol_version: DESKTOP_BROWSER_PROTOCOL_VERSION,
      sequence: ++this.#eventSequence,
      event,
    };
    client.send(JSON.stringify(envelope));
  }
}

function parseRequest(value: string): BrowserHostRequestEnvelope {
  if (Buffer.byteLength(value, "utf8") > MAX_MESSAGE_BYTES) throw new Error("browser_command_too_large");
  const request = JSON.parse(value) as BrowserHostRequestEnvelope;
  if (!request.request_id || typeof request.request_id !== "string") throw new Error("browser_protocol_invalid");
  if (
    request.protocol_version?.major !== DESKTOP_BROWSER_PROTOCOL_VERSION.major
    || request.protocol_version?.minor !== DESKTOP_BROWSER_PROTOCOL_VERSION.minor
  ) {
    throw new Error("browser_protocol_incompatible");
  }
  if (!request.command || typeof request.command.type !== "string") throw new Error("browser_protocol_invalid");
  if (
    request.command.type === "cancel"
    && (!request.command.payload || typeof request.command.payload.request_id !== "string")
  ) {
    throw new Error("browser_protocol_invalid");
  }
  return request;
}

function commandTabId(command: BrowserHostCommand): string | null {
  return "payload" in command && "tab_id" in command.payload
    ? String(command.payload.tab_id)
    : null;
}

function isPageStateInteraction(command: BrowserHostCommand): boolean {
  return command.type === "click"
    || command.type === "type"
    || command.type === "press"
    || command.type === "scroll";
}

function requirePrimaryBinding(manager: BrowserSurfaceManager, tabId: string) {
  const binding = manager.primaryBindingForTab(tabId);
  if (!binding) throw new Error("browser_surface_not_found");
  return binding;
}

function succeeded(result: BrowserCommandResult): { outcome: BrowserCommandOutcome } {
  return { outcome: { status: "succeeded", payload: result } };
}

function failedResponse(requestId: string, error: BrowserCommandError): BrowserHostResponseEnvelope {
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
    signal.addEventListener("abort", () => resolve(), { once: true });
  });
}

function normalizeError(cause: unknown): BrowserCommandError {
  const source = cause instanceof Error ? cause : new Error(String(cause));
  const code = source.message.split(":", 1)[0];
  return {
    code: code?.startsWith("browser_") ? code : "browser_desktop_control_failed",
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
  return received.length === expected.length && timingSafeEqual(received, expected);
}

function safeOrigin(value: string): string | null {
  try {
    const origin = new URL(value).origin;
    return origin === "null" ? null : origin;
  } catch {
    return null;
  }
}
