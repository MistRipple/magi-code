import { createHash, randomUUID } from "node:crypto";
import { copyFile, mkdir, readFile, stat, unlink, writeFile } from "node:fs/promises";
import { basename, dirname, join } from "node:path";
import type {
  BrowserContext,
  CDPSession,
  ConsoleMessage,
  Dialog,
  Download,
  Frame,
  Page,
  Request,
  Response,
} from "playwright-core";
import type {
  Browser as PuppeteerBrowser,
  Page as PuppeteerPage,
  ScreenRecorder,
} from "puppeteer-core";
import { chromium, errors } from "playwright-core";
import { ControlFence, ProtocolFailure, requireSafeInteger } from "./control";
import { HeapSnapshotModel } from "./heap-snapshot";
import type {
  BinaryPayload,
  ClipboardText,
  CommandResult,
  HostCommand,
  HostControl,
  HostDeviceType,
  HostLogicalViewport,
  HostEvent,
  AgentCursorAction,
  HostHandshake,
  HostViewport,
  HitTest,
  MouseButton,
  NormalizedRect,
  PageState,
  ScreencastFrame,
  UserInputEvent,
} from "./protocol";
import { PROTOCOL_VERSION } from "./protocol";
import { SnapshotRegistry } from "./snapshot";

const PAGE_NAVIGATION_TIMEOUT_MILLIS = 15_000;
const ACTION_NAVIGATION_EXPECT_TIMEOUT_MILLIS = 150;
const POPUP_URL_TIMEOUT_MILLIS = 15_000;
const ACTION_EXPECTED_URL_TIMEOUT_MILLIS = 3_000;
const ACTION_DOM_STABLE_FOR_MILLIS = 100;
const ACTION_DOM_STABILITY_TIMEOUT_MILLIS = 2_000;
const ACTION_NETWORK_STABLE_FOR_MILLIS = 200;
const ACTION_NETWORK_SETTLEMENT_TIMEOUT_MILLIS = 5_000;
const ACTION_PAGE_METADATA_STABLE_FOR_MILLIS = 120;
const ACTION_PAGE_METADATA_TIMEOUT_MILLIS = 2_000;
const ACTION_PAGE_METADATA_POLL_MILLIS = 20;
const SCREENCAST_INITIAL_FRAME_TIMEOUT_MILLIS = 5_000;
const MAX_SCREENCAST_WIDTH = 7_680;
const MAX_SCREENCAST_HEIGHT = 4_320;
const MAX_CONSOLE_MESSAGES = 500;
const MAX_NETWORK_REQUESTS = 500;
const MAX_NETWORK_BODY_BYTES = 512 * 1024;
const MAX_TRACE_EVENTS = 100_000;

interface BrowserConsoleEntry {
  id: number;
  level: string;
  text: string;
  url: string;
  line: number;
  column: number;
  timestamp: number;
}

interface BrowserNetworkEntry {
  id: number;
  method: string;
  url: string;
  resourceType: string;
  requestHeaders: Record<string, string>;
  status?: number;
  statusText?: string;
  responseHeaders?: Record<string, string>;
  mimeType?: string;
  failure?: string;
  startedAt: number;
  finishedAt?: number;
  response?: Response;
}

interface BrowserDialogEntry {
  id: number;
  dialogType: string;
  message: string;
  defaultValue: string;
  openedAt: number;
  handledAs?: "accepted" | "dismissed";
}

interface PendingBrowserDialog {
  dialog: Dialog;
  entry: BrowserDialogEntry;
  timeout: NodeJS.Timeout;
}

interface BrowserTraceState {
  events: unknown[];
  startedAt: number;
  listener: (event: { value?: unknown[] }) => void;
}

interface BrowserRecordingState {
  recorder: ScreenRecorder;
  filePath: string;
  format: "mp4" | "webm";
  startedAt: number;
}

export interface BrowserHostConfig {
  profilePath: string;
  chromiumExecutable: string;
  runtimeVersion: string;
  hostVersion: string;
  playwrightVersion: string;
  runtimeEpoch: number;
  headless: boolean;
  deviceScaleFactor: number;
  downloadPath: string;
  maxActivePages: number;
  maxTabs: number;
}

export interface HostTransport {
  emit(event: HostEvent): void;
  emitScreencast(event: HostEvent, payload: Buffer): boolean;
}

export interface ExecutedCommand {
  result: CommandResult;
  binary?: Buffer;
}

interface PageRecord {
  readonly tabId: string;
  page?: Page;
  readonly snapshot: SnapshotRegistry;
  navigationRevision: number;
  frameSequence: number;
  blockedNavigationUrl?: string;
  restoringInitialPage: boolean;
  viewport: HostViewport;
  defaultUserAgent: string;
  defaultPlatform: string;
  currentUrl: string;
  currentOrigin: string | null;
  currentTitle: string;
  lastUsedSequence: number;
  inFlightCommands: number;
  cdp?: CDPSession;
  cdpPageDomainEnabled: boolean;
  cdpMainFrameId?: string;
  screencastListener?: (event: ScreencastFrameEvent) => void;
  screencastSettings?: ScreencastSettings;
  screencastViewportRefreshScheduled: boolean;
  screencastAck: Promise<void>;
  presentationPhase: "stable" | "navigation_pending";
  presentationRevision: number;
  presentationSettlement?: {
    revision: number;
    promise: Promise<void>;
  };
  hasPresentedFrame: boolean;
  cdpFrameStartedNavigatingListener?: (event: FrameStartedNavigatingEvent) => void;
  cdpFrameStoppedLoadingListener?: (event: FrameStoppedLoadingEvent) => void;
  agentCursor?: {
    x: number;
    y: number;
    action: AgentCursorAction;
  };
  pageStateRefreshScheduled: boolean;
  consoleMessages: BrowserConsoleEntry[];
  networkRequests: BrowserNetworkEntry[];
  networkRequestIds: WeakMap<Request, number>;
  nextConsoleMessageId: number;
  nextNetworkRequestId: number;
  dialogs: BrowserDialogEntry[];
  pendingDialog?: PendingBrowserDialog;
  nextDialogId: number;
  trace?: BrowserTraceState;
  lastTraceSummary?: Record<string, unknown>;
  recording?: BrowserRecordingState;
  popupHandler?: (popup: Page) => void;
}

interface ScreencastSettings {
  format: "jpeg" | "png";
  quality: number;
  maxWidth: number;
  maxHeight: number;
}

type NavigationInput =
  | {
      action: "url";
      url: string;
      handle_before_unload?: "accept" | "dismiss";
      init_script?: string;
      timeout_ms?: number;
    }
  | { action: "back"; timeout_ms?: number }
  | { action: "forward"; timeout_ms?: number }
  | {
      action: "reload";
      ignore_cache?: boolean;
      handle_before_unload?: "accept" | "dismiss";
      timeout_ms?: number;
    };

interface ActionSettlement {
  waitForNavigationSignal(): Promise<boolean>;
  finishPopupNavigation(): Promise<boolean>;
  waitForNetworkSettlement(): Promise<void>;
  dispose(): void;
}

interface FrameStartedNavigatingEvent {
  frameId: string;
}

interface FrameStoppedLoadingEvent {
  frameId: string;
}

interface ScreencastFrameEvent {
  data: string;
  metadata: {
    deviceWidth: number;
    deviceHeight: number;
    pageScaleFactor: number;
  };
  sessionId: number;
}

export class BrowserHost {
  readonly control = new ControlFence();
  readonly #config: BrowserHostConfig;
  readonly #transport: HostTransport;
  readonly #pages = new Map<string, PageRecord>();
  readonly #heapSnapshots = new Map<string, HeapSnapshotModel>();
  #context?: BrowserContext;
  #browserCdp?: CDPSession;
  #puppeteerBrowser?: PuppeteerBrowser;
  #chromiumVersion = "unknown";
  #devtoolsBrowserWSEndpoint?: string;
  #useSequence = 0;
  #foregroundTabId?: string;

  constructor(config: BrowserHostConfig, transport: HostTransport) {
    this.#config = config;
    this.#transport = transport;
  }

  async start(): Promise<HostHandshake> {
    if (this.#context) {
      throw new ProtocolFailure(
        "browser_host_already_started",
        "browser Host is already started",
        false,
        false,
      );
    }
    await mkdir(this.#config.downloadPath, { recursive: true });
    this.#context = await chromium.launchPersistentContext(
      this.#config.profilePath,
      {
        executablePath: this.#config.chromiumExecutable,
        headless: this.#config.headless,
        chromiumSandbox: true,
        ignoreDefaultArgs: ["--disable-extensions"],
        acceptDownloads: true,
        downloadsPath: this.#config.downloadPath,
        permissions: [],
        serviceWorkers: "allow",
        viewport: null,
        args: [
          "--disable-background-networking",
          "--disable-component-update",
          "--disable-default-apps",
          "--disable-sync",
          "--enable-unsafe-extension-debugging",
          "--remote-debugging-port=0",
          "--no-first-run",
          "--no-default-browser-check",
          // 给 headless Chromium 一个不受 runner 物理屏幕限制的虚拟工作区。
          // 实际页面尺寸仍由 Emulation.setDeviceMetricsOverride 控制。
          "--window-size=3840,2160",
          `--force-device-scale-factor=${this.#config.deviceScaleFactor}`,
        ],
      },
    );
    this.#context.setDefaultTimeout(5_000);
    this.#context.setDefaultNavigationTimeout(PAGE_NAVIGATION_TIMEOUT_MILLIS);
    this.#devtoolsBrowserWSEndpoint = await readDevtoolsBrowserWSEndpoint(this.#config.profilePath);
    await this.installNavigationGuard();
    this.#chromiumVersion = this.#context.browser()?.version() ?? "unknown";
    for (const page of this.#context.pages()) {
      await page.close().catch(() => undefined);
    }
    return this.handshake();
  }

  handshake(): HostHandshake {
    return {
      protocol_version: PROTOCOL_VERSION,
      runtime_version: this.#config.runtimeVersion,
      host_version: this.#config.hostVersion,
      playwright_version: this.#config.playwrightVersion,
      chromium_version: this.#chromiumVersion,
      process_id: process.pid,
      runtime_epoch: this.#config.runtimeEpoch,
    };
  }

  async execute(command: HostCommand): Promise<ExecutedCommand> {
    const tabId = commandTabId(command);
    const record = tabId ? this.#pages.get(tabId) : undefined;
    if (record) record.inFlightCommands += 1;
    try {
      switch (command.type) {
      case "ping":
        return {
          result: {
            type: "pong",
            payload: { monotonic_millis: Math.floor(performance.now()) },
          },
        };
      case "update_control":
        this.control.update(command.payload.fence, command.payload.mode);
        if (command.payload.mode === "agent") this.showAgentCursors();
        else this.hideAgentCursors();
        return emptyResult();
      case "create_page":
        return this.createPage(command.payload);
      case "restore_page":
        return this.restorePage(command.payload);
      case "set_viewport":
        return this.setViewport(command.payload);
      case "set_logical_viewport":
        return this.setLogicalViewport(command.payload);
      case "close_page":
        return this.closePage(command.payload.tab_id);
      case "navigate":
        return this.navigate(command.payload);
      case "snapshot":
        return this.snapshot(command.payload);
      case "click":
        return this.click(command.payload);
      case "type":
        return this.type(command.payload);
      case "press":
        return this.press(command.payload);
      case "scroll":
        return this.scroll(command.payload);
      case "devtools":
        return this.devtools(command.payload);
      case "screenshot":
        return this.screenshot(command.payload);
      case "hit_test":
        return this.hitTest(command.payload);
      case "start_screencast":
        return this.startScreencast(command.payload);
      case "stop_screencast":
        return this.stopScreencast(command.payload.tab_id);
      case "user_input":
        return this.userInput(command.payload);
        case "shutdown":
          await this.close();
          return emptyResult();
        default:
          throw new ProtocolFailure(
            "browser_command_unsupported",
            "browser Host command is unsupported",
            false,
            false,
          );
      }
    } finally {
      if (record) record.inFlightCommands -= 1;
    }
  }

