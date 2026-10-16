import { randomUUID } from "node:crypto";
import type { BrowserAgentCursorPresentation, BrowserAgentCursorPresentationResult } from "@magi/desktop-browser-contracts";

/** 只保存尚未完成的宿主动画握手；页面和控制权仍由 Surface/Authority 持有。 */
export class BrowserCursorPresentations {
  readonly #pending = new Map<string, {
    windowId: string;
    request: BrowserAgentCursorPresentation;
    finish: (error?: Error) => void;
  }>();

  wait(
    windowId: string,
    identity: Omit<BrowserAgentCursorPresentation, "motionId">,
    emit: (request: BrowserAgentCursorPresentation) => void,
    assertCurrent: () => void,
    signal?: AbortSignal,
  ): Promise<void> {
    const request = { ...identity, motionId: randomUUID() };
    return new Promise((resolve, reject) => {
      const finish = (error?: Error) => {
        if (!this.#pending.delete(request.motionId)) return;
        clearInterval(poll);
        clearTimeout(timeout);
        signal?.removeEventListener("abort", abort);
        error ? reject(error) : resolve();
      };
      const abort = () => finish(new Error("browser_command_cancelled"));
      // 接管/导航不得等到动画 ACK 或超时才中止，更不能随后补发点击。
      const check = () => {
        try { assertCurrent(); } catch (error) { finish(error instanceof Error ? error : new Error(String(error))); }
      };
      const poll = setInterval(check, 20);
      const timeout = setTimeout(() => finish(new Error("browser_cursor_presentation_timeout")), 3_000);
      this.#pending.set(request.motionId, { windowId, request, finish });
      signal?.addEventListener("abort", abort, { once: true });
      if (signal?.aborted) return abort();
      check();
      if (this.#pending.has(request.motionId)) {
        try { emit(request); } catch (error) { finish(error instanceof Error ? error : new Error(String(error))); }
      }
    });
  }

  complete(windowId: string, result: BrowserAgentCursorPresentationResult): void {
    const pending = this.#pending.get(result.motionId);
    if (!pending || pending.windowId !== windowId
      || pending.request.tab_id !== result.tab_id || pending.request.surface_id !== result.surface_id
      || pending.request.navigation_revision !== result.navigation_revision) return;
    pending.finish(result.outcome === "interrupted" ? new Error("browser_cursor_presentation_interrupted") : undefined);
  }
}
