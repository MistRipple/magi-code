import { timingSafeEqual } from "node:crypto";
import { createServer, type IncomingMessage } from "node:http";
import { resolve } from "node:path";
import type { Socket } from "node:net";
import { WebSocket, WebSocketServer } from "ws";
import { BrowserHost, type BrowserHostConfig } from "./host";
import { ProtocolFailure } from "./control";
import type {
  CommandError,
  EventEnvelope,
  HostCommand,
  HostEvent,
  RequestEnvelope,
  ResponseEnvelope,
} from "./protocol";
import { PROTOCOL_VERSION } from "./protocol";

const MAX_COMMAND_BYTES = 2 * 1024 * 1024;
const HEARTBEAT_INTERVAL_MILLIS = 2_000;
// 控制响应和实时画面共享 Host WebSocket。画面只能丢帧，不能让图片
// 在发送缓冲中排队，否则后续导航、快照和交互响应会被图片阻塞。
const MAX_SCREENCAST_BUFFERED_BYTES = 16 * 1024 * 1024;

export interface BrowserHostServerConfig extends BrowserHostConfig {
  bindHost: string;
  port: number;
  authToken: string;
  daemonProcessId?: number;
}

export async function startBrowserHostServer(
  config: BrowserHostServerConfig,
): Promise<{ close(): Promise<void>; port: number }> {
  if (config.authToken.length < 32) {
    throw new Error("MAGI_BROWSER_HOST_TOKEN must contain at least 32 characters");
  }
  let client: WebSocket | undefined;
  let eventSequence = 0;
  const commandQueues = new Map<string, Promise<void>>();
  let shuttingDown = false;
  const transport = {
    emit(event: HostEvent) {
      if (!client || client.readyState !== WebSocket.OPEN) return;
      const envelope: EventEnvelope = {
        protocol_version: PROTOCOL_VERSION,
        sequence: ++eventSequence,
        event,
      };
      client.send(JSON.stringify(envelope));
    },
    emitScreencast(event: HostEvent, payload: Buffer) {
      if (!client || client.readyState !== WebSocket.OPEN) return false;
      if (client.bufferedAmount > MAX_SCREENCAST_BUFFERED_BYTES) return false;
      const envelope: EventEnvelope = {
        protocol_version: PROTOCOL_VERSION,
        sequence: ++eventSequence,
        event,
      };
      // 元数据和二进制必须作为一对发送；丢弃时整帧丢弃，避免 Rust 端
      // 的二进制队列与事件队列错位。
      client.send(JSON.stringify(envelope));
      client.send(payload, { binary: true });
      return true;
    },
  };
  const host = new BrowserHost(config, transport);
  const handshake = await host.start();

  const server = createServer((request, response) => {
    if (request.url !== "/health") {
      response.writeHead(404).end();
      return;
    }
    if (!authorized(request, config.authToken)) {
      response.writeHead(401).end();
      return;
    }
    response
      .writeHead(200, { "content-type": "application/json" })
      .end(
        JSON.stringify({
          status: "ready",
          protocol_version: PROTOCOL_VERSION,
          runtime_version: config.runtimeVersion,
        }),
      );
  });
  const websocketServer = new WebSocketServer({
    noServer: true,
    maxPayload: MAX_COMMAND_BYTES,
    perMessageDeflate: false,
  });

  server.on(
    "upgrade",
    (request: IncomingMessage, socket: Socket, head: Buffer) => {
      if (request.url !== "/control" || !authorized(request, config.authToken)) {
        socket.write("HTTP/1.1 401 Unauthorized\r\nConnection: close\r\n\r\n");
        socket.destroy();
        return;
      }
      if (client && client.readyState !== WebSocket.CLOSED) {
        socket.write("HTTP/1.1 409 Conflict\r\nConnection: close\r\n\r\n");
        socket.destroy();
        return;
      }
      websocketServer.handleUpgrade(request, socket, head, (websocket) => {
        websocketServer.emit("connection", websocket, request);
      });
    },
  );

  websocketServer.on("connection", (websocket) => {
    client = websocket;
    transport.emit({ type: "ready", payload: handshake });
    websocket.on("message", (data, isBinary) => {
      if (isBinary) {
        websocket.close(1003, "binary client messages are not supported");
        return;
      }
      const encodedRequest = data.toString();
      let request: RequestEnvelope;
      try {
        request = parseRequest(encodedRequest);
      } catch (error) {
        websocket.send(
          JSON.stringify(
            failedResponse(requestIdFromInvalidRequest(encodedRequest), normalizeError(error, false)),
          ),
        );
        return;
      }

      const run = async () => {
        try {
          const executed = await host.execute(request.command);
          const response: ResponseEnvelope = {
            request_id: request.request_id,
            protocol_version: PROTOCOL_VERSION,
            outcome: { status: "succeeded", payload: executed.result },
          };
          websocket.send(JSON.stringify(response));
          if (executed.binary) {
            websocket.send(executed.binary, { binary: true });
          }
          if (request.command.type === "shutdown") {
            shuttingDown = true;
            setImmediate(() => websocket.close(1000, "browser Host shutdown"));
          }
        } catch (error) {
          const normalized = normalizeError(error, false);
          const response: ResponseEnvelope = {
            request_id: request.request_id,
            protocol_version: PROTOCOL_VERSION,
            outcome: normalized.side_effect_started
              ? { status: "indeterminate", payload: normalized }
              : { status: "failed", payload: normalized },
          };
          websocket.send(JSON.stringify(response));
        }
      };

      if (request.command.type === "update_control") {
        void run();
      } else if (request.command.type === "shutdown") {
        void Promise.allSettled(Array.from(commandQueues.values())).then(run);
      } else {
        const queueKey = commandQueueKey(request.command);
        if (!queueKey) {
          void run();
          return;
        }
        const previous = commandQueues.get(queueKey) ?? Promise.resolve();
        const next = previous.then(run, run);
        commandQueues.set(queueKey, next);
        const releaseQueue = () => {
          if (commandQueues.get(queueKey) === next) commandQueues.delete(queueKey);
        };
        void next.then(releaseQueue, releaseQueue);
      }
    });
    websocket.on("close", () => {
      if (client === websocket) client = undefined;
      if (shuttingDown) {
        void close();
      }
    });
  });

  const heartbeat = setInterval(() => {
    transport.emit({
      type: "heartbeat",
      payload: { monotonic_millis: Math.floor(performance.now()) },
    });
  }, HEARTBEAT_INTERVAL_MILLIS);
  heartbeat.unref();

  await new Promise<void>((accept, reject) => {
    server.once("error", reject);
    server.listen(config.port, config.bindHost, () => {
      server.off("error", reject);
      accept();
    });
  });
  const address = server.address();
  if (!address || typeof address === "string") {
    throw new Error("browser Host did not bind a TCP address");
  }

  let closed = false;
  async function close(): Promise<void> {
    if (closed) return;
    closed = true;
    clearInterval(heartbeat);
    if (client && client.readyState !== WebSocket.CLOSED) {
      client.terminate();
    }
    websocketServer.close();
    await host.close().catch(() => undefined);
    server.closeAllConnections();
    await new Promise<void>((accept) => server.close(() => accept()));
  }

  return { close, port: address.port };
}

