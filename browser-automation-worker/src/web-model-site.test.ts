import assert from "node:assert/strict";
import test from "node:test";
import { runInNewContext } from "node:vm";
import {
  composerTextDigest,
  composerTextReadbackMatches,
  contentEditableBlockBoundaryVariant,
  firstTextDivergence,
  INSTALL_WEB_MODEL_ADAPTER,
  isSupportedChatGptOrigin,
  normalizeComposerText,
  normalizeWebModelProbe,
  normalizeWebModelTurnState,
  type WebModelRawSnapshot,
  type WebModelRawTurnState,
  isOpenAiPlatformOrigin,
  openAiPlatformProbe,
} from "./web-model-site.js";

function snapshot(overrides: Partial<WebModelRawSnapshot> = {}): WebModelRawSnapshot {
  return {
    origin: "https://chatgpt.com",
    path: "/",
    blocked: false,
    composerFound: true,
    conversationFound: true,
    accountHint: null,
    accountPresent: true,
    ...overrides,
  };
}

test("composerTextDigest 使用固定的 sha256 口径", () => {
  // 冻结算法：写入与回读共用同一个归一化与摘要，任何变动都必须显式改这条断言。
  assert.equal(
    composerTextDigest("hello"),
    "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
  );
  assert.equal(normalizeComposerText("a\r\nb\rc"), "a\nb\nc");
  // 归一化后才摘要：CRLF 与 LF 必须得到同一结果。
  assert.equal(composerTextDigest("a\r\nb"), composerTextDigest("a\nb"));
  // 富文本编辑器会把空行落成额外的块边界，回读时多出的换行不得判成写入失败。
  assert.equal(normalizeComposerText("A\n\nB"), "A\n\nB");
  assert.equal(normalizeComposerText("A\n\n\nB"), "A\n\nB");
  // 结尾块边界造成的多一个换行同样是无损排版差异。
  assert.equal(normalizeComposerText("A\n"), "A");
  assert.equal(normalizeComposerText("A\n\n"), "A");
  assert.equal(composerTextDigest("A\n\nB"), composerTextDigest("A\n\n\nB"));
  assert.equal(composerTextDigest("A\n"), composerTextDigest("A\n\n"));
  assert.notEqual(composerTextDigest("A\n\nB"), composerTextDigest("A\nB"));
  assert.equal(contentEditableBlockBoundaryVariant("A\nB"), "A\n\nB");
  assert.equal(contentEditableBlockBoundaryVariant("A\n\nB"), "A\n\nB");
  assert.equal(composerTextReadbackMatches("A\nB", "A\n\nB"), true);
  assert.equal(composerTextReadbackMatches("A\n\nB", "A\n\nB"), true);
  assert.equal(composerTextReadbackMatches("A\nB", "A  B"), false);
});

test("写入分歧诊断只暴露位置、码点和长度，不复制正文", () => {
  assert.deepEqual(firstTextDivergence("敏感a", "敏感b"), {
    index: 2,
    expectedChar: "U+0061",
    observedChar: "U+0062",
    expectedLength: 3,
    observedLength: 3,
  });
  assert.deepEqual(firstTextDivergence("abc", "ab"), {
    index: 2,
    expectedChar: "U+0063",
    observedChar: "<eof>",
    expectedLength: 3,
    observedLength: 2,
  });
});

test("站点 origin 必须是受支持的 ChatGPT Web origin", () => {
  assert.equal(isSupportedChatGptOrigin("https://chatgpt.com"), true);
  assert.equal(isSupportedChatGptOrigin("https://chat.openai.com"), true);
  assert.equal(isSupportedChatGptOrigin("https://example.test"), false);
  assert.equal(isSupportedChatGptOrigin("https://chatgpt.com.evil.test"), false);
});

test("探测只回答登录与页面可用性：两项证据都在才算已登录", () => {
  const ok = normalizeWebModelProbe(snapshot({ accountHint: "plus" }));
  assert.equal(ok.loginState, "signed_in");
  assert.equal(ok.composerAvailable, true);
  assert.equal(ok.accountHint, "plus");
  assert.equal(ok.diagnostic, null);
  // 探测结果里没有任何网页模型菜单相关字段（W18）。
  assert.deepEqual(Object.keys(ok).sort(), ["accountHint", "composerAvailable", "diagnostic", "loginState", "pageKind", "siteRevision"]);

  assert.equal(normalizeWebModelProbe(snapshot({ composerFound: false })).loginState, "signed_out");
  assert.equal(normalizeWebModelProbe(snapshot({ composerFound: false })).diagnostic, "composer_selector_missing");
  assert.equal(normalizeWebModelProbe(snapshot({ conversationFound: false })).loginState, "signed_out");
  assert.equal(normalizeWebModelProbe(snapshot({ conversationFound: false })).diagnostic, "conversation_selector_missing");
  assert.equal(normalizeWebModelProbe(snapshot({ blocked: true })).loginState, "blocked");
});

test("不受支持的 origin 永远不算登录", () => {
  const probe = normalizeWebModelProbe(snapshot({ origin: "https://chatgpt.com.evil.test" }));
  assert.equal(probe.loginState, "signed_out");
  assert.equal(probe.composerAvailable, false);
  assert.equal(probe.diagnostic, "site_origin_unsupported");
});

