# Magi Extension Protocol v1

本文是受信 Node 扩展宿主的**线协议规范**，是[插件系统架构评估与目标设计](plugin-system-requirements-design.md)（下称“设计文档”）第 5.5～5.8 节的展开。设计文档回答“为什么、谁拥有什么”，本文回答“字节上怎么说”。两者冲突时先改设计文档再改本文，不在本文中另立设计决策。

## 0. 状态与适用范围

- 【现状·部分实现】[`magi-extension-host`](../crates/magi-extension-host) 已实现：帧、握手、对端、`register.hook`/`register.tool`/`host.ready`、`tool.invoke`（结果 `{output}` 或 `{isError:true,output}`；握手 `registrations.tools` 是清单声明且已获 `tools` 授权的工具，其余注册为 `RegistrationDenied`；超时发 `host.cancel`；不经熔断）、`hook.invoke`/`hook.notify`/`hook.gap`/`host.cancel`/`host.ping`/`host.shutdown`、ExtensionBus 的 guard/advise/observe 分发、熔断与重启监管，并有真实 Node 进程的端到端测试（macOS 已运行）。已接入对话循环：`tool.call`（账本指纹、路径与并发分类、权限和审批之前，改写后用本轮工具定义重新校验）与 `tool.result`（只改模型可见投影，原始回执先写入 canonical 事实），经 `magi-conversation-runtime::tool_hooks` 端口和 daemon 的适配器接入；真实循环与真实 Node 进程各有端到端测试，并已由 daemon 的 `ExtensionRuntime` 协调器接上插件管理：已安装、已授权、应用级激活且声明 `extension` 的包会被落盘、登记到 ExtensionBus 并受监管启动，停用或摘要变化时回收并注销（找不到 Node 时登记仍保留，guard 钩子 fail-closed）。Node 来自发行包随带的固定版本（`apps/desktop/node-runtime.json` 固定版本与归档摘要，daemon 只在自身同级 `node/` 目录查找，开发时用 `MAGI_EXTENSION_NODE` 覆盖），永不回退到用户 PATH；监管状态已进入插件列表投影：`PluginInstalled.runtime`（`starting/running/restarting/failed/stopped`、最近失败原因、代次、崩溃次数，以及宿主不可用期间正被 fail-closed 阻止动作的 guard 钩子名），由 daemon 的 `ExtensionRuntime` 状态板提供、API 只读合并而不另存，状态变化发布 `plugin.extension.status.changed` 事件，设置页插件详情显示“扩展运行”行、失败原因与“被阻止的动作”提示，列表项对失败/重启中显示警示标记。**钩子接线现状**（清单声明未接线的钩子/模式会在安装阶段被拒绝，不留永不触发的声明）：

| 钩子 | 已接线模式 | 来源与语义 |
| --- | --- | --- |
| `tool.call` `tool.result` | guard、advise | 工具管线，见上 |
| `input` | guard（只能阻止） | `TurnService` 单一入口，幂等重放之后、任何会话/Turn 事实创建之前；steer 同样经过；扩展提出改写按失败处理（fail-closed），文本转换尚未支持 |
| `subagent.spawn` | guard（只能阻止） | 在 `agent_spawn` 完整预检之前征询（看到 `tool.call` 改写后的最终参数），之后原生预检照常重新执行；不能指定或覆盖模型 |
| `session.created` `session.deleted` `turn.accepted` `turn.end` `usage.update` | observe | 领域事件总线的先提交后发布事实，经 daemon 转发器投影为有限、脱敏字段（不含正文、提示词、凭据）；`turn.end` 由 `session.turn.item` 事件里的终态派生并按 Turn 去重；事件总线是广播通道，消费落后会丢事件（只记日志，观察钩子不承诺完整送达） |
| `model.resolved` `model.request` `model.response` | observe | `StepModelClient` 在 Step 事实提交后通知，任务主线（`conversation_loop`）与纯会话 profile（`session_turn_execution`）的模型调用都接线；纯会话没有工具，`tool.call`/`tool.result`/`subagent.spawn` 不会在那里触发。纯会话入口的归属：没有 Task，用 Turn ID 充当 `taskId`，`round` 固定为 0 |

