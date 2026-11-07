// DSH（DeepSeek Harness）API → Magi 能力映射规则。按顺序匹配，先匹配者生效；没有命中的条目一律 unknown。
//
// 依据（2026-11-05 读取，master 分支）：
//   [cookbook]  docs/cookbook/extension-cookbook.md（全文）
//   [tool]      docs/cookbook/adding-a-tool.md（前半，到 “Execution policy and observation”）
//   [pipeline]  docs/tool-execution-pipeline.md（全文）
//   [adapter]   docs/cookbook/adding-an-llm-adapter.md（全文）
//   [settings]  docs/cookbook/adding-a-settings-card.md（全文）
//   [events]    docs/cordis-api/events.md（仅方法签名）
// 没读到的事实不写进规则：没有出处的事件一律 unknown，而不是凭名字猜。
//
// 状态含义见 core/report.mjs；“Magi 能不能承载”（fidelity）与“翻译器做没做”（implemented）分开记录。
import { classifyWith, rule } from '../core/rules.mjs';

const GH = 'https://github.com/deepseek-ai/deepseek-harness/blob/master/';
const src = (file, range = '') => `${GH}${file}${range ? ` ${range}` : ''}`;
const COOKBOOK = src('docs/cookbook/extension-cookbook.md');
const PIPELINE = src('docs/tool-execution-pipeline.md');
const TOOL = src('docs/cookbook/adding-a-tool.md');
const ADAPTER = src('docs/cookbook/adding-an-llm-adapter.md');
const SETTINGS = src('docs/cookbook/adding-a-settings-card.md');

/** 翻译器当前已实现的能力。新增翻译能力时在这里登记，规则里的 full 才会从 planned 变成 mapped。 */
export const DSH_IMPLEMENTED = new Set([
  'tool.register', 'command.register', 'settings.schema', 'modelSource.publish', 'ui.route', 'ui.native',
]);

