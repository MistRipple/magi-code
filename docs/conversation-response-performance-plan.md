# Magi 对话响应链路性能开发与验收计划

> 文档类型：架构优化与开发验收基线
>
> 当前状态：阶段 0～5 已完成首轮实现，阶段 6 的本地入口、前端回归与 Electron 打包验收已完成；真实 Provider 五类场景已分别完成 20 轮后端采样，但 Electron 生产 Renderer timing 与性能前后对比仍未完成
>
> 更新日期：2026-08-24
>
> 适用范围：用户发送消息、会话接纳、任务准备、模型请求、流式事件、canonical turn、前端投影与对话区域渲染
>
> 对照源码：OpenAI Codex `068c49f075`；Magi `ff3bdad1`

## 1. 文档目的

本文档用于解决 Magi 在使用相同模型 API 时，相比 Codex Desktop 出现的发送确认慢、首内容慢和持续流式渲染不够轻的问题。

后续开发、代码评审、性能测试和最终验收均以本文档为准。未达到当前阶段退出条件，不进入下一阶段；不以调小单个延迟常量、隐藏 loading 或减少 UI 动画作为性能问题的最终解决方案。

## 2. 当前分析结论

Magi 的主要延迟不在模型 API，而在模型请求前后叠加了过多同步工作。当前主要链路为：

```text
用户点击发送
  -> 前端写入本地乐观 Turn
  -> POST /api/session/turn
  -> 会话与任务分类
  -> Snapshot 初始化
  -> Git 状态观测与 Session Git Context 建立
  -> 任务图、Mission、Thread 和执行注册表写入
  -> 会话与 Git Context 持久化
  -> Runner 启动
  -> HTTP accepted 返回
  -> Runner 调度
  -> 模型配置、知识上下文、技能和工具面准备
  -> 历史读取、token 估算和上下文压缩判断
  -> 必要时同步调用辅助模型压缩历史
  -> 新建 OS 线程、Tokio Runtime 和 reqwest::Client
  -> Provider 请求
  -> Provider SSE
  -> 完整运行 item 写回与 canonical turn 重建
  -> Magi SSE
  -> 前端 reducer、projection 和 Svelte 派生计算
  -> 对话区域渲染
```

Codex 的关键设计差异是：`turn/start` 只等待 Core 完成“启动、引导或拒绝”的路由决定，不等待用户 hooks、模型上下文更新、rollout 持久化和模型采样；模型事件通过独立的长连接事件通道持续发送。

### 2.1 已确认的主要根因

按优先级排序：

1. Magi 的 `/api/session/turn` 在返回 accepted 前同步执行 Git、Snapshot、持久化和 Runner 启动。
2. 首次 Provider 请求前需要完成知识上下文、工具定义、历史读取、token 估算和上下文压缩判断；长会话可能先同步调用一次辅助模型。
3. Magi 每次模型调用重新创建 OS 线程、Tokio Runtime 和 `reqwest::Client`，无法稳定复用 DNS、TCP、TLS、HTTP/2 和 Provider 连接池。
4. 每个 Provider delta 都构造完整累积内容和完整运行 item，并重建 canonical turn。
5. 前端 stream reducer 对完整字符串执行 `Array.from()`，部分状态比较使用 `JSON.stringify()`，projection 会重建变更 Turn 的全部 artifact。
6. 每个 `session.turn.item` 事件还会触发 Goal 权威快照刷新，给浏览器和 daemon 增加额外请求与状态更新。

### 2.2 已排除的错误方向

- 发送前 SSE 预连接采用 `waitUntilOpen: false`，不是当前 accepted 延迟的主要原因。
- Runner 的 100ms sleep 发生在一次 cycle 返回 `Continue` 之后，不是首 cycle 的固定等待；它会影响后续工具循环，但不是首 token 的首要根因。
- 后端 80ms / 24 字符的 stream publish gate 只控制事件发布频率，不能解决 accepted、模型建连和状态重建问题。
- 单纯提高 stream 频率会增加 CPU 和 GC 压力，不能作为“更快”的等价方案。

## 3. 性能指标与统一口径

性能验收必须区分三个阶段，禁止用一个“总耗时”掩盖真实瓶颈。

