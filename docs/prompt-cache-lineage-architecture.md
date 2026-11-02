# Magi Prompt Lineage 与缓存根架构

状态：缓存身份、失效边界与共同正文 Prompt IR 已接入运行时、canonical ModelStep 和三种 HTTP 协议。恢复控制消息已从 transcript 分离，HTTP attempt 的请求摘要、状态、已报告用量和统计归属已接入。canonical Step 与独立 auxiliary 请求的崩溃后 usage 投影恢复、重置/保留期门禁和冻结模型归属已接入；真实账号验证不能由本地夹具代替。

当前代码入口：`crates/magi-conversation-runtime/src/prompt_lineage.rs` 负责 lineage、branch、provider scope、摘要和断代算法；`StepModelClient` 为 orchestrator/worker/context-compaction 选择分支。辅助模型使用显式 `AuxiliaryModelPurpose`，不从共用的 `UsagePhase::Integration` 猜测用途。持久事实由 `magi-session-store::ModelPromptFrame` 和 `ModelCacheBinding` 拥有；不建立第二份 lineage 数据库。`magi-bridge-client::prompt_ir` 拥有共同正文编译器；提供方渲染器直接消费 `PromptIr`。Bridge 还提供实际连接身份、统一工具规范化和 `ProviderCacheContract`，不拥有会话缓存身份。

## 根因

提示词缓存的实际对象是“渲染后的完整前缀”，不是 `prompt_cache_key` 字段。当前代码已经稳定了会话键、工具排序、动态尾部和 Provider usage；Context Authority 与对话装配器声明可信消息来源和生命周期，共同编译器规范化正文、工具调用身份/参数和工具 schema。`PromptFrame::from_ir` 与 Provider adapter 消费相同的 IR 规则；协议只决定 wire 表达和原生缓存断点。实际 Bridge 通过 `ModelBridgeClient::cache_prompt_ir` 提供缓存摘要入口，HTTP 连接在该入口复用真实发送前的能力降级与工具 wire name 编码，避免 canonical 工具名摘要与实际请求分叉。

原实现有四类根本问题，本次重构已处理提示词和缓存身份根因，并接入第四类的逐次传输证据与用量归属：

1. 同一 session 内切换模型、推理强度、权限、Skill 或工具面时，缓存身份没有明确断代。
2. 主线、worker 和 auxiliary 共用 session 键，但它们的历史分支不同，容易争用同一个上游路由亲和性。
3. 工具目录虽然排序，嵌套 schema 和协议渲染仍可能改变 wire 前缀。
4. usage 只能告诉我们“读到了多少”，不能把一次 miss 绑定到具体前缀代际、压缩边界或传输重试。

## 一级事实：Prompt Lineage

daemon 的对话运行时新增唯一的 `PromptLineage` 权威。它拥有某一执行分支从静态规则到当前请求的完整渲染身份；Bridge Client 和 UI 不生成缓存身份。

```text
PromptLineage
├── lineage_id                 # 稳定的逻辑分支身份
├── cache_epoch                # 模型/权限/Skill/工具/压缩变化后的断代
├── provider_scope              # provider、协议、规范化 endpoint、resolved model
├── branch_scope                # orchestrator / worker(task) / auxiliary(purpose)
├── render_version              # Prompt IR 与 wire renderer 版本
├── stable_prefix_hash          # 可复用的完整前缀哈希
├── tool_surface_hash           # 工具名称、描述、schema、顺序的规范化哈希
├── provider_context_hash       # 稳定前缀中的 thinking/signature/Responses 私有上下文摘要
├── history_generation          # canonical history / compaction 代际
└── invalidation_reason         # 首个变化原因
```

`cache_key` 是 `PromptLineage` 的传输投影，而不是业务事实：

```text
cache_key = SHA256(
  "magi-prompt-cache-v2" + provider_scope + branch_scope + lineage_id + cache_epoch
)
```

同一逻辑分支的重试、输出上限调整和恢复沿同一 lineage；模型、provider、实际发送的推理参数、权限、Skill、工具面或压缩改变时创建新的 epoch。旧 Step 的身份不可改写。全局 settings revision 和绑定 revision 仍作为执行快照保留，但不参与缓存 epoch。

