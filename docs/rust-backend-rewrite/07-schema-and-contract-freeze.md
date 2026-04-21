# Rust 后端重构协议冻结与契约边界

更新时间：2026-04-17

> 本文档用于冻结本地影子 Rust 后端重构期间唯一有效的外部协议与对外读模型边界。
>
> 它不是完整 OpenAPI，也不是字段级接口文档；它负责定义“哪些外部结构必须稳定”“哪些边界不得由实现者临时发明”。

---

## 1. 文档目的

本文件用于回答 5 个问题：

1. Rust 后端当前必须对外稳定暴露哪些一级资源模型
2. SSE 事件的统一信封与事件分类如何冻结
3. Host Bridge 的能力边界如何定义
4. Tool Protocol 的稳定外形如何统一
5. Runtime Read Model 哪些部分属于稳定契约，哪些部分属于内部实现可变区

本文件与其他文档的分工如下：

- [02-capability-matrix.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/02-capability-matrix.md) 负责能力域覆盖
- [03-semantic-deviation-ledger.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/03-semantic-deviation-ledger.md) 负责语义偏差取舍
- [04-module-mapping-and-target-crates.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/04-module-mapping-and-target-crates.md) 负责 crate 归属
- 本文档只负责“外部结构边界”

---

## 2. 冻结原则

### 2.1 当前冻结的是结构与边界，不是最终字段全集

本轮冻结只要求明确：

- 一级资源
- 一级事件
- 一级命令
- 统一 envelope
- 稳定标识字段
- 允许内部演进的可变区

本轮不要求：

- 完整字段枚举
- OpenAPI 生成文件
- JSON Schema 成品
- 最终错误码全集

### 2.2 旧实现细节不是协议标准

当旧实现存在以下情况时，不得将其自动视为稳定契约：

- 明显错误行为
- 历史补丁产物
- 仅为当前 UI 表达方便而形成的临时结构
- 多个真相源混装后的偶然数据形状

协议基准优先级：

1. 目标领域模型
2. 已稳定的对外契约
3. 旧代码实现细节

### 2.3 协议变更必须先文档化

变更顺序固定为：

1. 先更新本文档
2. 再更新后续 `schema/` 产物
3. 再进入实现

禁止：

- 在 Agent 任务单中隐式扩协议
- 先改实现再补协议
- 用临时字段掩盖协议缺失

---

## 3. HTTP API 一级资源模型

当前冻结的一级资源模型如下。

### 3.1 `health`

职责：

- 健康检查
- 版本与进程状态探活
- 最小运行态确认

稳定契约要求：

- 必须能区分“进程存活”和“核心依赖可用”
- 必须暴露版本信息
- 不承载业务摘要

可变区：

- 具体子系统健康明细
- 内部依赖探测项

### 3.2 `bootstrap`

职责：

- UI / Host 初次接入时获取统一快照
- 返回 session、workspace、runtime、notification、knowledge 等稳定读模型摘要

稳定契约要求：

- 必须是只读快照
- 必须能表达“当前用户进入系统时需要看到的最小稳定状态”
- 不能混入仅对某一宿主或某一前端有效的私有表达

可变区：

- 快照内的子结构扩展字段
- 排序或聚合策略

### 3.3 `session`

职责：

- 会话创建、切换、重命名、删除
- timeline 与 notification 的入口级交互

稳定契约要求：

- `session_id` 是稳定标识
- session 生命周期操作语义必须稳定
- 不允许由 UI 自行推断 session 真相

可变区：

- session 读模型中的非核心派生字段

### 3.4 `workspace`

职责：

- workspace 注册、枚举、激活
- workspace 根目录与隔离运行容器查询

稳定契约要求：

- `workspace_id` 与 `root_path` 必须是稳定关联
- workspace 生命周期动作语义必须独立于宿主

可变区：

- workspace 统计摘要
- 容器级内部诊断字段

### 3.5 `task / mission / assignment / todo`

职责：

