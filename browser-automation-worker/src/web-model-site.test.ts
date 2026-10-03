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
