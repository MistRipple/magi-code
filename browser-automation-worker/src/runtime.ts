import { createHash, randomUUID } from "node:crypto";
import type {
  BrowserCommandOutcome,
  BrowserCommandResult,
  BrowserHostCommand,
  BrowserSnapshot,
  BrowserSnapshotTarget,
  BrowserAccessibilityNode,
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
  profilerActive: boolean;
  coverageActive: boolean;
  heapSnapshot: HeapSnapshotData | null;
  previousHeapSnapshot: HeapSnapshotData | null;
  cdpDomainsReady: boolean;
  cdpDomainsPromise: Promise<void> | null;
}

interface HeapSnapshotData {
  meta: Record<string, unknown>;
  nodes: number[];
  edges: number[];
  strings: string[];
  nodeFieldIndex: Record<string, number>;
  edgeFieldIndex: Record<string, number>;
  nodeTypes: string[];
  edgeTypes: string[];
  nodeFieldCount: number;
  edgeFieldCount: number;
  edgeStarts: number[];
}

interface HeapEdgeRecord {
  type: string;
  name: string | number;
  to_node: number;
  to: ReturnType<typeof heapNode>;
}

export class BrowserAutomationRuntime {
  readonly #cdp: CdpClient;
  readonly #pages = new Map<string, PageRuntimeState>();

  readonly #workerEpoch: string;

  constructor(cdp: CdpClient, workerEpoch: string = randomUUID()) {
    this.#cdp = cdp;
    this.#workerEpoch = workerEpoch;
    cdp.onEvent((binding, method, params) => this.onCdpEvent(binding, method, params));
  }

  rebind(bindings: BrowserSurfaceBinding[]): void {
    for (const binding of bindings) this.page(binding);
  }

  async execute(
    callId: string,
    binding: BrowserSurfaceBinding,
    command: BrowserHostCommand,
  ): Promise<WorkerCommandResponse> {
    try {
      if (command.type !== "ping") await this.ensureCdpDomains(binding);
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
            payload: await this.snapshot(
              binding,
              command.payload.limits,
              command.payload.navigation_revision,
              command.payload.snapshot_revision,
            ),
          },
        };
      case "set_annotations":
        return {
          result: {
            type: "json",
            payload: {
              value: await this.setAnnotations(binding, command.payload.annotations),
            },
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
      && current.binding.navigation_revision === binding.navigation_revision
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
      profilerActive: current?.profilerActive ?? false,
      coverageActive: current?.coverageActive ?? false,
      heapSnapshot: current?.heapSnapshot ?? null,
      previousHeapSnapshot: current?.previousHeapSnapshot ?? null,
      cdpDomainsReady: false,
      cdpDomainsPromise: null,
    };
    this.#pages.set(binding.surface_id, next);
    return next;
  }

  private async ensureCdpDomains(binding: BrowserSurfaceBinding): Promise<void> {
    const page = this.page(binding);
    if (page.cdpDomainsReady) return;
    if (!page.cdpDomainsPromise) {
      page.cdpDomainsPromise = Promise.all([
        this.#cdp.send(binding, "Page.enable"),
        this.#cdp.send(binding, "Runtime.enable"),
        this.#cdp.send(binding, "Network.enable"),
      ]).then(() => {
        page.cdpDomainsReady = true;
      }).finally(() => {
        page.cdpDomainsPromise = null;
      });
    }
    await page.cdpDomainsPromise;
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
    await this.evaluate(
      binding,
      `${INSTALL_PAGE_RUNTIME}(${JSON.stringify(this.#workerEpoch)})`,
      true,
    );
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
    navigationRevision: number,
    snapshotRevision: number,
  ): Promise<BrowserSnapshot> {
    if (navigationRevision !== binding.navigation_revision) {
      throw protocolFailure(
        "browser_navigation_revision_mismatch",
        `expected ${binding.navigation_revision}, received ${navigationRevision}`,
      );
    }
    if (!Number.isSafeInteger(snapshotRevision) || snapshotRevision < 0) {
      throw protocolFailure("browser_snapshot_revision_invalid", "snapshot_revision is invalid");
    }
    const value = await this.evaluate<Omit<BrowserSnapshot, "tab_id" | "navigation_revision" | "continuation_refs">>(
      binding,
      `globalThis.__magiBrowserAutomation.snapshot(${safeInteger(limits.max_nodes, 400)}, ${safeInteger(limits.max_text_bytes, 32768)}, ${snapshotRevision})`,
    );
    if (value.snapshot_revision !== snapshotRevision) {
      throw protocolFailure(
        "browser_snapshot_revision_mismatch",
        `expected ${snapshotRevision}, received ${value.snapshot_revision}`,
      );
    }
    const accessibilityTree = await this.accessibilityTree(binding, safeInteger(limits.max_nodes, 400), snapshotRevision);
    return {
      tab_id: binding.tab_id,
      navigation_revision: binding.navigation_revision,
      ...value,
      continuation_refs: [],
      accessibility_tree: accessibilityTree,
    };
  }