| 指标 | 起点 | 终点 | 目标 |
|---|---|---|---:|
| 提交确认延迟 | 用户点击发送 | 前端收到 accepted | 本地 P95 不高于 100ms |
| 首内容额外延迟 | 用户点击发送 | 前端收到首个可见模型 delta | 排除 Provider TTFT 后，Magi P95 额外开销不高于 300ms |
| UI 呈现延迟 | daemon 收到 Provider delta | 浏览器完成对应内容绘制 | P95 不高于 50ms |

补充指标：

- `accepted_to_runner_started_ms`
- `runner_started_to_context_ready_ms`
- `context_ready_to_provider_request_ms`
- `provider_request_to_headers_ms`
- `provider_headers_to_first_delta_ms`
- `provider_delta_to_event_publish_ms`
- `browser_event_to_reducer_ms`
- `reducer_to_dom_paint_ms`
- 每 1,000 个输出字符的 Rust CPU、浏览器主线程 CPU、内存分配和 SSE 事件数

## 4. 目标架构

```text
┌──────────────────── 提交控制面 ────────────────────┐
│ 输入校验 -> 幂等/admission -> Durable Submission   │
│          -> canonical accepted -> HTTP 立即返回     │
└────────────────────────┬───────────────────────────┘
                         │
                         v
┌──────────────────── 后台执行面 ────────────────────┐
│ preparing                                             │
│   -> Git / Snapshot / Context / Task preparation      │
│   -> shared model transport                           │
│   -> Provider streaming                               │
│   -> completed / blocked / failed                     │
└────────────────────────┬───────────────────────────┘
                         │ canonical delta events
                         v
┌──────────────────── 前端呈现面 ────────────────────┐
│ itemId + version + delta 增量 reducer                │
│   -> 单 artifact 更新                                 │
│   -> 原始模式 / 摘要模式共享同一事实源               │
└────────────────────────────────────────────────────┘
```

### 4.1 Turn 状态模型

统一状态流转：

```text
accepted -> preparing -> running -> streaming -> completed
                    \-> blocked
                    \-> failed
```

语义规定：

- `accepted`：请求已通过基础校验并可靠写入提交记录，可以在 daemon 重启后恢复。
- `preparing`：正在执行 Git、Snapshot、上下文、任务和模型调用准备。
- `running`：执行器已启动，但 Provider 尚未交付可见内容。
- `streaming`：已经收到 Provider 可见 delta。
- `blocked`：需要用户处理权限、Git 冲突或其他可恢复阻塞。
- `failed`：准备或执行不可恢复地失败。
- `completed`：当前 Turn 已完整收口并完成终态持久化。

### 4.2 核心所有权边界

| 模块 | 唯一职责 | 禁止行为 |
|---|---|---|
| Session submission | 基础校验、幂等、最小持久化、accepted 事件 | 等待模型采样和完整执行准备 |
| Preparation worker | Git、Snapshot、任务和上下文准备 | 重新生成第二条提交事实链 |
| Model transport | Provider 连接、取消、重试、流解析 | 每轮创建独立 Runtime 和 Client |
| Conversation runtime | 模型轮次、工具循环、stream buffer、终态写回 | 每个 delta 重建完整持久化状态 |
| Canonical turn store | 稳定 Turn/Item 事实、版本与恢复快照 | 承担 UI 布局和折叠逻辑 |
| Web reducer | 按版本应用 canonical delta | 重新推导后端业务状态 |
| 原始/摘要视图 | 基于同一 projection 呈现 | 维护两套消息或执行状态 |

## 5. 分阶段开发计划

### 阶段 0：全链路埋点与性能基线

#### 目标

先得到可重复、可比较的真实数据，确定每一毫秒消耗在哪个模块，作为所有后续阶段的验收依据。

#### 开发范围

- 生成并贯穿同一 `trace_id`、`request_id`、`turn_id` 和 `provider_call_id`。
- 后端记录：
  - `submit_received`
  - `admission_completed`
  - `accepted_persisted`
  - `accepted_response_sent`
  - `runner_started`
  - `context_prepare_started/completed`
  - `provider_request_started`
  - `provider_headers_received`
  - `provider_first_delta`
  - `canonical_event_published`
- 前端记录：
  - `submit_clicked`
  - `accepted_received`
  - `stream_event_received`
  - `reducer_completed`
  - `projection_completed`
  - `dom_painted`
- 开发态输出结构化性能报告；正常用户界面不增加长期性能卡片。
- 建立五类固定基准：
  - 新建个人纯文本会话；
  - 已有个人长会话；
  - 工作区纯聊天；
  - 工作区带工具调用；
  - 主代理与子代理并发。

