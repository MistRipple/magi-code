import { createHash } from "node:crypto";

/**
 * ChatGPT Web 站点适配层：**DOM 事实的唯一归属地**。
 *
 *
 * 分层：
 * - 页面脚本（`INSTALL_WEB_MODEL_ADAPTER`）只做 DOM 读取与写入，把结果整理成
 *   **原始快照**（`WebModelRawSnapshot` / 写入回读结果），不含任何业务判断；
 * - 归一化（`normalizeWebModelProbe`）是纯函数，只吃原始快照，不碰 DOM，
 *   因此可以用录制的快照 fixture 做单测来控制 selector 漂移。
 *
 * 这里不实现任何推理通道语义（槽位、存活判定、发送与接收的编排）：那些在
 * `crates/magi-web-model`，两半各只有一处实现。本层也不读取或复制网页的模型菜单：
 * 模型与强度由用户在网页里自己选（W18）。
 */

/** 站点适配层结构版本。selector 集变化即推进，用于诊断与漂移判定。 */
export const WEB_MODEL_SITE_REVISION = "chatgpt-web-4";

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
  /** 侧栏历史里的已保存对话链接（`/c/<conversation_id>`）。 */
  historyLink: [
    "nav a[href^='/c/']",
    "a[data-sidebar-item][href^='/c/']",
    "[data-testid^='history-item'] a[href^='/c/']",
    "a[href^='/c/']",
  ],
  /** 个人资料菜单 / 设置入口 / 设置里的连接器分区（连接器自动配置只读写这几处）。 */
  profileButton: [
    "[data-testid='accounts-profile-button']",
    "button[aria-label*='个人资料']",
    "button[aria-label*='profile' i]",
  ],
  settingsMenuItem: ["[data-testid='settings-menu-item']", "[role='menuitem']"],
  settingsTab: ["[role='dialog'] [role='tab']", "[role='dialog'] button", "[role='dialog'] a"],
  dialog: ["[role='dialog']", "[data-testid='modal-settings']"],
  /** 风险 / 验证页标志。 */
  blockPage: ["#challenge-form", "iframe[src*='challenges.cloudflare.com']", "[data-testid='blocked-page']"],
  /** 附件 chip：出现即说明输入被转成了附件。 */
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
    // 图片回复没有 unit 包装：助手标题 h4 之后的兄弟节点里直接是图片画廊。
    "h4[data-conversation-role='assistant'] ~ div:has([data-testid='generated-image-gallery'])",
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
  /** composer 是否可达（登录态的第二项证据）。 */
  composerFound: boolean;
  /** 是否读到会话主区域。 */
  conversationFound: boolean;
  /** 账号等级提示（Plus / Pro / Free），读不到为 null。 */
  accountHint: string | null;
  /** 页面上有账号入口且没有登录入口：在没有输入框的二级页面（插件、设置）上证明会话仍然有效。 */
  accountPresent?: boolean;
  /** 站点会话接口的结论：true 已登录、false 未登录、null/缺失 = 没拿到答案。登录态以它为准。 */
  sessionActive?: boolean | null;
}

export type WebModelProbeDiagnostic =
  | "site_origin_unsupported"
  | "composer_selector_missing"
  | "conversation_selector_missing"
  | null;

/**
 * ChatGPT Web 的受控页面 origin。
 *
 * 产品默认只允许官方 origin。自动化验收通过同一进程里的
 * `MAGI_WEB_MODEL_ORIGIN` 把站点替换为 loopback fixture；仅当该覆盖值本身是
 * loopback origin 且与当前页面完全相等时放行，避免把任意外部站点变成 Web 模型
 * 来源。
 */
