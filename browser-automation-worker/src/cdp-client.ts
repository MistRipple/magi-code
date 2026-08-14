import { randomUUID } from "node:crypto";
import type {
  BrowserCommandError,
  BrowserSurfaceBinding,
  MainToWorkerMessage,
  WorkerCdpRequest,
  WorkerToMainMessage,
} from "@magi/desktop-browser-contracts";

interface ParentPort {
  on(event: "message", listener: (event: { data: MainToWorkerMessage }) => void): void;
  postMessage(message: WorkerToMainMessage): void;
}

interface PendingRequest {
  binding: BrowserSurfaceBinding;
  resolve: (value: unknown) => void;
  reject: (error: Error) => void;
  timer: NodeJS.Timeout;
}

export class CdpClient {
  readonly #port: ParentPort;
  readonly #pending = new Map<string, PendingRequest>();
  readonly #listeners = new Set<(
    binding: BrowserSurfaceBinding,
    method: string,
    params: Record<string, unknown>,
  ) => void>();

  constructor(port: ParentPort) {
    this.#port = port;
    port.on("message", (event) => this.accept(event.data));
  }

  send<T = unknown>(
    binding: BrowserSurfaceBinding,
    method: string,
    params: Record<string, unknown> = {},
    timeoutMs = 30_000,
  ): Promise<T> {
    const requestId = `cdp-${randomUUID()}`;
    const request: WorkerCdpRequest = {
      type: "cdp_request",
      request_id: requestId,
      binding,
      method,
      params,
    };
    return new Promise<T>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.#pending.delete(requestId);
        reject(new Error(`browser_cdp_timeout:${method}`));
      }, timeoutMs);
      timer.unref();
      this.#pending.set(requestId, {
        binding,
        resolve: (value) => resolve(value as T),
        reject,
        timer,
      });
      this.#port.postMessage(request);
    });
  }

  onEvent(listener: (
    binding: BrowserSurfaceBinding,
    method: string,
    params: Record<string, unknown>,
  ) => void): () => void {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }

  close(error = new Error("browser_worker_stopped")): void {
    for (const pending of this.#pending.values()) {
      clearTimeout(pending.timer);
      pending.reject(error);
    }
    this.#pending.clear();
    this.#listeners.clear();
  }

  private accept(message: MainToWorkerMessage): void {
    if (message.type === "cdp_response") {
      const pending = this.#pending.get(message.request_id);
      if (!pending) return;
      this.#pending.delete(message.request_id);
      clearTimeout(pending.timer);
      if (!sameBinding(pending.binding, message.binding)) {
        pending.reject(new Error("browser_surface_stale"));
      } else if (message.error) {
        pending.reject(commandError(message.error));
      } else {
        pending.resolve(message.result);
      }
      return;
    }
    if (message.type === "cdp_event") {
      for (const listener of this.#listeners) {
        listener(message.binding, message.method, message.params);
      }
    }
  }
}

function sameBinding(left: BrowserSurfaceBinding, right: BrowserSurfaceBinding): boolean {
  return left.desktop_epoch === right.desktop_epoch
    && left.surface_id === right.surface_id
    && left.surface_revision === right.surface_revision
    && left.target_id === right.target_id;
}

function commandError(value: BrowserCommandError): Error {
  const error = new Error(`${value.code}:${value.message}`);
  error.name = "BrowserCdpError";
  return error;
}

export function parentPort(): ParentPort {
  const value = (process as NodeJS.Process & { parentPort?: ParentPort }).parentPort;
  if (!value) throw new Error("BrowserAutomationWorker requires Electron utilityProcess parentPort");
  return value;
}
