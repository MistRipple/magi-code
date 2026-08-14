import { createHash, randomUUID } from "node:crypto";
import type {
  BrowserCommandOutcome,
  BrowserCommandResult,
  BrowserHostCommand,
  BrowserSnapshot,
  BrowserSnapshotTarget,
  BrowserSurfaceBinding,
  WorkerCommandResponse,
} from "@magi/desktop-browser-contracts";
import { CdpClient } from "./cdp-client.js";
import { INSTALL_PAGE_RUNTIME, MAGI_AUTOMATION_WORLD } from "./page-script.js";

interface PageRuntimeState {
  binding: BrowserSurfaceBinding;
  executionContextId: number | null;
  console: Array<Record<string, unknown>>;
  nextConsoleId: number;
  network: Array<Record<string, unknown>>;
  nextNetworkId: number;
  dialog: Record<string, unknown> | null;
  newDocumentScripts: Set<string>;
  heapSnapshotChunks: string[];
  traceActive: boolean;
  traceEvents: Array<Record<string, unknown>>;
}

export class BrowserAutomationRuntime {
  readonly #cdp: CdpClient;
  readonly #pages = new Map<string, PageRuntimeState>();

  constructor(cdp: CdpClient) {
    this.#cdp = cdp;
    cdp.onEvent((binding, method, params) => this.onCdpEvent(binding, method, params));
  }

  async execute(
    callId: string,
    binding: BrowserSurfaceBinding,
    command: BrowserHostCommand,
  ): Promise<WorkerCommandResponse> {
    try {
      const executed = await this.executeCommand(binding, command);
      return {
        type: "worker_result",
        call_id: callId,
        binding,
        outcome: { status: "succeeded", payload: executed.result },
        ...(executed.binary ? { binary_base64: executed.binary.toString("base64") } : {}),
      };
    } catch (cause) {
      const error = normalizeError(cause);
      const outcome: BrowserCommandOutcome = error.side_effect_started
        ? { status: "indeterminate", payload: error }
        : { status: "failed", payload: error };
      return { type: "worker_result", call_id: callId, binding, outcome };
    }
  }

  private async executeCommand(
    binding: BrowserSurfaceBinding,
    command: BrowserHostCommand,
  ): Promise<{ result: BrowserCommandResult; binary?: Buffer }> {
    switch (command.type) {
      case "snapshot":
        return {
          result: {
            type: "snapshot",
            payload: await this.snapshot(binding, command.payload.limits),
          },
        };
      case "click":
        await this.click(binding, command.payload.target);
        return empty();
      case "type":
        await this.typeText(
          binding,
          command.payload.target,
          command.payload.text,
          command.payload.replace,
          command.payload.submit_key ?? null,
        );
        return empty();
      case "press":
        await this.press(binding, command.payload.key);
        return empty();
      case "scroll":
        await this.scroll(binding, command.payload.delta_x, command.payload.delta_y);
        return empty();
      case "screenshot":
        return this.screenshot(binding, command.payload);
      case "hit_test":
        return {
          result: {
            type: "hit_test",
            payload: await this.hitTest(binding, command.payload.x, command.payload.y),
          },
        };
      case "devtools":
        return {
          result: {
            type: "json",
            payload: { value: await this.devtools(binding, command.payload.operation, command.payload.arguments) },
          },
        };
      case "ping":
        return { result: { type: "pong", payload: { monotonic_millis: Math.floor(performance.now()) } } };
      default:
        throw protocolFailure("browser_worker_command_unsupported", `Worker cannot execute ${command.type}`);
    }
  }

  private page(binding: BrowserSurfaceBinding): PageRuntimeState {
    const current = this.#pages.get(binding.surface_id);
    if (
      current
      && current.binding.surface_revision === binding.surface_revision
      && current.binding.target_id === binding.target_id
    ) {
      return current;
    }
    const next: PageRuntimeState = {
      binding,
      executionContextId: null,
      console: current?.console ?? [],
      nextConsoleId: current?.nextConsoleId ?? 1,
      network: current?.network ?? [],
      nextNetworkId: current?.nextNetworkId ?? 1,
      dialog: current?.dialog ?? null,
      newDocumentScripts: current?.newDocumentScripts ?? new Set(),
      heapSnapshotChunks: current?.heapSnapshotChunks ?? [],
      traceActive: current?.traceActive ?? false,
      traceEvents: current?.traceEvents ?? [],
    };
    this.#pages.set(binding.surface_id, next);
    return next;
  }