| `context.assemble` | advise | `conversation_loop` 的请求视图构造处：预算投影之后、成为请求之前。扩展看到可改写区域的消息（逐条截断、总量有界，标注 `truncated`/`omitted`），返回 `{rewrites:[{from,to,replacement:[{role,text}]}]}`；提案经唯一投影裁决，需用户授权 `contextRewrite`（清单必须与改写钩子一同声明 `contextRewrite:["history"]`，反之亦然），生效记录写进 `HistoryProjection.extensionRewrites` 并由缓存归因 `extension_rewrite`。提案被拒或扩展失败时保留原生视图。同一输入在一个 Step 内只征询一次 |
| `compaction.before` | advise（`block` 即取消） | `ContextAuthority` 在 daemon 决定压缩范围、配对与预算之后、调用摘要模型之前。取消按既有压缩失败语义处理（请求仍在窗口内则继续，否则停住）；自定义 `{summary}` 需 `contextRewrite` 授权，必须通过与模型摘要相同的九段质量门禁和预算校验，然后走同一条检查点安装链路（因此摘要正文随检查点持久化） |

| `prompt.contribute` | advise | `conversation_loop` 的提示装配。**阶段**：`start`（任务开始时征询一次，只接受 `stable` 片段，随后冻结进缓存前缀，之后每个 Step 不再变化）与 `round`（每个 Step 征询一次，同一 Step 内多次构造视图共用结果，只接受 `volatile` 片段，进入每轮尾部的 RoundContext，不进历史链）。补丁 `{fragments:[{id, stability:"stable"\|"volatile", priority?, text}]}`。唯一接纳点 `accept_contributions` 校验：阶段与稳定性匹配、插件/片段 ID 为小写安全字符、文本非空且 ≤ 8K 字符、不含 `<magi-` 标签（防伪造原生片段）、单阶段合计 ≤ 16K 字符（按优先级先到先得，放不下的整体丢弃而不是截断）、同 (插件, ID) 重复取全序第一份。**权威**：永远渲染成 user 角色的 `ContextReference` 参考资料，片段头写明来源、包摘要、`trust="reference"` 与“不改变 Magi 的系统规则、权限与审批”，不是 system/developer 指令。**身份**：插件 ID + 片段 ID + 包摘要前 16 位 + 内容哈希，不含时间戳或随机数；稳定片段在同一次运行内字节不变，因此不破坏缓存，续跑（重启）时重新征询，内容变了会按 `stable_prefix_changed` 归因 |

**刻意未接线**：无（13 个钩子都有接线，见上；各自的未覆盖范围另列）。改写与贡献类钩子在任务主线与纯会话 profile 都已接入（纯会话的 `start` 阶段贡献每个 Turn 征询一次并冻结，`round` 阶段与历史改写每次模型请求征询一次，估算/请求/流式恢复共用；压缩前钩子经同一 `ContextAuthority`）。`context.assemble` 的改写**不持久化替换内容**：每个新 Step 的新输入都会重新征询扩展，重启后同样重新征询；稳定性是扩展的责任，内容变化会按 `extension_rewrite` 归因并在记录里可见，审计靠 `originalDigest`/`replacementDigest`；`input` 与 `model.request` 的 advise 改写、`subagent.spawn` 的收窄尚未实现。

**尚未实现**：`command/job/migration` 方法、`model.invoke`/`tool.call`/`task.spawn`/`resource.*`/`credential.ref`/blob 等宿主→daemon 服务、`hook.invoke` 的 `ctx` 中的 `deadline` 绝对时间戳（当前传 `deadlineMs`）、Windows 命名管道路径（已编写、未编译验证）、以及对 conversation-runtime 实际调用链的接线——其余钩子目前不会被真实 Turn 触发。落点引用沿用设计文档 5.6 的核对结果。
- 【目标】协议只有一条执行路径，服务 Node 宿主；不为 QuickJS 或 SDK 1 提供任何子集（设计文档 I14、D1）。
- 本文使用 MUST / SHOULD / MAY 表达规范强度。数值默认值与设计文档 5.8 一致，需压力测试校准，偏离须同步修订两处。
- 规范性示例使用 JSON；字段名一律 camelCase；未知字段在 v1 内一律拒绝（`deny_unknown_fields` 语义），新增字段只能经小版本并由握手声明启用。

## 1. 传输与帧