`MagiTurnHarness` 已能在真实 `TurnService` 链路上记录单轮 accepted 返回、非编排 Provider
请求开始、首个 Provider delta、首个 EventBus 流事件和 canonical 终态观察，并保留事件序号
用于验证时间线顺序。该证据用于确认埋点和事件边界，尚未替代五类场景各 20 轮的 P50/P95
采样，也不能作为性能目标已达标的结论。

#### 预计主要文件

- `crates/magi-api/src/routes/sessions.rs`
- `crates/magi-api/src/routes/dispatch_flow.rs`
- `crates/magi-api/src/state.rs`
- `crates/magi-bridge-client/src/http_model_client.rs`
- `crates/magi-conversation-runtime/src/session_turn_execution.rs`
- `web/src/shared/bridges/web-client-bridge.ts`
- `web/src/stores/turn-store.svelte.ts`
- `web/src/components/MessageList.svelte`

#### 退出条件

- 一轮真实对话可以生成完整的端到端时序。
- Provider 自身 TTFT 与 Magi 内部开销可独立计算。
- 五类基准各完成至少 20 轮采样，并输出 P50、P95、最大值。
- 埋点不会改变请求顺序和 canonical 事实。

#### 预计工期

1～2 个工作日。

### 阶段 1：提交接纳与后台准备解耦

#### 目标

让 `/api/session/turn` 只承担可靠接纳，不再等待完整执行准备；这是首个产品体感里程碑。

#### 开发范围

- 新增或收敛为唯一的 durable submission record。
- HTTP 返回前只执行：
  - 输入和范围校验；
  - requestId 幂等校验；
  - 会话可接纳状态校验；
  - 最小用户 Turn / submission 持久化；
  - canonical `turn accepted` 事件发布。
- 把以下工作移入后台 preparation：
  - `ensure_snapshot_session_for_workspace_id`；
  - `ensure_session_code_context`；
  - Git 观测和 baseline 对齐；
  - 任务图和执行计划的重型准备；
  - Runner 启动；
  - 非接纳必要的全量 checkpoint。
- 删除 `/api/session/turn` 对 `finalize_session_task_dispatch().await` 的同步等待。
- preparation 失败时沿原 Turn 发布 `blocked` 或 `failed`，禁止再创建独立错误消息链。
- daemon 重启时从 durable submission 恢复未完成 preparation。
- 前端本地乐观 Turn 与 accepted Turn 通过 requestId 原位合并。

#### 架构约束

- 不允许用纯 `tokio::spawn` 代替 durable submission；accepted 后必须可恢复。
- 不允许长期保留“旧同步提交”和“新异步提交”双轨实现。
- Git 冲突和权限问题从 HTTP 长等待改为明确的 Turn 阻塞状态，不能静默执行。
- accepted 不表示执行已经成功启动，产品文案和事件语义必须与状态模型一致。

#### 退出条件

- 本地 accepted P95 不高于 100ms。
- 大型 Git 仓库、dirty worktree 和慢磁盘不阻塞 accepted。
- accepted 后立即杀死并重启 daemon，Turn 可以继续 preparation 或稳定失败收口。
- 相同 requestId 重试不会生成重复 Turn、任务或用户消息。
- 个人、工作区、目标、普通、继续和编辑重试路径全部使用同一提交状态机。

#### 预计工期

3～5 个工作日。

### 阶段 2：模型 Transport 与连接复用

#### 目标

消除每次模型请求重新创建线程、Runtime、HTTP Client 和连接的固定成本。

#### 目标结构

```text
ModelTransportRuntime（进程级）
  -> 长生命周期 Tokio Runtime
  -> ProviderClientPool
       key = protocol + endpoint + auth identity + proxy/TLS config
  -> ProviderConcurrencyGate
  -> TurnTransportSession
```

#### 开发范围

- `HttpModelBridgeClient` 持有共享 transport handle。
- `reqwest::Client` 按 Provider 配置复用。
- 所有流式和非流式请求进入同一长生命周期 Runtime。
- 保留当前取消、idle timeout、重试、空流恢复和并发门控语义。
- 记录连接复用、DNS、connect、TLS、headers、首 delta 的分段耗时。
- Responses Provider 支持时，设计 turn-scoped transport session：
  - turn 内重试和工具 continuation 复用连接；
  - 支持 Provider sticky routing；
  - 支持 incremental request continuation；
  - 新会话可进行有限时长 prewarm。