- 暴露任务主链的稳定外形
- 提供 mission、assignment、todo 的只读查询与必要命令入口

稳定契约要求：

- `mission_id`、`assignment_id`、`todo_id` 必须稳定
- 生命周期状态必须来自核心真相源，不得由 UI 拼装
- `task tree` 必须能表达层级关系和执行状态

可变区：

- 内部调度摘要
- 运行时统计与诊断扩展字段

### 3.6 `knowledge`

职责：

- 项目知识、ADR、FAQ、learning、索引摘要查询

稳定契约要求：

- knowledge 查询必须返回统一的结果 envelope
- 必须可区分“知识记录”“索引结果”“治理后输出”

可变区：

- 具体排序策略
- 召回诊断细节

### 3.7 `changes / approvals`

职责：

- 变更预览
- 审批状态
- 风险门禁相关的交互入口

稳定契约要求：

- 变更对象与审批对象必须可关联到 mission / assignment / tool execution
- 审批结果必须具备明确状态与原因

可变区：

- diff 细节表达
- 风险说明扩展字段

### 3.8 `settings / version`

职责：

- 运行配置摘要
- 版本握手
- 前后端兼容信息暴露

稳定契约要求：

- 必须能判断 UI / Host 是否可与后端对接
- 版本握手字段不可随意漂移

可变区：

- 非核心运行配置项

---

## 4. 稳定 DTO 族边界

当前允许实现者使用不同内部结构，但对外 DTO 族必须收口到以下几类：

1. `HealthDto`
2. `BootstrapDto`
3. `SessionDto`
4. `WorkspaceDto`
5. `TaskTreeDto`
6. `KnowledgeQueryResultDto`
7. `ChangePreviewDto`
8. `ApprovalDecisionDto`
9. `SettingsDto`
10. `VersionHandshakeDto`

每类 DTO 必须遵循：

- 稳定 `id`
- 稳定 `status`
- 稳定 `timestamp`
- 稳定关联字段
- 可扩展 `metadata`

当前不允许：

- 针对不同 UI 或宿主分别定义不同主 DTO
- 直接透传内部聚合对象
- 用 `unknown` / 任意对象作为长期外部结构

---

## 5. SSE 事件冻结

### 5.1 统一事件信封

所有 SSE 事件必须统一使用同一信封语义，稳定字段包括：

- `event_id`
- `event_type`
- `occurred_at`
- `sequence`
- `workspace_id`
- `session_id`
- `mission_id`
- `assignment_id`
- `todo_id`
- `payload`

说明：

- 不是每个事件都必须填满所有关联字段
- 但只要事件可关联到某个核心对象，就必须使用统一字段名

### 5.2 事件分类

当前冻结的一级事件分类如下：

1. `session.*`
2. `workspace.*`
3. `mission.*`
4. `assignment.*`
5. `todo.*`
6. `worker.*`
7. `tool.*`
8. `approval.*`
9. `knowledge.*`
10. `memory.*`
11. `context.*`
12. `audit.*`
13. `usage.*`
14. `system.*`

### 5.3 事件语义要求

- `domain event`、`audit event`、`usage event`、`ui projection event` 必须可区分
- 一个事件不能同时承担多个一级语义职责
- 前端通知不得反向定义审计主链

### 5.4 可变区

可在不破坏冻结边界的前提下演进：

- `payload` 内的非稳定诊断字段
- 某一事件类型的扩展细节

不得演进：

- 统一 envelope 的主键命名
- 关联字段命名
- 一级事件分类体系

---

## 6. Host Bridge 协议边界

当前宿主范围只覆盖：

- `VSCode`
- `IDEA`

Host Bridge 只允许暴露以下核心能力：

1. `open_file`
2. `reveal_diff`
3. `read_diagnostics`
4. `read_symbols`
5. `terminal_exec`
6. `workspace_roots`

### 6.1 统一规则

- Host Bridge 是宿主能力边界，不是业务状态机
- Host Bridge 不持有后端真相源
- Core Runtime 不得直接依赖任何 IDE SDK

