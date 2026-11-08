# Magi 扩展协议 v1

更新：2026-11-06。本文描述当前受信 Node 宿主的公开合同；架构、所有权及后期路线见[插件系统设计](plugin-system-requirements-design.md)第 5.5～5.10 节。必须／不得为硬约束，应为默认行为，可为允许选项。实现依据为 [`protocol.rs`](../crates/magi-extension-host/src/protocol.rs)、[`instance.rs`](../crates/magi-extension-host/src/instance.rs)、[`host.mjs`](../crates/magi-extension-host/runtime/host.mjs)；未交付的旧方案不构成 API。

## 0. 状态与适用范围

【现状】SDK 2、传输协议 1.0、扩展 API 1.0。daemon 独立托管每插件一个 Node 进程；支持钩子、模型工具、用户命令、持久计划及反向服务。SDK 1／QuickJS、迁移 VM、工作流与会话引擎替换不提供。DSH 翻译功能按设计文档第 6.0 节的产品合同开发，当前能力与未完成项见第 11 节；pi 运行器后置；MCP 完整内置于 Magi，不经插件注册。

【现状】下表列出的 15 个钩子名称均有实际接线。`input`／`subagent.spawn` 仅允许 guard，`model.request` 仅允许 observe；清单声明未接线模式时安装拒绝。不能把目录中未来可能支持的模式当作已交付合同。

【现状·已运行验证】macOS 真实 Node 集成、真实 daemon＋本地 HTTP 模型，以及 Electron Chromium 工作台回归已运行；Windows demo、Windows 实机和正式安装包验收后置。证据和未覆盖范围见[验证入口](validation.md)及设计文档第 12 节。

## 1. 传输与帧

| 项 | 当前合同 |
| --- | --- |
| 通道 | macOS/Linux 使用 Unix socket，私有目录 0700、socket 0600；长状态路径改用短私有目录。Windows 使用拒绝远程客户端的命名管道；未实现／验证当前用户专属 DACL，不声称拥有该隔离 |
| 身份 | daemon 生成端点与随机一次性令牌，通过 `MAGI_EXT_ENDPOINT`／`MAGI_EXT_TOKEN` 注入宿主；令牌校验不等于 OS 沙箱 |
| 帧 | 4 字节大端长度＋UTF-8 JSON，单帧最多 4 MiB；非法 JSON／超限断开连接 |
| 消息 | JSON-RPC 2.0 request／response／notification，字符串 ID；不支持批量请求 |
| 背压 | 每方向挂起请求最多 256；Rust 发送队列 1024，满时返回 `Backpressure`；入站请求同样有并发上限 |
| 超时 | daemon 独立计时；Node 一般反向请求最多 120 秒，`plugins.preview`／`plugins.install` 最多 1000 秒。daemon 的上游准备阶段（含下载与翻译排队）最多 840 秒，准备失败不提交版本；预览／安装等待还为提交前的排空保留预算。外层调用可更早取消 |

公开清单和类型化服务拒绝未知字段；当前 JSON-RPC 信封解析只读取规定字段，不应把“所有层都实现未知字段拒绝”作为已验证事实。未定义方法必须明确拒绝。

## 2. 握手与版本

daemon 启动随包固定 Node，开发时可用 `MAGI_EXTENSION_NODE` 指定。不得回落到用户 PATH。连接等待默认 15 秒、握手 5 秒、入口就绪 10 秒。连接前退出必须快速失败；失败由监管回收进程树。

宿主首先发送：

```json
{"jsonrpc":"2.0","id":"h1","method":"host.hello","params":{
  "token":"<32 字节随机值的十六进制编码>",
  "protocol":{"major":1,"minor":0},
  "extensionApi":{"major":1,"minor":0},
  "runtime":{"node":"<随包版本>","platform":"darwin","arch":"arm64"},
  "features":["hook.guard"]
}}
```

daemon 响应形状：

```json
{"jsonrpc":"2.0","id":"h1","result":{
  "protocol":{"major":1,"minor":0},
  "instance":{"pluginId":"acme.review","version":"1.0.0","digest":"<包摘要>","generation":1},
  "limits":{"frameBytes":4194304,"pending":256},
  "grants":[],
  "registrations":{"hooks":[{"name":"tool.call","mode":"guard","deadlineMs":5000}],"tools":[],"commands":[]},
  "config":{"revision":0,"values":null},
  "features":["hook.guard"]
}}
```

主版本不符或错误令牌必须拒绝。features 取交集；声明 guard 的包必须协商 `hook.guard`。当前没有 `blob.v1` feature，不应发送该声明来推导额外能力。连接只执行一次握手；重启使用新端点、令牌和代次。

包版本轴为 `sdkVersion`、protocol、extensionApi、`dataSchemaVersion`；没有 `manifestVersion` 字段。版本变化不得用静默降级猜测语义。

## 3. 调用身份与上下文

工具／命令请求形状为 `{tool|command,input,ctx}`，钩子请求为 `{hook,input,ctx}`。`ctx` 包含 daemon 签发的 `invocationId`、`generation`、相对 `deadlineMs` 及可用的 `owner`／`binding`；钩子另带 `hookId`、`parentInvocationId`、`depth`。当前不传绝对 `deadline`，也不保证每个场景拥有同一组 owner 字段。