test("回合登录态需要 conversation 与可用 composer 两项证据", () => {
  const raw: WebModelRawTurnState = {
    origin: "https://chatgpt.com",
    path: "/?temporary-chat=true",
    blocked: false,
    generating: false,
    conversation_found: false,
    composer_found: true,
    user_message_count: 0,
    assistant_message_count: 0,
    assistant_text: "",
    thinking_text: "",
    last_message_role: null,
    last_message_text: null,
    account_hint: null,
  };
  assert.equal(normalizeWebModelTurnState(raw).loginState, "signed_out");
  assert.equal(normalizeWebModelTurnState({ ...raw, conversation_found: true }).loginState, "signed_in");
});

test("页面适配器字符串包含命令所需的原子写入、观察和回合读取接口", () => {
  for (const method of [
    "probe", "writeText", "observe", "submit", "turnState", "cancelGeneration",
    "savedConversations", "savedMessages", "connectorStatus", "configureConnector",
  ]) {
    assert.match(INSTALL_WEB_MODEL_ADAPTER, new RegExp(`\\b${method}\\b`));
  }
  assert.match(INSTALL_WEB_MODEL_ADAPTER, /found: false/u);
  assert.match(INSTALL_WEB_MODEL_ADAPTER, /timeout_ms/u);
  assert.match(INSTALL_WEB_MODEL_ADAPTER, /conversation_found/u);
  assert.match(INSTALL_WEB_MODEL_ADAPTER, /textWithLineBreaks/u);
  assert.match(INSTALL_WEB_MODEL_ADAPTER, /generating:/u);
  assert.match(INSTALL_WEB_MODEL_ADAPTER, /assistant_text:/u);
  // 页面脚本里不再有网页模型菜单的发现逻辑。
  assert.doesNotMatch(INSTALL_WEB_MODEL_ADAPTER, /modelMenu|modelItems|effortValues/u);
  // 删除只按 conversation_id 精确匹配，不按标题。
  assert.match(INSTALL_WEB_MODEL_ADAPTER, /entry\.id === conversationId/u);
});

test("适配器在未知 / 空 DOM 上安装后返回明确的未登录探测结果", async () => {
  const document = {
    documentElement: { getAttribute: () => null },
    querySelector: () => null,
    querySelectorAll: () => [],
    getElementById: () => null,
  };
  const context: Record<string, unknown> = {
    document,
    // 页面脚本里的等待在空 DOM 上会一直等到超时：测试里把所有延迟压成 0。
    setTimeout: (callback: () => void) => setTimeout(callback, 0),
    clearTimeout,
    location: { origin: "https://chatgpt.com", pathname: "/", search: "" },
    getComputedStyle: () => ({ display: "block", visibility: "visible", opacity: "1" }),
    globalThis: undefined,
  };
  context.globalThis = context;
  runInNewContext(INSTALL_WEB_MODEL_ADAPTER, context);
  const adapter = context.__magiWebModel as {
    probe: () => Promise<WebModelRawSnapshot>;
    savedConversations: () => { conversations: unknown[] };
    savedMessages: () => { conversation_id: string | null; messages: unknown[] };
    connectorStatus: (input: { name: string }) => Promise<{ supported: boolean; reason: string | null }>;
    observe: (input: { selector: string; fields: string[]; attribute: null }) => { revision: number; nodes: Array<Record<string, unknown>> };
  };
  const probe = await adapter.probe();
  assert.equal(probe.composerFound, false);
  assert.equal(probe.conversationFound, false);
  // 空 DOM：没有历史、不是已保存对话页面、没有连接器入口——都是明确的“没有”，不猜测。
  assert.deepEqual(JSON.parse(JSON.stringify(adapter.savedConversations())), { conversations: [] });
  const messages = adapter.savedMessages();
  assert.equal(messages.conversation_id, null);
  assert.deepEqual(JSON.parse(JSON.stringify(messages.messages)), []);
  assert.equal((await adapter.connectorStatus({ name: "Magi" })).supported, false);
  const observed = adapter.observe({ selector: "@assistantMessage", fields: ["existence"], attribute: null });
  assert.equal(observed.revision, 1);
  assert.equal(observed.nodes.length, 1);
  assert.equal(observed.nodes[0]?.found, false);
});