export const DSH_RULES = [
  // —— 上下文与生命周期（兼容层运行真实的 Cordis，这些调用由上游代码自身完成）——
  rule('calls', /^ctx\.(logger)(\.|$)/, { fixed: 'shim', group: 'runtime', target: 'api.log', note: '日志降级为诊断日志（最多 4096 字符，不写 canonical 日志）', source: COOKBOOK }),
  rule('calls', /^ctx\.(baseUrl)$/, { fixed: 'shim', group: 'runtime', target: 'Magi 调用期 profile 目录 URL', note: '用于读取当前 profile 身份与目录投影；Magi 路由仍由 daemon 统一拥有', source: COOKBOOK }),
  rule('calls', /^ctx\.connection(\.|$)/, { fixed: 'shim', group: 'runtime', target: 'Magi daemon 已认证的插件私有路由', note: '只提供 requestRejection；准入由 daemon 完成', source: COOKBOOK }),
  rule('calls', /^ctx\.pluginManager(\.(listBundles|registries))?$/, { fixed: 'shim', group: 'runtime', target: 'Magi daemon 插件目录只读投影', note: '每次读取真实包来源、版本和启用状态；在线管理沿 Magi 插件入口', source: COOKBOOK }),
  rule('calls', /^ctx\.pluginManager\.(listVersionExemptions|setVersionExemption)$/, { fixed: 'shim', group: 'runtime', target: 'daemon 全局版本豁免 CAS', note: '只读取当前兼容运行时的精确版本豁免；写入需要独立 compatibility 授权、风险确认及当前 revision', source: COOKBOOK }),
  rule('calls', /^ctx\.pluginManager\.(installBundle|setBundleEnabled|removeBundle)$/, { fixed: 'shim', group: 'runtime', target: 'Magi daemon 插件管理服务', note: '在线包预览、摘要核对、排空升级和启停；需 pluginManagement 授权，委托授权目标单独告知，不运行安装脚本', source: COOKBOOK }),
  rule('calls', /^ctx\.pluginManager\./, { capability: 'ui.view', fidelity: 'partial', group: 'runtime', note: '尚未接入 DSH 插件管理写操作；不伪造安装、升级或启停成功', source: COOKBOOK }),
  rule('calls', /^ctx\.loader(\.entries)?$/, { fixed: 'shim', group: 'runtime', target: 'daemon 插件目录的调用期 loader 投影', note: '同步读调用快照；profile 启停写入回执前由 daemon 提交，不保存第二份安装事实', source: COOKBOOK }),
  rule('calls', /^ctx\.loader\./, { capability: 'ui.view', fidelity: 'partial', group: 'runtime', note: '尚未接入的 loader 方法明确拒绝', source: COOKBOOK }),
  rule('calls', /^ctx\.profileContext(\.(name|dir))?$/, { fixed: 'shim', group: 'runtime', target: 'daemon 插件目录的调用期 profile 身份', note: '只读身份用于定位同一虚拟 profile，文件读取来自 daemon 快照', source: COOKBOOK }),
  rule('calls', /^ctx\.events(\.|$)/, { fixed: 'shim', group: 'runtime', target: 'Cordis 事件目录（只读）', note: '只暴露兼容层已登记的事件监听表，不提供任意宿主写入', source: COOKBOOK }),
  rule('calls', /^ctx\.(effect|fiber|on|once|collect|dispose|scope|inject|get|plugin|provide|root|parent)(\.|$)/, { fixed: 'shim', group: 'runtime', target: '真实 Cordis 运行时（兼容层内）', note: '注册与回收沿用上游 Cordis 语义', source: src('docs/cordis-api/context.md') }),
  rule('calls', /^ctx\.(emit|parallel|serial|bail|waterfall|accessor|mixin|extend|isolate|intercept|set)$/, { fixed: 'shim', group: 'runtime', target: '真实 Cordis 运行时（兼容层内）', source: src('docs/cordis-primer.md') }),
  rule('calls', /^ctx\.reflect\.(get|set|provide|accessor|mixin)$/, { fixed: 'shim', group: 'runtime', target: '真实 Cordis ReflectService', source: src('vendor/cordis/packages/core/src/reflect.ts') }),
  rule('calls', /^ctx\.settings(\.|$)/, { capability: 'settings.schema', fidelity: 'full', note: '配置 schema 构建期转成 JSON Schema；Volatile 字段随设置推送更新', source: SETTINGS }),

  // —— 工具、命令、模型 ——
  // `ctx.inject(['tools'], (c) => c.tools)`：取得服务本身；真正的注册调用另有 `.register` 条目。
  rule('calls', /^ctx\.tools$/, { capability: 'tool.register', fidelity: 'full', note: '取得 tools 服务；注册动作见 ctx.tools.register', source: TOOL }),
  rule('calls', /^ctx\.tools\.(register|get|executable|definitions|schemas)$/, { capability: 'tool.register', fidelity: 'full', note: '插件自己的动态工具目录，经 daemon 准入、版本化发布与回收', source: TOOL }),
  rule('calls', /^ctx\.tools\.(guard)$/, { capability: 'hook.tool.call', fidelity: 'partial', note: 'guard 是单调终局拒绝：映射为 tool.call guard 的阻止；DSH 同步 guard 返回值只有 reason', source: PIPELINE, needs: '无；阻止语义一致' }),
  rule('calls', /^ctx\.tools\.(restrict|resolve)$/, { capability: 'tool.register', fidelity: 'none', note: '按 Agent 作用域裁剪可见工具；Magi 的工具面由原生权限与角色绑定决定，扩展不能改变', source: src('packages/core/tools/README.md', '“Restrict tools per agent”'), needs: '工具可见性钩子（改变不变量，不建议）' }),
  rule('calls', /^ctx\.commands$/, { capability: 'command.register', fidelity: 'full', note: '取得 commands 服务；注册动作见 ctx.commands.register', source: COOKBOOK }),
  rule('calls', /^ctx\.commands\.register$/, { capability: 'command.register', fidelity: 'full', note: '运行期注册，沿 daemon 目录准入、版本化发布与回收', source: COOKBOOK }),
  rule('calls', /^ctx\.llm\.(registerAdapter|registerConfigurableProviders|registerModelDiscovery)$/, { capability: 'modelSource.publish', fidelity: 'full', note: '适配器经仅本机的 OpenAI 兼容端点接入；协议义务（usage 先于 finish、取消、replayState）由真实 dsh-llm 运行时保持', source: ADAPTER }),
  rule('calls', /^ctx\.llm(\.|$)/, { capability: 'modelSource.publish', fidelity: 'partial', note: '其余 llm 服务（stream/prepareCall 等）不对扩展开放，调用会被拒绝', source: ADAPTER }),

  // —— 拦截类扩展点 ——
  rule('calls', /^ctx\.systemPrompt(\.|$)/, { capability: 'hook.prompt.contribute', fidelity: 'partial', note: 'section() 可映射为带稳定性与预算的参考片段；作用域内遮蔽（shadowing）与顺序控制无对应', source: COOKBOOK, needs: '片段排序/遮蔽（会削弱“不得伪造原生系统段”不变量，不建议）' }),

  // —— 会话、消息、附件：Magi 内核独有 ——
  rule('calls', /^ctx\.(agents|sessionPersistence|subagents)(\.|$)/, { capability: 'service.spawn', fidelity: 'none', group: 'session', note: '会话/Agent 对象由 Magi 内核独有；扩展只能经钩子与宿主服务提案', source: COOKBOOK, needs: '无；与“Rust daemon 是唯一事实源”冲突' }),
  rule('calls', /^ctx\.sessions(\.|$)/, { capability: 'service.snapshot', fidelity: 'full', group: 'session', note: '会话投影需要沿 daemon 的原生快照和授权接入，尚未实现；不提供空会话替身', source: COOKBOOK }),
  rule('calls', /^ctx\.attachments\.(readImage|saveImage|readImageRequest)$/, { fixed: 'shim', group: 'runtime', target: 'Magi 有界 blob 附件桥', note: '图片以插件私有、短时 blob 保存；不复制会话附件事实', source: COOKBOOK }),
  rule('calls', /^ctx\.(sessionProjections|messages|memory|goals|workflowEngine|compaction|approval|sandbox)(\.|$)/, { capability: 'service.snapshot', fidelity: 'none', group: 'session', note: '对应能力由 Magi 原生拥有（审批、压缩、目标、沙箱），没有公开写入接口', source: COOKBOOK }),
  rule('calls', /^ctx\.(webServer|server)\.register$/, { capability: 'ui.route', fidelity: 'full', note: 'exact / prefix 路由，handler(req,res) 与 resource(Request) 两种形态；处理器留在宿主内，界面经视图桥调用。响应整段返回，流式响应不逐行推送', source: src('packages/host/webserver/README.md') }),
  rule('calls', /^ctx\.(webServer|server)\.registerUpgrade$/, { fixed: 'shim', group: 'ui', target: 'Magi 受限视图桥', note: 'WebSocket 分支在已有 HTTP 路由时不会启用；兼容层不开放监听端口', source: src('packages/host/webserver/README.md') }),
  rule('calls', /^ctx\.(webServer|server)\.(registerFallback|tapIndex|applyIndexTaps|renderIndex|collectIndexInjections|readResource)$/, { capability: 'ui.view', fidelity: 'none', group: 'ui', note: 'SPA 回退与 index 注入属于 DSH Web 外壳；Magi 的视图是独立页面，没有对应物', source: src('packages/host/webserver/README.md') }),
  rule('calls', /^ctx\.(webServer|server)(\.|$)/, { capability: 'ui.route', fidelity: 'partial', group: 'ui', note: 'webServer 的其余成员（地址、端口、协议）在 Magi 里没有意义：宿主不开放监听端口', source: src('packages/host/webserver/README.md') }),
  rule('calls', /^ctx\.(router|http)(\.|$)/, { capability: 'ui.view', fidelity: 'none', group: 'ui', note: '任意 HTTP 暴露不提供', source: src('packages/host/webserver/README.md') }),
  rule('calls', /^ctx\.(slots|locale|configForms)(\.|$)/, { capability: 'ui.native', fidelity: 'partial', group: 'ui', note: '上游自带界面原样挂载：注册在“插件配置”位置（settings.section，或较早的 plugins.bundle.config）的卡片在 Magi 设置页整页渲染；注册在 DSH 外壳其他位置（输入框、会话底栏、侧栏面板、全局提示）的槽位 Magi 没有对应位置，不渲染，构建报告逐项列出', source: SETTINGS }),
  rule('calls', /^ctx\.(\w+)/, { fixed: 'unknown', note: '文档未登记的 ctx 成员' }),

  // —— 服务名（inject）——
  rule('injects', /^(llm)$/, { capability: 'modelSource.publish', fidelity: 'full', source: ADAPTER }),
  rule('injects', /^(settings)$/, { capability: 'settings.schema', fidelity: 'full', source: SETTINGS }),
  rule('injects', /^(tools)$/, { capability: 'tool.register', fidelity: 'full', source: TOOL }),
  rule('injects', /^(loader)$/, { fixed: 'shim', group: 'runtime', target: 'daemon 插件目录的调用期 loader 投影', note: 'entries 与 profile 文件均来自同一调用快照；写入经 daemon 授权与生命周期入口', source: COOKBOOK }),
  rule('injects', /^profileContext$/, { fixed: 'shim', group: 'runtime', target: '调用期 profile 的只读身份', source: COOKBOOK }),
  rule('injects', /^(connection|pluginManager)$/, { fixed: 'shim', group: 'runtime', target: 'Magi daemon 已认证路由/插件管理服务', note: '目录读取实时投影；安装、启停与卸载需独立授权，并保留 Magi 的版本授权与排空边界', source: COOKBOOK }),
  rule('injects', /^(attachments)$/, { fixed: 'shim', group: 'runtime', target: 'Magi 有界附件兼容服务', note: '图片复用 blob 桥，不复制会话事实', source: COOKBOOK }),
  rule('injects', /^(sessions)$/, { capability: 'service.snapshot', fidelity: 'full', group: 'session', note: '会话投影尚未接入；必需依赖拒绝，条件注入保持缺席', source: COOKBOOK }),
  rule('injects', /^(commands)$/, { capability: 'command.register', fidelity: 'full', source: COOKBOOK }),
  rule('injects', /^(systemPrompt)$/, { capability: 'hook.prompt.contribute', fidelity: 'partial', note: '只能追加带稳定性与预算的参考片段；遮蔽与顺序控制无对应', source: COOKBOOK }),
  rule('injects', /^(webServer|server)$/, { capability: 'ui.route', fidelity: 'full', note: '路由注册由 Magi 的 api.route 承载；见 ctx.webServer.register', source: src('packages/host/webserver/README.md') }),
  rule('injects', /^(sessionProjections|attachments|agents|sessions|sessionPersistence|subagents|messages|memory|goals|compaction|approval|sandbox)$/, { capability: 'service.snapshot', fidelity: 'none', group: 'session', note: 'Magi 内核独有', source: COOKBOOK }),
  rule('injects', /^(slots|locale|configForms)$/, { capability: 'ui.native', fidelity: 'partial', group: 'ui', note: '见 ctx.slots：设置位置原生挂载，DSH 外壳其他位置不渲染', source: SETTINGS }),
  rule('injects', /^(tokenMeter)$/, { capability: 'hook.usage.update', fidelity: 'partial', note: '只能观察已提交用量；DSH 的 tokenMeter 还提供预算/计量服务', source: src('packages/llm/token-meter/README.md', '仅在文件树中确认存在，未读正文') }),
  rule('injects', /.*/, { fixed: 'unknown', note: '文档未登记的服务' }),

  // —— 事件 ——
  rule('events', /^loader\/volatile-update$/, { fixed: 'shim', group: 'settings', target: 'config.changed 通知', note: '配置热更新；兼容层已接', source: SETTINGS }),
  rule('events', /^settings\/updated$/, { fixed: 'shim', group: 'settings', target: 'config.changed 通知', note: '映射为插件设置更新通知', source: SETTINGS }),
  rule('events', /^plugin-manager\//, { fixed: 'shim', group: 'runtime', target: '插件管理器只读事件镜像', note: '事件只用于面板进度展示；Magi 安装事实仍由 daemon 拥有', source: COOKBOOK }),
  rule('events', /^tools\/pre-execute$/, { capability: 'hook.tool.call', fidelity: 'partial', note: 'deny→block；改参→replace（Magi 重验 schema 与权限）。allow/ask 无对应：扩展不能批准或发起审批，ask 由 Magi 原生审批接管', source: PIPELINE, needs: '无；扩展只能阻止不能批准是不变量' }),
  rule('events', /^tools\/post-execute$/, { capability: 'hook.tool.result', fidelity: 'partial', note: '可替换/阻止模型可见投影；DSH 的“附加上下文”“替换 value”无对应，原始回执不可改', source: TOOL, needs: '无；原始回执不变是不变量' }),
  rule('events', /^tools\/result$/, { capability: 'hook.tool.result', fidelity: 'partial', note: '观察冻结的最终结果；Magi 的 tool.result 在结果交给模型之前触发', source: PIPELINE }),
  rule('events', /^tools\/execute$/, { capability: 'hook.tool.call', fidelity: 'none', note: '环绕分发（超时/重试/指标）需要包裹原生执行；Magi 钩子是前后裁决，没有环绕', source: PIPELINE, needs: '环绕钩子（会让扩展改变原生执行生命周期，不建议）' }),
  rule('events', /^agent\/created$/, { capability: 'hook.session.created', fidelity: 'partial', note: '会话创建后的脱敏事实；拿不到 DSH Agent 对象', source: COOKBOOK }),
  rule('events', /^agent\/pre-step$/, { capability: 'hook.context.assemble', fidelity: 'partial', note: 'DSH 在每步前串行做压缩等；Magi 只开放历史重写提案，且需 contextRewrite 授权并由原生投影裁决', source: COOKBOOK }),
  rule('events', /^agent\/turn-stopping$/, { capability: 'hook.turn.end', fidelity: 'none', note: '“停止前再续一步”需要扩展驱动 Turn；Magi 不允许', source: COOKBOOK, needs: '无；与扩展不能驱动 Turn 的不变量冲突' }),
  rule('events', /^agent\/request-error$/, { capability: 'hook.model.response', fidelity: 'none', note: '请求错误恢复由 Magi 原生重试与压缩负责', source: COOKBOOK }),
  rule('events', /^agent\/assistant-stream$/, { capability: 'hook.model.response', fidelity: 'none', note: '逐 token 流帧不对扩展开放；model.response 只给提交后的脱敏投影', source: COOKBOOK }),
  rule('events', /^system-prompt\/assemble$/, { capability: 'hook.prompt.contribute', fidelity: 'partial', note: 'Magi 只接受带稳定性与预算的片段追加；DSH 整体改写 assembly 不可映射', source: COOKBOOK }),
  rule('events', /^session\/event$/, { capability: 'hook.turn.end', fidelity: 'partial', note: '只有 turn.accepted/turn.end/usage.update 等提交后事实；assistant/message、tool/call 等逐条事件无对应', source: COOKBOOK }),
  rule('events', /^turn\/end$/, { capability: 'hook.turn.end', fidelity: 'partial', note: '终态事实可观察；在 turn/end 后 followup 驱动下一轮（/loop）无对应', source: COOKBOOK }),
  rule('events', /^llm\/(adapters-updated|models-updated)$/, { fixed: 'shim', group: 'llm', target: '兼容层内部使用', note: '用于重新发布模型数据源', source: ADAPTER }),
  rule('events', /^internal\/(error|warning)$/, { fixed: 'shim', group: 'runtime', target: 'api.log', source: COOKBOOK }),
  rule('events', /^<dynamic>$/, { fixed: 'shim', group: 'runtime', target: '受限事件注册', note: '事件名由上游运行期决定；兼容层只允许其自身服务/已登记 Magi 事件，未支持宿主事件明确降级', source: COOKBOOK }),
  rule('events', /.*/, { fixed: 'unknown', note: '文档未登记的事件' }),

  // —— 导入 ——
  rule('imports', /^(type:)?node:/, { fixed: 'bundled', group: 'runtime', target: 'Node 内置模块' }),
  rule('imports', /^(type:)?\.{1,2}(\/|$)/, { fixed: 'bundled', group: 'internal', target: '包内相对导入，构建期打包' }),
  rule('imports', /^(type:)?@deepseek-ai\/dsh-client-/, { capability: 'ui.native', fidelity: 'partial', group: 'ui', note: '浏览器端界面依赖；页面壳只提供上游入口实际用到的服务，需要 DSH 外壳其他服务的界面在构建期记录阶段失败', source: SETTINGS }),
  rule('imports', /^(type:)?(react|react-dom)(\/|$)/, { fixed: 'bundled', group: 'ui', target: '页面壳随附同一份 React', note: '界面依赖的 React 由 Magi 视图页面提供' }),
  rule('imports', /^type:@deepseek-ai\//, { fixed: 'bundled', group: 'types', target: '仅类型，构建期擦除' }),
  rule('imports', /^@deepseek-ai\/dsh-llm-pi-ai$/, { capability: 'modelSource.publish', fidelity: 'full', note: '真实 dsh-llm-pi-ai 随包打入；与 pi 翻译器共用 pi-ai 适配层', source: ADAPTER }),
  rule('imports', /^@deepseek-ai\/dsh-llm$/, { capability: 'modelSource.publish', fidelity: 'full', note: '真实 dsh-llm 随包打入', source: ADAPTER }),
  rule('imports', /^@deepseek-ai\/dsh-tools$/, { capability: 'tool.register', fidelity: 'full', note: 'defineTool 等类型与校验随包打入', source: TOOL }),
  rule('imports', /^@deepseek-ai\/dsh-commands$/, { capability: 'command.register', fidelity: 'full', source: COOKBOOK }),
  rule('imports', /^@deepseek-ai\/dsh-host-webserver$/, { capability: 'ui.route', fidelity: 'full', source: src('packages/host/webserver/README.md') }),
  rule('imports', /^@deepseek-ai\/dsh-mcp-client$/, { fixed: 'bundled', group: 'runtime', target: '上游 MCP 客户端 + Magi 动态工具目录', note: '原版客户端负责连接、工具发现及回收；调用经过 Magi 的工具权限与执行入口。MCP 资源与提示服务仅在宿主提供时挂载', source: src('packages/mcp/mcp-client/README.md') }),
  rule('imports', /^@deepseek-ai\/dsh-typert-protocol$/, { fixed: 'bundled', group: 'runtime', target: '上游 Typert 声明 + 插件私有 Remote 路由', note: '承载本插件声明的 JSON unary 方法；流、Host 对象查找与原生会话上下文不伪造，调用时明确拒绝', source: src('docs/api-gateway.md') }),
  rule('imports', /^@deepseek-ai\/dsh-(atomic-write|home-paths)$/, { fixed: 'bundled', group: 'runtime', target: '文件与目录工具，随包打入' }),
  rule('imports', /^@deepseek-ai\/dsh-util-(workspace-path|code-language)$/, { fixed: 'bundled', group: 'runtime', target: '上游纯函数工具库，随包携带' }),
  rule('imports', /^@deepseek-ai\/dsh-settings$/, { capability: 'settings.schema', fidelity: 'full', source: SETTINGS }),
  rule('imports', /^@deepseek-ai\/dsh-attachment$/, { capability: 'service.snapshot', fidelity: 'none', group: 'session', note: '附件由 Magi 原生拥有', source: src('packages/attachment/attachment/README.md') }),
  rule('imports', /^@deepseek-ai\/schemastery$/, { fixed: 'bundled', group: 'settings', target: '纯 JS，构建期打包' }),
  rule('imports', /^@deepseek-ai\/cordis$/, { fixed: 'shim', group: 'runtime', target: '真实 Cordis，随包打入' }),
  rule('imports', /^(type:)?@earendil-works\/pi-ai(\/|$)/, { capability: 'modelSource.publish', fidelity: 'full', note: 'pi-ai 随包打入（与 pi 翻译器共用）' }),
  rule('imports', /^(type:)?@deepseek-ai\//, { fixed: 'unknown', note: '文档未登记的 DSH 包' }),
  rule('imports', /.*/, { fixed: 'bundled', group: 'other', target: '第三方纯 JS 依赖，构建期打包', note: '含原生模块时需另行确认' }),
];

export function classify(kind, name) {
  return classifyWith(DSH_RULES, DSH_IMPLEMENTED, kind, name);
}