  async close(): Promise<void> {
    const context = this.#context;
    this.#context = undefined;
    this.#browserCdp = undefined;
    this.#puppeteerBrowser?.disconnect();
    this.#puppeteerBrowser = undefined;
    this.#devtoolsBrowserWSEndpoint = undefined;
    for (const record of this.#pages.values()) this.disposePageRecord(record);
    this.#pages.clear();
    this.#heapSnapshots.clear();
    if (context) {
      await context.close();
    }
  }

  private async createPage(input: {
    tab_id: string;
    initial_url: string;
    viewport: HostViewport;
    navigation_revision: number;
    snapshot_revision: number;
    allow_streaming_eviction?: boolean;
  }): Promise<ExecutedCommand> {
    if (this.#pages.has(input.tab_id)) {
      throw new ProtocolFailure(
        "browser_tab_exists",
        `browser tab already exists: ${input.tab_id}`,
        false,
        false,
      );
    }
    validateViewport(input.viewport);
    validateNavigationUrl(input.initial_url);
    this.ensureRecordCapacity();
    const record = this.newPageRecord(input);
    this.#pages.set(input.tab_id, record);
    record.inFlightCommands = 1;
    try {
      await this.ensurePage(record, Boolean(input.allow_streaming_eviction));
      const state = await this.pageState(record);
      this.#transport.emit({ type: "page_updated", payload: state });
      return { result: { type: "page_state", payload: state } };
    } catch (error) {
      this.#pages.delete(input.tab_id);
      throw playwrightFailure("browser_page_creation_failed", error, false);
    } finally {
      record.inFlightCommands -= 1;
    }
  }

  private async restorePage(input: {
    tab_id: string;
    initial_url: string;
    viewport: HostViewport;
    navigation_revision: number;
    snapshot_revision: number;
    allow_streaming_eviction: boolean;
  }): Promise<ExecutedCommand> {
    validateViewport(input.viewport);
    validateNavigationUrl(input.initial_url);
    const existing = this.#pages.get(input.tab_id);
    if (existing) {
      await this.ensurePage(existing, input.allow_streaming_eviction);
      const existingState = await this.pageState(existing);
      if (existingState.url === input.initial_url) {
        const viewportChanged = await this.applyViewport(existing, input.viewport);
        existing.navigationRevision = Math.max(
          existing.navigationRevision,
          input.navigation_revision,
        );
        existing.snapshot.advanceTo(input.snapshot_revision);
        if (viewportChanged) existing.snapshot.invalidate();
        await this.requirePage(existing).bringToFront();
        this.#foregroundTabId = input.tab_id;
        this.touch(existing);
        return { result: { type: "page_state", payload: await this.pageState(existing) } };
      }

      // 右侧面板恢复期间，物理 Page 可能落后于逻辑 Authority。此时复用
      // 旧 Page 会返回过期 URL/修订号，破坏 Authority 的单调状态契约。
      await this.discardPhysicalPage(existing);
    }

    // Authority 是逻辑 Tab 的唯一来源。Host 只在真正使用 Tab 时创建记录和
    // Chromium Page，因此 daemon 重启后恢复的 Tab 不会预先占用 Host 卡槽。
    this.ensureRecordCapacity();
    const record = this.newPageRecord(input);
    this.#pages.set(input.tab_id, record);
    record.inFlightCommands = 1;
    try {
      await this.ensurePage(record, input.allow_streaming_eviction);
      await this.requirePage(record).bringToFront();
      this.#foregroundTabId = input.tab_id;
      const state = await this.pageState(record);
      this.#transport.emit({ type: "page_updated", payload: state });
      return { result: { type: "page_state", payload: state } };
    } catch (error) {
      this.#pages.delete(input.tab_id);
      throw playwrightFailure("browser_page_restore_failed", error, false);
    } finally {
      record.inFlightCommands -= 1;
    }
  }

  private newPageRecord(input: {
    tab_id: string;
    initial_url: string;
    viewport: HostViewport;
    navigation_revision: number;
    snapshot_revision: number;
  }): PageRecord {
    return {
      tabId: input.tab_id,
      snapshot: new SnapshotRegistry(input.snapshot_revision),
      navigationRevision: input.navigation_revision,
      frameSequence: 0,
      restoringInitialPage: false,
      viewport: input.viewport,
      defaultUserAgent: "",
      defaultPlatform: "",
      currentUrl: input.initial_url,
      currentOrigin: null,
      currentTitle: "",
      lastUsedSequence: 0,
      inFlightCommands: 0,
      cdpPageDomainEnabled: false,
      screencastViewportRefreshScheduled: false,
      screencastAck: Promise.resolve(),
      presentationPhase: "stable",
      presentationRevision: 0,
      hasPresentedFrame: false,
      pageStateRefreshScheduled: false,
      consoleMessages: [],
      networkRequests: [],
      networkRequestIds: new WeakMap(),
      nextConsoleMessageId: 1,
      nextNetworkRequestId: 1,
      dialogs: [],
      nextDialogId: 1,
    };
  }

  private async closePage(tabId: string): Promise<ExecutedCommand> {
    // Page 崩溃后会从 Host 的物理页面表移除，但 Authority 仍保留
    // crashed Tab 供用户查看和关闭。关闭操作必须幂等，不能因物理页已不存在
    // 阻止 Authority 收口这个逻辑 Tab。
    const record = this.#pages.get(tabId);
    if (!record) return emptyResult();
    await this.stopScreencast(tabId);
    this.disposePageRecord(record);
    this.#pages.delete(tabId);
    if (this.#foregroundTabId === tabId) this.#foregroundTabId = undefined;
    await record.page?.close().catch(() => undefined);
    return emptyResult();
  }

  private async setViewport(input: {
    tab_id: string;
    viewport: HostViewport;
  }): Promise<ExecutedCommand> {
    validateViewport(input.viewport);
    const record = this.pageRecord(input.tab_id);
    await this.ensurePage(record);
    const logicalViewportChanged = await this.applyViewport(record, input.viewport);
    if (logicalViewportChanged) record.snapshot.invalidate();
    if (record.screencastListener) {
      await this.emitCurrentViewportFrame(record);
    }
    return emptyResult();
  }

  private async setLogicalViewport(input: {
    tab_id: string;
    viewport: HostLogicalViewport;
  }): Promise<ExecutedCommand> {
    validateLogicalViewport(input.viewport);
    const record = this.pageRecord(input.tab_id);
    await this.ensurePage(record);
    await this.applyViewport(record, {
      ...input.viewport,
      surface_width: record.viewport.surface_width,
      surface_height: record.viewport.surface_height,
    });
    return emptyResult();
  }

  private async applyViewport(
    record: PageRecord,
    viewport: HostViewport,
    force = false,
  ): Promise<boolean> {
    const logicalViewportChanged = force || !record.cdp
      || record.viewport.width !== viewport.width
      || record.viewport.height !== viewport.height
      || record.viewport.device_scale_factor_millis !== viewport.device_scale_factor_millis
      || record.viewport.device_type !== viewport.device_type;
    if (logicalViewportChanged) {
      const page = this.requirePage(record);
      const cdp = await this.cdpSession(record);
      const mobile = viewport.device_type !== "desktop";
      const portrait = viewport.height >= viewport.width;
      await this.applyUserAgent(record, viewport.device_type);
      await cdp.send("Emulation.setTouchEmulationEnabled", {
        enabled: mobile,
        maxTouchPoints: mobile ? 5 : 1,
      });
      await cdp.send("Emulation.setDeviceMetricsOverride", {
        width: viewport.width,
        height: viewport.height,
        deviceScaleFactor: this.#config.deviceScaleFactor,
        mobile,
        screenWidth: viewport.width,
        screenHeight: viewport.height,
        screenOrientation: {
          type: portrait ? "portraitPrimary" : "landscapePrimary",
          angle: portrait ? 0 : 90,
        },
      });
      await page.evaluate(
        () => new Promise<void>((accept) => requestAnimationFrame(() => accept())),
      );
    }
    record.viewport = viewport;
    return logicalViewportChanged;
  }

  private async applyUserAgent(
    record: PageRecord,
    deviceType: HostDeviceType,
  ): Promise<void> {
    const cdp = await this.cdpSession(record);
    if (deviceType === "desktop") {
      await cdp.send("Emulation.setUserAgentOverride", {
        userAgent: record.defaultUserAgent,
        platform: record.defaultPlatform,
      });
      return;
    }
    const version = /^\d+(?:\.\d+){1,3}$/.test(this.#chromiumVersion)
      ? this.#chromiumVersion
      : "120.0.0.0";
    const major = version.split(".")[0] ?? "120";
    const model = "Pixel 7";
    const userAgent = `Mozilla/5.0 (Linux; Android 14; ${model}) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/${version} Mobile Safari/537.36`;
    await cdp.send("Emulation.setUserAgentOverride", {
      userAgent,
      platform: "Android",
      userAgentMetadata: {
        brands: [
          { brand: "Chromium", version: major },
          { brand: "Not_A Brand", version: "99" },
        ],
        fullVersionList: [
          { brand: "Chromium", version },
          { brand: "Not_A Brand", version: "99.0.0.0" },
        ],
        fullVersion: version,
        platform: "Android",
        platformVersion: "14.0.0",
        architecture: "",
        model,
        mobile: true,
        bitness: "",
        wow64: false,
      },
    });
  }

  private async ensurePage(
    record: PageRecord,
    allowStreamingEviction = false,
  ): Promise<void> {
    if (record.page) {
      // 页面已经物理存在时，恢复只是一次幂等的激活确认。实时画面流有
      // 自己的 ACK 管道，不能把它当成页面可用性的同步屏障：在持续输出
      // screencast 的页面上，ACK 队列可能一直有新帧，等待它会阻塞后续
      // 的导航、快照和交互命令。只有停止画面流时才需要收口 ACK。
      this.touch(record);
      this.ensureAgentCursor(record);
      return;
    }
    await this.ensureActiveCapacity(record, allowStreamingEviction);
    const page = await this.context().newPage();
    record.page = page;
    try {
      if (!record.defaultUserAgent || !record.defaultPlatform) {
        [record.defaultUserAgent, record.defaultPlatform] = await Promise.all([
          page.evaluate(() => navigator.userAgent),
          page.evaluate(() => navigator.platform),
        ]);
      }
      await this.applyViewport(record, record.viewport);
      await this.bindPageEvents(record);
      if (record.currentUrl && record.currentUrl !== "about:blank") {
        record.restoringInitialPage = true;
        try {
          await this.withNavigationGuard(record, () => page.goto(record.currentUrl, {
            waitUntil: "domcontentloaded",
            timeout: PAGE_NAVIGATION_TIMEOUT_MILLIS,
          }));
          this.throwBlockedNavigation(record);
        } finally {
          record.restoringInitialPage = false;
        }
      }
      await this.pageState(record);
      this.touch(record);
      this.ensureAgentCursor(record);
    } catch (error) {
      record.page = undefined;
      record.cdp = undefined;
      record.cdpPageDomainEnabled = false;
      record.cdpMainFrameId = undefined;
      await page.close().catch(() => undefined);
      throw error;
    }
  }

  private async ensureActiveCapacity(
    target: PageRecord,
    allowStreamingEviction: boolean,
  ): Promise<void> {
    const active = [...this.#pages.values()].filter((record) => record.page).length;
    if (active < this.#config.maxActivePages) return;
    const candidates = [...this.#pages.values()]
      .filter((record) => (
        record !== target
        && record.page
        && record.inFlightCommands === 0
      ))
      .sort((left, right) => left.lastUsedSequence - right.lastUsedSequence);
    const candidate = candidates.find((record) => !record.screencastSettings)
      ?? (allowStreamingEviction ? candidates[0] : undefined);
    if (candidate) {
      await this.suspendPage(candidate);
      return;
    }
    throw new ProtocolFailure(
      "browser_active_page_limit_reached",
      `active browser page limit reached: ${this.#config.maxActivePages}`,
      true,
      false,
      "所有浏览器页面当前都在执行操作，请稍后重试。",
    );
  }

  private ensureRecordCapacity(): void {
    while (this.#pages.size >= this.#config.maxTabs) {
      const candidate = [...this.#pages.values()]
        .filter((record) => !record.page && record.inFlightCommands === 0)
        .sort((left, right) => left.lastUsedSequence - right.lastUsedSequence)[0];
      if (!candidate) {
        throw new ProtocolFailure(
          "browser_tab_limit_reached",
          `browser tab limit reached: ${this.#config.maxTabs}`,
          true,
          false,
          "所有浏览器页面当前都在执行操作，请稍后重试。",
        );
      }
      // Suspended Page 的最新文档状态已经通过 page_suspended 事件交给
      // Authority；这里只清理 Host 的易失记录，不删除用户的逻辑 Tab。
      this.#pages.delete(candidate.tabId);
    }
  }

  private async suspendPage(record: PageRecord): Promise<void> {
    const page = record.page;
    if (!page) return;
    const state = await this.pageState(record).catch(() => ({
      tab_id: record.tabId,
      url: record.currentUrl,
      origin: record.currentOrigin,
      title: record.currentTitle,
      navigation_revision: record.navigationRevision,
    } satisfies PageState));
    record.screencastSettings = undefined;
    await this.stopScreencastSession(record);
    this.disposePageRecord(record);
    record.page = undefined;
    record.cdp = undefined;
    record.cdpPageDomainEnabled = false;
    record.cdpMainFrameId = undefined;
    record.screencastListener = undefined;
    record.navigationRevision += 1;
    record.snapshot.invalidate();
    record.frameSequence = 0;
    if (this.#foregroundTabId === record.tabId) this.#foregroundTabId = undefined;
    await page.close().catch(() => undefined);
    this.#pages.delete(record.tabId);
    this.#transport.emit({
      type: "page_suspended",
      payload: state,
    });
  }

  private async discardPhysicalPage(record: PageRecord): Promise<void> {
    const page = record.page;
    if (!page) return;
    record.screencastSettings = undefined;
    await this.stopScreencastSession(record);
    this.disposePageRecord(record);
    this.#pages.delete(record.tabId);
    if (this.#foregroundTabId === record.tabId) this.#foregroundTabId = undefined;
    record.page = undefined;
    record.cdp = undefined;
    record.cdpPageDomainEnabled = false;
    record.cdpMainFrameId = undefined;
    record.screencastListener = undefined;
    record.screencastSettings = undefined;
    await page.close().catch(() => undefined);
  }

  private touch(record: PageRecord): void {
    record.lastUsedSequence = ++this.#useSequence;
  }

  private requirePage(record: PageRecord): Page {
    if (!record.page) {
      throw new ProtocolFailure(
        "browser_page_not_active",
        `browser page is not active: ${record.tabId}`,
        true,
        false,
      );
    }
    return record.page;
  }

  private ensureNoPendingDialog(record: PageRecord): void {
    if (!record.pendingDialog) return;
    throw new ProtocolFailure(
      "browser_dialog_pending",
      "浏览器页面有一个待处理的对话框，请先调用 browser_dialog accept 或 dismiss",
      true,
      false,
    );
  }

  private async cdpSession(record: PageRecord): Promise<CDPSession> {
    if (!record.cdp) {
      record.cdp = await this.context().newCDPSession(this.requirePage(record));
      record.cdpPageDomainEnabled = false;
      record.cdpMainFrameId = undefined;
    }
    return record.cdp;
  }

  private async browserCdpSession(): Promise<CDPSession> {
    if (this.#browserCdp) return this.#browserCdp;
    const browser = this.context().browser();
    if (!browser) throw new Error("Chromium browser connection is unavailable");
    this.#browserCdp = await browser.newBrowserCDPSession();
    return this.#browserCdp;
  }

  private async puppeteerBrowser(): Promise<PuppeteerBrowser> {
    if (this.#puppeteerBrowser?.connected) return this.#puppeteerBrowser;
    const endpoint = this.#devtoolsBrowserWSEndpoint;
    if (!endpoint) throw new Error("Chromium DevTools connection is unavailable");
    const { default: puppeteer } = await import("puppeteer-core");
    this.#puppeteerBrowser = await puppeteer.connect({
      browserWSEndpoint: endpoint,
      defaultViewport: null,
      handleDevToolsAsPage: true,
      targetFilter: () => true,
    });
    return this.#puppeteerBrowser;
  }

  private async puppeteerPage(record: PageRecord): Promise<PuppeteerPage> {
    const cdp = await this.cdpSession(record);
    const targetInfo = await cdp.send("Target.getTargetInfo");
    const browser = await this.puppeteerBrowser();
    const target = browser.targets().find(candidate => (
      candidate.type() === "page"
      && (candidate as unknown as { _targetId?: string })._targetId === targetInfo.targetInfo.targetId
    ));
    const page = await target?.page();
    if (!page) throw new Error(`Puppeteer page target not found: ${targetInfo.targetInfo.targetId}`);
    return page;
  }

  private async navigate(input: {
    tab_id: string;
    control: HostControl;
    navigation: NavigationInput;
  }): Promise<ExecutedCommand> {
    this.control.validate(input.control);
    const record = this.pageRecord(input.tab_id);
    await this.ensurePage(record);
    this.ensureNoPendingDialog(record);
    const page = this.requirePage(record);
    const timeout = boundedTimeout(input.navigation.timeout_ms, PAGE_NAVIGATION_TIMEOUT_MILLIS, 60_000);
    const requestedBeforeUnload = "handle_before_unload" in input.navigation
      ? input.navigation.handle_before_unload
      : undefined;
    const beforeUnloadAction = requestedBeforeUnload ?? "accept";
    if (beforeUnloadAction !== "accept" && beforeUnloadAction !== "dismiss") {
      throw invalidDevtoolsArguments("handle_before_unload must be accept or dismiss");
    }
    const cdp = await this.cdpSession(record);
    let initScriptId: string | undefined;
    if (input.navigation.action === "url" && input.navigation.init_script) {
      const result = await cdp.send("Page.addScriptToEvaluateOnNewDocument", {
        source: input.navigation.init_script,
      }) as { identifier?: string };
      initScriptId = result.identifier;
    }
    let beforeUnloadHandler: ((dialog: Dialog) => void) | undefined;
    let beforeUnloadHandling: Promise<void> | undefined;
    this.beginNavigationPresentation(record, page);
    if (requestedBeforeUnload) {
      beforeUnloadHandler = (dialog: Dialog) => {
        if (dialog.type() !== "beforeunload") return;
        beforeUnloadHandling = (async () => {
          try {
            if (beforeUnloadAction === "accept") await dialog.accept();
            else await dialog.dismiss();
            if (record.pendingDialog?.dialog === dialog) {
              clearTimeout(record.pendingDialog.timeout);
              record.pendingDialog.entry.handledAs = beforeUnloadAction === "accept" ? "accepted" : "dismissed";
              record.pendingDialog = undefined;
            }
          } catch {
            // 导航结果中的对话框可能已被并发关闭，此时无需重复处理。
          }
        })();
        void beforeUnloadHandling;
      };
      page.on("dialog", beforeUnloadHandler);
    }
    try {
      await this.withNavigationGuard(record, async () => {
        this.control.validate(input.control);
        try {
          switch (input.navigation.action) {
            case "url":
              validateNavigationUrl(input.navigation.url);
              await page.goto(input.navigation.url, { waitUntil: "domcontentloaded", timeout });
              break;
            case "back":
              await page.goBack({ waitUntil: "domcontentloaded", timeout });
              break;
            case "forward":
              await page.goForward({ waitUntil: "domcontentloaded", timeout });
              break;
            case "reload":
              if (input.navigation.ignore_cache) {
                const loaded = page.waitForEvent("domcontentloaded", { timeout });
                await cdp.send("Page.reload", { ignoreCache: true });
                await loaded;
              } else {
                await page.reload({ waitUntil: "domcontentloaded", timeout });
              }
              break;
          }
        } catch (error) {
          // Playwright 的 page.goto 超时后，Chromium 可能仍保持一个未完成的
          // navigation。若直接返回，下一条 snapshot/type 会继续等待这次旧
          // navigation，形成连续工具错误。先停止当前加载，让页面回到可操作
          // 状态；当前导航仍按 indeterminate/recoverable 错误返回给调用方。
          await cdp.send("Page.stopLoading").catch(() => undefined);
          await page
            .waitForLoadState("domcontentloaded", { timeout: 1_000 })
            .catch(() => undefined);
          this.throwBlockedNavigation(record);
          if (beforeUnloadAction === "dismiss" && beforeUnloadHandling) {
            await beforeUnloadHandling;
            return;
          }
          throw playwrightFailure("browser_navigation_failed", error, true);
        }
      });
    } finally {
      await this.completeNavigationPresentation(record, page);
      if (beforeUnloadHandling) await beforeUnloadHandling;
      if (beforeUnloadHandler) page.off("dialog", beforeUnloadHandler);
      if (initScriptId) {
        await cdp.send("Page.removeScriptToEvaluateOnNewDocument", { identifier: initScriptId }).catch(() => undefined);
      }
    }
    this.throwBlockedNavigation(record);
    const state = await this.pageState(record);
    this.#transport.emit({ type: "page_updated", payload: state });
    return { result: { type: "page_state", payload: state } };
  }

  private async snapshot(input: {
    tab_id: string;
    limits: { max_nodes: number; max_text_bytes: number };
    subtree_ref?: string | null;
  }): Promise<ExecutedCommand> {
    const record = this.pageRecord(input.tab_id);
    await this.ensurePage(record);
    this.ensureNoPendingDialog(record);
    const page = this.requirePage(record);
    const snapshot = await record.snapshot.capture(
      page,
      record.tabId,
      input.limits,
      input.subtree_ref,
    );
    return { result: { type: "snapshot", payload: snapshot } };
  }

  private async click(input: {
    tab_id: string;
    control: HostControl;
    target: { snapshot_revision: number; element_ref: string };
  }): Promise<ExecutedCommand> {
    this.control.validate(input.control);
    const record = this.pageRecord(input.tab_id);
    await this.ensurePage(record);
    this.ensureNoPendingDialog(record);
    const page = this.requirePage(record);
    const locator = await record.snapshot.resolve(page, input.target);
    const clickTarget = await locator.evaluate((element) => {
      const form = element.closest("form");
      const descriptor = [
        element.textContent,
        element.getAttribute("aria-label"),
        element.getAttribute("title"),
        element.getAttribute("name"),
        element.getAttribute("id"),
        element.getAttribute("value"),
        form?.getAttribute("action"),
      ].filter(Boolean).join(" ").replace(/\s+/g, " ").trim().toLowerCase();
      const patterns: Array<[string, RegExp]> = [
        ["destructive", /\b(delete|remove|destroy|erase|terminate account|close account|revoke)\b|删除|移除|注销账户|撤销/],
        ["payment", /\b(pay|purchase|buy now|checkout|place order|submit order|transfer|withdraw)\b|支付|购买|结账|下单|提交订单|转账|提现/],
        ["publish", /\b(publish|deploy|release to production|go live)\b|发布|部署|上线/],
        ["permission", /\b(grant access|authorize|change permissions?|make public)\b|授权|更改权限|设为公开/],
      ];
      const sensitiveActionKind = patterns.find(([, pattern]) => pattern.test(descriptor))?.[0] ?? null;
      const anchor = element.closest("a[href]") as HTMLAnchorElement | null;
      let expectedNavigationUrl: string | null = null;
      if (
        anchor
        && !anchor.hasAttribute("download")
        && (!anchor.target || anchor.target === "_self")
      ) {
        const target = new URL(anchor.href, document.baseURI);
        const current = new URL(globalThis.location.href);
        const onlyHashChanges = target.origin === current.origin
          && target.pathname === current.pathname
          && target.search === current.search;
        if (
          (target.protocol === "http:" || target.protocol === "https:")
          && target.href !== current.href
          && !onlyHashChanges
        ) {
          expectedNavigationUrl = target.href;
        }
      }
      return { sensitiveActionKind, expectedNavigationUrl };
    });
    if (clickTarget.sensitiveActionKind) {
      throw new ProtocolFailure(
        "browser_sensitive_action_requires_user",
        `model click is blocked for sensitive action: ${clickTarget.sensitiveActionKind}`,
        true,
        false,
        "用户接管浏览器后可以手动完成该操作；不要自动重试当前点击。",
      );
    }
    this.control.validate(input.control);
    await this.withNavigationGuard(record, async () => {
      try {
        await locator.scrollIntoViewIfNeeded();
          const bounds = await locator.boundingBox();
        if (!bounds) {
          throw new Error("browser click target has no visible bounds");
        }
        let resolveDialog: (() => void) | undefined;
        const dialogOpened = new Promise<void>((resolve) => { resolveDialog = resolve; });
        const onDialog = () => resolveDialog?.();
        page.once("dialog", onDialog);
        try {
          const cdp = await this.cdpSession(record);
          const x = bounds.x + bounds.width / 2;
          const y = bounds.y + bounds.height / 2;
          this.showAgentCursor(record, x, y, "click");
          await cdp.send("Input.dispatchMouseEvent", { type: "mouseMoved", x, y });
          await cdp.send("Input.dispatchMouseEvent", {
            type: "mousePressed",
            x,
            y,
            button: "left",
            buttons: 1,
            clickCount: 1,
          });
          const released = cdp.send("Input.dispatchMouseEvent", {
            type: "mouseReleased",
            x,
            y,
            button: "left",
            buttons: 0,
            clickCount: 1,
          });
          const dialogWon = await Promise.race([
            released.then(() => false),
            dialogOpened.then(() => true),
          ]);
          if (dialogWon) void released.catch(() => undefined);
        } finally {
          page.off("dialog", onDialog);
        }
      } catch (error) {
        this.throwBlockedNavigation(record);
        throw playwrightFailure("browser_click_failed", error, true);
      }
    }, clickTarget.expectedNavigationUrl);
    this.throwBlockedNavigation(record);
    return { result: { type: "page_state", payload: await this.pageState(record) } };
  }

  private async type(input: {
    tab_id: string;
    control: HostControl;
    target: { snapshot_revision: number; element_ref: string };
    text: string;
    replace: boolean;
    submit_key?: string | null;
  }): Promise<ExecutedCommand> {
    this.control.validate(input.control);
    const record = this.pageRecord(input.tab_id);
    await this.ensurePage(record);
    this.ensureNoPendingDialog(record);
    const page = this.requirePage(record);
    const locator = await record.snapshot.resolve(page, input.target);
    const sensitiveInputKind = await locator.evaluate((element) => {
      const type = ((element as HTMLInputElement).type || element.getAttribute("type") || "text").toLowerCase();
      const autocomplete = (element.getAttribute("autocomplete") ?? "").toLowerCase();
      const descriptor = [
        element.getAttribute("name"),
        element.getAttribute("id"),
        element.getAttribute("aria-label"),
        element.getAttribute("placeholder"),
        autocomplete,
      ].filter(Boolean).join(" ").toLowerCase();
      if (type === "password" || /password|passcode|passwd/.test(descriptor)) return "password";
      if (autocomplete === "one-time-code" || /one[- ]?time|otp|verification code|security code|auth code/.test(descriptor)) return "one_time_code";
      if (autocomplete.startsWith("cc-") || /credit[- ]?card|card number|cardholder|cvv|cvc|ccv|expiration date|expiry date/.test(descriptor)) return "payment_card";
      return null;
    });
    if (sensitiveInputKind) {
      throw new ProtocolFailure(
        "browser_sensitive_input_blocked",
        `model input is blocked for sensitive field: ${sensitiveInputKind}`,
        false,
        false,
        "用户接管浏览器后可以手动填写敏感字段；敏感值不会进入模型快照或日志。",
      );
    }
    await this.withNavigationGuard(record, async () => {
      await locator.focus();
      const bounds = await locator.boundingBox();
      if (bounds) {
        this.showAgentCursor(
          record,
          bounds.x + bounds.width / 2,
          bounds.y + bounds.height / 2,
          "type",
        );
      }
      this.control.validate(input.control);
      try {
        if (input.replace) {
          await locator.fill(input.text);
        } else {
          await page.keyboard.insertText(input.text);
        }
        if (input.submit_key) {
          await page.keyboard.press(input.submit_key);
        }
      } catch (error) {
        this.throwBlockedNavigation(record);
        throw playwrightFailure("browser_type_failed", error, true);
      }
    });
    this.throwBlockedNavigation(record);
    return { result: { type: "page_state", payload: await this.pageState(record) } };
  }

  private async press(input: {
    tab_id: string;
    control: HostControl;
    key: string;
  }): Promise<ExecutedCommand> {
    this.control.validate(input.control);
    const record = this.pageRecord(input.tab_id);
    await this.ensurePage(record);
    this.ensureNoPendingDialog(record);
    const page = this.requirePage(record);
    await this.withNavigationGuard(record, async () => {
      this.control.validate(input.control);
      try {
        await page.keyboard.press(input.key);
      } catch (error) {
        this.throwBlockedNavigation(record);
        throw playwrightFailure("browser_press_failed", error, true);
      }
    });
    this.throwBlockedNavigation(record);
    return { result: { type: "page_state", payload: await this.pageState(record) } };
  }

  private async scroll(input: {
    tab_id: string;
    control: HostControl;
    target?: { snapshot_revision: number; element_ref: string } | null;
    delta_x: number;
    delta_y: number;
  }): Promise<ExecutedCommand> {
    this.control.validate(input.control);
    requireFinite("delta_x", input.delta_x);
    requireFinite("delta_y", input.delta_y);
    const record = this.pageRecord(input.tab_id);
    await this.ensurePage(record);
    this.ensureNoPendingDialog(record);
    const page = this.requirePage(record);
    await this.withNavigationGuard(record, async () => {
      if (input.target) {
        const locator = await record.snapshot.resolve(page, input.target);
        const bounds = await locator.boundingBox();
        if (bounds) {
          this.showAgentCursor(
            record,
            bounds.x + bounds.width / 2,
            bounds.y + bounds.height / 2,
            "scroll",
          );
        }
        this.control.validate(input.control);
        await locator.evaluate(
          (element, delta) => (element as HTMLElement).scrollBy(delta.x, delta.y),
          { x: input.delta_x, y: input.delta_y },
        );
      } else {
        this.control.validate(input.control);
        this.showAgentCursor(record, record.viewport.width / 2, record.viewport.height / 2, "scroll");
        await page.mouse.wheel(input.delta_x, input.delta_y);
      }
    });
    this.throwBlockedNavigation(record);
    return { result: { type: "page_state", payload: await this.pageState(record) } };
  }

  private async devtools(input: {
    tab_id: string;
    control?: HostControl | null;
    operation: string;
    arguments: Record<string, unknown>;
  }): Promise<ExecutedCommand> {
    if (input.control) this.control.validate(input.control);
    const record = this.pageRecord(input.tab_id);
    await this.ensurePage(record);
    if (input.operation !== "dialog") this.ensureNoPendingDialog(record);
    const page = this.requirePage(record);
    const args = input.arguments ?? {};
    if (devtoolsOperationRequiresControl(input.operation, args)) {
      this.requireDevtoolsControl(input.control);
    }

    switch (input.operation) {
      case "wait_for": {
        const timeout = boundedTimeout(args.timeout_ms, 5_000, 60_000);
        const selector = optionalDevtoolsString(args, "selector");
        const url = optionalDevtoolsString(args, "url");
        const texts = devtoolsStringArray(args.text ?? args.texts);
        if (!selector && !url && texts.length === 0) {
          throw invalidDevtoolsArguments("wait_for requires text, selector, or url");
        }
        try {
          if (selector) {
            await page.locator(selector).first().waitFor({ state: "visible", timeout });
          }
          if (url) {
            await page.waitForURL(value => value.href.includes(url), {
              timeout,
              waitUntil: "domcontentloaded",
            });
          }
          if (texts.length > 0) {
            await page.waitForFunction(
              expected => expected.some(text => document.body?.innerText.includes(text)),
              texts,
              { timeout },
            );
          }
        } catch (error) {
          if (error instanceof errors.TimeoutError) {
            throw new ProtocolFailure(
              "browser_wait_timeout",
              `browser wait condition was not met within ${timeout}ms`,
              true,
              false,
            );
          }
          throw playwrightFailure("browser_wait_failed", error, false);
        }
        return jsonResult({ matched: { selector, url, texts }, url: page.url() });
      }
      case "hover": {
        const target = devtoolsTarget(args);
        const locator = await record.snapshot.resolve(page, target);
        await locator.scrollIntoViewIfNeeded();
        const bounds = await locator.boundingBox();
        if (!bounds) throw new Error("browser hover target has no visible bounds");
        this.showAgentCursor(
          record,
          bounds.x + bounds.width / 2,
          bounds.y + bounds.height / 2,
          "move",
        );
        await locator.hover();
        return jsonResult({ hovered: target.element_ref });
      }
      case "drag": {
        const from = devtoolsTarget(devtoolsObject(args, "from"));
        const to = devtoolsTarget(devtoolsObject(args, "to"));
        const fromLocator = await record.snapshot.resolve(page, from);
        const toLocator = await record.snapshot.resolve(page, to);
        await fromLocator.scrollIntoViewIfNeeded();
        const fromBounds = await fromLocator.boundingBox();
        const toBounds = await toLocator.boundingBox();
        if (!fromBounds || !toBounds) throw new Error("browser drag target has no visible bounds");
        this.showAgentCursor(
          record,
          fromBounds.x + fromBounds.width / 2,
          fromBounds.y + fromBounds.height / 2,
          "drag",
        );
        await fromLocator.dragTo(toLocator);
        this.showAgentCursor(
          record,
          toBounds.x + toBounds.width / 2,
          toBounds.y + toBounds.height / 2,
          "drag",
        );
        return jsonResult({ from: from.element_ref, to: to.element_ref });
      }
      case "fill_form": {
        const elements = devtoolsObjectArray(args, "elements");
        for (const element of elements) {
          const target = devtoolsTarget(element);
          const value = element.value;
          const locator = await record.snapshot.resolve(page, target);
          const bounds = await locator.boundingBox();
          if (bounds) {
            this.showAgentCursor(
              record,
              bounds.x + bounds.width / 2,
              bounds.y + bounds.height / 2,
              "type",
            );
          }
          const kind = await locator.evaluate(node => ({
            tag: node.tagName.toLowerCase(),
            type: (node.getAttribute("type") ?? "").toLowerCase(),
          }));
          if (kind.tag === "select") {
            await locator.selectOption(String(value ?? ""));
          } else if (kind.type === "checkbox" || kind.type === "radio") {
            const checked = value === true || value === "true";
            if (checked) await locator.check();
            else if (kind.type === "checkbox") await locator.uncheck();
          } else {
            await locator.fill(String(value ?? ""));
          }
        }
        return jsonResult({ filled: elements.length });
      }
      case "upload_file": {
        const target = devtoolsTarget(args);
        const filePath = requiredDevtoolsString(args, "file_path");
        const locator = await record.snapshot.resolve(page, target);
        const bounds = await locator.boundingBox();
        if (bounds) {
          this.showAgentCursor(
            record,
            bounds.x + bounds.width / 2,
            bounds.y + bounds.height / 2,
            "click",
          );
        }
        await locator.setInputFiles(filePath);
        return jsonResult({ uploaded: true, element_ref: target.element_ref });
      }
      case "click_at": {
        const x = requiredFiniteDevtoolsNumber(args, "x");
        const y = requiredFiniteDevtoolsNumber(args, "y");
        this.showAgentCursor(record, x, y, "click");
        await page.mouse.click(x, y, { clickCount: args.double_click === true ? 2 : 1 });
        return jsonResult({ x, y, double_click: args.double_click === true });
      }
      case "evaluate": {
        const source = requiredDevtoolsString(args, "function");
        const callArgs = Array.isArray(args.args) ? args.args : [];
        const result = await page.evaluate(
          async ({ expression, values }) => {
            const candidate = (0, eval)(`(${expression})`);
            if (typeof candidate !== "function") {
              throw new Error("browser evaluate input must be a function declaration");
            }
            return await candidate(...values);
          },
          { expression: source, values: callArgs },
        );
        if (args.wait_for_stable_dom !== false) {
          await this.waitForStableDom(page);
        }
        return jsonResult({ value: boundJsonValue(result) });
      }
      case "console":
        return this.consoleOperation(record, args);
      case "network":
        return this.networkOperation(record, args);
      case "dialog":
        return this.dialogOperation(record, args);
      case "emulate": {
        return this.emulateOperation(record, args);
      }
      case "performance":
        return this.performanceOperation(record, args);
      case "lighthouse":
        return this.lighthouseOperation(record, args);
      case "heap":
        return this.heapOperation(record, args);
      case "extensions":
        return this.extensionsOperation(record, args);
      case "third_party":
        return this.thirdPartyOperation(page, args);
      case "webmcp":
        return this.webMcpOperation(page, args);
      case "pwa":
        return this.pwaOperation(record, args);
      case "recording":
        return this.recordingOperation(record, args);
      default:
        throw new ProtocolFailure(
          "browser_devtools_operation_unsupported",
          `unsupported browser devtools operation: ${input.operation}`,
          false,
          false,
        );
    }
  }

  private requireDevtoolsControl(control?: HostControl | null): void {
    if (!control) {
      throw invalidDevtoolsArguments("browser devtools write operation requires control");
    }
    this.control.validate(control);
  }

  private consoleOperation(
    record: PageRecord,
    args: Record<string, unknown>,
  ): ExecutedCommand {
    const action = optionalDevtoolsString(args, "action") ?? "list";
    if (action === "clear") {
      record.consoleMessages = [];
      return jsonResult({ cleared: true });
    }
    if (action === "get") {
      const id = requiredDevtoolsInteger(args, "message_id");
      const message = record.consoleMessages.find(entry => entry.id === id);
      if (!message) throw invalidDevtoolsArguments(`console message not found: ${id}`);
      return jsonResult({ message });
    }
    if (action !== "list") throw invalidDevtoolsArguments("invalid console action");
    const levels = new Set(devtoolsStringArray(args.levels));
    const pageSize = boundedInteger(args.page_size, 100, 1, 500);
    const messages = record.consoleMessages
      .filter(entry => levels.size === 0 || levels.has(entry.level))
      .slice(-pageSize);
    return jsonResult({ messages, total: record.consoleMessages.length });
  }

  private async networkOperation(
    record: PageRecord,
    args: Record<string, unknown>,
  ): Promise<ExecutedCommand> {
    const action = optionalDevtoolsString(args, "action") ?? "list";
    if (action === "clear") {
      record.networkRequests = [];
      record.networkRequestIds = new WeakMap();
      return jsonResult({ cleared: true });
    }
    if (action === "get") {
      const id = requiredDevtoolsInteger(args, "request_id");
      const entry = record.networkRequests.find(candidate => candidate.id === id);
      if (!entry) throw invalidDevtoolsArguments(`network request not found: ${id}`);
      let responseBody: string | null = null;
      let bodyTruncated = false;
      if (args.include_body === true && entry.response) {
        const body = await entry.response.body().catch(() => Buffer.alloc(0));
        bodyTruncated = body.length > MAX_NETWORK_BODY_BYTES;
        responseBody = body.subarray(0, MAX_NETWORK_BODY_BYTES).toString("utf8");
      }
      return jsonResult({ request: publicNetworkEntry(entry), response_body: responseBody, body_truncated: bodyTruncated });
    }
    if (action !== "list") throw invalidDevtoolsArguments("invalid network action");
    const resourceTypes = new Set(devtoolsStringArray(args.resource_types));
    const pageSize = boundedInteger(args.page_size, 100, 1, 500);
    const requests = record.networkRequests
      .filter(entry => resourceTypes.size === 0 || resourceTypes.has(entry.resourceType))
      .slice(-pageSize)
      .map(publicNetworkEntry);
    return jsonResult({ requests, total: record.networkRequests.length });
  }

  private async dialogOperation(
    record: PageRecord,
    args: Record<string, unknown>,
  ): Promise<ExecutedCommand> {
    const action = optionalDevtoolsString(args, "action") ?? "list";
    if (action === "clear") {
      record.dialogs = [];
      return jsonResult({ cleared: true });
    }
    if (action === "list") {
      return jsonResult({
        dialogs: record.dialogs.slice(-100),
        pending: record.pendingDialog ? {
          id: record.pendingDialog.entry.id,
          dialog_type: record.pendingDialog.entry.dialogType,
          message: record.pendingDialog.entry.message,
          default_value: record.pendingDialog.entry.defaultValue,
        } : null,
      });
    }
    if (action !== "accept" && action !== "dismiss") {
      throw invalidDevtoolsArguments("invalid dialog action");
    }
    const pending = record.pendingDialog;
    if (!pending) throw invalidDevtoolsArguments("no browser dialog is pending");
    clearTimeout(pending.timeout);
    record.pendingDialog = undefined;
    try {
      if (action === "accept") {
        await pending.dialog.accept(optionalDevtoolsString(args, "prompt_text"));
        pending.entry.handledAs = "accepted";
      } else {
        await pending.dialog.dismiss();
        pending.entry.handledAs = "dismissed";
      }
    } catch (error) {
      throw new ProtocolFailure(
        "browser_dialog_handle_failed",
        error instanceof Error ? error.message : String(error),
        true,
        true,
      );
    }
    return Promise.resolve(jsonResult({ handled: action, dialog_id: pending.entry.id }));
  }

  private async emulateOperation(
    record: PageRecord,
    args: Record<string, unknown>,
  ): Promise<ExecutedCommand> {
    const page = this.requirePage(record);
    const cdp = await this.cdpSession(record);
    const colorScheme = optionalDevtoolsString(args, "color_scheme");
    if (colorScheme && !["auto", "dark", "light"].includes(colorScheme)) {
      throw invalidDevtoolsArguments("color_scheme must be auto, dark, or light");
    }
    await page.emulateMedia({
      colorScheme: !colorScheme || colorScheme === "auto"
        ? null
        : colorScheme as "dark" | "light",
    });
    const cpuRate = optionalDevtoolsNumber(args, "cpu_throttling_rate");
    if (cpuRate !== undefined && (cpuRate < 1 || cpuRate > 20)) {
      throw invalidDevtoolsArguments("cpu_throttling_rate must be between 1 and 20");
    }
    await cdp.send("Emulation.setCPUThrottlingRate", { rate: cpuRate ?? 1 });
    const userAgent = optionalDevtoolsString(args, "user_agent");
    if (userAgent) await cdp.send("Emulation.setUserAgentOverride", { userAgent });
    else await this.applyUserAgent(record, record.viewport.device_type);
    if (args.geolocation && typeof args.geolocation === "object") {
      const location = args.geolocation as Record<string, unknown>;
      const latitude = requiredFiniteDevtoolsNumber(location, "latitude");
      const longitude = requiredFiniteDevtoolsNumber(location, "longitude");
      const accuracy = optionalDevtoolsNumber(location, "accuracy") ?? 1;
      if (latitude < -90 || latitude > 90 || longitude < -180 || longitude > 180 || accuracy < 0) {
        throw invalidDevtoolsArguments("geolocation is outside the supported coordinate range");
      }
      await cdp.send("Emulation.setGeolocationOverride", {
        latitude,
        longitude,
        accuracy,
      });
    } else {
      await cdp.send("Emulation.clearGeolocationOverride");
    }
    if (Object.prototype.hasOwnProperty.call(args, "extra_http_headers")) {
      if (args.extra_http_headers !== null && typeof args.extra_http_headers !== "object") {
        throw invalidDevtoolsArguments("extra_http_headers must be an object or null");
      }
      const headers = args.extra_http_headers && typeof args.extra_http_headers === "object"
        ? Object.fromEntries(
          Object.entries(args.extra_http_headers as Record<string, unknown>)
            .filter((entry): entry is [string, string] => typeof entry[1] === "string"),
        )
        : {};
      await cdp.send("Network.enable");
      await cdp.send("Network.setExtraHTTPHeaders", { headers });
    }
    const network = optionalDevtoolsString(args, "network_conditions");
    await cdp.send("Network.enable");
    await cdp.send("Network.emulateNetworkConditions", network
      ? networkCondition(network)
      : { offline: false, latency: 0, downloadThroughput: -1, uploadThroughput: -1 });
    return jsonResult({ configured: true });
  }

  private async performanceOperation(
    record: PageRecord,
    args: Record<string, unknown>,
  ): Promise<ExecutedCommand> {
    const action = optionalDevtoolsString(args, "action") ?? "metrics";
    const cdp = await this.cdpSession(record);
    if (action === "start") {
      if (record.trace) throw invalidDevtoolsArguments("performance trace already running");
      const trace: BrowserTraceState = {
        events: [],
        startedAt: Date.now(),
        listener: event => {
          if (!Array.isArray(event.value) || trace.events.length >= MAX_TRACE_EVENTS) return;
          trace.events.push(...event.value.slice(0, MAX_TRACE_EVENTS - trace.events.length));
        },
      };
      record.trace = trace;
      cdp.on("Tracing.dataCollected", trace.listener);
      await cdp.send("Tracing.start", {
        categories: "devtools.timeline,blink.user_timing,loading,disabled-by-default-devtools.timeline",
        transferMode: "ReportEvents",
      });
      if (args.reload === true) {
        await this.requirePage(record).reload({ waitUntil: "domcontentloaded" });
        await this.waitForStableDom(this.requirePage(record));
      }
      if (args.auto_stop === true) {
        return this.performanceOperation(record, { ...args, action: "stop" });
      }
      return jsonResult({ tracing: true, started_at: trace.startedAt });
    }
    if (action === "stop") {
      const trace = record.trace;
      if (!trace) throw invalidDevtoolsArguments("performance trace is not running");
      await new Promise<void>((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error("performance trace stop timed out")), 10_000);
        cdp.once("Tracing.tracingComplete", () => {
          clearTimeout(timer);
          resolve();
        });
        void cdp.send("Tracing.end").catch(reject);
      });
      cdp.off("Tracing.dataCollected", trace.listener);
      record.trace = undefined;
      record.lastTraceSummary = summarizeTrace(trace.events, trace.startedAt);
      const filePath = optionalDevtoolsString(args, "file_path");
      if (filePath) {
        await mkdir(dirname(filePath), { recursive: true });
        await writeFile(filePath, JSON.stringify({ traceEvents: trace.events }), "utf8");
        record.lastTraceSummary.file_path = filePath;
      }
      return jsonResult(record.lastTraceSummary);
    }
    await cdp.send("Performance.enable");
    const metrics = await cdp.send("Performance.getMetrics");
    if (action === "analyze") {
      const trace = record.lastTraceSummary ?? null;
      const insightName = optionalDevtoolsString(args, "insight_name");
      const insightSetId = optionalDevtoolsString(args, "insight_set_id");
      if (insightName || insightSetId) {
        if (!trace) throw invalidDevtoolsArguments("no completed performance trace is available");
        const sets = Array.isArray(trace.insight_sets) ? trace.insight_sets as Array<Record<string, unknown>> : [];
        const set = sets.find(candidate => !insightSetId || candidate.id === insightSetId);
        const insights = set && Array.isArray(set.insights) ? set.insights as Array<Record<string, unknown>> : [];
        const insight = insights.find(candidate => !insightName || candidate.name === insightName);
        if (!set || !insight) throw invalidDevtoolsArguments("performance insight was not found in the latest trace");
        return jsonResult({ insight_set: set.id, insight, metrics: metrics.metrics });
      }
      return jsonResult({ metrics: metrics.metrics, trace });
    }
    if (action !== "metrics") throw invalidDevtoolsArguments("invalid performance action");
    return jsonResult({ metrics: metrics.metrics });
  }

  private async lighthouseOperation(
    record: PageRecord,
    args: Record<string, unknown>,
  ): Promise<ExecutedCommand> {
    const endpoint = this.#devtoolsBrowserWSEndpoint;
    if (!endpoint) {
      throw new ProtocolFailure(
        "browser_lighthouse_unavailable",
        "Chromium DevTools 连接地址不可用",
        true,
        false,
      );
    }
    const mode = optionalDevtoolsString(args, "mode") ?? "navigation";
    const device = optionalDevtoolsString(args, "device") ?? "desktop";
    if (mode !== "navigation" && mode !== "snapshot") {
      throw invalidDevtoolsArguments("lighthouse mode must be navigation or snapshot");
    }
    if (device !== "desktop" && device !== "mobile") {
      throw invalidDevtoolsArguments("lighthouse device must be desktop or mobile");
    }
    const cdp = await this.cdpSession(record);
    const targetInfo = await cdp.send("Target.getTargetInfo");
    const [{ default: puppeteer }, lighthouse] = await Promise.all([
      import("puppeteer-core"),
      import("lighthouse"),
    ]);
    const browser = await puppeteer.connect({
      browserWSEndpoint: endpoint,
      defaultViewport: null,
    });
    try {
      const target = browser.targets().find(candidate => (
        candidate as unknown as { _targetId?: string }
      )._targetId === targetInfo.targetInfo.targetId);
      const puppeteerPage = await target?.page();
      if (!puppeteerPage) {
        throw new Error(`Lighthouse target not found: ${targetInfo.targetInfo.targetId}`);
      }
      const flags = {
        onlyCategories: ["accessibility", "seo", "best-practices", "agentic-browsing"],
        output: ["json", "html"],
        maxWaitForLoad: 30_000,
        formFactor: device,
        screenEmulation: device === "desktop"
          ? { mobile: false, width: 1_350, height: 940, deviceScaleFactor: 1, disabled: false }
          : { mobile: true, width: 412, height: 823, deviceScaleFactor: 1.75, disabled: false },
      } as const;
      const result = mode === "navigation"
        ? await lighthouse.navigation(puppeteerPage as never, puppeteerPage.url(), { flags: flags as never })
        : await lighthouse.snapshot(puppeteerPage as never, { flags: flags as never });
      if (!result) throw new Error("Lighthouse audit did not produce a result");
      const outputDir = optionalDevtoolsString(args, "output_dir_path")
        ?? join(this.#config.downloadPath, `lighthouse-${Date.now()}-${record.tabId}`);
      await mkdir(outputDir, { recursive: true });
      const jsonPath = join(outputDir, "report.json");
      const htmlPath = join(outputDir, "report.html");
      await Promise.all([
        writeFile(jsonPath, lighthouse.generateReport(result.lhr, "json"), "utf8"),
        writeFile(htmlPath, lighthouse.generateReport(result.lhr, "html"), "utf8"),
      ]);
      const categories = Object.values(result.lhr.categories).map(category => ({
        id: category.id,
        title: category.title,
        score: category.score,
      }));
      const audits = Object.values(result.lhr.audits);
      return jsonResult({
        mode,
        device,
        url: result.lhr.mainDocumentUrl,
        scores: categories,
        audits: {
          passed: audits.filter(audit => audit.score === 1).length,
          failed: audits.filter(audit => audit.score !== null && audit.score < 1).length,
        },
        timing: { total: result.lhr.timing.total },
        reports: [jsonPath, htmlPath],
      });
    } finally {
      browser.disconnect();
    }
  }

  private async heapOperation(
    record: PageRecord,
    args: Record<string, unknown>,
  ): Promise<ExecutedCommand> {
    const action = optionalDevtoolsString(args, "action") ?? "usage";
    const cdp = await this.cdpSession(record);
    if (action === "usage") {
      const [heap, dom] = await Promise.all([
        cdp.send("Runtime.getHeapUsage"),
        cdp.send("Memory.getDOMCounters"),
      ]);
      return jsonResult({ heap, dom });
    }
    if (action === "take_snapshot") {
      const chunks: string[] = [];
      const listener = (event: { chunk?: string }) => {
        if (typeof event.chunk === "string") chunks.push(event.chunk);
      };
      cdp.on("HeapProfiler.addHeapSnapshotChunk", listener);
      try {
        await cdp.send("HeapProfiler.enable");
        await cdp.send("HeapProfiler.takeHeapSnapshot", { reportProgress: false });
      } finally {
        cdp.off("HeapProfiler.addHeapSnapshotChunk", listener);
      }
      const bytes = Buffer.from(chunks.join(""));
      const filePath = optionalDevtoolsString(args, "file_path")
        ?? join(this.#config.downloadPath, `${Date.now()}-${record.tabId}.heapsnapshot`);
      await mkdir(dirname(filePath), { recursive: true });
      await writeFile(filePath, bytes);
      return jsonResult({ file_path: filePath, byte_length: bytes.length });
    }
    if (action === "close_snapshot") {
      const filePath = requiredDevtoolsString(args, "file_path");
      return jsonResult({ file_path: filePath, closed: this.#heapSnapshots.delete(filePath) });
    }
    if (action === "compare_snapshots") {
      const basePath = requiredDevtoolsString(args, "base_file_path");
      const currentPath = requiredDevtoolsString(args, "current_file_path");
      const [base, current] = await Promise.all([
        this.loadHeapSnapshot(basePath),
        this.loadHeapSnapshot(currentPath),
      ]);
      return jsonResult(base.compare(current, optionalDevtoolsInteger(args, "class_id")));
    }
    const filePath = requiredDevtoolsString(args, "file_path");
    const snapshot = await this.loadHeapSnapshot(filePath);
    const pageIndex = boundedInteger(args.page_index, 0, 0, Number.MAX_SAFE_INTEGER);
    const pageSize = boundedInteger(args.page_size, 100, 1, 500);
    if (action === "summary") return jsonResult(snapshot.summary());
    if (action === "details") return jsonResult(snapshot.details(pageIndex, pageSize));
    if (action === "class_nodes") {
      return jsonResult(snapshot.classNodes(requiredDevtoolsInteger(args, "class_id"), pageIndex, pageSize));
    }
    if (action === "dominators") return jsonResult(snapshot.dominators(requiredDevtoolsInteger(args, "node_id")));
    if (action === "duplicate_strings") return jsonResult(snapshot.duplicateStrings(pageIndex, pageSize));
    if (action === "edges") {
      return jsonResult(snapshot.edges(requiredDevtoolsInteger(args, "node_id"), pageIndex, pageSize));
    }
    if (action === "object_details") {
      return jsonResult(snapshot.objectDetails(requiredDevtoolsInteger(args, "node_id")));
    }
    if (action === "retainers") {
      return jsonResult(snapshot.retainersFor(requiredDevtoolsInteger(args, "node_id"), pageIndex, pageSize));
    }
    if (action === "retaining_paths") {
      return jsonResult(snapshot.retainingPaths(
        requiredDevtoolsInteger(args, "node_id"),
        boundedInteger(args.max_depth, 8, 1, 64),
        boundedInteger(args.max_nodes, 200, 1, 10_000),
        boundedInteger(args.max_siblings, 20, 1, 1_000),
      ));
    }
    throw invalidDevtoolsArguments("invalid heap action");
  }

  private async loadHeapSnapshot(filePath: string): Promise<HeapSnapshotModel> {
    const cached = this.#heapSnapshots.get(filePath);
    if (cached) return cached;
    const snapshot = await HeapSnapshotModel.load(filePath);
    this.#heapSnapshots.set(filePath, snapshot);
    return snapshot;
  }

  private async extensionsOperation(
    record: PageRecord,
    args: Record<string, unknown>,
  ): Promise<ExecutedCommand> {
    const action = optionalDevtoolsString(args, "action") ?? "list";
    const browser = await this.puppeteerBrowser();
    const extensions = await browser.extensions();
    if (action === "list") {
      return jsonResult({ extensions: [...extensions.values()].map(extension => ({
        id: extension.id,
        path: extension.path,
        name: extension.name,
        version: extension.version,
        enabled: extension.enabled,
      })) });
    }
    if (action === "install") {
      const id = await browser.installExtension(requiredDevtoolsString(args, "path"));
      const extension = (await browser.extensions()).get(id);
      return jsonResult({ result: extension ? {
        id: extension.id,
        path: extension.path,
        name: extension.name,
        version: extension.version,
        enabled: extension.enabled,
      } : { id } });
    }
    const id = requiredDevtoolsString(args, "extension_id");
    const extension = extensions.get(id);
    if (action === "uninstall") {
      await browser.uninstallExtension(id);
      return jsonResult({ uninstalled: id });
    }
    if (!extension) throw invalidDevtoolsArguments(`extension not found: ${id}`);
    if (action === "reload") {
      await browser.installExtension(extension.path);
      return jsonResult({ reloaded: id });
    }
    if (action === "trigger_action") {
      const page = await this.puppeteerPage(record);
      await extension.triggerAction(page);
      return jsonResult({ triggered: id, url: page.url() });
    }
    throw invalidDevtoolsArguments("invalid extensions action");
  }

  private async thirdPartyOperation(
    page: Page,
    args: Record<string, unknown>,
  ): Promise<ExecutedCommand> {
    const action = optionalDevtoolsString(args, "action") ?? "list";
    return jsonResult(await page.evaluate(async input => {
      const api = (globalThis as typeof globalThis & { __dtmcp?: { listTools?: () => unknown; executeTool?: (name: string, params: unknown) => unknown } }).__dtmcp;
      if (!api) return { available: false, tools: [] };
      if (input.action === "execute") {
        if (!api.executeTool) throw new Error("third-party developer tool execution is unavailable");
        return { available: true, result: await api.executeTool(String(input.tool_name ?? ""), input.params ?? {}) };
      }
      return { available: true, tools: api.listTools ? await api.listTools() : [] };
    }, { action, tool_name: args.tool_name, params: args.params }));
  }

  private async webMcpOperation(
    page: Page,
    args: Record<string, unknown>,
  ): Promise<ExecutedCommand> {
    const action = optionalDevtoolsString(args, "action") ?? "list";
    return jsonResult(await page.evaluate(async input => {
      const context = (navigator as Navigator & { modelContext?: { tools?: Array<{ name: string; execute?: (value: unknown) => unknown }> } }).modelContext;
      const tools = Array.isArray(context?.tools) ? context.tools : [];
      if (input.action === "execute") {
        const tool = tools.find(candidate => candidate.name === input.tool_name);
        if (!tool?.execute) throw new Error(`WebMCP tool not found: ${String(input.tool_name ?? "")}`);
        return { available: true, result: await tool.execute(input.input ?? {}) };
      }
      return { available: Boolean(context), tools: tools.map(tool => ({ name: tool.name })) };
    }, { action, tool_name: args.tool_name, input: args.input }));
  }

  private async pwaOperation(
    _record: PageRecord,
    args: Record<string, unknown>,
  ): Promise<ExecutedCommand> {
    const action = requiredDevtoolsString(args, "action");
    const manifestId = requiredDevtoolsString(args, "manifest_id");
    const cdp = await this.browserCdpSession();
    if (action === "state") {
      return jsonResult(await cdp.send("PWA.getOsAppState" as never, { manifestId } as never));
    }
    if (action === "install") {
      const installResult = await cdp.send("PWA.install" as never, {
        manifestId,
        installUrlOrBundleUrl: requiredDevtoolsString(args, "install_url"),
      } as never);
      const displayMode = optionalDevtoolsString(args, "display_mode");
      if (displayMode && displayMode !== "browser" && displayMode !== "standalone") {
        throw invalidDevtoolsArguments("display_mode must be browser or standalone");
      }
      if (displayMode) {
        await cdp.send("PWA.changeAppUserSettings" as never, {
          manifestId,
          displayMode,
        } as never);
      }
      return jsonResult({ manifest_id: manifestId, display_mode: displayMode, result: installResult });
    }
    if (action === "launch") {
      return jsonResult(await cdp.send("PWA.launch" as never, {
        manifestId,
        url: optionalDevtoolsString(args, "url"),
      } as never));
    }
    if (action === "uninstall") {
      return jsonResult(await cdp.send("PWA.uninstall" as never, { manifestId } as never));
    }
    throw invalidDevtoolsArguments("invalid PWA action");
  }

  private async recordingOperation(
    record: PageRecord,
    args: Record<string, unknown>,
  ): Promise<ExecutedCommand> {
    const action = requiredDevtoolsString(args, "action");
    if (action === "start") {
      if (record.recording) {
        throw invalidDevtoolsArguments("a browser recording is already in progress for this tab");
      }
      let filePath = optionalDevtoolsString(args, "file_path")
        ?? join(this.#config.downloadPath, `${Date.now()}-${record.tabId}.mp4`);
      let extension = filePath.toLowerCase().match(/\.(mp4|webm)$/)?.[1] as "mp4" | "webm" | undefined;
      if (!extension) {
        if (filePath.includes(".")) {
          throw invalidDevtoolsArguments("browser recording file_path must use .mp4 or .webm");
        }
        extension = "mp4";
        filePath = `${filePath}.mp4`;
      }
      await mkdir(dirname(filePath), { recursive: true });
      const page = await this.puppeteerPage(record);
      const recorder = await page.screencast({
        path: filePath as `${string}.mp4` | `${string}.webm`,
        format: extension,
        ffmpegPath: optionalDevtoolsString(args, "ffmpeg_path"),
      });
      record.recording = {
        recorder,
        filePath,
        format: extension,
        startedAt: Date.now(),
      };
      return jsonResult({
        recording: true,
        file_path: filePath,
        format: extension,
        started_at: record.recording.startedAt,
      });
    }
    if (action === "stop") {
      const recording = record.recording;
      if (!recording) {
        throw invalidDevtoolsArguments("no browser recording is active for this tab");
      }
      record.recording = undefined;
      await recording.recorder.stop();
      const metadata = await stat(recording.filePath);
      return jsonResult({
        recording: false,
        file_path: recording.filePath,
        format: recording.format,
        byte_length: metadata.size,
        duration_millis: Date.now() - recording.startedAt,
      });
    }
    throw invalidDevtoolsArguments("browser recording action must be start or stop");
  }

  private async screenshot(input: {
    tab_id: string;
    target?: { snapshot_revision: number; element_ref: string } | null;
    clip?: NormalizedRect | null;
    full_page: boolean;
    format: "png" | "jpeg" | "webp";
    quality?: number;
  }): Promise<ExecutedCommand> {
    if (Number(Boolean(input.target)) + Number(Boolean(input.clip)) + Number(input.full_page) > 1) {
      throw new ProtocolFailure(
        "browser_screenshot_scope_invalid",
        "screenshot target, clip, and full_page are mutually exclusive",
        false,
        false,
      );
    }
    if (input.clip) validateNormalizedRect(input.clip);
    const record = this.pageRecord(input.tab_id);
    await this.ensurePage(record);
    const page = this.requirePage(record);
    const cdp = await this.cdpSession(record);
    const metrics = await cdp.send("Page.getLayoutMetrics");
    const deviceScaleFactor = this.#config.deviceScaleFactor;
    let clip: {
      x: number;
      y: number;
      width: number;
      height: number;
      scale: number;
    };
    let captureBeyondViewport: boolean;
    if (input.target) {
      const locator = await record.snapshot.resolve(page, input.target);
      const bounds = await locator.boundingBox();
      if (!bounds || bounds.width <= 0 || bounds.height <= 0) {
        throw new ProtocolFailure(
          "browser_screenshot_target_unavailable",
          "screenshot target is not visible",
          true,
          false,
        );
      }
      clip = {
        x: bounds.x + metrics.cssVisualViewport.pageX,
        y: bounds.y + metrics.cssVisualViewport.pageY,
        width: bounds.width,
        height: bounds.height,
        scale: 1 / deviceScaleFactor,
      };
      captureBeyondViewport = true;
    } else if (input.clip) {
      const visualViewport = metrics.cssVisualViewport;
      const visualScale = Math.min(
        record.viewport.width / Math.max(1, visualViewport.clientWidth),
        record.viewport.height / Math.max(1, visualViewport.clientHeight),
      );
      clip = {
        x: visualViewport.pageX + input.clip.x * visualViewport.clientWidth,
        y: visualViewport.pageY + input.clip.y * visualViewport.clientHeight,
        width: Math.max(1, input.clip.width * visualViewport.clientWidth),
        height: Math.max(1, input.clip.height * visualViewport.clientHeight),
        scale: visualScale / deviceScaleFactor,
      };
      captureBeyondViewport = false;
    } else {
      const visualViewport = metrics.cssVisualViewport;
      const visualScale = Math.min(
        record.viewport.width / Math.max(1, visualViewport.clientWidth),
        record.viewport.height / Math.max(1, visualViewport.clientHeight),
      );
      const bounds = input.full_page
        ? metrics.cssContentSize
        : {
            x: visualViewport.pageX,
            y: visualViewport.pageY,
            width: visualViewport.clientWidth,
            height: visualViewport.clientHeight,
          };
      clip = {
        x: bounds.x,
        y: bounds.y,
        width: Math.max(1, bounds.width),
        height: Math.max(1, bounds.height),
        scale: visualScale / deviceScaleFactor,
      };
      captureBeyondViewport = input.full_page;
    }
    const capture = await cdp.send("Page.captureScreenshot", {
      format: input.format,
      quality: input.format === "png" ? undefined : Math.max(0, Math.min(100, Math.floor(input.quality ?? 90))),
      fromSurface: true,
      captureBeyondViewport,
      clip,
    });
    const bytes = Buffer.from(capture.data, "base64");
    const payload = binaryPayload(
      bytes,
      input.format === "png" ? "image/png" : input.format === "webp" ? "image/webp" : "image/jpeg",
    );
    return {
      result: { type: "binary_payload", payload },
      binary: bytes,
    };
  }

  private async hitTest(input: {
    tab_id: string;
    navigation_revision: number;
    x: number;
    y: number;
  }): Promise<ExecutedCommand> {
    const record = this.pageRecord(input.tab_id);
    await this.ensurePage(record);
    const page = this.requirePage(record);
    if (input.navigation_revision !== record.navigationRevision) {
      throw new ProtocolFailure(
        "browser_navigation_stale",
        `navigation changed: current=${record.navigationRevision}, received=${input.navigation_revision}`,
        true,
        false,
      );
    }
    requireFinite("x", input.x);
    requireFinite("y", input.y);
    if (input.x < 0 || input.x > 1 || input.y < 0 || input.y > 1) {
      throw new ProtocolFailure(
        "browser_hit_test_invalid_coordinates",
        "hit-test coordinates must be normalized to [0, 1]",
        false,
        false,
      );
    }
    const cssViewport = await page.evaluate(() => ({
      width: Math.max(1, window.innerWidth),
      height: Math.max(1, window.innerHeight),
    }));
    const hit = await page.evaluate(
      ({ x, y, frameSequence, navigationRevision, viewportWidth, viewportHeight }): HitTest | null => {
        const element = document.elementFromPoint(x, y);
        if (!element) return null;
        const rect = element.getBoundingClientRect();
        const text = (value: string | null | undefined, max: number) => {
          const normalized = (value ?? "").replace(/\s+/g, " ").trim();
          return normalized.length > max
            ? `${normalized.slice(0, max - 1)}…`
            : normalized;
        };
        const cssPath = (target: Element): string => {
          const id = target.getAttribute("id");
          if (id) return `#${CSS.escape(id)}`;
          const testId = target.getAttribute("data-testid");
          if (testId) return `[data-testid="${CSS.escape(testId)}"]`;
          const parts: string[] = [];
          let current: Element | null = target;
          while (current && current.tagName.toLowerCase() !== "html") {
            const tag = current.tagName.toLowerCase();
            const siblings = current.parentElement
              ? Array.from(current.parentElement.children).filter(
                  (candidate) => candidate.tagName === current?.tagName,
                )
              : [];
            parts.unshift(`${tag}:nth-of-type(${siblings.indexOf(current) + 1})`);
            current = current.parentElement;
          }
          return `html > ${parts.join(" > ")}`;
        };
        const ariaRole = element.getAttribute("role");
        const ariaName =
          text(
            element.getAttribute("aria-label") ??
              element.getAttribute("alt") ??
              element.getAttribute("title"),
            240,
          ) || null;
        const textExcerpt = text(element.textContent, 240) || null;
        const ancestorFingerprint = Array.from(
          { length: 4 },
          (_, index) => index,
        )
          .reduce<Element[]>((ancestors, _) => {
            const last = ancestors.at(-1) ?? element;
            if (last.parentElement) ancestors.push(last.parentElement);
            return ancestors;
          }, [])
          .map((ancestor) => ancestor.tagName.toLowerCase())
          .join("/");
        const domFingerprint = [
          element.tagName.toLowerCase(),
          element.getAttribute("id") ?? "",
          element.getAttribute("data-testid") ?? "",
          ariaRole ?? "",
          ariaName ?? "",
          textExcerpt?.slice(0, 80) ?? "",
        ].join("|");
        return {
          frame_sequence: frameSequence,
          navigation_revision: navigationRevision,
          viewport_width: viewportWidth,
          viewport_height: viewportHeight,
          scroll_x: window.scrollX,
          scroll_y: window.scrollY,
          element_ref: `hit-${frameSequence}-${Math.round(x)}-${Math.round(y)}`,
          tag_name: element.tagName.toLowerCase(),
          test_id: element.getAttribute("data-testid"),
          stable_id: element.getAttribute("id"),
          aria_role: ariaRole,
          aria_name: ariaName,
          text_excerpt: textExcerpt,
          css_path: cssPath(element),
          ancestor_fingerprint: ancestorFingerprint,
          dom_fingerprint: domFingerprint,
          bounds: {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
          },
        };
      },
      {
        x: input.x * cssViewport.width,
        y: input.y * cssViewport.height,
        frameSequence: record.frameSequence,
        navigationRevision: input.navigation_revision,
        viewportWidth: cssViewport.width,
        viewportHeight: cssViewport.height,
      },
    );
    if (!hit) {
      throw new ProtocolFailure(
        "browser_hit_test_empty",
        "no page element exists at the requested coordinates",
        true,
        false,
      );
    }
    return { result: { type: "hit_test", payload: hit } };
  }

  private showAgentCursor(
    record: PageRecord,
    x: number,
    y: number,
    action: AgentCursorAction,
  ): void {
    const width = record.viewport.width;
    const height = record.viewport.height;
    if (!Number.isFinite(x) || !Number.isFinite(y) || width < 1 || height < 1) return;
    record.agentCursor = {
      x: Math.max(0, Math.min(1, x / width)),
      y: Math.max(0, Math.min(1, y / height)),
      action,
    };
    this.emitAgentCursor(record);
  }

  private emitAgentCursor(record: PageRecord): void {
    const cursor = record.agentCursor;
    if (!cursor) return;
    this.#transport.emit({
      type: "agent_cursor",
      payload: {
        tab_id: record.tabId,
        visible: true,
        x: cursor.x,
        y: cursor.y,
        action: cursor.action,
      },
    });
  }

  private ensureAgentCursor(record: PageRecord): void {
    if (this.control.mode !== "agent" || !record.page) return;
    if (record.agentCursor) return;
    this.showAgentCursor(
      record,
      record.viewport.width / 2,
      record.viewport.height / 2,
      "move",
    );
  }

  private hideAgentCursor(record: PageRecord): void {
    record.agentCursor = undefined;
    this.#transport.emit({
      type: "agent_cursor",
      payload: {
        tab_id: record.tabId,
        visible: false,
        x: null,
        y: null,
        action: null,
      },
    });
  }

  private hideAgentCursors(): void {
    for (const record of this.#pages.values()) this.hideAgentCursor(record);
  }

  private showAgentCursors(): void {
    for (const record of this.#pages.values()) {
      if (!record.page) continue;
      if (record.agentCursor) this.emitAgentCursor(record);
      else this.ensureAgentCursor(record);
    }
  }

  private async startScreencast(input: {
    tab_id: string;
    format: "jpeg" | "png";
    quality: number;
    max_width: number;
    max_height: number;
  }): Promise<ExecutedCommand> {
    const record = this.pageRecord(input.tab_id);
    await this.ensurePage(record, true);
    record.screencastSettings = {
      format: input.format,
      quality: Math.max(0, Math.min(Math.floor(input.quality), 100)),
      maxWidth: Math.max(320, Math.min(Math.floor(input.max_width), MAX_SCREENCAST_WIDTH)),
      maxHeight: Math.max(240, Math.min(Math.floor(input.max_height), MAX_SCREENCAST_HEIGHT)),
    };
    await this.restartScreencast(record);
    return emptyResult();
  }

  private async emitCurrentViewportFrame(
    record: PageRecord,
    geometry?: {
      width: number;
      height: number;
      deviceScaleFactorMillis: number;
    },
  ): Promise<void> {
    const settings = record.screencastSettings;
    if (!settings || !record.page) return;
    const cdp = await this.cdpSession(record);
    let bytes: Buffer | undefined;
    for (let attempt = 0; attempt < 2; attempt += 1) {
      const deviceScaleFactor = this.#config.deviceScaleFactor;
      const physicalWidth = record.viewport.width * deviceScaleFactor;
      const physicalHeight = record.viewport.height * deviceScaleFactor;
      const scale = Math.min(
        1,
        settings.maxWidth / physicalWidth,
        settings.maxHeight / physicalHeight,
      );
      const capture = await cdp.send("Page.captureScreenshot", {
        format: settings.format,
        quality: settings.format === "jpeg" ? settings.quality : undefined,
        fromSurface: true,
        captureBeyondViewport: false,
        clip: {
          x: 0,
          y: 0,
          width: record.viewport.width,
          height: record.viewport.height,
          scale,
        },
      });
      const candidate = Buffer.from(capture.data, "base64");
      if (this.screencastBitmapMatchesViewport(record, settings, candidate)) {
        bytes = candidate;
        break;
      }
      await this.applyViewport(record, record.viewport, true);
    }
    if (!bytes) {
      throw new ProtocolFailure(
        "browser_screencast_frame_size_mismatch",
        "Chromium screenshot dimensions do not match the active browser viewport",
        true,
        false,
        "浏览器画面尺寸尚未稳定，请稍后重试。",
      );
    }
    record.frameSequence += 1;
    const payload = binaryPayload(
      bytes,
      settings.format === "png" ? "image/png" : "image/jpeg",
    );
    const metadata: ScreencastFrame = {
      tab_id: record.tabId,
      frame_sequence: record.frameSequence,
      navigation_revision: record.navigationRevision,
      payload_id: payload.payload_id,
      mime_type: payload.mime_type,
      byte_length: payload.byte_length,
      sha256: payload.sha256,
      width: geometry?.width ?? record.viewport.width,
      height: geometry?.height ?? record.viewport.height,
      surface_width: Math.max(
        1,
        Math.round(record.viewport.width * viewportSurfaceScale(record.viewport)),
      ),
      surface_height: Math.max(
        1,
        Math.round(record.viewport.height * viewportSurfaceScale(record.viewport)),
      ),
      device_scale_factor_millis: geometry?.deviceScaleFactorMillis ?? Math.max(
        1,
        Math.round(this.#config.deviceScaleFactor * 1_000),
      ),
    };
    if (this.#transport.emitScreencast({ type: "screencast_frame", payload: metadata }, bytes)) {
      record.hasPresentedFrame = true;
    }
  }

  private async restartScreencast(record: PageRecord): Promise<void> {
    const settings = record.screencastSettings;
    if (!settings) return;
    await this.stopScreencastSession(record);
    const cdp = await this.cdpSession(record);
    let acceptFirstFrame: (() => void) | undefined;
    const firstFrame = new Promise<void>((accept) => {
      acceptFirstFrame = accept;
    });
    const listener = (frame: ScreencastFrameEvent) => {
      const bytes = Buffer.from(frame.data, "base64");
      const pageScaleFactor = Number.isFinite(frame.metadata.pageScaleFactor)
        && frame.metadata.pageScaleFactor > 0
        ? frame.metadata.pageScaleFactor
        : 1;
      const width = Math.max(
        1,
        Math.round(frame.metadata.deviceWidth / pageScaleFactor),
      );
      const height = Math.max(
        1,
        Math.round(frame.metadata.deviceHeight / pageScaleFactor),
      );
      // Chromium 偶尔会在 Page.startScreencast 后发出虚拟窗口尺寸的旧首帧，
      // 同时 metadata 却已经是新 View 的尺寸。只检查 metadata 会把一张
      // 7680px 位图当成 478px 面板帧，最终在 UI 中被非等比压扁。
      // 二进制位图尺寸才是生产端协议边界，必须与当前设备视口和 DPR 一致。
      const frameMatchesViewport = this.screencastBitmapMatchesViewport(
        record,
        settings,
        bytes,
      );
      record.screencastAck = record.screencastAck
        .catch(() => undefined)
        .then(async () => {
          await cdp
            .send("Page.screencastFrameAck", { sessionId: frame.sessionId })
            .catch(() => undefined);
        });
      if (!frameMatchesViewport) {
        if (!record.screencastViewportRefreshScheduled) {
          record.screencastViewportRefreshScheduled = true;
          void this.applyViewport(record, record.viewport, true)
            .catch(() => undefined)
            .finally(() => {
              record.screencastViewportRefreshScheduled = false;
            });
        }
        return;
      }
      // 导航期间 Chromium 会短暂提交空白合成帧。面板继续展示上一张已发布
      // 画面，直到新文档稳定后由 completeNavigationPresentation 原子替换。
      if (record.presentationPhase === "navigation_pending" && record.hasPresentedFrame) return;
      record.frameSequence += 1;
      const payload = binaryPayload(
        bytes,
        settings.format === "png" ? "image/png" : "image/jpeg",
      );
      const metadata: ScreencastFrame = {
        tab_id: record.tabId,
        frame_sequence: record.frameSequence,
        navigation_revision: record.navigationRevision,
        payload_id: payload.payload_id,
        mime_type: payload.mime_type,
        byte_length: payload.byte_length,
        sha256: payload.sha256,
        width,
        height,
        surface_width: Math.max(1, Math.round(record.viewport.width * viewportSurfaceScale(record.viewport))),
        surface_height: Math.max(1, Math.round(record.viewport.height * viewportSurfaceScale(record.viewport))),
        device_scale_factor_millis: Math.max(
          1,
          Math.round(
            pageScaleFactor
              * this.#config.deviceScaleFactor
              * 1_000,
          ),
        ),
      };
      if (this.#transport.emitScreencast({ type: "screencast_frame", payload: metadata }, bytes)) {
        record.hasPresentedFrame = true;
        acceptFirstFrame?.();
        acceptFirstFrame = undefined;
      }
    };
    record.screencastListener = listener;
    cdp.on("Page.screencastFrame", listener);
    let timeoutHandle: ReturnType<typeof setTimeout> | undefined;
    try {
      await cdp.send("Page.startScreencast", {
        format: settings.format,
        quality: settings.format === "jpeg" ? settings.quality : undefined,
        // Keep the stream at the logical resolution. The UI fits this stable
        // stream into its panel without stopping the stream during resize.
        maxWidth: settings.maxWidth,
        maxHeight: settings.maxHeight,
        everyNthFrame: 1,
      });
      // 并发创建多个 Page 时，Chromium 可能先发出窗口默认尺寸的首帧，
      // 即使之前已经设置过 Emulation。重新应用一次只改变当前 Page 的
      // metrics，不导航、不重载页面，并由上面的首帧校验决定何时对外发布。
      await this.applyViewport(record, record.viewport, true);
      await Promise.race([
        firstFrame,
        new Promise<void>((_, reject) => {
          timeoutHandle = setTimeout(
            () => reject(new ProtocolFailure(
              "browser_screencast_start_timeout",
              "Chromium did not produce the initial browser frame in time",
              true,
              false,
              "请稍后重试浏览器面板。",
            )),
            SCREENCAST_INITIAL_FRAME_TIMEOUT_MILLIS,
          );
        }),
      ]);
    } catch (error) {
      record.screencastSettings = undefined;
      await this.stopScreencastSession(record);
      throw error;
    } finally {
      if (timeoutHandle) clearTimeout(timeoutHandle);
    }
  }

  private screencastBitmapMatchesViewport(
    record: PageRecord,
    settings: ScreencastSettings,
    bytes: Buffer,
  ): boolean {
    const actual = encodedImageDimensions(bytes, settings.format);
    if (!actual) return false;
    const physicalWidth = record.viewport.width * this.#config.deviceScaleFactor;
    const physicalHeight = record.viewport.height * this.#config.deviceScaleFactor;
    const scale = Math.min(
      1,
      settings.maxWidth / physicalWidth,
      settings.maxHeight / physicalHeight,
    );
    const expected = {
      width: Math.max(1, Math.round(physicalWidth * scale)),
      height: Math.max(1, Math.round(physicalHeight * scale)),
    };
    return actual.width === expected.width && actual.height === expected.height;
  }

  private async stopScreencast(tabId: string): Promise<ExecutedCommand> {
    const record = this.#pages.get(tabId);
    if (!record) return emptyResult();
    record.screencastSettings = undefined;
    await this.stopScreencastSession(record);
    return emptyResult();
  }

  private async stopScreencastSession(record: PageRecord): Promise<void> {
    if (!record.cdp) return;
    const cdp = record.cdp;
    if (record.screencastListener) {
      cdp.off("Page.screencastFrame", record.screencastListener);
    }
    record.screencastListener = undefined;
    record.screencastViewportRefreshScheduled = false;
    await this.flushScreencastAck(record);
    await cdp.send("Page.stopScreencast").catch(() => undefined);
  }

  private async flushScreencastAck(record: PageRecord): Promise<void> {
    await record.screencastAck.catch(() => undefined);
  }

  private async userInput(input: {
    tab_id: string;
    control: HostControl;
    event: UserInputEvent;
  }): Promise<ExecutedCommand> {
    this.control.validate(input.control);
    const record = this.pageRecord(input.tab_id);
    await this.ensurePage(record);
    const page = this.requirePage(record);
    const cdp = await this.cdpSession(record);
    this.hideAgentCursor(record);
    this.control.validate(input.control);
    const clipboard = await clipboardTextForShortcut(page, input.event);
    await dispatchUserInput(cdp, input.event);
    return clipboard
      ? { result: { type: "clipboard_text", payload: clipboard } }
      : emptyResult();
  }

  private async bindPageEvents(record: PageRecord): Promise<void> {
    const page = this.requirePage(record);
    const cdp = await this.cdpSession(record);
    if (!record.cdpPageDomainEnabled) {
      await cdp.send("Page.enable");
      record.cdpPageDomainEnabled = true;
    }
    const frameTree = await cdp.send("Page.getFrameTree") as {
      frameTree?: { frame?: { id?: string } };
    };
    record.cdpMainFrameId = frameTree.frameTree?.frame?.id;
    record.cdpFrameStartedNavigatingListener = (event) => {
      if (record.cdpMainFrameId && event.frameId !== record.cdpMainFrameId) return;
      this.beginNavigationPresentation(record, page);
    };
    record.cdpFrameStoppedLoadingListener = (event) => {
      if (record.cdpMainFrameId && event.frameId !== record.cdpMainFrameId) return;
      void this.completeNavigationPresentation(record, page);
    };
    cdp.on("Page.frameStartedNavigating", record.cdpFrameStartedNavigatingListener);
    cdp.on("Page.frameStoppedLoading", record.cdpFrameStoppedLoadingListener);
    page.on("domcontentloaded", () => {
      void this.completeNavigationPresentation(record, page);
    });
    page.on("load", () => {
      void this.completeNavigationPresentation(record, page);
    });
    page.on("console", (message: ConsoleMessage) => {
      const location = message.location();
      boundedPush(record.consoleMessages, {
        id: record.nextConsoleMessageId++,
        level: message.type(),
        text: message.text(),
        url: location.url || page.url(),
        line: location.lineNumber ?? 0,
        column: location.columnNumber ?? 0,
        timestamp: Date.now(),
      }, MAX_CONSOLE_MESSAGES);
      this.#transport.emit({
        type: "console",
        payload: {
          tab_id: record.tabId,
          level: message.type(),
          text: message.text(),
        },
      });
    });
    page.on("dialog", (dialog: Dialog) => {
      const entry: BrowserDialogEntry = {
        id: record.nextDialogId++,
        dialogType: dialog.type(),
        message: dialog.message(),
        defaultValue: dialog.defaultValue(),
        openedAt: Date.now(),
      };
      boundedPush(record.dialogs, entry, 100);
      this.#transport.emit({
        type: "dialog",
        payload: {
          tab_id: record.tabId,
          dialog_id: entry.id,
          dialog_type: dialog.type(),
          message: dialog.message(),
        },
      });
      const timeout = setTimeout(() => {
        if (record.pendingDialog?.entry !== entry) return;
        record.pendingDialog = undefined;
        void dialog.dismiss()
          .then(() => { entry.handledAs = "dismissed"; })
          .catch(() => undefined);
      }, 30_000);
      record.pendingDialog = { dialog, entry, timeout };
    });
    page.on("request", (request: Request) => {
      const id = record.nextNetworkRequestId++;
      record.networkRequestIds.set(request, id);
      boundedPush(record.networkRequests, {
        id,
        method: request.method(),
        url: request.url(),
        resourceType: request.resourceType(),
        requestHeaders: request.headers(),
        startedAt: Date.now(),
      }, MAX_NETWORK_REQUESTS);
    });
    page.on("response", (response: Response) => {
      const entry = networkEntry(record, response.request());
      if (!entry) return;
      entry.status = response.status();
      entry.statusText = response.statusText();
      entry.responseHeaders = response.headers();
      entry.mimeType = response.headers()["content-type"] ?? "";
      entry.response = response;
    });
    page.on("requestfinished", (request: Request) => {
      const entry = networkEntry(record, request);
      if (entry) entry.finishedAt = Date.now();
    });
    page.on("requestfailed", (request: Request) => {
      const entry = networkEntry(record, request);
      if (!entry) return;
      entry.finishedAt = Date.now();
      entry.failure = request.failure()?.errorText ?? "request failed";
    });
    page.on("download", (download: Download) => {
      const suggestedFilename = basename(download.suggestedFilename()) || "download";
      this.#transport.emit({
        type: "download",
        payload: {
          tab_id: record.tabId,
          suggested_filename: suggestedFilename,
          state: "started",
        },
      });
      void (async () => {
        try {
          const sourcePath = await download.path();
          if (!sourcePath) throw new Error("download path is unavailable");
          const destinationPath = join(
            this.#config.downloadPath,
            `${Date.now()}-${randomUUID()}-${suggestedFilename}`,
          );
          await copyFile(sourcePath, destinationPath);
          await unlink(sourcePath).catch(() => undefined);
          const fileStat = await stat(destinationPath);
          this.#transport.emit({
            type: "download",
            payload: {
              tab_id: record.tabId,
              suggested_filename: suggestedFilename,
              state: "completed",
              byte_length: fileStat.size,
            },
          });
        } catch (error) {
          this.#transport.emit({
            type: "download",
            payload: {
              tab_id: record.tabId,
              suggested_filename: suggestedFilename,
              state: "failed",
              error: error instanceof Error ? error.message : String(error),
            },
          });
        }
      })();
    });
    page.on("filechooser", (filechooser) => {
      this.#transport.emit({
        type: "file_chooser",
        payload: { tab_id: record.tabId },
      });
      void filechooser.setFiles([]).catch(() => undefined);
    });
    page.on("popup", (popup) => {
      const handler = record.popupHandler;
      if (handler) {
        handler(popup);
        return;
      }
      void this.adoptPopupNavigation(record, page, popup).catch(() => {
        this.#transport.emit({
          type: "popup_blocked",
          payload: { tab_id: record.tabId },
        });
      });
    });
    page.on("crash", () => {
      if (!this.retirePageRecord(record, page)) return;
      this.#transport.emit({
        type: "page_crashed",
        payload: { tab_id: record.tabId },
      });
    });
    page.on("close", () => {
      if (!this.retirePageRecord(record, page)) return;
      this.#transport.emit({
        type: "page_crashed",
        payload: { tab_id: record.tabId },
      });
    });
    page.on("framenavigated", (frame) => {
      if (frame !== page.mainFrame()) return;
      if (record.restoringInitialPage) return;
      record.navigationRevision += 1;
      record.snapshot.invalidate();
      this.scheduleSettledPageState(record);
    });
  }

  private async adoptPopupNavigation(
    record: PageRecord,
    sourcePage: Page,
    popup: Page,
  ): Promise<void> {
    let targetUrl = popup.url();
    try {
      if (!targetUrl || targetUrl === "about:blank") {
        await popup.waitForURL(
          value => value.href !== "about:blank",
          { timeout: POPUP_URL_TIMEOUT_MILLIS, waitUntil: "commit" },
        );
        targetUrl = popup.url();
      }
      validateNavigationUrl(targetUrl);
      await popup.close().catch(() => undefined);
      if (record.page !== sourcePage) {
        throw new ProtocolFailure(
          "browser_page_not_active",
          `browser page is not active: ${record.tabId}`,
          true,
          false,
        );
      }
      try {
        await sourcePage.goto(targetUrl, {
          waitUntil: "domcontentloaded",
          timeout: PAGE_NAVIGATION_TIMEOUT_MILLIS,
        });
      } catch (error) {
        const cdp = await this.cdpSession(record).catch(() => undefined);
        await cdp?.send("Page.stopLoading").catch(() => undefined);
        throw playwrightFailure("browser_popup_navigation_failed", error, true);
      }
      this.throwBlockedNavigation(record);
      this.#transport.emit({
        type: "page_updated",
        payload: await this.pageState(record),
      });
    } catch (error) {
      await popup.close().catch(() => undefined);
      if (error instanceof ProtocolFailure) throw error;
      throw playwrightFailure("browser_popup_navigation_failed", error, true);
    }
  }

  private async withNavigationGuard<T>(
    record: PageRecord,
    action: () => Promise<T>,
    expectedNavigationUrl?: string | null,
  ): Promise<T> {
    // The security policy is installed once for the whole Context. Keeping a
    // per-command Context route creates cross-tab races because Playwright
    // routes are Context-scoped, not Page-scoped.
    const page = this.requirePage(record);
    const settle = await this.prepareActionSettlement(record, page);
    try {
      const result = await action();
      if (!record.pendingDialog) {
        await this.finishActionSettlement(page, settle, expectedNavigationUrl);
      }
      return result;
    } finally {
      await this.completeNavigationPresentation(record, page);
      settle.dispose();
    }
  }

  private beginNavigationPresentation(record: PageRecord, page: Page): void {
    if (
      record.page !== page
      || !record.screencastListener
      || !record.hasPresentedFrame
    ) {
      return;
    }
    record.presentationRevision += 1;
    record.presentationPhase = "navigation_pending";
    record.presentationSettlement = undefined;
  }

  private async completeNavigationPresentation(record: PageRecord, page: Page): Promise<void> {
    if (
      record.page !== page
      || record.presentationPhase !== "navigation_pending"
    ) {
      return;
    }
    const revision = record.presentationRevision;
    if (record.presentationSettlement?.revision === revision) {
      await record.presentationSettlement.promise;
      return;
    }
    const promise = (async () => {
      await this.waitForStableDom(page);
      await this.waitForStablePageMetadata(page);
      await page.evaluate(
        () => new Promise<void>((resolve) => {
          requestAnimationFrame(() => requestAnimationFrame(() => resolve()));
        }),
      );
      if (
        record.page !== page
        || record.presentationPhase !== "navigation_pending"
        || record.presentationRevision !== revision
      ) {
        return;
      }
      await this.emitCurrentViewportFrame(record);
      if (
        record.page === page
        && record.presentationRevision === revision
      ) {
        record.presentationPhase = "stable";
      }
    })().catch(() => undefined);
    record.presentationSettlement = { revision, promise };
    await promise;
    if (record.presentationSettlement?.revision === revision) {
      record.presentationSettlement = undefined;
    }
  }

  private async prepareActionSettlement(
    record: PageRecord,
    page: Page,
  ): Promise<ActionSettlement> {
    const initialUrl = page.url();
    const cdp = await this.cdpSession(record);
    if (!record.cdpPageDomainEnabled) {
      await cdp.send("Page.enable");
      record.cdpPageDomainEnabled = true;
    }
    if (!record.cdpMainFrameId) {
      const frameTree = await cdp.send("Page.getFrameTree") as {
        frameTree?: { frame?: { id?: string } };
      };
      record.cdpMainFrameId = frameTree.frameTree?.frame?.id;
    }
    const mainFrameId = record.cdpMainFrameId;
    let navigationDetected = false;
    let navigationWaiter: ((value: boolean) => void) | undefined;
    let urlPoll: ReturnType<typeof setInterval> | undefined;
    let navigationTimeout: ReturnType<typeof setTimeout> | undefined;
    let popupNavigation: Promise<void> | undefined;
    const trackedRequests = new Set<Request>();
    let networkWaiter: (() => void) | undefined;
    let networkStableTimer: ReturnType<typeof setTimeout> | undefined;
    let networkTimeout: ReturnType<typeof setTimeout> | undefined;
    const markNavigationDetected = () => {
      if (navigationDetected) return;
      navigationDetected = true;
      if (urlPoll) clearInterval(urlPoll);
      if (navigationTimeout) clearTimeout(navigationTimeout);
      navigationWaiter?.(true);
      navigationWaiter = undefined;
    };
    const handlePopup = (popup: Page) => {
      popupNavigation = this.adoptPopupNavigation(record, page, popup);
      markNavigationDetected();
    };
    const onFrameStartedNavigating = (event: FrameStartedNavigatingEvent) => {
      if (mainFrameId && event.frameId !== mainFrameId) return;
      markNavigationDetected();
    };
    const onFrameNavigated = (frame: Frame) => {
      if (frame.page() !== page || frame !== page.mainFrame()) return;
      markNavigationDetected();
    };
    const resolveNetworkWaiter = () => {
      if (networkStableTimer) clearTimeout(networkStableTimer);
      if (networkTimeout) clearTimeout(networkTimeout);
      networkStableTimer = undefined;
      networkTimeout = undefined;
      networkWaiter?.();
      networkWaiter = undefined;
    };
    const scheduleNetworkSettlement = () => {
      if (!networkWaiter || trackedRequests.size > 0) return;
      if (networkStableTimer) clearTimeout(networkStableTimer);
      networkStableTimer = setTimeout(
        resolveNetworkWaiter,
        ACTION_NETWORK_STABLE_FOR_MILLIS,
      );
    };
    const onRequest = (request: Request) => {
      if (!isActionSettlementRequest(request)) return;
      trackedRequests.add(request);
      if (networkStableTimer) clearTimeout(networkStableTimer);
      networkStableTimer = undefined;
    };
    const onRequestSettled = (request: Request) => {
      if (!trackedRequests.delete(request)) return;
      scheduleNetworkSettlement();
    };
    cdp.on("Page.frameStartedNavigating", onFrameStartedNavigating);
    page.on("framenavigated", onFrameNavigated);
    page.on("request", onRequest);
    page.on("requestfinished", onRequestSettled);
    page.on("requestfailed", onRequestSettled);
    record.popupHandler = handlePopup;
    return {
      waitForNavigationSignal: async () => {
        if (navigationDetected || page.url() !== initialUrl) return true;
        return new Promise<boolean>((resolve) => {
          navigationWaiter = resolve;
          urlPoll = setInterval(() => {
            if (page.url() !== initialUrl) markNavigationDetected();
          }, 20);
          navigationTimeout = setTimeout(() => {
            if (navigationDetected) return;
            navigationWaiter = undefined;
            resolve(false);
          }, ACTION_NAVIGATION_EXPECT_TIMEOUT_MILLIS);
        });
      },
      finishPopupNavigation: async () => {
        const pending = popupNavigation;
        if (!pending) return false;
        await pending;
        return true;
      },
      waitForNetworkSettlement: () => new Promise<void>((resolve) => {
        networkWaiter = resolve;
        networkTimeout = setTimeout(
          resolveNetworkWaiter,
          ACTION_NETWORK_SETTLEMENT_TIMEOUT_MILLIS,
        );
        scheduleNetworkSettlement();
      }),
      dispose: () => {
        cdp.off("Page.frameStartedNavigating", onFrameStartedNavigating);
        page.off("framenavigated", onFrameNavigated);
        page.off("request", onRequest);
        page.off("requestfinished", onRequestSettled);
        page.off("requestfailed", onRequestSettled);
        if (record.popupHandler === handlePopup) record.popupHandler = undefined;
        if (urlPoll) clearInterval(urlPoll);
        if (navigationTimeout) clearTimeout(navigationTimeout);
        resolveNetworkWaiter();
      },
    };
  }

  private async finishActionSettlement(
    page: Page,
    settlement: ActionSettlement,
    expectedNavigationUrl?: string | null,
  ): Promise<boolean> {
    let navigated = await settlement.waitForNavigationSignal();
    await settlement.waitForNetworkSettlement();
    navigated = (await settlement.finishPopupNavigation()) || navigated;
    if (expectedNavigationUrl && !navigationUrlMatches(page.url(), expectedNavigationUrl)) {
      const reachedExpectedUrl = await page.waitForURL(
        (url) => navigationUrlMatches(url.href, expectedNavigationUrl),
        {
          timeout: ACTION_EXPECTED_URL_TIMEOUT_MILLIS,
          waitUntil: "domcontentloaded",
        },
      ).then(() => true).catch(() => false);
      navigated = navigated || reachedExpectedUrl;
    }
    if (navigated) {
      await page.waitForLoadState("domcontentloaded", {
        timeout: PAGE_NAVIGATION_TIMEOUT_MILLIS,
      }).catch(() => undefined);
    }
    // 只等待本次动作开始后产生的 document/xhr/fetch/script/style 请求。
    // Playwright 的全页 networkidle 对已加载的 SPA 可能立即返回，无法覆盖
    // 路由提交后才加载的代码块和数据请求。
    // 从动作边界开始观察稳定窗口，同时覆盖同文档 SPA 渲染，以及原生导航
    // 提交后才发生的延迟 DOM 更新。
    await this.waitForStableDom(page);
    // URL 和标题属于页面元数据，不是 DOM 内容。客户端路由提交标题之前，
    // Chromium 可能已经暴露新 DOM，因此页面状态必须在同一稳定窗口内同时
    // 观察这两个值。
    await this.waitForStablePageMetadata(page);
    return navigated;
  }

  private async waitForStableDom(page: Page): Promise<boolean> {
    return page.evaluate(
      ({ stableFor, timeout }) => new Promise<void>((resolve) => {
        const root = document.documentElement;
        if (!root) {
          resolve();
          return;
        }
        let finished = false;
        let stableTimer: ReturnType<typeof setTimeout> | undefined;
        let timeoutTimer: ReturnType<typeof setTimeout>;
        let observer: MutationObserver;
        const finish = () => {
          if (finished) return;
          finished = true;
          if (stableTimer) clearTimeout(stableTimer);
          clearTimeout(timeoutTimer);
          observer.disconnect();
          resolve();
        };
        const schedule = () => {
          if (stableTimer) clearTimeout(stableTimer);
          stableTimer = setTimeout(finish, stableFor);
        };
        observer = new MutationObserver(schedule);
        timeoutTimer = setTimeout(finish, timeout);
        observer.observe(root, {
          childList: true,
          subtree: true,
          attributes: true,
          characterData: true,
        });
        schedule();
      }),
      {
        stableFor: ACTION_DOM_STABLE_FOR_MILLIS,
        timeout: ACTION_DOM_STABILITY_TIMEOUT_MILLIS,
      },
    ).then(() => true).catch(() => false);
  }

  private async waitForStablePageMetadata(page: Page): Promise<boolean> {
    return page.evaluate(
      ({ stableFor, timeout, pollEvery }) => new Promise<void>((resolve) => {
        const startedAt = performance.now();
        let lastValue = `${globalThis.location.href}\u0000${document.title}`;
        let stableSince = startedAt;
        const check = () => {
          const now = performance.now();
          const value = `${globalThis.location.href}\u0000${document.title}`;
          if (value !== lastValue) {
            lastValue = value;
            stableSince = now;
          }
          if (
            now - stableSince >= stableFor
            || now - startedAt >= timeout
          ) {
            resolve();
            return;
          }
          globalThis.setTimeout(check, pollEvery);
        };
        check();
      }),
      {
        stableFor: ACTION_PAGE_METADATA_STABLE_FOR_MILLIS,
        timeout: ACTION_PAGE_METADATA_TIMEOUT_MILLIS,
        pollEvery: ACTION_PAGE_METADATA_POLL_MILLIS,
      },
    ).then(() => true).catch(() => false);
  }

  private scheduleSettledPageState(record: PageRecord): void {
    if (record.pageStateRefreshScheduled || !record.page) return;
    record.pageStateRefreshScheduled = true;
    const page = record.page;
    void this.waitForStableDom(page)
      .then(() => this.waitForStablePageMetadata(page))
      .then(() => this.pageState(record))
      .then((state) => {
        if (record.page === page) {
          this.#transport.emit({ type: "page_updated", payload: state });
        }
      })
      .catch(() => undefined)
      .finally(() => {
        record.pageStateRefreshScheduled = false;
      });
  }

  private async installNavigationGuard(): Promise<void> {
    await this.context().route("**/*", async (route) => {
      const request = route.request();
      let frame: ReturnType<typeof request.frame>;
      try {
        // Chromium may emit the initial navigation before its main frame exists.
        // Request.frame() is intentionally unavailable in that window.
        frame = request.frame();
      } catch {
        if (isBlockedNavigationUrl(request.url())) {
          await route.abort("blockedbyclient");
        } else {
          await route.continue();
        }
        return;
      }
      const page = frame.page();
      const record = [...this.#pages.values()].find((candidate) => candidate.page === page);
      if (
        !record
        || !request.isNavigationRequest()
        || frame !== page.mainFrame()
      ) {
        await route.continue();
        return;
      }
      if (isBlockedNavigationUrl(request.url())) {
        record.blockedNavigationUrl = request.url();
        await route.abort("blockedbyclient");
        return;
      }
      await route.continue();
    });
  }

  private throwBlockedNavigation(record: PageRecord): void {
    const blockedUrl = record.blockedNavigationUrl;
    record.blockedNavigationUrl = undefined;
    if (!blockedUrl) return;
    throw new ProtocolFailure(
      "browser_navigation_target_blocked",
      `navigation to a blocked metadata endpoint was rejected: ${blockedUrl}`,
      true,
      true,
      blockedUrl,
    );
  }

  private async pageState(record: PageRecord): Promise<PageState> {
    const page = record.page;
    if (!page || record.pendingDialog) {
      return {
        tab_id: record.tabId,
        url: record.currentUrl,
        origin: record.currentOrigin,
        title: record.currentTitle,
        navigation_revision: record.navigationRevision,
      };
    }
    const url = page.url();
    let origin: string | null = null;
    try {
      const parsed = new URL(url);
      origin = parsed.origin === "null" ? null : parsed.origin;
    } catch {
      origin = null;
    }
    const state = {
      tab_id: record.tabId,
      url,
      origin,
      title: await page.title().catch(() => ""),
      navigation_revision: record.navigationRevision,
    };
    record.currentUrl = state.url;
    record.currentOrigin = state.origin ?? null;
    record.currentTitle = state.title;
    return state;
  }

  private context(): BrowserContext {
    if (!this.#context) {
      throw new ProtocolFailure(
        "browser_host_not_ready",
        "browser Host is not ready",
        true,
        false,
      );
    }
    return this.#context;
  }

  private pageRecord(tabId: string): PageRecord {
    const record = this.#pages.get(tabId);
    if (!record) {
      throw new ProtocolFailure(
        "browser_tab_unknown",
        `browser tab does not exist: ${tabId}`,
        true,
        false,
      );
    }
    return record;
  }

  private retirePageRecord(record: PageRecord, page: Page): boolean {
    if (this.#pages.get(record.tabId) !== record || record.page !== page) return false;
    this.disposePageRecord(record);
    this.#pages.delete(record.tabId);
    if (this.#foregroundTabId === record.tabId) this.#foregroundTabId = undefined;
    record.page = undefined;
    record.cdp = undefined;
    record.cdpPageDomainEnabled = false;
    record.cdpMainFrameId = undefined;
    record.screencastListener = undefined;
    record.screencastSettings = undefined;
    return true;
  }

  private disposePageRecord(record: PageRecord): void {
    if (record.cdp && record.cdpFrameStartedNavigatingListener) {
      record.cdp.off("Page.frameStartedNavigating", record.cdpFrameStartedNavigatingListener);
      record.cdpFrameStartedNavigatingListener = undefined;
    }
    if (record.cdp && record.cdpFrameStoppedLoadingListener) {
      record.cdp.off("Page.frameStoppedLoading", record.cdpFrameStoppedLoadingListener);
      record.cdpFrameStoppedLoadingListener = undefined;
    }
    record.presentationPhase = "stable";
    record.presentationSettlement = undefined;
    record.hasPresentedFrame = false;
    if (record.pendingDialog) {
      clearTimeout(record.pendingDialog.timeout);
      record.pendingDialog = undefined;
    }
    if (record.trace && record.cdp) {
      record.cdp.off("Tracing.dataCollected", record.trace.listener);
      void record.cdp.send("Tracing.end").catch(() => undefined);
      record.trace = undefined;
    }
    if (record.recording) {
      void record.recording.recorder.stop().catch(() => undefined);
      record.recording = undefined;
    }
  }
}

async function readDevtoolsBrowserWSEndpoint(profilePath: string): Promise<string> {
  const activePortPath = join(profilePath, "DevToolsActivePort");
  const deadline = Date.now() + 5_000;
  while (Date.now() < deadline) {
    try {
      const [port, websocketPath] = (await readFile(activePortPath, "utf8")).trim().split(/\r?\n/);
      if (port && websocketPath) return `ws://127.0.0.1:${port}${websocketPath}`;
    } catch {
      await new Promise(resolve => setTimeout(resolve, 25));
    }
  }
  throw new ProtocolFailure(
    "browser_devtools_endpoint_unavailable",
    "Chromium 未在启动期限内提供 DevToolsActivePort",
    true,
    false,
  );
}

function emptyResult(): ExecutedCommand {
  return { result: { type: "empty" } };
}

function jsonResult(value: unknown): ExecutedCommand {
  return { result: { type: "json", payload: { value: boundJsonValue(value) } } };
}

function boundedPush<T>(items: T[], value: T, max: number): void {
  items.push(value);
  if (items.length > max) items.splice(0, items.length - max);
}

function networkEntry(record: PageRecord, request: Request): BrowserNetworkEntry | undefined {
  const id = record.networkRequestIds.get(request);
  return id === undefined ? undefined : record.networkRequests.find(entry => entry.id === id);
}

function publicNetworkEntry(entry: BrowserNetworkEntry): Omit<BrowserNetworkEntry, "response"> {
  const { response: _response, ...publicEntry } = entry;
  return publicEntry;
}

function invalidDevtoolsArguments(message: string): ProtocolFailure {
  return new ProtocolFailure("browser_devtools_invalid_arguments", message, false, false);
}

function devtoolsOperationRequiresControl(
  operation: string,
  args: Record<string, unknown>,
): boolean {
  const action = optionalDevtoolsString(args, "action");
  switch (operation) {
    case "hover":
    case "drag":
    case "fill_form":
    case "upload_file":
    case "click_at":
    case "evaluate":
    case "emulate":
      return true;
    case "dialog":
      return action !== "list";
    case "console":
    case "network":
      return action === "clear";
    case "performance":
      return action === "start" || action === "stop";
    case "lighthouse":
      return (optionalDevtoolsString(args, "mode") ?? "navigation") === "navigation";
    case "heap":
      return action === "take_snapshot";
    case "extensions":
      return action !== "list";
    case "third_party":
    case "webmcp":
      return action === "execute";
    case "pwa":
      return action !== "state";
    case "recording":
      return true;
    default:
      return false;
  }
}

function requiredDevtoolsString(args: Record<string, unknown>, name: string): string {
  const value = optionalDevtoolsString(args, name);
  if (!value) throw invalidDevtoolsArguments(`missing ${name}`);
  return value;
}

function optionalDevtoolsString(args: Record<string, unknown>, name: string): string | undefined {
  return typeof args[name] === "string" && args[name].trim() ? args[name].trim() : undefined;
}

function optionalDevtoolsNumber(args: Record<string, unknown>, name: string): number | undefined {
  return typeof args[name] === "number" && Number.isFinite(args[name]) ? args[name] : undefined;
}

function requiredFiniteDevtoolsNumber(args: Record<string, unknown>, name: string): number {
  const value = optionalDevtoolsNumber(args, name);
  if (value === undefined) throw invalidDevtoolsArguments(`${name} must be a finite number`);
  return value;
}

function requiredDevtoolsInteger(args: Record<string, unknown>, name: string): number {
  const value = requiredFiniteDevtoolsNumber(args, name);
  if (!Number.isSafeInteger(value) || value < 1) throw invalidDevtoolsArguments(`${name} must be a positive integer`);
  return value;
}

function optionalDevtoolsInteger(args: Record<string, unknown>, name: string): number | undefined {
  const value = optionalDevtoolsNumber(args, name);
  if (value === undefined) return undefined;
  if (!Number.isSafeInteger(value) || value < 1) throw invalidDevtoolsArguments(`${name} must be a positive integer`);
  return value;
}

function boundedInteger(value: unknown, fallback: number, min: number, max: number): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value)) return fallback;
  return Math.min(max, Math.max(min, value));
}

