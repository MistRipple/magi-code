import { createHash } from "node:crypto";

/**
 * ChatGPT Web 站点适配层：**DOM 事实的唯一归属地**。
 *
 * 设计依据：设计基线 §5.5、§5.6，实现计划 §7.7。
 *
 * 分层：
 * - 页面脚本（`INSTALL_WEB_MODEL_ADAPTER`）只做 DOM 读取与写入，把结果整理成
 *   **原始快照**（`WebModelRawSnapshot` / 写入回读结果），不含任何业务判断；
 * - 归一化（`normalizeWebModelProbe`）是纯函数，只吃原始快照，不碰 DOM，
 *   因此可以用录制的快照 fixture 做单测来控制 selector 漂移（实现计划 §9.4）。
 *
 * 这里不实现任何协议层语义（续轮封装、`magi-tool-call` 解析等）：那些在
 * `crates/magi-web-model`，两半各只有一处实现。
 */

/** 站点适配层结构版本。selector 集变化即推进，用于诊断与漂移判定（A23）。 */
export const WEB_MODEL_SITE_REVISION = "chatgpt-web-3";

/** ChatGPT 页面的 DOM 事实（selector、命名）。集中在这里，不得散落。 */
export const WEB_MODEL_SELECTORS = {
  /** 主 composer（contenteditable 富文本编辑器）。 */
  composer: [
    // Current ChatGPT (ProseMirror) no longer emits the old id/test-id.  Keep
    // the semantic role/label selectors behind this adapter instead of making
    // the daemon know about the site's renderer.
    "form[data-chatgpt-composer] [data-composer-markdown][contenteditable='true'][role='textbox']",
    "[contenteditable='true'][role='textbox'][aria-label*='ChatGPT']",
    "[contenteditable='true'][role='textbox'][aria-label*='询问']",
    "[contenteditable='true'][role='textbox'][aria-label*='message' i]",
    "[contenteditable='true'][role='textbox'][aria-label*='消息']",
    "[data-testid='prompt-textarea']",
    "div#prompt-textarea[contenteditable='true']",
    "div[contenteditable='true'][data-testid='prompt-textarea']",
    "main div.ProseMirror[contenteditable='true']",
    "main [contenteditable='true'][data-placeholder]",
    "main [contenteditable='true']",
    "textarea#prompt-textarea",
    "textarea[placeholder*='Ask' i]",
    "textarea[placeholder*='询问']",
  ],
  /** 会话主区域，用于判定已进入对话页。 */
  conversation: ["main", "[role='main']"],
  /** 模型选择器按钮。 */
  modelMenuButton: [
    "button[aria-label='选择 ChatGPT 模型']",
    "button[aria-label='Choose a model']",
    "button[aria-label='Model selector']",
    "button[aria-label*='model selector' i]",
    "button[aria-label*='选择模型']",
    // Current zh-CN ChatGPT labels the popover trigger "模型选择器".  It
    // does not contain the older "选择模型" word order and may expose
    // `aria-haspopup="dialog"` instead of `menu`, so keep this explicit
    // semantic selector before the generic fallback below.
    "button[aria-label*='模型选择器']",
    "[role='button'][aria-label*='模型选择器']",
    "button[aria-label*='Choose model']",
    "button[data-testid='model-switcher-dropdown-button']",
    "button[data-testid*='model-switcher']",
    "button[data-testid*='model-picker']",
    "[data-testid='model-selector'] button",
    "button[aria-haspopup='menu'][data-testid*='model']",
  ],
  /** 模型菜单里的候选项（打开菜单后读取）。 */
  modelMenuItem: [
    "[role='menuitemradio']",
    "[role='option'][data-value*='gpt' i]",
    "[role='option'][data-model]",
    "[role='menuitem'][data-testid*='model' i]",
    "[data-testid*='model-option' i]",
    "[data-model-id]",
    "[data-model]",
    // The current menu renders model rows as plain menuitems/buttons without
    // a data-testid.  The adapter still filters/normalizes their labels below;
    // non-model actions (for example Upgrade) are ignored by normalizeModelFamily.
    "[role='menu'] [role='menuitem']",
    "[role='menu'] button",
    "[role='menu'] [data-value]",
  ],
  /** 强度（effort）子菜单选项。 */
  effortMenuItem: ["[role='menuitemradio'][data-testid*='effort']", "[role='menuitem'][data-testid*='effort']"],
  /** 风险 / 验证页标志。 */
  blockPage: ["#challenge-form", "iframe[src*='challenges.cloudflare.com']", "[data-testid='blocked-page']"],
  /** 附件 chip：出现即说明输入被转成了附件（§5.9.6）。 */
  attachmentChip: ["[data-testid='attachment-chip']", "[data-testid*='attachment'][role='listitem']"],
  /** 提交按钮。找不到时退回 composer 上的 Enter 键（ChatGPT 的默认提交手势）。 */
  submitButton: [
    "button[data-testid='send-button']",
    "button[data-testid='composer-submit-button']",
    "button[aria-label*='Send' i]",
    "button[aria-label*='发送']",
  ],
  /** 可点击的停止按钮；流式状态标志单独放在 generatingControl。 */
  stopButton: [
    "button[data-testid='stop-button']",
    "button[data-testid='stop-generating-button']",
    "button[aria-label*='Stop' i]",
    "button[aria-label*='停止']",
  ],
  /** ChatGPT 首次进入临时聊天时显示的确认按钮。 */
  temporaryChatConfirmButton: [
    "[role='dialog'] button",
    "[data-testid*='temporary-chat' i] button",
    "button[aria-label*='Continue' i]",
    "button[aria-label*='继续']",
    "button",
  ],
  /** 生成中控制（停止按钮 / 流式状态）。出现即视为仍在生成。 */
  generatingControl: [
    "button[data-testid='stop-button']",
    "button[data-testid='stop-generating-button']",
    "button[aria-label*='Stop' i]",
    "button[aria-label*='停止']",
    "[data-testid='streaming-status']",
    "[data-testid*='streaming-status']",
    "[data-testid*='conversation-turn-loading']",
    "[data-message-author-role='assistant'][aria-busy='true']",
  ],
  /** 用户消息节点：提交是否被站点接受以它为准，而不是命令返回成功。 */
  userMessage: [
    "[data-message-author-role='user']",
    "[data-user-message-bubble]",
    "[data-testid^='conversation-turn-'][data-turn='user']",
    "[data-testid^='conversation-turn-']:has([data-user-message-bubble])",
  ],
  /** 助手消息节点：流式读取与完成判定都用最后一条。 */
  assistantMessage: [
    "[data-message-author-role='assistant']",
    "[data-testid^='conversation-turn-'][data-turn='assistant']",
    "[data-testid^='conversation-turn-'][data-message-author-role='assistant']",
    "[data-testid^='conversation-turn-']:has([data-conversation-role='assistant'])",
    "[data-content-search-unit-key]:has([data-conversation-role='assistant'])",
    "[data-content-search-unit-key]:has([data-chatgpt-agent-turn-start])",
  ],
  /** 隐藏推理区块：映射到 `thinking`。 */
  reasoningBlock: [
    "[data-testid='reasoning-block']",
    "[data-message-author-role='assistant'] [data-testid*='reasoning']",
    "[data-testid^='conversation-turn-'] [data-testid*='reasoning']",
    "[data-content-search-unit-key] [data-testid*='reasoning']",
  ],
} as const;