function commandQueueKey(command: HostCommand): string | null {
  switch (command.type) {
    case "create_page":
    case "restore_page":
    case "set_viewport":
    case "set_logical_viewport":
    case "close_page":
    case "navigate":
    case "snapshot":
    case "click":
    case "type":
    case "press":
    case "scroll":
    case "screenshot":
    case "hit_test":
    case "start_screencast":
    case "stop_screencast":
    case "user_input":
      return command.payload.tab_id;
    case "devtools":
      // 对话框处理必须绕过标签页动作队列。点击或导航可能一直等待 Chromium
      // 收到接受或取消指令；若把恢复命令排在该动作之后，会让标签页死锁。
      return command.payload.operation === "dialog" ? null : command.payload.tab_id;
    case "ping":
    case "update_control":
    case "shutdown":
      return null;
  }
}

function parseRequest(value: string): RequestEnvelope {
  if (Buffer.byteLength(value, "utf8") > MAX_COMMAND_BYTES) {
    throw new ProtocolFailure(
      "browser_command_too_large",
      "browser Host command exceeds the size limit",
      false,
      false,
    );
  }
  const request = JSON.parse(value) as Partial<RequestEnvelope>;
  if (!request.request_id || typeof request.request_id !== "string") {
    throw new ProtocolFailure(
      "browser_protocol_invalid",
      "request_id is required",
      false,
      false,
    );
  }
  if (
    request.protocol_version?.major !== PROTOCOL_VERSION.major ||
    request.protocol_version.minor !== PROTOCOL_VERSION.minor
  ) {
    throw new ProtocolFailure(
      "browser_host_protocol_incompatible",
      "browser Host protocol version does not match",
      false,
      false,
    );
  }
  if (!isHostCommand(request.command)) {
    throw new ProtocolFailure(
      "browser_protocol_invalid",
      "command type is missing or unsupported",
      false,
      false,
    );
  }
  return request as RequestEnvelope;
}