网关账号亲和与 Provider 缓存键分开：`X-Session-Id` 使用稳定的 `lineage_id`，请求体的 `prompt_cache_key` 使用包含 epoch 的投影。一次工具或权限变化会切换缓存代际，同时保持同一逻辑分支的账号亲和。未知兼容 Chat 端点不接收未经声明的缓存扩展字段；稳定亲和头不依赖该字段是否受支持。网关是否启用亲和仍由网关配置决定。

环境回落 HTTP client 在 daemon bootstrap 解析一次，直接承担缓存 IR、连接身份和发送合同；设置变更仍由既有会话/角色模型解析入口处理，不经过逐方法重建的转发包装器。

HTTP 客户端从与真实发送共用的 renderer 取得规范化端点、实际模型、协议和实际推理字段；不从可变设置重新解析，也不把未发到 wire 的 effort/capability 元数据计入身份。端点 userinfo/query/fragment 和 API Key 不进入持久记录，运行时只保存连接摘要。工具哈希和 transport 共用 `canonical_tool_surface`，先排序目录、递归规范化对象键，再计算摘要；数组顺序保留。

## Prompt IR：先分层，再渲染

Context Authority 与对话装配器提供带可信 `ChatMessageOrigin` 的源消息，`compile_prompt_ir` 是正文规范化的唯一入口。主线、worker 和辅助请求都从实际 Bridge 取得该 IR，再交给 `PromptFrame::from_ir`；`from_request` 仅供纯编译器测试，不开放生产绕过连接预处理的摘要入口。正文 IR 只存在于本次请求中，不持久化第二份 transcript：

| 层 | 内容 | 缓存规则 |
| --- | --- | --- |
| `StaticPolicy` | 系统规则、产品合同、不可变角色约束 | 只在版本或配置变化时失效 |
| `SessionStable` | 项目规则、稳定记忆、权限声明 | 对同一 lineage 保持字节一致 |
| `AppendOnlyHistory` | canonical Turn 历史、已确认工具回执 | 只追加，压缩时创建新 epoch |
| `ToolSurface` | 工具名、描述、schema、来源和授权 | 规范化后单独哈希，变化即断代 |
| `RoundContext` | 当前计划、邮箱、执行步骤、引用资料 | 只放在前缀之后的尾部，不进入历史缓存段 |
| `ProviderContext` | thinking/signature/Responses output item | 由 provider contract 决定可否回放 |

稳定层不得混入当前时间、随机 ID、未确认的异步投影或仅供 UI 展示的字段。缓存身份与持久诊断只保存摘要，不保存正文和 secret；用户输入与已确认工具证据仍由原 canonical history 拥有。工具调用 ID 来自 canonical history，缺失 ID 使用规范化后的参数确定性派生，JSON 空白和对象键顺序不影响身份。

`StaticPolicy` 由装配器根据片段的静态生命周期声明，不能由正文中的特殊字符串触发。旧缓存 boundary 标记与 `LlmMessageParams` 已删除；字面相同的用户文本现在只是普通内容。IR 保存消息来源，renderer 不再依赖另一个易漂移的 volatile 消息计数。PromptFrame 与 renderer 共用 `PromptIr::history_end` 边界；其后仍带 Conversation/Skill 等来源的消息也只属于尾部，postBoundaryHash 保留误放位置的诊断。renderer/摘要版本 4 对旧记录明确断代，旧 Step 仍按原版本读取。系统规则仅来自有序消息列表，已移除独立的 system_prompt 字符串入口，避免摘要和 wire 分别读取不同正文。Anthropic 先分别渲染历史和 RoundContext，再给历史末个受支持 user block 放断点；首个 RoundContext 之后的恢复说明也受同一尾部边界保护。

worker 的 lineage 只拥有一个 task，冻结交接引用在该 task 生命周期内保持稳定前缀；主线 lineage 跨 task/Turn，当前任务引用放在尾部。邮箱、计划和非持久历史边界说明对所有分支统一放在易变尾部，不再由 worker 的旧顺序决定这些状态的位置。持久化历史、压缩、恢复和权限事实继续沿原有权威入口，不因缓存而复制或延迟授权撤销。