/** 页面脚本回传的原始快照。只有站点结构，没有 Magi 语义。 */
export interface WebModelRawSnapshot {
  origin: string;
  path: string;
  /** 命中风险 / 验证页。 */
  blocked: boolean;
  /** composer 是否可达（登录态的第二项证据，§5.5）。 */
  composerFound: boolean;
  /** composer 的单条字符上限；读不到为 null。 */
  composerCharLimit: number | null;
  /** 是否读到会话主区域。 */
  conversationFound: boolean;
  /** 模型菜单是否被成功打开并读到候选项。 */
  modelMenuFound: boolean;
  /** 菜单里读到的模型项。 */
  modelItems: Array<{
    label: string;
    modelId?: string | null;
    checked: boolean;
    efforts: string[];
    /** 页面明确暴露的当前档位；没有证据时为 null。 */
    defaultEffort?: string | null;
  }>;
  /** 账号等级提示（Plus / Pro / Free），读不到为 null。 */
  accountHint: string | null;
  /** 站点是否暴露自定义连接器设置入口。 */
  connectorSettingsFound: boolean;
}

export type WebModelProbeDiagnostic =
  | "site_origin_unsupported"
  | "composer_selector_missing"
  | "conversation_selector_missing"
  | "model_menu_selector_missing"
  | "model_items_unrecognized"
  | null;

/**
 * ChatGPT Web 的受控页面 origin。
 *
 * 产品默认只允许官方 origin。自动化验收通过同一进程里的
 * `MAGI_WEB_MODEL_ORIGIN` 把站点替换为 loopback fixture；仅当该覆盖值本身是
 * loopback origin 且与当前页面完全相等时放行，避免把任意外部站点变成 Web 模型
 * 来源。
 */
export function isSupportedChatGptOrigin(origin: string): boolean {
  if (origin === "https://chatgpt.com" || origin === "https://chat.openai.com") return true;
  const override = process.env.MAGI_WEB_MODEL_ORIGIN?.trim() || "";
  if (!override || origin !== override) return false;
  try {
    const url = new URL(override);
    return (url.protocol === "http:" || url.protocol === "https:")
      && (url.hostname === "127.0.0.1" || url.hostname === "localhost" || url.hostname === "[::1]")
      && url.username === ""
      && url.password === ""
      // `new URL("http://127.0.0.1:1234").pathname` is `/` even though the
      // configured override is an origin without a path. Treat the implicit
      // root path as equivalent to the empty path; otherwise every loopback
      // fixture is rejected before the site adapter can run.
      && (url.pathname === "" || url.pathname === "/")
      && url.search === ""
      && url.hash === "";
  } catch {
    return false;
  }
}

/**
 * 写入 / 回读共用的归一化（《实现计划》§7.7 冻结的口径）。
 *
 * 富文本编辑器（Chromium 的 contenteditable 与站点使用的 ProseMirror）会用块元素
 * 表达换行，回读时与写入文本存在两种**无损的**排版差异：
 * 1. 空行会落成额外的块边界：`A\n\nB` 回读得到 `A\n\n\nB`；
 * 2. 末尾总有一个块边界：`A\n` 回读得到 `A\n\n`。
 *
 * 因此比对前统一换行、折叠 3 个及以上连续换行、并去掉末尾换行；两端用同一口径，
 * 才能既不错判「写入未确认」，也不放过真实的内容改写（其余差异仍逐字比对）。
 */
export function normalizeComposerText(text: string): string {
  return text
    // Chromium / ProseMirror expose several non-breaking-space variants through
    // innerText. They are DOM serializer artifacts, not user-visible rewrites;
    // map every space separator used by the browser before applying the stricter
    // newline rules below. Keeping this list here (rather than using `\s`) is
    // intentional: other whitespace must remain observable so a real site-side
    // rewrite cannot be mistaken for a successful write.
    .replace(/[\u00a0\u1680\u180e\u2000-\u200a\u202f\u205f\u3000]/gu, ' ')
    .replace(/\r\n?/gu, "\n")
    .replace(/\n{3,}/gu, "\n\n")
    .replace(/\n+$/u, "");
}