| 项 | 规定 |
| --- | --- |
| 通道 | macOS/Linux：Unix 域套接字，目录 0700、套接字 0600；Windows：命名管道，DACL 仅当前用户。路径由 daemon 为每个实例单独生成，经环境变量 `MAGI_EXT_ENDPOINT` 传给宿主，不放命令行 |
| 帧 | 4 字节大端无符号长度 + UTF-8 JSON 正文。单帧上限 4 MiB（宿主策略，不由清单自抬）。超限即关闭连接并按协议错误处理 |
| 消息 | 沿用 JSON-RPC 2.0 的 `request`（带 `id`）、`response`（`result` 或 `error`）、`notification`（无 `id`）三形态。`jsonrpc` 字段固定 `"2.0"`。批量请求不支持 |
| 方向 | 双向：daemon 与宿主均可发起请求。`id` 为各自方向内唯一的字符串，由发起方生成 |
| 背压 | 每连接挂起请求上限 256；每个插件 observe 队列 1024（设计文档 5.8）；发送缓冲超限视为对端失速，按 `Backpressure` 终止该连接而不是无界缓存 |
| 编码 | 不支持二进制帧。大负载（附件、文件）经 `blob` 引用（第 8 节）传递，不内联 |

通道不承载任何 OS 权限：连上套接字只代表“是 daemon 启动的那个进程”，不代表可以写权威事实（I1）。

## 2. 握手与版本

daemon 启动宿主时同时注入一次性令牌环境变量 `MAGI_EXT_TOKEN`（32 字节随机，Base64）。宿主连接后必须在 5 秒内发送 `hello`；令牌只能使用一次，使用后立即失效。

宿主 → daemon：

```json
{"jsonrpc":"2.0","id":"h1","method":"host.hello","params":{
  "token":"…",
  "protocol":{"major":1,"minor":0},
  "extensionApi":{"major":1,"minor":0},
  "runtime":{"node":"22.x.y","platform":"darwin|win32|linux","arch":"arm64|x64"},
  "features":["hook.guard","blob.v1"]
}}
```

daemon → 宿主（结果）：

```json
{"jsonrpc":"2.0","id":"h1","result":{
  "protocol":{"major":1,"minor":0},
  "instance":{"pluginId":"acme.review","version":"1.2.0","digest":"sha256:…","generation":7},
  "limits":{"frameBytes":4194304,"pending":256,"guardDeadlineMs":5000,"adviseDeadlineMs":5000},
  "grants":["contextRewrite"],
  "registrations":{"hooks":[{"name":"tool.call","mode":"guard","deadlineMs":5000}]},
  "config":{"revision":3,"values":{}},
  "features":["hook.guard","blob.v1"]
}}
```

- 版本轴共四条（设计文档 5.5）：`manifestVersion`（包）、`extensionApi`、`protocol`、数据 schema。本握手只协商后两条；`manifestVersion` 与数据 schema 在安装/升级路径校验。
- 主版本不同 MUST 拒绝（`VersionMismatch`），不降级、不猜测。次版本只增不删：`features` 取交集，**只有双方都声明的功能才可使用**；daemon 不得使用宿主未声明的 feature。
- `instance.generation` 是该插件的实例代次，每次宿主（重）启动递增，并出现在其后所有 daemon 发出的请求里。宿主 MUST 忽略代次与自己不符的请求；daemon MUST 拒绝代次过期的宿主请求（`StaleGeneration`）。
- `registrations` 是 daemon 按清单与授权**批准**的集合；宿主能做的注册不超出它（第 5 节）。
- 握手失败、超时或令牌重放：daemon 终止该进程，计入崩溃退避，不自动重试同一令牌。
- 握手完成前宿主发出的任何其他方法一律拒绝并断开。
- 握手响应中的 `config.values` 是配置快照；运行时把它以只读形式提供给入口的 `api.config`。入口约定：包内 `extension/index.mjs` 默认导出 `activate(api)`。

## 3. 调用身份与上下文

daemon 发给宿主的每个**业务请求**都带统一的 `ctx`：

```json
{"ctx":{
  "invocationId":"inv_…","hookId":"hk_…","parentInvocationId":null,
  "generation":7,"deadline":"2026-11-02T10:00:05.000Z","depth":0,
  "owner":{"sessionId":"…","turnId":"…","taskId":null,"stepId":"…","attempt":1},
  "binding":{"workspaceId":"…","sourceFolderId":"…","cwd":"/abs/path"},
  "configRevision":3
}}
```