test("附件：按真实页面结构挑选多选图片输入、数出附件并识别上传中", () => {
  type FakeNode = Record<string, unknown>;
  const visibleNode = (attributes: Record<string, string>, extra: FakeNode = {}): FakeNode => ({
    hidden: false,
    getAttribute: (name: string) => attributes[name] ?? null,
    hasAttribute: (name: string) => name in attributes,
    getBoundingClientRect: () => ({ width: 20, height: 20 }),
    ...extra,
  });
  const fileInput = (id: string, attributes: Record<string, string>, multiple: boolean): FakeNode => {
    const marks = new Map<string, string>();
    return visibleNode(attributes, {
      id,
      type: "file",
      disabled: false,
      multiple,
      setAttribute: (name: string, value: string) => marks.set(name, value),
      removeAttribute: (name: string) => marks.delete(name),
      marks,
    });
  };
  // 真实页面的四个输入：拍照（单选 capture）、照片或视频、仅照片、任意文件。
  const camera = fileInput("camera", { accept: "image/*", capture: "environment" }, false);
  const photosVideos = fileInput("photos-videos", { accept: "image/*,video/*" }, true);
  const photos = fileInput("photos", { accept: "image/*" }, true);
  const anyFile = fileInput("any", {}, true);
  let removeButtons: FakeNode[] = [];
  let chips: FakeNode[] = [];
  let progress: FakeNode[] = [];
  const document = {
    documentElement: { getAttribute: () => null },
    contains: () => true,
    querySelector: () => null,
    querySelectorAll: (selector: string) => {
      if (selector === "input[type='file']") return [camera, photosVideos, photos, anyFile];
      if (selector === "[data-composer-attachments] button[aria-label]") return removeButtons;
      if (selector === "[data-composer-attachments] [role='button'].composer-attachment-surface") return chips;
      if (selector === "[data-composer-attachments] [role='progressbar']") return progress;
      return [];
    },
    getElementById: () => null,
  };
  const context: Record<string, unknown> = {
    document,
    setTimeout: (callback: () => void) => setTimeout(callback, 0),
    clearTimeout,
    location: { origin: "https://chatgpt.com", pathname: "/", search: "" },
    getComputedStyle: () => ({ display: "block", visibility: "visible", opacity: "1" }),
    globalThis: undefined,
  };
  context.globalThis = context;
  runInNewContext(INSTALL_WEB_MODEL_ADAPTER, context);
  const adapter = context.__magiWebModel as {
    attachmentState: () => { count: number; uploading: boolean };
    fileInputTarget: () => string | null;
  };

  assert.deepEqual(JSON.parse(JSON.stringify(adapter.attachmentState())), { count: 0, uploading: false });
  const selector = adapter.fileInputTarget();
  assert.equal(selector, "input[data-magi-upload-target='1']");
  // 选中的是「只收图片」的多选输入，不是拍照，也不是任意文件。
  assert.equal((photos.marks as Map<string, string>).get("data-magi-upload-target"), "1");
  assert.equal((photosVideos.marks as Map<string, string>).size, 0);
  assert.equal((camera.marks as Map<string, string>).size, 0);
  assert.equal((anyFile.marks as Map<string, string>).size, 0);

  // 上传中：每个附件一个移除按钮（文案随界面语言变，不能按文案数），缩略图上有圆形进度。
  removeButtons = [visibleNode({ "aria-label": "Remove a.png" }), visibleNode({ "aria-label": "移除 b.png" })];
  chips = [visibleNode({}), visibleNode({})];
  progress = [visibleNode({ role: "progressbar" })];
  assert.deepEqual(JSON.parse(JSON.stringify(adapter.attachmentState())), { count: 2, uploading: true });
  progress = [];
  assert.deepEqual(JSON.parse(JSON.stringify(adapter.attachmentState())), { count: 2, uploading: false });
});

test("账号等级只读资料行里文本恰好等于套餐名的叶子节点，忽略用户名和折叠栏的骨架入口", async () => {
  const leaf = (text: string) => ({ childElementCount: 0, textContent: text });
  // 折叠侧栏里的资料入口只有头像骨架；带用户名的那一行才有等级。用户名里带 free 字样不能被当成等级。
  const skeletonButton = {
    parentElement: { parentElement: { querySelectorAll: () => [leaf("正在加载个人资料")] } },
  };
  const labelledButton = {
    parentElement: {
      parentElement: { querySelectorAll: () => [leaf("free-man"), { childElementCount: 2, textContent: "free-man Plus" }, leaf("Plus")] },
    },
  };
  const probeWith = async (buttons: unknown[]) => {
    const context: Record<string, unknown> = {
      document: {
        documentElement: { getAttribute: () => null },
        querySelector: () => null,
        querySelectorAll: (selector: string) => (selector.includes("个人资料") ? buttons : []),
        getElementById: () => null,
      },
      location: { origin: "https://chatgpt.com", pathname: "/", search: "" },
      getComputedStyle: () => ({ display: "block", visibility: "visible", opacity: "1" }),
      globalThis: undefined,
    };
    context.globalThis = context;
    runInNewContext(INSTALL_WEB_MODEL_ADAPTER, context);
    return (context.__magiWebModel as { probe: () => Promise<WebModelRawSnapshot> }).probe();
  };
  assert.equal((await probeWith([skeletonButton, labelledButton])).accountHint, "plus");
  assert.equal((await probeWith([skeletonButton])).accountHint, null);
});

test("登录态以站点会话为准：停在二级页面不影响，访客态即使有输入框也不算已登录", () => {
  const secondary = normalizeWebModelProbe(snapshot({
    path: "/plugins",
    composerFound: false,
    conversationFound: false,
    sessionActive: true,
  }));
  assert.equal(secondary.loginState, "signed_in");
  assert.equal(secondary.composerAvailable, false);
  assert.equal(secondary.pageKind, "other");
  assert.equal(secondary.diagnostic, null);
  // 会话接口说未登录：二级页面、对话页（访客也有输入框）都不能算已登录。
  assert.equal(normalizeWebModelProbe(snapshot({
    path: "/plugins", composerFound: false, conversationFound: false, sessionActive: false, accountPresent: true,
  })).loginState, "signed_out");
  assert.equal(normalizeWebModelProbe(snapshot({ sessionActive: false })).loginState, "signed_out");
  // 对话页上缺输入框仍是站点改版，会话有效也不能掩盖。
  const drift = normalizeWebModelProbe(snapshot({
    path: "/", composerFound: false, conversationFound: true, sessionActive: true,
  }));
  assert.equal(drift.loginState, "signed_in");
  assert.equal(drift.composerAvailable, false);
  assert.equal(drift.diagnostic, "composer_selector_missing");
  assert.equal(drift.pageKind, "chat");
});