/**
 * Chromium 的 contenteditable 在把纯文本交给 ProseMirror / React 后，
 * 可能把单个换行回读成一个额外的块边界（`A\nB` → `A\n\nB`）。
 * 这不是用户内容被改写，而是页面编辑器的 DOM 序列化方式；连续空行
 * 已由 `normalizeComposerText` 收敛为两个换行，因此这里必须只把每个
 * 换行 run 归一成一个块边界，不能简单地把所有双换行都删掉。
 */
export function contentEditableBlockBoundaryVariant(text: string): string {
  return normalizeComposerText(text).replace(/\n+/gu, "\n\n");
}

/**
 * 判断页面回读是否仍表达了同一段文本。
 *
 * 第一项是严格语义比较；第二项只接受已知的 contenteditable 块边界
 * 序列化差异。真实字符改写、丢失或插入的空格不会通过此判断。
 */
export function composerTextReadbackMatches(expected: string, observed: string): boolean {
  const normalizedExpected = normalizeComposerText(expected);
  const normalizedObserved = normalizeComposerText(observed);
  return normalizedObserved === normalizedExpected
    || normalizedObserved === contentEditableBlockBoundaryVariant(normalizedExpected);
}

/**
 * 回读不一致时的**最小可诊断信息**。
 *
 * 只报告第一个分歧位置与两侧码点（用 U+ 形式），以及两侧总长度——不复制正文。正文可能很长，
 * 也可能包含敏感内容，诊断入口与日志都不需要看到原文就能定位「站点改写了什么」。
 */
export interface TextDivergence {
  index: number;
  expectedChar: string;
  observedChar: string;
  expectedLength: number;
  observedLength: number;
}

export function firstTextDivergence(expected: string, observed: string): TextDivergence {
  const left = Array.from(expected);
  const right = Array.from(observed);
  const limit = Math.min(left.length, right.length);
  for (let index = 0; index < limit; index += 1) {
    if (left[index] !== right[index]) {
      return {
        index,
        expectedChar: diagnosticCodePoint(left[index]),
        observedChar: diagnosticCodePoint(right[index]),
        expectedLength: left.length,
        observedLength: right.length,
      };
    }
  }
  return {
    index: limit,
    expectedChar: diagnosticCodePoint(left[limit]),
    observedChar: diagnosticCodePoint(right[limit]),
    expectedLength: left.length,
    observedLength: right.length,
  };
}

function diagnosticCodePoint(value: string | undefined): string {
  if (value === undefined) return "<eof>";
  const codePoint = value.codePointAt(0);
  return codePoint === undefined ? "<eof>" : `U+${codePoint.toString(16).toUpperCase().padStart(4, "0")}`;
}

/**
 * 写入 / 回读共用的摘要算法（sha256 hex over UTF-8）。
 *
 * 摘要有且只有这一处实现：页面脚本只回读文本，摘要由调用方计算并比对，
 * 因此写入与回读必然使用同一算法（实现计划 §7.7）。
 */
export function composerTextDigest(text: string): string {
  return createHash("sha256").update(normalizeComposerText(text), "utf8").digest("hex");
}

/**
 * 安装站点适配器。幂等：同一 `adapterEpoch` 下重复安装直接返回。
 *
 * 只读写 DOM，不做任何 Magi 侧判断；对外暴露 `globalThis.__magiWebModel`。
 */