- `owner` 各字段在对应场景才存在；后台作业没有 Turn 时 MUST 携带 `jobId`，否则 daemon 拒绝其后的模型调用（设计文档 5.7）。缺少归属的请求不能冒充用户输入。
- `deadline` 是 daemon 的绝对截止时间；宿主 SHOULD 据此放弃工作，但 daemon 不依赖宿主自报超时，使用自己的独立计时。
- `depth` 为重入深度（第 7 节）。`parentInvocationId` 链用于审计与环路检测。
- `binding` 描述 Magi 服务的目录绑定，不是对 Node 的文件沙箱（设计文档 5.10）。

## 4. 方法目录

### 4.1 daemon → 宿主

| 方法 | 类型 | 说明 |
| --- | --- | --- |
| `hook.invoke` | request | 同步裁决型钩子（guard/advise）。第 6 节逐钩子规定 |
| `hook.notify` | notification | observe 钩子的事件投递，带单调 `seq` |
| `hook.gap` | notification | observe 队列丢失标记：`{from,to,reason}`，宿主 MAY 通过 `session.snapshot` 重读权威事实 |
| `command.invoke` | request | 用户调用已注册命令 |
| `tool.invoke` | request | 模型调用已注册工具；结果进入原生账本 |
| `job.run` | request | 原生调度或事件触发的插件作业；带 `jobId` 与用量预算 |
| `migration.run` | request | 仅在专用迁移会话中使用（第 9 节） |
| `config.changed` | notification | 配置快照更新 `{revision,values}`；新值影响下一边界 |
| `host.cancel` | notification | `{invocationId,reason}` 取消在途调用 |
| `host.ping` | request | 心跳，宿主须 2 秒内响应 |
| `host.shutdown` | request | 有限排空后退出；宿主响应后 daemon 关闭连接并回收进程树 |

### 4.2 宿主 → daemon

| 方法 | 类型 | 说明 |
| --- | --- | --- |
| `register.tool` / `register.command` / `register.skill` / `register.mcp` | request | 在清单已声明范围内绑定处理器或贡献，进入现有治理入口 |
| `register.hook` | request | 绑定钩子处理器；钩子必须在握手 `registrations.hooks` 的获批集合内，否则 `RegistrationDenied`。只有已绑定的钩子才会收到分发 |
| `register.dispose` | request | 注销；也在连接断开时由 daemon 隐式执行 |
| `host.ready` | notification | 入口 `activate` 完成后发送；daemon 在期限内未收到即视为激活失败并回收进程 |
| `session.snapshot` | request | 会话只读投影（有界、脱敏） |
| `model.invoke` | request | 带归属的模型调用，走原生服务；**不接受 model 参数**（I12），只接受角色或默认绑定 |
| `tool.call` | request | 经原生管线调用工具（含审批、权限、账本） |
| `task.spawn` | request | 子任务提案，走完整 `subagent.spawn` 预检；只能指定角色，不能指定模型 |
| `notify.user` / `ask.user` | request | 通知与用户问题；答复由用户给出，扩展不能代答审批（I2） |
| `resource.get/put/list` | request | 本插件命名空间的资源与设置，带版本号乐观并发 |
| `credential.ref` | request | 取凭据引用句柄（不返回明文，设计文档 5.7） |
| `blob.put` / `blob.get` | request | 大负载传递（第 8 节） |
| `host.log` | notification | 有界、脱敏的诊断日志 |

宿主**没有**任何写 canonical 日志、TaskStore、审批结果、用量的方法（I1）。方法名未在上表出现一律返回 `MethodNotFound`。

## 5. 注册

1. 清单声明允许的贡献与钩子（含 `mode`、`deadlineMs`、触发条件）。安装预览与授权面板展示它们；授权绑定包摘要。
2. 握手的 `registrations` 回传已批准集合。宿主调用 `register.*` 时，daemon 校验：贡献 ID 在清单内、不覆盖原生同名身份、命名空间为 `<pluginId>.*`。未声明的注册 `RegistrationDenied`。
3. 钩子的 `mode` 以清单与授权为准，**运行时不能降级**：宿主无权把 guard 改为 advise（设计文档 5.8）。
4. 注册带实例代次；连接断开或代次变化时全部失效，不遗留半注册状态。
5. 动态远端目录（运行时发现的工具集）在设计文档 5.7 定案前一律 `RegistrationDenied`，不得用通配声明绕开。