## 分支策略

主线、worker、auxiliary 不再默认只用 session 一个键：

```text
session/<id>/orchestrator
session/<id>/worker/<task-id>
session/<id>/auxiliary/<purpose>
```

分支可以复用相同的 `StaticPolicy` 和 `SessionStable` 前缀，但拥有不同的 route affinity，避免并发 worker 改写主线的上游亲和性。只有当 auxiliary 明确发送同一历史前缀时，才复用父分支的 prefix lineage；短的独立辅助请求使用自己的分支。

## Provider Cache Contract

每个连接在模型目录或能力探测中生成 `CacheContract`：

```text
CacheContract
├── route_key: supported | unsupported | unknown
├── cache_control: none | anthropic_breakpoints
├── continuation: none | previous_response_id | websocket
├── usage_shape: provider-specific parser
├── minimum_prefix_tokens: optional
├── ttl: unknown | provider-declared
└── renderer_version
```

Responses、直接 OpenAI Chat、Anthropic 和自定义代理不能共享一套“猜测式”字段策略。unknown 必须保持保守，不发送未确认扩展，也不能把缺失 usage 解释成 miss。

## 用量与诊断事实

账本以逻辑调用和传输尝试分层：

```text
logical_call_id
├── lineage_id
├── prefix_hash
├── cache_key_fingerprint
├── expected_cache_contract
└── attempts[]
    ├── transport_attempt_id
    ├── request_hash
    ├── provider_status
    └── provider_usage
```

当前失效通知复用已存在的 `session.turn.step` / `model_step_updated` 事件，不再另建 `model.cache.prefix_changed` 写入路径。该事件与 session/read、重启重放读取同一个 ModelStep。`promptFrame.cacheBinding` 保存 lineage、实际发送的缓存键、提供方/模型/推理/权限/Skill 摘要及 `invalidationReason`；正常续写为 null，首次记录为 initial。规则正文变化没有更具体来源时报告 stable_prefix_changed，不猜测成 Skill 或权限变化。

每个 Step 只保存历史条数和末端哈希链。当前请求在内存计算前缀链，比较前驱记录长度处的摘要：一致为正常追加，旧消息被删改或重排时为 `history_rewritten`；Provider 私有上下文（thinking/signature/output item）遵循所属消息的生命周期：随 append-only 历史正常追加时保持 epoch，改写旧历史时沿同一条链报告 `history_rewritten`；稳定前缀中的私有上下文变化报告 `provider_context_changed`；首个 RoundContext 及其后全部消息和私有上下文归易变尾部，不进入历史链或 epoch。改写后的 epoch 标识沿后续追加传递，避免每一步都再次轮换。摘要存储不随历史长度增长；独立辅助请求没有追加历史合同，内容变化不被误报为历史改写。

`PromptFrame::diff` 继续提供请求内各段变化与首个不同段的诊断，不保存请求正文。每个 `ModelAttempt.transport` 现在保存规范化实际 wire body 的 SHA-256、HTTP 状态、供应商已报告的 usage 和传输 outcome。发送前必须持久化请求摘要；记录失败阻止发送。SSE usage 按协议累积快照记录，空流或缺少 terminal 的失败不再丢失已经收到的用量。非 HTTP 执行器和旧记录保持未知，不从逻辑响应伪造传输证据。

HTTP 观测与重试进度是两个合同：重试通知继续负责在再次发送前申请原 Step 的新 attempt；观测器负责冻结该 attempt 实际发送的 body，包括强制 tool_choice 降级后的不同请求。普通非流式、流式与带输出限制的请求共用该路径，移除了独立阻塞 HTTP 重试实现。凭据、HTTP 头和正文不进入回执。

用量投影以 `attempt_id` 作为 `callIdentity.callId` / event 去重键，以原 Step 或辅助调用 ID 作为 `parentCallId`；`callIdentity.transport` 提供请求摘要与 HTTP 状态。统计中的调用数对应传输尝试数。已绑定主线/worker/压缩请求从 canonical Step 投影；独立辅助请求直接使用相同 observer/reducer 的回执，未建立第二份持久 transcript。没有缓存 usage 的 attempt 不进入缓存命中率样本。上下文压力仍使用最后一次成功请求的输入，不能把多次尝试的用量总和当作当前上下文大小；Goal 则沿原 Step 事务累计各 attempt 已报告的总消耗。