function boundedTimeout(value: unknown, fallback: number, max: number): number {
  return boundedInteger(value, fallback, 1, max);
}

function devtoolsStringArray(value: unknown): string[] {
  if (typeof value === "string") return value.trim() ? [value.trim()] : [];
  if (!Array.isArray(value)) return [];
  return value.filter((entry): entry is string => typeof entry === "string" && Boolean(entry.trim())).map(entry => entry.trim());
}

function devtoolsObject(args: Record<string, unknown>, name: string): Record<string, unknown> {
  const value = args[name];
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw invalidDevtoolsArguments(`${name} must be an object`);
  }
  return value as Record<string, unknown>;
}

function devtoolsObjectArray(args: Record<string, unknown>, name: string): Array<Record<string, unknown>> {
  const value = args[name];
  if (!Array.isArray(value)) throw invalidDevtoolsArguments(`${name} must be an array`);
  return value.map((entry, index) => {
    if (!entry || typeof entry !== "object" || Array.isArray(entry)) {
      throw invalidDevtoolsArguments(`${name}[${index}] must be an object`);
    }
    return entry as Record<string, unknown>;
  });
}

function devtoolsTarget(args: Record<string, unknown>): { snapshot_revision: number; element_ref: string } {
  const revision = requiredDevtoolsInteger(args, "snapshot_revision");
  const elementRef = requiredDevtoolsString(args, "element_ref");
  return { snapshot_revision: revision, element_ref: elementRef };
}