test("停在 OpenAI 平台页面时只报告页面类型，不当成未登录或改版", () => {
  const platform = openAiPlatformProbe();
  assert.equal(platform.pageKind, "platform");
  assert.equal(platform.composerAvailable, false);
  assert.equal(platform.diagnostic, null);
  assert.equal(isOpenAiPlatformOrigin("https://platform.openai.com"), true);
  assert.equal(isOpenAiPlatformOrigin("https://platform.openai.com.evil.example"), false);
});

test("会话接口没拿到答案时回到页面证据，不把“不知道”当成未登录", () => {
  const unknown = { sessionActive: null } as const;
  assert.equal(normalizeWebModelProbe(snapshot({ ...unknown })).loginState, "signed_in");
  assert.equal(normalizeWebModelProbe(snapshot({
    ...unknown, accountPresent: false,
  })).loginState, "signed_out", "访客输入框不能在会话接口不可用时成为登录证据");
  assert.equal(normalizeWebModelProbe(snapshot({
    ...unknown, path: "/plugins", composerFound: false, conversationFound: false, accountPresent: true,
  })).loginState, "signed_in");
  assert.equal(normalizeWebModelProbe(snapshot({
    ...unknown, path: "/plugins", composerFound: false, conversationFound: false, accountPresent: false,
  })).loginState, "signed_out");
  assert.equal(normalizeWebModelProbe(snapshot({
    ...unknown, composerFound: false,
  })).loginState, "signed_out");
});

test("适配器脚本整体可解析，连接器入口按精确名称查找", () => {
  // 页面脚本是模板字符串里的一大段代码：重复声明、反引号、转义错误都会让整个适配器装不上。
  assert.doesNotThrow(() => new Function(INSTALL_WEB_MODEL_ADAPTER));
  assert.match(INSTALL_WEB_MODEL_ADAPTER, /textOf\(element\)\.toLowerCase\(\) === wanted/u);
});

test("未知账号等级归为 unknown，不猜等级", () => {
  const probe = normalizeWebModelProbe(snapshot({ accountHint: "business" }));
  assert.equal(probe.accountHint, "unknown");
});

test("文本提取跳过仅供读屏器的隐藏标题，回复正文不带“ChatGPT 说：”", () => {
  assert.match(INSTALL_WEB_MODEL_ADAPTER, /classList\.contains\('sr-only'\)/u);
  assert.match(INSTALL_WEB_MODEL_ADAPTER, /classList\.contains\('visually-hidden'\)/u);
});

// ── DOM → Markdown ──────────────────────────────────────────────────────────

interface FakeNode {
  nodeType: number;
  tagName?: string;
  nodeValue?: string;
  childNodes: FakeNode[];
  textContent: string;
  classList: { contains: (name: string) => boolean };
  getAttribute: (name: string) => string | null;
  hasAttribute: (name: string) => boolean;
}

function text(value: string): FakeNode {
  return {
    nodeType: 3,
    nodeValue: value,
    childNodes: [],
    textContent: value,
    classList: { contains: () => false },
    getAttribute: () => null,
    hasAttribute: () => false,
  };
}

function el(tag: string, attrs: Record<string, string> = {}, ...children: Array<FakeNode | string>): FakeNode {
  const nodes = children.map((child) => (typeof child === "string" ? text(child) : child));
  const classes = (attrs.class ?? "").split(/\s+/u).filter(Boolean);
  const node = {
    nodeType: 1,
    naturalWidth: attrs.__naturalWidth === undefined ? undefined : Number(attrs.__naturalWidth),
    parentElement: null as unknown,
    tagName: tag.toUpperCase(),
    childNodes: nodes,
    get textContent() {
      return nodes.map((node) => node.textContent).join("");
    },
    classList: { contains: (name: string) => classes.includes(name) },
    getAttribute: (name: string) => attrs[name] ?? null,
    hasAttribute: (name: string) => name in attrs,
  } as FakeNode;
  for (const child of nodes) (child as unknown as { parentElement: unknown }).parentElement = node;
  return node;
}

function markdownAdapter(): (root: FakeNode) => string {
  const context: Record<string, unknown> = {
    document: {
      documentElement: { getAttribute: () => null },
      querySelector: () => null,
      querySelectorAll: () => [],
      getElementById: () => null,
    },
    location: { origin: "https://chatgpt.com", pathname: "/", search: "" },
    URL,
    getComputedStyle: () => ({ display: "block", visibility: "visible", opacity: "1" }),
    globalThis: undefined,
  };
  context.globalThis = context;
  runInNewContext(INSTALL_WEB_MODEL_ADAPTER, context);
  return (context.__magiWebModel as { markdownOf: (root: FakeNode) => string }).markdownOf;
}

