import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { withGoldenViteServer } from './golden-vite.mjs';

const toolCallSource = await readFile(
  new URL('../src/components/ToolCall.svelte', import.meta.url),
  'utf8',
);
const terminalCardSource = await readFile(
  new URL('../src/components/TerminalSessionCard.svelte', import.meta.url),
  'utf8',
);
const conversationTurnSource = await readFile(
  new URL('../src/components/ConversationTurn.svelte', import.meta.url),
  'utf8',
);
const conversationToolGroupSource = await readFile(
  new URL('../src/components/ConversationToolGroup.svelte', import.meta.url),
  'utf8',
);
const conversationProcessRowSource = await readFile(
  new URL('../src/components/ConversationProcessRow.svelte', import.meta.url),
  'utf8',
);
const modelFailureCardSource = await readFile(
  new URL('../src/components/ModelFailureCard.svelte', import.meta.url),
  'utf8',
);

assert.match(
  modelFailureCardSource,
  /\.model-failure\s*\{[\s\S]*?width:\s*100%;[\s\S]*?max-width:\s*none;[\s\S]*?box-sizing:\s*border-box;/,
  '模型失败卡片必须跟随主消息区域占满可用宽度',
);
assert.doesNotMatch(
  modelFailureCardSource,
  /width:\s*min\(720px,\s*100%\)/,
  '模型失败卡片不得保留独立固定宽度上限',
);

for (const [component, source] of [
  ['TerminalSessionCard', terminalCardSource],
]) {
  assert.match(source, /let collapsed = \$state\(true\);/, `${component} 必须默认折叠`);
  assert.doesNotMatch(
    source,
    /collapsed = false/,
    `${component} 不得在运行中、完成或失败状态下自动展开`,
  );
  assert.match(
    source,
    /aria-expanded=\{!collapsed\}/,
    `${component} 的展开按钮必须暴露真实可访问状态`,
  );
}

assert.match(
  toolCallSource,
  /let collapsed = \$state\(untrack\(\(\) => presentationRole !== 'artifact' && presentationRole !== 'attention'\)\);/,
  '普通工具过程必须默认折叠，用户产物和待处理事项必须默认可见',
);
assert.match(
  toolCallSource,
  /presentationRole\?: ConversationPresentationRole;/,
  '工具卡必须接收统一的对话展示角色，而不是维护摘要模式专用实现',
);
assert.match(
  toolCallSource,
  /aria-expanded=\{!collapsed\}/,
  'ToolCall 的展开按钮必须暴露真实可访问状态',
);
assert.match(
  toolCallSource,
  /const approvalId = toolApproval\?\.approvalId \|\| '';[\s\S]*?collapsed = false;/,
  '待授权工具必须自动展开授权动作，但不得因普通运行态自动展开',
);
assert.match(
  toolCallSource,
  /const canExpand = \$derived\([\s\S]*?Boolean\(toolApproval\) \|\| hasError \|\| outcomeUnconfirmed \|\| \(!isCompactReadOnlyTool && !isCompactMutation\)/,
  '紧凑文件工具出现待授权状态时必须保留可展开的授权动作',
);

assert.match(
  toolCallSource,
  /const detailVisible = \$derived\(canExpand && !collapsed\);[\s\S]*?detailVisible && !outputIsStructuredError \? formatToolOutput\(name, output\) : ''/,
  '折叠工具卡不得在首屏格式化完整工具输出',
);
assert.match(
  toolCallSource,
  /detailVisible \? parseToolDiagramPayload\(name, output\) : null/,
  '折叠工具卡不得在首屏解析图表载荷',
);
assert.match(
  toolCallSource,
  /const viewImagePreview = \$derived\(\s*!outputIsStructuredError \? parseViewImagePreview\(name, output\) : null,/,
  '折叠 view_image 卡片必须保留直接打开图片所需的数据解析',
);
assert.match(
  toolCallSource,
  /const directImagePreview = viewImagePreview;[\s\S]*?imageDataUrl: directImagePreview\.src/,
  'view_image 文件入口必须把工具结果中的图片数据直接交给右栏预览',
);
assert.match(
  terminalCardSource,
  /const displayOutput = \$derived\(isExpanded \? formatOutput\(rawDisplayOutput\) : ''\);/,
  '折叠 Shell 卡片不得在首屏格式化终端输出',
);
assert.match(
  terminalCardSource,
  /error:\s*terminalPayloadErrorText\(structuredErrorPayload\)/,
  'Shell 卡片只能从已确认的错误载荷提取错误文本',
);
assert.match(
  terminalCardSource,
  /const showErrorHint = \$derived\.by\(\(\) => \{\s*if \(statusClass !== 'error'\)/,
  'Shell 执行提示只能在错误状态下显示',
);
assert.doesNotMatch(
  terminalCardSource,
  /const publicErrorText = \$derived\([\s\S]*?publicToolPayloadMessage\(terminal\?\.output\)/,
  'Shell 标准输出不得被当作公开错误提示',
);
// 呈现角色是投影写入消息的事实：原始与摘要两种模式都由 MessageItem 自己读取，
// 不能由某个调用方单独传入，否则两种模式对同一条消息（例如产物卡片）的默认展开会不一致。
{
  const messageItemSource = await readFile(new URL('../src/components/MessageItem.svelte', import.meta.url), 'utf8');
  assert.match(
    messageItemSource,
    /const presentationRole = \$derived\(inferConversationPresentationRole\(message\)\);/,
    'MessageItem 必须从消息本身推断呈现角色',
  );
  assert.doesNotMatch(messageItemSource, /presentationRole\?: ConversationPresentationRole/, 'MessageItem 不再接受外部传入的呈现角色');
}
// 过程流的组织逻辑在 conversation-disclosure.ts 里（组件只调用 buildConversationStreamEntries），
// 其追加稳定性另由 conversation-disclosure 黄金测试按行为验证。
assert.match(
  await readFile(new URL('../src/lib/conversation-disclosure.ts', import.meta.url), 'utf8'),
  /const flushToolGroup = \(\) => \{[\s\S]*?toolGroupItems = \[\];[\s\S]*?flushToolGroup\(\);\s*result\.push\(\{ kind: 'event'/,
  '非工具过程事件必须提交当前连续工具组，保持真实时间顺序',
);
assert.match(
  conversationTurnSource,
  /\{:else if durationLabel\}[\s\S]*?class="turn-status-header"/,
  '无过程的直接回复也必须显示一致的静态轮次状态行',
);
assert.doesNotMatch(
  conversationTurnSource,
  /TurnRuntimeSummary|responseCompletedAt/,
  '摘要模式只在轮次标题显示处理耗时，不得在轮次底部再渲染第二层时间信息',
);
assert.match(
  await readFile(new URL('../src/components/TurnRuntimeSummary.svelte', import.meta.url), 'utf8'),
  /showDuration\?: boolean;[\s\S]*?\{#if showDuration\}[\s\S]*?responseDurationLabel[\s\S]*?\{:else if normalizedCompletedAt !== null\}/,
  '时间组件必须支持只显示完成时间，原始模式默认仍显示耗时与完成时间',
);
assert.match(
  await readFile(new URL('../src/components/MarkdownRenderer.svelte', import.meta.url), 'utf8'),
  /rawtext:\s*MdText/,
  '文件引用识别必须挂在 rawtext 上，text 交给默认渲染器递归渲染嵌套行内 token',
);
assert.doesNotMatch(
  await readFile(new URL('../src/components/MarkdownRenderer.svelte', import.meta.url), 'utf8'),
  /\btext:\s*MdText/,
  '不得再用自定义 text 渲染器拦截紧凑列表项，否则列表里的加粗与行内代码会显示成原始符号',
);
assert.match(
  conversationToolGroupSource,
  /<MessageItem[\s\S]*?message=\{item\.message\}/,
  '工具组必须直接复用原始工具卡片，不能再套一层工具详情组件',
);
assert.doesNotMatch(
  conversationToolGroupSource,
  /ConversationToolItem/,
  '摘要模式不得保留重复工具详情实现',
);
assert.match(
  conversationToolGroupSource,
  /:global\(\.conversation-turn \.summary-tool-flow \.tool-call\)\s*\{[\s\S]*?border:\s*0;[\s\S]*?background:\s*transparent;/,
  '工具组内的原始工具卡片必须压平，避免出现嵌套卡片外框',
);
assert.doesNotMatch(
  conversationToolGroupSource,
  /<Icon name="plus"/,
  '工具组不得混入另一套彩色加号图标，折叠层统一使用 chevron',
);
assert.match(
  conversationToolGroupSource,
  /\.tool-group-list\s*\{[\s\S]*?padding:\s*0 0 1px;[\s\S]*?border-left:\s*0;/,
  '工具组不得增加水平偏移，右侧状态和箭头应与同层卡片保持共同列',
);
assert.doesNotMatch(
  conversationProcessRowSource,
  /line-clamp/,
  '用户展开轮次后必须能阅读完整过程文字',
);
assert.doesNotMatch(
  conversationProcessRowSource,
  /<Icon\s/,
  '普通过程行使用统一的中性节点，不得混入多套装饰图标',
);
assert.doesNotMatch(
  conversationTurnSource,
  /turn-process-entry/,
  '整轮折叠里的思考、文字阶段与独立工具组同级：不得再给工具组套一层额外缩进 / 引导线，缩进只存在于展开后的内容里',
);
assert.match(
  toolCallSource,
  /getAgentBrandInfo\(role, \(key\) => i18n\.t\(key\)\)/,
  'agent_spawn 角色角标必须通过当前语言翻译，不能直接展示英文模板名',
);

await withGoldenViteServer(async (server) => {
  const display = await server.ssrLoadModule('/src/lib/tool-call-display.ts');
  const agentColors = await server.ssrLoadModule('/src/lib/agent-colors.ts');
  const fileChange = await server.ssrLoadModule('/src/lib/canonical-tool-file-change.ts');
  const terminal = await server.ssrLoadModule('/src/lib/terminal-utils.ts');
  const toolCallFailure = await server.ssrLoadModule('/src/lib/tool-call-failure.ts');

  assert.equal(
    agentColors.getAgentVisualInfo(
      'executor',
      undefined,
      (key) => key === 'roleTemplate.executor.displayName' ? '执行工程师' : key,
    ).label,
    '执行工程师',
    '统一代理视觉信息必须保留角色翻译器',
  );

  assert.deepEqual(
    toolCallFailure.parseToolCallFailureDiagnostic({
      schemaVersion: 'tool-call-failure.v1',
      code: 'tool_arguments_invalid',
      summary: '模型连续提交无效的 shell_exec 工具参数；工具未执行，本轮已停止。',
      detail: '缺少必填参数 command',
      stage: 'tool_call_validation',
      toolName: 'shell_exec',
      reasonCode: 'tool_arguments_missing_required',
      missingFields: ['command'],
      argumentsPreview: '{}',
      retryAttempts: 1,
    }),
    {
      schemaVersion: 'tool-call-failure.v1',
      code: 'tool_arguments_invalid',
      summary: '模型连续提交无效的 shell_exec 工具参数；工具未执行，本轮已停止。',
      detail: '缺少必填参数 command',
      stage: 'tool_call_validation',
      toolName: 'shell_exec',
      reasonCode: 'tool_arguments_missing_required',
      missingFields: ['command'],
      argumentsPreview: '{}',
      retryAttempts: 1,
    },
    '重复无效工具调用必须保留真实工具名、缺失字段和参数摘要',
  );

  const { render } = await server.ssrLoadModule('svelte/server');
  const { default: ToolCardMeta } = await server.ssrLoadModule('/src/components/ToolCardMeta.svelte');
  const fileStatus = render(ToolCardMeta, { props: { status: 'success', duration: 1234 } }).body;
  assert.match(fileStatus, /status-without-chevron/, '无折叠箭头的状态保留外侧操作间距，与相邻工具圆点对齐');
  assert.doesNotMatch(fileStatus, /class="tool-card-chevron-slot/, '不可折叠工具不生成空箭头节点');
  const expandableStatus = render(ToolCardMeta, { props: { status: 'running', expanded: false } }).body;
  assert.doesNotMatch(expandableStatus, /class="[^"]*status-without-chevron/, '可折叠工具不重复增加右侧间距');
  const codeMeta = render(ToolCardMeta, { props: { expanded: false } }).body;
  assert.doesNotMatch(codeMeta, /class="[^"]*status-without-chevron|class="tool-card-status-slot/, '无状态的代码栏不保留中间空位');

  const failureCard = await server.ssrLoadModule('/src/components/ModelFailureCard.svelte');
  const toolCall = await server.ssrLoadModule('/src/components/ToolCall.svelte');
  const messageItem = await server.ssrLoadModule('/src/components/MessageItem.svelte');
  const { default: ToolCallRenderer } = await server.ssrLoadModule('/src/components/ToolCallRenderer.svelte');
  const truncatedBlock = (name) => ({
    id: `truncated-${name}`, type: 'tool_call', content: '',
    toolCall: {
      id: `truncated-${name}`, name, status: 'success', arguments: { path: 'README.md', command: 'cat README.md' },
      result: '历史输出预览',
      truncatedResult: { sessionId: 's', turnId: 't', itemId: 'i', omittedChars: 9235 },
    },
  });
  for (const name of ['file_read', 'shell_exec', 'web_fetch']) {
    const markup = render(ToolCallRenderer, { props: { block: truncatedBlock(name) } }).body;
    assert.doesNotMatch(markup, /data-testid="tool-output-truncated"/, `${name} 收起时不得在卡片外暴露历史输出提示`);
    if (name === 'file_read') {
      assert.doesNotMatch(markup, /aria-expanded=/, '成功文件读取保持原有直接打开文件入口');
    }
  }
  const expandedHistory = render(ToolCallRenderer, { props: {
    block: truncatedBlock('web_fetch'), presentationRole: 'artifact',
  } }).body;
  assert.match(expandedHistory, /aria-expanded="true"/, '产物工具默认展开');
  assert.match(expandedHistory, /class="tool-content[\s\S]*data-testid="tool-output-truncated"/, '历史输出加载入口必须位于已展开的详情内部');
  assert.match(expandedHistory, /9235/, '仍保留服务端报告的省略数量');
  const { resolveAgentChildStatus } = await server.ssrLoadModule('/src/lib/agent-child-status.ts');
  const staleProjection = { root_task: { task_id: 'root' }, agents: [], tasks: [{ task_id: 'child', status: 'failed', updated_at: 100 }] };
  assert.equal(resolveAgentChildStatus(staleProjection, 'child', 'completed', 200), 'completed', '旧失败快照不能覆盖独立继续后的完成状态');
  staleProjection.tasks[0] = { task_id: 'child', status: 'running', updated_at: 300 };
  assert.equal(resolveAgentChildStatus(staleProjection, 'child', 'completed', 200), 'running', '较新的 TaskStore 执行事实优先于旧完成记录');
  // 从完整 MessageItem 入口验证 metadata 经 BlockRenderer、ToolCallRenderer 到达卡片。
  for (const [childStatus, visualStatus] of [
    ['completed', 'success'], ['failed', 'error'], ['killed', 'cancelled'],
    ['degraded', 'degraded'], ['queued', 'pending'], ['running', 'running'],
  ]) {
    const markup = render(messageItem.default, { props: {
      message: {
        id: `spawn-${childStatus}`, role: 'assistant', source: 'orchestrator', agent: 'orchestrator',
        type: 'tool_call', content: '', timestamp: 1,
        metadata: { canonical: true, agentChildStatus: childStatus },
        blocks: [{ id: 'spawn-block', type: 'tool_call', content: '', toolCall: {
          id: 'spawn-call', name: 'agent_spawn', status: 'success', arguments: {},
          result: JSON.stringify({ status: 'started', child_task_id: 'historical-child' }),
        } }],
      },
      filePreviewScope: { sessionId: 'historical-session', workspaceId: '' },
    } }).body;
    assert.ok(markup.includes(`agent-spawn-card status-${visualStatus}`), `${childStatus} 必须渲染真实子任务状态`);
    if (!['running', 'queued'].includes(childStatus)) assert.doesNotMatch(markup, /status-dot pulsing/);
  }
  const selectedAgent = render(toolCall.default, {props:{name:'agent_spawn',status:'success',output:{status:'started',child_task_id:'selected-child',model_selection:{model:'glm-fixture',effective_effort:'max'}}}}).body;
  assert.match(selectedAgent, /glm-fixture/, '派发卡片展示服务端固定模型');
  assert.match(selectedAgent, /最高|Max/, '派发卡片展示服务端实际强度');
  const { getAgentRunColor } = await server.ssrLoadModule('/src/lib/agent-colors.ts');
  const { agentModelLabel } = await server.ssrLoadModule('/src/lib/agent-model-display.ts');
  const { i18n } = await server.ssrLoadModule('/src/stores/i18n.svelte.ts');
  const translate = (key) => i18n.t(key);
  assert.deepEqual(getAgentRunColor('selected-child'), getAgentRunColor('selected-child'), '同一 Task 识别色固定');
  assert.notDeepEqual(getAgentRunColor('child-0'), getAgentRunColor('child-1'), '多个同角色代理按 Task 区分颜色');
  assert.equal(agentModelLabel({ model: 'historical' }, translate), 'historical', '历史未记录等级时不编造默认等级');
  assert.equal(agentModelLabel({ model: 'historical', effective_effort: 'future-level' }, translate), 'historical');
  assert.equal(agentModelLabel(null, translate), '', '缺少模型快照时不使用当前角色配置补写');
  for (const compactAgent of [false, true]) {
    for (const effort of ['none', 'low', 'max', null]) {
      const label = agentModelLabel({ model: 'snapshot-model', effective_effort: effort }, translate);
      const props = {
        name: 'agent_spawn', compactAgent, status: 'success',
        input: { role: 'explorer', reasoning_effort: 'high' },
        output: JSON.stringify({ status: 'started', child_task_id: 'selected-child', model_selection: { model: 'snapshot-model', effective_effort: effort } }),
      };
      const markup = render(toolCall.default, { props }).body;
      assert.ok(markup.includes(label), '两种模式展示回执的模型与实际强度，不能拿请求强度代替');
      assert.ok(markup.includes(`--agent-accent: ${getAgentRunColor('selected-child').color}`));
      assert.match(markup, /data-agent-task-id="selected-child"/, '颜色和详情保持原 Task 身份');
      const stopped = render(toolCall.default, { props: { ...props, agentChildStatus: 'killed' } }).body;
      assert.match(stopped, /status-cancelled/);
      assert.match(stopped, /data-icon="stop"/);
      assert.doesNotMatch(stopped, /代理运行失败|spinning/, '已停止不显示运行或失败状态');
    }
  }
  const { getAgentRunState } = await server.ssrLoadModule('/src/stores/agent-run-store.svelte.ts');
  const projectionState = getAgentRunState('model-snapshot-session', '');
  projectionState.projection = {
    root_task: { task_id: 'snapshot-root' }, agents: [],
    tasks: [{ task_id: 'snapshot-child', status: 'completed', executor_binding: { model_selection: { model: 'persisted-model', effective_effort: 'low' } } }],
  };
  const snapshotCard = render(toolCall.default, { props: {
    name: 'agent_spawn', status: 'success',
    filePreviewScope: { sessionId: 'model-snapshot-session', workspaceId: '' },
    output: { status: 'started', child_task_id: 'snapshot-child' },
  } }).body;
  assert.match(snapshotCard, /persisted-model/, '派发回执缺少模型时可读取同一 Task 的已存快照');
  assert.match(snapshotCard, /status-success/, '补充模型信息不能覆盖任务终态');
  projectionState.projection = null;
  const compactReadFailure = render(toolCall.default, { props: {
    name:'file_read', id:'compact-read-failure', status:'error',
    input:{path:'missing.ts'}, error:'读取路径失败：路径不存在',
  } }).body;
  assert.match(compactReadFailure, /aria-expanded="false" aria-controls="tool-call-detail-compact-read-failure"/, '只读工具失败后可展开诊断，不会丢失错误详情');
  assert.match(compactReadFailure, /status-error/, '只读失败仍保留红点');
  const recoveredCard = render(toolCall.default, { props: {
    name: 'agent_spawn', status: 'success', agentChildStatus: 'completed',
    output: { status: 'failed', child_task_id: 'recovered-child' },
  } }).body;
  assert.match(recoveredCard, /agent-spawn-card status-success/);
  assert.doesNotMatch(recoveredCard, /class="agent-spawn-error"/, '恢复成功后不再显示旧派发回执中的失败提示');
  const agentGroup = await server.ssrLoadModule('/src/components/ConversationAgentGroup.svelte');
  const failedGroup = render(agentGroup.default, { props: {
    items: [{ key: 'failed-spawn', message: { blocks: [{ toolCall: {
      name: 'agent_spawn', status: 'success', result: { status: 'failed' },
    } }] } }],
    filePreviewScopeForItem: () => ({ sessionId: 'historical-session', workspaceId: '' }),
  } }).body;
  assert.match(failedGroup, /agent-spawn-card status-error/, '摘要代理行显示真实失败状态');
  assert.doesNotMatch(failedGroup, /agent-group-header|aria-expanded/, '单个代理直接显示任务行，不再增加分组折叠');
  assert.match(failedGroup, /compact-agent/, '摘要模式复用紧凑任务行');
  const multipleAgents = render(agentGroup.default, { props: {
    items: ['completed', 'running'].map((agentChildStatus, index) => ({ key: `agent-${index}`, message: {
      metadata: { agentChildStatus }, blocks: [{ id: `block-${index}`, type: 'tool_call', toolCall: {
        name: 'agent_spawn', status: 'success', arguments: { display_name: `任务 ${index}`, role: 'executor' },
        result: { status: 'started', child_task_id: `child-${index}` },
      } }],
    } })),
    filePreviewScopeForItem: () => ({ sessionId: 'historical-session', workspaceId: '' }),
  } }).body;
  assert.match(multipleAgents, /2 个代理/);
  assert.match(multipleAgents, /任务 0/);
  assert.match(multipleAgents, /任务 1/);
  assert.match(multipleAgents, /status-success/);
  assert.match(multipleAgents, /status-running/);
  assert.doesNotMatch(multipleAgents, /class="agent-spawn-role"|查看详情<|aria-expanded/, '摘要不重复角色标签、详情文案和折叠层');
  const originalAgent = render(toolCall.default, { props: {
    name: 'agent_spawn', status: 'success', agentChildStatus: 'completed',
    input: { display_name: '检查数据加载', role: 'explorer' },
    output: { status: 'started', child_task_id: 'existing-child' },
  } }).body;
  assert.match(originalAgent, /agent-spawn-role/, '详细模式保留角色标识，与右侧代理详情对应');
  assert.match(originalAgent, /查看详情/, '详细模式保留详情操作的无障碍说明');
  assert.match(originalAgent, /data-icon="check-circle"/, '代理完成状态使用独立形状标识');
  assert.match(originalAgent, /<span[^>]*>已完成<\/span>/, '代理状态可直接阅读，不依赖颜色');
  assert.doesNotMatch(originalAgent, /agent-spawn-role-badge/, '角色身份无需额外的胶囊色块');
  assert.doesNotMatch(originalAgent, /compact-agent/, '详细模式保留任务卡片外观');
  assert.match(originalAgent, /data-agent-task-id="existing-child"/, '两种风格均打开原 Task');
  for (const [errorCode, errorMessage] of [
    ['file_patch_no_match', '目标内容与当前文件不匹配，请重新读取文件后再修改'],
    ['file_patch_ambiguous_match', '目标内容在当前文件中不是唯一匹配，请增加上下文后重试'],
    ['file_patch_not_applicable', 'patch 与当前文件内容不匹配，请重新读取文件并生成精确修改'],
    ['file_patch_invalid_input', 'patches[1] 缺少 new_string 字段'],
  ]) {
    const mismatchMarkup = render(toolCall.default, {
      props: {
        name: 'file_patch',
        status: 'error',
        input: { path: 'backend/internal/handlers/app.go' },
        error: JSON.stringify({
          tool: 'file_patch',
          status: 'failed',
          error_code: errorCode,
          error: errorMessage,
        }),
      },
    }).body;
    assert.match(mismatchMarkup, /输入不匹配/);
    assert.ok(mismatchMarkup.includes(errorMessage));
    assert.doesNotMatch(mismatchMarkup, /写入失败|检查文件是否可写/);
  }

  const writeFailureMarkup = render(toolCall.default, {
    props: {
      name: 'file_patch',
      status: 'error',
      input: { path: 'backend/internal/handlers/app.go' },
      error: JSON.stringify({
        tool: 'file_patch',
        status: 'failed',
        error_code: 'file_patch_failed',
        error: '文件暂不可写入，请检查路径或权限',
      }),
    },
  }).body;
  assert.match(writeFailureMarkup, /写入失败/);
  assert.match(writeFailureMarkup, /文件暂不可写入，请检查路径或权限/);
  assert.doesNotMatch(writeFailureMarkup, /输入不匹配/);

  for (const [errorCode, errorMessage] of [
    ['file_patch_read_failed', '文件暂不可读取，请检查路径或权限'],
    ['file_patch_path_resolution_failed', '路径暂不可解析，请检查工作区或路径'],
  ]) {
    const runtimeFailureMarkup = render(toolCall.default, {
      props: {
        name: 'file_patch',
        status: 'error',
        input: { path: 'backend/internal/handlers/app.go' },
        error: JSON.stringify({
          tool: 'file_patch',
          status: 'failed',
          error_code: errorCode,
          error: errorMessage,
        }),
      },
    }).body;
    assert.match(runtimeFailureMarkup, /执行异常/);
    assert.ok(runtimeFailureMarkup.includes(errorMessage));
    assert.doesNotMatch(runtimeFailureMarkup, /输入不匹配|写入失败|检查文件是否可写/);
  }

  const invalidToolMarkup = render(failureCard.default, {
    props: {
      failure: toolCallFailure.parseToolCallFailureDiagnostic({
        schemaVersion: 'tool-call-failure.v1',
        code: 'tool_arguments_invalid',
        summary: '模型连续提交无效的 shell_exec 工具参数；工具未执行，本轮已停止。',
        detail: '工具：shell_exec\n直接原因：缺少必填参数 command\n收到的参数：{}',
        stage: 'tool_call_validation',
        toolName: 'shell_exec',
        reasonCode: 'tool_arguments_missing_required',
        missingFields: ['command'],
        argumentsPreview: '{}',
        retryAttempts: 1,
      }),
    },
  }).body;
  assert.match(invalidToolMarkup, /工具调用格式错误/);
  assert.match(invalidToolMarkup, /shell_exec/);
  assert.match(invalidToolMarkup, /缺少必填参数 command/);
  assert.match(invalidToolMarkup, /tool_arguments_invalid/);

  const unavailableToolMarkup = render(failureCard.default, {
    props: {
      failure: toolCallFailure.parseToolCallFailureDiagnostic({
        schemaVersion: 'tool-call-failure.v1',
        code: 'tool_not_available',
        summary: '模型连续调用本轮未提供的 shell_exec 工具；工具未执行，本轮已停止。',
        detail: '工具：shell_exec\n直接原因：当前运行环境未提供工具 shell_exec。',
        stage: 'tool_call_validation',
        toolName: 'shell_exec',
        reasonCode: 'tool_not_available',
        missingFields: [],
        argumentsPreview: '{"command":"pwd"}',
        retryAttempts: 1,
      }),
    },
  }).body;
  assert.match(unavailableToolMarkup, /工具不可用/);
  assert.match(unavailableToolMarkup, /校验工具可用性/);
  assert.match(unavailableToolMarkup, /重新选择可用工具/);
  assert.match(unavailableToolMarkup, /tool_not_available/);
  assert.doesNotMatch(unavailableToolMarkup, /工具调用格式错误/);

  assert.equal(
    terminal.resolveTerminalArgumentId({ command: 'npm test', action: 'run', terminal_id: 0 }),
    undefined,
    '前台 shell_exec 不得把模型占位 terminal_id=0 显示为真实终端会话',
  );
  assert.equal(
    terminal.resolveTerminalArgumentId({ action: 'read', terminal_id: 12 }),
    12,
    '后台终端控制动作必须保留真实 terminal_id',
  );
  assert.equal(
    terminal.terminalPayloadOutput({
      status: 'failed',
      stdout: '',
      stderr: 'npm ERR! Unknown option --runInBand',
    }),
    'npm ERR! Unknown option --runInBand',
    '失败的 shell_exec 必须把 stderr 作为终端输出展示',
  );
  assert.equal(
    terminal.terminalPayloadErrorText({
      status: 'failed',
      error_code: 'shell_exec_working_directory_unavailable',
      error: '当前工作区目录不可访问，请重新选择工作区',
    }),
    '当前工作区目录不可访问，请重新选择工作区',
    '失败的 shell_exec 必须展示结构化公开错误，不能退回通用提示',
  );

  const terminalCard = await server.ssrLoadModule('/src/components/TerminalSessionCard.svelte');
  const failedTerminalMarkup = render(terminalCard.default, {
    props: {
      status: 'error',
      toolCall: {
        id: 'shell-failure',
        name: 'shell_exec',
        arguments: {
          command: 'npm test -- --runInBand',
          action: 'run',
          terminal_id: 0,
        },
        status: 'error',
        error: JSON.stringify({
          status: 'failed',
          error_code: 'shell_exec_failed',
          error: '测试命令参数无效',
          stderr: 'npm ERR! Unknown option --runInBand',
          exit_code: 1,
        }),
      },
    },
  }).body;
  assert.doesNotMatch(failedTerminalMarkup, /终端会话 #0|data-terminal-id="0"/);
  assert.match(failedTerminalMarkup, /class="[^"]*\btool-call terminal-call [^"]*\bcollapsed"/);
  assert.match(failedTerminalMarkup, /npm test -- --runInBand/);
  assert.match(failedTerminalMarkup, /测试命令参数无效/);
  assert.doesNotMatch(
    failedTerminalMarkup,
    /npm ERR! Unknown option --runInBand/,
    'Shell 详细输出必须在用户展开前保持隐藏',
  );
  assert.doesNotMatch(failedTerminalMarkup, /占用状态[^<]*否/);

  assert.deepEqual(
    display.coerceToolArgumentsRecord('src/App.svelte'),
    { input: 'src/App.svelte' },
    'raw tool arguments must survive projection instead of becoming an empty object',
  );

  assert.deepEqual(
    display.resolveToolCardTarget({
      toolName: 'file_read',
      input: { file_path: 'src/App.svelte' },
    }),
    { primaryPath: 'src/App.svelte', paths: ['src/App.svelte'] },
    'file_read should display file_path in the card title',
  );

  assert.deepEqual(
    display.resolveToolCardTarget({
      toolName: 'file_patch',
      input: { filePath: 'src/lib/state.ts' },
    }),
    { primaryPath: 'src/lib/state.ts', paths: ['src/lib/state.ts'] },
    'file_patch should display filePath in the card title',
  );

  assert.deepEqual(
    display.resolveToolCardTarget({
      toolName: 'image_generate',
      input: { prompt: 'blue square' },
      output: JSON.stringify({
        status: 'succeeded',
        path: 'generated-images/blue-square.png',
      }),
    }),
    {
      primaryPath: 'generated-images/blue-square.png',
      paths: ['generated-images/blue-square.png'],
    },
    'image_generate should locate its generated workspace file from the tool result',
  );

  assert.deepEqual(
    display.resolveToolCardTarget({
      toolName: 'apply_patch',
      input: {
        patch: [
          '*** Begin Patch',
          '*** Update File: src/App.svelte',
          '@@',
          '-old',
          '+new',
          '*** End Patch',
        ].join('\n'),
      },
    }),
    { primaryPath: 'src/App.svelte', paths: ['src/App.svelte'] },
    'apply_patch should derive the target from patch text before output is available',
  );

  assert.deepEqual(
    display.resolveToolCardTarget({
      toolName: 'apply_patch',
      input: {},
      output: JSON.stringify({
        tool: 'apply_patch',
        status: 'succeeded',
        changed_paths: ['src/App.svelte', 'src/lib/state.ts'],
      }),
    }),
    { paths: ['src/App.svelte', 'src/lib/state.ts'] },
    'apply_patch should display changed_paths from the tool result when multiple files changed',
  );

  const filePatchBlocks = fileChange.buildCanonicalToolFileChangeBlocks({
    blockIdBase: 'call-file-patch',
    sessionId: 'session-a',
    toolName: 'file_patch',
    arguments: {
      path: 'styles.css',
      old_string: 'button,\ninput {',
      new_string: 'button,\ninput,\nselect {',
    },
    result: {
      status: 'succeeded',
      path: 'styles.css',
    },
    status: 'success',
  });
  assert.equal(filePatchBlocks.length, 1, 'file_patch success should project as one file_change block');
  assert.equal(filePatchBlocks[0].type, 'file_change', 'file_patch should not render as raw JSON ToolCall');
  assert.equal(filePatchBlocks[0].fileChange.filePath, 'styles.css');
  assert.equal(filePatchBlocks[0].fileChange.changeType, 'modify');
  assert.match(filePatchBlocks[0].fileChange.diff, /-button,/);
  assert.match(filePatchBlocks[0].fileChange.diff, /\+select \{/);

  const applyPatchBlocks = fileChange.buildCanonicalToolFileChangeBlocks({
    blockIdBase: 'call-apply-patch',
    sessionId: 'session-a',
    toolName: 'apply_patch',
    arguments: {
      patch: [
        '*** Begin Patch',
        '*** Update File: src/App.svelte',
        '@@',
        '-old',
        '+new',
        '*** End Patch',
      ].join('\n'),
    },
    result: { status: 'succeeded' },
    status: 'success',
  });
  assert.equal(applyPatchBlocks.length, 1, 'apply_patch success should project patch text as file_change');
  assert.equal(applyPatchBlocks[0].fileChange.filePath, 'src/App.svelte');
  assert.match(applyPatchBlocks[0].fileChange.diff, /-old/);
  assert.match(applyPatchBlocks[0].fileChange.diff, /\+new/);

  // 浏览器工具卡片：摘要只展示对人有意义的参数，browser_read 展示正文而不是 JSON。
  const browserDisplay = await server.ssrLoadModule('/src/lib/browser-tool-display.ts');
  assert.equal(browserDisplay.browserToolSummary('file_read', { path: 'a' }), null, '非浏览器工具交回通用摘要');
  assert.equal(browserDisplay.browserToolSummary('browser_navigate', { url: 'https://example.com' }), 'https://example.com');
  assert.equal(browserDisplay.browserToolSummary('browser_navigate', { action: 'back' }), 'back');
  assert.equal(browserDisplay.browserToolSummary('browser_type', { element_ref: 'e:3:1', text: 'magi' }), '“magi”');
  assert.equal(browserDisplay.browserToolSummary('browser_click', { element_ref: 'e:3:12' }), '', '元素引用对用户没有意义，不显示');
  const clickResult = JSON.stringify({ tool: 'browser_click', status: 'succeeded', target: { role: 'button', name: '提交订单' } });
  assert.equal(browserDisplay.browserToolSummary('browser_click', { element_ref: 'e:3:12' }, clickResult), '提交订单', '显示实际点击的元素名称');
  assert.equal(
    browserDisplay.browserToolSummary('browser_click', {}, JSON.stringify({ status: 'succeeded', target: { role: 'link', name: null } })),
    'link',
    '元素没有名称时退回角色',
  );
  assert.equal(
    browserDisplay.browserToolSummary('browser_type', { text: 'magi' }, JSON.stringify({ status: 'succeeded', target: { role: 'searchbox', name: '搜索' } })),
    '“magi” → 搜索',
  );
  assert.equal(
    browserDisplay.browserToolSummary('browser_hover', {}, JSON.stringify({ status: 'succeeded', result: { hovered: true, target: { role: 'menuitem', name: '设置' } } })),
    '设置',
    'hover 的目标在 DevTools 操作结果里',
  );
  assert.deepEqual(
    browserDisplay.parseBrowserScreenshotPreview('browser_screenshot', JSON.stringify({
      tool: 'browser_screenshot', status: 'succeeded', path: '/state/browser/artifacts/s1/browser-shot-1-0.png', mime: 'image/png', bytes: 42,
    })),
    { path: '/state/browser/artifacts/s1/browser-shot-1-0.png', fileName: 'browser-shot-1-0.png', mime: 'image/png', bytes: 42 },
  );
  assert.equal(
    browserDisplay.parseBrowserScreenshotPreview('browser_screenshot', JSON.stringify({ status: 'failed', path: '/x.png', mime: 'image/png' })),
    null,
    '失败的截图没有预览',
  );
  assert.equal(browserDisplay.browserToolSummary('browser_read', { query: '价格' }), '“价格”');
  assert.equal(browserDisplay.browserToolSummary('browser_viewport', { action: 'set', mode: 'fixed', width: 390, height: 844 }), 'fixed 390×844');
  assert.equal(browserDisplay.browserToolSummary('browser_upload_file', { file_paths: ['/tmp/a/x.png', '/tmp/a/y.pdf'] }), 'x.png, y.pdf');
  assert.equal(browserDisplay.browserToolSummary('browser_storage', { area: 'local', action: 'set', key: 'feature_flag', value: 'on' }), 'local set feature_flag', '存储摘要不展示写入的值');
  assert.equal(browserDisplay.browserToolSummary('browser_storage', { area: 'cookies', action: 'list' }), 'cookies list');
  assert.equal(browserDisplay.browserToolSummary('browser_download', { action: 'list' }), 'list');
  assert.equal(browserDisplay.browserToolSummary('browser_download', { action: 'save', download_id: 'd1', destination_path: 'build/assets/report.pdf' }), 'report.pdf', '保存下载只展示目标文件名');
  const readPayload = JSON.stringify({
    tool: 'browser_read',
    status: 'succeeded',
    result: { url: 'https://example.com', title: 'Example', text: '# 标题\n正文内容', total_chars: 9, truncated: false },
  });
  assert.equal(browserDisplay.formatBrowserReadToolOutput('browser_read', readPayload), '# 标题\n正文内容');
  const queryPayload = { tool: 'browser_read', status: 'succeeded', result: { query: '价格', match_count: 2, matches: [{ index: 3, context: '本月价格上涨' }, { index: 40, context: '价格 说明' }] } };
  assert.equal(browserDisplay.formatBrowserReadToolOutput('browser_read', queryPayload), '- …本月价格上涨…\n- …价格 说明…');
  assert.equal(browserDisplay.formatBrowserReadToolOutput('browser_click', readPayload), null);
  assert.equal(browserDisplay.formatBrowserReadToolOutput('browser_read', 'not json'), null, '结构不符时交回通用格式化');

  console.log('tool call display golden replay passed');
}, { configFile: './vite.web.config.ts' });