- WebSocket 能力只有在取消、重试、代理并发、工具循环和错误归一化全部覆盖后，才能替换对应 Provider HTTP 路径。

#### 退出条件

- 连续模型调用能证明复用底层连接。
- 不再为每个请求创建 OS 线程和 Tokio Runtime。
- 取消后 Provider 槽和连接状态正确释放。
- 主代理和至少五个子代理并发时不阻塞 daemon 异步事件循环。
- 重试不会重复交付已经输出的 delta。
- 相同 API 下的 `context_ready -> first_delta` 明显接近 Codex 基线。

#### 预计工期

HTTP/Runtime 复用 3～5 个工作日；Responses WebSocket 完整能力另计 3～5 个工作日。

### 阶段 3：上下文准备前移、缓存与预测压缩

#### 目标

减少 Provider 请求发出前的重复计算，并让绝大多数长会话不在用户发送后同步等待辅助模型压缩。

#### 开发范围

- 缓存模型配置解析结果，按配置 revision 失效。
- 缓存工具定义，按工具、技能、MCP、浏览器能力和权限 revision 失效。
- 知识上下文按 workspace、query fingerprint 和知识 revision 复用。
- 会话历史维护增量 token 统计和历史 fingerprint。
- Turn 终态或 daemon 空闲时预测下一轮上下文压力。
- 接近 proactive threshold 时后台生成候选 context checkpoint。
- 新消息到达后只在历史 fingerprint、模型 identity 和 checkpoint generation 全部一致时安装候选结果。
- 达到硬限制仍允许同步压缩，但必须进入可见 `preparing/context_compaction` 阶段。
- 辅助模型与主模型的 Provider 并发策略需要避免互相饿死。

#### 退出条件

- 普通轮次不再完整重算未变化的工具面和历史 token。
- 长会话的大多数轮次可以直接复用有效 checkpoint。
- 过期的后台压缩结果不会被安装。
- 模型切换、权限变化、工具变化和知识 revision 变化会正确失效缓存。
- 同步压缩失败有明确终态，不会重复执行无界重试。

#### 预计工期

3～5 个工作日。

### 阶段 4：后端流式写回增量化

#### 目标

Provider delta 热路径只追加增量，避免反复复制完整正文、完整 sidecar 和完整 canonical turn。

#### 目标数据流

```text
Provider delta
  -> TurnStreamBuffer 追加
  -> canonical stream delta
  -> SSE
  -> 定时内存快照
  -> 阶段边界、工具边界和终态强制持久化
```

#### 开发范围

- 为正文、思考和工具参数分别建立 `TurnStreamBuffer`。
- 热路径只维护新增 delta、内容长度、itemVersion 和更新时间。
- canonical stream 协议继续使用 `itemId + version + baseLength + delta`。
- 完整 canonical item 只在首帧、reset、阶段边界和终态发布。
- sidecar 使用 dirty 标记与定时 flush；终态必须强制 flush。
- 崩溃恢复时用最后 durable snapshot 与事件游标恢复，不得损坏已有 Turn。
- 移除每个 delta 调用完整 `upsert_current_turn_item` 和 `upsert_canonical_turn_in_state` 的路径。

#### 退出条件

- 长文本输出时，Rust CPU、分配次数和全局状态锁持有时间明显下降。
- 中文、多字节字符、Markdown、代码块和图表内容不丢失、不重复、不乱码。
- SSE 断线后可以由权威快照补齐尾部内容。
- Provider 最终正文、canonical 终态正文和重启恢复正文完全一致。
- 思考、正文与工具参数不会因增量缓冲错误混合。

#### 预计工期

4～6 个工作日。

### 阶段 5：前端增量 reducer 与渲染热路径收敛

#### 目标

让一个 stream delta 只更新一个 item 和一个可见 artifact，不再触发整轮 projection 和全部消息签名重算。

#### 开发范围

- reducer 建立 `turnId -> index`、`itemId -> index` 索引。
- 使用协议长度和增量偏移，移除热路径对完整正文的多次 `Array.from()`。
- 使用字段版本和稳定 revision 代替 `JSON.stringify()` 等价比较。
- stream delta 直接更新当前 artifact；阶段或终态变化才重建 Turn presentation。
- projection 的 `updatedAt` 只在事实实际变化时更新。
- MessageList 不再遍历所有可见消息生成内容签名。
- 当前流式消息独立触发滚动与 ResizeObserver 协调。
- `session.turn.item` 不再无条件刷新 Goal；仅 Goal、Plan、工具状态和 Turn 终态事件触发权威刷新。
- 原始模式和摘要模式共享同一 reducer/projection，只保留呈现层差异。
- 主对话和代理对话面板复用同一增量组件行为。

