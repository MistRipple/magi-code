import { createHash, randomUUID } from "node:crypto";
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
  readonly page: Page;
  readonly snapshot: SnapshotRegistry;
  navigationRevision: number;
  frameSequence: number;
  blockedNavigationUrl?: string;
  restoringInitialPage: boolean;
  viewport: HostViewport;
  readonly defaultUserAgent: string;
  readonly defaultPlatform: string;
  cdp?: CDPSession;
  screencastListener?: (event: ScreencastFrameEvent) => void;
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
    this.#context = await chromium.launchPersistentContext(
      this.#config.profilePath,
      {
        executablePath: this.#config.chromiumExecutable,
        headless: this.#config.headless,
        acceptDownloads: true,
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
    const page = await this.context().newPage();
    try {
      const [defaultUserAgent, defaultPlatform] = await Promise.all([
        page.evaluate(() => navigator.userAgent),
        page.evaluate(() => navigator.platform),
      ]);
      const record: PageRecord = {
        tabId: input.tab_id,
        page,
        snapshot: new SnapshotRegistry(input.snapshot_revision),
        navigationRevision: input.navigation_revision,
        frameSequence: 0,
        restoringInitialPage: false,
        viewport: input.viewport,
        defaultUserAgent,
        defaultPlatform,
      };
      this.#pages.set(input.tab_id, record);
      await this.applyViewport(record, input.viewport);
      await this.bindPageEvents(record);
      if (input.initial_url && input.initial_url !== "about:blank") {
        record.restoringInitialPage = true;
        try {
          await this.withNavigationGuard(record, () => page.goto(input.initial_url, {
            waitUntil: "domcontentloaded",
            timeout: PAGE_NAVIGATION_TIMEOUT_MILLIS,
          }));
          this.throwBlockedNavigation(record);
        } finally {
          record.restoringInitialPage = false;
        }
      }
      // CreatePage 的 revision 是页面创建完成后的权威基线；首次加载属于恢复/创建过程，
      // 不能被记成一次额外导航，否则 about:blank 与普通 URL 会产生不同的版本语义。
      const state = await this.pageState(record);
      this.#transport.emit({ type: "page_updated", payload: state });
      return { result: { type: "page_state", payload: state } };
    } catch (error) {
      this.#pages.delete(input.tab_id);
      await page.close().catch(() => undefined);
      throw playwrightFailure("browser_page_creation_failed", error, false);
    }
  }

  private async closePage(tabId: string): Promise<ExecutedCommand> {
    const record = this.pageRecord(tabId);
    await this.stopScreencast(tabId);
    this.#pages.delete(tabId);
    await record.page.close();
    return emptyResult();
  }

  private async setViewport(input: {
    tab_id: string;
    viewport: HostViewport;
  }): Promise<ExecutedCommand> {
    validateViewport(input.viewport);
    const record = this.pageRecord(input.tab_id);
    await this.applyViewport(record, input.viewport);
    record.snapshot.invalidate();
    return emptyResult();
  }

  private async applyViewport(record: PageRecord, viewport: HostViewport): Promise<void> {
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
    await record.page.evaluate(
      () => new Promise<void>((accept) => requestAnimationFrame(() => accept())),
    );
    record.viewport = viewport;
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

  private async cdpSession(record: PageRecord): Promise<CDPSession> {
    record.cdp ??= await this.context().newCDPSession(record.page);
    return record.cdp;
  }

  private async activatePage(tabId: string): Promise<ExecutedCommand> {
    const record = this.pageRecord(tabId);
    await record.page.bringToFront();
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
    return this.withNavigationGuard(record, async () => {
      this.control.validate(input.control);
      try {
        switch (input.navigation.action) {
          case "url":
            validateNavigationUrl(input.navigation.url);
            await record.page.goto(input.navigation.url, {
              waitUntil: "domcontentloaded",
            });
            break;
          case "back":
            await record.page.goBack({ waitUntil: "domcontentloaded" });
            break;
          case "forward":
            await record.page.goForward({ waitUntil: "domcontentloaded" });
            break;
          case "reload":
            await record.page.reload({ waitUntil: "domcontentloaded" });
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
    const snapshot = await record.snapshot.capture(
      record.page,
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
    const locator = await record.snapshot.resolve(record.page, input.target);
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
    const locator = await record.snapshot.resolve(record.page, input.target);
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
          await record.page.keyboard.insertText(input.text);
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
    return this.withNavigationGuard(record, async () => {
      this.control.validate(input.control);
      try {
        await record.page.keyboard.press(input.key);
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
    return this.withNavigationGuard(record, async () => {
      if (input.target) {
        const locator = await record.snapshot.resolve(record.page, input.target);
        this.control.validate(input.control);
        await locator.evaluate(
          (element, delta) => (element as HTMLElement).scrollBy(delta.x, delta.y),
          { x: input.delta_x, y: input.delta_y },
        );
      } else {
        this.control.validate(input.control);
        await record.page.mouse.wheel(input.delta_x, input.delta_y);
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
      const locator = await record.snapshot.resolve(record.page, input.target);
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
    const hit = await record.page.evaluate(
      ({ x, y, frameSequence, navigationRevision }): HitTest | null => {
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
        x: input.x,
        y: input.y,
        frameSequence: record.frameSequence,
        navigationRevision: input.navigation_revision,
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
    await this.stopScreencast(input.tab_id);
    const cdp = await this.cdpSession(record);
    const quality = Math.max(0, Math.min(Math.floor(input.quality), 100));
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
        input.format === "png" ? "image/png" : "image/jpeg",
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
      void cdp
        .send("Page.screencastFrameAck", { sessionId: frame.sessionId })
        .catch(() => undefined);
    };
    record.screencastListener = listener;
    cdp.on("Page.screencastFrame", listener);
    await cdp.send("Page.startScreencast", {
      format: input.format,
      quality: input.format === "jpeg" ? quality : undefined,
      maxWidth: Math.max(320, Math.min(Math.floor(input.max_width), MAX_SCREENCAST_WIDTH)),
      maxHeight: Math.max(240, Math.min(Math.floor(input.max_height), MAX_SCREENCAST_HEIGHT)),
      everyNthFrame: 1,
    });
    return emptyResult();
  }

  private async stopScreencast(tabId: string): Promise<ExecutedCommand> {
    const record = this.#pages.get(tabId);
    if (!record?.cdp) return emptyResult();
    const cdp = record.cdp;
    if (record.screencastListener) {
      cdp.off("Page.screencastFrame", record.screencastListener);
    }
    record.screencastListener = undefined;
    await cdp.send("Page.stopScreencast").catch(() => undefined);
    return emptyResult();
  }

  private async userInput(input: {
    tab_id: string;
    control: HostControl;
    event: UserInputEvent;
  }): Promise<ExecutedCommand> {
    this.control.validate(input.control);
    const record = this.pageRecord(input.tab_id);
    const cdp = await this.cdpSession(record);
    this.control.validate(input.control);
    const clipboard = await clipboardTextForShortcut(record.page, input.event);
    await dispatchUserInput(cdp, input.event);
    return clipboard
      ? { result: { type: "clipboard_text", payload: clipboard } }
      : emptyResult();
  }

  private async bindPageEvents(record: PageRecord): Promise<void> {
    record.page.on("console", (message: ConsoleMessage) => {
      this.#transport.emit({
        type: "console",
        payload: {
          tab_id: record.tabId,
          level: message.type(),
          text: message.text(),
        },
      });
    });
    record.page.on("dialog", (dialog: Dialog) => {
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
    record.page.on("download", (download: Download) => {
      this.#transport.emit({
        type: "download",
        payload: {
          tab_id: record.tabId,
          suggested_filename: download.suggestedFilename(),
          state: "started",
        },
      });
    });
    record.page.on("crash", () => {
      this.#transport.emit({
        type: "page_crashed",
        payload: { tab_id: record.tabId },
      });
    });
    record.page.on("framenavigated", (frame) => {
      if (frame !== record.page.mainFrame()) return;
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
    const handler = async (route: import("playwright-core").Route) => {
      const request = route.request();
      if (
        !request.isNavigationRequest()
        || request.frame() !== record.page.mainFrame()
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
    };
    const context = this.context();
    await context.route("**/*", handler);
    try {
      return await action();
    } finally {
      await context.unroute("**/*", handler).catch(() => undefined);
    }
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
    const url = record.page.url();
    let origin: string | null = null;
    try {
      const parsed = new URL(url);
      origin = parsed.origin === "null" ? null : parsed.origin;
    } catch {
      origin = null;
    }
    return {
      tab_id: record.tabId,
      url,
      origin,
      title: await record.page.title().catch(() => ""),
      navigation_revision: record.navigationRevision,
    };
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
  device_scale_factor_millis: number;
  device_type: HostDeviceType;
}): void {
  requireSafeInteger("viewport.width", viewport.width);
  requireSafeInteger("viewport.height", viewport.height);
  requireSafeInteger(
    "viewport.device_scale_factor_millis",
    viewport.device_scale_factor_millis,
  );
  if (
    viewport.width < 320 ||
    viewport.width > 7_680 ||
    viewport.height < 240 ||
    viewport.height > 4_320 ||
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