当前仍保留一个边界：canonical Step 的业务提交与 usage 事件追加不是同一个物理文件事务；恢复投影使用确定性 attempt event ID、冻结的脱敏绑定和账本重置/保留期门禁，重复恢复不会重复计费。非流式响应头已到达但读取响应体失败时，HTTP 状态会先写入 attempt；未知用量不能推算成零成本。

统计分别回答：

- provider 是否返回了缓存 usage；
- 返回的读取量是多少；
- 哪些 lineage/branch 发生了失效；
- 传输重试是否产生额外 usage；
- 失效是 Magi 前缀变化、provider 能力未知，还是 provider 没有命中。

## 输入区会话统计

输入区的 `SessionStats` 与上下文圆环分别展示累计调用用量和当前上下文压力，二者不合并分母。
`GET /api/settings/stats?sessionId=…` 沿原用量账本与 `UsageAuthority` reducer 过滤会话，
全局设置页仍复用同一入口。全部调用包括图片/视觉等角色，主对话、子代理和辅助调用可独立筛选。
调用数按 HTTP attempt 计数；统计继承账本保留期与全局清零边界，不承诺被清零或过期前的终身累计。

- 缓存 token 命中率 = 已报告缓存读取 / 同一观测样本的完整输入；请求命中率 = 命中请求 / 已观测请求。
  零与缺失分别保留；缓存写入单独统计观测覆盖，不能用读取观测推断写入已报告。
  OpenAI Chat 与 DeepSeek 的 `prompt_cache_hit_tokens` 共用同一流式/非流式解析器；
  DeepSeek hit 已包含在 prompt token 内，不重复加入完整输入。
  输入已含缓存但缓存读取超过完整输入的异常回执不计入命中样本，不能用饱和相减制造 100% 命中。
- `ModelTransportRecord.responseTiming` 在 HTTP I/O 线程采集正文首段和末段时间，沿原 Step /
  auxiliary 回执持久化。仅成功交付的正文流参与性能累计；TTFT 按有效流次数平均，TPS 按可配对
  正文 token 总量 / 首末正文网络接收间隔总和计算，不平均各次速度。同一 HTTP read 的所有
  SSE 事件共用接收时间，不能把解析 CPU 耗时当作生成间隔。它是受网络与上游缓冲影响的客户端
  观测吞吐，不声称是模型内部纯解码速度。OpenAI 协议缺少明确推理 token 拆分、
  混有工具参数/音频/其他非正文输出、Anthropic 含未拆分或 redacted thinking、同批接收或
  非流式输出时不推算 TPS。推理明细必须来自与最终输出数相同的一份 usage，不能沿用中间拆分。
- `ModelTransportRecord.usageObserved` 冻结 Provider 是否实际报告用量；投影到
  `callIdentity.transport.usageReported` 并提供用量覆盖次数。缺失/空 usage 不能被 adapter 的零默认值补造。
  全部缺失时累计 Token 显示未知，部分缺失时展示已报告合计及覆盖数；不能把缺失用量解释为零成本。
  旧记录缺少存在性标记且全零时保持未知；有实际非零 token 或显式缓存字段时保守纳入已知样本。
- Chat 的完整 usage 和 Responses 终态 usage 可修正较早快照；Anthropic 保留协议规定的分段合并。
  持久化约束冻结请求身份和接收顺序；在途快照允许计数修正，用量不能整体丢失。
  计时与用量配对后、HTTP outcome 确认后都不可再改写；canonical 恢复消费最新冻结回执。
- 运行中显示已结算值与待返回提示，不把字符数当作真实 token。旧记录缺少 timing 时保留未知；
  恢复与重复事件仍按 attempt 去重。UI 仅投影服务端数据，切换会话丢弃旧请求结果；刷新失败保留
  上次结果并提供重试，不持久化第二份统计。