function boundJsonValue(value: unknown): unknown {
  try {
    const encoded = JSON.stringify(value);
    if (encoded === undefined) return null;
    if (Buffer.byteLength(encoded, "utf8") <= 128 * 1024) return JSON.parse(encoded);
    return { truncated: true, preview: encoded.slice(0, 128 * 1024) };
  } catch {
    return { unserializable: true, value: String(value) };
  }
}

function networkCondition(name: string): {
  offline: boolean;
  latency: number;
  downloadThroughput: number;
  uploadThroughput: number;
} {
  if (name.toLowerCase() === "offline") {
    return { offline: true, latency: 0, downloadThroughput: 0, uploadThroughput: 0 };
  }
  const presets: Record<string, { latency: number; downloadThroughput: number; uploadThroughput: number }> = {
    "slow 3g": { latency: 400, downloadThroughput: 50 * 1024, uploadThroughput: 50 * 1024 },
    "fast 3g": { latency: 100, downloadThroughput: 750 * 1024, uploadThroughput: 750 * 1024 },
    "slow 4g": { latency: 100, downloadThroughput: 1_500 * 1024, uploadThroughput: 750 * 1024 },
    "fast 4g": { latency: 20, downloadThroughput: 9_000 * 1024, uploadThroughput: 1_500 * 1024 },
  };
  const preset = presets[name.toLowerCase()];
  if (!preset) throw invalidDevtoolsArguments(`unknown network condition: ${name}`);
  return { offline: false, ...preset };
}

