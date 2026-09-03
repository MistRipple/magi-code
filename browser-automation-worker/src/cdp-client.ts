import { randomUUID } from "node:crypto";
import { AsyncLocalStorage } from "node:async_hooks";
import type {
  BrowserCommandError,
  BrowserSurfaceBinding,
  MainToWorkerMessage,
  WorkerCdpRequest,
  WorkerToMainMessage,
} from "@magi/desktop-browser-contracts";
import {
  parseMainToWorkerMessage,
  parseWorkerToMainMessage,
} from "@magi/desktop-browser-contracts/validation";

export interface ParentPort {
  on(event: "message", listener: (event: { data: unknown }) => void): void;
  postMessage(message: WorkerToMainMessage): void;
}

interface PendingRequest {
  callId: string;
  binding: BrowserSurfaceBinding;
  resolve: (value: unknown) => void;
  reject: (error: Error) => void;
  timer: NodeJS.Timeout;
  signalCleanup: (() => void) | null;
}

export interface CdpCallContext {
  callId: string;
  signal: AbortSignal;
}

export class CdpClient {
  readonly #port: ParentPort;
  readonly #pending = new Map<string, PendingRequest>();
  readonly #callContext = new AsyncLocalStorage<CdpCallContext>();
  readonly #listeners = new Set<(
    binding: BrowserSurfaceBinding,
    method: string,
    params: Record<string, unknown>,
    sessionId?: string,
  ) => void>();

  constructor(port: ParentPort) {
    this.#port = port;
    port.on("message", (event) => {
      try {
        this.accept(event.data);
      } catch (cause) {
        console.error(
          "[browser-worker] invalid inbound CDP message",
          cause instanceof Error ? cause.message : String(cause),
        );
      }
    });
  }

  run<T>(context: CdpCallContext, operation: () => Promise<T>): Promise<T> {
    return this.#callContext.run(context, operation);
  }

  currentSignal(): AbortSignal | undefined {
    return this.#callContext.getStore()?.signal;
  }

  send<T = unknown>(
    binding: BrowserSurfaceBinding,
    method: string,
    params: Record<string, unknown> = {},
    timeoutMs = 30_000,
    sessionId?: string,
    options: { allowNavigationAdvance?: boolean } = {},
  ): Promise<T> {
    const context = this.#callContext.getStore();
    if (context?.signal.aborted) {
      return Promise.reject(new Error("browser_command_cancelled"));
    }
    const requestId = `cdp-${randomUUID()}`;
    const callId = context?.callId ?? `worker-internal-${randomUUID()}`;
    const request: WorkerCdpRequest = {
      type: "cdp_request",
      call_id: callId,
      request_id: requestId,
      binding,
      method,
      params,
      ...(sessionId ? { session_id: sessionId } : {}),
      ...(options.allowNavigationAdvance ? { allow_navigation_advance: true } : {}),
    };
    return new Promise<T>((resolve, reject) => {
      let pending!: PendingRequest;
      const timer = setTimeout(() => {
        if (!this.#pending.has(requestId)) return;
        this.cancelPending(requestId, pending, new Error(`browser_cdp_timeout:${method}`));
      }, timeoutMs);
      timer.unref();
      pending = {
        callId,
        binding,
        resolve: (value) => resolve(value as T),
        reject,
        timer,
        signalCleanup: null,
      };
      this.#pending.set(requestId, pending);
      if (context?.signal) {
        const onAbort = () => this.cancelPending(requestId, pending);
        pending.signalCleanup = () => context.signal.removeEventListener("abort", onAbort);
        context.signal.addEventListener("abort", onAbort, { once: true });
        if (context.signal.aborted) onAbort();
      }
      if (this.#pending.get(requestId) !== pending) return;
      try {
        this.#port.postMessage(parseWorkerToMainMessage(request));
      } catch (cause) {
        this.settlePending(requestId, pending, cause instanceof Error ? cause : new Error(String(cause)));
      }
    });
  }

  onEvent(listener: (
    binding: BrowserSurfaceBinding,
    method: string,
    params: Record<string, unknown>,
    sessionId?: string,
  ) => void): () => void {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }

  close(error = new Error("browser_worker_stopped")): void {
    for (const pending of this.#pending.values()) {
      clearTimeout(pending.timer);
      pending.signalCleanup?.();
      pending.reject(error);
    }
    this.#pending.clear();
    this.#listeners.clear();
  }

  private accept(value: unknown): void {
    const message: MainToWorkerMessage = parseMainToWorkerMessage(value);
    if (message.type === "cdp_response") {
      const pending = this.#pending.get(message.request_id);
      if (!pending) return;
      if (pending.callId !== message.call_id) return;
      this.#pending.delete(message.request_id);
      clearTimeout(pending.timer);
      pending.signalCleanup?.();
      pending.signalCleanup = null;
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
        listener(message.binding, message.method, message.params, message.session_id);
      }
    }
  }

  private cancelPending(
    requestId: string,
    pending: PendingRequest,
    error = new Error("browser_command_cancelled"),
  ): void {
    if (this.#pending.get(requestId) !== pending) return;
    this.settlePending(requestId, pending, error, true);
  }

  private settlePending(
    requestId: string,
    pending: PendingRequest,
    error: Error,
    notifyMain = false,
  ): void {
    if (this.#pending.get(requestId) !== pending) return;
    this.#pending.delete(requestId);
    clearTimeout(pending.timer);
    pending.signalCleanup?.();
    pending.signalCleanup = null;
    if (notifyMain) {
      try {
        this.#port.postMessage(parseWorkerToMainMessage({
          type: "cdp_cancel",
          call_id: pending.callId,
          request_id: requestId,
          binding: pending.binding,
        }));
      } catch (cause) {
        console.warn(
          "[browser-worker] CDP cancel message failed",
          cause instanceof Error ? cause.message : String(cause),
        );
      }
    }
    pending.reject(error);
  }
}

function sameBinding(left: BrowserSurfaceBinding, right: BrowserSurfaceBinding): boolean {
  return left.desktop_epoch === right.desktop_epoch
    && left.window_id === right.window_id
    && left.surface_id === right.surface_id
    && left.surface_revision === right.surface_revision
    && left.tab_id === right.tab_id
    && left.web_contents_id === right.web_contents_id
    && left.target_id === right.target_id
    && left.browser_context_id === right.browser_context_id
    && left.navigation_revision === right.navigation_revision;
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
