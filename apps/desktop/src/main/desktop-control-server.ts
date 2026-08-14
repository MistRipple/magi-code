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
          this.emit({ type: "page_updated", payload: event.page });
        }
        break;
      case "page_crashed":
        if (this.#surfaceManager.isPrimary(event.binding)) {
          this.emit({ type: "page_crashed", payload: { binding: event.binding, diagnostic: event.reason } });
        }
        break;
      case "popup_blocked":
        this.emit({ type: "popup_blocked", payload: { tab_id: event.binding.tab_id } });
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
      case "cdp_event":
        break;
    }
  }

  async close(): Promise<void> {
    if (this.#heartbeat) clearInterval(this.#heartbeat);
    this.#heartbeat = null;
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
      const run = async () => {
        const response = await this.execute(request);
        if (websocket.readyState !== WebSocket.OPEN) return;
        websocket.send(JSON.stringify(response.envelope));
        if (response.binary) websocket.send(response.binary, { binary: true });
      };
      const queueKey = commandTabId(request.command);
      if (!queueKey) {
        void run();
        return;
      }
      const previous = this.#queues.get(queueKey) ?? Promise.resolve();
      const next = previous.then(run, run);
      this.#queues.set(queueKey, next);
      void next.finally(() => {
        if (this.#queues.get(queueKey) === next) this.#queues.delete(queueKey);
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

  private async executeCommand(command: BrowserHostCommand): Promise<{
    outcome: BrowserCommandOutcome;
    binary?: Buffer;
  }> {
    switch (command.type) {
      case "ping":
        return succeeded({ type: "pong", payload: { monotonic_millis: Math.floor(performance.now()) } });
      case "create_page":
      case "restore_page": {
        const binding = await this.#surfaceManager.materialize({
          windowId: this.#activeWindowId(),
          tabId: command.payload.tab_id,
          browserSessionId: command.payload.browser_session_id,
          initialUrl: command.payload.initial_url,
          navigationRevision: command.payload.navigation_revision,
          viewport: command.payload.logical_viewport,
        });
        const view = this.#surfaceManager.recordForBinding(binding);
        return succeeded({
          type: "page_state",
          payload: {
            tab_id: binding.tab_id,
            url: view.webContents.getURL() || "about:blank",
            origin: safeOrigin(view.webContents.getURL()),
            title: view.webContents.getTitle(),
            navigation_revision: binding.navigation_revision,
          },
        });
      }
      case "close_page":
        await this.#surfaceManager.closeTab(command.payload.tab_id);
        return succeeded({ type: "empty" });
      case "navigate": {
        const binding = requireBinding(this.#surfaceManager, command.payload.tab_id);
        const page = await this.#surfaceManager.navigate(binding, command.payload.navigation);
        return succeeded({ type: "page_state", payload: page });
      }
      case "set_logical_viewport": {
        const binding = requireBinding(this.#surfaceManager, command.payload.tab_id);
        await this.#surfaceManager.setViewport(binding, command.payload.viewport);
        return succeeded({ type: "empty" });
      }
      case "get_logical_viewport": {
        const binding = requireBinding(this.#surfaceManager, command.payload.tab_id);
        const state = this.#surfaceManager.viewportStateForSurface(binding.surface_id);
        if (!state) throw new Error("browser_surface_not_found");
        return succeeded({
          type: "json",
          payload: {
            value: {
              tab_id: binding.tab_id,
              viewport: state.viewport,
              surface_slot: state.slot,
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
        const binding = requireBinding(this.#surfaceManager, tabId);
        return this.#worker.execute(binding, command);
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
  return request;
}

function commandTabId(command: BrowserHostCommand): string | null {
  return "payload" in command && "tab_id" in command.payload
    ? String(command.payload.tab_id)
    : null;
}

function requireBinding(manager: BrowserSurfaceManager, tabId: string) {
  const binding = manager.bindingForTab(tabId);
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
