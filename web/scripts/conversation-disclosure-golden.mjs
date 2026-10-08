import assert from 'node:assert/strict';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { withGoldenViteServer } from './golden-vite.mjs';

await withGoldenViteServer(async (server) => {
  const disclosure = await server.ssrLoadModule('/src/lib/conversation-disclosure.ts');
  const translate = (key) => key;

  const message = (overrides) => ({
    id: 'm',
    role: 'assistant',
    type: 'text',
    content: '',
    blocks: [],
    metadata: {},
    isStreaming: false,
    timestamp: 0,
    ...overrides,
  });
  const eventEntry = (key, overrides) => ({
    kind: 'event',
    key,
    item: { key, message: message(overrides) },
  });
  const toolEntry = (key) => ({
    kind: 'tool-group',
    key,
    items: [{ key: `${key}-item`, message: message({ type: 'tool_call' }) }],
  });

  // ---- 预览：只去语法标记，保留词内连字符、路径与标点，不凭空插入空格 ----
  const label = (content) => disclosure.resolveConversationProcessLabel(message({ content }), translate);
  assert.equal(
    label('**项目分析**：`magi-rust-rewrite`（Magi）'),
    '项目分析：magi-rust-rewrite（Magi）',
    '预览不得把 foo-bar 拆成 foo bar，也不得在标点两侧插入空格',
  );
  assert.equal(label('## 标题\n- 第一条\n- 第二条'), '标题 第一条 第二条', '标题与列表标记应被去掉');
  assert.equal(label('看 [文档](https://example.com/a-b) 与 src/lib/foo_bar.ts'), '看 文档 与 src/lib/foo_bar.ts', '链接保留文字，路径中的下划线保留');
  assert.equal(label('前文\n```rust\nfn main() {}\n```\n后文'), '前文 后文', '代码块不进入预览');
  const long = label('字'.repeat(500));
  assert.ok(long.length <= 160 && long.endsWith('…'), '预览必须截断，避免超长标题');

  // ---- 工具组标签：用真实文案渲染，不能出现未替换的占位符；浏览器工具按命名空间归类 ----
  const { readFileSync } = await import('node:fs');
  const zh = JSON.parse(readFileSync(new URL('../src/i18n/zh-CN.json', import.meta.url), 'utf8'));
  const zhTranslate = (key, vars = {}) => {
    const template = zh[key];
    assert.equal(typeof template, 'string', `缺少文案 ${key}`);
    return template.replace(/\{(\w+)\}/gu, (match, name) => (name in vars ? String(vars[name]) : match));
  };
  const toolItem = (key, name, args = {}, status = 'success') => ({
    key,
    message: message({
      type: 'tool_call',
      metadata: { toolName: name },
      blocks: [{ id: `${key}-b`, type: 'tool_call', toolCall: { id: key, name, arguments: args, status } }],
    }),
  });
  const groupLabel = (items) => disclosure.resolveConversationToolGroupLabel(items, zhTranslate);
  for (const [name, args] of [['file_read', {}], ['shell_exec', {}], ['search_text', {}], ['file_write', {}]]) {
    const text = groupLabel([toolItem('t', name, args)]);
    assert.ok(!/\{\w+\}/u.test(text), `${name} 缺少目标时不得显示占位符：${text}`);
  }
  assert.equal(groupLabel([toolItem('t', 'file_read', { path: 'src/lib/a.ts' })]), '读取 a.ts');
  assert.equal(groupLabel([toolItem('t', 'file_read', {})]), '读取文件');
  assert.equal(groupLabel([toolItem('t', 'browser_read', { query: '价格' })]), '浏览器读取正文 “价格”', 'browser_read 是浏览器操作而不是读文件');
  assert.equal(groupLabel([toolItem('t', 'browser_click', { element_ref: 'e:1:2' })]), '浏览器点击', '结果未到达时不显示元素引用');
  {
    const clicked = toolItem('c', 'browser_click', { element_ref: 'e:1:2' });
    clicked.message.blocks[0].toolCall.result = JSON.stringify({
      tool: 'browser_click', status: 'succeeded', target: { role: 'button', name: '提交订单' },
    });
    assert.equal(groupLabel([clicked]), '浏览器点击 提交订单', '点击完成后显示运行时报告的实际元素');
  }
  assert.equal(
    groupLabel([toolItem('a', 'browser_read'), toolItem('b', 'browser_click')]),
    '完成了 2 项浏览器操作',
    '浏览器读取与点击应合并成浏览器操作组',
  );
  assert.equal(
    groupLabel([toolItem('a', 'file_read', { path: 'a.ts' }), toolItem('b', 'browser_read')]),
    '完成了 2 项操作',
    '文件读取与浏览器读取不能被合并成“读取了 2 个文件”',
  );
  assert.equal(
    groupLabel([toolItem('t', 'browser_click', {}, 'error')]),
    '浏览器点击 · 失败',
  );

  // ---- 结果未确认（副作用可能已发生）与普通失败分开呈现 ----
  const unconfirmedItem = (key) => {
    const item = toolItem(key, 'browser_click', { element_ref: 'e:1:1' }, 'unconfirmed');
    item.message.blocks[0].toolCall.result = JSON.stringify({ tool: 'browser_click', status: 'indeterminate', error_code: 'browser_host_request_indeterminate' });
    return item;
  };
  assert.equal(groupLabel([unconfirmedItem('u')]), '浏览器点击 · 结果未确认');
  {
    // 未确认来自正式的 item 状态，而不是从结果文本里嗅探
    const sniffed = toolItem('s', 'browser_click', {}, 'error');
    sniffed.message.blocks[0].toolCall.result = JSON.stringify({ status: 'indeterminate' });
    assert.equal(groupLabel([sniffed]), '浏览器点击 · 失败');
  }
  assert.equal(
    groupLabel([unconfirmedItem('u'), toolItem('f', 'browser_type', { text: 'x' }, 'error'), toolItem('ok', 'browser_read')]),
    '完成了 3 项浏览器操作，其中 1 项失败，1 项结果未确认',
  );

  // ---- 过程流：追加条目不能让已显示的条目消失或改变 key（否则阶段重挂载、展开状态丢失） ----
  const thinkingItem = (key) => ({ key, message: message({ id: key, type: 'thinking', content: '想一想' }) });
  const textItem = (key) => ({ key, message: message({ id: key, type: 'text', content: '我先看看。' }) });
  const process = (...items) => items.map((item) => ({ item, role: 'process' }));
  const streamKeys = (entries) => disclosure.buildConversationStreamEntries(entries).map((entry) => entry.key);
  const t1 = thinkingItem('t1');
  const a = toolItem('a', 'file_read', { path: 'a.ts' });
  const t2 = thinkingItem('t2');
  const b = toolItem('b', 'shell_exec', { command: 'ls' });
  assert.deepEqual(streamKeys(process(t1)), ['event:t1']);
  assert.deepEqual(
    streamKeys(process(t1, a)),
    ['event:t1', 'tool-group:a'],
    '第一个工具出现后，之前已显示的思考必须保留',
  );
  const withMiddleThinking = disclosure.buildConversationStreamEntries(process(t1, a, t2, b));
  assert.deepEqual(withMiddleThinking.map((entry) => entry.key), ['event:t1', 'tool-group:a'], '工具之间的思考不能把工具组切开');
  assert.deepEqual(withMiddleThinking[1].items.map((item) => item.key), ['a', 'b']);
  const prefixKeys = streamKeys(process(textItem('x'), a));
  const extendedKeys = streamKeys(process(textItem('x'), a, textItem('y'), b));
  assert.deepEqual(extendedKeys.slice(0, prefixKeys.length), prefixKeys, '追加条目后，已有条目的 key 保持不变');

  // ---- 紧凑事件：短小纯文字才是紧凑的 ----
  assert.equal(disclosure.isCompactProcessEvent(message({ content: '我先读取入口文件。' })), true);
  assert.equal(disclosure.isCompactProcessEvent(message({ content: 'a\nb' })), false, '含换行不能压成一行');
  assert.equal(disclosure.isCompactProcessEvent(message({ content: '- 一\n- 二' })), false, '列表必须完整渲染');
  assert.equal(disclosure.isCompactProcessEvent(message({ content: '| a | b |' })), false, '表格必须完整渲染');
  assert.equal(disclosure.isCompactProcessEvent(message({ content: '字'.repeat(200) })), false, '超长文字必须完整渲染');
  assert.equal(disclosure.isCompactProcessEvent(message({ type: 'thinking', content: '想一下' })), false, '思考块必须完整渲染');
  assert.equal(
    disclosure.isCompactProcessEvent(message({ content: '短', blocks: [{ id: 'c', type: 'code', content: 'x' }] })),
    false,
    '含代码块的消息必须完整渲染',
  );

  // ---- 阶段展开内容 ----
  const shortOnly = { key: 'p1', entries: [eventEntry('e1', { content: '我先读取入口文件。' })] };
  assert.deepEqual(
    disclosure.resolveConversationPhaseDetails(shortOnly),
    [],
    '标题已完整表达的单条短文字，展开后没有更多内容：阶段应是静态标题',
  );
  const richFirst = { key: 'p2', entries: [eventEntry('e2', { content: '# 报告\n\n- 结论一\n- 结论二' })] };
  assert.deepEqual(
    disclosure.resolveConversationPhaseDetails(richFirst).map((detail) => detail.kind),
    ['rich'],
    '带结构的首条内容必须展开后完整渲染',
  );
  const mixed = {
    key: 'p3',
    entries: [
      eventEntry('e3', { content: '先看看目录。' }),
      toolEntry('t3'),
      eventEntry('e4', { content: '再确认一下。' }),
    ],
  };
  assert.deepEqual(
    disclosure.resolveConversationPhaseDetails(mixed).map((detail) => detail.kind),
    ['tool-group', 'compact'],
    '首条短文字由标题表达；工具组沿用工具卡片；其余短文字保持紧凑行',
  );

  // ---- 最终输出：唯一依据是后端的 assistantOutputKind（final / error），不靠位置或耗时元数据猜测 ----
  const toolCallBlock = { id: 't', type: 'tool_call', content: '', toolCall: { name: 'shell_exec', status: 'success' } };
  assert.equal(
    disclosure.isConversationFinalMessage(message({ content: '答案', metadata: { responseDurationMs: 1200 } })),
    false,
    '没有 final 标记的文字即使带耗时也不是最终输出',
  );
  assert.equal(
    disclosure.isConversationFinalMessage(message({ content: '我先看看', metadata: { assistantOutputKind: 'progress' } })),
    false,
    '流式中的 progress 文字不是最终输出，模型之后可能继续调用工具',
  );
  assert.equal(
    disclosure.isConversationFinalMessage(message({ content: '答案', metadata: { assistantOutputKind: 'final' } })),
    true,
    '明确的 final 标记优先',
  );
  assert.equal(
    disclosure.isConversationFinalMessage(message({ type: 'tool_call', blocks: [toolCallBlock], metadata: { responseDurationMs: 19000 } })),
    false,
    '一轮以工具调用结束时，附着耗时的工具调用不是最终输出，必须留在过程组里',
  );
  assert.equal(
    disclosure.isConversationFinalMessage(message({ type: 'thinking', metadata: { responseDurationMs: 5 } })),
    false,
    '思考消息不能靠耗时冒充最终输出',
  );
  assert.equal(
    disclosure.isConversationFinalMessage(message({ content: '出错了', metadata: { assistantOutputKind: 'error' } })),
    true,
    'error 标记同样是最终输出',
  );
  assert.equal(
    disclosure.isToolLikeMessage(message({ blocks: [{ id: 'f', type: 'file_change', content: '' }] })),
    true,
    '文件变更块属于工具类过程',
  );

  // ---- 系统通知不能成为阶段标题，也不能吞掉后面的工具组 ----
  const blocks = disclosure.buildConversationDisclosureBlocks([
    eventEntry('n1', { type: 'system-notice', content: 'Context compacted' }),
    toolEntry('t9'),
  ]);
  assert.deepEqual(
    blocks.map((block) => block.kind),
    ['phase', 'tool-group'],
    '系统通知独占一行，其后的工具组是独立块而不是通知的子项',
  );
  assert.equal(
    disclosure.resolveConversationPhaseDetails(blocks[0].phase).length,
    0,
    '系统通知独占的阶段是静态行',
  );
  const modelWork = disclosure.buildConversationDisclosureBlocks([
    eventEntry('m1', { content: '开始处理。' }),
    toolEntry('t10'),
    eventEntry('m2', { content: '继续。' }),
  ]);
  assert.deepEqual(
    modelWork.map((block) => block.kind),
    ['phase', 'phase'],
    '模型文字之后的新文字仍然开始下一个阶段',
  );

  // ---- 流式正文不能出现「标题 + 同样内容的正文」：结束时标题会凭空消失，同一段话看起来出现两遍 ----
  const listText = '还可以帮你：\n- 制定计划：拆分步骤\n- 比较与决策：分析优缺点';
  const streamingPhase = { key: 'phase:stream', entries: [eventEntry('s1', { content: listText })] };
  const streaming = disclosure.resolveConversationPhasePresentation(streamingPhase, {
    active: true,
    expanded: true,
    manualOverride: false,
  });
  assert.equal(streaming.bodyOnly, true, '正在流式输出的一段多行文字只显示正文，不带标题行');
  assert.equal(streaming.headerRepeatsBody, true);
  assert.equal(
    disclosure.resolveConversationPhasePresentation(streamingPhase, {
      active: true,
      expanded: true,
      manualOverride: true,
    }).bodyOnly,
    false,
    '用户手动操作过的阶段保留标题行，否则没有办法再收起',
  );
  assert.equal(
    disclosure.resolveConversationPhasePresentation(streamingPhase, {
      active: false,
      expanded: false,
      manualOverride: false,
    }).headerRepeatsBody,
    false,
    '收起时标题就是内容摘要，不算重复',
  );
  const withTool = {
    key: 'phase:tool',
    entries: [eventEntry('s2', { content: listText }), toolEntry('t20')],
  };
  const withToolPresentation = disclosure.resolveConversationPhasePresentation(withTool, {
    active: true,
    expanded: true,
    manualOverride: false,
  });
  assert.equal(withToolPresentation.bodyOnly, false, '带工具的阶段需要标题行来收起 / 展开');
  assert.equal(withToolPresentation.headerRepeatsBody, true, '但展开后标题不应再重复首条正文');
  const shortText = { key: 'phase:short', entries: [eventEntry('s3', { content: '开始处理。' })] };
  assert.deepEqual(
    disclosure.resolveConversationPhasePresentation(shortText, { active: true, expanded: true, manualOverride: false }),
    { bodyOnly: false, headerRepeatsBody: false, thinkingOnly: false },
    '一句短文字本身就是标题，没有重复',
  );

  // ---- 思考：摘要模式不能再套一张原始风格的思考卡片，也不能因为「去重」丢掉标题 ----
  const thinkingMessage = (overrides = {}) => message({
    type: 'thinking',
    content: '',
    blocks: [{
      id: 'tb',
      type: 'thinking',
      content: '',
      thinking: {
        groupId: 'g1',
        status: overrides.status ?? 'running',
        isStreaming: overrides.streaming ?? true,
        segments: [{
          segmentId: 's1',
          messageId: 'm',
          status: overrides.status ?? 'running',
          content: '先算每小时的注水量：A 是 1/6，B 是 1/4，C 排水 1/12。',
        }],
      },
    }],
  });
  const thinkingPhase = { key: 'phase:think', entries: [eventEntry('th1', thinkingMessage())] };
  const thinkingPresentation = disclosure.resolveConversationPhasePresentation(thinkingPhase, {
    active: true,
    expanded: true,
    manualOverride: false,
  });
  assert.equal(thinkingPresentation.bodyOnly, false, '思考阶段始终保留标题行：标题承担「思考中 / 已思考」的状态');
  assert.equal(thinkingPresentation.thinkingOnly, true);
  assert.equal(thinkingPresentation.headerRepeatsBody, true, '展开后标题不再重复思考正文，改为状态标题');
  assert.equal(
    disclosure.resolveConversationPhasePresentation(
      { key: 'phase:mixed', entries: [eventEntry('th2', thinkingMessage()), toolEntry('t30')] },
      { active: true, expanded: true, manualOverride: false },
    ).thinkingOnly,
    false,
    '思考后面跟着工具的阶段不能把标题写成「思考中」',
  );

  {
    const { render: renderComponent } = await server.ssrLoadModule('svelte/server');
    const phaseComponent = await server.ssrLoadModule('/src/components/ConversationPhase.svelte');
    const phaseHtml = (phase, props = {}) => renderComponent(phaseComponent.default, {
      props: {
        phase,
        active: true,
        filePreviewScopeForItem: () => undefined,
        continueInterruptedSession: () => undefined,
        ...props,
      },
    }).body.replace(/<!--[\s\S]*?-->/gu, '');
    const activeThinking = phaseHtml(thinkingPhase);
    assert.doesNotMatch(activeThinking, /class="thinking-block/u, '摘要风格不渲染原始风格的思考卡片');
    assert.match(activeThinking, /先算每小时的注水量/u, '思考文字展开后仍然可见');
    assert.match(activeThinking, /conversation-phase-label[^>]*>思考中\.\.\.</u, '展开的思考阶段标题是「思考中」状态而不是内容预览');
    const doneThinking = phaseHtml(
      { key: 'phase:done', entries: [eventEntry('th3', thinkingMessage({ status: 'completed', streaming: false }))] },
      { active: false },
    );
    assert.doesNotMatch(doneThinking, /conversation-thinking-text/u, '收起的思考阶段只显示标题行里的内容预览，不渲染正文');
    assert.match(doneThinking, /conversation-phase-label[^>]*>思考已完成</u, '收起的思考行标题始终是思考自己的状态，不是一句看不出是思考的内容预览');
    assert.match(doneThinking, /title="先算每小时的注水量/u, '内容预览留在悬停提示里');
    const streamingText = phaseHtml({
      key: 'phase:stream-render',
      entries: [eventEntry('s9', { content: listText, source: 'orchestrator' })],
    });
    assert.doesNotMatch(streamingText, /conversation-phase-header/u, '流式正文不带重复的标题行');
  }

  // ---- 思考独立成块：自己折叠，不跟整轮「已处理」绑在一起 ----
  const split = disclosure.buildConversationDisclosureBlocks([
    eventEntry('p1', { content: '先看一下。' }),
    eventEntry('th10', thinkingMessage({ status: 'completed', streaming: false })),
    eventEntry('p2', { content: '再看一下。' }),
    toolEntry('t40'),
  ]);
  assert.deepEqual(
    split.map((block) => block.kind === 'phase' && disclosure.isThinkingOnlyPhase(block.phase)),
    [false, true, false],
    '思考前后的文字各成阶段，思考单独一块，且被识别为独立展示',
  );
  assert.equal(split.length, 3);
  const emptyThinking = disclosure.buildConversationDisclosureBlocks([
    eventEntry('th11', message({
      type: 'thinking',
      blocks: [{ id: 'e', type: 'thinking', content: '', thinking: { groupId: 'g', status: 'completed', isStreaming: false, segments: [{ segmentId: 's', messageId: 'm', status: 'completed', content: '  ' }] } }],
    })),
  ]);
  assert.deepEqual(emptyThinking, [], '没有内容的思考不占一行');
  const streamingEmpty = thinkingMessage({ status: 'running', streaming: true });
  streamingEmpty.blocks[0].thinking.segments[0].content = '';
  assert.equal(
    disclosure.buildConversationDisclosureBlocks([eventEntry('th12', streamingEmpty)]).length,
    1,
    '刚开始、还没有文字的流式思考要显示出来，用户才知道模型在思考',
  );

  {
    // 整轮折叠（有最终回答、没有在运行）时，思考仍然作为独立的一行显示，自己决定展开与否。
    const { render: renderTurn } = await server.ssrLoadModule('svelte/server');
    const turnComponent = await server.ssrLoadModule('/src/components/ConversationTurn.svelte');
    const turnItems = [
      { key: 'u1', message: message({ id: 'u1', role: 'user', type: 'user_input', content: '问题', source: 'user' }) },
      { key: 'th20', message: thinkingMessage({ status: 'completed', streaming: false }) },
      { key: 'f1', message: message({ id: 'f1', content: '答案是 3 小时。', source: 'orchestrator', metadata: { assistantOutputKind: 'final' } }) },
    ];
    const turnHtml = (extraItems = []) => renderTurn(turnComponent.default, {
      props: {
        turnId: 'turn-1',
        items: [...turnItems, ...extraItems],
        runtimeActive: false,
        initialExpanded: false,
        filePreviewScopeForItem: () => undefined,
        canEditMessage: () => false,
        editMessage: () => undefined,
        continueInterruptedSession: () => undefined,
      },
    }).body.replace(/<!--[\s\S]*?-->/gu, '');
    const collapsedTurn = turnHtml();
    assert.match(collapsedTurn, /data-conversation-phase=/u, '整轮折叠时思考行仍然显示');
    assert.match(collapsedTurn, /思考已完成/u);
    assert.doesNotMatch(collapsedTurn, /turn-disclosure-header/u, '只有思考时没有可折叠的过程，不画整轮折叠的标题');
    const withTool = turnHtml([{ key: 't50', message: message({ type: 'tool_call', blocks: [] }) }]);
    assert.doesNotMatch(
      withTool.replace(/<section class="conversation-phase[\s\S]*?<\/section>/u, ''),
      /conversation-phase-header/u,
      '整轮折叠时，文字阶段和工具仍然收在「已处理」里',
    );
  }

  // ---- Markdown：紧凑列表项里的行内标记必须被解析，而不是显示成原始符号 ----
  // render 必须经由同一个模块运行器加载，才能与组件共用同一份 svelte 服务端上下文。
  const { render } = await server.ssrLoadModule('svelte/server');
  const markdown = await server.ssrLoadModule('/src/components/MarkdownRenderer.svelte');
  const html = render(markdown.default, {
    props: { source: '- Rust：**332 个文件**；`Cargo.toml` 共 33 项\n- 普通条目\n\n段落里的 **粗体**' },
  }).body.replace(/<!--[\s\S]*?-->/gu, '');
  assert.match(html, /<li>[\s\S]*<strong>332 个文件<\/strong>/u, '列表项里的加粗必须渲染成 <strong>');
  assert.match(html, /<li>[\s\S]*<code[\s>][\s\S]*Cargo\.toml/u, '列表项里的行内代码必须渲染成 <code>');
  assert.doesNotMatch(html, /\*\*332|`Cargo/u, '不得残留原始的 ** 或反引号');
  assert.match(html, /<strong>粗体<\/strong>/u, '段落里的加粗保持正常');
}, {
  plugins: [svelte()],
  // 让 svelte-markdown 与组件共用同一个 svelte 运行时实例，服务端渲染才能取到组件上下文。
  ssr: { noExternal: ['@humanspeak/svelte-markdown'] },
});

console.log('conversation disclosure golden passed');