  private async context(binding: BrowserSurfaceBinding): Promise<number> {
    const page = this.page(binding);
    if (page.executionContextId !== null) return page.executionContextId;
    const frameTree = await this.#cdp.send<{
      frameTree: { frame: { id: string } };
    }>(binding, "Page.getFrameTree");
    const world = await this.#cdp.send<{ executionContextId: number }>(
      binding,
      "Page.createIsolatedWorld",
      {
        frameId: frameTree.frameTree.frame.id,
        worldName: MAGI_AUTOMATION_WORLD,
        grantUniveralAccess: false,
      },
    );
    page.executionContextId = world.executionContextId;
    await this.evaluate(binding, INSTALL_PAGE_RUNTIME, true);
    return world.executionContextId;
  }

  private async evaluate<T>(
    binding: BrowserSurfaceBinding,
    expression: string,
    returnByValue = true,
  ): Promise<T> {
    const contextId = await this.context(binding);
    const response = await this.#cdp.send<{
      result: { value?: T; description?: string };
      exceptionDetails?: { text?: string; exception?: { description?: string } };
    }>(binding, "Runtime.evaluate", {
      expression,
      contextId,
      returnByValue,
      awaitPromise: true,
      userGesture: false,
    });
    if (response.exceptionDetails) {
      throw protocolFailure(
        "browser_page_script_failed",
        response.exceptionDetails.exception?.description
          || response.exceptionDetails.text
          || "page script failed",
      );
    }
    return response.result.value as T;
  }

  private async snapshot(
    binding: BrowserSurfaceBinding,
    limits: { max_nodes: number; max_text_bytes: number },
  ): Promise<BrowserSnapshot> {
    const value = await this.evaluate<Omit<BrowserSnapshot, "tab_id" | "continuation_refs">>(
      binding,
      `globalThis.__magiBrowserAutomation.snapshot(${safeInteger(limits.max_nodes, 400)}, ${safeInteger(limits.max_text_bytes, 32768)})`,
    );
    return {
      tab_id: binding.tab_id,
      ...value,
      continuation_refs: [],
    };
  }

  private async target(
    binding: BrowserSurfaceBinding,
    target: BrowserSnapshotTarget,
    focus = false,
  ): Promise<{ x: number; y: number; bounds: { x: number; y: number; width: number; height: number }; editable: boolean; sensitive: string | null }> {
    if (target.element_ref === "root") {
      throw protocolFailure("browser_element_ref_invalid", "root is not an interactive element");
    }
    return this.evaluate(
      binding,
      `globalThis.__magiBrowserAutomation.${focus ? "focus" : "target"}(${JSON.stringify(target.element_ref)}, ${safeInteger(target.snapshot_revision, 0)})`,
    );
  }

  private async click(binding: BrowserSurfaceBinding, ref: BrowserSnapshotTarget): Promise<void> {
    const target = await this.target(binding, ref);
    await this.pointer(binding, "mouseMoved", target.x, target.y);
    await this.pointer(binding, "mousePressed", target.x, target.y, { button: "left", buttons: 1, clickCount: 1 });
    await this.pointer(binding, "mouseReleased", target.x, target.y, { button: "left", buttons: 0, clickCount: 1 });
  }

  private async typeText(
    binding: BrowserSurfaceBinding,
    ref: BrowserSnapshotTarget,
    text: string,
    replace: boolean,
    submitKey: string | null,
  ): Promise<void> {
    const target = await this.target(binding, ref, true);
    if (!target.editable) throw protocolFailure("browser_target_not_editable", "target is not editable");
    if (target.sensitive) {
      throw protocolFailure("browser_sensitive_action_requires_user", `sensitive input: ${target.sensitive}`);
    }
    if (replace) {
      await this.key(binding, "keyDown", "a", process.platform === "darwin" ? 4 : 2);
      await this.key(binding, "keyUp", "a", process.platform === "darwin" ? 4 : 2);
      await this.press(binding, "Backspace");
    }
    await this.#cdp.send(binding, "Input.insertText", { text });
    if (submitKey) await this.press(binding, submitKey);
  }

  private async press(binding: BrowserSurfaceBinding, key: string): Promise<void> {
    const normalized = key.trim();
    if (!normalized) throw protocolFailure("browser_key_invalid", "key is required");
    const description = keyDescription(normalized);
    await this.key(binding, "keyDown", description.key, description.modifiers, description.code);
    await this.key(binding, "keyUp", description.key, description.modifiers, description.code);
  }

  private async key(
    binding: BrowserSurfaceBinding,
    type: "keyDown" | "keyUp",
    key: string,
    modifiers = 0,
    code?: string,
  ): Promise<void> {
    await this.#cdp.send(binding, "Input.dispatchKeyEvent", {
      type,
      key,
      code: code ?? key,
      modifiers,
    });
  }

  private async scroll(binding: BrowserSurfaceBinding, deltaX: number, deltaY: number): Promise<void> {
    const metrics = await this.#cdp.send<{
      layoutViewport: { clientWidth: number; clientHeight: number };
    }>(binding, "Page.getLayoutMetrics");
    await this.#cdp.send(binding, "Input.dispatchMouseEvent", {
      type: "mouseWheel",
      x: metrics.layoutViewport.clientWidth / 2,
      y: metrics.layoutViewport.clientHeight / 2,
      deltaX,
      deltaY,
    });
  }

  private async pointer(
    binding: BrowserSurfaceBinding,
    type: string,
    x: number,
    y: number,
    extra: Record<string, unknown> = {},
  ): Promise<void> {
    await this.#cdp.send(binding, "Input.dispatchMouseEvent", { type, x, y, ...extra });
  }

  private async screenshot(
    binding: BrowserSurfaceBinding,
    input: Extract<BrowserHostCommand, { type: "screenshot" }>["payload"],
  ): Promise<{ result: BrowserCommandResult; binary: Buffer }> {
    let clip: { x: number; y: number; width: number; height: number; scale: number } | undefined;
    if (input.target && input.target.element_ref !== "root") {
      const target = await this.target(binding, input.target);
      clip = { ...target.bounds, scale: 1 };
    } else if (input.clip) {
      const metrics = await this.#cdp.send<{
        layoutViewport: { clientWidth: number; clientHeight: number };
      }>(binding, "Page.getLayoutMetrics");
      clip = {
        x: input.clip.x * metrics.layoutViewport.clientWidth,
        y: input.clip.y * metrics.layoutViewport.clientHeight,
        width: input.clip.width * metrics.layoutViewport.clientWidth,
        height: input.clip.height * metrics.layoutViewport.clientHeight,
        scale: 1,
      };
    } else if (input.full_page) {
      const metrics = await this.#cdp.send<{
        contentSize: { x: number; y: number; width: number; height: number };
      }>(binding, "Page.getLayoutMetrics");
      clip = { ...metrics.contentSize, scale: 1 };
    }
    const captured = await this.#cdp.send<{ data: string }>(binding, "Page.captureScreenshot", {
      format: input.format,
      ...(input.quality !== undefined ? { quality: input.quality } : {}),
      ...(clip ? { clip } : {}),
      captureBeyondViewport: input.full_page,
      fromSurface: true,
    });
    const binary = Buffer.from(captured.data, "base64");
    const mime = input.format === "png" ? "image/png" : `image/${input.format}`;
    return {
      result: {
        type: "binary_payload",
        payload: {
          payload_id: `browser-payload-${randomUUID()}`,
          mime_type: mime,
          byte_length: binary.byteLength,
          sha256: createHash("sha256").update(binary).digest("hex"),
        },
      },
      binary,
    };
  }

  private async hitTest(binding: BrowserSurfaceBinding, x: number, y: number) {
    const result = await this.evaluate<Record<string, unknown>>(
      binding,
      `globalThis.__magiBrowserAutomation.hitTest(${Number(x)}, ${Number(y)})`,
    );
    return { ...result, navigation_revision: binding.navigation_revision } as never;
  }

  private async devtools(
    binding: BrowserSurfaceBinding,
    operation: string,
    args: Record<string, unknown>,
  ): Promise<unknown> {
    switch (operation) {
      case "hover": {
        const target = await this.target(binding, snapshotTarget(args));
        await this.pointer(binding, "mouseMoved", target.x, target.y);
        return { hovered: true };
      }
      case "click_at": {
        const x = finiteNumber(args.x, "x");
        const y = finiteNumber(args.y, "y");
        await this.pointer(binding, "mouseMoved", x, y);
        await this.pointer(binding, "mousePressed", x, y, { button: "left", buttons: 1, clickCount: 1 });
        await this.pointer(binding, "mouseReleased", x, y, { button: "left", buttons: 0, clickCount: 1 });
        return { clicked: true };
      }
      case "drag": {
        const source = await this.target(binding, snapshotTarget(args, "source"));
        const target = await this.target(binding, snapshotTarget(args, "target"));
        await this.pointer(binding, "mouseMoved", source.x, source.y);
        await this.pointer(binding, "mousePressed", source.x, source.y, { button: "left", buttons: 1, clickCount: 1 });
        await this.pointer(binding, "mouseMoved", target.x, target.y, { button: "left", buttons: 1 });
        await this.pointer(binding, "mouseReleased", target.x, target.y, { button: "left", buttons: 0, clickCount: 1 });
        return { dragged: true };
      }
      case "fill_form":
        return this.fillForm(binding, args);
      case "wait_for":
        return this.waitFor(binding, args);
      case "evaluate": {
        const expression = String(args.expression ?? "").trim();
        if (!expression) throw protocolFailure("browser_evaluate_invalid", "expression is required");
        return this.evaluate(binding, `globalThis.__magiBrowserAutomation.evaluate(${JSON.stringify(expression)})`);
      }
      case "console": {
        const page = this.page(binding);
        if (args.action === "clear") page.console.length = 0;
        if (args.action === "get") {
          const messageId = Number(args.message_id);
          return {
            entry: page.console.find((entry) => entry.id === messageId || entry.messageId === messageId) ?? null,
          };
        }
        return { entries: page.console.slice(-safeInteger(args.page_size ?? args.limit, 100)) };
      }
      case "network":
        return this.network(binding, args);
      case "performance": {
        return this.performance(binding, args);
      }
      case "emulate":
        return this.emulate(binding, args);
      case "dialog":
        if (args.action === "list") return { dialog: this.page(binding).dialog };
        await this.#cdp.send(binding, "Page.handleJavaScriptDialog", {
          accept: args.action !== "dismiss",
          ...(typeof args.prompt_text === "string" ? { promptText: args.prompt_text } : {}),
        });
        return { handled: true };
      case "webmcp":
        return this.webmcp(binding, args);
      case "pwa":
        return this.pwaAudit(binding);
      case "third_party":
        return this.thirdPartySummary(binding);
      case "heap":
        return this.heap(binding, args);
      case "scripts":
        return this.newDocumentScript(binding, args);
      case "overlay":
        return this.overlay(binding, args);
      case "lighthouse":
        return this.lighthouseCompatibleAudit(binding);
      case "upload_file":
        throw protocolFailure(
          "browser_file_authorization_required",
          "file upload requires a Desktop file authorization token",
        );
      default:
        throw protocolFailure("browser_devtools_operation_unsupported", operation);
    }
  }

  private async fillForm(binding: BrowserSurfaceBinding, args: Record<string, unknown>): Promise<unknown> {
    const fields = Array.isArray(args.fields) ? args.fields : [];
    let filled = 0;
    for (const raw of fields) {
      if (!raw || typeof raw !== "object") continue;
      const field = raw as Record<string, unknown>;
      await this.typeText(
        binding,
        snapshotTarget(field),
        String(field.value ?? ""),
        field.replace !== false,
        null,
      );
      filled += 1;
    }
    return { filled };
  }

  private async waitFor(binding: BrowserSurfaceBinding, args: Record<string, unknown>): Promise<unknown> {
    const selector = typeof args.selector === "string" ? args.selector.trim() : "";
    const text = typeof args.text === "string" ? args.text : "";
    const timeoutMs = Math.min(30_000, Math.max(0, safeInteger(args.timeout_ms, 5_000)));
    const deadline = Date.now() + timeoutMs;
    do {
      const found = await this.evaluate<unknown>(
        binding,
        selector
          ? `globalThis.__magiBrowserAutomation.query(${JSON.stringify(selector)})`
          : `document.body?.innerText?.includes(${JSON.stringify(text)}) || false`,
      );
      if (found) return { matched: true, value: found };
      await new Promise((resolve) => setTimeout(resolve, 100));
    } while (Date.now() < deadline);
    throw protocolFailure("browser_wait_timeout", "wait condition timed out");
  }

  private async pwaAudit(binding: BrowserSurfaceBinding): Promise<unknown> {
    return this.evaluate(binding, `(() => ({
      manifest: document.querySelector('link[rel="manifest"]')?.href || null,
      serviceWorkerControlled: Boolean(navigator.serviceWorker?.controller),
      secureContext: globalThis.isSecureContext,
      displayMode: matchMedia('(display-mode: standalone)').matches ? 'standalone' : 'browser'
    }))()`);
  }

  private async network(binding: BrowserSurfaceBinding, args: Record<string, unknown>): Promise<unknown> {
    const page = this.page(binding);
    if (args.action === "clear") {
      page.network.length = 0;
      return { entries: [] };
    }
    if (args.action === "get") {
      const requestId = String(args.request_id ?? "").trim();
      if (!requestId) throw protocolFailure("browser_network_request_id_required", "request_id is required");
      const entry = page.network.find((candidate) => String(candidate.requestId ?? candidate.id) === requestId) ?? null;
      if (args.include_body) {
        const body = await this.#cdp.send(binding, "Network.getResponseBody", { requestId });
        return { entry, body };
      }
      return { entry };
    }
    const entries = page.network.slice(-safeInteger(args.page_size ?? args.limit, 100));
    if (args.action === "failed") {
      return { entries: entries.filter((entry) => entry.type === "failed") };
    }
    return { entries };
  }

  private async emulate(binding: BrowserSurfaceBinding, args: Record<string, unknown>): Promise<unknown> {
    const action = String(args.action ?? "").trim();
    if (!action) {
      const applied: string[] = [];
      if (typeof args.user_agent === "string" && args.user_agent.trim()) {
        await this.#cdp.send(binding, "Emulation.setUserAgentOverride", { userAgent: args.user_agent });
        applied.push("user_agent");
      } else if (args.user_agent === null) {
        await this.#cdp.send(binding, "Emulation.setUserAgentOverride", { userAgent: "" });
        applied.push("user_agent");
      }
      if (args.geolocation && typeof args.geolocation === "object") {
        const location = args.geolocation as Record<string, unknown>;
        await this.#cdp.send(binding, "Emulation.setGeolocationOverride", {
          latitude: finiteNumber(location.latitude, "geolocation.latitude"),
          longitude: finiteNumber(location.longitude, "geolocation.longitude"),
          accuracy: Math.max(0, finiteNumber(location.accuracy ?? 1, "geolocation.accuracy")),
        });
        applied.push("geolocation");
      } else if (args.geolocation === null) {
        await this.#cdp.send(binding, "Emulation.clearGeolocationOverride");
        applied.push("geolocation");
      }
      if (typeof args.color_scheme === "string") {
        const scheme = args.color_scheme === "auto" ? "" : args.color_scheme;
        await this.#cdp.send(binding, "Emulation.setEmulatedMedia", {
          media: "",
          features: scheme ? [{ name: "prefers-color-scheme", value: scheme }] : [],
        });
        applied.push("color_scheme");
      }
      if (typeof args.cpu_throttling_rate === "number") {
        await this.#cdp.send(binding, "Emulation.setCPUThrottlingRate", { rate: Math.max(1, args.cpu_throttling_rate) });
        applied.push("cpu_throttling_rate");
      }
      if (typeof args.network_conditions === "string") {
        const conditions = networkConditions(args.network_conditions);
        await this.#cdp.send(binding, "Network.emulateNetworkConditions", conditions);
        applied.push("network_conditions");
      }
      if (args.extra_http_headers && typeof args.extra_http_headers === "object") {
        await this.#cdp.send(binding, "Network.setExtraHTTPHeaders", { headers: args.extra_http_headers });
        applied.push("extra_http_headers");
      }
      return { applied };
    }
    switch (action) {
      case "set_user_agent":
        await this.#cdp.send(binding, "Emulation.setUserAgentOverride", {
          userAgent: String(args.user_agent ?? ""),
          ...(typeof args.accept_language === "string" ? { acceptLanguage: args.accept_language } : {}),
          ...(typeof args.platform === "string" ? { platform: args.platform } : {}),
        });
        return { applied: true, action };
      case "set_geolocation":
        await this.#cdp.send(binding, "Emulation.setGeolocationOverride", {
          latitude: finiteNumber(args.latitude, "latitude"),
          longitude: finiteNumber(args.longitude, "longitude"),
          accuracy: Math.max(0, finiteNumber(args.accuracy ?? 1, "accuracy")),
        });
        return { applied: true, action };
      case "clear_geolocation":
        await this.#cdp.send(binding, "Emulation.clearGeolocationOverride");
        return { applied: true, action };
      case "set_color_scheme":
        await this.#cdp.send(binding, "Emulation.setEmulatedMedia", {
          media: "",
          features: [{ name: "prefers-color-scheme", value: String(args.scheme ?? "light") }],
        });
        return { applied: true, action };
      case "set_cpu_throttling":
        await this.#cdp.send(binding, "Emulation.setCPUThrottlingRate", {
          rate: Math.max(1, finiteNumber(args.rate ?? 1, "rate")),
        });
        return { applied: true, action };
      case "set_network_conditions":
        await this.#cdp.send(binding, "Network.emulateNetworkConditions", {
          offline: Boolean(args.offline),
          latency: Math.max(0, finiteNumber(args.latency ?? 0, "latency")),
          downloadThroughput: Math.max(0, finiteNumber(args.download_throughput ?? -1, "download_throughput")),
          uploadThroughput: Math.max(0, finiteNumber(args.upload_throughput ?? -1, "upload_throughput")),
        });
        return { applied: true, action };
      case "set_headers":
        await this.#cdp.send(binding, "Network.setExtraHTTPHeaders", {
          headers: args.headers && typeof args.headers === "object" ? args.headers : {},
        });
        return { applied: true, action };
      case "clear":
        await this.#cdp.send(binding, "Emulation.clearGeolocationOverride");
        await this.#cdp.send(binding, "Emulation.setEmulatedMedia", { media: "", features: [] });
        await this.#cdp.send(binding, "Emulation.setCPUThrottlingRate", { rate: 1 });
        await this.#cdp.send(binding, "Network.emulateNetworkConditions", {
          offline: false,
          latency: 0,
          downloadThroughput: -1,
          uploadThroughput: -1,
        });
        return { applied: true, action };
      default:
        throw protocolFailure("browser_emulation_operation_unsupported", action || "missing action");
    }
  }

  private async performance(binding: BrowserSurfaceBinding, args: Record<string, unknown>): Promise<unknown> {
    const page = this.page(binding);
    const action = String(args.action ?? "metrics");
    if (action === "start") {
      page.traceEvents = [];
      page.traceActive = true;
      await this.#cdp.send(binding, "Tracing.start", {
        transferMode: "ReportEvents",
        categories: "-*,devtools.timeline,v8.execute,blink.user_timing,loading",
      });
      return { started: true };
    }
    if (action === "stop") {
      if (page.traceActive) await this.#cdp.send(binding, "Tracing.end");
      page.traceActive = false;
      return { stopped: true, events: page.traceEvents };
    }
    await this.#cdp.send(binding, "Performance.enable");
    const metrics = await this.#cdp.send(binding, "Performance.getMetrics");
    return { metrics, traceActive: page.traceActive, events: action === "analyze" ? page.traceEvents : undefined };
  }

  private async heap(binding: BrowserSurfaceBinding, args: Record<string, unknown>): Promise<unknown> {
    const page = this.page(binding);
    const action = String(args.action ?? "usage");
    if (action === "usage") return this.#cdp.send(binding, "Runtime.getHeapUsage");
    if (action === "take_snapshot") {
      page.heapSnapshotChunks = [];
      await this.#cdp.send(binding, "HeapProfiler.enable");
      await this.#cdp.send(binding, "HeapProfiler.takeHeapSnapshot", { reportProgress: false });
      const snapshot = page.heapSnapshotChunks.join("");
      return { snapshot, byte_length: Buffer.byteLength(snapshot, "utf8") };
    }
    if (action === "close_snapshot") {
      page.heapSnapshotChunks = [];
      await this.#cdp.send(binding, "HeapProfiler.disable");
      return { closed: true };
    }
    if (page.heapSnapshotChunks.length === 0) {
      throw protocolFailure("browser_heap_snapshot_missing", "take_snapshot must run before heap analysis");
    }
    return { action, snapshot: page.heapSnapshotChunks.join(""), page_size: safeInteger(args.page_size, 100) };
  }

  private async webmcp(binding: BrowserSurfaceBinding, args: Record<string, unknown>): Promise<unknown> {
    if (args.action !== "execute") {
      return this.evaluate(binding, "({ available: typeof navigator.modelContext !== 'undefined' })");
    }
    const expression = String(args.expression ?? "").trim();
    if (!expression) throw protocolFailure("browser_webmcp_expression_required", "expression is required");
    return this.evaluate(binding, `globalThis.__magiBrowserAutomation.evaluate(${JSON.stringify(expression)})`);
  }

  private async newDocumentScript(binding: BrowserSurfaceBinding, args: Record<string, unknown>): Promise<unknown> {
    const page = this.page(binding);
    const action = String(args.action ?? "add");
    if (action === "clear") {
      for (const identifier of page.newDocumentScripts) {
        await this.#cdp.send(binding, "Page.removeScriptToEvaluateOnNewDocument", { identifier });
      }
      page.newDocumentScripts.clear();
      return { cleared: true };
    }
    if (action === "remove") {
      const identifier = String(args.identifier ?? "").trim();
      if (!identifier) throw protocolFailure("browser_script_identifier_required", "identifier is required");
      await this.#cdp.send(binding, "Page.removeScriptToEvaluateOnNewDocument", { identifier });
      page.newDocumentScripts.delete(identifier);
      return { removed: true, identifier };
    }
    const source = String(args.source ?? "").trim();
    if (!source) throw protocolFailure("browser_script_source_required", "source is required");
    const response = await this.#cdp.send<{ identifier: string }>(binding, "Page.addScriptToEvaluateOnNewDocument", { source });
    page.newDocumentScripts.add(response.identifier);
    return response;
  }

  private async overlay(binding: BrowserSurfaceBinding, args: Record<string, unknown>): Promise<unknown> {
    if (args.action === "hide") {
      await this.#cdp.send(binding, "Overlay.hideHighlight");
      return { hidden: true };
    }
    const selector = String(args.selector ?? "").trim();
    if (!selector) throw protocolFailure("browser_overlay_selector_required", "selector is required");
    const document = await this.#cdp.send<{ root: { nodeId: number } }>(binding, "DOM.getDocument", { depth: 1 });
    const node = await this.#cdp.send<{ nodeId: number }>(binding, "DOM.querySelector", {
      nodeId: document.root.nodeId,
      selector,
    });
    if (!node.nodeId) throw protocolFailure("browser_overlay_target_not_found", "selector did not match");
    await this.#cdp.send(binding, "Overlay.highlightNode", {
      nodeId: node.nodeId,
      highlightConfig: { showInfo: true, showStyles: false, showRulers: false, showExtensionLines: false },
    });
    return { highlighted: true, selector };
  }

  private async thirdPartySummary(binding: BrowserSurfaceBinding): Promise<unknown> {
    return this.evaluate(binding, `(() => {
      const origin = location.origin;
      const hosts = new Map();
      for (const entry of performance.getEntriesByType('resource')) {
        try {
          const url = new URL(entry.name);
          if (url.origin === origin) continue;
          hosts.set(url.host, (hosts.get(url.host) || 0) + 1);
        } catch {}
      }
      return { origins: [...hosts.entries()].map(([host, requests]) => ({ host, requests })) };
    })()`);
  }

  private async lighthouseCompatibleAudit(binding: BrowserSurfaceBinding): Promise<unknown> {
    const [metrics, pwa, accessibility] = await Promise.all([
      this.devtools(binding, "performance", {}),
      this.pwaAudit(binding),
      this.#cdp.send(binding, "Accessibility.getFullAXTree"),
    ]);
    return {
      auditEngine: "magi-cdp",
      target: binding.target_id,
      performance: metrics,
      pwa,
      accessibility,
    };
  }

  private onCdpEvent(
    binding: BrowserSurfaceBinding,
    method: string,
    params: Record<string, unknown>,
  ): void {
    const page = this.page(binding);
    if (method === "Runtime.executionContextsCleared") {
      page.executionContextId = null;
      return;
    }
    if (method === "Runtime.consoleAPICalled") {
      page.console.push({ id: page.nextConsoleId++, timestamp: Date.now(), ...params });
      if (page.console.length > 500) page.console.splice(0, page.console.length - 500);
      return;
    }
    if (method === "Network.requestWillBeSent") {
      page.network.push({ id: page.nextNetworkId++, timestamp: Date.now(), ...params });
      if (page.network.length > 500) page.network.splice(0, page.network.length - 500);
    }
    if (method === "Network.responseReceived") {
      page.network.push({ type: "response", timestamp: Date.now(), ...params });
      if (page.network.length > 500) page.network.splice(0, page.network.length - 500);
    }
    if (method === "Network.loadingFailed") {
      page.network.push({ type: "failed", timestamp: Date.now(), ...params });
      if (page.network.length > 500) page.network.splice(0, page.network.length - 500);
    }
    if (method === "Tracing.dataCollected" && page.traceActive && Array.isArray(params.value)) {
      page.traceEvents.push(...params.value.filter((value): value is Record<string, unknown> => Boolean(value && typeof value === "object")));
      if (page.traceEvents.length > 10_000) page.traceEvents.splice(0, page.traceEvents.length - 10_000);
    }
    if (method === "Tracing.tracingComplete") page.traceActive = false;
    if (method === "Tracing.tracingComplete") page.traceActive = false;
    if (method === "Page.javascriptDialogOpening") page.dialog = params;
    if (method === "Page.javascriptDialogClosed") page.dialog = null;
    if (method === "HeapProfiler.addHeapSnapshotChunk") {
      const chunk = typeof params.chunk === "string" ? params.chunk : "";
      if (chunk) page.heapSnapshotChunks.push(chunk);
    }
  }
}