export const INSTALL_WEB_MODEL_ADAPTER = String.raw`
((adapterEpoch) => {
  if (globalThis.__magiWebModel?.adapter_epoch === adapterEpoch) return;
  const SELECTORS = ${JSON.stringify(WEB_MODEL_SELECTORS)};
  const REVISION = ${JSON.stringify(WEB_MODEL_SITE_REVISION)};
  const queryAll = (root, selector) => {
    try {
      return [...root.querySelectorAll(selector)];
    } catch (error) {
      throw new Error('web_selector_invalid:' + selector);
    }
  };
  const firstMatch = (values, root = document) => {
    for (const selector of values) {
      let found;
      try {
        found = root.querySelector(selector);
      } catch (error) {
        throw new Error('web_selector_invalid:' + selector);
      }
      if (found) return found;
    }
    return null;
  };
  const allMatches = (values, root = document) => {
    const seen = new Set();
    const out = [];
    for (const selector of values) {
      for (const node of queryAll(root, selector)) {
        if (seen.has(node)) continue;
        seen.add(node);
        out.push(node);
      }
    }
    return out;
  };
  // 语义 selector 记号：daemon 只发 @composer 这类引用，真实 selector 只在本层
  // （设计基线 §5.5：selector 只有一个实现处；R49 的驱动路径不复制选择器）。
  const resolveSelector = (token) => {
    if (typeof token !== 'string' || token.trim() === '') {
      throw new Error('web_selector_invalid:selector is empty');
    }
    if (token.startsWith('@')) {
      const resolved = SELECTORS[token.slice(1)];
      if (!resolved) throw new Error('web_selector_unknown:' + token);
      return resolved;
    }
    return [token];
  };
  const visible = (element) => {
    if (!element) return false;
    if (element.hidden || element.getAttribute('aria-hidden') === 'true') return false;
    const style = getComputedStyle(element);
    const rect = element.getBoundingClientRect();
    return style.display !== 'none'
      && style.visibility !== 'hidden'
      && Number(style.opacity || '1') > 0
      && rect.width > 0
      && rect.height > 0;
  };
  // 状态标志有时是无尺寸的 aria / data 节点。停止按钮仍要求可见尺寸，
  // 生成状态只检查它没有被 hidden/display/aria-hidden 收起。
  const exposed = (element) => {
    if (!element) return false;
    if (element.hidden || element.getAttribute('aria-hidden') === 'true') return false;
    const style = getComputedStyle(element);
    return style.display !== 'none' && style.visibility !== 'hidden' && Number(style.opacity || '1') > 0;
  };
  const editable = (element) => {
    if (!element) return false;
    const tag = element.tagName;
    if (tag === 'TEXTAREA' || tag === 'INPUT') return !element.disabled;
    return element.getAttribute('contenteditable') !== null
      && element.getAttribute('contenteditable') !== 'false'
      && !element.hasAttribute('disabled');
  };
  const firstVisible = (values, root = document) => {
    for (const element of allMatches(values, root)) {
      if (visible(element)) return element;
    }
    return null;
  };
  const firstComposer = () => {
    for (const element of allMatches(SELECTORS.composer)) {
      if (visible(element) && editable(element)) return element;
    }
    return null;
  };
  const textOf = (element) => (element?.innerText || element?.textContent || '').replace(/\s+/g, ' ').trim();
  const composerText = (element) => {
    if (!element) return '';
    if (element.tagName === 'TEXTAREA' || element.tagName === 'INPUT') return element.value || '';
    return element.innerText ?? element.textContent ?? '';
  };
  const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const integerAttribute = (element, names) => {
    for (const name of names) {
      const value = Number(element?.getAttribute(name) ?? '');
      if (Number.isSafeInteger(value) && value >= 0) return value;
    }
    return null;
  };

  const accountHint = () => {
    const owner = document.documentElement.getAttribute('data-magi-account-plan');
    if (owner?.trim()) return owner.trim().toLowerCase();
    const probe = document.querySelector('[data-testid="account-plan"], [data-testid="plan-badge"]');
    const profile = document.querySelector(
      'button[aria-label*="个人资料"], button[aria-label*="profile" i]',
    );
    // The current renderer keeps the account tier next to the profile trigger
    // without a stable test id.  Limit the read to that small subtree; never
    // infer a plan from the entire conversation body.
    const profileText = profile?.parentElement?.parentElement ?? profile;
    const text = [textOf(probe), textOf(profileText)].join(' ').toLowerCase();
    const hasWord = (word) => new RegExp('(?:^|[^a-z])' + word + '(?:$|[^a-z])', 'i').test(text);
    if (hasWord('pro')) return 'pro';
    if (hasWord('plus')) return 'plus';
    if (hasWord('free')) return 'free';
    return null;
  };

  const effortLabel = (value) => {
    const text = String(value ?? '').trim().toLowerCase();
    if (!text) return null;
    const match = text.match(/(?:^|[^a-z])(low|medium|high|x[- ]?high|max)(?:$|[^a-z])/i);
    if (!match) return null;
    return match[1].replace(/[- ]/g, '') === 'xhigh' ? 'xhigh' : match[1].toLowerCase();
  };
  const effortLabelFromElement = (element) => {
    for (const value of [
      element?.getAttribute('data-effort'),
      element?.getAttribute('aria-valuetext'),
      element?.getAttribute('aria-label'),
      element?.getAttribute('data-value'),
      textOf(element),
    ]) {
      const label = effortLabel(value);
      if (label) return label;
    }
    return null;
  };
  const effortValues = () => {
    const sliderRoot = firstVisible([
      '[data-model-picker-power-slider]',
      '[data-model-reasoning-effort-slider]',
    ]);
    const slider = sliderRoot?.querySelector('[role="slider"]');
    const min = Number(slider?.getAttribute('aria-valuemin') ?? '');
    const max = Number(slider?.getAttribute('aria-valuemax') ?? '');
    const count = Number.isSafeInteger(min) && Number.isSafeInteger(max) && max >= min
      ? max - min + 1
      : 0;
    if (count >= 1 && count <= 5) {
      // ChatGPT exposes an ordinal slider rather than stable text labels.  This
      // mapping is kept inside the site adapter and is only used when the
      // slider exposes a complete finite range.
      const values = ['low', 'medium', 'high', 'xhigh', 'max'].slice(0, count);
      const now = Number(slider?.getAttribute('aria-valuenow') ?? '');
      const selected = Number.isSafeInteger(now) && now >= min && now <= max
        ? values[now - min] ?? null
        : effortLabelFromElement(slider);
      return { values, selected };
    }
    const options = allMatches([
      ...SELECTORS.effortMenuItem,
      '[data-model-reasoning-effort-slider] [data-effort]',
      '[data-model-picker-power-slider] [data-effort]',
      '[role="menuitemradio"][data-effort]',
    ]).filter(exposed);
    const values = [];
    let selected = null;
    for (const option of options) {
      const label = effortLabelFromElement(option);
      if (!label) continue;
      if (!values.includes(label)) values.push(label);
      if (option.getAttribute('aria-checked') === 'true' || option.getAttribute('aria-selected') === 'true') {
        selected = label;
      }
    }
    return { values, selected };
  };

  const modelMenuItems = (menuButton) => {
    const controls = menuButton?.getAttribute('aria-controls');
    const controlled = controls ? document.getElementById(controls) : null;
    const topLevel = (root) => allMatches(SELECTORS.modelMenuItem, root)
      .filter((item) => !item.parentElement || item.parentElement.closest('[role="menuitemradio"]') === null)
      .filter(visible);
    if (controlled) return topLevel(controlled);
    const menus = allMatches(['[role="menu"]']).filter(visible);
    for (const menu of menus) {
      const items = topLevel(menu);
      if (items.length > 0) return items;
    }
    return topLevel(document);
  };

  const modelButtonLabel = (element) => [
    element?.getAttribute('aria-label'),
    element?.getAttribute('data-testid'),
    textOf(element),
  ].filter(Boolean).join(' ');
  const modelMenuButton = () => {
    const explicit = firstVisible(SELECTORS.modelMenuButton);
    if (explicit) return explicit;
    const semantic = allMatches([
      'button[aria-haspopup="menu"]',
      '[role="button"][aria-haspopup="menu"]',
    ]).find((element) => visible(element) && /(?:model|模型|gpt|codex|o[0-9])/iu.test(modelButtonLabel(element)));
    if (semantic) return semantic;
    // The current ChatGPT renderer exposes the trigger as a popup button in
    // accessibility, but omits both aria-haspopup and a stable test id.  Its
    // accessible name is the selected family (normally "ChatGPT").  Limit
    // this fallback to an exact family-shaped label so attachment/voice/menu
    // buttons cannot be mistaken for the model picker.
    return allMatches(['button', '[role="button"]']).find((element) => {
      if (!visible(element)) return false;
      const label = modelButtonLabel(element).trim();
      return /^(?:chatgpt|gpt(?:[- ]?[0-9]|$)|codex|o[0-9])/iu.test(label)
        || /模型选择器/iu.test(label);
    }) || null;
  };

  const probe = async () => {
    const composer = firstComposer();
    const blocked = allMatches(SELECTORS.blockPage).some(exposed);
    const conversation = firstVisible(SELECTORS.conversation);
    const snapshot = {
      origin: location.origin,
      path: location.pathname + location.search,
      blocked,
      composerFound: Boolean(composer),
      composerCharLimit: integerAttribute(composer, ['data-max-length', 'maxlength', 'aria-maxlength']),
      conversationFound: Boolean(conversation),
      modelMenuFound: false,
      modelItems: [],
      accountHint: accountHint(),
      connectorSettingsFound: Boolean(document.querySelector(
        '[data-testid="connectors-settings"], [data-testid*="connector-settings"], a[href*="/settings/connectors"], a[href*="/settings/apps"]',
      )),
    };
    if (blocked) return snapshot;
    const menuButton = modelMenuButton();
    if (!menuButton) return snapshot;
    let items = modelMenuItems(menuButton);
    let openedByProbe = false;
    try {
      // 站点模型按钮是切换语义：菜单已展开时不能再次点击，否则会把它关掉。
      if (items.length === 0) {
        menuButton.click();
        openedByProbe = true;
        await sleep(120);
        items = modelMenuItems(menuButton);
      }
      if (items.length > 0) {
        snapshot.modelMenuFound = true;
        const effortState = effortValues();
        snapshot.modelItems = items.map((item) => {
          // Prefer effort controls inside this model item. A page can expose
          // different effort sets per model; using the global set would merge
          // sibling options and report capabilities the selected model lacks.
          const itemEfforts = [];
          const itemEffortElements = allMatches(SELECTORS.effortMenuItem, item).filter(exposed);
          for (const element of [
            ...itemEffortElements,
            ...queryAll(item, '[data-effort], [aria-valuetext], [aria-label], [data-value]').filter(exposed),
          ]) {
            const label = effortLabelFromElement(element);
            if (label && !itemEfforts.includes(label)) itemEfforts.push(label);
          }
          if (itemEfforts.length === 0) itemEfforts.push(...effortState.values);
          const itemSelected = [
            ...itemEffortElements,
            ...queryAll(item, '[data-effort], [aria-valuetext], [aria-label], [data-value]').filter(exposed),
          ]
            .filter((element) => element.getAttribute('aria-checked') === 'true'
              || element.getAttribute('aria-selected') === 'true'
              || element.getAttribute('data-state') === 'checked')
            .map(effortLabelFromElement)
            .find(Boolean) || null;
          const checked = item.getAttribute('aria-checked') === 'true'
            || item.getAttribute('aria-selected') === 'true'
            || item.getAttribute('data-state') === 'checked';
          const defaultEffort = checked
            ? itemSelected || effortState.selected
            : null;
          return {
            label: textOf(item),
            modelId: item.getAttribute('data-model-id')
              || item.getAttribute('data-model')
              || item.getAttribute('data-value')
              || null,
            checked,
            efforts: itemEfforts,
            defaultEffort,
          };
        });
      }
    } finally {
      // 收口：先按真实站点的 Escape 语义关闭菜单；只有本次探测打开过菜单且
      // Escape 没有收口时才点击按钮，避免误触碰用户本来打开的菜单。
      if (openedByProbe) {
        document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
        await sleep(0);
        if (modelMenuItems(menuButton).length > 0) menuButton.click();
      }
    }
    return snapshot;
  };

  const selectAllContents = (element) => {
    element.focus();
    const selection = globalThis.getSelection?.();
    if (!selection || typeof document.createRange !== 'function') {
      throw new Error('web_composer_selection_unavailable');
    }
    const range = document.createRange();
    range.selectNodeContents(element);
    selection.removeAllRanges();
    selection.addRange(range);
  };

  const dispatchTextInput = (element, value) => {
    try {
      element.dispatchEvent(new InputEvent('input', {
        bubbles: true,
        data: value,
        inputType: 'insertText',
      }));
    } catch (error) {
      element.dispatchEvent(new Event('input', { bubbles: true }));
    }
  };

  const writeText = async ({ selector, text, mode, timeout_ms: timeoutMs }) => {
    if (typeof text !== 'string') throw new Error('web_write_text_invalid:text must be a string');
    if (mode !== 'replace' && mode !== 'append') throw new Error('web_write_text_invalid:mode is invalid');
    const element = allMatches(resolveSelector(selector)).find((candidate) => visible(candidate) && editable(candidate));
    if (!element) throw new Error('web_selector_not_found:' + selector);
    const current = composerText(element);
    const next = mode === 'append' ? current + text : text;
    const attachmentCountBefore = allMatches(SELECTORS.attachmentChip).length;
    if (element.tagName === 'TEXTAREA' || element.tagName === 'INPUT') {
      const setter = Object.getOwnPropertyDescriptor(element.constructor.prototype, 'value')?.set;
      if (setter) setter.call(element, next); else element.value = next;
      dispatchTextInput(element, next);
    } else {
      // 先选中整个编辑器，再用浏览器原生 insertText。这样 ProseMirror / React
      // 能收到真实的 input 路径，避免直接改 textContent 后内部状态仍为空。
      selectAllContents(element);
      const inserted = typeof document.execCommand === 'function'
        && document.execCommand('insertText', false, next);
      if (!inserted) {
        element.textContent = next;
        dispatchTextInput(element, next);
      }
    }
    const parsedTimeout = Number(timeoutMs);
    const waitMs = Number.isFinite(parsedTimeout) && parsedTimeout >= 0 ? parsedTimeout : 250;
    const deadline = Date.now() + waitMs;
    let observed = composerText(element);
    while (Date.now() < deadline && element.isConnected && observed.length === 0 && next.length > 0) {
      await sleep(Math.min(25, Math.max(1, deadline - Date.now())));
      observed = composerText(element);
    }
    // 给站点的 controlled input 一次微任务机会，即使 timeout_ms=0 也不做长等待。
    if (waitMs > 0) await sleep(0);
    observed = composerText(element);
    const currentElement = allMatches(resolveSelector(selector)).find((candidate) => visible(candidate) && editable(candidate)) || element;
    observed = composerText(currentElement);
    const attachmentCountAfter = allMatches(SELECTORS.attachmentChip).length;
    return {
      text: observed,
      charCount: Array.from(observed).length,
      becameAttachment: attachmentCountAfter > attachmentCountBefore
        || (attachmentCountAfter > 0 && observed.trim().length === 0 && next.trim().length > 0),
    };
  };

  const observe = ({ selector, fields, attribute }) => {
    if (!Array.isArray(fields) || fields.length === 0) {
      throw new Error('web_observe_invalid:fields must not be empty');
    }
    const elements = allMatches(resolveSelector(selector));
    const nodes = elements.map((element) => {
      const node = { found: true, text: undefined, html: undefined, attributes: undefined };
      if (fields.includes('text')) node.text = element.innerText ?? element.textContent ?? '';
      if (fields.includes('html')) node.html = element.innerHTML ?? '';
      if (fields.includes('attribute')) {
        node.attributes = {};
        if (typeof attribute === 'string') {
          node.attributes[attribute] = element.getAttribute(attribute) ?? '';
        }
      }
      return node;
    });
    if (nodes.length === 0) nodes.push({ found: false });
    const revision = (globalThis.__magiWebModelRevision ?? 0) + 1;
    globalThis.__magiWebModelRevision = revision;
    return { revision, nodes };
  };

  const submit = async () => {
    const composer = firstComposer();
    if (!composer) return { submitted: false, composer_empty: true, reason: 'composer_missing' };
    if (!composerText(composer).trim()) return { submitted: false, composer_empty: true, reason: 'composer_empty' };
    const submitRoot = composer.closest('form') || document;
    const button = firstVisible(SELECTORS.submitButton, submitRoot);
    if (button) {
      if (button.disabled || button.getAttribute('aria-disabled') === 'true') {
        throw new Error('web_submit_unavailable:submit button is disabled');
      }
      button.click();
    } else {
      composer.focus();
      composer.dispatchEvent(new KeyboardEvent('keydown', {
        key: 'Enter', code: 'Enter', keyCode: 13, which: 13, bubbles: true, cancelable: true,
      }));
    }
    await sleep(80);
    const after = firstComposer();
    return { submitted: true, composer_empty: !after || composerText(after).trim().length === 0, reason: null };
  };

  // ChatGPT 在第一次打开 ?temporary-chat=true 时会先显示一个确认层。
  // 该层覆盖真实 composer，但 composer 节点仍可能已经挂载；如果直接写入，
  // 页面会把输入当成尚未确认的草稿，导致写入回读失败或下一次打开仍卡在确认层。
  // 只接受精确的 Continue/继续语义，不能按按钮位置或“临时聊天”标题猜测，
  // 避免误点“关闭临时聊天”。
  const confirmTemporaryChat = async () => {
    const candidates = allMatches(SELECTORS.temporaryChatConfirmButton).filter(visible);
    const button = candidates.find((element) => {
      const label = [
        element.getAttribute('aria-label'),
        element.getAttribute('data-testid'),
        textOf(element),
      ].filter(Boolean).join(' ').trim();
      return /^(?:continue|继续)$/iu.test(label)
        || /(?:^|[^a-z])continue(?:$|[^a-z])/iu.test(label);
    });
    if (!button) return false;
    button.click();
    await sleep(120);
    return true;
  };

  const BLOCK_TAGS = new Set(['ADDRESS', 'ARTICLE', 'ASIDE', 'BLOCKQUOTE', 'DIV', 'DL', 'FIELDSET', 'FIGURE', 'FOOTER', 'FORM', 'H1', 'H2', 'H3', 'H4', 'H5', 'H6', 'HEADER', 'HR', 'LI', 'MAIN', 'NAV', 'OL', 'P', 'PRE', 'SECTION', 'TABLE', 'TR', 'UL']);
  const textWithLineBreaks = (root) => {
    const walk = (node) => {
      if (!node) return '';
      if (node.nodeType === 3) return node.nodeValue || '';
      if (node.nodeType !== 1 || node.getAttribute('aria-hidden') === 'true') return '';
      if (node.tagName === 'BR') return '\n';
      const body = [...node.childNodes].map(walk).join('');
      return BLOCK_TAGS.has(node.tagName) ? '\n' + body + '\n' : body;
    };
    return walk(root).replace(/\n{3,}/g, '\n\n').replace(/^\n+|\n+$/g, '');
  };
  const assistantBody = (element) => {
    if (!element) return '';
    const contentCandidates = allMatches([
      '[data-message-content]',
      '[data-markdown-text-style="assistant-message"]',
      '[data-testid="conversation-turn-content"]',
      'div.markdown',
    ], element)
      .filter(exposed)
      .map((candidate) => ({
        candidate,
        text: textWithLineBreaks(candidate),
      }))
      .filter((entry) => entry.text.trim().length > 0);

    // ChatGPT's current renderer can expose the speaker heading and the actual
    // markdown body through multiple matching descendants.  Taking the first
    // match (the old behavior) may therefore return only "ChatGPT 说：" even
    // though the response is already visible below it.  All candidates above
    // are content selectors, not action/toolbars; choose the longest textual
    // body so a heading-only match cannot mask the real answer.  Keep the
    // assistant turn itself as the fallback for a renderer that does not emit
    // one of these content markers.
    if (contentCandidates.length > 0) {
      contentCandidates.sort((left, right) => right.text.length - left.text.length);
      return contentCandidates[0].text;
    }
    return textWithLineBreaks(element);
  };

  const turnState = () => {
    const blocked = allMatches(SELECTORS.blockPage).some(exposed);
    const conversation = firstVisible(SELECTORS.conversation);
    const root = conversation || document;
    const composer = firstComposer();
    const users = allMatches(SELECTORS.userMessage, root);
    const assistants = allMatches(SELECTORS.assistantMessage, root);
    const reasoning = allMatches(SELECTORS.reasoningBlock, root);
    const lastAssistant = assistants.length > 0 ? assistants[assistants.length - 1] : null;
    const lastReasoning = reasoning.length > 0 ? reasoning[reasoning.length - 1] : null;
    return {
      origin: location.origin,
      path: location.pathname + location.search,
      blocked,
      conversation_found: Boolean(conversation),
      // 生成中只认 exposed 的停止按钮 / 流式状态；停止按钮存在但 hidden 的
      // 版本不能把整轮永久判定为生成中。
      generating: allMatches(SELECTORS.generatingControl).some(exposed),
      composer_found: Boolean(composer),
      user_message_count: users.length,
      assistant_message_count: assistants.length,
      assistant_text: assistantBody(lastAssistant),
      thinking_text: lastReasoning ? textWithLineBreaks(lastReasoning) : '',
      account_hint: accountHint(),
    };
  };

  const cancelGeneration = async () => {
    const stop = firstVisible(SELECTORS.stopButton);
    if (!stop) return false;
    stop.click();
    await sleep(60);
    return !allMatches(SELECTORS.generatingControl).some(exposed);
  };

  const writeTextWithTemporaryChat = async (input) => {
    await confirmTemporaryChat();
    return writeText(input);
  };

  globalThis.__magiWebModel = {
    adapter_epoch: adapterEpoch,
    revision: REVISION,
    probe,
    writeText: writeTextWithTemporaryChat,
    observe,
    submit,
    turnState,
    cancelGeneration,
  };
})(${JSON.stringify(WEB_MODEL_SITE_REVISION)});
`;

