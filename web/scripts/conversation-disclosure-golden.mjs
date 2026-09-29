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

  // ---- 最终输出：工具调用不能靠耗时元数据冒充回答（否则会以原始卡片风格混进摘要视图）----
  const toolCallBlock = { id: 't', type: 'tool_call', content: '', toolCall: { name: 'shell_exec', status: 'success' } };
  assert.equal(
    disclosure.isConversationFinalMessage(message({ content: '答案', metadata: { responseDurationMs: 1200 } })),
    true,
    '带耗时的文字回答仍是最终输出',
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
