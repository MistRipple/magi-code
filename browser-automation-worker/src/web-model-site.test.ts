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
  normalizeModelFamily,
  normalizeWebModelProbe,
  normalizeWebModelTurnState,
  type WebModelRawSnapshot,
  type WebModelRawTurnState,
} from "./web-model-site.js";

function snapshot(overrides: Partial<WebModelRawSnapshot> = {}): WebModelRawSnapshot {
  return {
    origin: "https://chatgpt.com",
    path: "/",
    blocked: false,
    composerFound: true,
    composerCharLimit: null,
    conversationFound: true,
    modelMenuFound: true,
    modelItems: [],
    accountHint: null,
    connectorSettingsFound: false,
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

test("normalizeModelFamily 只识别站点真实模型族", () => {
  assert.equal(normalizeModelFamily("ChatGPT"), "chatgpt");
  assert.equal(normalizeModelFamily("ChatGPT 适用于快速任务和回答"), "chatgpt");
  assert.equal(normalizeModelFamily("ChatGPT Business 助力高效工作"), null);
  assert.equal(normalizeModelFamily("GPT-5 Thinking"), "gpt-5-thinking");
  assert.equal(normalizeModelFamily("GPT-5.6 Sol"), "gpt-5.6-sol");
  assert.equal(normalizeModelFamily("GPT-5.5"), "gpt-5.5");
  assert.equal(normalizeModelFamily("GPT-5"), "gpt-5");
  assert.equal(normalizeModelFamily("GPT-4o"), "gpt-4o");
  assert.equal(normalizeModelFamily("o4-mini"), "o4-mini");
  assert.equal(normalizeModelFamily("GPT-Next Preview"), "gpt-next-preview");
  assert.equal(normalizeModelFamily("o5-mini"), "o5-mini");
  assert.equal(normalizeModelFamily("随便一个名字"), null);
});

test("站点 origin 必须是受支持的 ChatGPT Web origin", () => {
  assert.equal(isSupportedChatGptOrigin("https://chatgpt.com"), true);
  assert.equal(isSupportedChatGptOrigin("https://chat.openai.com"), true);
  assert.equal(isSupportedChatGptOrigin("https://example.test"), false);
  assert.equal(isSupportedChatGptOrigin("https://chatgpt.com.evil.test"), false);
});

test("当前 ChatGPT 模型菜单档位快照可直接投影到 Web 引擎", () => {
  const probe = normalizeWebModelProbe(snapshot({
    accountHint: "Plus",
    modelItems: [
      { label: "GPT-5.6 Sol", checked: true, efforts: ["low", "medium", "high"] },
      { label: "GPT-5.5", checked: false, efforts: ["low", "medium", "high"] },
    ],
  }));
  assert.deepEqual(probe.models.map((model) => model.family), ["gpt-5.6-sol", "gpt-5.5"]);
  assert.deepEqual(probe.models[0]?.efforts, ["low", "medium", "high"]);
});

test("未登录时探测结果没有模型，且状态为 signed_out", () => {
  // 登录态需要两项证据：会话有效 + composer 可用（设计基线 §5.5）。
  const noComposer = normalizeWebModelProbe(snapshot({ composerFound: false }));
  assert.equal(noComposer.loginState, "signed_out");
  assert.deepEqual(noComposer.models, []);

  const noConversation = normalizeWebModelProbe(snapshot({ conversationFound: false }));
  assert.equal(noConversation.loginState, "signed_out");
  assert.deepEqual(noConversation.models, []);
});

test("未登录时即使读到了模型菜单也不返回模型", () => {
  const probe = normalizeWebModelProbe(snapshot({
    composerFound: false,
    modelItems: [{ label: "GPT-5", checked: true, efforts: ["medium"] }],
  }));
  assert.equal(probe.loginState, "signed_out");
  assert.deepEqual(probe.models, [], "未登录不得用缓存列表冒充可用结果");
});

test("风控页判定为 blocked 且不返回模型", () => {
  const probe = normalizeWebModelProbe(snapshot({
    blocked: true,
    modelItems: [{ label: "GPT-5", checked: true, efforts: ["medium"] }],
  }));
  assert.equal(probe.loginState, "blocked");
  assert.deepEqual(probe.models, []);
});

test("已登录时按站点菜单归一化模型族与档位", () => {
  const probe = normalizeWebModelProbe(snapshot({
    accountHint: "Plus",
    connectorSettingsFound: true,
    composerCharLimit: 1_050_000,
    modelItems: [
      { label: "GPT-5 Thinking", checked: true, efforts: ["medium", "high"], defaultEffort: "medium" },
      { label: "GPT-4o", checked: false, efforts: [] },
    ],
  }));
  assert.equal(probe.loginState, "signed_in");
  assert.equal(probe.accountHint, "plus");
  assert.equal(probe.composerCharLimit, 1_050_000);
  assert.equal(probe.connectorSupport, true);
  assert.deepEqual(probe.models.map((model) => model.family), ["gpt-5-thinking", "gpt-4o"]);
  const [thinking, chat] = probe.models;
  assert.ok(thinking && chat);
  assert.deepEqual(thinking.efforts, ["medium", "high"]);
  // 菜单没有给出档位时不能猜测站点能力；GPT-4o 仍可作为无 effort 的模型发现。
  assert.deepEqual(chat.efforts, []);
  assert.equal(thinking.defaultEffort, "medium");
  assert.equal(chat.defaultEffort, null);
});

test("模型族按 family 去重，并且不凭档位顺序猜 default_effort", () => {
  const probe = normalizeWebModelProbe(snapshot({
    modelItems: [
      { label: "GPT-5", checked: true, efforts: ["high", "medium"], defaultEffort: null },
      { label: "GPT-5 (recommended)", checked: false, efforts: ["low", "medium"] },
    ],
  }));
  assert.equal(probe.diagnostic, null);
  assert.deepEqual(probe.models, [{
    family: "gpt-5",
    displayName: "GPT-5",
    efforts: ["low", "medium", "high"],
    defaultEffort: null,
  }]);
});

test("已登录但模型菜单或模型标签不可识别时给出 selector 漂移诊断", () => {
  const menuMissing = normalizeWebModelProbe(snapshot({ modelMenuFound: false }));
  assert.equal(menuMissing.loginState, "signed_in");
  assert.equal(menuMissing.diagnostic, "model_menu_selector_missing");
  assert.deepEqual(menuMissing.models, []);

  const labelsMissing = normalizeWebModelProbe(snapshot({
    modelItems: [{ label: "Temporary chat", checked: false, efforts: [] }],
  }));
  assert.equal(labelsMissing.loginState, "signed_in");
  assert.equal(labelsMissing.diagnostic, "model_items_unrecognized");
  assert.deepEqual(labelsMissing.models, []);
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
    account_hint: null,
  };
  assert.equal(normalizeWebModelTurnState(raw).loginState, "signed_out");
  assert.equal(normalizeWebModelTurnState({ ...raw, conversation_found: true }).loginState, "signed_in");
});

test("页面适配器字符串包含命令所需的原子写入、观察和回合读取接口", () => {
  for (const method of ["probe", "writeText", "observe", "submit", "turnState", "cancelGeneration"]) {
    assert.match(INSTALL_WEB_MODEL_ADAPTER, new RegExp(`\\b${method}\\b`));
  }
  assert.match(INSTALL_WEB_MODEL_ADAPTER, /found: false/u);
  assert.match(INSTALL_WEB_MODEL_ADAPTER, /timeout_ms/u);
  assert.match(INSTALL_WEB_MODEL_ADAPTER, /conversation_found/u);
  assert.match(INSTALL_WEB_MODEL_ADAPTER, /textWithLineBreaks/u);
  assert.match(INSTALL_WEB_MODEL_ADAPTER, /generating:/u);
  assert.match(INSTALL_WEB_MODEL_ADAPTER, /assistant_text:/u);
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
    location: { origin: "https://chatgpt.com", pathname: "/", search: "" },
    getComputedStyle: () => ({ display: "block", visibility: "visible", opacity: "1" }),
    globalThis: undefined,
  };
  context.globalThis = context;
  runInNewContext(INSTALL_WEB_MODEL_ADAPTER, context);
  const adapter = context.__magiWebModel as {
    probe: () => Promise<WebModelRawSnapshot>;
    observe: (input: { selector: string; fields: string[]; attribute: null }) => { revision: number; nodes: Array<Record<string, unknown>> };
  };
  const probe = await adapter.probe();
  assert.equal(probe.composerFound, false);
  assert.equal(probe.conversationFound, false);
  const observed = adapter.observe({ selector: "@assistantMessage", fields: ["existence"], attribute: null });
  assert.equal(observed.revision, 1);
  assert.equal(observed.nodes.length, 1);
  assert.equal(observed.nodes[0]?.found, false);
});

test("未知账号等级归为 unknown，不猜等级", () => {
  const probe = normalizeWebModelProbe(snapshot({ accountHint: "business" }));
  assert.equal(probe.accountHint, "unknown");
});