/** 页面脚本回传的原始回合状态（只有 DOM 事实）。 */
export interface WebModelRawTurnState {
  origin: string;
  path: string;
  blocked: boolean;
  generating: boolean;
  conversation_found: boolean;
  composer_found: boolean;
  user_message_count: number;
  assistant_message_count: number;
  assistant_text: string;
  thinking_text: string;
  account_hint: string | null;
}

/**
 * 归一化后的回合状态（对应 contracts 的 `BrowserWebTurnStateResult`）。
 *
 * 站点适配层只给**原始语义信号**（是否生成中、助手文本、消息计数）；
 * 「文本稳定且无生成标志」的最终完成谓词由推理通道叠加两个读取周期判定，
 * 因为稳定性需要跨轮比较，而站点层只做单次快照（设计基线 §5.6 步骤 6）。
 */
export interface WebModelTurnState {
  siteRevision: string;
  loginState: "signed_in" | "signed_out" | "blocked";
  blocked: boolean;
  generating: boolean;
  composerFound: boolean;
  userMessageCount: number;
  assistantMessageCount: number;
  assistantText: string;
  thinkingText: string;
}

/** 纯函数归一化：只吃页面原始快照，不碰 DOM。 */
export function normalizeWebModelTurnState(
  snapshot: WebModelRawTurnState,
): WebModelTurnState {
  const loginState: WebModelTurnState["loginState"] = snapshot.blocked
    ? "blocked"
    : isSupportedChatGptOrigin(snapshot.origin)
      && snapshot.conversation_found
      && snapshot.composer_found
      ? "signed_in"
      : "signed_out";
  return {
    siteRevision: WEB_MODEL_SITE_REVISION,
    loginState,
    blocked: snapshot.blocked,
    generating: snapshot.generating,
    composerFound: snapshot.composer_found,
    userMessageCount: snapshot.user_message_count,
    assistantMessageCount: snapshot.assistant_message_count,
    assistantText: snapshot.assistant_text,
    thinkingText: snapshot.thinking_text,
  };
}