function empty(): { result: BrowserCommandResult } {
  return { result: { type: "empty" } };
}

function normalizeError(cause: unknown) {
  const source = cause instanceof Error ? cause : new Error(String(cause));
  const separator = source.message.indexOf(":");
  const code = separator > 0 ? source.message.slice(0, separator) : "browser_worker_failed";
  const message = separator > 0 ? source.message.slice(separator + 1) : source.message;
  return {
    code,
    message,
    recoverable: !code.includes("permission") && !code.includes("protocol"),
    side_effect_started: false,
    diagnostic: source.stack ?? null,
  };
}

function protocolFailure(code: string, message: string): Error {
  const error = new Error(`${code}:${message}`);
  error.name = "BrowserAutomationError";
  return error;
}

function safeInteger(value: unknown, fallback: number): number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0
    ? value
    : fallback;
}

function finiteNumber(value: unknown, name: string): number {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    throw protocolFailure("browser_argument_invalid", `${name} must be a finite number`);
  }
  return value;
}

function networkConditions(value: string): Record<string, unknown> {
  const presets: Record<string, { latency: number; downloadThroughput: number; uploadThroughput: number }> = {
    "slow 3g": { latency: 400, downloadThroughput: 500_000, uploadThroughput: 500_000 },
    "fast 3g": { latency: 150, downloadThroughput: 1_500_000, uploadThroughput: 750_000 },
    "slow 4g": { latency: 80, downloadThroughput: 4_000_000, uploadThroughput: 2_000_000 },
    "fast 4g": { latency: 20, downloadThroughput: 12_000_000, uploadThroughput: 6_000_000 },
  };
  if (value === "offline") return { offline: true, latency: 0, downloadThroughput: -1, uploadThroughput: -1 };
  const preset = presets[value.toLowerCase()];
  if (!preset) throw protocolFailure("browser_network_preset_invalid", `unknown network preset: ${value}`);
  return { offline: false, ...preset };
}