工具与命令另带 `execution:{workingDirectory,sourceFolders,accessProfile}`：workingDirectory 是本次实际执行根的本机绝对路径字符串，缺失为 null；sourceFolders 是获授项目源目录的 `{sourceFolderId,workingDirectory}` 列表，只传实际执行根，个人会话为空。工具保留本次所选 source_folder_id；命令保留 Turn 接纳的默认源目录。daemon 在调用前核对完整目录映射及物理身份；命令与原生工具共用资源准备入口，不能在副本丢失后回落源目录、个人目录或宿主 cwd。此上下文不向页面路由或普通钩子推断补齐，不授予新的 OS 权限。

- 工具／命令调用使用插件＋代次＋递增序号形成独立身份；相同 caller 身份的并发调用可独立取消。
- `owner` 描述实际 Session／Turn／Task／worker；插件命令由原生 Task 调度并携带实际 Task／worker 身份，停止与结果提交均核对该任务的当前租约。是否能调用其他原生服务仍由其权限及策略独立判断。
- 作业增加 `ctx.job={jobId,scheduleId,budget}`，仍拥有 canonical Turn。预算由 daemon 签发。
- Node 以 AsyncLocalStorage 随反向请求携带 `invocationId`；daemon 从当前连接租约补入内部上下文，拒绝扩展自报下划线字段。
- 结束、取消和超时使租约失效。失效租约拒绝新业务服务；迟到响应不得写入下一轮。
- `binding` 是业务归属，不是文件系统访问限制。插件直接 OS 行为仍以用户权限运行。

## 4. 方法目录

### 4.1 daemon → 宿主

| 方法 | 类型 | 当前合同 |
| --- | --- | --- |
| `hook.invoke` | request | guard／advise 裁决，见第 6 节 |
| `hook.notify` | notification | observe 通知 `{hook,seq,input}` |
| `hook.gap` | notification | 队列缺口通知；当前 Node SDK 不暴露单独的 gap 回调，不承诺观察事件完整送达 |
| `tool.invoke` | request | 已绑定工具；返回 `{output,isError?,completion?}`，处理器抛错返回 `{isError:true,output}`；结构化结果与完成意图见下文 |
| `command.invoke` | request | 用户命令；返回 `{content}` 或 `{isError:true,content}` |
| `job.run` | request | 原生计划触发，复用已绑定命令；有真实 Turn、作业身份和预算 |
| `config.changed` | notification | `{revision,values}`，只接纳更高版本并更新只读配置快照 |
| `settings.validate` | request | `{generation,value}`，调用扩展注册的设置校验器；实例代次不符或未注册校验器时明确失败 |
| `host.cancel` | notification | `{invocationId,reason}`，中止对应 `ctx.signal` |
| `host.ping` | request | 心跳，daemon 等待最多 2 秒 |
| `host.shutdown` | request | 有限停止；随后由 daemon 回收受管进程树 |

### 4.2 宿主 → daemon

`tool.outcome` feature 协商后，工具处理器可返回 `api.toolResult(output,{isError?,completion?})` 创建的结果。宿主按本次调用 signal 身份解包，拒绝跨调用、重复使用和已失效调用；普通 JSON 对象不根据字段名推断控制。`completion` 只允许成功结果携带非空且最多 1 MiB UTF-8 的最终内容。daemon 严格校验回执，为成功完成意图附加插件／贡献身份，保存在原 canonical 工具结果；未确定的同批调用阻止完成。任务验收、Goal、租约和终态由原生执行链路裁决，恢复使用原回执，不重放工具。DSH 的 additionalContexts 与 Agent.inject 使用同一 session.message 交付；管线失败保留纠正上下文和结构化结果，交付失败单独标明，不能自动重试已经执行的业务。