/** GPT Web 标签页顶部快捷地址允许去的 OpenAI 平台页面所在 origin。 */
export function isOpenAiPlatformOrigin(origin: string): boolean {
  return origin === "https://platform.openai.com";
}

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
 * 写入 / 回读共用的归一化。
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
 * 因此写入与回读必然使用同一算法。
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
  // 。
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
  // 富文本输入框（ProseMirror）一行一个段落。不能用 innerText 回读：输入框会把文本里的 URL
  // 自动变成「链接徽章」控件，innerText 会在徽章前后插入换行，段落之间还会多一个空行，
  // 使带链接 / 多行的消息回读与写入不一致。这里按段落读取文本节点，跳过不可编辑 / 隐藏的装饰控件。
  const editorText = (root) => {
    const lines = [];
    let line = '';
    const flush = () => {
      lines.push(line);
      line = '';
    };
    const isBlock = (node) => /^(?:P|DIV|LI|PRE|BLOCKQUOTE|UL|OL|H[1-6])$/.test(node.tagName);
    const walk = (node) => {
      if (node.nodeType === 3) {
        line += node.nodeValue || '';
        return;
      }
      if (node.nodeType !== 1) return;
      if (node.getAttribute('aria-hidden') === 'true' || node.getAttribute('contenteditable') === 'false') return;
      if (node.tagName === 'BR') {
        // 空段落里只有占位 <br>，不代表换行；段落内的软换行才是 \n。
        if (node.classList.contains('ProseMirror-trailingBreak')) return;
        const parent = node.parentElement;
        const onlyChild = parent && parent.childNodes.length === 1;
        if (!onlyChild) line += '\n';
        return;
      }
      const block = isBlock(node);
      if (block && line !== '') flush();
      for (const child of node.childNodes) walk(child);
      if (block) flush();
    };
    for (const child of root.childNodes) walk(child);
    if (line !== '') flush();
    return lines.join('\n');
  };
  const composerText = (element) => {
    if (!element) return '';
    if (element.tagName === 'TEXTAREA' || element.tagName === 'INPUT') return element.value || '';
    return editorText(element);
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
    // 页面会渲染多个资料入口（折叠侧栏里的只有头像骨架），等级文字只在带用户名的那一行。
    // 只读资料按钮所在行里「文本恰好等于套餐名」的叶子节点：用户名、对话内容里出现的
    // free / pro 字样不能参与判断；隐藏的侧栏同样要读，所以用 textContent 而不是 innerText。
    const plans = { free: 'free', plus: 'plus', pro: 'pro', '免费版': 'free' };
    const buttons = document.querySelectorAll(
      'button[aria-label*="个人资料"], button[aria-label*="profile" i]',
    );
    for (const button of buttons) {
      const row = button.parentElement?.parentElement;
      if (!row) continue;
      for (const leaf of row.querySelectorAll('span, div')) {
        if (leaf.childElementCount > 0) continue;
        const plan = plans[(leaf.textContent || '').trim().toLowerCase()];
        if (plan) return plan;
      }
    }
    return null;
  };

  // 站点自己的会话接口：同源、只读，只看有没有 user 字段——登录态来自会话，与当前停在哪个页面无关。
  // 响应里的访问令牌之类字段一概不读、不保存、不回传。读不到（网络 / 非 200）返回 null，
  // 由下面的页面证据兜底，不把“没拿到答案”当成“未登录”。
  const sessionActive = async () => {
    const controller = typeof AbortController === 'function' ? new AbortController() : null;
    const timer = controller ? setTimeout(() => controller.abort(), 4000) : null;
    try {
      const response = await fetch('/api/auth/session', {
        credentials: 'same-origin',
        cache: 'no-store',
        headers: { accept: 'application/json' },
        signal: controller?.signal,
      });
      if (!response.ok) return null;
      const body = await response.json();
      return Boolean(body && typeof body === 'object' && body.user);
    } catch (error) {
      return null;
    } finally {
      if (timer) clearTimeout(timer);
    }
  };

  // 会话接口不可用时的兜底证据：有账号入口且没有登录入口。
  const accountPresent = () => {
    const loginEntry = allMatches(['[data-testid="login-button"]', 'a[href*="/auth/login"]']).some(exposed)
      || allMatches(['button', 'a']).filter(visible).some((element) => (
        /^(?:登录|Log in|Sign in)$/iu.test(textOf(element))
      ));
    return !loginEntry && allMatches(SELECTORS.profileButton).length > 0;
  };

  // 只读探测：登录与页面可用性。不打开任何菜单、不读取模型列表（W18）。
  const probe = async () => {
    const composer = firstComposer();
    const blocked = allMatches(SELECTORS.blockPage).some(exposed);
    const conversation = firstVisible(SELECTORS.conversation);
    return {
      origin: location.origin,
      path: location.pathname + location.search,
      blocked,
      composerFound: Boolean(composer),
      conversationFound: Boolean(conversation),
      accountHint: accountHint(),
      accountPresent: accountPresent(),
      sessionActive: await sessionActive(),
    };
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
      // 写入后页面框架要过一小会儿才会放开发送按钮（刚打开页面时尤其明显）；
      // 短暂等待，仍禁用才算不可用。
      const unavailable = () => button.disabled || button.getAttribute('aria-disabled') === 'true';
      for (let waited = 0; unavailable() && waited < 1500; waited += 100) await sleep(100);
      if (unavailable()) {
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
      // 仅供读屏器的隐藏标题（例如 sr-only 的 h4「ChatGPT 说：」）不是回复内容。
      if (node.classList && (node.classList.contains('sr-only') || node.classList.contains('visually-hidden'))) return '';
      if (node.tagName === 'BR') return '\n';
      const body = [...node.childNodes].map(walk).join('');
      return BLOCK_TAGS.has(node.tagName) ? '\n' + body + '\n' : body;
    };
    return walk(root).replace(/\n{3,}/g, '\n\n').replace(/^\n+|\n+$/g, '');
  };

  // ── 助手回复：DOM → Markdown ─────────────────────────────────────────────────
  // ChatGPT 的回复是渲染后的 DOM（标题 / 列表 / 表格 / 代码块 / 引用 / 链接）。Magi 的会话是 Markdown，
  // 所以这里还原成 Markdown，而不是压平成纯文本（那样表格单元格会粘在一起、列表丢序号、链接只剩文字）。
  // 只依赖 nodeType / tagName / childNodes / getAttribute / hasAttribute / classList / textContent。
  const TICK = String.fromCharCode(96);
  const MD_SKIP_TAGS = new Set(['SCRIPT', 'STYLE', 'SVG', 'NOSCRIPT', 'TEMPLATE', 'INPUT', 'TEXTAREA', 'SELECT']);
  const MD_BLOCK_TAGS = new Set(['P', 'UL', 'OL', 'LI', 'BLOCKQUOTE', 'PRE', 'TABLE', 'HR', 'DIV', 'SECTION', 'ARTICLE', 'ASIDE', 'FIGURE', 'DETAILS', 'SUMMARY', 'DL', 'DT', 'DD', 'HEADER', 'FOOTER', 'MAIN', 'NAV', 'FIELDSET']);
  const tagOf = (node) => String(node.tagName || '').toUpperCase();
  const mdSkipped = (node) => {
    if (node.nodeType !== 1) return false;
    if (MD_SKIP_TAGS.has(tagOf(node))) return true;
    // 按钮都是操作入口（复制 / 分享 / 编辑），唯独图片预览按钮里装着生成的图片本身。
    if (tagOf(node) === 'BUTTON' && node.getAttribute('data-testid') !== 'generated-image-preview') return true;
    if (node.getAttribute('aria-hidden') === 'true' || node.hasAttribute('hidden')) return true;
    // 复制按钮、代码块头部、表格操作栏：ChatGPT 自己标注「复制时排除」的装饰区域。
    if (node.getAttribute('data-markdown-copy') === 'exclude') return true;
    const cls = node.classList;
    return Boolean(cls && (cls.contains('sr-only') || cls.contains('visually-hidden')));
  };
  const mdEscape = (text) => text.replace(/[\\*_\x60\[\]<~]/g, '\\$&');
  const mdGuardLineStart = (text) => text.replace(/^(\s*)(#{1,6}\s|[-+]\s|>|\d+[.)]\s)/gm, '$1\\$2');
  const mdWrap = (mark, inner) => {
    const parts = /^(\s*)([\s\S]*?)(\s*)$/.exec(inner);
    if (!parts || !parts[2]) return inner;
    return parts[1] + mark + parts[2] + mark + parts[3];
  };
  const mdCodeSpan = (raw) => {
    const text = raw.replace(/\s*\n\s*/g, ' ');
    if (!text) return '';
    const longest = (text.match(/\x60+/g) || []).reduce((max, run) => Math.max(max, run.length), 0);
    const fence = TICK.repeat(longest + 1);
    const pad = /^\x60|\x60$/.test(text) ? ' ' : '';
    return fence + pad + text + pad + fence;
  };
  const mdCleanUrl = (href) => {
    try {
      const url = new URL(href, location.origin);
      if (url.protocol !== 'http:' && url.protocol !== 'https:' && url.protocol !== 'mailto:') return null;
      // ChatGPT 给外链追加的来源标记不是链接内容的一部分。
      if (url.searchParams.get('utm_source') === 'chatgpt.com') url.searchParams.delete('utm_source');
      return url.toString();
    } catch (error) {
      return null;
    }
  };
  const mdLink = (node, inner) => {
    const href = node.getAttribute('href') || '';
    const url = mdCleanUrl(href);
    if (!url) return inner;
    const label = inner.replace(/\s*\n\s*/g, ' ').trim();
    const plain = (node.textContent || '').trim();
    if (!label || plain === href || plain === url || mdCleanUrl(plain) === url) return '<' + url + '>';
    const target = url.replace(/[()\s]/g, (char) => (char === '(' ? '%28' : char === ')' ? '%29' : '%20'));
    return '[' + label + '](' + target + ')';
  };
  // 公式：站点把 TeX 源码放在 data-math-source 上，其下是 MathML 和排版用的 HTML 两份渲染，
  // 两份都不能进正文（否则公式会重复成乱码）。统一还原为 $...$ / $$...$$。
  const mathSource = (node) => {
    const source = node.getAttribute('data-math-source');
    return typeof source === 'string' && source.trim() ? source.trim() : null;
  };
  // 图片：站点生成的图是 blob: 地址（只在页面里有效），由 daemon 之后按地址读出字节并落到工作区。
  // 站点图标 / 来源小图（alt 为空且很小）不是回复内容。
  const mdImage = (node) => {
    const width = Number(node.naturalWidth);
    if (Number.isFinite(width) && width > 0 && width < 100) return '';
    // 还没加载完的 blob 图（画廊骨架）不是回复内容。
    if (Number.isFinite(width) && width === 0 && /^blob:/.test(node.getAttribute('src') || '')) return '';
    const parent = node.parentElement;
    const alt = ((node.getAttribute('alt') || '').trim()
      || (parent && parent.getAttribute('aria-label')) || '').replace(/[\[\]\n]/g, ' ').trim();
    if (!alt && !(Number.isFinite(width) && width >= 100)) return '';
    const src = (node.getAttribute('src') || '').trim();
    if (!/^(?:blob:|https?:)/.test(src)) return '';
    const target = src.replace(/[()\s]/g, (char) => (char === '(' ? '%28' : char === ')' ? '%29' : '%20'));
    return '![' + (alt || '图片') + '](' + target + ')';
  };
  const mdImagesIn = (root) => {
    const found = [];
    const walk = (node) => {
      if (node.nodeType !== 1 || mdSkipped(node)) return;
      if (tagOf(node) === 'IMG') {
        const markdown = mdImage(node);
        if (markdown) found.push(markdown);
        return;
      }
      for (const child of node.childNodes) walk(child);
    };
    walk(root);
    return found;
  };
  const mdInline = (node) => {
    if (node.nodeType === 3) return mdEscape(node.nodeValue || '').replace(/[ \t\r\n]+/g, ' ');
    if (node.nodeType !== 1 || mdSkipped(node)) return '';
    const math = mathSource(node);
    if (math !== null) return '$' + math.replace(/\s*\n\s*/g, ' ') + '$';
    const tag = tagOf(node);
    if (tag === 'BR') return '\n';
    if (tag === 'IMG') return mdImage(node);
    if (tag === 'CODE' || node.getAttribute('data-markdown-copy') === 'inline-code') {
      return mdCodeSpan(node.textContent || '');
    }
    const children = () => [...node.childNodes].map(mdInline).join('');
    if (tag === 'STRONG' || tag === 'B') return mdWrap('**', children());
    if (tag === 'EM' || tag === 'I') return mdWrap('*', children());
    if (tag === 'DEL' || tag === 'S') return mdWrap('~~', children());
    if (tag === 'A') return mdLink(node, children());
    return children();
  };
  const mdInlineChildren = (node) => [...node.childNodes].map(mdInline).join('').replace(/[ \t]+\n/g, '\n').trim();
  const mdFind = (node, predicate) => {
    for (const child of node.childNodes) {
      if (child.nodeType !== 1) continue;
      if (predicate(child)) return child;
      const nested = mdFind(child, predicate);
      if (nested) return nested;
    }
    return null;
  };
  const mdCodeBlock = (node) => {
    const code = tagOf(node) === 'CODE' ? node : mdFind(node, (child) => tagOf(child) === 'CODE');
    const source = ((code || node).textContent || '').replace(/\n$/, '');
    let language = '';
    const fromClass = /language-([\w+#.-]+)/.exec((code && code.getAttribute('class')) || '');
    if (fromClass) {
      language = fromClass[1];
    } else {
      // 头部（标记为复制时排除）里的第一个 div 是语言名，例如「Python」。
      const header = mdFind(node, (child) => child.getAttribute('data-markdown-copy') === 'exclude');
      const label = header ? mdFind(header, (child) => tagOf(child) === 'DIV') : null;
      const text = ((label && label.textContent) || '').trim();
      if (/^[A-Za-z][\w+#.-]{0,30}$/.test(text)) language = text.toLowerCase();
    }
    const longest = (source.match(/\x60{3,}/g) || []).reduce((max, run) => Math.max(max, run.length), 0);
    const fence = TICK.repeat(Math.max(3, longest + 1));
    return fence + language + '\n' + source + '\n' + fence;
  };
  const mdList = (node) => {
    const ordered = tagOf(node) === 'OL';
    const startAttr = Number(node.getAttribute('start'));
    let index = Number.isSafeInteger(startAttr) && startAttr > 0 ? startAttr : 1;
    const lines = [];
    for (const child of node.childNodes) {
      if (child.nodeType !== 1 || mdSkipped(child) || tagOf(child) !== 'LI') continue;
      const marker = ordered ? index + '. ' : '- ';
      index += 1;
      const parts = mdBlocks(child);
      let body = '';
      parts.forEach((part, position) => {
        if (position === 0) body = part;
        else body += (/^(?:[-]|\d+\.) /.test(part) ? '\n' : '\n\n') + part;
      });
      const pad = ' '.repeat(marker.length);
      const rendered = body.split('\n').map((line, position) => (
        position === 0 ? marker + line : line ? pad + line : line
      )).join('\n');
      lines.push(body ? rendered : marker.trim());
    }
    return lines.join('\n');
  };
  const mdTable = (node) => {
    const rows = [];
    const collect = (parent) => {
      for (const child of parent.childNodes) {
        if (child.nodeType !== 1 || mdSkipped(child)) continue;
        const tag = tagOf(child);
        if (tag === 'TR') {
          rows.push([...child.childNodes]
            .filter((cell) => cell.nodeType === 1 && !mdSkipped(cell) && (tagOf(cell) === 'TH' || tagOf(cell) === 'TD'))
            .map((cell) => mdInlineChildren(cell).replace(/\|/g, '\\|').replace(/\s*\n\s*/g, '<br>')));
        } else if (tag === 'THEAD' || tag === 'TBODY' || tag === 'TFOOT') {
          collect(child);
        }
      }
    };
    collect(node);
    if (rows.length === 0) return '';
    const width = Math.max(...rows.map((row) => row.length));
    const fill = (row) => Array.from({ length: width }, (unused, column) => row[column] ?? '');
    const line = (row) => '| ' + fill(row).join(' | ') + ' |';
    return [line(rows[0]), line(Array.from({ length: width }, () => '---')), ...rows.slice(1).map(line)].join('\n');
  };
  const mdBlock = (node) => {
    const tag = tagOf(node);
    const math = mathSource(node);
    if (math !== null) {
      return node.getAttribute('data-math-display') === 'true' ? '$$\n' + math + '\n$$' : null;
    }
    // 生成的图片画廊：一张图一段，多张图依次排列。
    if (node.getAttribute('data-testid') === 'generated-image-gallery') return mdImagesIn(node).join('\n\n');
    if (node.getAttribute('data-markdown-copy') === 'code-block' || tag === 'PRE') return mdCodeBlock(node);
    const heading = /^H([1-6])$/.exec(tag);
    if (heading) {
      const text = mdInlineChildren(node).replace(/\s*\n\s*/g, ' ');
      return text ? '#'.repeat(Number(heading[1])) + ' ' + text : '';
    }
    if (tag === 'P') return mdGuardLineStart(mdInlineChildren(node));
    if (tag === 'UL' || tag === 'OL') return mdList(node);
    if (tag === 'TABLE') return mdTable(node);
    if (tag === 'HR') return '---';
    if (tag === 'BLOCKQUOTE') {
      const inner = mdBlocks(node).join('\n\n');
      return inner ? inner.split('\n').map((line) => (line ? '> ' + line : '>')).join('\n') : '';
    }
    if (MD_BLOCK_TAGS.has(tag)) return mdBlocks(node).join('\n\n');
    return null;
  };
  const mdBlocks = (parent) => {
    const out = [];
    let run = '';
    const flush = () => {
      const text = run.replace(/[ \t]+\n/g, '\n').trim();
      run = '';
      if (text) out.push(mdGuardLineStart(text));
    };
    for (const node of parent.childNodes) {
      if (node.nodeType === 3) {
        run += mdInline(node);
        continue;
      }
      if (node.nodeType !== 1 || mdSkipped(node)) continue;
      const block = mdBlock(node);
      if (block === null) {
        run += mdInline(node);
        continue;
      }
      flush();
      if (block) out.push(block);
    }
    flush();
    return out;
  };
  // ── 回复里的图片字节 ───────────────────────────────────────────────────────────
  // blob: 地址只在页面里有效；在页面里取出字节，按块交给 daemon（单条控制消息有上限）。
  const imageCache = new Map();
  const loadImage = (source) => {
    const cached = imageCache.get(source);
    if (cached) return cached;
    const pending = fetch(source, { credentials: 'include' }).then(async (response) => {
      if (!response.ok) throw new Error('web_image_fetch_failed:' + response.status);
      const blob = await response.blob();
      return { mime: blob.type || 'application/octet-stream', bytes: new Uint8Array(await blob.arrayBuffer()) };
    });
    // 失败的读取不留在缓存里；缓存只留最近几张。
    pending.catch(() => { if (imageCache.get(source) === pending) imageCache.delete(source); });
    imageCache.set(source, pending);
    while (imageCache.size > 8) imageCache.delete(imageCache.keys().next().value);
    return pending;
  };
  const toBase64 = (bytes) => {
    let binary = '';
    for (let index = 0; index < bytes.length; index += 0x8000) {
      binary += String.fromCharCode.apply(null, bytes.subarray(index, index + 0x8000));
    }
    return btoa(binary);
  };
  const readImage = async ({ source, offset, length }) => {
    if (typeof source !== 'string' || !/^(?:blob:|https?:)/.test(source)) throw new Error('web_image_source_invalid');
    // 只读页面上真实存在的图片，不当成任意地址的取数通道。
    const onPage = [...document.querySelectorAll('img')].some((image) => (image.currentSrc || image.src) === source);
    if (!onPage) throw new Error('web_image_not_on_page');
    const { mime, bytes } = await loadImage(source);
    const start = Math.min(Math.max(0, Number(offset) || 0), bytes.length);
    const end = Math.min(bytes.length, start + Math.min(Math.max(1, Number(length) || 786432), 786432));
    return { mime, total: bytes.length, offset: start, data_base64: toBase64(bytes.subarray(start, end)), done: end >= bytes.length };
  };
  const markdownOf = (root) => mdBlocks(root).join('\n\n').replace(/\n{3,}/g, '\n\n').trim();

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
        text: markdownOf(candidate) || textWithLineBreaks(candidate),
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
      const best = contentCandidates[0].text;
      // 图片画廊可能不在正文容器里：补上正文里没有的图片。
      const extra = mdImagesIn(element).filter((markdown) => !best.includes(markdown));
      return [best, ...extra].join('\n\n');
    }
    return markdownOf(element) || textWithLineBreaks(element);
  };

  // 站点生成图片时没有停止按钮：先出现带「编辑」按钮的画廊骨架，几秒后图片才加载出来。
  // 画廊里还有没加载完的图片，这一轮就还没结束。
  const imagesPending = (element) => {
    if (!element) return false;
    const gallery = queryAll(element, "[data-testid='generated-image-gallery']");
    if (gallery.length === 0) return false;
    const images = queryAll(element, "[data-testid='generated-image-gallery'] img");
    return images.length === 0 || images.some((image) => !image.complete || !(image.naturalWidth > 0));
  };

  const turnState = () => {
    const blocked = allMatches(SELECTORS.blockPage).some(exposed);
    const conversation = firstVisible(SELECTORS.conversation);
    const root = conversation || document;
    const composer = firstComposer();
    const users = allMatches(SELECTORS.userMessage, root);
    // 多种 selector 合并后按文档顺序排列：同一页里可能既有文字回复也有图片回复。
    const assistants = allMatches(SELECTORS.assistantMessage, root).sort((left, right) => (
      left.compareDocumentPosition(right) & Node.DOCUMENT_POSITION_FOLLOWING ? -1 : 1
    ));
    const reasoning = allMatches(SELECTORS.reasoningBlock, root);
    const lastAssistant = assistants.length > 0 ? assistants[assistants.length - 1] : null;
    const lastReasoning = reasoning.length > 0 ? reasoning[reasoning.length - 1] : null;
    const userSet = new Set(users);
    const assistantSet = new Set(assistants);
    const messages = [...new Set([...users, ...assistants])].sort((left, right) => {
      const position = left.compareDocumentPosition(right);
      return position & Node.DOCUMENT_POSITION_FOLLOWING ? -1 : 1;
    });
    const lastMessage = messages.length > 0 ? messages[messages.length - 1] : null;
    const lastMessageRole = lastMessage
      ? userSet.has(lastMessage)
        ? 'user'
        : assistantSet.has(lastMessage)
          ? 'assistant'
          : null
      : null;
    return {
      origin: location.origin,
      path: location.pathname + location.search,
      blocked,
      conversation_found: Boolean(conversation),
      // 生成中只认 exposed 的停止按钮 / 流式状态；停止按钮存在但 hidden 的
      // 版本不能把整轮永久判定为生成中。
      generating: allMatches(SELECTORS.generatingControl).some(exposed) || imagesPending(lastAssistant),
      composer_found: Boolean(composer),
      user_message_count: users.length,
      assistant_message_count: assistants.length,
      assistant_text: assistantBody(lastAssistant),
      thinking_text: lastReasoning ? textWithLineBreaks(lastReasoning) : '',
      last_message_role: lastMessageRole,
      last_message_text: lastMessage
        ? lastMessageRole === 'assistant'
          ? assistantBody(lastMessage)
          : textWithLineBreaks(lastMessage)
        : null,
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

  // ── 已保存对话（只读列表 / 远端事实 / 精确删除）──────────────────────────────
  const CONVERSATION_ID = /^\/c\/([A-Za-z0-9-]{8,})(?:[/?#]|$)/;
  const conversationIdFromHref = (href) => {
    try {
      const url = new URL(href, location.origin);
      const match = CONVERSATION_ID.exec(url.pathname);
      return match ? match[1] : null;
    } catch (error) {
      return null;
    }
  };
  const historyAnchors = () => allMatches(SELECTORS.historyLink)
    .map((anchor) => ({ anchor, id: conversationIdFromHref(anchor.getAttribute('href') || '') }))
    .filter((entry) => entry.id !== null);

  const savedConversations = () => {
    const seen = new Set();
    const conversations = [];
    for (const { anchor, id } of historyAnchors()) {
      if (seen.has(id)) continue;
      seen.add(id);
      conversations.push({ conversation_id: id, title: textOf(anchor), updated_at: null });
      if (conversations.length >= 200) break;
    }
    return { conversations };
  };

  const savedMessages = () => {
    const conversationId = conversationIdFromHref(location.pathname);
    const nodes = allMatches([
      "[data-message-author-role='user']",
      "[data-message-author-role='assistant']",
    ]);
    const messages = nodes.map((node) => {
      const role = node.getAttribute('data-message-author-role') === 'assistant' ? 'assistant' : 'user';
      const holder = node.closest('[data-message-id]') || node;
      return {
        role,
        text: role === 'assistant' ? assistantBody(node) : textWithLineBreaks(node),
        remote_id: holder.getAttribute('data-message-id') || null,
      };
    });
    const active = historyAnchors().find((entry) => entry.id === conversationId);
    const title = (active ? textOf(active.anchor) : '')
      || String(document.title || '').replace(/\s*[|\-–—]\s*ChatGPT\s*$/u, '').trim()
      || null;
    return {
      conversation_id: conversationId,
      title,
      messages,
      last_message_id: messages.length > 0 ? messages[messages.length - 1].remote_id : null,
      updated_at: null,
    };
  };

  const labelOf = (element) => [
    element?.getAttribute('aria-label'),
    element?.getAttribute('data-testid'),
    textOf(element),
  ].filter(Boolean).join(' ').trim();

  // ── 连接器（只读状态 / 创建并回读）──────────────────────────────────────────
  // 连接器入口随套餐与站点改版变化很大：找不到入口一律返回 supported=false，
  // 由上层显示“缺哪一项”，绝不猜测页面结构去点别的东西。
  // ── 连接器（只读状态 / 在当前页面创建）─────────────────────────────────────
  // 页面脚本**不做任何跨页面导航**：命令执行期间页面路由一旦换代，Desktop 会把这条命令的结果判为
  // 过期（browser_surface_stale）。去哪个页面由 daemon 用 Host 的导航命令负责（设置里的「插件」页
  // /settings/plugins-settings 读取已安装列表，/plugins 目录页创建 MCP 应用），脚本只在已经打开
  // 的页面里读或点。找不到入口一律返回具体原因，绝不猜测页面结构去点别的东西。

  // Radix 菜单 / 对话框只响应完整的指针事件序列；单独的 click() 不生效。
  const press = (element) => {
    const rect = element.getBoundingClientRect();
    const init = {
      bubbles: true, cancelable: true, button: 0, pointerId: 1, pointerType: 'mouse', isPrimary: true,
      clientX: rect.x + rect.width / 2, clientY: rect.y + rect.height / 2,
    };
    for (const type of ['pointerdown', 'mousedown', 'pointerup', 'mouseup', 'click']) {
      element.dispatchEvent(type.startsWith('pointer') ? new PointerEvent(type, init) : new MouseEvent(type, init));
    }
  };
  const waitFor = async (find, timeoutMs) => {
    for (let waited = 0; waited <= timeoutMs; waited += 100) {
      const found = find();
      if (found) return found;
      await sleep(100);
    }
    return null;
  };
  const buttonWithText = (root, pattern) => allMatches(['button', '[role="button"]', 'a'], root)
    .filter(visible)
    .find((candidate) => pattern.test(textOf(candidate)));
  const setFieldValue = (input, value) => {
    const setter = Object.getOwnPropertyDescriptor(input.constructor.prototype, 'value')?.set;
    if (setter) setter.call(input, value); else input.value = value;
    input.dispatchEvent(new Event('input', { bubbles: true }));
    input.dispatchEvent(new Event('change', { bubbles: true }));
  };
  // 已安装列表的行没有稳定的角色或 test id：先找文本恰好等于应用名的叶子节点（标题），
  // 再取它所在的行（带副标题 / 开关的外层容器）。名称必须完全相等，不做包含匹配。
  const connectorEntry = (root, name) => {
    const wanted = name.trim().toLowerCase();
    const title = allMatches(['*'], root)
      .filter((element) => element.childElementCount === 0 && visible(element))
      .find((element) => textOf(element).toLowerCase() === wanted);
    if (!title) return null;
    return title.closest('[role="listitem"], li, a, button') || title.parentElement?.parentElement || title;
  };
  const connectorUnsupported = (reason) => ({
    supported: false, exists: false, enabled: false, tool_count: null, reason,
  });

  // 已安装的插件 / MCP 应用列表：当前页必须是设置里的「插件」页。
  const connectorStatus = async ({ name }) => {
    if (!location.pathname.startsWith('/settings/')) return connectorUnsupported('connector_page_not_open');
    const entry = await waitFor(() => connectorEntry(document.body, name), 5000);
    if (!entry) {
      // 列表还没渲染完时没有任何可见内容：此时不下“不存在”的结论。
      const rendered = (document.body.innerText || '').trim().length > 0 && document.querySelector('main, [role="main"]');
      return rendered
        ? { supported: true, exists: false, enabled: false, tool_count: null, reason: null }
        : connectorUnsupported('connector_page_not_open');
    }
    const toggle = entry.querySelector('[role="switch"], input[type="checkbox"]');
    const enabled = toggle
      ? toggle.getAttribute('aria-checked') === 'true' || toggle.checked === true
      // 已安装列表里的应用默认就是启用的；行里没有开关时，只有明确写了停用才算未启用。
      : !/(?:disabled|paused|已停用|已禁用|未启用|已暂停)/iu.test(textOf(entry));
    const countMatch = /(\\d+)\\s*(?:tools?|个?工具)/iu.exec(textOf(entry));
    return {
      supported: true,
      exists: true,
      enabled,
      tool_count: countMatch ? Number(countMatch[1]) : null,
      reason: null,
    };
  };

  // 在 /plugins 目录页创建 MCP 应用：添加 → 创建 MCP 应用 → 连接选“隧道”→ 填名称与 Tunnel id →
  // 身份验证选“无需身份验证”→ 勾选风险确认 → 创建。是否真的创建成功由 daemon 回读已安装列表确认。
  const configureConnector = async ({ name, tunnel_id: tunnelId }) => {
    const fail = (reason) => ({ configured: false, confirmed_enabled: false, reason });
    if (!location.pathname.startsWith('/plugins')) return fail('connector_page_not_open');
    const add = await waitFor(() => buttonWithText(document, /^(?:添加|Add)$/iu), 8000);
    if (!add) return fail('connector_create_entry_missing');
    press(add);
    const create = await waitFor(() => allMatches(['[role="menuitem"]']).filter(visible).find((item) => (
      /(?:创建|Create).*MCP/iu.test(textOf(item))
    )), 3000);
    if (!create) return fail('connector_create_entry_missing');
    press(create);
    const form = await waitFor(() => firstVisible(SELECTORS.dialog), 3000);
    if (!form) return fail('connector_form_fields_missing');
    const tunnelTab = buttonWithText(form, /^(?:隧道|Tunnel)$/iu);
    if (!tunnelTab) return fail('connector_tunnel_option_missing');
    press(tunnelTab);
    const tunnelField = await waitFor(() => allMatches(['input'], form).filter(visible).find((input) => (
      /(?:tunnel|隧道)/iu.test([input.getAttribute('aria-label'), input.placeholder, input.name].filter(Boolean).join(' '))
    )), 3000);
    const nameField = allMatches(['input[type="text"]', 'input:not([type])'], form).filter(visible).find((input) => (
      input !== tunnelField
    ));
    if (!nameField || !tunnelField) return fail('connector_form_fields_missing');
    setFieldValue(nameField, name);
    setFieldValue(tunnelField, tunnelId);
    const auth = allMatches(['select'], form).filter(visible)[0];
    const none = auth && [...auth.options].find((option) => /(?:无需身份验证|no authentication|none)/iu.test(option.text));
    if (!none) return fail('connector_auth_option_missing');
    setFieldValue(auth, none.value);
    const acknowledge = allMatches(['input[type="checkbox"]'], form).filter(visible)[0];
    if (!acknowledge) return fail('connector_form_fields_missing');
    if (!acknowledge.checked) press(acknowledge);
    const save = allMatches(['button[type="submit"]', 'button'], form).filter(visible).find((button) => (
      /^(?:create|创建)$/iu.test(textOf(button))
    ));
    if (!save || save.disabled) return fail('connector_save_button_missing');
    press(save);
    const closed = await waitFor(() => (firstVisible(SELECTORS.dialog) ? null : true), 10000);
    if (!closed) {
      const message = textOf(firstVisible(SELECTORS.dialog)).match(/无法[^。]*。|Unable[^.]*\\./u)?.[0];
      return fail(message ? 'connector_create_rejected:' + message.slice(0, 80) : 'connector_create_rejected');
    }
    return { configured: true, confirmed_enabled: false, reason: null };
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
    markdownOf,
    readImage,
    cancelGeneration,
    savedConversations,
    savedMessages,
    connectorStatus,
    configureConnector,
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
  last_message_role: "user" | "assistant" | null;
  last_message_text: string | null;
  account_hint: string | null;
}

/**
 * 归一化后的回合状态（对应 contracts 的 `BrowserWebTurnStateResult`）。
 *
 * 站点适配层只给**原始语义信号**（是否生成中、助手文本、消息计数）；
 * 「文本稳定且无生成标志」的最终完成谓词由推理通道叠加两个读取周期判定，
 * 因为稳定性需要跨轮比较，而站点层只做单次快照。
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
  lastMessageRole: "user" | "assistant" | null;
  lastMessageText: string | null;
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
    lastMessageRole: snapshot.last_message_role,
    lastMessageText: snapshot.last_message_text,
  };
}

/** 对话页路由：首页、`/c/<id>`、`/g/...`（自定义 GPT / 项目内对话）。 */
export function isChatRoute(path: string): boolean {
  const pathname = path.split(/[?#]/u)[0] ?? "";
  return pathname === "/" || pathname === "" || pathname.startsWith("/c/") || pathname.startsWith("/g/");
}

/**
 * 标签页停在 OpenAI 平台页面（Tunnels / API keys）时的探测结果：这不是 ChatGPT 页面，
 * 登录态与输入框都不适用，只报告页面类型，让界面和 daemon 不把它当成未登录或改版。
 */
export function openAiPlatformProbe(): WebModelProbe {
  return {
    siteRevision: WEB_MODEL_SITE_REVISION,
    loginState: "signed_out",
    composerAvailable: false,
    pageKind: "platform",
    accountHint: "unknown",
    diagnostic: null,
  };
}

/** 归一化后的探测结果（对应 contracts 的 `BrowserWebModelProbe`）：只有登录与页面可用性。 */
export interface WebModelProbe {
  siteRevision: string;
  loginState: "signed_in" | "signed_out" | "blocked";
  composerAvailable: boolean;
  /** 当前页面是否是对话页；二级页面（插件、设置）上没有输入框是正常的。 */
  pageKind: "chat" | "other" | "platform";
  accountHint: "plus" | "pro" | "free" | "unknown";
  /** 站点事实不足以判定“已登录且可用”时的内部原因；daemon 据此 fail closed。 */
  diagnostic: WebModelProbeDiagnostic;
}

/**
 * 纯函数归一化：只吃页面原始快照，不碰 DOM（fixture 单测的入口）。
 *
 * 关键不变量：**登录态来自站点会话**，与当前页面无关；`composerAvailable` 另行表示此刻能否直接发送。
 * 未登录 / 过期时 `loginState = signed_out`，GPT Web 入口随之消失。
 */
export function normalizeWebModelProbe(snapshot: WebModelRawSnapshot): WebModelProbe {
  const originSupported = isSupportedChatGptOrigin(snapshot.origin);
  const onChatPage = isChatRoute(snapshot.path);
  // 登录态以站点会话为准，不随用户停在哪个页面变化（插件、设置等二级页面没有输入框是正常的）。
  // 会话接口没拿到答案时才回到页面证据：对话页要输入框与会话区，二级页面看账号入口。
  const authenticated = typeof snapshot.sessionActive === "boolean"
    ? snapshot.sessionActive
    : onChatPage
      ? snapshot.conversationFound && snapshot.composerFound
      : snapshot.accountPresent === true;
  const loginState: WebModelProbe["loginState"] = snapshot.blocked
    ? "blocked"
    : originSupported && authenticated
      ? "signed_in"
      : "signed_out";
  const accountHint = ((): WebModelProbe["accountHint"] => {
    const value = (snapshot.accountHint ?? "").toLowerCase();
    if (value === "plus" || value === "pro" || value === "free") return value;
    return "unknown";
  })();
  const diagnostic: WebModelProbeDiagnostic = !originSupported
    ? "site_origin_unsupported"
    : snapshot.blocked || (authenticated && !onChatPage)
      ? null
      : !snapshot.conversationFound
        ? "conversation_selector_missing"
        : !snapshot.composerFound
          ? "composer_selector_missing"
          : null;
  return {
    siteRevision: WEB_MODEL_SITE_REVISION,
    loginState,
    composerAvailable: originSupported && snapshot.composerFound,
    pageKind: onChatPage ? "chat" : "other",
    accountHint,
    diagnostic,
  };
}