function isHostCommand(value: unknown): value is HostCommand {
  if (!value || typeof value !== "object") return false;
  const type = (value as { type?: unknown }).type;
  return (
    typeof type === "string" &&
    [
      "ping",
      "create_page",
      "restore_page",
      "set_viewport",
      "set_logical_viewport",
      "close_page",
      "navigate",
      "snapshot",
      "click",
      "type",
      "press",
      "scroll",
      "devtools",
      "screenshot",
      "hit_test",
      "start_screencast",
      "stop_screencast",
      "user_input",
      "update_control",
      "shutdown",
    ].includes(type)
  );
}

function requestIdFromInvalidRequest(value: string): string {
  try {
    const request = JSON.parse(value) as { request_id?: unknown };
    return typeof request.request_id === "string" && request.request_id
      ? request.request_id
      : "invalid-request";
  } catch {
    return "invalid-request";
  }
}

function authorized(request: IncomingMessage, expectedToken: string): boolean {
  const header = request.headers.authorization;
  if (!header?.startsWith("Bearer ")) return false;
  const received = Buffer.from(header.slice("Bearer ".length), "utf8");
  const expected = Buffer.from(expectedToken, "utf8");
  return received.length === expected.length && timingSafeEqual(received, expected);
}

function normalizeError(error: unknown, sideEffectStarted: boolean): CommandError {
  if (error instanceof ProtocolFailure) {
    return {
      code: error.code,
      message: error.message,
      recoverable: error.recoverable,
      side_effect_started: error.sideEffectStarted,
      diagnostic: error.diagnostic ?? null,
    };
  }
  return {
    code: "browser_host_internal_error",
    message: error instanceof Error ? error.message : String(error),
    recoverable: true,
    side_effect_started: sideEffectStarted,
    diagnostic: error instanceof Error ? error.stack ?? null : null,
  };
}

function failedResponse(requestId: string, error: CommandError): ResponseEnvelope {
  return {
    request_id: requestId,
    protocol_version: PROTOCOL_VERSION,
    outcome: { status: "failed", payload: error },
  };
}