#### 退出条件

- 72 条以上可见历史消息时持续输出无明显掉帧。
- 浏览器性能录制中，普通 delta 不产生超过 50ms 的主线程长任务。
- 原始模式与摘要模式最终内容、工具状态和时间信息一致。
- 摘要阶段自动展开/折叠、工具小折叠和代理面板行为无回归。
- 后端事件进入浏览器到 DOM 绘制 P95 不高于 50ms。

#### 预计工期

4～6 个工作日。

### 阶段 6：整链路回归、打包与最终验收

#### 功能测试矩阵

| 维度 | 场景 |
|---|---|
| 会话范围 | 个人、工作区 |
| 会话历史 | 新会话、短会话、长历史、压缩后会话 |
| 展示模式 | 原始模式、摘要模式 |
| 内容 | 纯文本、思考、工具调用、文件、图片、图表 |
| 执行者 | 主代理、子代理、多个子代理并发 |
| 状态 | 正常、取消、权限等待、Git 冲突、Provider 重试、空流 |
| 恢复 | SSE 断线、daemon 重启、桌面应用重启 |
| Git | clean、dirty、conflict、fast-forward、branch 变化 |
| 运行载体 | daemon 开发入口、Electron 打包产物 |

#### 验证命令

按实际改动范围至少执行：

```bash
cargo fmt --all -- --check
cargo check -p magi-daemon
cargo check -p magi-api
cargo check -p magi-conversation-runtime
cargo check -p magi-bridge-client
npm --prefix web run check
npm --prefix web run build
```

最终必须执行项目完整本地发布前置校验和当前操作系统 Electron 打包验证，并使用打包产物完成真实模型对话、工具调用、取消、权限、恢复和两种展示模式验收。

#### 退出条件

- 所有目标性能指标达到要求，或对未达标项给出可复现证据和明确阻断原因。
- 开发服务与打包产物行为一致。
- 没有旧同步提交、旧独立 Runtime、旧完整 stream upsert 等重复实现残留。
- 没有为性能优化牺牲权限、Git 安全、幂等、崩溃恢复和 canonical 一致性。
- 完整回归测试通过，并形成最终性能前后对比记录。

#### 预计工期

2～3 个工作日。

## 6. 阶段顺序与里程碑

```text
M0 真实基线可观测
  -> M1 accepted 与 preparation 解耦
  -> M2 HTTP Client / Runtime 复用
  -> M3 上下文准备前移
  -> M4 后端 stream 增量化
  -> M5 前端单 artifact 增量渲染
  -> M6 Electron 全流程验收
```

里程碑说明：

- M1 完成后，发送消息的产品体感应首先明显改善。
- M2、M3 决定实际首 token 是否接近 Codex。
- M4、M5 决定长回复、工具循环和长历史下的持续流畅度。
- M6 完成后才能判定本项目性能优化真正收口。

## 7. 工作量评估

不包含 Responses WebSocket 时，预计 17～27 个工作日；包含完整 WebSocket transport session 时，预计 20～32 个工作日。

预计有效变更规模：

- Rust：2,000～3,500 行；
- Web：800～1,500 行；
- 测试与基准：1,000～2,000 行。

该工作属于中大型架构优化，必须分阶段提交和验收，不应一次性跨越提交、Transport、状态存储和 UI 四层后再统一排错。

## 8. 风险与控制措施

### 8.1 accepted 过早导致状态不可靠

控制：HTTP 返回前必须完成最小 durable submission；后台 preparation 只能消费可靠记录。

### 8.2 Git 与权限安全被性能优化绕开

控制：Git、Snapshot、权限检查仍是执行前置条件，只从 HTTP 同步路径移入明确的 `preparing` 状态，不得跳过。

### 8.3 连接复用导致跨会话状态污染

控制：HTTP Client 可以跨请求复用；Provider turn state、sticky token 和增量 request state 必须限定在 turn-scoped session，不能跨 Turn 复用。

### 8.4 流式增量导致崩溃时尾部丢失

控制：定义明确的 flush 周期、终态强制 flush 和恢复协议；性能基准必须同时验证一致性和恢复能力。