  private async accessibilityTree(binding: BrowserSurfaceBinding, maxNodes: number, snapshotRevision: number): Promise<BrowserAccessibilityNode[]> {
    const response = await this.#cdp.send<{ nodes?: Array<Record<string, unknown>> }>(
      binding,
      "Accessibility.getFullAXTree",
      {},
    );
    const nodes = Array.isArray(response.nodes) ? response.nodes : [];
    const mapped = [];
    for (const node of nodes.slice(0, maxNodes)) {
      const backendDomNodeId = typeof node.backendDOMNodeId === "number" ? node.backendDOMNodeId : null;
      const elementRef = backendDomNodeId === null
        ? null
        : await this.accessibilityElementRef(binding, backendDomNodeId, snapshotRevision);
      mapped.push({
      node_id: String(node.nodeId ?? ""),
      element_ref: elementRef,
      parent_id: node.parentId == null ? null : String(node.parentId),
      child_ids: Array.isArray(node.childIds) ? node.childIds.map(String) : [],
      role: axValue(node.role),
      name: axValue(node.name),
      value: axValue(node.value),
      description: axValue(node.description),
      ignored: Boolean(node.ignored),
      properties: Array.isArray(node.properties)
        ? Object.fromEntries(node.properties
          .filter((property): property is { name: string; value?: unknown } => Boolean(property && typeof property === "object" && typeof (property as { name?: unknown }).name === "string"))
          .map((property) => [property.name, axValueObject(property.value)]))
        : {},
      actions: Array.isArray(node.actions)
        ? node.actions.map((action) => typeof action === "object" && action !== null ? String((action as { name?: unknown }).name ?? "") : String(action)).filter(Boolean)
        : [],
      backend_dom_node_id: backendDomNodeId,
      });
    }
    return mapped;
  }

  private async accessibilityElementRef(
    binding: BrowserSurfaceBinding,
    backendDomNodeId: number,
    snapshotRevision: number,
  ): Promise<string | null> {
    try {
      const described = await this.#cdp.send<{ node?: { nodeId?: number } }>(binding, "DOM.describeNode", { backendNodeId: backendDomNodeId });
      if (!described.node?.nodeId) return null;
      const resolved = await this.#cdp.send<{ object?: { objectId?: string } }>(binding, "DOM.resolveNode", { nodeId: described.node.nodeId });
      const objectId = resolved.object?.objectId;
      if (!objectId) return null;
      const result = await this.#cdp.send<{ result?: { value?: unknown } }>(binding, "Runtime.callFunctionOn", {
        objectId,
        functionDeclaration: `function() { return globalThis.__magiBrowserAutomation.elementRef(this); }`,
        returnByValue: true,
        awaitPromise: false,
        arguments: [],
      });
      return typeof result.result?.value === "string" ? result.result.value : null;
    } catch {
      return null;
    }
  }

  private async setAnnotations(binding: BrowserSurfaceBinding, annotations: unknown[]): Promise<unknown> {
    if (!Array.isArray(annotations)) {
      throw protocolFailure("browser_annotations_invalid", "annotations must be an array");
    }
    return this.evaluate(
      binding,
      `globalThis.__magiBrowserAutomation.setAnnotations(${JSON.stringify(annotations)})`,
    );
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
    const viewport = await this.pageViewport(binding);
    await this.#cdp.send(binding, "Input.dispatchMouseEvent", {
      type: "mouseWheel",
      x: viewport.width / 2,
      y: viewport.height / 2,
      deltaX,
      deltaY,
    });
  }

  private async pageViewport(binding: BrowserSurfaceBinding): Promise<{ width: number; height: number }> {
    const viewport = await this.evaluate<{ width: number; height: number }>(
      binding,
      "globalThis.__magiBrowserAutomation.viewport()",
    );
    if (!Number.isFinite(viewport?.width) || !Number.isFinite(viewport?.height)
      || viewport.width <= 0 || viewport.height <= 0) {
      throw protocolFailure("browser_viewport_invalid", "page viewport is unavailable");
    }
    return viewport;
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
      const viewport = await this.pageViewport(binding);
      clip = {
        x: input.clip.x * viewport.width,
        y: input.clip.y * viewport.height,
        width: input.clip.width * viewport.width,
        height: input.clip.height * viewport.height,
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
        return this.thirdParty(binding, args);
      case "heap":
        return this.heap(binding, args);
      case "scripts":
        return this.newDocumentScript(binding, args);
      case "overlay":
        return this.overlay(binding, args);
      case "lighthouse":
        return this.lighthouse(binding, args);
      case "upload_file":
        return this.uploadFile(binding, args);
      default:
        throw protocolFailure("browser_devtools_operation_unsupported", operation);
    }
  }

  private async uploadFile(binding: BrowserSurfaceBinding, args: Record<string, unknown>): Promise<unknown> {
    const snapshot = snapshotTarget(args);
    const target = await this.target(binding, snapshot);
    const paths = Array.isArray(args.file_paths)
      ? args.file_paths.filter((value): value is string => typeof value === "string" && value.trim().length > 0)
      : typeof args.file_path === "string" && args.file_path.trim() ? [args.file_path.trim()] : [];
    if (!paths.length) throw protocolFailure("browser_upload_file_invalid", "file_path or file_paths is required");
    for (const path of paths) {
      if (path.includes("\0") || path.length > 4096) {
        throw protocolFailure("browser_upload_file_invalid", "file path is invalid");
      }
    }
    if (!target.editable) throw protocolFailure("browser_upload_target_invalid", "target must be a file input");
    const document = await this.#cdp.send<{ root: { nodeId: number } }>(binding, "DOM.getDocument", { depth: -1, pierce: true });
    const selector = await this.evaluate<string | null>(
      binding,
      "(() => { const e = globalThis.__magiBrowserAutomation.resolve(" + JSON.stringify(snapshot.element_ref) + ", " + safeInteger(snapshot.snapshot_revision, 0) + "); return e instanceof HTMLInputElement && e.type === 'file' ? globalThis.__magiBrowserAutomation.cssPath(e) : null; })()",
    );
    if (!selector) throw protocolFailure("browser_upload_target_invalid", "target must be a file input");
    const node = await this.#cdp.send<{ nodeId?: number }>(binding, "DOM.querySelector", { nodeId: document.root.nodeId, selector });
    if (!node.nodeId) throw protocolFailure("browser_upload_target_invalid", "file input is no longer connected");
    await this.#cdp.send(binding, "DOM.setFileInputFiles", { nodeId: node.nodeId, files: paths });
    await this.#cdp.send(binding, "DOM.focus", { nodeId: node.nodeId });
    return { uploaded: paths.map((path) => path.split(/[\\\\/]/).pop() || path), count: paths.length };
  }

  private async thirdParty(binding: BrowserSurfaceBinding, args: Record<string, unknown>): Promise<unknown> {
    const page = this.page(binding);
    if (args.action === "clear") {
      page.network.length = 0;
      return { cleared: true };
    }
    const entries = page.network.filter((entry) => entry.event_type === "response");
    const origin = await this.evaluate<string>(binding, "location.origin");
    const groups = new Map<string, { origin: string; requests: number; bytes: number; resource_types: Record<string, number>; urls: string[] }>();
    for (const entry of entries) {
      const url = typeof entry.url === "string" ? entry.url : "";
      if (!url) continue;
      let resourceOrigin: string;
      try { resourceOrigin = new URL(url).origin; } catch { continue; }
      if (resourceOrigin === origin || resourceOrigin === "null") continue;
      const group = groups.get(resourceOrigin) ?? { origin: resourceOrigin, requests: 0, bytes: 0, resource_types: {}, urls: [] };
      group.requests += 1;
      group.bytes += Number(entry.encodedDataLength ?? entry.encoded_data_length ?? 0) || 0;
      const type = String(entry.resource_type ?? entry.resourceType ?? "other");
      group.resource_types[type] = (group.resource_types[type] ?? 0) + 1;
      if (group.urls.length < 20) group.urls.push(url);
      groups.set(resourceOrigin, group);
    }
    return { page_origin: origin, entries: [...groups.values()].sort((a, b) => b.bytes - a.bytes || b.requests - a.requests), total_requests: entries.length };
  }

  private async lighthouse(binding: BrowserSurfaceBinding, args: Record<string, unknown>): Promise<unknown> {
    const mode = String(args.mode ?? "snapshot");
    if (mode !== "snapshot" && mode !== "navigation") throw protocolFailure("browser_lighthouse_invalid", "mode must be snapshot or navigation");
    const lighthouseModule = await import("lighthouse");
    const lighthouse = lighthouseModule.default;
    const pageHandle = createLighthousePage(this.#cdp, binding, () => this.page(binding));
    const page = pageHandle as unknown as NonNullable<Parameters<typeof lighthouse>[3]>;
    const flags = {
      logLevel: "error" as const,
      disableStorageReset: true,
      output: "json" as const,
      ...(args.device === "desktop" ? { preset: "desktop" as const } : {}),
    };
    const result = mode === "navigation"
      ? await lighthouse(await page.url(), flags, undefined, page)
      : await lighthouseModule.snapshot(page, { flags });
    if (!result) throw protocolFailure("browser_lighthouse_failed", "Lighthouse returned no result");
    const lhr = (result as unknown as { lhr?: Record<string, unknown> }).lhr ?? {};
    return { mode, url: await pageHandle.url(), lighthouse_version: lhr.lighthouseVersion ?? null, categories: lhr.categories ?? {}, audits: lhr.audits ?? {}, timing: lhr.timing ?? null };
  }

  private async fillForm(binding: BrowserSurfaceBinding, args: Record<string, unknown>): Promise<unknown> {
    if (!Array.isArray(args.fields) || args.fields.length === 0) {
      throw protocolFailure("browser_fill_form_invalid", "fields must be a non-empty array");
    }
    const fields = args.fields;
    let filled = 0;
    for (const raw of fields) {
      if (!raw || typeof raw !== "object") {
        throw protocolFailure("browser_fill_form_invalid", "each fields item must be an object");
      }
      const field = raw as Record<string, unknown>;
      if (!Object.prototype.hasOwnProperty.call(field, "value")) {
        throw protocolFailure("browser_fill_form_invalid", "each fields item must include value");
      }
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
    const manifest = await this.#cdp.send<Record<string, unknown>>(binding, "Page.getAppManifest").catch(() => ({}));
    const pageState = await this.evaluate<Record<string, unknown>>(binding, `(() => ({
      manifest: document.querySelector('link[rel="manifest"]')?.href || null,
      serviceWorkerControlled: Boolean(navigator.serviceWorker?.controller),
      secureContext: globalThis.isSecureContext,
      displayMode: matchMedia('(display-mode: standalone)').matches ? 'standalone' : 'browser',
      hasServiceWorker: Boolean(navigator.serviceWorker),
      scope: navigator.serviceWorker?.controller?.scriptURL || null
    }))()`);
    return { ...pageState, app_manifest: manifest };
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
      return { entries: entries.filter((entry) => entry.event_type === "failed") };
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
    if (action === "profile_start") {
      if (page.profilerActive) return { started: true, already_active: true };
      await this.#cdp.send(binding, "Profiler.enable");
      await this.#cdp.send(binding, "Profiler.start");
      page.profilerActive = true;
      return { started: true, type: "cpu_profile" };
    }
    if (action === "profile_stop") {
      if (!page.profilerActive) return { stopped: false, profile: null };
      const profile = await this.#cdp.send(binding, "Profiler.stop");
      page.profilerActive = false;
      await this.#cdp.send(binding, "Profiler.disable");
      return { stopped: true, profile };
    }
    if (action === "coverage_start") {
      if (page.coverageActive) return { started: true, already_active: true };
      await this.#cdp.send(binding, "Profiler.enable");
      await this.#cdp.send(binding, "Profiler.startPreciseCoverage", { callCount: true, detailed: true });
      page.coverageActive = true;
      return { started: true, type: "precise_coverage" };
    }
    if (action === "coverage_take") {
      if (!page.coverageActive) throw protocolFailure("browser_coverage_not_started", "coverage_start must run first");
      return this.#cdp.send(binding, "Profiler.takePreciseCoverage");
    }
    if (action === "coverage_stop") {
      if (!page.coverageActive) return { stopped: false, coverage: [] };
      const coverage = await this.#cdp.send(binding, "Profiler.takePreciseCoverage");
      await this.#cdp.send(binding, "Profiler.stopPreciseCoverage");
      await this.#cdp.send(binding, "Profiler.disable");
      page.coverageActive = false;
      return { stopped: true, coverage };
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
      const parsed = parseHeapSnapshot(snapshot);
      page.previousHeapSnapshot = page.heapSnapshot;
      page.heapSnapshot = parsed;
      return { byte_length: Buffer.byteLength(snapshot, "utf8"), ...heapSummary(parsed) };
    }
    if (action === "close_snapshot") {
      page.heapSnapshotChunks = [];
      page.heapSnapshot = null;
      page.previousHeapSnapshot = null;
      await this.#cdp.send(binding, "HeapProfiler.disable");
      return { closed: true };
    }
    if (!page.heapSnapshot) {
      throw protocolFailure("browser_heap_snapshot_missing", "take_snapshot must run before heap analysis");
    }
    return analyzeHeap(page.heapSnapshot, page.previousHeapSnapshot, action, args);
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

  private onCdpEvent(
    binding: BrowserSurfaceBinding,
    method: string,
    params: Record<string, unknown>,
    sessionId?: string,
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
      page.network.push({ event_type: "request", id: page.nextNetworkId++, timestamp: Date.now(), ...params, ...(params.request && typeof params.request === "object" ? params.request : {}), resource_type: params.type ?? null });
      if (page.network.length > 500) page.network.splice(0, page.network.length - 500);
    }
    if (method === "Network.responseReceived") {
      page.network.push({ event_type: "response", timestamp: Date.now(), ...params, ...(params.response && typeof params.response === "object" ? params.response : {}), resource_type: params.type ?? null });
      if (page.network.length > 500) page.network.splice(0, page.network.length - 500);
    }
    if (method === "Network.loadingFailed") {
      page.network.push({ event_type: "failed", timestamp: Date.now(), ...params });
      if (page.network.length > 500) page.network.splice(0, page.network.length - 500);
    }
    if (method === "Network.loadingFinished") {
      const requestId = String(params.requestId ?? "");
      const encodedDataLength = Number(params.encodedDataLength ?? 0);
      const request = [...page.network].reverse().find((entry) => entry.event_type === "request" && String(entry.requestId ?? "") === requestId);
      const response = [...page.network].reverse().find((entry) => entry.event_type === "response" && String(entry.requestId ?? "") === requestId);
      if (Number.isFinite(encodedDataLength)) {
        if (request) request.encodedDataLength = encodedDataLength;
        if (response) response.encodedDataLength = encodedDataLength;
      }
    }
    if (!sessionId && method === "Tracing.dataCollected" && page.traceActive && Array.isArray(params.value)) {
      page.traceEvents.push(...params.value.filter((value): value is Record<string, unknown> => Boolean(value && typeof value === "object")));
      if (page.traceEvents.length > 10_000) page.traceEvents.splice(0, page.traceEvents.length - 10_000);
    }
    if (!sessionId && method === "Tracing.tracingComplete") page.traceActive = false;
    if (!sessionId && method === "Page.javascriptDialogOpening") page.dialog = params;
    if (!sessionId && method === "Page.javascriptDialogClosed") page.dialog = null;
    if (!sessionId && method === "HeapProfiler.addHeapSnapshotChunk") {
      const chunk = typeof params.chunk === "string" ? params.chunk : "";
      if (chunk) page.heapSnapshotChunks.push(chunk);
    }
    if (!sessionId && method === "Profiler.consoleProfileFinished") {
      page.traceEvents.push({ type: "cpu_profile", ...params });
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

function axValue(value: unknown): string | null {
  const raw = axValueObject(value);
  return raw == null ? null : String(raw);
}

function axValueObject(value: unknown): unknown {
  if (!value || typeof value !== "object") return value ?? null;
  const candidate = value as { value?: unknown };
  return "value" in candidate ? candidate.value : value;
}

function parseHeapSnapshot(raw: string): HeapSnapshotData {
  let parsed: { snapshot?: { meta?: Record<string, unknown> }; nodes?: number[]; edges?: number[]; strings?: string[] };
  try {
    parsed = JSON.parse(raw) as typeof parsed;
  } catch {
    throw protocolFailure("browser_heap_snapshot_invalid", "heap snapshot is not valid JSON");
  }
  const meta = parsed.snapshot?.meta ?? {};
  const nodeFields = Array.isArray(meta.node_fields) ? meta.node_fields.map(String) : [];
  const edgeFields = Array.isArray(meta.edge_fields) ? meta.edge_fields.map(String) : [];
  const nodeTypesValue = meta.node_types;
  const edgeTypesValue = meta.edge_types;
  const nodeTypes = Array.isArray(nodeTypesValue) && Array.isArray(nodeTypesValue[0]) ? (nodeTypesValue[0] as unknown[]).map(String) : [];
  const edgeTypes = Array.isArray(edgeTypesValue) && Array.isArray(edgeTypesValue[0]) ? (edgeTypesValue[0] as unknown[]).map(String) : [];
  const nodeFieldCount = nodeFields.length;
  const edgeFieldCount = edgeFields.length;
  const edgeCountIndex = nodeFields.indexOf("edge_count");
  const edgeStarts: number[] = [];
  let edgeStart = 0;
  const nodeValues = Array.isArray(parsed.nodes) ? parsed.nodes : [];
  const nodeCount = nodeFieldCount ? Math.floor(nodeValues.length / nodeFieldCount) : 0;
  for (let index = 0; index < nodeCount; index += 1) {
    edgeStarts.push(edgeStart);
    edgeStart += edgeCountIndex >= 0 ? Number(nodeValues[index * nodeFieldCount + edgeCountIndex] ?? 0) : 0;
  }
  return {
    meta,
    nodes: Array.isArray(parsed.nodes) ? parsed.nodes : [],
    edges: Array.isArray(parsed.edges) ? parsed.edges : [],
    strings: Array.isArray(parsed.strings) ? parsed.strings : [],
    nodeFieldIndex: Object.fromEntries(nodeFields.map((field, index) => [field, index])),
    edgeFieldIndex: Object.fromEntries(edgeFields.map((field, index) => [field, index])),
    nodeTypes,
    edgeTypes,
    nodeFieldCount,
    edgeFieldCount,
    edgeStarts,
  };
}

function heapNode(snapshot: HeapSnapshotData, index: number): { id: number; type: string; name: string; self_size: number; edge_count: number } {
  const offset = index * snapshot.nodeFieldCount;
  const node = snapshot.nodes;
  const stringId = Number(node[offset + (snapshot.nodeFieldIndex.name ?? 1)] ?? 0);
  return {
    id: index,
    type: snapshot.nodeTypes[Number(node[offset + (snapshot.nodeFieldIndex.type ?? 0)] ?? 0)] ?? "unknown",
    name: snapshot.strings[stringId] ?? "",
    self_size: Number(node[offset + (snapshot.nodeFieldIndex.self_size ?? 3)] ?? 0),
    edge_count: Number(node[offset + (snapshot.nodeFieldIndex.edge_count ?? 4)] ?? 0),
  };
}

function heapSummary(snapshot: HeapSnapshotData): Record<string, unknown> {
  const count = snapshot.nodeFieldCount ? Math.floor(snapshot.nodes.length / snapshot.nodeFieldCount) : 0;
  const types: Record<string, { count: number; self_size: number }> = {};
  let selfSize = 0;
  for (let index = 0; index < count; index += 1) {
    const node = heapNode(snapshot, index);
    const group = types[node.type] ?? { count: 0, self_size: 0 };
    group.count += 1;
    group.self_size += node.self_size;
    types[node.type] = group;
    selfSize += node.self_size;
  }
  return {
    node_count: count,
    edge_count: snapshot.edgeFieldCount ? Math.floor(snapshot.edges.length / snapshot.edgeFieldCount) : 0,
    self_size: selfSize,
    types,
    snapshot_format: snapshot.meta,
  };
}

function analyzeHeap(snapshot: HeapSnapshotData, previous: HeapSnapshotData | null, action: string, args: Record<string, unknown>): Record<string, unknown> {
  if (action === "summary") return heapSummary(snapshot);
  if (action === "compare_snapshots") {
    const current = heapSummary(snapshot);
    const base = previous ? heapSummary(previous) : null;
    return { current, base, delta: base ? {
      node_count: Number(current.node_count) - Number(base.node_count),
      edge_count: Number(current.edge_count) - Number(base.edge_count),
      self_size: Number(current.self_size) - Number(base.self_size),
    } : null };
  }
  const count = snapshot.nodeFieldCount ? Math.floor(snapshot.nodes.length / snapshot.nodeFieldCount) : 0;
  const pageIndex = safeInteger(args.page_index, 0);
  const pageSize = Math.min(500, Math.max(1, safeInteger(args.page_size, 100)));
  if (action === "details" || action === "class_nodes") {
    const className = typeof args.class_name === "string" ? args.class_name : null;
    const classId = typeof args.class_id === "number" ? args.class_id : null;
    const nodes = Array.from({ length: count }, (_, index) => heapNode(snapshot, index))
      .filter((node) => action !== "class_nodes" || (className ? node.name === className : classId == null || node.id === classId))
      .sort((left, right) => right.self_size - left.self_size);
    return { action, page_index: pageIndex, page_size: pageSize, nodes: nodes.slice(pageIndex * pageSize, (pageIndex + 1) * pageSize) };
  }
  if (action === "dominators") {
    return { action, page_index: pageIndex, page_size: pageSize, nodes: heapDominators(snapshot).slice(pageIndex * pageSize, (pageIndex + 1) * pageSize) };
  }
  if (action === "duplicate_strings") {
    const counts = new Map<string, number>();
    for (const value of snapshot.strings) counts.set(value, (counts.get(value) ?? 0) + 1);
    return { strings: [...counts.entries()].filter(([, count]) => count > 1).map(([value, count]) => ({ value, count })).sort((a, b) => b.count - a.count).slice(0, pageSize) };
  }
  const nodeId = typeof args.node_id === "number" ? args.node_id : 0;
  if (action === "edges") {
    return { node_id: nodeId, edges: heapEdges(snapshot, nodeId).slice(0, pageSize) };
  }
  if (action === "object_details") return { node: heapNode(snapshot, Math.max(0, Math.min(count - 1, nodeId))), edges: heapEdges(snapshot, nodeId).slice(0, pageSize) };
  if (action === "retainers" || action === "retaining_paths") {
    return { node_id: nodeId, paths: heapRetainingPaths(snapshot, nodeId, Math.min(64, Math.max(1, safeInteger(args.max_depth, 8))), pageSize) };
  }
  return { action, page_index: pageIndex, page_size: pageSize, nodes: [] };
}

function heapEdges(snapshot: HeapSnapshotData, nodeIndex: number): HeapEdgeRecord[] {
  const offset = nodeIndex * snapshot.nodeFieldCount;
  if (nodeIndex < 0 || offset < 0 || offset >= snapshot.nodes.length || snapshot.edgeFieldCount <= 0) return [];
  const start = Number(snapshot.edgeStarts[nodeIndex] ?? 0);
  const count = Number(snapshot.nodes[offset + (snapshot.nodeFieldIndex.edge_count ?? 4)] ?? 0);
  const edges: HeapEdgeRecord[] = [];
  for (let index = 0; index < count; index += 1) {
    const edgeOffset = start + index * snapshot.edgeFieldCount;
    if (edgeOffset < 0 || edgeOffset + snapshot.edgeFieldCount > snapshot.edges.length) break;
    const type = snapshot.edgeTypes[Number(snapshot.edges[edgeOffset + (snapshot.edgeFieldIndex.type ?? 0)] ?? 0)] ?? "unknown";
    const nameId = Number(snapshot.edges[edgeOffset + (snapshot.edgeFieldIndex.name_or_index ?? 1)] ?? 0);
    const toNode = Number(snapshot.edges[edgeOffset + (snapshot.edgeFieldIndex.to_node ?? 2)] ?? 0) / snapshot.nodeFieldCount;
    const edgeType = String(type);
    const name = edgeType === "element" || edgeType === "hidden" || edgeType === "weak"
      ? nameId
      : snapshot.strings[nameId] ?? String(nameId);
    if (Number.isInteger(toNode) && toNode >= 0 && toNode < snapshot.nodes.length / snapshot.nodeFieldCount) {
      edges.push({ type, name, to_node: toNode, to: heapNode(snapshot, toNode) });
    }
  }
  return edges;
}

function heapDominators(snapshot: HeapSnapshotData): Array<Record<string, unknown>> {
  const count = snapshot.nodeFieldCount ? Math.floor(snapshot.nodes.length / snapshot.nodeFieldCount) : 0;
  if (!count) return [];
  const predecessors = Array.from({ length: count }, () => new Set<number>());
  for (let source = 0; source < count; source += 1) {
    for (const edge of heapEdges(snapshot, source)) predecessors[edge.to_node]?.add(source);
  }
  const root = 0;
  const dominators = Array.from({ length: count }, (_, index) => new Set(index === root ? [root] : Array.from({ length: count }, (_, candidate) => candidate)));
  let changed = true;
  while (changed) {
    changed = false;
    for (let node = 0; node < count; node += 1) {
      if (node === root) continue;
      const parents = [...(predecessors[node] ?? [])].filter((parent) => parent !== node);
      if (!parents.length) continue;
      const next = new Set<number>(dominatorSetIntersection(parents.map((parent) => dominators[parent] ?? new Set<number>())));
      next.add(node);
      if (!sameNumberSet(next, dominators[node] ?? new Set<number>())) {
        dominators[node] = next;
        changed = true;
      }
    }
  }
  return dominators.map((set, node) => {
    const strict = [...set].filter((candidate) => candidate !== node);
    const immediate = strict.find((candidate) => strict.every((other) => other === candidate || !dominators[other]?.has(candidate))) ?? null;
    return { ...heapNode(snapshot, node), dominator_count: set.size, immediate_dominator: immediate };
  }).sort((left, right) => Number(right.self_size) - Number(left.self_size));
}

function dominatorSetIntersection(sets: Array<Set<number>>): number[] {
  const first = sets[0];
  if (!first) return [];
  return [...first].filter((value) => sets.every((set) => set.has(value)));
}

function sameNumberSet(left: Set<number>, right: Set<number>): boolean {
  return left.size === right.size && [...left].every((value) => right.has(value));
}

function heapRetainingPaths(snapshot: HeapSnapshotData, target: number, maxDepth: number, maxPaths: number): number[][] {
  const reverse = new Map<number, number[]>();
  const count = snapshot.nodeFieldCount ? Math.floor(snapshot.nodes.length / snapshot.nodeFieldCount) : 0;
  for (let source = 0; source < count; source += 1) {
    for (const edge of heapEdges(snapshot, source)) {
      const destination = Number(edge.to_node);
      const parents = reverse.get(destination) ?? [];
      if (parents.length < 16) parents.push(source);
      reverse.set(destination, parents);
    }
  }
  const paths: number[][] = [];
  const queue: number[][] = [[target]];
  const visited = new Set<string>();
  while (queue.length && paths.length < maxPaths) {
    const path = queue.shift()!;
    const current = path[path.length - 1] ?? target;
    const parents = reverse.get(current) ?? [];
    if (!parents.length || path.length >= maxDepth) { paths.push(path); continue; }
    for (const parent of parents) {
      if (path.includes(parent)) continue;
      const next = [parent, ...path];
      const key = next.join(",");
      if (!visited.has(key)) { visited.add(key); queue.push(next); }
    }
  }
  return paths;
}

type LighthouseListener = (...args: unknown[]) => void;

class LighthouseCdpSession {
  readonly #cdp: CdpClient;
  readonly #binding: BrowserSurfaceBinding;
  readonly #sessionId: string | undefined;
  readonly #targetInfo: Record<string, unknown>;
  readonly #listeners = new Map<string, Set<LighthouseListener>>();
  readonly #unsubscribe: () => void;

  constructor(
    cdp: CdpClient,
    binding: BrowserSurfaceBinding,
    sessionId?: string,
    targetInfo?: Record<string, unknown>,
  ) {
    this.#cdp = cdp;
    this.#binding = binding;
    this.#sessionId = sessionId;
    this.#targetInfo = targetInfo ?? { targetId: binding.target_id, type: "page" };
    this.#unsubscribe = cdp.onEvent((eventBinding, method, params, eventSessionId) => {
      if (eventBinding.surface_id !== binding.surface_id || eventBinding.surface_revision !== binding.surface_revision) return;
      if (eventBinding.navigation_revision !== binding.navigation_revision) return;
      if (eventSessionId !== this.#sessionId) return;
      this.emit(method, params);
    });
  }

  id(): string {
    return this.#sessionId ?? "magi-lighthouse-" + this.#binding.target_id;
  }

  on(event: string, listener: LighthouseListener): this {
    const listeners = this.#listeners.get(event) ?? new Set<LighthouseListener>();
    listeners.add(listener);
    this.#listeners.set(event, listeners);
    return this;
  }

  off(event: string, listener: LighthouseListener): this {
    this.#listeners.get(event)?.delete(listener);
    return this;
  }

  private emit(event: string, params: Record<string, unknown>): void {
    for (const listener of this.#listeners.get("*") ?? []) listener(event, params);
    for (const listener of this.#listeners.get(event) ?? []) listener(params);
    if (event === "Target.attachedToTarget") {
      const sessionId = typeof params.sessionId === "string" ? params.sessionId : "";
      const targetInfo = params.targetInfo && typeof params.targetInfo === "object"
        ? params.targetInfo as Record<string, unknown>
        : undefined;
      if (sessionId) {
        const child = new LighthouseCdpSession(this.#cdp, this.#binding, sessionId, targetInfo);
        for (const listener of this.#listeners.get("sessionattached") ?? []) listener(child);
      }
    }
  }

  async send(method: string, params: Record<string, unknown> = {}, options?: { timeout?: number }): Promise<unknown> {
    if (method === "Target.getTargetInfo") {
      return { targetInfo: { ...this.#targetInfo, url: this.#targetInfo.url ?? await this.pageUrl() } };
    }
    return this.#cdp.send(this.#binding, method, params, options?.timeout ?? 30_000, this.#sessionId);
  }

  async detach(): Promise<void> {
    if (this.#sessionId) {
      await this.#cdp.send(this.#binding, "Target.detachFromTarget", { sessionId: this.#sessionId }).catch(() => undefined);
    }
    this.#unsubscribe();
    this.#listeners.clear();
  }

  setTargetInfo(_targetInfo: unknown): void {}
  hasNextProtocolTimeout(): boolean { return false; }
  getNextProtocolTimeout(): number { return 30_000; }
  setNextProtocolTimeout(_timeout: number): void {}
  sendCommand(method: string, params?: Record<string, unknown>): Promise<unknown> { return this.send(method, params ?? {}); }
  sendCommandAndIgnore(method: string, params?: Record<string, unknown>): Promise<void> {
    return this.send(method, params ?? {}).then(() => undefined).catch(() => undefined);
  }
  onCrashPromise(): Promise<never> { return new Promise(() => undefined); }

  async pageUrl(): Promise<string> {
    const response = await this.#cdp.send<{ result?: { value?: unknown } }>(this.#binding, "Runtime.evaluate", {
      expression: "location.href",
      returnByValue: true,
      awaitPromise: false,
    }, 30_000, this.#sessionId);
    return typeof response?.result?.value === "string" ? response.result.value : "about:blank";
  }
}

function createLighthousePage(cdp: CdpClient, binding: BrowserSurfaceBinding, pageState: () => PageRuntimeState): {
  url(): Promise<string>;
  target(): { createCDPSession(): Promise<LighthouseCdpSession> };
} {
  return {
    async url() {
      const session = new LighthouseCdpSession(cdp, binding);
      try {
        return await session.pageUrl();
      } finally {
        await session.detach();
      }
    },
    target() {
      return {
        async createCDPSession() {
          pageState();
          return new LighthouseCdpSession(cdp, binding);
        },
      };
    },
  };
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