export function configFromEnvironment(
  environment: NodeJS.ProcessEnv = process.env,
): BrowserHostServerConfig {
  const required = (name: string): string => {
    const value = environment[name]?.trim();
    if (!value) throw new Error(`${name} is required`);
    return value;
  };
  const runtimeEpoch = Number(environment.MAGI_BROWSER_RUNTIME_EPOCH ?? "0");
  if (!Number.isSafeInteger(runtimeEpoch) || runtimeEpoch < 0) {
    throw new Error("MAGI_BROWSER_RUNTIME_EPOCH must be a non-negative integer");
  }
  const daemonProcessId = Number(required("MAGI_BROWSER_DAEMON_PID"));
  if (!Number.isSafeInteger(daemonProcessId) || daemonProcessId <= 0) {
    throw new Error("MAGI_BROWSER_DAEMON_PID must be a positive integer");
  }
  const port = Number(environment.MAGI_BROWSER_HOST_PORT ?? "0");
  if (!Number.isSafeInteger(port) || port < 0 || port > 65_535) {
    throw new Error("MAGI_BROWSER_HOST_PORT must be between 0 and 65535");
  }
  const deviceScaleFactor = Number(
    environment.MAGI_BROWSER_DEVICE_SCALE_FACTOR
      ?? (process.platform === "darwin" ? "2" : "1"),
  );
  if (!Number.isFinite(deviceScaleFactor) || deviceScaleFactor < 1 || deviceScaleFactor > 4) {
    throw new Error("MAGI_BROWSER_DEVICE_SCALE_FACTOR must be between 1 and 4");
  }
  const maxActivePages = Number(environment.MAGI_BROWSER_MAX_ACTIVE_PAGES ?? "8");
  const maxTabs = Number(environment.MAGI_BROWSER_MAX_TABS ?? "64");
  if (!Number.isSafeInteger(maxActivePages) || maxActivePages < 1 || maxActivePages > 64) {
    throw new Error("MAGI_BROWSER_MAX_ACTIVE_PAGES must be an integer between 1 and 64");
  }
  if (!Number.isSafeInteger(maxTabs) || maxTabs < 1 || maxTabs > 256 || maxActivePages > maxTabs) {
    throw new Error("MAGI_BROWSER_MAX_TABS must be an integer between 1 and 256");
  }
  const profilePath = resolve(required("MAGI_BROWSER_PROFILE_PATH"));
  return {
    profilePath,
    chromiumExecutable: resolve(required("MAGI_BROWSER_CHROMIUM_EXECUTABLE")),
    runtimeVersion: required("MAGI_BROWSER_RUNTIME_VERSION"),
    hostVersion: environment.MAGI_BROWSER_HOST_VERSION?.trim() || "0.1.0",
    playwrightVersion: required("MAGI_BROWSER_PLAYWRIGHT_VERSION"),
    runtimeEpoch,
    headless: environment.MAGI_BROWSER_HEADLESS !== "0",
    deviceScaleFactor,
    downloadPath: resolve(
      environment.MAGI_BROWSER_DOWNLOAD_PATH?.trim() || resolve(profilePath, "Downloads"),
    ),
    maxActivePages,
    maxTabs,
    bindHost: "127.0.0.1",
    port,
    authToken: required("MAGI_BROWSER_HOST_TOKEN"),
    daemonProcessId,
  };
}

async function main(): Promise<void> {
  const config = configFromEnvironment();
  const server = await startBrowserHostServer(config);
  process.stdout.write(
    `${JSON.stringify({
      status: "ready",
      port: server.port,
      protocol_version: PROTOCOL_VERSION,
      process_id: process.pid,
    })}\n`,
  );
  let stopping = false;
  const stop = () => {
    if (stopping) return;
    stopping = true;
    void server.close().finally(() => process.exit(0));
  };
  const parentWatchdog = setInterval(() => {
    if (config.daemonProcessId && process.ppid !== config.daemonProcessId) stop();
  }, 500);
  parentWatchdog.unref();
  process.once("SIGINT", stop);
  process.once("SIGTERM", stop);
}

if (require.main === module) {
  void main().catch((error) => {
    process.stderr.write(
      `${JSON.stringify({
        status: "failed",
        error: error instanceof Error ? error.message : String(error),
      })}\n`,
    );
    process.exitCode = 1;
  });
}
