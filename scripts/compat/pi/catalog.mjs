// pi（earendil-works/pi-mono，packages/coding-agent）扩展 API → Magi 能力映射规则。
//
// 依据：docs/extensions.md（3367 行；2026-11-05 读取 main 分支）。已读行号范围：
//   [events]    123–1165  扩展位置、可用导入、写法、事件（会话/Agent/模型/工具/Bash/输入）
//   [context]   1165–1320 ExtensionContext 概览（ui/mode/cwd/sessionManager/modelRegistry/signal/shutdown/compact/getSystemPrompt）
//   [api]       1567–1975 ExtensionAPI 方法（on/registerTool/sendMessage/sendUserMessage/appendEntry/会话名/标签/
//                         registerCommand/getCommands/快捷键/flag/exec/工具启停/setModel/thinking/events）
//   [provider]  1974–2140 registerProvider/unregisterProvider/virtualModel/MCP/ToolRenderer/getSettings
//   [tool]      2180–2360 自定义工具与工具定义
// 未读：1320–1567（ExtensionCommandContext：newSession/fork/navigateTree/switchSession/reload）、
//       2360–3367（覆盖内置工具、远程执行、截断、渲染、动态加载、暴露、调用其他工具、MCP、自定义 UI、模式、示例）。
// 这些未读区域对应的 API 一律走 unknown，由人工归类后补进本表——不凭名字猜。
import { classifyWith, rule } from '../core/rules.mjs';

const RAW = 'https://raw.githubusercontent.com/badlogic/pi-mono/main/packages/coding-agent/docs/extensions.md';
const at = (range) => `${RAW} 行 ${range}`;
const EVENTS = at('307–1163');
const API = at('1567–1975');
const PROVIDER = at('1974–2140');
const TOOL = at('2180–2360');
const CONTEXT = at('1165–1320');

/** 翻译器当前已实现的 pi 侧能力。pi 翻译器尚未实现运行时，因此为空。 */
export const PI_IMPLEMENTED = new Set();