| 方法 | Node API | 当前合同 |
| --- | --- | --- |
| `register.hook` | `api.on(name,handler)` | 只能绑定清单获批钩子 |
| `register.tool` | `api.tool(id,handler)` | 只能绑定清单或当前实例运行目录中声明且获 tools 授权的工具 |
| `register.command` | `api.command(id,handler)` | 只能绑定清单或当前实例运行目录中的公开命令 |
| `catalog.replace` | `api.catalog.replace(expectedRevision,catalog)` | 整体替换当前实例的动态目录，返回 `{revision}`；校验清单授权上界、唯一 ID、schema、大小及实例租约，见第 5 节 |
| `unregister.tool/command` | `api.tool`/`api.command` 返回的释放函数 | 收回本实例的处理器绑定；只缩小已绑定集合，未绑定的名字幂等成功。未绑定的工具对模型不可见，调用返回 `MethodNotFound` |
| `resource.read/write` | `api.resource.read(id)`／`write(id,expectedVersion,value)` | 当前插件全局资源，逐次 storage 授权与 CAS |
| `settings.read/write` | `api.settings.read()`／`write(expectedVersion,value)` | `__settings` 授权、schema 校验、CAS 与事件同步 |
| `settings.describe` | `api.settings.describe()` | 返回 `{schema}`，包含当前实例已接纳的运行期设置 schema |
| `model.invoke` | `api.model.invoke(input)` | `{prompt,roleId?,credentialRef?,maxOutputTokens?}`，models 授权、原生模型解析和账本 |
| `credential.ref` | `api.credential.ref({roleId?})` | 返回模型绑定引用，不返回 secret；只由 model.invoke 消费 |
| `session.snapshot` | `api.session.snapshot(input)` | 仅签发会话；`sessionId?`、`afterTurnSeq?`、`limit?`；最多 10 Turn，每 Turn 最多 20 条截断文本 |
| `session.message` | `api.session.message(input)` | 协商 `session.messages`，输入与回执为 `PluginSessionMessageRequest/Receipt`。需要独立 `sessionWrite:["context"]` 或 `["wake"]` 授权，仅写当前有效调用所属的主会话；daemon 注入的真实 Task、Turn 与执行租约必须一致。子任务拒绝；计划作业只能提供 context，不能借 wake 绕过模型预算。正文保存插件来源和稳定身份，不能冒充用户／系统或指定其他会话；不提供后台入口 |
| `session.directory` | `api.session.directory(input?)` | 需协商 `session.directory` 及独立 `sessionRead:["directory"]` 授权。按当前实例读取 daemon 用户会话目录，不要求活动 Turn；输入 `PluginSessionDirectoryRequest`，输出 `PluginSessionDirectory`。按 sessionId 升序分页，每页 1–200 条（默认 100），可筛选 workspaceId；仅身份、标题、项目归属与创建/更新时间，不含外部工具会话、消息、文件路径或凭据。末页游标为 null；跨页并非事务快照，调用者需按身份处理期间新增/删除。旧版本、停用、撤权、排空及失效实例拒绝 |
| `plugins.list` | `api.plugins.list()` | 读取 daemon 的既有 `PluginList` 目录投影，每次校验当前插件包、激活状态及实例租约；不包含配置、资源或凭据 |
| `plugins.changed`（通知） | `api.plugins.onChanged(listener)` | 协商 `plugin.directory.events`；daemon 在安装目录或运行状态变化时发送无载荷的失效通知，返回值为解除订阅函数；消费者重新调用 plugins.list，不由通知构造安装或运行事实 |
| `plugins.compatibility`／`plugins.setVersionExemption` | `api.plugins.compatibility()`／`setVersionExemption(input)` | 需协商 `plugin.management`。读取要求活动宿主租约；写入另需 `pluginManagement:["compatibility"]`，使用 `PluginDshVersionExemptionRequest` 的精确 target、当前 runtimeVersion／runtimeId、revision CAS 和显式风险确认。安装／升级提交前重验，撤销后旧译包不能继续提交 |
| `plugins.preview/install` | `api.plugins.preview(input)`／`install(input)` | 需协商 `plugin.management` 与 `pluginManagement:["install"]` 授权；复用 `PluginUpstreamRequest`、预览与安装入口，返回既有预览／目录 DTO；仅支持已接入的上游生态。安装必须指定精确版本与预览摘要，不替用户授权新版本 |
| `plugins.installBundle/cancelInstall` | `api.plugins.installBundle(input)`／`cancelInstall(requestId)` | 另协商 `plugin.installations`，使用 `PluginBundleInstall*` DTO；由 daemon 完成在线版本解析、准备、安装／升级及请求的启停，返回实际应用结果。两者均需 install 授权，后台调用另需 background；启停及代授新版本分别核对 lifecycle、authorize 委托 |
| `plugins.reconcile` | `api.plugins.reconcile({pluginIds})` | 协商 `plugin.reconciliation`，需 `pluginManagement:["lifecycle"]`，后台另需 background。输入 `PluginReconcileRequest`（至多 512 个唯一已安装 ID），沿 daemon 原监管器收敛已提交的激活意图并等待所选活动扩展真实就绪；所有目标共用 110 秒预算。返回既有目录 DTO；停用项保持停用，未变化实例保持代次，缺少监管器、启动失败、排空、版本／激活变化、过期租约和撤权明确失败。不授予权限、不重新安装、不清空故障或强制重启 |
| `plugins.setEnabled/remove` | `api.plugins.setEnabled(pluginId,enabled,expectedDigest?,options?)`／`remove(pluginId,expectedDigest?)` | 需协商 `plugin.management` 与 `pluginManagement:["lifecycle"]`；复用 HTTP 的生命周期、排空与停止端口。启停输入为 `PluginSetEnabledRequest`；options 可带 `reason`（仅停用，1–4096 字节）及 `expectedState`，后者须与 expectedDigest 一起提交。daemon 注入停用者身份，与 enabled/active 原子保存；启用及显式停用会清除旧来源。排空前后比较相同摘要及启停来源，过期恢复拒绝。拒绝当前发起插件的自身停止、删除或升级；启用后等待真实宿主运行状态，启动失败不返回成功 |
| `tool.call` | `api.tools.call(tool,input)` | 只接受公开原生工具；需要真实 Task、租约、策略、schema、权限、审批与账本 |
| `task.spawn` | `api.task.spawn(input)` | 原生 agent_spawn 合同；保留角色、自定义模型绑定、目录与策略，不接受伪造归属 |
| `notify.user` | `api.notify.user({message,title?})` | 原生通知事实、持久化与事件；正文最多 4096 字节、标题 256 字节 |
| `ask.user` | `api.ask.user({questions})` | 原生问题校验、持久交互和等待；扩展不能代答审批 |
| `blob.put/get` | `api.blob.put(value,contentType?)`／`get(ref)` | 第 8 节有界 JSON 传递 |
| `images.begin/append/commit/abort` | `api.images.save({data,mediaType,name?})` | 协商 `images.store`，独立 `attachments:["images"]` 授权；以当前宿主实例上传，SDK 分块后由 daemon 校验并持久化。不是会话写入，也不发送消息 |
| `images.describe/read/release` | `api.images.read(attachmentId)`／`release(attachmentId)` | 仅当前插件自己的持久图片；读取分块并核对完整 SHA-256，释放幂等；每次复核包摘要、激活、权限和宿主实例 |
| `modelSource.set/clear` | `api.modelSource.set(sourceId,{baseUrl,apiKey,apiProtocol,models})`／`clear(sourceId)` | 清单声明且获 `modelSources:[sourceId]` 授权；`set` 整体替换，校验与用户数据源一致；只追加到主对话模型目录（`plugin/<pluginId>/<sourceId>`），仅存运行期内存，宿主实例退出时撤销 |
| `host.ready` | 宿主自动通知 | `activate(api)` 成功后发送 |
| `host.log` | `api.log(message)` | 最多 4096 字符的诊断通知，不写 canonical 日志 |