## 6. 钩子合同

通用规则：

- **三种模式**：observe 走 `hook.notify`，无响应；advise 与 guard 走 `hook.invoke`，必须响应。同一钩子在清单中声明哪些模式合法，见下表“允许模式”；不在表中的组合清单校验阶段拒绝。
- **裁决类型**：`hook.invoke` 的 `result` 为 `{"decision":"continue"|"block"|"replace","reason":"…","patch":…}`。`continue` 不带 `patch`；`block` 必须带 `reason`（展示给用户）；`replace` 仅在表中允许的钩子上使用，`patch` 的 schema 逐钩子规定。
- **扩展能阻止，不能批准**（I2）：任何结果都不含 `allow`/`approve` 语义；`block` 不可被后续插件撤销。
- **逐插件裁决**（设计文档 5.5）：daemon 对冻结的有序扩展集合逐项调用；`replace` 依次作用于最新候选；每次调用的裁决进入裁决轨迹。
- **改写后重新裁决**（I3）：所有 `replace` 结果由 daemon 以最终内容重新做 schema、权限、审批、并发分类，原参数的批准凭据不复用。
- **大小**：单个 `patch` 不超过帧上限，且各钩子另有上限（见下表）。超限视为协议错误。
- **失败语义**：见第 10 节。guard 失败阻止；advise 失败保留原生行为并提示；observe 失败只记录。

| 钩子 | 允许模式 | 输入（`params`） | `replace` 的 `patch` | 取消点 | 审计记录 | 恢复时重发 |
| --- | --- | --- | --- | --- | --- | --- |
| `session.created` `session.deleted` | observe | `{sessionId,workspaceId?,reason}` | — | 无 | 通知序号 | 否；缺口走 `hook.gap` |
| `turn.accepted` `turn.end` | observe | `{turnId,sessionId,profile,outcome?}` | — | 无 | 通知序号 | 否 |
| `input` | guard, advise | `{raw:{text,attachments:[blobRef]},source,queued}` | `{text,attachments}` 转换后输入；原始输入另存 | 接纳前 | 原始/转换哈希、裁决 | 否：重放使用同一处理身份，不再次调用 |
| `prompt.contribute` | advise | `{target:"task"\|"conversation",budget}` | `{fragments:[{id,stability,priority,text}]}` | 装配前 | 贡献 ID/摘要/预算 | 否 |
| `context.assemble` | advise | `{segments:[{id,kind,tokens,rewritable}],budget}` | `{ops:[insert\|remove\|reorder]}`，仅作用于 `rewritable` 段；改写历史须授权 `contextRewrite` | 装配后、IR 前 | 范围、原文引用、缓存代际变化（`extension_rewrite`） | 否 |
| `model.request` | observe, advise | `{stepId,roleBinding,params,fingerprintPending}` | 仅受限参数 `{temperature?,maxTokens?,stop?}`；不含 model/provider/body | 指纹冻结前 | 参数差异 | 否 |
| `model.response` | observe | `{stepId,finish,usage?}` | — | 无 | 通知序号 | 否 |
| `tool.call` | guard, advise | `{callId,tool,input,readOnlyHint}` | `{input}` 改写参数；须在账本指纹/并发分类之前 | 审批前 | 原/改写哈希、裁决 | 否：恢复按账本意图，不重放钩子 |
| `tool.result` | guard, advise | `{callId,tool,projection}` | `{projection}` 仅模型可见投影；原始回执不改 | 回执固定后 | 原始回执引用、投影哈希 | 否 |
| `compaction.before` | advise | `{reason:"manual"\|"threshold"\|"overflow",range,budget}` | `{summary}` 自定义摘要提案；范围/配对/预算由 daemon 定 | 摘要请求前 | 范围、摘要哈希、epoch | 否 |
| `subagent.spawn` | guard, advise | `{role,task,requestedTools?}` | `{role?,narrowedTools?}` 只能收窄；**永不含 model** | 完整预检前，之后重新预检 | 提案、预检结果 | 否 |
| `model.resolved` | observe | `{stepId,role,model,provider}` | — | 无 | 通知序号 | 否 |
| `usage.update` | observe | `{stepId,usage,committed:true}` | — | 无 | 通知序号 | 否 |