test("回复还原为 Markdown：标题、行内格式、链接、列表、表格、代码块、引用、来源", () => {
  const markdownOf = markdownAdapter();
  const excluded = (tag: string, ...children: Array<FakeNode | string>) =>
    el(tag, { "data-markdown-copy": "exclude" }, ...children);
  const root = el(
    "div",
    { "data-markdown-text-style": "assistant-message" },
    el("h2", {}, el("span", {}, "格式测试")),
    el(
      "p",
      {},
      el("span", {}, "这是 "),
      el("strong", {}, el("span", {}, "粗体")),
      el("span", {}, "、"),
      el("em", {}, el("span", {}, "斜体")),
      el("span", {}, "、"),
      el("span", { "data-markdown-copy": "inline-code" }, "foo()"),
      el("span", {}, " 和 "),
      el("a", { href: "https://example.com/docs?utm_source=chatgpt.com&q=1" }, el("span", {}, "文档")),
    ),
    el(
      "ol",
      { start: "1" },
      el("li", {}, el("span", {}, "第一项")),
      el(
        "li",
        {},
        el("span", {}, "第二项"),
        el("ul", {}, el("li", {}, el("span", {}, "子项 A")), el("li", {}, el("span", {}, "子项 B"))),
      ),
      el("li", {}, el("span", {}, "第三项")),
    ),
    el(
      "div",
      { "data-markdown-table": "true" },
      el(
        "div",
        {},
        el(
          "table",
          {},
          el("thead", {}, el("tr", {}, el("th", {}, "列一"), el("th", {}, "列二"))),
          el("tbody", {}, el("tr", {}, el("td", {}, "A1"), el("td", {}, "B|1"))),
        ),
      ),
      excluded("div", el("button", {}, "复制")),
    ),
    el(
      "div",
      { "data-markdown-copy": "code-block" },
      excluded("div", el("div", {}, "Python"), el("button", {}, "复制代码")),
      el("div", {}, el("code", {}, el("span", {}, "def foo():\n    return 1\n"))),
    ),
    el("blockquote", {}, el("p", {}, "引用一句")),
    el(
      "p",
      {},
      "见来源",
      el(
        "span",
        { "data-chatgpt-copy-reference": "1" },
        el(
          "a",
          { href: "https://blog.rust-lang.org/2026/10/01/Rust-1.99.0/?utm_source=chatgpt.com" },
          el("img", { alt: "" }),
          el("span", {}, "blog.rust-lang.org"),
        ),
      ),
    ),
  );

  assert.equal(
    markdownOf(root),
    [
      "## 格式测试",
      "这是 **粗体**、*斜体*、`foo()` 和 [文档](https://example.com/docs?q=1)",
      "1. 第一项\n2. 第二项\n   - 子项 A\n   - 子项 B\n3. 第三项",
      "| 列一 | 列二 |\n| --- | --- |\n| A1 | B\\|1 |",
      "```python\ndef foo():\n    return 1\n```",
      "> 引用一句",
      "见来源[blog.rust-lang.org](https://blog.rust-lang.org/2026/10/01/Rust-1.99.0/)",
    ].join("\n\n"),
  );
});

test("回复里的字面 Markdown 符号被转义，链接文字等于地址时输出自动链接，装饰区域不进正文", () => {
  const markdownOf = markdownAdapter();
  const root = el(
    "div",
    {},
    el("p", {}, "写作 [OpenAI](", el("a", { href: "https://openai.com?utm_source=chatgpt.com" }, "https://openai.com"), ") 的 a*b_c"),
    el("p", {}, "# 不是标题"),
    el("h4", { class: "sr-only" }, "ChatGPT 说："),
    el("p", {}, el("span", { hidden: "" }, "隐藏"), el("a", { href: "javascript:alert(1)" }, "危险链接")),
  );
  assert.equal(
    markdownOf(root),
    ["写作 \\[OpenAI\\](<https://openai.com/>) 的 a\\*b\\_c", "\\# 不是标题", "危险链接"].join("\n\n"),
  );
});

test("公式还原为 TeX 源码：行内 $…$、独立 $$…$$，MathML 与排版副本不进正文", () => {
  const markdownOf = markdownAdapter();
  const rendered = (...parts: Array<FakeNode | string>) => el("span", {}, ...parts);
  const inlineMath = el(
    "span",
    { "data-math-display": "false", "data-math-source": "x^2" },
    rendered(el("math", {}, el("annotation", {}, "x^2")), rendered("x", el("span", {}, "2"))),
  );
  const displayMath = el(
    "span",
    { "data-math-display": "true", "data-math-source": "\\int_0^1 x^2\\,dx=\\frac{1}{3}" },
    rendered(el("math", {}, el("annotation", {}, "\\int_0^1 x^2\\,dx=\\frac{1}{3}")), rendered("∫01x2dx")),
  );
  const root = el(
    "div",
    {},
    el("p", {}, el("span", {}, "函数 "), inlineMath, el("span", {}, " 在 "), "区间上"),
    el("p", {}, "独立公式："),
    displayMath,
  );
  assert.equal(
    markdownOf(root),
    ["函数 $x^2$ 在 区间上", "独立公式：", "$$\n\\int_0^1 x^2\\,dx=\\frac{1}{3}\n$$"].join("\n\n"),
  );
});