function summarizeTrace(events: unknown[], startedAt: number): Record<string, unknown> {
  const records = events.filter(value => value && typeof value === "object") as Array<Record<string, unknown>>;
  const longTasks = records.filter(event => event.name === "RunTask" && typeof event.dur === "number" && event.dur >= 50);
  const paints = records.filter(event => typeof event.name === "string" && /Paint|LargestContentfulPaint|firstContentfulPaint/i.test(event.name));
  const layoutShifts = records.filter(event => event.name === "LayoutShift" && (event.args as { data?: { had_recent_input?: boolean } } | undefined)?.data?.had_recent_input !== true);
  const requests = records.filter(event => event.name === "ResourceSendRequest");
  const responses = records.filter(event => event.name === "ResourceReceiveResponse");
  const lcp = [...records].reverse().find(event => typeof event.name === "string" && /LargestContentfulPaint/i.test(event.name));
  const navigation = records.find(event => event.name === "navigationStart");
  const insightSetId = `navigation-${Number(navigation?.ts ?? startedAt)}`;
  const longTaskDuration = longTasks.reduce((total, event) => total + Number(event.dur ?? 0) / 1000, 0);
  const insights = [
    {
      name: "DocumentLatency",
      request_count: requests.length,
      response_count: responses.length,
      navigation_timestamp: navigation?.ts ?? null,
    },
    {
      name: "LCPBreakdown",
      lcp_timestamp: lcp?.ts ?? null,
      paint_event_count: paints.length,
    },
    {
      name: "LongTasks",
      long_task_count: longTasks.length,
      total_blocking_duration_ms: longTaskDuration,
      max_long_task_ms: longTasks.reduce((max, event) => Math.max(max, Number(event.dur ?? 0) / 1000), 0),
    },
    {
      name: "CLSCulprits",
      layout_shift_count: layoutShifts.length,
      score_sum: layoutShifts.reduce((total, event) => total + Number((event.args as { data?: { weighted_score_delta?: number } } | undefined)?.data?.weighted_score_delta ?? 0), 0),
    },
  ];
  return {
    started_at: startedAt,
    stopped_at: Date.now(),
    event_count: records.length,
    long_task_count: longTasks.length,
    max_long_task_ms: longTasks.reduce((max, event) => Math.max(max, Number(event.dur ?? 0) / 1000), 0),
    paint_event_count: paints.length,
    insight_sets: [{ id: insightSetId, insights }],
  };
}