- 统计入口在宽容器显示速度与缓存摘要，窄容器收为图标；沿原向上方向展开，按会话列和
  visual viewport 约束尺寸，复用原生 Popover 层避免会话导航穿透。触控入口为 44 CSS px 热区，
  与上下文入口互斥，支持点击外部关闭及 Esc 返回焦点。

## 历史投影唯一权威

模型可见历史是 canonical 状态的**纯函数**，只有一个实现：`magi-conversation-runtime::history_projection`。它拥有三件事——单条工具结果的模型可见形态（先补事实标注，再做单条上限裁剪）、文件读取事实的有效性校验（重建时按工作区当前内容判定 current/stale）、工具结果总预算（降级边界只落在从头累计的块边界上）。

约束：

- 实时循环的 `messages` 保存**原始**（仅经单条裁剪）序列；每次构造请求时在副本上投影，投影结果不写回。`ContextAuthority::prepare` 同样返回校验后的原始序列，总预算只用于压缩判定。把投影结果写回再叠加预算，会让降级边界按已降级的体积重新计算，与回合边界的重建分叉，超出预算后每个回合边界都使其后的整段历史缓存失效。
- 实时与重建走同一组函数。`ToolResultSlot` 让 `ChatMessage` 与持久化的 `ThreadChatMessage` 共用同一个预算实现，不再有两份拷贝；`project_tool_result` 取代只服务实时路径的专用补丁。
- 一致性由测试强制：逐次追加 150 条结果（含超过单条上限的大结果），实时视图必须与从持久化原文重建的视图逐字节相同，且降级只在块边界前移。

## 声明式前缀改写

改写已发送前缀只有两类原因，由投影声明，而不是靠事后比对猜测：

| 原因 | 来源 | 声明方式 |
| --- | --- | --- |
| `tool_history_demoted` | 总预算把更早的结果降级为摘要 | `HistoryProjection.demotedToolResults` 增长 |
| `file_facts_refreshed` | 工作区文件变化，读取事实 current → stale | `HistoryProjection.staleFileFacts` 增长 |

`HistoryProjection` 随请求冻结进 `ModelPromptFrame.historyProjection`。历史链哈希发现旧历史被改写时，`bind_lineage` 先按上表归因；没有任何计数增长的改写才是 `history_rewritten`，它表示前缀被意外破坏，会记录警告，应当视为缺陷而不是正常现象。模型/权限/Skill/工具面/压缩等其他断代原因保持原有的判定顺序。读取旧记录缺失该字段时按未声明处理。

## 缓存核算

`ModelPromptFrame.prefixBytes / tailBytes` 记录易变尾部之前（含工具表）与尾部的序列化字节数。同一 lineage、同一 epoch（`invalidationReason` 为空）时，上一请求的整个前缀仍是本请求的前缀，因此：

```text
expected_cached = input_tokens × previous.prefixBytes / (prefixBytes + tailBytes)
volatile_tail   = input_tokens × tailBytes / (prefixBytes + tailBytes)
provider_shortfall = max(0, expected_cached − 实际 cache_read)
```

Magi 保持稳定的部分如果没有被读到，缺口归因到 provider 侧（冷启动、淘汰、路由、复用粒度）；Magi 自身的成本只有每次重发的易变尾部和本轮新增内容。字节比例是 Token 比例的近似，只用于判断前缀是否被 Magi 破坏，不用于计费。核算在 Step 结算时用该 attempt 的传输回执用量计算，写 `magi::prompt_cache` 日志，不新增持久事实或账本字段；任何断代的 Step 没有预期（原因已经由 `invalidationReason` 声明）。受控夹具把它变成不变量：只追加的 Step 其前缀字节数不收缩，易变尾部不超过请求的 35%。

不为 `ProviderCacheContract` 增加“复用粒度”字段：中转网关背后的复用行为无法静态声明，实测还不稳定（见 [缓存优化记录](prompt-cache-optimization.md) 7.4），写成合同等于猜测。它用核算结果观测，而不是声明。

## 不做：压缩作为主线分叉

结论：评估后决定不实现（曾实现到可通过单测的程度，见下，因收益未证明、风险在压缩这条关键路径上而撤回）。