### 8.5 前端优化造成两种展示模式事实分叉

控制：原始模式与摘要模式共享 canonical reducer 和 timeline projection，只允许视图组合不同。

### 8.6 性能指标被 Provider 波动污染

控制：单独记录 Provider TTFT；同时使用本地可控 SSE mock 和真实 Provider 两套基准。

## 9. 首批开发检查表

- [x] 完成阶段 0 timing schema 和统一 trace 设计。
- [x] 完成本地 mock Provider 的确定性延迟基准；`MagiTurnHarness` 以 ignored 基准固定五类场景各 20 轮，输出 accepted、首 delta、首 EventBus 事件和 Turn 终态的 P50/P95/最大值，并校验时序单调和事件序号存在。
- [x] 完成真实 Provider/daemon 五类场景各 20 轮的后端 P50/P95 基线；统一汇总见 9.4，直接证据为 `/tmp/magi-real-provider-perf-personal20.json`、`/tmp/magi-real-provider-perf-workspace20.json`、`/tmp/magi-real-provider-perf-tool20.json` 和 `/tmp/magi-real-provider-perf-subagent20.json`。
- [ ] 完成真实 Provider 五类场景各 20 轮的端到端 P50/P95 基线。
  - 该端到端项目仍需把 Electron 生产 Renderer 的 `frontend_event_received`、reducer/projection、`dom_painted` 与同一轮的 accepted、Provider 首 delta、首 EventBus 事件、terminal canonical event 关联起来；当前三类单轮 CDP timing 已有直接证据，五类场景各 20 轮尚未完成。
- [x] 明确 durable submission 的最小字段和恢复规则。
- [x] 明确 accepted、preparing、running、streaming 的 canonical 事件合同。
- [x] 完成阶段 1 代码改造与异步 preparation 回归测试。
- [x] 完成阶段 2 的 HTTP Client、Tokio Runtime 与连接池复用首轮实现。
- [x] 完成阶段 3 的模型、工具定义、知识上下文缓存与 checkpoint 绑定校验首轮实现。
- [x] 完成阶段 4 的 canonical stream item 增量写回首轮实现。
- [x] 完成阶段 5 的前端增量 reducer、projection 与 Goal 刷新收敛首轮实现。
- [x] 完成本地 daemon 真实入口的页面启动、会话历史、摘要折叠与工具组逐层展开验收。
- [x] 完成 Electron `--dir` 打包产物启动、静态资源加载与摘要折叠/工具组展开验收。
- [x] 通过 `scripts/verify-electron-conversation-dom.mjs` 完成打包 Electron 真实 Renderer DOM 单轮场景验收：初始窗口、个人/工作区 Chat、摘要 Turn/工具组折叠、ReadOnly 明确写工具阻断、daemon 重启恢复、历史会话切换、取消和 Renderer reload 历史恢复共 25 项基础 DOM 检查通过；同一脚本进一步对个人 Chat、工作区 Chat、Task 工具记录四阶段生产 Renderer timing，合计 37 项检查通过，直接证据为 `/tmp/magi-electron-dom-timing-9331.json`。该证据仍不替代五类 Electron 性能 20 轮采样。
- [ ] 汇总性能前后对比；当前已完成真实 Provider 五场景后端 20 轮基线，但不宣称性能目标达标。
  - 真实 Provider 性能脚本区分 accepted、首个 `session.turn.item`、terminal canonical event 与 daemon 后端阶段；Electron 生产 Renderer 已提供受限内存 timing registry（`window.__magiPerformanceTiming.snapshot()`），三类单轮场景已经通过打包 Electron CDP 读取，仍需扩展到五类场景 20 轮并补齐可审计的 before 数据。

### 9.1 本轮本地验收记录

已通过：

- Rust：`cargo fmt --all -- --check`；`cargo check -p magi-daemon -p magi-api -p magi-conversation-runtime -p magi-bridge-client`。
- Rust 测试：settings 17、knowledge 104、context 21、session 99、bridge 240、conversation 452、API 579 全部通过。
- Web：`npm run check`、`npm run build`、完整 `npm run test` 全部通过；canonical turn、transport、agent、bridge、turn navigation 等 golden replay 均通过。
- daemon 入口：`http://127.0.0.1:38123/web.html` 返回 200，`/health` 返回 `status=ok`。
- Electron：`npm run desktop:package -- --dir` 成功生成并启动
  `target/electron-dist/mac-arm64/Magi.app`；已在打包窗口验证摘要模式下的轮次折叠和工具组二级展开。