function commandTabId(command: HostCommand): string | null {
  const payload = (command as { payload?: unknown }).payload;
  if (!payload || typeof payload !== "object") return null;
  const tabId = (payload as { tab_id?: unknown }).tab_id;
  return typeof tabId === "string" ? tabId : null;
}

function binaryPayload(bytes: Buffer, mimeType: string): BinaryPayload {
  return {
    payload_id: randomUUID(),
    mime_type: mimeType,
    byte_length: bytes.length,
    sha256: createHash("sha256").update(bytes).digest("hex"),
  };
}

function validateViewport(viewport: {
  width: number;
  height: number;
  surface_width: number;
  surface_height: number;
  device_scale_factor_millis: number;
  device_type: HostDeviceType;
}): void {
  requireSafeInteger("viewport.width", viewport.width);
  requireSafeInteger("viewport.height", viewport.height);
  requireSafeInteger("viewport.surface_width", viewport.surface_width);
  requireSafeInteger("viewport.surface_height", viewport.surface_height);
  requireSafeInteger(
    "viewport.device_scale_factor_millis",
    viewport.device_scale_factor_millis,
  );
  if (
    viewport.width < 320 ||
    viewport.width > 7_680 ||
    viewport.height < 240 ||
    viewport.height > 4_320 ||
    viewport.surface_width < 1 ||
    viewport.surface_width > 7_680 ||
    viewport.surface_height < 1 ||
    viewport.surface_height > 4_320 ||
    viewport.device_scale_factor_millis < 500 ||
    viewport.device_scale_factor_millis > 4_000
  ) {
    throw new ProtocolFailure(
      "browser_viewport_invalid",
      "viewport is outside the supported range",
      false,
      false,
    );
  }
  if (!(["desktop", "mobile"] as const).includes(viewport.device_type)) {
    throw new ProtocolFailure(
      "browser_device_type_invalid",
      "viewport.device_type is invalid",
      false,
      false,
    );
  }
}