/** 归一化后的探测结果（对应 contracts 的 `BrowserWebModelProbe`）。 */
export interface WebModelProbe {
  siteRevision: string;
  loginState: "signed_in" | "signed_out" | "blocked";
  composerAvailable: boolean;
  composerCharLimit: number | null;
  accountHint: "plus" | "pro" | "free" | "unknown";
  models: Array<{ family: string; displayName: string; efforts: string[]; defaultEffort: string | null }>;
  connectorSupport: boolean;
  /** 站点事实不足以安全发现模型时的内部原因；运行时会 fail closed。 */
  diagnostic: WebModelProbeDiagnostic;
}

/** 已知的模型族标记：标签里出现即视为该族。 */
const MODEL_FAMILY_LABELS: Array<{ family: string; match: RegExp }> = [
  { family: "gpt-5.6-sol", match: /gpt-?5\.6\s*sol/iu },
  { family: "gpt-5.5", match: /gpt-?5\.5/iu },
  { family: "gpt-5-thinking", match: /gpt-?5\s*(thinking|思考)/iu },
  { family: "gpt-5-pro", match: /gpt-?5\s*pro/iu },
  { family: "gpt-5", match: /gpt-?5/iu },
  { family: "gpt-4.1", match: /gpt-?4\.1/iu },
  { family: "gpt-4o", match: /gpt-?4o/iu },
  { family: "o3", match: /\bo3\b/iu },
  { family: "o4-mini", match: /\bo4-?mini\b/iu },
];