### 6.2 各命令的稳定语义

`open_file`

- 输入必须包含文件路径
- 可选行列定位
- 仅表达“打开或定位”，不承载业务状态

`reveal_diff`

- 输入必须关联某个稳定变更对象或文件对比对象
- 仅负责展示，不负责审批决策

`read_diagnostics`

- 返回统一 diagnostics 集合
- 不允许夹带宿主私有 UI 结构

`read_symbols`

- 返回统一 symbol 查询结果
- 宿主私有层级结构必须在桥内消化

`terminal_exec`

- 只表达宿主 terminal 能力调用
- 不与后端 `tool runtime` 的 shell 执行真相混装

`workspace_roots`

- 统一返回宿主当前可见的 workspace roots
- 不直接替代后端 workspace registry 真相源

### 6.4 Bridge Transport Client

本地影子 Rust 后端允许使用统一的桥接传输客户端，但该层仍属于边界桥接实现，不进入 core runtime。

稳定契约要求：

- host / model / MCP 三类请求必须通过同一 transport abstraction 发出
- 默认可采用本地 JSON-RPC over stdio 或等价本地进程协议
- 传输错误、协议错误、远端业务错误必须分层可见；其中 JSON-RPC 标准协议错误码（parse / invalid request / method not found / invalid params / internal error）必须落入 protocol 层，业务错误码只能落入 remote business 层
- transport client 只负责 client 侧；可附带最小 loopback server 验证入口，但不承担真实服务端职责

可变区：

- 本地进程启动参数
- method 名称映射
- 传输协议的非核心扩展字段
- 重试/超时的实现细节

---

## 7. Tool Protocol 冻结

### 7.1 Tool Protocol 的统一外形

所有工具调用必须具有以下稳定外形语义：

- `tool_name`
- `tool_kind`
- `request_id`
- `input`
- `result_status`
- `result_payload`
- `approval_requirement`
- `risk_level`

### 7.2 工具分类

当前冻结的一级工具分类如下：

1. `builtin`
2. `mcp`
3. `skill_bound`
4. `host_bound`

说明：

- `host_bound` 只表示通过 Host Bridge 间接调用的能力
- 不代表宿主进入 core runtime

### 7.3 执行结果状态

稳定状态必须至少可区分：

- `succeeded`
- `failed`
- `rejected`
- `needs_approval`
- `cancelled`

### 7.4 审批与风险字段

审批与风险必须是稳定协议字段，不能藏在工具私有 payload 中。

至少要能表达：

- 是否需要审批
- 审批是否已完成
- 风险等级
- 拒绝原因或阻断原因

---

## 8. Runtime Read Model 冻结

当前冻结的 Runtime Read Model 不再是松散的“若干视图”，而是固定的五段式结构：

1. `meta`
2. `overview`
3. `details`
4. `operations`
5. `recovery`

### 8.1 一级结构冻结

必须稳定：

- 一级 section 名称与顺序：`meta / overview / details / operations / recovery`
- `contract_version`
- `contract_sections`
- `ordering_strategy`
- `section_ordering_rules`
- `validation`
- `freeze`
- `freeze_gate`
- `freeze_evidence`
- `freeze_report`
- `freeze_consistency`
- `freeze_closure`

### 8.2 稳定排序与冻结门槛

Runtime Read Model 当前必须满足以下冻结门槛：

- 集合输出使用确定性排序，避免顺序漂移形成伪协议变化
- `freeze_gate.required_validation_refs` 与 `freeze_gate.satisfied_validation_refs` 必须可解释
- `freeze_report` 与 `freeze_evidence` 必须能形成统一签名
- `freeze_consistency` 必须能检测 `validation / freeze_gate / freeze_report` 之间的不一致
- `freeze_closure` 必须明确表达“当前是否具备闭环冻结证据”

### 8.3 当前冻结证据链

当前已经具备的最小冻结证据链如下：