function validateLogicalViewport(viewport: HostLogicalViewport): void {
  requireSafeInteger("viewport.width", viewport.width);
  requireSafeInteger("viewport.height", viewport.height);
  requireSafeInteger(
    "viewport.device_scale_factor_millis",
    viewport.device_scale_factor_millis,
  );
  if (
    viewport.width < 320
    || viewport.width > 7_680
    || viewport.height < 240
    || viewport.height > 4_320
    || viewport.device_scale_factor_millis < 500
    || viewport.device_scale_factor_millis > 4_000
  ) {
    throw new ProtocolFailure(
      "browser_viewport_invalid",
      "logical viewport is outside the supported range",
      false,
      false,
    );
  }
  if (!( ["desktop", "mobile"] as const).includes(viewport.device_type)) {
    throw new ProtocolFailure(
      "browser_device_type_invalid",
      "viewport.device_type is invalid",
      false,
      false,
    );
  }
}

function viewportSurfaceScale(viewport: HostViewport): number {
  return Math.min(
    1,
    viewport.surface_width / viewport.width,
    viewport.surface_height / viewport.height,
  );
}

function encodedImageDimensions(
  bytes: Buffer,
  format: "jpeg" | "png",
): { width: number; height: number } | undefined {
  if (format === "png") {
    if (
      bytes.length < 24
      || !bytes.subarray(0, 8).equals(Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]))
    ) {
      return undefined;
    }
    return {
      width: bytes.readUInt32BE(16),
      height: bytes.readUInt32BE(20),
    };
  }
  if (bytes.length < 4 || bytes[0] !== 0xff || bytes[1] !== 0xd8) {
    return undefined;
  }
  const startOfFrameMarkers = new Set([
    0xc0, 0xc1, 0xc2, 0xc3, 0xc5, 0xc6, 0xc7,
    0xc9, 0xca, 0xcb, 0xcd, 0xce, 0xcf,
  ]);
  let offset = 2;
  while (offset + 3 < bytes.length) {
    if (bytes[offset] !== 0xff) {
      offset += 1;
      continue;
    }
    const marker = bytes[offset + 1];
    offset += 2;
    if (
      marker === 0xd8
      || marker === 0xd9
      || marker === 0x01
      || (marker >= 0xd0 && marker <= 0xd7)
    ) {
      continue;
    }
    if (offset + 2 > bytes.length) return undefined;
    const segmentLength = bytes.readUInt16BE(offset);
    if (segmentLength < 2 || offset + segmentLength > bytes.length) return undefined;
    if (startOfFrameMarkers.has(marker)) {
      if (segmentLength < 7) return undefined;
      return {
        width: bytes.readUInt16BE(offset + 5),
        height: bytes.readUInt16BE(offset + 3),
      };
    }
    offset += segmentLength;
  }
  return undefined;
}