const KNOWN_EFFORTS = ["low", "medium", "high", "xhigh", "max"];

/** 适配层允许向上层报告的 effort 取值；实际集合必须来自页面事实。 */
export function normalizeModelFamily(label: string): string | null {
  const normalized = label.replace(/\s+/gu, " ").trim();
  if (!normalized) return null;
  // ChatGPT 的免费/基础账号当前会把可选项显示为“ChatGPT + 描述”，
  // 不带数字或分隔符。这个选项是真正可选的模型，不应被当成站点漂移；
  // 升级入口（如“ChatGPT Business”）则明确排除，避免把升级卡片错误
  // 投影成可调用模型。
  if (/^chatgpt(?:\s+business|\s+plus|\s+pro|\s+enterprise|\s+upgrade|\s+升级)\b/iu.test(normalized)) {
    return null;
  }
  if (/^chatgpt(?:$|\s)/iu.test(normalized)) return "chatgpt";
  for (const entry of MODEL_FAMILY_LABELS) {
    if (entry.match.test(normalized)) return entry.family;
  }
  // 模型菜单是 ChatGPT 的服务端数据，不能因为新模型尚未进入 Magi 的
  // 上限表就把它静默丢掉。对明显属于模型名称的标签保留一个稳定 slug；
  // 上限表随后按账号默认值回退，直到阶段 0 标定补上更精确的族记录。
  // 普通菜单文案不符合这些前缀，仍然视为 selector/映射异常而拒绝投影。
  if (/^(?:gpt|codex)(?:[-\s.]|\d)/iu.test(normalized)) {
    return normalized
      .toLowerCase()
      .replace(/[^a-z0-9]+/gu, "-")
      .replace(/^-+|-+$/gu, "");
  }
  if (/^o\d(?:[-\s.]|$)/iu.test(normalized)) {
    return normalized
      .toLowerCase()
      .replace(/[^a-z0-9]+/gu, "-")
      .replace(/^-+|-+$/gu, "");
  }
  return null;
}