需要在关闭页面后保持事件订阅的隔离客户端，可声明 `contributions.client.entry` 并申请
`client:["persistent"]`。入口必须属于同一清单的 views；daemon 激活投影决定其挂载和销毁，
Renderer 不自行延长插件激活。每个工作台窗口为该插件持有一个客户端文档，设置和右栏
切换通过保留文档的物理挂载与视图上下文切换完成，不重新运行上游 apply。无可见入口时
停放在不参与布局、焦点或辅助技术导航的容器中，业务订阅继续；停用、撤权和包摘要变更
销毁旧文档、关闭事件连接并废弃迟到回执。该合同不会代替 composer、attachments 或
sessionRead 的各自授权。未声明客户端贡献的普通视图仍按页面生命周期运行。

所有插件视图共用 daemon 运行状态门禁：声明后台扩展的包仅在宿主 running 后挂载原包文档，启动及重启期间展示真实等待状态，失败展示 daemon 原因。包摘要、入口或运行代次变化销毁旧文档和消息桥；其他插件的目录变化不重建当前文档。纯声明视图在激活后直接挂载，常驻客户端同样遵守该门禁，不自行延长后台实例的生命周期。

原生 DSH 页面如需把图片交给当前 Magi 输入框，必须在翻译清单中声明 `composer:["images"]`；页面通过 `ctx.magiComposer` 的异步 `addImages(files)`、`removeImage(id)`、`focus()` 和 `notify(level,message)` 使用宿主。`addImages` 只有在浏览器真实解码、插件摘要/页面身份/权限复核以及 InputArea 接纳后才返回 `accepted:true`；同一批次任一图片失败不会部分挂载。会话切换、页面卸载、停用、撤权或包升级会使旧请求失效。PNG、JPEG、GIF、WebP、AVIF、BMP、ICO 和 SVG 均保留原始草稿字节；发送到不支持原格式的模型时，模型桥只在传输边界生成有界 PNG/JPEG 派生图，canonical 消息仍保留原始图片。

持久图片使用 schema 中的 `PluginImageAttachment*` DTO。单图最多 10 MiB，分块最多 786432 字节，每个实例最多两个未提交上传、共 20 MiB，60 秒无活动回收；实例失效立即丢弃暂存，不能用旧上传身份跨实例提交。相同偏移与字节的分块重传、同一完整上传的提交重试幂等。每插件持久图片最多 1024 个引用、逻辑总量 256 MiB；作者应以 release 回收不再使用的引用。相同内容可共享载荷文件，但引用和授权仍按插件隔离。存储保留 PNG、JPEG、GIF、WebP、AVIF、BMP、ICO 与安全 SVG 原始字节，拒绝类型伪造；不承诺已完成浏览器或模型解码，解码仍由相应消费入口负责。

图片元数据复用 daemon 现有插件资源事实和升级数据重置事务，内容寻址文件只保存载荷。先原子写载荷，再提交元数据；未提交载荷不可由 API 读取，重启及下次保存时回收。释放先提交元数据删除，没有其他插件引用时才删除共享载荷。已发送图片沿原生 canonical 消息保存自己的字节，不引用这份插件私有存储。`images.store` 明确允许已授权插件在后台事件回调中使用当前实例，不携带已结束的 invocationId；这不放宽 blob、模型、工具、会话或通知服务的原调用租约。

管理 API 的后台模式另协商 `plugin.management.background`：SDK 的 `api.plugins.background` 以显式 `background:true` 传输元字段发送管理写请求，不携带已结束的 invocationId。HostHandler 仅接受插件管理方法、拒绝同时携带 invocationId，移除该元字段并注入当前 `_instance`／`_background` 后交给 daemon。daemon 在初次准入和异步提交锁内均检查 background 及具体动作授权；停用、撤权和旧实例失效仍拒绝。只读目录和兼容策略可沿当前实例重新读取；普通管理 API、模型、工具、会话和钩子原有调用租约边界不变。

插件目录订阅只有当前实例内的监听器，不传递其他插件的配置、事件日志或凭据；不提供跨重启游标或逐条迁移重放。消费者先订阅再读取完整目录，并合并读取期间的失效通知后补读；daemon 广播接收落后时再次通知所有活动实例重新读取。解除订阅及宿主退出回收监听器，旧实例的读取仍由 plugins.list 身份准入拒绝。DSH 全局 internal/status 的观察者接收由当前目录生成的只读生命周期投影（entry.options、state、await），本地可执行 fiber 仍归真实 Cordis；外部故障不得触发观察插件自身的失败或处置。观察按包摘要、运行代次、状态与原因去重；恢复及移除使旧故障清除，不维持第二套插件运行状态。