尚未宣称完成的验收：

- 真实 Provider 五类场景各 20 轮的 Provider/daemon P50/P95 已完成（见 9.4）；Electron 生产 Renderer 阶段 timing 与阶段 6 的性能前后对比尚未完成，当前仍不能宣称性能目标达标。

### 9.3 本地 mock Provider 五场景 20 轮基线

显式命令：

```bash
cargo test -p magi-api --lib turn_harness::tests::local_mock_provider_five_scenario_p50_p95_baseline -- --ignored --test-threads=1 --nocapture
```

本轮结果（单位：ms，格式为 accepted / 首 delta / 首 EventBus 事件 / Turn 终态）：

| 场景 | P50 | P95 | 最大值 |
|---|---:|---:|---:|
| 新建个人普通会话 | 0 / 1 / 13 / 13 | 0 / 3 / 14 / 14 | 4 / 7 / 17 / 17 |
| 已有个人长历史 | 1 / 2 / 14 / 15 | 2 / 4 / 16 / 17 | 2 / 4 / 17 / 18 |
| 工作区纯聊天 | 0 / 1 / 13 / 13 | 0 / 1 / 13 / 13 | 0 / 1 / 13 / 13 |
| 工作区工具调用 | 1 / 72 / 124 / 124 | 1 / 77 / 133 / 133 | 1 / 88 / 132 / 134 |
| 主代理与子代理并发 | 1 / 111 / 127 / 127 | 1 / 112 / 130 / 130 | 1 / 112 / 137 / 137 |

该基线只证明本地 mock 的采样链路和指标计算可复现，不代表真实 Provider、前端 DOM 绘制或性能前后对比已完成。

### 9.4 真实 Provider 五场景 20 轮基线

使用 `gpt-5.6-luna`、独立 daemon/state/workspace fixture 和 `scripts/verify-real-provider-performance.mjs` 完成五类场景各 20 轮；指标单位为 ms，顺序为 accepted / 首个 `session.turn.item` / Turn terminal canonical event。

| 场景 | P50 | P95 | 最大值 | 终态 |
|---|---:|---:|---:|---|
| 新建个人普通会话 | 88 / 14797 / 15169 | 98 / 27054 / 27488 | 108 / 28756 / 29163 | 20/20 completed |
| 已有个人长历史 | 92 / 15645 / 15794 | 110 / 26669 / 26782 | 115 / 35299 / 35463 | 20/20 completed |
| 工作区纯聊天 | 93 / 2981 / 3529 | 104 / 14214 / 14730 | 106 / 18623 / 19037 | 20/20 completed |
| 工作区工具调用 | 51 / 61 / 9144 | 60 / 94 / 34131 | 64 / 103 / 49879 | 20/20 completed |
| 主代理与子代理并发 | 97 / 130 / 53011 | 131 / 175 / 72882 | 143 / 220 / 139254 | 20/20 completed |

直接 JSON 证据：`/tmp/magi-real-provider-perf-personal20.json`、`/tmp/magi-real-provider-perf-workspace20.json`、`/tmp/magi-real-provider-perf-tool20.json`、`/tmp/magi-real-provider-perf-subagent20.json`。该基线完成 Provider/daemon 时序采样；Electron 生产 Renderer 的 timing registry 已落地，三类单轮 CDP 读取证据见 `/tmp/magi-electron-dom-timing-9331.json`，五类场景 20 轮 CDP 采样和性能前后对比仍未完成。

### 9.2 真实 Provider 单轮验收记录

本轮使用已保存的 Magi API 配置，在打包产物
`target/electron-dist/mac-arm64/Magi.app` 中通过桌面端真实入口执行，未使用无凭证的裸 `curl` 探测替代应用链路：