function validateNormalizedRect(rect: NormalizedRect): void {
  for (const [name, value] of Object.entries(rect)) {
    requireFinite(`screenshot.clip.${name}`, value);
  }
  if (
    rect.x < 0
    || rect.y < 0
    || rect.width <= 0
    || rect.height <= 0
    || rect.x + rect.width > 1
    || rect.y + rect.height > 1
  ) {
    throw new ProtocolFailure(
      "browser_screenshot_clip_invalid",
      "screenshot clip must be a non-empty normalized viewport rectangle",
      false,
      false,
    );
  }
}

export function validateNavigationUrl(value: string): void {
  if (value === "about:blank") return;
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    throw new ProtocolFailure(
      "browser_url_invalid",
      "navigation URL is invalid",
      false,
      false,
    );
  }
  if (!["http:", "https:"].includes(url.protocol)) {
    throw new ProtocolFailure(
      "browser_url_scheme_blocked",
      `navigation URL scheme is not supported: ${url.protocol}`,
      false,
      false,
    );
  }
  if (!url.hostname || url.username || url.password) {
    throw new ProtocolFailure(
      "browser_url_invalid",
      "navigation URL must contain a host and must not contain credentials",
      false,
      false,
    );
  }
  if (isBlockedNavigationUrl(url.toString())) {
    throw new ProtocolFailure(
      "browser_navigation_target_blocked",
      "navigation to a cloud metadata endpoint is blocked",
      false,
      false,
    );
  }
}

function navigationUrlMatches(currentValue: string, expectedValue: string): boolean {
  try {
    const current = new URL(currentValue);
    const expected = new URL(expectedValue);
    return current.origin === expected.origin
      && current.pathname.replace(/\/$/, "") === expected.pathname.replace(/\/$/, "")
      && current.search === expected.search;
  } catch {
    return currentValue === expectedValue;
  }
}

function isActionSettlementRequest(request: Request): boolean {
  return ["document", "xhr", "fetch", "script", "stylesheet"].includes(
    request.resourceType(),
  );
}

function isBlockedNavigationUrl(value: string): boolean {
  try {
    const hostname = new URL(value).hostname.replace(/\.$/, "").toLowerCase();
    return hostname === "metadata.google.internal" || hostname.startsWith("169.254.");
  } catch {
    return false;
  }
}

function requireFinite(name: string, value: number): void {
  if (!Number.isFinite(value)) {
    throw new ProtocolFailure(
      "browser_protocol_invalid",
      `${name} must be finite`,
      false,
      false,
    );
  }
}

function playwrightFailure(
  code: string,
  error: unknown,
  sideEffectStarted: boolean,
): ProtocolFailure {
  if (error instanceof ProtocolFailure) return error;
  return new ProtocolFailure(
    code,
    error instanceof Error ? error.message : String(error),
    true,
    sideEffectStarted,
  );
}

async function dispatchUserInput(
  cdp: CDPSession,
  event: UserInputEvent,
): Promise<void> {
  switch (event.type) {
    case "mouse_move":
      await cdp.send("Input.dispatchMouseEvent", {
        type: "mouseMoved",
        x: event.x,
        y: event.y,
        button: "none",
      });
      return;
    case "mouse_down":
      await cdp.send("Input.dispatchMouseEvent", {
        type: "mousePressed",
        x: event.x,
        y: event.y,
        button: cdpMouseButton(event.button),
        clickCount: event.click_count,
      });
      return;
    case "mouse_up":
      await cdp.send("Input.dispatchMouseEvent", {
        type: "mouseReleased",
        x: event.x,
        y: event.y,
        button: cdpMouseButton(event.button),
        clickCount: event.click_count,
      });
      return;
    case "mouse_wheel":
      await cdp.send("Input.dispatchMouseEvent", {
        type: "mouseWheel",
        x: event.x,
        y: event.y,
        deltaX: event.delta_x,
        deltaY: event.delta_y,
      });
      return;
    case "key_down":
      await cdp.send("Input.dispatchKeyEvent", {
        type: "keyDown",
        key: event.key,
        code: event.code,
        windowsVirtualKeyCode: event.key_code,
        nativeVirtualKeyCode: event.key_code,
        modifiers: event.modifiers,
        commands: editingCommands(event),
      });
      return;
    case "key_up":
      await cdp.send("Input.dispatchKeyEvent", {
        type: "keyUp",
        key: event.key,
        code: event.code,
        windowsVirtualKeyCode: event.key_code,
        nativeVirtualKeyCode: event.key_code,
        modifiers: event.modifiers,
      });
      return;
    case "insert_text":
      await cdp.send("Input.insertText", { text: event.text });
      return;
  }
}

function editingCommands(
  event: Extract<UserInputEvent, { type: "key_down" }>,
): string[] {
  const primaryModifier = (event.modifiers & (2 | 4)) !== 0;
  if (!primaryModifier) return [];
  switch (event.key.toLowerCase()) {
    case "a":
      return ["selectAll"];
    case "c":
      return ["copy"];
    case "x":
      return ["cut"];
    case "y":
      return ["redo"];
    case "z":
      return [(event.modifiers & 8) !== 0 ? "redo" : "undo"];
    default:
      return [];
  }
}

async function clipboardTextForShortcut(
  page: Page,
  event: UserInputEvent,
): Promise<ClipboardText | null> {
  if (event.type !== "key_down" || (event.modifiers & (2 | 4)) === 0) {
    return null;
  }
  const key = event.key.toLowerCase();
  if (key !== "c" && key !== "x") return null;
  const text = await page.evaluate(() => {
    const active = document.activeElement;
    if (active instanceof HTMLInputElement || active instanceof HTMLTextAreaElement) {
      const start = active.selectionStart;
      const end = active.selectionEnd;
      if (start !== null && end !== null && end > start) {
        return active.value.slice(start, end);
      }
    }
    return window.getSelection()?.toString() ?? "";
  });
  if (!text) return null;
  return { operation: key === "c" ? "copy" : "cut", text };
}

function cdpMouseButton(
  button: MouseButton,
): "none" | "left" | "middle" | "right" | "back" | "forward" {
  return button;
}