DSH 的 `hmr.runExclusive(callback)` 串行运行当前实例的 profile 应用操作，禁止嵌套；回调异常不阻塞后续显式操作。`reconcileProfile(requiredIds?)` 必须在该事务内调用：先沿原提交入口结算当前调用的清单／启停提案，再由 `plugins.reconcile` 核对运行目录并刷新 profile 投影。必需 Loader 条目未运行时失败；已启用但未激活的条目返回可执行的授权／运行状态提示。不可变安装归档的版本替换仍归原安装事务，因此没有物理 profile、文件监听或另一份模块缓存；该服务不提供上游 `watchConfig` 或任意模块替换。调用结束、取消和实例处置后，遗留队列及快照访问拒绝。页面刷新继续运行原包代码与共享视图桥。

DSH `profileContext.packageManager` 提供插件实例拥有的内部 Node 命令入口。原包通过当前 Node 的 `spawn(node,[script,...])` 启动 JS／MJS／CJS 辅助脚本时，脚本继续运行原字节，profile 文件读取仍来自 daemon 快照。同步调用该包管理入口只阻塞辅助进程：独立传输进程等待父扩展经 `plugins.installBundle` 取得实际结果，再刷新同一调用的 profile 投影。只接受当前 profile 的 `add`、npm 版本／范围／标签、公开 GitHub 仓库引用以及工作区、忽略脚本、强制获取选项；不会启动 pnpm 或执行构建脚本。GitHub 引用在同一 daemon 安装任务中固定为提交，原包、来源、版本与回执沿原生命周期保存。其他 Node 进程调用形态和非 Node 原生程序保留原有路径。

辅助进程仅取得父实例私有管道的独立随机委托，不继承 daemon 扩展 RPC 凭据。每实例最多 16 个直接辅助进程、32 个连接，请求和回执各限 4 MiB；读写仍经当前父实例的 SDK 和 daemon 授权入口，派生辅助进程继续沿父委托转交。成功退出前，变动过的清单／启停提案按当前摘要复核并经既有 profile 提交入口结算；失败退出丢弃提案，不写物理 profile。调用取消、子进程退出和实例处置撤销该委托，并取消其准备中的安装；已进入提交的安装仍按原任务合同结算。包管理能力的使用单独进入安装授权预览，单纯读取 profile 名称或目录不因此增加安装／代授权委托。

在线安装任务由 daemon 持有，安装及授权事实仍只写入 PluginManager。`plugins/installations.json` 仅保存原始请求身份、候选摘要、提交阶段和回执，不是第二份安装目录。requestId 为 1–128 个字母、数字或 `-_.:`，绑定发起插件、包摘要和完整输入；同身份同输入只执行一次，不同输入拒绝复用。身份不依赖宿主实例代次，重启后仍可沿相同 requestId 读取原回执；每次读取仍须当前实例的 install 授权，后台调用另需 background。取消及实例结束只作用于原发起实例，旧实例不能操纵新任务。

请求先持久化，再开始解析／下载；提交前持久保存候选 ID、版本和摘要；结果先落盘再返回。日志写入失败不进入未记录的提交阶段，也不返回未经持久化的成功。重启时对未完成记录只读取 PluginManager 的当前摘要：候选已存在时返回 `failed`、`installed:true`、`stage:enable` 与 `installation-interrupted`，表示生命周期结算未确认；候选未确认时返回中断错误，不推断安装从未发生。`changed` 保持未知。恢复不下载、不重放安装、不自动授权或激活；已提交的激活意愿仍由原监管器恢复，不能据进程 running 改写旧回执为成功。用户检查安装与授权状态后，用新 requestId 显式重试；同摘要重试沿原操作收敛，不打断运行中的目标。

全局至多保留 256 个任务／回执，同一插件摘要至多 16 个活动任务。完成回执至少保留至原请求创建后七天，之后在新请求进入时清退；容量不足明确拒绝，不提前淘汰仍在保留期的身份。超出保留期不承诺无限去重，不宣称外部副作用 exactly-once。日志损坏或恢复写入失败保留原文件并阻止安装任务入口，已安装插件的读取和运行仍由原服务负责。重复等待中的一个连接断开不会取消其他等待者；最后一个等待者断开或原宿主退出，只取消尚在准备的任务。

准备阶段在线解析 npm 精确版本、标签或范围并保留原包身份与 integrity，最多 840 秒；取消返回 `cancelled` 前停止该准备流程及受控翻译子进程，不能提交候选版本。进入应用阶段后返回 `too-late`，daemon 继续结算实际结果；已结算或非本实例任务返回 `not-running`。`application:applied` 只有在请求的安装及启停完成后返回；安装已提交但启用失败返回 `failed`、`installed:true`、`stage:enable`，缺少独立授权时带 `authorization-required`，调用方应引导用户处理权限，不能再次伪装为安装未发生。安装任务不会在自己的调用内升级发起插件自身，也不会执行 npm 安装脚本。

插件私有页面的路由请求可携带可选 `headers`（`PluginRouteCallRequest`）。只转交页面自己提供的应用头，不转交 Magi HTTP 请求的 Cookie、凭据和 Origin；Host、Origin、Cookie、代理与传输控制头禁止覆盖。DSH 原生页面的 localStorage/sessionStorage 由 iframe 消息桥按插件 ID 隔离代理，仅保存浏览器 profile 内的界面缓存；插件业务配置仍走 daemon 设置 CAS。右栏导航仅能打开当前激活插件清单声明的右栏视图。