| 场景 | 结果 | UI 终态/耗时 |
|---|---|---:|
| 纯文本流式响应 `PERFSMOKEOK` | 通过，响应正文准确收口 | 已处理 10s |
| 完全访问纯文本 `TEXTSCENEOK` | 通过，无工具调用，正文准确收口 | 已处理 5s |
| 只读 Shell 工具 `pwd` | 通过，工具卡片可见，阶段折叠后可二级展开 | 已处理 9s |
| 完全访问只读 Shell `ls -la /tmp` | 通过，正文准确收口 | 已处理 14s |
| 受限访问只读 Shell `pwd` | 通过，正文准确收口 | 已处理 10s |
| 只读访问只读 Shell `pwd` | 通过，正文准确收口 | 已处理 13s |
| 摘要模式阶段折叠与工具二级展开 | 通过，展开阶段后显示 `运行 pwd`，继续展开显示 `Shell 命令 pwd` | — |
| 长流取消 | 通过，停止后进入“代理运行中心·任务已停止”，可归档 | — |
| 只读模式下写入请求 | 权限拒绝被明确展示，未创建项目文件；模型重复尝试导致任务未自行快速收口，已手动停止 | 1m42s |
| 只读访问明确写入拦截 | 通过，返回 `READONLYBLOCKED`，未创建临时文件 | 已处理 10s |
| 受限访问下写入请求 | 通过，明确显示“权限受限，已停止，未重试”，未创建临时文件 | 已处理 12s |
| 完全访问下写入 `/tmp` | 通过，工具完成并返回 `PERMISSIONWRITEOK` | 已处理 13s |
| 完全访问复现写入 `/tmp` | 通过，当前界面显示完全访问，Shell 写入成功并返回完整结果 | 已处理 16s |
| 同一会话旧只读历史后切换完全访问 | 通过，显式 `full_access` 下 Shell 写入成功并返回 `POSTFIXOK` | 已处理 30s 内 |
| 同一会话切回只读后写入 | 通过，立即拒绝且不重试，未创建临时文件 | 已处理 1s 内 |
| 只读模式下继续执行 `pwd` | 通过，Shell 读取成功并返回 `READONLYPOSTFIXREADOK` | 已处理 4s |
| 多阶段长任务 | 通过，23 次工具调用、连续写入/读取/校验/删除/计划收口，最终 27 个 turn item、无失败项，最终仅保留 `manifest.txt` 与 `report.txt` | 182s |

注意：对话历史中的权限结果属于提交该轮时的访问模式快照；底部访问模式按钮表示下一轮发送将使用的模式，不会改写历史轮次。因此，历史轮次显示“权限受限”而当前按钮显示“完全访问”并不矛盾。

该记录证明已保存的 Provider 配置可以驱动真实流式、工具、取消和访问模式路径；受限访问已经能够快速阻断并收口。旧回归记录中只读模式下模型重复尝试属于修复前的历史现象；本轮在新打包产物中确认权限快照会覆盖线程旧历史，写入被立即拒绝且不会重复调用，切换为完全访问后可正常写入。

## 10. 关键源码证据

Magi 当前主要证据位置：

- `crates/magi-api/src/routes/sessions.rs`：`submit_mainline_session_turn` 在返回前等待 dispatch finalize。
- `crates/magi-api/src/routes/dispatch_flow.rs`：接纳前执行 Snapshot、Git Context 和 checkpoint。
- `crates/magi-api/src/state.rs`：Session Git 观测、完整 durable state 和 Git Context 持久化。
- `crates/magi-bridge-client/src/http_model_client.rs`：每次流式请求创建线程、Runtime 和 `reqwest::Client`。
- `crates/magi-conversation-runtime/src/session_turn_execution.rs`：首请求前上下文准备，以及每个 delta 的完整 item upsert。
- `crates/magi-session-store/src/store/sidecar.rs`：运行 item 更新时重建 canonical turn。
- `web/src/stores/turn-reducer.ts`：完整字符串字符数组转换与对象等价比较。
- `web/src/stores/turn-projection.ts`：变更 Turn 的 presentation/artifact 重建。
- `web/src/shared/bridges/web-client-bridge.ts`：stream item 事件处理及 Goal 刷新。
- `web/src/components/MessageList.svelte`：可见消息内容签名和滚动派生计算。

Codex 对照位置：

- `/Users/xie/code/codex/codex-rs/core/src/session/turn_input.rs`：只等待 Core 路由决定。
- `/Users/xie/code/codex/codex-rs/app-server/src/request_processors/turn_processor.rs`：`turn/start` 在启动后立即返回。
- `/Users/xie/code/codex/codex-rs/core/src/client.rs`：turn-scoped `ModelClientSession` 与连接复用。
- `/Users/xie/code/codex/codex-rs/core/src/session_startup_prewarm.rs`：启动预热。
- `/Users/xie/code/codex/codex-rs/app-server/src/outgoing_message.rs`：响应和事件通过独立 outgoing 消息发送。