function snapshotTarget(args: Record<string, unknown>, prefix = ""): BrowserSnapshotTarget {
  const candidate = prefix && args[prefix] && typeof args[prefix] === "object"
    ? args[prefix] as Record<string, unknown>
    : args;
  const elementRef = typeof candidate.element_ref === "string"
    ? candidate.element_ref.trim()
    : typeof candidate.elementRef === "string"
      ? candidate.elementRef.trim()
      : "";
  const revision = Number(candidate.snapshot_revision ?? candidate.snapshotRevision);
  if (!elementRef || !Number.isSafeInteger(revision) || revision < 0) {
    throw protocolFailure("browser_snapshot_target_invalid", "element_ref and snapshot_revision are required");
  }
  return { element_ref: elementRef, snapshot_revision: revision };
}

function keyDescription(value: string): { key: string; code?: string; modifiers: number } {
  const parts = value.split("+").map((part) => part.trim()).filter(Boolean);
  const key = parts.pop() ?? value;
  let modifiers = 0;
  for (const modifier of parts) {
    switch (modifier.toLowerCase()) {
      case "alt": modifiers |= 1; break;
      case "control":
      case "ctrl": modifiers |= 2; break;
      case "meta":
      case "command":
      case "cmd": modifiers |= 4; break;
      case "shift": modifiers |= 8; break;
    }
  }
  const aliases: Record<string, { key: string; code: string }> = {
    enter: { key: "Enter", code: "Enter" },
    tab: { key: "Tab", code: "Tab" },
    escape: { key: "Escape", code: "Escape" },
    backspace: { key: "Backspace", code: "Backspace" },
    delete: { key: "Delete", code: "Delete" },
    arrowup: { key: "ArrowUp", code: "ArrowUp" },
    arrowdown: { key: "ArrowDown", code: "ArrowDown" },
    arrowleft: { key: "ArrowLeft", code: "ArrowLeft" },
    arrowright: { key: "ArrowRight", code: "ArrowRight" },
    space: { key: " ", code: "Space" },
  };
  const alias = aliases[key.toLowerCase()];
  return alias ? { ...alias, modifiers } : { key, code: key.length === 1 ? `Key${key.toUpperCase()}` : key, modifiers };
}