路由载荷只有一个实际正文：`body` 承载 JSON 值或文本，`bodyBase64` 承载原始 HTTP 字节，两者不能同时包含非空值。字节载荷使用标准、有 padding 的 base64，解码后最多 2 MiB；请求与响应的 JSON 消息最多 3 MiB 加 64 KiB 元数据，保留 4 MiB 宿主帧的封装空间。限制在执行业务前核对，非法编码、冲突或超限明确失败；不截断载荷，也不重放业务。独立图片附件的分块合同不受此路由限制影响。

`api.route()` 返回值可提供 `headers` 和 `bodyBase64`。应用响应头（例如 Content-Disposition、ETag、业务回执头）经 daemon 校验后进入本插件的 Fetch Response；Set-Cookie、宿主身份、代理和传输控制头不进入 Magi origin。DSH 的 Node／Fetch 处理器仍运行原代码：页面 Blob、File、FormData 和 Request 正文按字节上传，保留原 multipart 边界；PNG、ZIP 等响应按字节返回，文本／JSON 仍提供结构化结果。HEAD／OPTIONS 进入同一路由准入；HEAD 和 204／205／304 在页面没有响应正文。页面关闭、取消、停用及旧实例失效继续沿原调用租约回收，无第二条文件服务。

视图消息桥支持按文档 `instanceId` 和在途 `requestId` 取消 `route.call`（`type:cancel`，不新增业务写入口）。Renderer 关闭对应 HTTP 等待，daemon 沿原调用租约通知 Node 取消；Node 请求/响应释放后拒绝迟到写入。原页面的 fetch 以真实 Request 处理输入、相对路径及 signal 优先级；普通 fetch 仍是有界完整响应，HEAD 无正文。取消只能终止等待与可取消执行，不能撤销已提交副作用，不自动重放。资源和设置写入不采用该取消消息；页面销毁后所有等待结束且旧回执不再生效。

DSH 页面里的 `EventSource` 通过相同路由调用承载 SSE。兼容层保留 `/__magi_compat/streams` 控制路由，打开、读取和关闭均经过上述 daemon 准入，不增加监听端口或会话写入口。连接只可调用本插件已登记的 GET 路由；Node 与 Fetch 响应均须为 `200 text/event-stream`，`204` 表示结束且不重连。每插件至多 8 条连接，每条至多保留 2 MiB／8192 个未确认块；按单调序号确认和分批返回，重复读取不丢未确认数据。读取等待至多 15 秒，45 秒无读取自动关闭。Renderer 按文档 nonce 记录连接归属，导航/卸载会取消在途路由并关闭已登记连接，旧文档回执不能更新新页面；上游路由撤销和宿主停用也关闭对应原请求。SSE 的命名事件、多行数据、跨块 UTF-8/CRLF、事件 ID 与重连由页面适配器承载；普通 fetch 仍返回完整响应，WebSocket 未由此实现。

`api.config`／`api.configRevision` 是只读配置快照。SDK 不提供 `register.skill`、`register.mcp`、`register.dispose`、`migration.run` 或任意事实写入接口。MCP 为内置功能，Skill 来自静态清单或声明后提交的运行期贡献。未知方法返回 `MethodNotFound`。

### 会话消息交付

`context` 不独立启动模型：当前模型 Task 尚可接收输入时沿原生引导边界提交，否则保留在 SessionInput 待处理列表，由下一次模型 Task 接纳事务领取。插件命令不会领取这些上下文。`steer` 优先补充当前模型 Task，不能引导时进入原队列；`followup` 始终排队启动下一次模型 Task。命令本身没有模型循环，steer 在命令结算后沿原调度入口唤醒模型。所有模型调用、取消与用量仍归原生 Task/Turn。

消息限非空文本 200000 UTF-8 字节、消息身份及来源各 128 字节；来源不得为 user/system/assistant 或包含控制字符。每插件在同一会话最多 32 条未领取消息。身份按插件、会话及 messageId 生成；相同身份的原始内容、来源或调用归属改变时拒绝。回执的 pending/attached/consumed/cancelled 来自同一 SessionInput，表示接纳、绑定、消费或取消，不代表模型已经成功答复。原始正文与接纳时插件名称写 canonical；模型输入和重建历史使用同一来源表达。

待处理消息沿原 Input 日志恢复，重启不自动执行 context；用户可取消，不能编辑原插件消息或把它改投递为用户输入。UI 完整展示已交付正文及来源。已接纳消息是会话事实，停用插件不撤销已提交内容；它仍可由用户取消。DSH 的同步 inject/steer/followup 在本次业务成功返回后统一等待真实 daemon 回执，失败、取消及失效调用不提交临时消息；不为这些方法另建 Agent、Session 或消息存储。

## 5. 注册

清单声明、用户授权和包摘要决定能力上界。静态贡献直接来自清单；动态贡献需要清单的 `extension.runtimeContributions` 显式列出 `tools`、`commands`、`modelSources`、`settings` 或 `skills`。只有声明对应动态贡献种类时，`tools`／`modelSources` 权限才允许 `"*"` 目标；它不授予模型调用、存储或其他能力。钩子仍完全静态，不能降低 guard 模式。插件工具名由 Magi 生成，作者不依赖模型侧编码后的名称。