export const PI_RULES = [
  // —— 订阅入口：语义由事件名条目判定，这里只确认入口本身是 pi 的公开 API ——
  rule('calls', /^pi\.(on|once)$/, { fixed: 'shim', group: 'runtime', target: '兼容层的事件订阅表', note: '事件名是字面量时按事件条目判定；事件名不是字面量会记为 <dynamic> 并阻塞', source: API }),
  // —— 贡献类 ——
  rule('calls', /^pi\.registerTool$/, { capability: 'tool.register', fidelity: 'partial', note: '参数 schema 与结果可映射；execute 的 onUpdate 流式进度、terminate、executionMode、defaultActive/exposure、promptSnippet/Guidelines、渲染函数无对应，运行期遇到会被明确拒绝', source: TOOL }),
  rule('calls', /^pi\.registerCommand$/, { capability: 'command.register', fidelity: 'partial', note: '命令名与描述可映射；getArgumentCompletions 无对应；同名命令的 :1/:2 后缀由 Magi 以插件 ID 命名空间代替', source: API }),
  rule('calls', /^pi\.registerProvider$/, { capability: 'modelSource.publish', fidelity: 'partial', note: '静态/动态模型目录与 baseUrl+apiKey 可经本机 OpenAI 兼容端点接入；oauth /login、自定义 streamSimple、覆盖内置供应商（只追加不覆盖）无对应', source: PROVIDER }),
  rule('calls', /^pi\.unregisterProvider$/, { capability: 'modelSource.publish', fidelity: 'full', note: '对应 modelSource.clear', source: PROVIDER }),
  rule('calls', /^pi\.registerVirtualModel$/, { capability: 'modelSource.publish', fidelity: 'none', note: '把请求路由到物理模型；Magi 的模型选择由会话/角色绑定决定，扩展不能路由', source: PROVIDER, needs: '无；模型漂移会破坏角色模型绑定不变量' }),
  rule('calls', /^pi\.(registerMcpServer|unregisterMcpServer|getMcpServers)$/, { capability: 'tool.register', fidelity: 'none', note: 'MCP 是 Magi 内置功能，不经插件注册', source: PROVIDER }),
  rule('calls', /^pi\.getSettings$/, { capability: 'settings.schema', fidelity: 'partial', note: '读到的是 Magi 的插件设置而不是 pi 的全局设置', source: PROVIDER }),

  // —— 会话与消息：扩展不能写 canonical 日志、不能驱动 Turn ——
  rule('calls', /^pi\.(sendMessage|sendUserMessage)$/, { capability: 'service.spawn', fidelity: 'none', group: 'session', note: '向会话注入消息或触发新 Turn；Magi 的扩展不能写 canonical 日志也不能驱动 Turn', source: API, needs: '无；与不变量冲突' }),
  rule('calls', /^pi\.(appendEntry|setSessionName|getSessionName|setLabel)$/, { capability: 'service.snapshot', fidelity: 'none', group: 'session', note: '写会话条目/名称/标签；会话事实由 Magi 独有', source: API }),
  rule('calls', /^pi\.(setModel|setThinkingLevel|getThinkingLevel)$/, { capability: 'service.model', fidelity: 'none', group: 'llm', note: '改变会话模型或推理档位；扩展不能改变角色模型绑定', source: API }),
  rule('calls', /^pi\.(getActiveTools|getAllTools|setActiveTools)$/, { capability: 'tool.register', fidelity: 'none', note: '运行期启停工具；Magi 的工具面由原生权限与角色绑定决定', source: API }),
  rule('calls', /^pi\.exec$/, { fixed: 'bundled', group: 'runtime', target: '直接派生进程（完全信任扩展的 OS 能力）', note: '不经 Magi 工具审批；信任告知必须覆盖', source: API }),
  rule('calls', /^pi\.events(\.|$)/, { fixed: 'shim', group: 'runtime', target: '兼容层内的扩展间事件总线（同一插件内）', note: '仅限同一插件包内的模块；跨插件通信无对应', source: API }),
  rule('calls', /^pi\.(registerShortcut|registerFlag|getFlag|getCommands)$/, { capability: 'ui.view', fidelity: 'none', group: 'ui', note: '快捷键、CLI 标志属于 pi 的终端形态', source: API }),
  rule('calls', /^pi\.(registerMessageRenderer|registerMarkdownTransformer|registerEntryRenderer|registerToolRenderer)$/, { capability: 'ui.view', fidelity: 'none', group: 'ui', note: 'TUI 渲染组件；Magi 视图是独立沙箱页面', source: API }),

  // —— ctx（事件与命令处理器里的第二个参数）——
  rule('calls', /^ctx\.ui\.notify$/, { capability: 'service.notify', fidelity: 'partial', group: 'ui', note: '映射为 notify.user；只能在工具/命令调用租约内使用，钩子处理器里调用会被 Magi 以 ReentryDenied 拒绝；级别（info/warning/error）无对应，只保留文本', source: CONTEXT }),
  rule('calls', /^ctx\.ui\.(confirm|select|input)$/, { capability: 'service.ask', fidelity: 'partial', group: 'ui', note: '映射为 ask.user；同样只能在工具/命令调用租约内使用；editor/custom 无对应', source: CONTEXT }),
  rule('calls', /^ctx\.ui(\.|$)/, { capability: 'ui.view', fidelity: 'none', group: 'ui', note: 'setStatus/setWidget/footer/editor/custom 等 TUI 组件在 Magi 里没有对应', source: CONTEXT }),
  rule('calls', /^ctx\.(mode|hasUI|cwd|isProjectTrusted)$/, { fixed: 'shim', group: 'runtime', target: '兼容层提供固定值：mode="print"、hasUI=false、cwd=工作区', note: 'Magi 里没有 pi 的运行模式。pi 文档（行 1175）：hasUI 在 TUI/RPC 为 true，在 print/JSON 为 false；兼容层声明 print，使扩展走无界面分支，而不是声称存在 RPC 界面', source: CONTEXT }),
  rule('calls', /^ctx\.sessionManager(\.|$)/, { capability: 'service.snapshot', fidelity: 'partial', group: 'session', note: '只读且有界：最多 10 Turn、每 Turn 20 条文本；getEntries/getBranch/getTree 的完整树不提供', source: CONTEXT }),
  rule('calls', /^ctx\.(modelRegistry|model|thinkingLevel|scopedModels)(\.|$)/, { capability: 'service.model', fidelity: 'partial', group: 'llm', note: 'model.invoke 只接受 prompt 文本；streamSimple/complete、图像与分类模型无对应', source: CONTEXT }),
  rule('calls', /^ctx\.signal$/, { fixed: 'shim', group: 'runtime', target: '调用的取消信号', source: CONTEXT }),
  rule('calls', /^ctx\.(isIdle|abort|hasPendingMessages|waitForIdle|shutdown|compact|getContextUsage|getSystemPrompt|getSystemPromptOptions)$/, { capability: 'service.snapshot', fidelity: 'none', group: 'session', note: '运行状态与压缩/关停控制归 Magi 原生；扩展不能驱动', source: CONTEXT }),
  rule('calls', /^ctx\.(tools|executeTool)(\.|$)/, { capability: 'service.tools', fidelity: 'partial', note: '只能调用公开原生工具，且需要活动 Task；命令上下文没有 Task 会明确拒绝', source: at('2790–2800（仅标题，正文未读）') }),
  rule('calls', /^ctx\.(newSession|fork|navigateTree|switchSession|reload)$/, { fixed: 'unknown', note: 'ExtensionCommandContext 方法所在区域（行 1320–1567）未读，待评审；预期无对应（会话生命周期归 Magi）' }),

  // —— 事件 ——
  rule('events', /^session_start$/, { capability: 'hook.session.created', fidelity: 'partial', note: '会话创建可观察；reason（startup/reload/new/resume/fork）与 previousSessionFile 无对应', source: at('469–612') }),
  rule('events', /^session_shutdown$/, { capability: 'hook.session.deleted', fidelity: 'partial', note: '只有会话删除事实；reload/new/resume 的关停时机无对应。扩展自己的清理由 Magi 在停用时终止宿主进程保证', source: at('469–612') }),
  rule('events', /^session_(before_switch|before_fork|before_tree|tree|info_changed)$/, { capability: 'hook.session.created', fidelity: 'none', note: '会话树导航/切换/分叉是 pi 的会话模型；Magi 会话生命周期归内核', source: at('469–612') }),
  rule('events', /^session_before_compact$/, { capability: 'hook.compaction.before', fidelity: 'partial', note: 'cancel→block；compaction.summary→summary（需 contextRewrite 授权并过质量门禁）；firstKeptEntryId/tokensBefore 由 Magi 决定', source: at('469–612'), needs: '无' }),
  rule('events', /^session_compact(_failed)?$/, { capability: 'hook.turn.end', fidelity: 'none', note: '压缩完成/失败通知；Magi 没有对应的观察钩子', source: at('469–612'), needs: '观察类钩子 compaction.after（低成本，可扩）' }),
  rule('events', /^input$/, { capability: 'hook.input.guard', fidelity: 'partial', note: 'continue→continue；handled→block；transform 改写输入无对应（Magi input 只能继续或阻止）', source: at('1110–1163'), needs: 'input 的 advise 模式（已在目录但未接线；需评估对幂等重放的影响）' }),
  rule('events', /^before_agent_start$/, { capability: 'hook.prompt.contribute', fidelity: 'partial', note: 'systemPrompt 追加→带稳定性的参考片段；整体替换系统提示、注入持久消息无对应', source: at('613–930') }),
  rule('events', /^(turn_start|agent_start)$/, { capability: 'hook.turn.accepted', fidelity: 'partial', note: 'Turn 接纳可观察；pi 的低层 run/turn 分界无对应', source: at('613–930') }),
  rule('events', /^(turn_end|agent_end)$/, { capability: 'hook.turn.end', fidelity: 'partial', note: '终态事实可观察；turn_end 返回 entries/continue 驱动续跑无对应', source: at('613–930') }),
  rule('events', /^agent_before_settle$/, { capability: 'hook.turn.end', fidelity: 'none', note: '最后一道可行动边界：追加条目并请求续跑；扩展不能驱动 Turn 也不能写 canonical 日志', source: at('613–930'), needs: '无；与不变量冲突' }),
  rule('events', /^agent_settled$/, { capability: 'hook.turn.end', fidelity: 'full', note: '终态通知', source: at('613–930') }),
  rule('events', /^(message_start|message_update)$/, { capability: 'hook.model.response', fidelity: 'none', note: '逐消息/逐 token 流事件不对扩展开放', source: at('780–930') }),
  rule('events', /^message_end$/, { capability: 'hook.model.response', fidelity: 'none', note: '替换已落盘消息违反“不写 canonical 日志”', source: at('780–930') }),
  rule('events', /^context$/, { capability: 'hook.context.assemble', fidelity: 'partial', note: '返回 messages 的整体替换→受控 rewrites（区间替换）；需 contextRewrite 授权并由原生投影裁决，缓存归因进入 ModelStep', source: at('780–930') }),
  rule('events', /^context_with_system$/, { capability: 'hook.context.assemble', fidelity: 'none', note: '改写包含 system 消息的完整转录，Magi 不允许扩展触碰系统段', source: at('780–930') }),
  rule('events', /^before_provider_(headers|request)$/, { capability: 'hook.model.request', fidelity: 'none', note: '改写供应商请求头/请求体会绕过缓存与预算裁决；Magi 的 model.request 只允许观察', source: at('780–930'), needs: 'model.request 的 advise（目录允许但未接线；会削弱缓存前缀稳定性，不建议）' }),
  rule('events', /^(after_provider_response|provider_stream_event)$/, { capability: 'hook.model.response', fidelity: 'none', note: '供应商原始响应/流事件不对扩展开放', source: at('780–930') }),
  rule('events', /^cache_warming_decision$/, { capability: 'hook.model.request', fidelity: 'none', note: '提示缓存刷新由 Magi 原生管理', source: at('780–930') }),
  rule('events', /^(model_select|thinking_level_select)$/, { capability: 'hook.model.resolved', fidelity: 'partial', note: '模型解析结果可观察；pi 的交互式切换来源（set/cycle）无对应', source: at('931–970') }),
  rule('events', /^tool_call$/, { capability: 'hook.tool.call', fidelity: 'partial', note: 'block→block；改 event.input→replace（pi 不重验，Magi 会重验 schema 与权限，更严格）；terminate 无对应；pi 的原地改写需在兼容层转成返回 patch', source: at('971–1075'), needs: '无' }),
  rule('events', /^tool_result$/, { capability: 'hook.tool.result', fidelity: 'partial', note: '只能改模型可见投影（content）；details/structuredContent/isError/usage 的改写无对应，原始回执不变', source: at('971–1075') }),
  rule('events', /^tool_execution_(start|update|end)$/, { capability: 'hook.tool.result', fidelity: 'partial', note: 'end 可由 tool.result 观察；start/update 的实时进度无对应', source: at('971–1075') }),
  rule('events', /^user_bash$/, { capability: 'hook.tool.call', fidelity: 'none', note: '拦截用户 ! 命令；Magi 没有对应入口', source: at('1076–1109') }),
  rule('events', /^(project_trust|resources_discover|mcp_servers_change|ui_prompt_start|ui_prompt_end)$/, { capability: 'ui.view', fidelity: 'none', group: 'runtime', note: '属于 pi 的项目信任、资源发现、MCP 与 TUI 提示生命周期', source: at('427–468, 613–930') }),
  rule('events', /^<dynamic>$/, { fixed: 'unknown', note: '事件名不是字面量，静态分析无法确定' }),
  rule('events', /.*/, { fixed: 'unknown', note: '文档已读区域未登记的事件' }),

  // —— 导入 ——
  rule('imports', /^(type:)?node:/, { fixed: 'bundled', group: 'runtime', target: 'Node 内置模块' }),
  rule('imports', /^(type:)?\.{1,2}(\/|$)/, { fixed: 'bundled', group: 'internal', target: '包内相对导入，构建期打包' }),
  rule('imports', /^type:@earendil-works\//, { fixed: 'bundled', group: 'types', target: '仅类型，构建期擦除' }),
  rule('imports', /^(@earendil-works|@mariozechner)\/pi-coding-agent$/, { fixed: 'shim', group: 'runtime', target: '兼容层提供 isToolCallEventType 等辅助函数', note: '只提供在规则表里登记过的符号；其余符号在符号级规则里判定', source: at('155–172') }),
  rule('imports', /^(@earendil-works|@mariozechner)\/pi-agent-core$/, { fixed: 'unknown', note: 'Agent 消息与工具类型包；仅类型可擦除，运行期使用待评审' }),
  rule('imports', /^(@earendil-works|@mariozechner)\/pi-ai(\/|$)/, { capability: 'modelSource.publish', fidelity: 'full', note: 'pi-ai 随包打入（与 DSH 翻译器共用）', source: at('155–172') }),
  rule('imports', /^(@earendil-works|@mariozechner)\/pi-tui$/, { capability: 'ui.view', fidelity: 'none', group: 'ui', note: '终端组件库', source: at('155–172') }),
  rule('imports', /^(typebox|@sinclair\/typebox)(\/|$)/, { fixed: 'bundled', group: 'tools', target: '参数 schema 定义，随包打入；导出 JSON Schema 到清单', source: at('155–185') }),
  rule('imports', /.*/, { fixed: 'bundled', group: 'other', target: '第三方纯 JS 依赖，构建期打包', note: '含原生模块时需另行确认' }),
];

/** 具名导入符号规则：`模块#符号`。只列已读文档里出现过的符号。 */
export const PI_SYMBOL_RULES = [
  rule('symbols', /^[^#]+pi-coding-agent#(isToolCallEventType|isBashToolResult|defineTool|withFileMutationQueue)$/, { fixed: 'shim', group: 'runtime', target: '兼容层提供同名辅助函数', source: at('971–1075, 2180–2230') }),
  rule('symbols', /^[^#]+pi-coding-agent#(CONFIG_DIR_NAME)$/, { fixed: 'shim', group: 'runtime', target: '兼容层提供固定值', source: CONTEXT }),
  rule('symbols', /^[^#]+pi-coding-agent#(createLocalBashOperations)$/, { capability: 'hook.tool.call', fidelity: 'none', note: '用于 user_bash 自定义执行后端', source: at('1076–1109') }),
  rule('symbols', /^[^#]+pi-coding-agent#(.+)$/, { fixed: 'unknown', note: '文档已读区域未登记的符号' }),
  rule('symbols', /.*/, { fixed: 'bundled', group: 'other', target: '随依赖打包' }),
];

export function classify(kind, name) {
  return classifyWith(kind === 'symbols' ? PI_SYMBOL_RULES : PI_RULES, PI_IMPLEMENTED, kind, name);
}