test("生成的图片：画廊里每张图一段，按钮里的图用按钮标签作说明，站点小图标不进正文", () => {
  const markdownOf = markdownAdapter();
  const preview = (label: string, src: string) =>
    el(
      "button",
      { "data-testid": "generated-image-preview", "aria-label": label },
      el("img", { src, __naturalWidth: "1536" }),
      el("button", { "aria-label": "编辑生成的图像" }, "编辑"),
    );
  const root = el(
    "div",
    {},
    el("h4", { class: "sr-only" }, "ChatGPT 说："),
    el(
      "div",
      { "data-testid": "generated-image-gallery" },
      preview("已生成图像 1", "blob:https://chatgpt.com/aaa"),
      preview("已生成图像 2", "blob:https://chatgpt.com/bbb"),
    ),
    el("button", { "aria-label": "复制图像" }, "复制"),
    el("p", {}, "来源", el("a", { href: "https://example.com/" }, el("img", { alt: "", src: "https://t0.gstatic.com/f.png", __naturalWidth: "32" }), "example.com")),
  );
  assert.equal(
    markdownOf(root),
    [
      "![已生成图像 1](blob:https://chatgpt.com/aaa)",
      "![已生成图像 2](blob:https://chatgpt.com/bbb)",
      "来源[example.com](https://example.com/)",
    ].join("\n\n"),
  );
});

function connectorFixture(options: { stuck?: boolean; rejected?: boolean; duplicate?: boolean; login?: boolean; wrongTunnel?: boolean; noTools?: boolean; unlinked?: boolean; entryText?: string; detailDelay?: number; refreshDelay?: number; wrongHeading?: boolean; empty?: boolean; loading?: boolean; listDelay?: number } = {}) {
  let now = 0;
  let refreshedAt = 0;
  let refreshClicks = 0;
  const makeNode = (text: string) => ({
    innerText: text, textContent: text, childElementCount: 0, disabled: false,
    getAttribute: (_: string) => null as string | null,
    getBoundingClientRect: () => ({ width: 100, height: 20 }),
    querySelector: (_: string) => null,
    closest: (_: string) => null as unknown,
  });
  const entry = makeNode(options.entryText ?? (options.noTools ? 'Magi Magi' : 'Magi 12 tools'));
  const title = makeNode('Magi');
  title.closest = () => entry;
  const duplicate = makeNode('Magi');
  duplicate.closest = () => makeNode('Magi 9 tools');
  const refresh = makeNode(options.unlinked ? '关联' : 'Refresh tools');
  Object.defineProperty(refresh, 'disabled', { get: () => now < (options.detailDelay ?? 0)
    || (refreshClicks > 0 && (Boolean(options.stuck) || now < refreshedAt)) });
  const connect = makeNode('连接 Magi');
  const dialog = { ...makeNode('连接 Magi'), querySelectorAll: (selector: string) =>
    selector === 'h2' ? [makeNode('连接 Magi')] : selector === 'button' ? [connect] : [] };
  let connected = false;
  const alert = makeNode('Private error text');
  let clicked = false;
  const body = {
    innerText: 'Installed apps Magi 12 tools' + (options.wrongTunnel ? ' tunnel_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa' : ''),
    querySelectorAll: (selector: string) => selector === '*' && !options.empty && !options.loading && now >= (options.listDelay ?? 0)
      ? [title, ...(options.duplicate ? [duplicate] : [])]
      : selector === 'div' && options.empty ? [makeNode('你尚未安装任何插件')] : [],
  };
  const document = {
    body, documentElement: { getAttribute: () => null },
    querySelector: (selector: string) => options.login && selector === 'input[type="password"]' ? makeNode('')
      : options.rejected && clicked && selector === '[role="alert"]' ? alert
      : options.unlinked && clicked && !connected && selector === "[role='dialog']" ? dialog : null,
    querySelectorAll: (selector: string) => selector === 'button' ? [refresh]
      : options.unlinked && clicked && !connected && selector === "[role='dialog']" ? [dialog]
      : selector === 'h1' ? [makeNode(options.wrongHeading ? 'Other app' : 'Magi')]
      : options.login && selector === 'input[type="password"]' ? [makeNode('')]
      : options.rejected && clicked && selector === '[role="alert"]' ? [alert] : [],
  };
  const context: Record<string, unknown> = {
    document, location: { origin: 'https://chatgpt.com', pathname: '/settings/plugins-settings/magi', search: '' },
    getComputedStyle: () => ({ display: 'block', visibility: 'visible', opacity: '1' }),
    setTimeout: (callback: () => void, ms: number) => { now += ms; callback(); }, clearTimeout,
    __magiSetupInput: { perform: async (_: string, target: unknown) => {
      clicked = true;
      if (target === refresh) { refreshClicks++; refreshedAt = now + (options.refreshDelay ?? 0); }
      if (target === connect) connected = true;
    } },
  };
  runInNewContext(INSTALL_WEB_MODEL_ADAPTER, context);
  const adapter = context.__magiWebModel as {
    configureConnector: (input: { name: string; tunnel_id: string }) => Promise<{ confirmed_enabled: boolean; reason: string }>;
    connectorStatus: (input: { name: string }) => Promise<{ supported: boolean; enabled: boolean; tool_count: number | null; reason: string | null }>;
  };
  return {
    ...adapter,
    get refreshClicks() { return refreshClicks; },
    connectorStatus: (input: { name: string }) => {
      (context.location as { pathname: string }).pathname = '/settings/plugins-settings';
      return adapter.connectorStatus(input);
    },
  };
}