`catalog.replace` 接收 `{tools?,commands?,modelSources?,settingsSchema?,skills?}`；省略的数组按空数组处理，设置 schema 省略或为 null 时使用安装清单。revision 从每个新实例的 0 开始，成功提交递增，版本冲突或任何校验失败都不改变旧目录。动态目录最多 2 MiB（为 4 MiB RPC 帧保留余量），最多 1024 个工具、128 个命令、8 个模型来源、32 个技能；动态工具的单个输入 schema 最多 256 KiB，安装清单与设置 schema 仍为 32 KiB；安装清单仍限 128 项贡献，合并后继续校验共享 ID、各项 schema 与授权上界。先提交目录，再调用 `api.tool`／`api.command` 绑定；撤销时先释放处理器，再提交新目录。同批注册/回收合并为一次目录提交，避免逐工具复制全目录。移除模型来源声明也会清除该来源已发布的端点和模型。

动态技能通过 `plugin.skills` feature 协商，复用 `PluginSkillContribution` 的 ID、标题、说明及原文正文校验；不绑定执行处理器。输入框从已有 `/api/plugins/manifests` 读取技能，模型目录和调用从同一 manager 实时解析；不另设技能 HTTP 目录或写入本地技能设置。配置替换、停用、撤权、升级与实例退出沿同一目录撤销；应用重启后由新实例重新注册。

初始化阶段的动态目录仅供本实例校验与绑定，`host.ready` 后才进入对外贡献投影。动态目录不写入安装包或持久设置，重启后重新提交。daemon 为每个实例签发独立租约：旧实例不能修改新目录、标记新实例就绪或清理新来源。停用或撤权一经提交，旧租约永久失效；重新授权并激活也不能恢复它。

同一插件只激活一个包摘要。停用、撤权、升级先关闭准入并停止旧宿主；注册随实例释放。没有独立的持久动态注册表，也没有通配远程目录注册。

`api.onConfig(listener)` 按通知顺序交付 `{version,value}`，`api.onDispose(disposer)` 注册回收动作；两者返回取消监听函数。入口也可返回一个 disposer 或 `{dispose}`。`api.settings.onValidate(handler)` 只能注册一个校验器；它补充 daemon 的 JSON Schema 校验。`api.fail(error)` 报告无法继续的运行失败，由原宿主监管回收并按既有策略处理。

## 6. 钩子合同

同步结果为 `{decision:"continue"}`、`{decision:"block",reason}` 或 `{decision:"replace",patch}`。block 必须有非空原因；replace 必须是有界对象，且只作用于该钩子合同。guard 和 advise 均可返回 block；guard 失败阻止，advise 失败保留原生行为。扩展不能批准原生拒绝或代用户审批。

| 钩子 | 当前模式 | 输入／提案与落点 |
| --- | --- | --- |
| `session.created/deleted` | observe | 提交后的脱敏领域事件 |
| `turn.accepted/end` | observe | 接纳／终态事实；不含完整正文 |
| `input` | guard | `{raw:{text,imageCount},kind,requestId}`；接纳事实前，幂等重放后；只能继续或阻止，replace 按失败阻止 |
| `prompt.contribute` | advise | `{target,phase,round,budget}`；patch 为 `{fragments:[{id,stability,priority?,text}]}` |
| `context.assemble` | advise | `{sessionId,contextWindowTokens,rewritable:{from,to},messages,omitted}`；patch 为 `{rewrites:[{from,to,replacement:[{role,text}]}]}` |
| `model.request/response/resolved` | observe | StepModelClient 提交后的模型与用量脱敏投影；不支持参数改写 |
| `tool.call` | guard／advise | `{callId,tool,input}`；patch 为 `{input}`，在账本指纹、目录、并发与权限审批前生效；最终入参重验 schema／权限 |
| `tool.result` | guard／advise | `{callId,tool,status,projection}`；patch 为 `{projection}`，只改模型可见文本；原始 canonical 回执不变 |
| `compaction.before` | advise | `{reason,compactedMessages,originalTokens,summaryBudgetTokens,source}`；block 取消，patch `{summary}` 需授权与原生质量门禁 |
| `subagent.spawn` | guard | `{role,taskName,goal,toolCallId}`；原生完整预检前，只能继续／阻止，不支持改角色或模型 |
| `usage.update` | observe | 已提交用量事实，不含凭据 |

实际调用链见设计文档第 5.6 节及 [`extension_bridge.rs`](../crates/magi-daemon/src/daemon/extension_bridge.rs)。工具钩子作用于有工具的 Task 管线，不能据此声称纯 conversation 会执行工具。

提示片段每条最多 8K 字符、每阶段总计 16K 字符；start 仅 stable 并冻结，round 仅 volatile 并放在尾部。来源由 daemon 注入，不能伪造原生系统段。历史投影 patch 最多 64 KiB；模型可见原文逐条截断且总量有界。历史改写与自定义摘要必须有独立 `contextRewrite:["history"]` 授权；原始会话不被替换，改写来源与缓存归因进入 ModelStep，界面显示通知。

## 7. 租约与重入

resource／settings 可在 activate 期间调用；其他业务服务必须拥有仍活动的工具／命令租约。当前钩子和 observe 回调内不得启动模型、工具、任务、通知或提问，返回 `ReentryDenied`；不存在隐式重入或后台无归属动作。

插件管理预览可在活动宿主内读取；安装、启停与移除需要活动调用租约（包括已准入的插件视图路由）。下载／翻译及排空可能异步等待，提交前在 manager 锁内重新核对发起插件摘要、宿主租约、激活与授权，取消沿原调用结束；不在钩子内发起这些写操作。安装不会继承目标旧版本授权，也不执行上游安装脚本。失败可能已完成安装或停止等先前步骤，不宣称整组操作具有自动回滚或幂等性。