1. `magi-event-bus` 生成统一 `RuntimeReadModelInput`
2. `read_model/contract.rs` 统一生成 `validation / freeze / freeze_gate / freeze_evidence / freeze_report / freeze_consistency / freeze_closure`
3. `EventBus::runtime_ledger_summary()` 将 ledger 状态注入 `meta.ledger`
4. `magi-api` 的 bootstrap 组装层只做 sidecar merge 与 ledger 对齐，不改写 contract 主语义
5. `cargo test -p magi-event-bus`
6. `cargo test -p magi-api`
7. `cargo test --workspace`

当前可直接作为冻结证据入口的实现位置：

- `crates/magi-event-bus/src/read_model.rs`
- `crates/magi-event-bus/src/read_model/contract.rs`
- `crates/magi-event-bus/src/bus.rs`
- `crates/magi-api/src/dto/bootstrap.rs`

### 8.4 bootstrap 边界

当前 `bootstrap` 与 Runtime Read Model 的边界固定如下：

- runtime read model 的一级结构与冻结链路只由 `magi-event-bus` 维护
- `magi-api` 只允许做 sidecar export merge、ledger 对齐与 DTO 导出
- `bootstrap.audit_usage_ledger` 与 `runtime_read_model.meta.ledger` 必须保持单真相源一致
- `details.sessions` / `details.workspaces` 的 sidecar export 只能并入已有 details 视图，不得额外发明顶层 contract

---

## 9. 稳定契约与内部可变区

### 9.1 稳定契约

以下部分视为稳定契约：

- 一级资源名称
- `bridges/cutover-smoke` 这类独立 cutover 辅助资源的“一级资源名 + 不并入 bootstrap 的职责边界”
- 一级事件分类
- 统一 envelope 主键
- 统一关联字段命名
- 统一 ID 命名
- 稳定生命周期状态名
- Host Bridge 核心命令名
- Tool Protocol 主字段
- Runtime Read Model 的一级视图名称

### 9.2 内部可变区

以下部分允许在不破坏稳定契约的前提下演进：

- DTO 内的扩展 `metadata`
- 内部聚合与持久化结构
- read model 的非核心扩展字段
- payload 中的诊断信息
- 二级错误结构

---

## 10. 协议变更流程

任一 Agent 若需要改动外部结构，必须按以下流程执行：

1. 先确认是否属于稳定契约
2. 若属于稳定契约，先更新本文档
3. 再更新后续 schema 产物或协议文档
4. 再修改实现
5. 再更新验证矩阵

禁止：

- 仅在任务单里口头说明协议变化
- 直接通过实现默认值兼容两套协议
- 不登记变更直接新增字段

---

## 11. 当前结论

在本地影子 Rust 后端重构阶段：

- 本文档是协议与契约边界的唯一真相源
- 实现者不再自行发明 API / SSE / Host Bridge / Tool Protocol
- 若后续要引入字段级 schema 产物，应以本文档为上位约束，而不是反向覆盖本文档
- 当前已具备冻结复核资格的边界主要是：`health / version / bootstrap`、`/bridges/services / /bridges/preflight / /bridges/cutover-smoke` 的一级资源名与职责边界、统一 SSE envelope、Host/Model/MCP 的 `bridge.handshake / bridge.health / bridge.describe_services`、以及 Runtime Read Model 的五段式 contract
- 当前仍未具备最终冻结资格的边界主要是：`/bridges/cutover-smoke` 的深层 per-service payload、真实 provider stream/retry/normalizer 语义、真实 MCP server 生命周期、以及 `IDEA host` 的真实宿主行为；但其顶层 blocking summary（如 `overall_ok`）、顶层 `blocking_issues + reason_code`、顶层 `blocking_issue_counts_by_reason_code / blocking_issue_counts_by_server_kind`、service-level summary（如 `service_ok`）、provider 专属 `model_provider_*` 原因码、`BridgeProbeErrorDto.code` 以及 MCP 的 `mcp_default_route_gate.route_status / route_target / resolved_server / contract_ok` 已经可以作为 cutover gate 契约消费