补充：

- 单项 `patch` 上限：`input` 1 MiB、`prompt.contribute` 256 KiB、`tool.call` 1 MiB、`tool.result` 1 MiB、`compaction.before` 64 KiB，其余 64 KiB；均为宿主策略。
- “不重发”指恢复/重放路径**不再次调用同一调用**；最终参数已持久化为意图，恢复读意图而非重新问扩展。仍可对新发生的事件调用。
- `tool.call` 对 `readOnlyHint` 的声明只用于并发分类的**输入**，daemon 以工具自身分类为准。

## 7. 重入、环路与死锁

- 宿主在处理 `hook.invoke` 期间可以发起 `model.invoke`、`tool.call`、`task.spawn`；这些请求自动继承父调用：`parentInvocationId` 由 daemon 按连接上下文填入，**宿主不能自报**。
- 重入深度上限 4：`depth` 每进一层加一，超过即 `ReentryDenied`。检测到同一 `(plugin, hook, 逻辑键)` 在当前调用链中已出现，按环路拒绝。
- **禁止等待被阻塞动作自身完成**：在 `tool.call` 钩子内对“同一个 callId 的工具”发起 `tool.call` 返回 `Deadlock`。钩子内产生的重入调用同样触发其他插件的钩子，但受 depth 限制。
- 重入调用使用最终参数重新走完整裁决，不借用外层批准。
- 状态型请求（写资源、派发子任务、调用外部写工具）MUST 带 `requestId`（宿主生成的幂等键）；daemon 对相同 `(plugin, requestId)` 返回首次结果。**超时不自动重试外部写入。**

## 8. 大负载（blob）

- `blob.put` 返回 `{blobId,size,sha256}`；`blob.get` 取回。blob 属于 `invocationId` 的租约，调用结束后回收；生命周期之外的访问 `BlobExpired`。
- 单个 blob ≤ 32 MiB，每插件在途 blob 总量受宿主策略限制。blob 不是持久存储，持久值走 `resource.put`。
- 附件、长工具输出、`session.snapshot` 的大字段使用 blob 引用 `{"$blob":"<id>"}`。

## 9. 迁移会话

- 仅升级流程可以创建迁移会话（设计文档 9.4）；该会话的宿主进程使用**新版本包**，但此时 daemon 只开放以下方法：`migration.run`、`resource.*`（只读旧快照，写入暂存区）、`blob.*`、`host.log`。其余方法 `MethodNotFound`。
- `migration.run` 参数：`{fromDataVersion,toDataVersion,snapshot:{cursor},staging}`；宿主分页读取旧快照，把结果写入暂存区。daemon 在提交前对暂存结果做 schema/大小/版本核对，随版本切换原子提交。
- 迁移入口以用户权限运行，不是沙箱纯函数；声明 `externalMigration` 的插件，其外部副作用不在 daemon 事务内，预览中须披露恢复办法。迁移前 daemon 已完成完整备份，迁移失败不自动回退，由显式恢复动作处理。

## 10. 失败、超时与取消

| 情形 | daemon 行为 | 宿主应对 |
| --- | --- | --- |
| `hook.invoke` 超时（独立计时） | 先发 `host.cancel`，有限等待结算，仍无响应则按模式处理；累计触发熔断 | 放弃工作，不再响应该调用 |
| guard 失败/超时/熔断打开/宿主缺失/协议错误 | **阻止受影响动作**并给出可见原因（含插件、钩子、错误码）；不默认放行 | — |
| advise 失败 | 保留原生行为并提示；不计为阻止 | — |
| observe 队列满 | 丢弃最旧并发 `hook.gap` | 需要事实时调用 `session.snapshot` |
| 宿主崩溃 / 连接断开 | 隐式 `register.dispose`；挂起请求以 `HostGone` 结算；撤销该实例所有注册；退避重启（1s 起，上限 60s；10 分钟内崩溃超过 5 次停止自动重启） | — |
| 取消 | 关闭新调用准入，发 `host.cancel`，有限等待，必要时回收进程树；迟到结果只用于该调用结算，不写入下一轮 | 取消后停止发起新的服务请求，否则 `Cancelled` |
| 版本/代次不符 | `StaleGeneration`、`VersionMismatch` | 退出，由 daemon 重启 |