工具服务只开放原生工具，拒绝扩展自调用／跨扩展工具递归。Task／审批／canonical 日志不向 Node 暴露可写对象。资源使用 CAS；作业使用持久幂等身份；不得宣称所有反向服务都有通用 requestId 去重或外部副作用回滚。

## 8. 大负载（blob）

`blob.put` 接受 `{value,contentType?}`，返回 `{ref,bytes,expiresInMs:300000}`；`blob.get({ref})` 返回 `{value,contentType}`。当前只存 JSON，单项最多 2 MiB，每宿主最多 64 项／16 MiB，5 分钟过期，归当前插件所有；跨插件读取拒绝。

调用结束不保证立即删除 blob；宿主替换会清空其存储，过期或不存在返回 `BlobExpired`。持久数据使用 resource.write，不提供任意文件读取或 32 MiB 二进制传输承诺。

## 9. 作业与数据版本

`job.run` 由 daemon 原生计划触发，复用 `api.command` 处理器。计划只保存命令、会话、输入、包摘要、时点、间隔与预算；执行事实引用 canonical Turn，不复制会话或用量。触发前持久化 pending 身份；重启先核对 canonical 事实，不重放已接纳副作用。

失败、取消、未知结果不自动重试；重复间隔至少一分钟。模型调用次数 0～100、单次输出 1～32768 Token、作业期限 1000～120000ms，由签发租约强制；Node 直接外调不在 Magi 模型账本内。

不提供迁移会话或转换 VM。升级改变 dataSchemaVersion 且已有持久数据时，预览必须提示 dataReset；升级清空该插件资源与设置，不触碰 MCP、原生会话及其他插件数据。新版需重新授权与激活。

## 10. 失败、超时与取消

| 情形 | 当前行为 |
| --- | --- |
| guard 异常／超时／熔断／宿主缺失 | 阻止动作并提供原因，不默认放行 |
| advise 失败 | 保留原生行为并记录诊断 |
| observe | 有序队列但不承诺完整送达；队列最多 1024，领域广播落后也可能丢事件 |
| 期限 | guard 默认 5 秒、上限 30 秒；advise 默认 5 秒、上限 15 秒；工具 60 秒，命令最多 120 秒 |
| 熔断 | 连续 5 次失败，或 60 秒内至少 10 次且失败率过半；冷却 30 秒后只放行一次探测 |
| 崩溃／卡死 | 挂起请求以 HostGone 结算；心跳失败回收；1 秒起指数退避，上限 60 秒，10 分钟超过 5 次崩溃停止自动重启 |
| 取消 | 先使调用租约失效、通知 host.cancel；只读工具为 Cancelled，可能写入且结果不确定为 Indeterminate，不自动重试 |
| 停用／升级 | 关闭准入、quiesce、终止宿主，然后提交生命周期状态 |

超时通知不证明扩展已停止所有直接 OS 行为。进程隔离、AbortSignal 和业务租约不能提供对完全受信恶意代码的撤销或只读保证。

## 11. 错误码

响应为 `{"code":-32000,"message":"…","data":{"kind":"…","retryable":false,"detail":null}}`。当前 kind 集合为 ProtocolError、VersionMismatch、StaleGeneration、MethodNotFound、InvalidParams、Conflict、RegistrationDenied、PermissionDenied、Blocked、Cancelled、DeadlineExceeded、ReentryDenied、Deadlock、Backpressure、HostGone、BlobExpired、Internal；定义见 [`protocol.rs`](../crates/magi-extension-host/src/protocol.rs)。枚举存在不等于每条服务都会产生对应错误。

## 12. 安全与诚实披露

扩展以用户权限直接读写文件、联网和派生进程。full-trust 是明确授权告知；storage／models／tools 只限制 Magi 服务入口，不是 Node OS 权限沙箱。插件可读取同用户凭据，daemon 业务所有权不抵御同用户恶意代码。

包摘要证明字节身份，目录签名证明来源链，均不证明代码安全。安装不得执行脚本／在线构建；依赖应由作者构建期打入固定单入口。不提供任意 Node 原生模块或包内二进制。

## 13. 一致性验收（协议层）

| 验证面 | 可重复入口／证据 |
| --- | --- |
| 帧、握手、注册、预算、取消、卡死及崩溃监管 | `cargo test -p magi-extension-host --all-targets`：15 单元＋17 真实 Node 集成 |
| 清单、权限、全局资源、生命周期、签名目录与计划持久化 | `cargo test -p magi-plugin-system --all-targets` |
| daemon 接线、资源与模型授权 | `cargo test -p magi-daemon --lib extension` |
| 接纳前作业取消收口 | `cargo test -p magi-api --lib plugin_jobs` |
| 本地完整安装／服务／调度／停用／升级／卸载 | `npm run test:local-plugins`：20 项 |
| 真实工作台计划创建与 390px 深色视口 | `npm run test:local-plugins-ui`：21 项，含上述 20 项 |

以上是本机证据；没有运行全仓测试、Windows 实机、正式安装包、长期负载及所有断电注入。当前不将这些项声明为通过。

## 14. 变更规则

公开 API 变化必须同步 schema／校验、宿主、调用方、作者指南与一致性验收；不支持的契约明确拒绝。破坏性传输／API 语义须升级对应主版本，新增协商能力须显式声明。固定上游版本与翻译器报告另行评审，不通过默认分支更新扩大兼容声明。