把压缩摘要请求改成“主线最近一次请求的前缀 + 一条压缩指令”，可以让最大的单次输入复用主线缓存。如果将来重新评估，必须同时满足以下条件：

- 压缩模型必须等于主模型、整段待压缩历史加工具表能放进窗口，且不是分块或辅助模型路径。任一不满足仍走现有路径，这会形成两条并存的摘要路径，必须由同一个摘要来源接口约束，不能散落在调用方。
- 历史由“不可信数据文本”变成原生角色消息，注入防护从形态变成指令；工具表必须保留以维持前缀，因此要用 `tool_choice: none` 禁止调用，各协议的行为需要逐个验证。
- 必须有可测的收益目标：本网关上对非整体延长的请求，上游复用不稳定，无法证明收益。

## 所有权边界

| 组件 | 权威职责 |
| --- | --- |
| Context Authority / 对话装配器 | 源消息及其可信生命周期、history generation、compaction epoch |
| Prompt IR compiler | 规范化正文、工具调用与 schema，向摘要和 renderer 提供同一套结构规则 |
| History Projection | 模型可见历史的唯一纯函数投影：单条形态、文件事实、总预算，并声明对已有结果的改写量 |
| Tool Surface Authority | 工具目录、授权、规范化 schema、tool surface hash |
| Prompt Lineage Authority | branch、provider scope、cache epoch、cache key、失效原因 |
| Provider Adapter | IR → wire renderer、协议断点、usage parser |
| Model Step | 领取已生成的 lineage 并发送；不自行生成缓存身份 |
| Usage Authority | logical call / transport attempt 去重和聚合 |
| UI | 展示服务端事实，不推断 cache miss 原因 |

## 迁移顺序

1. 引入 `PromptLineage`、`PromptSegment`、`CacheContract` 和稳定哈希，先以诊断模式生成 v2 身份，与现有 v1 键并行比较，不改变发送行为。（已完成）
2. 将已装配的 Context Authority 和 Tool Surface Authority 输出投影为 Prompt IR，验证实时构建与重启重建的 `stable_prefix_hash` 一致。（已完成：装配器声明源消息生命周期，共同编译器产出正文 IR，摘要与 renderer 共用）
3. 将 branch/epoch 纳入键，先覆盖模型切换、Skill/权限变化、worker 和压缩边界。（已完成请求边界接入，失效原因沿原 canonical Step 事件交付）
4. 将 Provider adapter 改为消费 IR，并为 Anthropic、Responses、Chat 和自定义代理分别实现 CacheContract。（已完成三种协议及未知代理的保守合同；未知能力仍不视为支持）
5. 将账本升级为 logical call / transport attempt 两层，接入失效事件和统计页。 空响应、中断与补证控制提示已迁移到 `RequestControls`，重复恢复替换当前原因，得到可用输出后清除；已确认的片段仍沿 canonical 历史追加。探索停滞提醒由当前状态逐请求重建。逐 attempt 的 HTTP 证据、统计和重启补账已接入；辅助请求不创建虚构 Turn，使用独立持久传输事实。
6. 通过连续对话、文件工具、worker、auxiliary、模型矩阵和真实 Anthropic 账号验收。当前生产代码已无 v1 键生成器；主线与辅助入口都调用同一个 lineage 算法。不得为等待真实账号验收保留第二套运行时缓存策略。

缓存预热、WebSocket continuation 和长期 TTL 暂不进入迁移；它们必须建立在 lineage、contract 和 cost ledger 稳定之后。

## 本轮验证与边界

2026-10-31 的独立 daemon 受控夹具覆盖连续对话、多回合文件工具、worker、Prompt 强化及自然辅助请求。验收入口新增 canonical lineage 与实际 wire/header 的交叉核对；夹具 usage 为合成数据，不作为真实命中率提升证据。最终次数和产物见 [缓存优化记录](prompt-cache-optimization.md) 第 8–9 节。

定向回归覆盖无关设置 revision、等价工具排序、实际连接/推理参数变化、历史正常追加/改写、改写后继续、重启恢复、schema DTO 兼容与 daemon 日志重放。真实 Anthropic、网关启用亲和后的多账号效果仍需独立验证；本地 canonical/auxiliary 传输账本的崩溃补偿已完成。
