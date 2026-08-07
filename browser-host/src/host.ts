import { createHash, randomUUID } from "node:crypto";
import { copyFile, mkdir, stat, unlink } from "node:fs/promises";
import { basename, join } from "node:path";
import type {
  BrowserContext,
  CDPSession,
  ConsoleMessage,
  Dialog,
  Download,
  Page,
} from "playwright-core";
import { chromium } from "playwright-core";
import { ControlFence, ProtocolFailure, requireSafeInteger } from "./control";
import type {
  BinaryPayload,
  ClipboardText,
  CommandResult,
  HostCommand,
  HostControl,
  HostDeviceType,
  HostLogicalViewport,
  HostEvent,
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
const SCREENCAST_INITIAL_FRAME_TIMEOUT_MILLIS = 5_000;
const MAX_SCREENCAST_WIDTH = 7_680;
const MAX_SCREENCAST_HEIGHT = 4_320;

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
  emitBinary(payload: Buffer): void;
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
  screencastListener?: (event: ScreencastFrameEvent) => void;
  screencastSettings?: ScreencastSettings;
  screencastAck: Promise<void>;
}

interface ScreencastSettings {
  format: "jpeg" | "png";
  quality: number;
  maxWidth: number;
  maxHeight: number;
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
  #context?: BrowserContext;
  #chromiumVersion = "unknown";
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
        acceptDownloads: true,
        downloadsPath: this.#config.downloadPath,
        permissions: [],
        serviceWorkers: "block",
        viewport: null,
        args: [
          "--disable-background-networking",
          "--disable-component-update",
          "--disable-default-apps",
          "--disable-sync",
          "--no-first-run",
          "--no-default-browser-check",
          `--force-device-scale-factor=${this.#config.deviceScaleFactor}`,
        ],
      },
    );
    this.#context.setDefaultTimeout(5_000);
    this.#context.setDefaultNavigationTimeout(PAGE_NAVIGATION_TIMEOUT_MILLIS);
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
        return emptyResult();
      case "create_page":
        return this.createPage(command.payload);
      case "set_viewport":
        return this.setViewport(command.payload);
      case "set_logical_viewport":
        return this.setLogicalViewport(command.payload);
      case "close_page":
        return this.closePage(command.payload.tab_id);
      case "activate_page":
        return this.activatePage(command.payload.tab_id);
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
    this.#pages.clear();
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
    if (this.#pages.size >= this.#config.maxTabs) {
      throw new ProtocolFailure(
        "browser_tab_limit_reached",
        `browser tab limit reached: ${this.#config.maxTabs}`,
        true,
        false,
        "请关闭不再使用的浏览器面板后重试。",
      );
    }
    const record: PageRecord = {
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
      screencastAck: Promise.resolve(),
    };
    this.#pages.set(input.tab_id, record);
    record.inFlightCommands = 1;
    try {
      await this.ensurePage(record);
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

  private async closePage(tabId: string): Promise<ExecutedCommand> {
    const record = this.pageRecord(tabId);
    await this.stopScreencast(tabId);
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

  private async applyViewport(record: PageRecord, viewport: HostViewport): Promise<boolean> {
    const logicalViewportChanged = !record.cdp
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

  private async ensurePage(record: PageRecord): Promise<void> {
    if (record.page) {
      await this.flushScreencastAck(record);
      this.touch(record);
      return;
    }
    await this.ensureActiveCapacity();
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
    } catch (error) {
      record.page = undefined;
      record.cdp = undefined;
      await page.close().catch(() => undefined);
      throw error;
    }
  }

  private async ensureActiveCapacity(): Promise<void> {
    const active = [...this.#pages.values()].filter((record) => record.page).length;
    if (active < this.#config.maxActivePages) return;
    throw new ProtocolFailure(
      "browser_active_page_limit_reached",
      `active browser page limit reached: ${this.#config.maxActivePages}`,
      true,
      false,
      "请先关闭其他浏览器面板后重试。页面不会被自动关闭或重建。",
    );
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

  private async cdpSession(record: PageRecord): Promise<CDPSession> {
    record.cdp ??= await this.context().newCDPSession(this.requirePage(record));
    return record.cdp;
  }

  private async activatePage(tabId: string): Promise<ExecutedCommand> {
    const record = this.pageRecord(tabId);
    await this.ensurePage(record);
    await this.requirePage(record).bringToFront();
    this.#foregroundTabId = tabId;
    this.touch(record);
    return {
      result: { type: "page_state", payload: await this.pageState(record) },
    };
  }

  private async navigate(input: {
    tab_id: string;
    control: HostControl;
    navigation:
      | { action: "url"; url: string }
      | { action: "back" }
      | { action: "forward" }
      | { action: "reload" };
  }): Promise<ExecutedCommand> {
    this.control.validate(input.control);
    const record = this.pageRecord(input.tab_id);
    await this.ensurePage(record);
    const page = this.requirePage(record);
    return this.withNavigationGuard(record, async () => {
      this.control.validate(input.control);
      try {
        switch (input.navigation.action) {
          case "url":
            validateNavigationUrl(input.navigation.url);
            await page.goto(input.navigation.url, {
              waitUntil: "domcontentloaded",
            });
            break;
          case "back":
            await page.goBack({ waitUntil: "domcontentloaded" });
            break;
          case "forward":
            await page.goForward({ waitUntil: "domcontentloaded" });
            break;
          case "reload":
            await page.reload({ waitUntil: "domcontentloaded" });
            break;
        }
      } catch (error) {
        this.throwBlockedNavigation(record);
        throw playwrightFailure("browser_navigation_failed", error, true);
      }
      this.throwBlockedNavigation(record);
      const state = await this.pageState(record);
      this.#transport.emit({ type: "page_updated", payload: state });
      return { result: { type: "page_state", payload: state } };
    });
  }

  private async snapshot(input: {
    tab_id: string;
    limits: { max_nodes: number; max_text_bytes: number };
    subtree_ref?: string | null;
  }): Promise<ExecutedCommand> {
    const record = this.pageRecord(input.tab_id);
    await this.ensurePage(record);
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
    const page = this.requirePage(record);
    const locator = await record.snapshot.resolve(page, input.target);
    const sensitiveActionKind = await locator.evaluate((element) => {
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
      return patterns.find(([, pattern]) => pattern.test(descriptor))?.[0] ?? null;
    });
    if (sensitiveActionKind) {
      throw new ProtocolFailure(
        "browser_sensitive_action_requires_user",
        `model click is blocked for sensitive action: ${sensitiveActionKind}`,
        true,
        false,
        "用户接管浏览器后可以手动完成该操作；不要自动重试当前点击。",
      );
    }
    this.control.validate(input.control);
    return this.withNavigationGuard(record, async () => {
      try {
        await locator.click();
      } catch (error) {
        this.throwBlockedNavigation(record);
        throw playwrightFailure("browser_click_failed", error, true);
      }
      this.throwBlockedNavigation(record);
      return { result: { type: "page_state", payload: await this.pageState(record) } };
    });
  }

  private async type(input: {
    tab_id: string;
    control: HostControl;
    target: { snapshot_revision: number; element_ref: string };
    text: string;
    replace: boolean;
  }): Promise<ExecutedCommand> {
    this.control.validate(input.control);
    const record = this.pageRecord(input.tab_id);
    await this.ensurePage(record);
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
    return this.withNavigationGuard(record, async () => {
      await locator.focus();
      this.control.validate(input.control);
      try {
        if (input.replace) {
          await locator.fill(input.text);
        } else {
          await page.keyboard.insertText(input.text);
        }
      } catch (error) {
        this.throwBlockedNavigation(record);
        throw playwrightFailure("browser_type_failed", error, true);
      }
      this.throwBlockedNavigation(record);
      return { result: { type: "page_state", payload: await this.pageState(record) } };
    });
  }

  private async press(input: {
    tab_id: string;
    control: HostControl;
    key: string;
  }): Promise<ExecutedCommand> {
    this.control.validate(input.control);
    const record = this.pageRecord(input.tab_id);
    await this.ensurePage(record);
    const page = this.requirePage(record);
    return this.withNavigationGuard(record, async () => {
      this.control.validate(input.control);
      try {
        await page.keyboard.press(input.key);
      } catch (error) {
        this.throwBlockedNavigation(record);
        throw playwrightFailure("browser_press_failed", error, true);
      }
      this.throwBlockedNavigation(record);
      return { result: { type: "page_state", payload: await this.pageState(record) } };
    });
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
    const page = this.requirePage(record);
    return this.withNavigationGuard(record, async () => {
      if (input.target) {
        const locator = await record.snapshot.resolve(page, input.target);
        this.control.validate(input.control);
        await locator.evaluate(
          (element, delta) => (element as HTMLElement).scrollBy(delta.x, delta.y),
          { x: input.delta_x, y: input.delta_y },
        );
      } else {
        this.control.validate(input.control);
        await page.mouse.wheel(input.delta_x, input.delta_y);
      }
      this.throwBlockedNavigation(record);
      return { result: { type: "page_state", payload: await this.pageState(record) } };
    });
  }

  private async screenshot(input: {
    tab_id: string;
    target?: { snapshot_revision: number; element_ref: string } | null;
    clip?: NormalizedRect | null;
    full_page: boolean;
    format: "png" | "jpeg";
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
      fromSurface: true,
      captureBeyondViewport,
      clip,
    });
    const bytes = Buffer.from(capture.data, "base64");
    const payload = binaryPayload(
      bytes,
      input.format === "png" ? "image/png" : "image/jpeg",
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

  private async startScreencast(input: {
    tab_id: string;
    format: "jpeg" | "png";
    quality: number;
    max_width: number;
    max_height: number;
  }): Promise<ExecutedCommand> {
    const record = this.pageRecord(input.tab_id);
    await this.ensurePage(record);
    record.screencastSettings = {
      format: input.format,
      quality: Math.max(0, Math.min(Math.floor(input.quality), 100)),
      maxWidth: Math.max(320, Math.min(Math.floor(input.max_width), MAX_SCREENCAST_WIDTH)),
      maxHeight: Math.max(240, Math.min(Math.floor(input.max_height), MAX_SCREENCAST_HEIGHT)),
    };
    await this.restartScreencast(record);
    return emptyResult();
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
      record.frameSequence += 1;
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
      this.#transport.emit({ type: "screencast_frame", payload: metadata });
      this.#transport.emitBinary(bytes);
      acceptFirstFrame?.();
      acceptFirstFrame = undefined;
      record.screencastAck = record.screencastAck
        .catch(() => undefined)
        .then(async () => {
          await cdp
            .send("Page.screencastFrameAck", { sessionId: frame.sessionId })
            .catch(() => undefined);
        });
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
    this.control.validate(input.control);
    const clipboard = await clipboardTextForShortcut(page, input.event);
    await dispatchUserInput(cdp, input.event);
    return clipboard
      ? { result: { type: "clipboard_text", payload: clipboard } }
      : emptyResult();
  }

  private async bindPageEvents(record: PageRecord): Promise<void> {
    const page = this.requirePage(record);
    page.on("console", (message: ConsoleMessage) => {
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
      this.#transport.emit({
        type: "dialog",
        payload: {
          tab_id: record.tabId,
          dialog_type: dialog.type(),
          message: dialog.message(),
        },
      });
      void dialog.dismiss().catch(() => undefined);
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
      this.#transport.emit({
        type: "popup_blocked",
        payload: { tab_id: record.tabId },
      });
      void popup.close().catch(() => undefined);
    });
    page.on("crash", () => {
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
      void this.pageState(record).then((state) => {
        this.#transport.emit({ type: "page_updated", payload: state });
      });
    });
  }

  private async withNavigationGuard<T>(
    record: PageRecord,
    action: () => Promise<T>,
  ): Promise<T> {
    // The security policy is installed once for the whole Context. Keeping a
    // per-command Context route creates cross-tab races because Playwright
    // routes are Context-scoped, not Page-scoped.
    void record;
    return action();
  }

  private async installNavigationGuard(): Promise<void> {
    await this.context().route("**/*", async (route) => {
      const request = route.request();
      const page = request.frame().page();
      const record = [...this.#pages.values()].find((candidate) => candidate.page === page);
      if (
        !record
        || !request.isNavigationRequest()
        || request.frame() !== page.mainFrame()
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
    if (!page) {
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
}

function emptyResult(): ExecutedCommand {
  return { result: { type: "empty" } };
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