test('创建表单消失后等待超过十秒才出现的精确连接确认层，不能提前结束', async () => {
  let stage = 'directory';
  let now = 0;
  let connectAt = 0;
  let connects = 0;
  let connectDelay = 12000;
  const node = (label: string, attrs: Record<string, string> = {}) => ({
    innerText: label, textContent: label, disabled: false, checked: false,
    getAttribute: (key: string) => attrs[key] ?? null,
    getBoundingClientRect: () => ({ width: 100, height: 20 }),
    querySelectorAll: (_: string): unknown[] => [],
  });
  const add = node('Add'), create = node('Add custom MCP server'), tunnel = node('Tunnel');
  const name = node('', { type: 'text' }), id = node('', { 'aria-label': 'Tunnel id' });
  const auth = { ...node(''), options: [{ text: 'No authentication', value: 'none' }] };
  const ack = { ...node(''), checked: true };
  const save = node('Create as plugin'), heading = node('Connect Magi'), connect = node('Connect Magi');
  const form = node('Create app');
  form.querySelectorAll = selector => ({
    button: [tunnel, save], input: [name, id], 'input[type="text"]': [name],
    select: [auth], 'input[type="checkbox"]': [ack], 'button[type="submit"]': [save],
  })[selector] ?? [];
  const confirmation = node('Connect Magi');
  confirmation.querySelectorAll = selector => selector === 'h2' ? [heading] : selector === 'button' ? [connect] : [];
  const document = {
    documentElement: { getAttribute: () => null },
    querySelector(selector: string) { return this.querySelectorAll(selector)[0] ?? null; },
    querySelectorAll(selector: string): unknown[] {
      if (selector === '[role=\'dialog\']') return stage === 'form' ? [form]
        : stage === 'gap' && now >= connectAt ? [confirmation] : [];
      return selector === 'button' ? [add] : selector === '[role="menuitem"]' ? [create] : [];
    },
  };
  const context = {
    document, location: { origin: 'https://chatgpt.com', pathname: '/plugins', search: '' },
    getComputedStyle: () => ({ display: 'block', visibility: 'visible', opacity: '1' }),
    Date: class extends Date { static override now() { return now; } },
    setTimeout: (callback: () => void, ms: number) => { now += ms; callback(); }, clearTimeout,
    __magiSetupInput: { perform: async (_: string, target: unknown) => {
      if (target === create) stage = 'form';
      if (target === save) { stage = 'gap'; connectAt = now + connectDelay; }
      if (target === connect) { connects++; stage = 'closed'; }
    } },
  };
  runInNewContext(INSTALL_WEB_MODEL_ADAPTER, context);
  const adapter = (context as unknown as { __magiWebModel: {
    configureConnector: (input: { name: string; tunnel_id: string }) => Promise<{ configured: boolean }>;
  } }).__magiWebModel;
  const result = await adapter.configureConnector({ name: 'Magi', tunnel_id: 'tunnel_0123456789abcdef0123456789abcdef' });
  assert.equal(result.configured, true);
  assert.equal(connects, 1, '延迟出现的连接确认必须实际点击一次');
  stage = 'directory';
  connects = 0;
  heading.innerText = heading.textContent = 'Connect Other';
  const wrongApp = await adapter.configureConnector({ name: 'Magi', tunnel_id: 'tunnel_0123456789abcdef0123456789abcdef' });
  assert.equal(wrongApp.configured, false, '其他应用的确认层不能视为成功');
  assert.equal(connects, 0, '标题不匹配时不点击确认按钮');
  stage = 'directory';
  heading.innerText = heading.textContent = 'Connect Magi';
  connectDelay = 31000;
  const timeout = await adapter.configureConnector({ name: 'Magi', tunnel_id: 'tunnel_0123456789abcdef0123456789abcdef' });
  assert.equal(timeout.configured, false, '等待仍有上限，超时保留未确认结果');
  assert.equal(connects, 0, '超时不补点连接');
});

test('连接器刷新等待超时或出现错误不能报告成功', async () => {
  for (const options of [{ stuck: true }, { rejected: true }]) {
    const result = await connectorFixture(options).configureConnector({ name: 'Magi', tunnel_id: 'tunnel_0123456789abcdef0123456789abcdef' });
    assert.equal(result.confirmed_enabled, false);
    assert.equal(result.reason, options.stuck ? 'connector_refresh_unconfirmed' : 'connector_refresh_rejected');
    assert.ok(!result.reason.includes('Private'));
  }
  assert.equal((await connectorFixture().configureConnector({ name: 'Magi', tunnel_id: 'tunnel_0123456789abcdef0123456789abcdef' })).confirmed_enabled, true);
});