熔断参数：连续 5 次失败，或最近 60 秒内 ≥10 次调用且失败率过半则打开；冷却 30 秒后半开，只放行 1 次探测。guard 熔断期间拒绝受影响动作；用户显式停用该插件后新动作才按变更后的策略接纳（设计文档 5.8）。

## 11. 错误码

`error` 对象为 `{"code":-32000,"message":"…","data":{"kind":"<Kind>","retryable":false,"detail":{…}}}`；`kind` 为稳定枚举：

| kind | 含义 |
| --- | --- |
| `ProtocolError` | 帧/JSON/schema 不合法、未知字段、超限 |
| `VersionMismatch` | 协议或扩展 API 主版本不符，或使用未协商的 feature |
| `StaleGeneration` | 实例代次过期 |
| `MethodNotFound` / `InvalidParams` | 方法不存在 / 参数不符 |
| `RegistrationDenied` | 注册超出清单或授权 |
| `PermissionDenied` | 缺少所需授权项（如 `contextRewrite`） |
| `Blocked` | guard 阻止；`data.detail` 含插件与 reason |
| `Cancelled` | 调用已取消 |
| `DeadlineExceeded` | 超过 deadline |
| `ReentryDenied` / `Deadlock` | 重入深度、环路或自等待 |
| `Backpressure` | 队列或缓冲超限 |
| `HostGone` | 宿主已断开 |
| `BlobExpired` | blob 已回收 |
| `Internal` | daemon 内部错误；宿主不得依赖其 detail |

`retryable` 仅表示“重试不会重复副作用”，写入类请求仍须依赖 `requestId` 幂等；daemon 不为外部写入自动重试。

## 12. 安全与诚实披露

- 协议不增加宿主的 OS 能力，也不限制它：Node 以用户权限运行，可直接读写文件、联网、启动进程，Magi 不能限制（I15）。协议只约束**经 Magi 路由的**行为。
- 宿主无法通过本协议获得：写权威事实、批准/代答审批、覆盖角色模型绑定、读取其他插件的资源、读取凭据明文。
- 审计记录有界并脱敏，由 daemon 写入原生持久化边界；敏感正文不复制进第二份审计库。
- 界面不得把“经协议调用”表述成“被沙箱保护”。

## 13. 一致性验收（协议层）

每项都要有可重复的自动化验收；在对应入口落地前，不得宣称协议已实现（与[验证入口](validation.md)中“受信扩展宿主”一节同步维护）：

1. 握手：错误令牌、令牌重放、超时、主版本不符、未协商 feature、代次过期，均被拒绝且进程被回收。
2. 帧：超限、非法 JSON、未知字段、批量请求、握手前业务方法，均断开并计入崩溃退避。
3. 注册：未声明贡献、覆盖原生同名、运行时降级 guard、动态通配，均 `RegistrationDenied`。
4. 逐钩子（第 6 节每行）：输入输出 schema、`replace` 后重新裁决、`block` 不可撤销、不重发语义、取消点。
5. guard 失败矩阵：异常、超时、熔断、宿主缺失、协议错误全部阻止受影响动作，且给出可见原因；advise/observe 按表降级。
6. 重入：深度 4、环路、自等待、父调用不可自报、幂等 `requestId`、超时不重试外部写入。
7. 取消与崩溃：迟到结果不写下一轮；崩溃后注册失效、挂起请求 `HostGone`、不自动重放；退避与停止阈值。
8. 迁移会话：方法集合受限、暂存校验、原子提交、`externalMigration` 披露、失败不自动回退。
9. 平台：macOS 与 Windows 命名管道/套接字访问控制、进程组/Job Object 回收，行为一致；安全强度差异按设计文档 7.3 如实记录。

## 14. 变更规则

- v1 内的字段、方法、错误码只增不删；新增必须有对应 feature 名并经握手协商。
- 删除或语义变化只能升主版本，宿主只接受自己发布时的当前主版本（设计文档 5.5）。
- 本文每次变更必须同步：设计文档对应章节、[验证入口](validation.md)的验收面、[插件作者指南](plugin-authoring.md)。