/**
 * 纯函数归一化：只吃页面原始快照，不碰 DOM（fixture 单测的入口）。
 *
 * 关键不变量（设计基线 §5.5）：**登录态需要会话有效与 composer 可用两项证据**；
 * 未登录 / 过期时 `loginState = signed_out`，调用方必须返回 `engines = []`。
 */
export function normalizeWebModelProbe(snapshot: WebModelRawSnapshot): WebModelProbe {
  const originSupported = isSupportedChatGptOrigin(snapshot.origin);
  const loginState: WebModelProbe["loginState"] = snapshot.blocked
    ? "blocked"
    : originSupported && snapshot.conversationFound && snapshot.composerFound
      ? "signed_in"
      : "signed_out";
  const accountHint = ((): WebModelProbe["accountHint"] => {
    const value = (snapshot.accountHint ?? "").toLowerCase();
    if (value === "plus" || value === "pro" || value === "free") return value;
    return "unknown";
  })();
  const modelsByFamily = new Map<string, WebModelProbe["models"][number]>();
  if (loginState === "signed_in") {
    for (const item of snapshot.modelItems) {
      const family = normalizeModelFamily(item.label)
        ?? (item.modelId ? normalizeModelFamily(item.modelId) : null);
      if (!family) continue;
      const efforts = KNOWN_EFFORTS.filter((effort) => item.efforts
        .some((candidate) => candidate.trim().toLowerCase() === effort));
      const current = modelsByFamily.get(family);
      if (!current) {
        const displayName = item.label.trim() || item.modelId?.trim() || family;
        const defaultEffort = item.defaultEffort?.trim().toLowerCase() ?? null;
        modelsByFamily.set(family, {
          family,
          displayName,
          efforts,
          defaultEffort: item.checked
            ? defaultEffort && efforts.includes(defaultEffort)
              ? defaultEffort
              : null
            : null,
        });
        continue;
      }
      for (const effort of efforts) {
        if (!current.efforts.includes(effort)) current.efforts.push(effort);
      }
      current.efforts = KNOWN_EFFORTS.filter((effort) => current.efforts.includes(effort));
      if (current.defaultEffort === null && item.checked
        && item.defaultEffort
        && current.efforts.includes(item.defaultEffort.trim().toLowerCase())) {
        current.defaultEffort = item.defaultEffort.trim().toLowerCase();
      }
    }
  }
  const models = [...modelsByFamily.values()];
  const diagnostic: WebModelProbeDiagnostic = !originSupported
    ? "site_origin_unsupported"
    : snapshot.blocked
      ? null
      : !snapshot.conversationFound
        ? "conversation_selector_missing"
        : !snapshot.composerFound
          ? "composer_selector_missing"
          : !snapshot.modelMenuFound
            ? "model_menu_selector_missing"
            : models.length === 0
              ? "model_items_unrecognized"
              : null;
  return {
    siteRevision: WEB_MODEL_SITE_REVISION,
    loginState,
    composerAvailable: originSupported && snapshot.composerFound,
    composerCharLimit: snapshot.composerCharLimit,
    accountHint,
    models,
    connectorSupport: snapshot.connectorSettingsFound,
    diagnostic,
  };
}