test('连接器回读识别工具数量，登录页和同名歧义不能当成配置成功', async () => {
  const ready = await connectorFixture().connectorStatus({ name: 'Magi' });
  assert.equal(ready.enabled, true);
  assert.equal(ready.tool_count, 12);
  const duplicate = await connectorFixture({ duplicate: true }).connectorStatus({ name: 'Magi' });
  assert.equal(duplicate.supported, false);
  assert.equal(duplicate.reason, 'connector_ambiguous');
  const login = await connectorFixture({ login: true }).connectorStatus({ name: 'Magi' });
  assert.equal(login.supported, false);
  const wrongTunnel = await connectorFixture({ wrongTunnel: true }).configureConnector({ name: 'Magi', tunnel_id: 'tunnel_0123456789abcdef0123456789abcdef' });
  assert.equal(wrongTunnel.confirmed_enabled, false);
  assert.equal(wrongTunnel.reason, 'connector_tunnel_mismatch');
  const installedOnly = await connectorFixture({ noTools: true }).connectorStatus({ name: 'Magi' });
  assert.equal(installedOnly.enabled, false, '只有已安装名称不能证明已关联');
  const associated = await connectorFixture({ unlinked: true }).configureConnector({ name: 'Magi', tunnel_id: 'tunnel_0123456789abcdef0123456789abcdef' });
  assert.equal(associated.confirmed_enabled, true);
  assert.equal(associated.reason, 'connection_confirmed');
});

test('新版列表的权限摘要不能替代工具就绪证据，加载中的空列表不能触发创建', async () => {
  const permission = await connectorFixture({ entryText: 'Magi Magi 允许使用低风险工具' }).connectorStatus({ name: 'Magi' });
  assert.equal(permission.supported, true);
  assert.equal(permission.enabled, false);
  assert.equal(permission.tool_count, null);
  const loading = await connectorFixture({ loading: true }).connectorStatus({ name: 'Magi' });
  assert.equal(loading.supported, false);
  assert.equal(loading.reason, 'connector_page_not_open');
  const empty = await connectorFixture({ empty: true }).connectorStatus({ name: 'Magi' });
  assert.equal(empty.supported, true);
  const delayed = await connectorFixture({ listDelay: 2500 }).connectorStatus({ name: 'Magi' });
  assert.equal(delayed.enabled, true);
});

test('连接器详情首次加载和刷新超过旧时限时等待完成，只点击一次刷新', async () => {
  const fixture = connectorFixture({ detailDelay: 12000, refreshDelay: 18000 });
  const result = await fixture.configureConnector({ name: 'Magi', tunnel_id: 'tunnel_0123456789abcdef0123456789abcdef' });
  assert.equal(result.confirmed_enabled, true);
  assert.equal(fixture.refreshClicks, 1);
  const wrong = connectorFixture({ wrongHeading: true });
  assert.equal((await wrong.configureConnector({ name: 'Magi', tunnel_id: 'tunnel_0123456789abcdef0123456789abcdef' })).reason, 'connector_detail_unconfirmed');
  assert.equal(wrong.refreshClicks, 0);
});

test('离屏推理页通过按钮标准激活提交，并回读实际清空状态', async () => {
  const makeNode = <T extends Record<string, unknown>>(extra: T) => ({
    getAttribute: () => null, hasAttribute: () => false,
    getBoundingClientRect: () => ({ width: 100, height: 30 }), ...extra,
  });
  const composer = makeNode({ tagName: 'TEXTAREA', value: '只读验收', closest: () => form });
  let clicks = 0;
  const button = makeNode({ click: () => { clicks++; composer.value = ''; } });
  const form = { querySelectorAll: (selector: string) => selector === "button[data-testid='send-button']" ? [button] : [] };
  const context: Record<string, unknown> = {
    document: { documentElement: { getAttribute: () => null },
      querySelectorAll: (selector: string) => selector === 'textarea#prompt-textarea' ? [composer] : [] },
    getComputedStyle: () => ({ display: 'block', visibility: 'visible', opacity: '1' }),
    setTimeout: (callback: () => void) => setTimeout(callback, 0),
  };
  runInNewContext(INSTALL_WEB_MODEL_ADAPTER, context);
  const adapter = context.__magiWebModel as { submit: () => Promise<{ submitted: boolean; composer_empty: boolean }> };
  const result = await adapter.submit();
  assert.equal(clicks, 1);
  assert.equal(result.submitted, true);
  assert.equal(result.composer_empty, true);
  composer.value = '未被接纳';
  button.click = () => { clicks++; };
  assert.equal((await adapter.submit()).composer_empty, false);
  assert.equal(clicks, 2);
});

test('首次临时聊天延迟出现且标签重复时，先确认再写入草稿', async () => {
  let confirmed = false;
  let reads = 0;
  const confirm = { hidden: false, innerText: '继续',
    getAttribute: (key: string) => key === 'aria-label' ? '继续' : null,
    getBoundingClientRect: () => ({ width: 100, height: 30 }),
    click: () => { confirmed = true; confirm.hidden = true; } };
  const document = { documentElement: { getAttribute: () => null },
    querySelectorAll: (selector: string) => {
      if (selector !== "[role='dialog'] button") return [];
      return ++reads > 1 ? [confirm] : [];
    } };
  const context: Record<string, unknown> = { document,
    location: { search: '?temporary-chat=true' },
    getComputedStyle: () => ({ display: 'block', visibility: 'visible', opacity: '1' }),
    setTimeout: (callback: () => void) => setTimeout(callback, 0),
  };
  runInNewContext(INSTALL_WEB_MODEL_ADAPTER, context);
  const adapter = context.__magiWebModel as { writeText: (input: unknown) => Promise<unknown> };
  await assert.rejects(adapter.writeText({ selector: '@composer', text: 'draft', mode: 'replace' }), /web_selector_not_found/u);
  assert.equal(confirmed, true);
});
