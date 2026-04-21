# Rust 后端重构完整任务列表与团队推进方案

更新时间：2026-04-17

> 本文档用于把现有 Rust 重构文档、crate 现状与验证矩阵收口成一份可持续执行的完整任务列表。
>
> 它不是替代 `02/03/04/05/09`，而是把这些文档转成“下一步具体要做什么、谁适合做、依赖什么、做到什么算过”的执行台账。

---

## 1. 当前阶段判断

基于当前文档与实际代码状态，当前阶段判断如下：

- 当前整体处于 `M5 收口中`，尚未进入 `M6 统一切换评估`
- Rust 影子 workspace 已具备完整 crate 版图，并已通过 `cargo check --workspace`
- Rust 影子 workspace 已通过 `cargo test --workspace`
- 多条关键主链已能在 Rust 侧闭环，但真实宿主壳、真实 provider、真实 MCP 外部生态与最终 TS 接线尚未完成

当前推进原则：

1. 继续保持“影子重构、运行隔离、最终统一切换”
2. 优先做会影响切换判断的收口项，而不是零散补功能
3. 优先消化“大文件/大对象/边界混装”这类会阻碍后续并行推进的问题
4. 所有任务必须能映射回能力矩阵、语义偏差台账和验证矩阵

---

## 2. 状态定义

- `已完成`：实现、测试与当前文档口径一致，当前轮次不再作为主任务推进
- `收口中`：已有实现，但还缺结构治理、验证补齐或更真实的运行链路
- `待推进`：已确认必须做，但当前还没有完整实现
- `待切换评估`：实现基本到位，但需要协议冻结、验证矩阵与接线评估才能前进
- `阻塞`：依赖上游任务或外部边界，不适合直接开工

---

## 3. 完整任务列表

## 3.1 P0：治理、文档与切换前基线

### T-001 文档真相源同步收口

- 状态：已完成
- 写域：`docs/rust-backend-rewrite/**`
- 目标：让 `02/04/05/09/25` 对当前代码状态保持一致
- 主要内容：
  - 对齐 crate 实际结构与模块映射
  - 对齐能力矩阵中的状态字段
  - 对齐验证矩阵中的“开发中 / 待验证”判断
  - 明确哪些项是“已有实现但缺验证”，哪些项是“仍未真正实现”
- 验收：
  - 文档之间没有互相冲突的阶段判断
  - 每个关键能力域都能映射到实际 crate

### T-002 Runtime Read Model 冻结证据补齐

- 状态：已完成
- 写域：`crates/magi-event-bus/**`、`crates/magi-api/**`、`docs/rust-backend-rewrite/07-schema-and-contract-freeze.md`
- 目标：把现有 `meta / overview / details / operations / recovery` 从“稳定实现”推进到“可用于切换评估的冻结证据”
- 主要内容：
  - 补齐 contract freeze 的回归验证覆盖
  - 补齐稳定排序、validation、freeze gate、freeze report 的证据链
  - 明确 bootstrap DTO 与 read model 的边界
- 验收：
  - 冻结链路输出自洽
  - 文档与实现一致

### T-003 M6 预检清单生成

- 状态：已完成
- 写域：`docs/rust-backend-rewrite/**`
- 目标：在真正进入切换评估前，先生成可执行的 M6 预检清单
- 主要内容：
  - 逐条对应 `09-validation-matrix-and-readiness-checklist.md`
  - 标注每项证据来源、阻塞项和责任 crate
- 验收：
  - 每项切换门槛都有对应证据入口或明确缺口

## 3.2 P1：基础入口与状态内核

### T-101 `magi-daemon` 内部模块化治理

- 状态：已完成
- 写域：`crates/magi-daemon/**`、必要时 `apps/daemon/**`
- 目标：把 daemon 从单大文件继续拆到接近 `bootstrap / config / lifecycle / maintenance / shutdown`
- 主要内容：
  - 稳定配置结构
  - 稳定 maintenance 运行策略边界
  - 稳定 ledger/sidecar 启动恢复和关闭收口路径
- 验收：
  - crate 测试通过
  - 无业务状态机混入 daemon 启动层

### T-102 `magi-api` 内部模块化治理

- 状态：已完成
- 写域：`crates/magi-api/**`
- 目标：把 API 从当前集中式实现继续拆到接近 `state / routes / sse / dto / errors`
- 主要内容：
  - 路由装配与 DTO 组装分离
  - SSE 出口与普通路由分离
  - 为后续统一错误映射留出稳定位置
- 验收：
  - crate 测试通过
  - API 不承载业务真相源

### T-103 `magi-api` 统一错误码与一级资源补齐

- 状态：已完成
- 写域：`crates/magi-api/**`
- 目标：补齐文档里尚未完成的统一错误模型与更多一级资源路由
- 主要内容：
  - 建立统一错误映射
  - 明确一级资源与 DTO 冻结边界
  - 为后续真实接线补稳定出口
- 依赖：T-102
- 验收：
  - 错误模型统一
  - 不把业务判断塞回 route handler

### T-104 `magi-session-store` 恢复语义补完

- 状态：已完成
- 写域：`crates/magi-session-store/**`
- 目标：把 session durable state、projection 与 execution sidecar 的恢复语义补齐
- 主要内容：
  - 继续收口 recovery/apply/export 语义
  - 补会话恢复后的查询与一致性验证
  - 巩固 sidecar flush metadata 的调度语义
- 验收：
  - session 相关测试能覆盖恢复与刷新路径
  - sidecar 与 projection 无双真相源

### T-105 `magi-workspace` 真实 worktree 分配/释放语义

- 状态：已完成
- 写域：`crates/magi-workspace/**`
- 目标：从“状态建模完整”推进到“资源分配语义更真实”
- 主要内容：
  - register / activate / allocate / release 的约束补齐
  - snapshot / recovery 与 worktree 生命周期进一步对齐
  - recovery 诊断与异常回收语义补齐
- 验收：
  - worktree 分配、释放、恢复状态可解释
  - recovery sidecar 与 durable state 协作稳定

### T-106 Session / Workspace 持久化长链验证

- 状态：已完成
- 写域：`crates/magi-daemon/**`、`crates/magi-session-store/**`、`crates/magi-workspace/**`
- 目标：把 sidecar 独立持久化、兼容读取、自动 flush 从单测级推进到更完整长链验证
- 主要内容：
  - 启动恢复 -> 运行期变更 -> 增量 flush -> 重启恢复 的闭环验证
  - 补更完整 fixture
- 依赖：T-104、T-105
- 验收：
  - 长链测试稳定
  - 不再依赖人工推断 sidecar 是否一致

## 3.3 P2：执行主链

### T-201 `magi-orchestrator` 内部模块拆分

- 状态：已完成
- 写域：`crates/magi-orchestrator/**`
- 目标：从超大 `lib.rs` 继续拆向 `plan / dispatch / governance_apply / control_plane / execution_runtime / summary`
- 主要内容：
  - 模块拆分但不改当前对外语义
  - 稳定 command/result 类型位置
  - 降低后续并行冲突面
- 验收：
  - crate 测试通过
  - 不再继续向单文件集中膨胀

### T-202 `magi-worker-runtime` 内部模块拆分

- 状态：已完成
- 写域：`crates/magi-worker-runtime/**`
- 目标：从超大 `lib.rs` 和 local process 执行器文件继续拆分
- 主要内容：
  - 拆 `loop / report / intent / governance / executor observation`
  - 拆 `local_process` 的 request/probe/lease/capability
- 验收：
  - crate 测试通过
  - worker loop、executor、reporting 边界更清晰

### T-203 真实外部执行器替换最小 local-process loopback

- 状态：已完成
- 写域：`crates/magi-worker-runtime/**`、必要时 `crates/magi-orchestrator/**`
- 目标：让 `LocalProcessWorkerExecutor` 从“候选协议”走向“首个更真实的外部执行器”
- 主要内容：
  - 补更真实的子进程生命周期
  - 收口 lease、binding、parallelism 与 process model
  - 明确 compare 路径与默认路径的角色
- 依赖：T-202
- 验收：
  - execute/review/verify/repair 均有稳定外部执行语义
  - 不再依赖 shadow fallback 掩盖能力缺失

### T-204 Builtin Tool 更广执行链集成

- 状态：已完成
- 写域：`crates/magi-tool-runtime/**`、必要时 `crates/magi-worker-runtime/**`
- 目标：把五类 builtin 真实执行器从“已存在”推进到“更广的主链集成验证”
- 主要内容：
  - 巩固 governance 前置与 usage 事件
  - 补更多上下文下的冲突写保护验证
  - 对齐 worker/orchestrator 侧观测
- 验收：
  - tool summary、usage、governance 三者一致

### T-205 Orchestrator / Worker / Tool 长链验证

- 状态：已完成
- 写域：`crates/magi-orchestrator/**`、`crates/magi-worker-runtime/**`、`crates/magi-tool-runtime/**`
- 目标：把 mission -> dispatch -> worker -> tool -> report -> overview 路径做成更完整的长链验证
- 主要内容：
  - 覆盖治理阻断、approval、resume、repair retry
  - 覆盖 local-process executor 关键失败面
- 依赖：T-201、T-202、T-204
- 验收：
  - 执行主链具备切换前可审计证据

## 3.4 P3：知识、记忆、上下文与技能

### T-301 `magi-knowledge-store` 真实 code index / 审计接线

- 状态：已完成
- 写域：`crates/magi-knowledge-store/**`
- 目标：从当前索引/查询骨架推进到更真实的 code index 能力和审计关联
- 验收：
  - 索引、查询、governed output 与审计链有更真实联动

### T-302 `magi-memory-store` 真实 extraction 主链接入

- 状态：已完成
- 写域：`crates/magi-memory-store/**`
- 目标：把 preference / extraction / compaction 从数据结构层推进到更真实的提取主链
- 验收：
  - extraction result 与 memory record 的关联闭环可验证

### T-303 Context / Knowledge / Memory 长链验证

- 状态：已完成
- 写域：`crates/magi-context-runtime/**`、必要时 `crates/magi-knowledge-store/**`、`crates/magi-memory-store/**`
- 目标：补齐 runtime source assembly 的长链验证
- 主要内容：
  - recent turns / governed knowledge / memory / shared context / file summary 协同验证
- 验收：
  - 结构化来源、预算、截断、输出稳定

### T-304 Skill Runtime 切换前验证补齐

- 状态：已完成
- 写域：`crates/magi-skill-runtime/**`、必要时 `crates/magi-worker-runtime/**`
- 目标：把当前”待验证”推进到”验证通过”
- 主要内容：
  - 补更完整的 builtin / bridge / denied 分流验证
  - 补 skill dispatch 进入 worker/orchestrator/event-bus 的证据链
- 验收：
  - `09` 中 skill 相关条目标记可从“待验证”推进

## 3.5 P4：桥接边界与真实外部接线前置

### T-401 `magi-bridge-client` 内部模块拆分

- 状态：已完成
- 写域：`crates/magi-bridge-client/**`
- 目标：把当前 bridge client 的超大实现拆向 `transport / protocol / host / model / mcp / catalog / errors`
- 验收：
  - crate 测试通过
  - host/model/mcp 三层边界更清晰

### T-402 VSCode real-prehost 继续前置化

- 状态：已完成
- 写域：`crates/magi-bridge-client/**`
- 目标：在不接真实 UI 的前提下继续巩固 VSCode prehost
- 主要内容：
  - 补更真实的 workspace/session 上下文
  - 补更稳定的 terminal policy / diagnostics / symbols 边界
- 验收：
  - prehost 语义更稳定
  - 仍保持 Core 零 IDE SDK 污染

### T-403 真实 provider 适配骨架

- 状态：已完成
- 写域：`crates/magi-bridge-client/**` 或未来独立桥接写域
- 目标：把 model loopback 推向真实 provider 适配前置层
- 主要内容：
  - 为 model bridge 增加不止一个 provider service descriptor
  - 为 `openai-compatible` provider 增加 env-configurable prehost skeleton
  - 允许 provider alias / health / reason / default model 进入 service catalog
- 验收：
  - model bridge 不再只有 shadow provider

### T-404 MCP manager 真实生命周期前置

- 状态：已完成
- 写域：`crates/magi-bridge-client/**`
- 目标：把 env-configurable registry 推向更真实的 manager 生命周期与 server 管理
- 验收：
  - server 注册、启停、health、默认路由不再只是静态前置目录

### T-405 IDEA host 真正起盘或明确延后决策

- 状态：已完成
- 写域：未来 `hosts/idea` 或相关 bridge 写域
- 目标：决定 IDEA 是进入本轮真实实现，还是明确延后到切换后阶段
- 阻塞：当前 `IDEA` 仍是 `boundary-placeholder`
- 验收：
  - 决策落文档
  - 若开工则要有独立任务包

## 3.6 P5：切换前验证与 TS 接线准备

### T-501 关键能力域状态重标

- 状态：已完成
- 写域：`docs/rust-backend-rewrite/02-capability-matrix.md`、`docs/rust-backend-rewrite/09-validation-matrix-and-readiness-checklist.md`
- 目标：把能力域从“开发中”推进到更准确的“已覆盖 / 待验证”
- 依赖：各主链验证完成
- 验收：
  - 状态变化有测试或验证证据支撑

### T-502 对外协议冻结复核

- 状态：已完成
- 写域：`docs/rust-backend-rewrite/07-schema-and-contract-freeze.md`、相关 crate
- 目标：复核 API / SSE / Host Bridge / Tool Protocol 是否已具备冻结资格
- 验收：
  - 协议稳定项与未稳定项边界清晰

### T-503 TS 链路接线准备清单

- 状态：已完成
- 写域：`docs/rust-backend-rewrite/**`
- 目标：在不提前接线的前提下，先把 TS 接线改造拆成清晰 checklist
- 主要内容：
  - API 替换点
  - SSE 替换点
  - Host bridge 替换点
  - bootstrap / runtime query 替换点
- 验收：
  - 不需要再由接线过程反向定义后端语义

### T-504 M6 统一切换评估包

- 状态：已完成
- 写域：`docs/rust-backend-rewrite/**`
- 目标：生成切换清单、风险清单、回归清单和执行窗口建议
- 依赖：T-501、T-502、T-503
- 验收：
  - 满足 `05` 和 `09` 的切换门槛

---

## 3.7 三批推进建议

### Batch 1（6 个 crate）

- `magi-daemon`
- `magi-api`
- `magi-event-bus`
- `magi-orchestrator`
- `magi-worker-runtime`
- `magi-bridge-client`

### Batch 2（6 个 crate）

- `magi-session-store`
- `magi-workspace`
- `magi-tool-runtime`
- `magi-skill-runtime`
- `magi-context-runtime`
- `magi-governance`

### Batch 3（3 个 crate）

- `magi-core`
- `magi-knowledge-store`
- `magi-memory-store`

> 说明：workspace 实际为 15 个 Rust crate，因此按“6 个一组”推进时采用 `6 / 6 / 3` 三批滚动方式。

---

## 3.6 三批并行编组

为匹配“每批最多 6 个完整 crate 负责人并行推进”的团队模式，15 个 crate 统一按以下三批滚动推进：

### Batch 1：热路径收口批

- `magi-daemon`
- `magi-api`
- `magi-event-bus`
- `magi-orchestrator`
- `magi-worker-runtime`
- `magi-bridge-client`

说明：

- 这一批覆盖 daemon 入口、API 出口、runtime read model、执行主链和桥接边界，是最接近切换判断面的 6 个 crate
- 当前已进入并行推进状态，目标是继续压缩大文件、稳定模块边界，并保持 crate 级测试持续通过

### Batch 2：状态与执行配套批

- `magi-session-store`
- `magi-workspace`
- `magi-governance`
- `magi-tool-runtime`
- `magi-skill-runtime`
- `magi-context-runtime`

说明：

- 这一批负责把持久化恢复、治理、tool/skill 执行和上下文组装继续收口
- 建议在 Batch 1 完成并重新跑通 workspace 总验收后接续推进

### Batch 3：基础与知识记忆批

- `magi-core`
- `magi-knowledge-store`
- `magi-memory-store`

说明：

- 这一批 crate 数量较少，但承担底层模型稳定和知识/记忆真实接线
- 适合作为前两批完成后的第三波定向收口与切换前补强

---

## 4. 团队模式建议分组

为减少冲突，建议按写域拆成以下并行小组：

### Lane A：入口与状态治理组

- 负责：`magi-daemon`、`magi-api`、`magi-session-store`、`magi-workspace`
- 对应任务：
  - T-101
  - T-102
  - T-103
  - T-104
  - T-105
  - T-106

### Lane B：执行内核组

- 负责：`magi-orchestrator`、`magi-worker-runtime`、`magi-tool-runtime`
- 对应任务：
  - T-201
  - T-202
  - T-203
  - T-204
  - T-205

### Lane C：长期能力域组

- 负责：`magi-knowledge-store`、`magi-memory-store`、`magi-context-runtime`、`magi-skill-runtime`
- 对应任务：
  - T-301
  - T-302
  - T-303
  - T-304

### Lane D：桥接边界组

- 负责：`magi-bridge-client` 与未来 host/model/mcp 外部桥接写域
- 对应任务：
  - T-401
  - T-402
  - T-403
  - T-404
  - T-405

### Lane E：切换评估组

- 负责：文档、验证矩阵、协议冻结、接线准备
- 对应任务：
  - T-001
  - T-002
  - T-003
  - T-501
  - T-502
  - T-503
  - T-504

---

## 5. 当前推荐推进顺序

下一轮推荐按以下顺序推进：

1. 先完成 T-101 / T-102 / T-201 / T-202 / T-401 这类“去单点膨胀”的结构治理任务
2. 再完成 T-103 / T-105 / T-203 / T-204 / T-304 这类“已有实现但还缺关键收口”的任务
3. 再推进 T-106 / T-205 / T-303 / T-402 / T-404 这类长链验证与更真实边界任务
4. 最后再推进 T-501 ~ T-504，形成 M6 评估包

原因：

- 先拆边界，团队并行才不会不断撞到同一大文件
- 先补真实外部边界前置，再谈切换资格更稳
- 在协议冻结和切换评估之前，先把“已有实现但缺验证”的能力域收口更划算

---

## 6. 本轮团队模式执行记录

本轮已按团队模式启动以下并行收口方向：

1. `magi-daemon` 内部模块化治理
2. `magi-api` 内部模块化治理
3. 总任务台账与团队推进方案落文档
4. `magi-orchestrator` 内部模块拆分（T-201）
5. `magi-worker-runtime` 内部模块拆分（T-202）
6. `magi-bridge-client` 内部模块拆分（T-401）

### 当前滚动批次：Batch 1（6 / 6 / 3 的第一批）

当前按完整 crate 负责制推进以下 6 个热路径 crate：

- `magi-daemon`
- `magi-api`
- `magi-event-bus`
- `magi-orchestrator`
- `magi-worker-runtime`
- `magi-bridge-client`

当前批次目标：

- 继续压缩单文件膨胀点
- 把执行主链、runtime read model 与桥接边界的内部模块边界继续拉清
- 保持 crate 级测试和最终 `cargo test --workspace` 持续可通过

### 下一滚动批次：Batch 2（6 / 6 / 3 的第二批）

当前按完整 crate 负责制启动以下 6 个状态与执行配套 crate：

- `magi-session-store`
- `magi-workspace`
- `magi-governance`
- `magi-tool-runtime`
- `magi-skill-runtime`
- `magi-context-runtime`

当前批次目标：

- 继续压缩 `store / registry / lib.rs` 级别的大文件膨胀点
- 把恢复、治理、tool/skill dispatch、context assembly 的内部模块边界继续拉清
- 保持各 crate 测试与下一轮 workspace 总验收持续可通过

### 当前滚动批次：Batch 3（6 / 6 / 3 的第三批）

当前按完整 crate 负责制启动以下 3 个基础与知识记忆 crate：

- `magi-core`
- `magi-knowledge-store`
- `magi-memory-store`

当前批次目标：

- 稳定 core 基础类型与导出边界
- 把 knowledge / memory 的单文件实现继续收口到更清晰的内部模块
- 为第三批完成后的全 workspace 验收与切换前补强做准备

本轮已完成的具体收口结果：

- `magi-daemon` 已从单入口大文件收口为薄入口加 `app / bootstrap / events / maintenance / persistence / types / tests` 子模块
- `magi-api` 已拆出 `state / routes / sse`，当前 `lib.rs` 仅保留装配与导出职责
- `magi-orchestrator` 已拆出 `control_plane` 与 `execution_runtime`，控制面与执行时语义不再继续堆积在单个 `lib.rs`
- `magi-worker-runtime` 已拆出 `loop_controller`，把 worker loop 运行控制与 summary 聚合从主文件中抽离
- `magi-bridge-client` 已拆出 `types / transport / clients / dispatch / tests`，桥接 transport、客户端装配与调度入口边界更清晰
- `magi-worker-runtime` 已继续把 `local_process_executor` 收口为 `types / runtime / loopback` 目录模块，为后续真实外部执行器替换留出更清晰边界
- `magi-api` 已把 DTO 目录化为 `bootstrap / exports / read_model`，并恢复 bootstrap 对 sidecar/runtime ledger 的整合语义与测试
- `magi-orchestrator` 已把内联测试从 `lib.rs` 抽离到独立 `tests.rs`，进一步压缩主入口体积并降低后续冲突面
- `magi-worker-runtime` 已把 skill dispatch / governance / snapshot / summary 查询层抽到独立 `runtime_queries` 模块，进一步压缩 `lib.rs` 主入口
- `magi-worker-runtime` 已继续把 deterministic executor、executor probe / observation、report/helper 相关类型与实现拆到 `executor / executor_observation / reporting` 内部模块，进一步缩小 `lib.rs` 的职责面
- `magi-daemon` 已继续把配置/错误类型与运行时装配从 `app.rs` 下沉到独立 `config / runtime` 模块，`app.rs` 进一步收口为 `Daemon` 与启动流程薄入口
- `magi-api` 已把 system DTO 与 session-action DTO 继续拆开，`health / version / bootstrap` 组装回收到 DTO / state 边界，route 进一步收口为 HTTP 入口层，并把零散错误字符串映射统一回 `errors` helper
- `magi-orchestrator` 已继续把恢复路径中的 planner / command 组装抽离到 `recovery_planner` 模块，`ResumeCommand` / `ResumeDispatchDecision` 的构建与 payload 拼装不再继续堆在主入口
- `magi-event-bus` 已把 `read_model` 尾部的 diagnostics / attention / work queue / recovery resume 聚合实现收口到独立私有模块，降低单文件膨胀速度
- `magi-governance` 已继续从单文件 `lib.rs` 收口到 `requests / decision / service / tests` 模块，crate 根变为薄导出入口，治理输入、决策类型与服务逻辑边界更清晰
- `magi-context-runtime` 已把上下文装配主链从单体 `lib.rs` 收口到 `source_assembly / budgeting / structured_output` 内部模块，`assemble` 与 runtime source assembly 入口进一步变薄
- `magi-workspace` 已把 `registry` 内的 `worktree allocation/release + ownership filtering` 与 `recovery sidecar + flush metadata` 分别下沉到 `registry/worktree` 与 `registry/recovery` 模块，主入口不再同时承载两类状态迁移细节
- `magi-session-store` 已把单体 `store.rs` 收口成目录模块，查询/导出下沉到 `store/queries`，recovery/apply/flush hook 下沉到 `store/sidecar`，测试独立到 `store/tests`
- `magi-skill-runtime` 已把 builtin/bridge/denied dispatch、route/binding 解析、dispatch observation 和 skill normalize/policy evaluate 分别下沉到 `dispatch / routing / observation / validation` 内部模块，`lib.rs` 继续变薄
- `magi-tool-runtime` 已把 builtin 执行栈与解析 helper 下沉到 `builtin` 模块，把 policy / requested access mode / write-protection 冲突检测下沉到 `policy` 模块，`lib.rs` 进一步收口为 registry 与事件记录入口
- `magi-knowledge-store` 已补 `CodeIndexIngestion / CodeIndexSource / KnowledgeAuditLink / KnowledgeGovernanceLink`，并通过 `ingest_code_index(...)` 把 code index sidecar 推入 store，使 query 与 governed output 可以直接暴露 `code_source / audit_link / governance_link`
- `magi-memory-store` 已补 `apply_extraction(...) / extraction_linkage(...) / verify_extraction_linkage(...)`，可让 extraction result 直接生成并校验关联 memory record 的闭环
- `magi-knowledge-store` 已把单文件 `lib.rs` 收口到 `normalization / indexer / query / governed_output / state / tests` 内部模块，根入口进一步变成公共类型与 `KnowledgeStore` 的薄委托
- `magi-memory-store` 已把单文件 `lib.rs` 收口到 `query / preferences / extraction / history / compaction / tests` 内部模块，memory query、preference、extraction normalization、history read-side 与 compaction 写路径职责已经分层
- `magi-bridge-client` 已把 model loopback 从单一 shadow provider 推进到 provider registry 形态，新增 env-configurable `openai-compatible` prehost skeleton，并让 service catalog 直接暴露 provider alias / health / reason / default model 等桥接前置信号
- `docs/rust-backend-rewrite` 已补齐 `M6` 预检清单、TS 接线准备清单、统一切换评估包与 `IDEA host` 延后决策，文档真相源不再只停留在“结构治理完成、但切换判断靠口头说明”
- `07-schema-and-contract-freeze.md` 已把 Runtime Read Model 冻结证据补到可直接指向 `magi-event-bus -> magi-api bootstrap` 的单一路径，协议冻结复核已有统一落点
- `02-capability-matrix.md` 与 `09-validation-matrix-and-readiness-checklist.md` 已把 `Builtin Tool Runtime`、`Context Runtime` 从“开发中”重标到“待验证”，当前状态判断更接近真实代码落地程度
- 本轮并行结果已重新通过 `cargo test --workspace` 总验收；当前仍存在的已知非阻断项主要是 `magi-bridge-client` 中 `mcp_loopback` 的 `dead_code` 警告
- `magi-core` 已继续收紧内部模块对根导出的反向耦合，统一路径值对象最小接口，并补上 core 导出边界与值对象一致性的回归测试

本轮验证结果：

- `cargo test -p magi-daemon` 通过
- `cargo test -p magi-api` 通过
- `cargo test -p magi-orchestrator` 通过
- `cargo test -p magi-worker-runtime` 通过
- `cargo test -p magi-bridge-client` 通过
- `cargo test --workspace` 通过
- `cargo test -p magi-governance` 通过
- `cargo test -p magi-context-runtime` 通过
- `cargo test -p magi-workspace` 通过
- `cargo test -p magi-session-store` 通过
- `cargo test -p magi-skill-runtime` 通过
- `cargo test -p magi-tool-runtime` 通过
- `cargo test -p magi-knowledge-store` 通过
- `cargo test -p magi-core` 通过

当前结论：

- `6 / 6 / 3` 三批 crate 级结构治理已经完成，这一轮“去单点膨胀、稳定模块边界、降低并行冲突面”的目标可以视为达成
- 这不等于整个 Rust 重构已经 `100%` 完成；当前完成的是“crate 内部结构收口”主任务，不是“真实外部生态接线 + 协议冻结 + M6 切换评估”全任务
- 当前这份完整任务台账中的条目已经全部完成
- 这不等于 Rust 后端已经可以统一切换；[28-m6-cutover-evaluation-package.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/28-m6-cutover-evaluation-package.md) 的评估结论仍然是“暂不放行切换”，因为真实 provider / MCP / TS 接线还没有进入最终验证

后续应持续把每轮团队推进记录补到本节，避免”做了很多事但没有统一作战图”。

---

### Round 2 执行记录

本轮按文档推荐顺序推进第二批任务（T-103 / T-105 / T-203 / T-204 / T-304），全部完成。

已完成任务：

- **T-103** `magi-api` 统一错误码与一级资源补齐：建立统一错误映射模型，明确一级资源与 DTO 冻结边界
- **T-105** `magi-workspace` 真实 worktree 分配/释放语义：register / activate / allocate / release 约束补齐，recovery 诊断与异常回收语义补齐
- **T-203** 真实外部执行器替换最小 local-process loopback：
  - 协议层：`types.rs` 补齐 `LocalProcessProtocolRequestKind::{Review, Verify, Repair}` 与对应 request/response 类型；`loopback.rs` 补齐子进程侧 review/verify/repair 请求处理
  - 运行时层：`runtime.rs` 新增 `review_request()` / `verify_request()` / `repair_request()` 私有方法，扩展 `ShadowWorkerExecutor` impl 实现完整 review/verify/repair 委派
  - trait 扩展：`lib.rs` 的 `ShadowWorkerExecutor` trait 新增 `review()` / `verify()` / `repair()` 默认方法
  - 集成测试：新增 5 个测试覆盖 review/verify/repair 各阶段及 prior_trace 链式传递，总计 23 个测试全部通过
  - 验收：execute/review/verify/repair 均有稳定外部执行语义，不再依赖 shadow fallback 掩盖能力缺失

已完成任务（续）：

- **T-204** Builtin Tool 更广执行链集成：
  - 新增 `governance_blocked_invocations_appear_in_summary_and_events` 测试：验证 Succeeded / NeedsApproval / Failed 三种执行结果在 summary 与 event_bus 中的一致性
  - 新增 `path_level_write_protection_detects_overlapping_paths` 测试：验证 `WriteProtectionClaim.conflicts_with()` 的 paths 维度冲突检测与 guard 释放后恢复
  - 新增 `summary_for_query_filters_by_context_fields` 测试：验证 `summary_for_query()` 按 worker_id / todo_id / session_id / workspace_id 正确过滤与聚合
  - 新增 `policy_rejection_reflected_in_summary_and_events` 测试：验证 `ToolExecutionPolicy` denied/not-allowed 拒绝在 summary blocked_invocations 计数和 audit/usage 事件中的一致性
  - 新增 `full_chain_invocations_events_summary_consistent` 测试：5 种执行结果（成功×2、governance 阻断、执行失败、策略拒绝）的全链一致性——invocations 列表、summary 聚合、audit 事件、usage 事件四个数据源互相校验
  - 验收：tool summary、usage、governance 三者一致——每个 invocation record 都有对应 audit/usage 事件，status 字段完全匹配，summary 聚合与逐条记录一致
  - 测试结果：`cargo test -p magi-tool-runtime` 14 个测试全部通过（原 8 + 新增 6）

- **T-304** Skill Runtime 切换前验证补齐：
  - 新增 `unknown_requested_tool_yields_rejected_observation` 测试：验证请求不在 plan.routing 中的工具名时返回 Rejected + UnknownRequestedTool
  - 新增 `ambiguous_bridge_binding_yields_rejected_observation` 测试：验证两个 binding 共享同一 tool_name 时返回 Rejected + AmbiguousBridgeBinding
  - 新增 `missing_bridge_binding_id_yields_rejected_observation` 测试：验证指定不存在的 binding_id 时返回 Rejected + MissingBridgeBinding
  - 新增 `builtin_dispatch_emits_events_to_event_bus` 测试：验证 skill dispatch 经 tool_registry 的 audit+usage 事件流完整性，包括上下文字段（session_id / workspace_id / worker_id / todo_id）正确传递
  - 新增 `mixed_builtin_and_bridge_plan_dispatches_correctly` 测试：验证单一 plan 中 builtin 路由、bridge 路由和 unknown 拒绝三条路径同时正确工作
  - 新增 `skill_dispatch_observation_fields_are_fully_populated` 测试：验证 builtin 和 bridge 两种路由下 SkillDispatchObservation 的所有字段（route / binding_id / bridge_kind / status / error_kind / duration）完整填充
  - 验收：builtin / bridge / denied 三条分流路径均有完整测试覆盖；skill dispatch 经 tool_registry 进入 event_bus 的证据链可验证；observation 字段完整性可审计
  - 测试结果：`cargo test -p magi-skill-runtime` 11 个测试全部通过（原 5 + 新增 6）

Round 2 全部任务已完成（T-103 / T-105 / T-203 / T-204 / T-304）。

---

### Round 3 执行记录

本轮按文档推荐顺序推进第三批任务（T-104 / T-106 / T-205 / T-303 / T-402 / T-404），全部完成。

已完成任务：

- **T-104** `magi-session-store` 恢复语义补完：
  - 补齐 recovery/apply/export 语义：新增 `SessionRecoveryDiagnostic` 结构体，包含 `has_uncommitted_events` / `last_flush_lag` / `projection_drift` / `sidecar_consistent` 四项诊断指标
  - 新增 `SessionRecoverySidecar` 实现，包含 recovery checkpoint、apply pending events、export snapshot 三条恢复路径
  - 补齐会话恢复后的查询与一致性验证：新增 `recovery_apply_then_query_reflects_applied_state` 测试验证恢复后查询状态一致性
  - 新增 `recovery_diagnostics_detect_uncommitted_and_drift` 测试验证诊断指标在有未提交事件和 projection 偏移时的准确性
  - 新增 `sidecar_flush_metadata_scheduling` 测试验证 sidecar flush metadata 的调度语义
  - 新增 `export_snapshot_captures_full_durable_state` 测试验证 export snapshot 包含完整持久化状态
  - 验收：session 相关测试覆盖恢复与刷新路径；sidecar 与 projection 无双真相源
  - 测试结果：`cargo test -p magi-session-store` 13 个测试全部通过

- **T-106** Session / Workspace 持久化长链验证：
  - 新增 `session_full_lifecycle_boot_mutate_flush_recover` 长链测试：覆盖 启动恢复 → 运行期变更 → 增量 flush → 重启恢复 完整闭环
  - 新增 `workspace_full_lifecycle_register_allocate_snapshot_recover` 长链测试：覆盖 register → activate → allocate → snapshot → recover → release 全生命周期
  - 新增 `session_workspace_cross_recovery_consistency` 长链测试：验证 session 与 workspace 在交叉恢复场景下的一致性
  - 补齐更完整 fixture：包含多轮变更、多次 flush、跨恢复点的状态校验
  - 验收：长链测试稳定；不再依赖人工推断 sidecar 是否一致
  - 测试结果：session-store 13 tests + workspace 12 tests 全部通过

- **T-205** Orchestrator / Worker / Tool 长链验证：
  - 新增 `mission_dispatch_worker_tool_report_overview_long_chain` 长链测试：覆盖 mission → dispatch → worker → tool → report → overview 完整主链
  - 新增 `governance_block_approval_resume_long_chain` 长链测试：覆盖治理阻断 → approval → resume 路径
  - 新增 `repair_retry_after_execution_failure_long_chain` 长链测试：覆盖执行失败 → repair → retry 路径
  - 新增 `local_process_executor_stage_rejection_long_chain` 长链测试：覆盖 local-process executor 阶段禁用时的拒绝面
  - 修复既有 `worker_loop_rejects_review_when_executor_stage_is_disabled` 测试：添加 `.with_env("MAGI_LOCAL_WORKER_STAGE_REVIEW", "false")` 显式禁用 review 阶段（原测试使用 `cargo_loopback()` 默认全部阶段启用导致断言失败）
  - 验收：执行主链具备切换前可审计证据；治理阻断/approval/resume/repair/retry 路径均有覆盖
  - 测试结果：`cargo test -p magi-orchestrator` 14 tests + `cargo test -p magi-worker-runtime` 28 tests + `cargo test -p magi-tool-runtime` 14 tests 全部通过

- **T-303** Context / Knowledge / Memory 长链验证：
  - 新增 `recent_turns_governed_knowledge_memory_assembly` 长链测试：验证 recent turns / governed knowledge / memory 三个来源的协同组装
  - 新增 `budget_truncation_and_priority_ordering` 长链测试：验证结构化来源的预算分配、截断策略和优先级排序
  - 新增 `shared_context_and_file_summary_integration` 长链测试：验证 shared context 与 file summary 在 assembly 中的正确集成
  - 新增 `source_assembly_output_stability` 长链测试：验证相同输入下输出的确定性稳定
  - 验收：结构化来源、预算、截断、输出稳定；recent turns / governed knowledge / memory / shared context / file summary 协同验证通过
  - 测试结果：`cargo test -p magi-context-runtime` 8 个测试全部通过

- **T-402** VSCode real-prehost 继续前置化：
  - 补齐更真实的 workspace/session 上下文：新增 `VscodeWorkspaceContext` 包含 workspace_folders / active_editor / visible_editors / workspace_state
  - 补齐更稳定的 terminal policy / diagnostics / symbols 边界：新增 `VscodeTerminalPolicy` / `VscodeDiagnosticsSnapshot` / `VscodeSymbolIndex` 类型
  - 新增 `prehost_workspace_context_propagation` 测试验证 workspace 上下文在 prehost 中的完整传递
  - 新增 `prehost_terminal_policy_enforcement` 测试验证 terminal policy 的约束执行
  - 新增 `prehost_diagnostics_snapshot_consistency` 测试验证 diagnostics snapshot 的一致性
  - 验收：prehost 语义更稳定；仍保持 Core 零 IDE SDK 污染
  - 测试结果：`cargo test -p magi-bridge-client` 76 个测试全部通过（unit 40 + host 16 + mcp 14 + model 6）

- **T-404** MCP manager 真实生命周期前置：
  - 新增 `McpLifecycleEvent` 与 `McpLifecycleEventKind` 枚举：Registered / Started / Stopped / HealthChanged / Deregistered 五种生命周期事件
  - 新增 `McpServerLifecycleState` 状态机：Registered → Starting → Running → Stopping → Stopped → Failed → Deregistered 七个状态
  - 扩展 `McpServerDescriptor` 添加 `lifecycle_state` 字段
  - 扩展 `McpServerRegistry` 添加 `lifecycle_events` 事件记录与生命周期管理方法：`register_server` / `start_server` / `stop_server` / `deregister_server` / `update_server_health`
  - 新增 10 个生命周期测试覆盖状态转换、事件记录、非法转换拒绝、health 更新、deregister 语义
  - 验收：server 注册、启停、health、默认路由不再只是静态前置目录
  - 测试结果：`cargo test -p magi-bridge-client` MCP 子模块 14 个测试全部通过（原 4 + 新增 10 lifecycle）

Round 3 验证结果：

- `cargo test -p magi-session-store` 13 tests 通过
- `cargo test -p magi-workspace` 12 tests 通过
- `cargo test -p magi-orchestrator` 14 tests 通过
- `cargo test -p magi-worker-runtime` 28 tests 通过
- `cargo test -p magi-tool-runtime` 14 tests 通过
- `cargo test -p magi-context-runtime` 8 tests 通过
- `cargo test -p magi-bridge-client` 76 tests 通过
- `cargo test --workspace` 全量通过，零失败

Round 3 全部任务已完成（T-104 / T-106 / T-205 / T-303 / T-402 / T-404）。

Round 4 增量推进记录：

- `magi-governance` 已补 `GovernanceAction`、`GovernanceTarget` 与 `GovernanceDecisionTrace`，使 tool / sandbox / path / worker control 四类治理请求都能导出统一可序列化的决策轨迹，显式表达 `action / outcome / summary`
- `GovernanceService` 已新增 `trace_tool_request` / `trace_sandbox` / `trace_path_access` / `trace_worker_control_request` 四类出口，避免上游 API / 审计 / 展示层重复拼接治理语义
- 新增治理轨迹测试，覆盖 tool manual approval、worker repair retry reject、path blocked 三类轨迹语义
- `magi-api` 已把 runtime read model 与 ledger 收口为独立只读路由 `/runtime/read-model` 与 `/ledger`，并继续复用 bootstrap 同一套组装路径，避免 API 层出现第二套导出真相
- `magi-api` 已把 runtime read model sidecar merge 与 ledger 对齐逻辑压到 `dto/read_model.rs`，`bootstrap` 仅消费统一 helper，不再内联装配细节
- `magi-api` 已新增 `/bridges/services` 只读桥接快照出口，可稳定导出 model / host / MCP 的 `handshake / health / service catalog`，并支持通过 probe transport 或 snapshot provider 注入；默认 `ApiState::new(...)` 不受影响
- `magi-bridge-client` 已把 `openai-compatible` 从 provider skeleton 推进到最小 HTTP smoke path：可构建真实 `POST {base_url}/chat/completions` 请求、解析 `choices[0].message.content / text`，并显式映射 `-32003 ~ -32007` provider 错误边界
- `magi-bridge-client` 已补真实 subprocess + 本地 HTTP stub 的 model bridge smoke 测试，同时清理 `mcp_loopback` 生命周期辅助结构的 `dead_code` 噪音
- `magi-bridge-client` 已把本地 bridge 协议从单业务方法扩成多业务方法调度，并保留原有单方法兼容入口；MCP manager 现在已能真实处理 `mcp.list_servers / mcp.describe_server / mcp.enable_server / mcp.disable_server / mcp.register_server / mcp.start_server / mcp.stop_server / mcp.deregister_server / mcp.update_health`
- `magi-bridge-client` 已新增 transport 级 MCP lifecycle / registry 集成测试，直接验证 list/describe、enable/disable、start/stop/update_health/register 真实 JSON-RPC 调用链

### 2026-04-17 第 5 轮：knowledge / memory 系统级消费链收口

- 负责：`magi-orchestrator`、`magi-event-bus`、`docs`
- 目标：把 knowledge / memory 从 `context-runtime` 级消费继续推进到系统级 `mission.execution.overview -> runtime read model`
- 实际产出：
  - `magi-orchestrator` 已新增 `MissionContextSummary`，可直接从 `ContextAssemblyResult` 收口 `used_knowledge / used_memory / truncation_parts / knowledge_source_paths / memory_extraction_refs`
  - mission execution overview 事件现在会继续发布 `context` 摘要，不再只表达 worker / tool / governance / skill dispatch
  - `magi-event-bus` 的 runtime read model 已吸收 mission 级 context summary，并在 `details.missions` / `overview.diagnostics` 下稳定导出这些摘要
  - 已新增 `magi-orchestrator` 系统级长链测试，直接验证 `ContextRuntime -> Orchestrator -> EventBus` 的消费链
  - 已新增 `magi-event-bus` 读模型测试，直接验证 `mission.execution.overview.context` 的解析、排序与 diagnostics 聚合
- 验证：
  - `cargo test -p magi-orchestrator` 16 tests 通过
  - `cargo test -p magi-event-bus` 12 tests 通过
- 测试结果：`cargo test -p magi-governance` 4 个测试全部通过
- 测试结果：`cargo test -p magi-api` 24 个测试全部通过
- 测试结果：`cargo test -p magi-bridge-client` 86 个测试全部通过（lib 45 + host 16 + mcp 17 + model 8）

### 2026-04-17 第 6 轮：execution runtime 默认入口接入 context assembly

- 负责：`magi-orchestrator`、`magi-context-runtime`、`docs`
- 目标：把 orchestrator 默认 dispatch 执行入口在配置了 `with_context_runtime(...)` 时，自动接到 `context-runtime`，并把 `MissionContextSummary` 带入系统级 mission / runtime 摘要
- 实际产出：
  - `magi-orchestrator` 默认 dispatch 执行入口在显式配置 `with_context_runtime(...)` 后，已能自动调用 `ContextRuntime::assemble_execution_context(...)`
  - 自动装配出的 `MissionContextSummary` 已会进入 `mission.execution.overview`
  - `magi-event-bus` 的 runtime read model 已能稳定吸收这条默认入口发布出来的 mission context summary
  - 这轮新增证据解决的是“默认入口可按配置自动接入 context assembly”，不是“所有执行入口无条件启用 context assembly”
  - `magi-memory-store::apply_extraction(...)` 仍未自动接入默认 dispatch 执行入口；memory extraction 的默认回写仍需后续继续收口
  - 更广真实 bridge / provider 接线本轮没有新增落地，仍保留为统一切换前的硬阻塞
- 验证：
  - `cargo test -p magi-context-runtime` 通过
  - `cargo test -p magi-orchestrator` 通过
  - `cargo test -p magi-event-bus` 通过
  - `cargo test --workspace` 通过
- 当前结论：
  - `knowledge` 侧证据已从 `context-runtime -> orchestrator -> mission.execution.overview -> runtime read model`，进一步推进到“配置了 `with_context_runtime(...)` 的默认 dispatch 执行入口”级别
  - `memory` 侧仍不能宣称默认入口已经统一完成，因为 extraction 自动 `apply_extraction(...)` 尚未接入

### 2026-04-17 第 7 轮：daemon -> api shadow dispatch 真正接线

- 负责：`magi-api`、`magi-daemon`、`magi-event-bus`
- 目标：把之前只存在于测试构造里的 orchestrator execution runtime，真正接到后端默认 shadow 路径里，让 `/session/action` 能驱动一次完整 dispatch
- 实际产出：
  - `magi-api` 已新增 `ShadowExecutionPipeline`，`ApiState` 现在可显式携带 `OrchestratorService + OrchestratedExecutionRuntime`
  - `/session/action` 不再只写 timeline 和发 accepted 事件；当 API state 配置了 shadow pipeline 后，会真实创建 mission / assignment / todo、绑定 session ownership，并调用 orchestrator dispatch 执行
  - `magi-daemon` 的 router 装配现在会真实构造 `orchestrator + tool registry + skill runtime + worker runtime + context runtime`，并把这条 shadow execution pipeline 注入 `ApiState`
  - `magi-event-bus` 已修复“没有活跃订阅者时 publish 返回 channel closed”的稳定性问题；事件保留与账本刷新现在不会因为 SSE 无订阅而失败
  - `magi-api` 已新增路由级回归测试，直接验证 `/session/action -> shadow dispatch -> mission.execution.overview -> runtime read model`
  - `magi-event-bus` 已新增“无 live subscriber 也可 publish”测试，锁定这次稳定性修复
- 验证：
  - `cargo test -p magi-event-bus` 通过
  - `cargo test -p magi-api` 通过
  - `cargo test -p magi-daemon` 通过
  - `cargo test --workspace` 通过
- 当前结论：
  - 之前“默认 dispatch 执行入口只有测试构造会调用”的缺口已经补上，shadow 后端默认 API 路径现在能真实驱动 orchestrator dispatch
  - `knowledge / memory` 的系统级消费证据已从“默认 dispatch 执行入口按配置可导出 context summary”，进一步推进到“`/session/action` shadow 路由可驱动 dispatch 并更新 runtime read model”级别
  - `memory` 默认入口里的 `apply_extraction(...)` 自动回写仍未完成；真实 bridge / provider / MCP / TS 接线仍然是统一切换前的硬阻塞

### 2026-04-17 第 8 轮：`/session/action` 自动 extraction 回写

- 负责：`magi-api`、`magi-daemon`
- 目标：把 `magi-memory-store::apply_extraction(...)` 真正接到默认 shadow API 入口里，让同次 dispatch 的 context assembly 能直接读到 route 级 memory 写回
- 实际产出：
  - `ApiState` 的 `ShadowExecutionPipeline` 已显式携带共享的 `MemoryStore`
  - `/session/action` 在 shadow dispatch 成功之后，现在会自动调用 `apply_extraction(...)`
  - route 写入的 extraction 与 `ContextRuntime` 使用的是同一份 `MemoryStore`，因此后续同 session 的 dispatch 已能继续吸收到这条 route 级记忆
  - `magi-api` 路由级回归测试已补强成“两跳闭环”：第一跳验证 route 触发的 extraction linkage 一致性，第二跳验证上一跳写回的 extraction 能进入新的 runtime read model
- 验证：
  - `cargo test -p magi-api` 通过
  - `cargo test -p magi-daemon` 通过
  - `cargo test --workspace` 通过
- 当前结论：
  - `/session/action` 这条默认 shadow 入口已经自动完成 `apply_extraction(...)` 回写，而且是“dispatch 成功后再写回”的更稳路径
  - `memory` 侧系统证据现在已经从“context summary 可见”推进到“route 级回写 -> 下一次 dispatch 可消费”
  - 仍未完成的是“把 extraction 自动回写抽成对所有 execution runtime 调用方统一生效”，以及更广真实 bridge / provider / MCP / TS 接线

### 2026-04-17 第 9 轮：openai-compatible 结构化成功响应

- 负责：`magi-bridge-client`
- 目标：把 `openai-compatible` 从“最小文本 smoke path”继续推进到能保留更多上游成功响应语义
- 实际产出：
  - `openai-compatible` 成功响应现在会解析并保留 `usage`、`finish_reason`、`message.tool_calls`
  - 若上游只有纯文本且没有额外字段，`BridgeResponse.payload` 继续保持原来的纯文本兼容行为
  - 若上游返回了 `finish_reason / usage / tool_calls`，`BridgeResponse.payload` 现在会返回结构化 JSON 字符串，稳定收口 `content / finish_reason / usage / tool_calls`
  - 纯 `tool_calls`、没有 `content` 的成功响应现在已被接受，不再误判为 invalid success payload
  - 已补 lib 单测与 loopback 集成测试，覆盖结构化成功 payload 与 tool-call-only 路径
- 验证：
  - `cargo test -p magi-bridge-client model_loopback -- --nocapture` 通过
  - `cargo test -p magi-bridge-client --test model_loopback -- --nocapture` 通过
  - `cargo test --workspace` 通过
- 当前结论：
  - `openai-compatible` 仍然是 smoke path，不是完整 provider 适配层
  - 但它已经不再只会“吐纯文本”，而是开始保留切换前更关键的结构化成功语义

### 2026-04-17 第 10 轮：shadow helper 收口与 bridge contract 补强

- 负责：`magi-api`、`magi-bridge-client`、`docs`
- 目标：把 `/session/action` 的 shadow dispatch / extraction 逻辑收成单一 helper，并继续补强 bridge 侧 typed contract 与 provider 成功语义覆盖
- 实际产出：
  - `magi-api` 已新增 `shadow_execution.rs`，`/session/action` 路由不再内联维护 mission / assignment / todo 创建、dispatch 执行与 extraction 回写的整段流程
  - `magi-api` 当前这条默认 shadow 入口的副作用顺序已经收口成单一实现：先执行 dispatch，成功后再写回 extraction
  - `magi-bridge-client` 已补 `JsonRpcMcpManagerClient` 与 typed MCP manager contract，`mcp.list_servers / describe_server / enable_server / disable_server / register_server / start_server / stop_server / deregister_server / update_health` 现在都能走 typed JSON-RPC round-trip
  - `openai-compatible` 额外补了两条 loopback 集成测试，锁定“纯 tool-call success payload”与“structured content parts 扁平化为纯文本 payload”的成功语义
- 验证：
  - `cargo test -p magi-api` 通过
  - `cargo test -p magi-daemon` 通过
  - `cargo test -p magi-bridge-client` 通过
  - `cargo test --workspace` 通过
- 当前结论：
  - `magi-api` 这条默认 shadow 入口已经从“可用”进一步收口到“有单一 helper 真相源”，后续要把 extraction 自动回写继续扩到更多调用方时，不必再回头清理重复路由逻辑
  - `magi-bridge-client` 现在不只具备 shadow MCP manager 生命周期方法，还开始有更稳定的 typed contract 与 provider 成功语义测试护栏

### 2026-04-17 第 11 轮：pipeline 失败回归与 tool arguments 宽容解析

- 负责：`magi-api`、`magi-bridge-client`、`docs`
- 目标：继续收紧默认 shadow 主线的失败边界，并补强 `openai-compatible` 对 provider 方言的兼容能力
- 实际产出：
  - `magi-api` 已把 “dispatch 成功后再写回” 这段语义明确挂到 `ShadowExecutionPipeline` 层，`shadow_execution.rs` 现在通过单一 `execute_dispatch_then_writeback` helper 承载 post-dispatch side effects
  - `/session/action` 的 success path 现在会在 dispatch 完成后再绑定 mission / todo / worker ownership，因此失败路径不再留下 mission ownership 与 extraction 污染
  - `magi-api` 已新增不健康执行器回归测试，验证 dispatch 失败时不会写入 extraction，也不会把 session ownership 绑定到失败的 mission / todo / worker
  - `magi-bridge-client` 已补 `openai-compatible` 对 `tool_calls.function.arguments` 的宽容解析：当上游返回 object / array / 其他 JSON 值时，现在会统一序列化成字符串化 JSON，而不再误判为 invalid provider payload
  - `magi-bridge-client` 已补 unit test 与 loopback 集成测试，锁定结构化 `tool_call.arguments` 的 round-trip 行为
- 验证：
  - `cargo test -p magi-api` 通过
  - `cargo test -p magi-daemon` 通过
  - `cargo test -p magi-bridge-client` 通过
  - `cargo test --workspace` 通过
- 当前结论：
  - 默认 shadow API 入口现在不只“副作用顺序正确”，而且失败时也不会留下新的 route 级 memory / ownership 污染
  - `openai-compatible` 现在又多覆盖了一类常见 provider 方言差异，bridge 侧切换前护栏继续变厚

### 2026-04-17 第 12 轮：dispatch success hook 下沉到 execution runtime

- 负责：`magi-orchestrator`、`magi-api`、`docs`
- 目标：把“dispatch 成功后执行统一副作用”的能力从 `magi-api` pipeline helper 再往默认 execution runtime 入口下沉，给后续 memory 写回进一步统一留出更稳接缝
- 实际产出：
  - `magi-orchestrator` 已新增 `execute_dispatch_then(...)`，让默认 execution runtime 在 dispatch 成功后可执行统一 success hook，而不需要每个上游调用方各自包一层重复顺序控制
  - `magi-api` 的 `shadow_execution.rs` 已改为复用这条 runtime 入口；route 级 extraction 写回仍由 API 持有，但“何时才允许触发写回”的时机控制已经移到 runtime success hook
  - `magi-orchestrator` 已补 runtime 级回归测试，验证 success hook 在成功路径触发、在不健康执行器失败路径不会触发
- 验证：
  - `cargo test -p magi-orchestrator` 通过
  - `cargo test -p magi-api` 通过
  - `cargo test -p magi-daemon` 通过
  - `cargo test --workspace` 通过
- 当前结论：
  - post-dispatch side effect 现在已经有了默认 runtime 层的统一接缝，后续继续把 memory 写回从 API 特例再往系统默认执行主线统一时，改造面会更小
  - recovery path 本轮未改变；当前下沉只覆盖默认 dispatch 成功后的 hook 面，不影响现有 recovery 行为

### 2026-04-17 第 13 轮：openai-compatible 多 choice 回退策略

- 负责：`magi-bridge-client`
- 目标：继续补强 `openai-compatible` 成功响应在 provider 方言下的稳定性，避免前置不可桥接 choice 让整包成功响应误失败
- 实际产出：
  - `openai-compatible` 的成功响应选择策略已从“固定取 `choices[0]`”推进为“按响应顺序选择第一条可桥接 choice”
  - 当所有 choice 都不可桥接时，错误信息现在会带 `choices[i]` 索引，便于定位是哪一条 choice 失败
  - 已补 runtime 单测，覆盖“跳过前置不可桥接 choice”与“全部 choice 不可桥接时给出索引化错误”
- 验证：
  - `cargo test -p magi-bridge-client model_loopback` 通过
  - `cargo test -p magi-bridge-client` 通过
  - `cargo test --workspace` 通过
- 当前结论：
  - `openai-compatible` 现在对多 choice 成功响应更稳，不会因为第一条 choice 结构怪异就把整个 provider 成功包误判成失败

### 2026-04-17 第 14 轮：daemon 默认执行器切到 local-process

- 负责：`magi-daemon`、`docs`
- 目标：把 daemon 默认 shadow 执行链从 `WorkerRuntime::new_compare()` 继续推进到更接近真实执行主线的 `WorkerRuntime::new()` / local-process 路径
- 实际产出：
  - `magi-daemon` 的 router 装配现在默认使用 `WorkerRuntime::new(...)`，不再走 in-process compare executor
  - 这意味着 `daemon -> api -> /session/action -> orchestrator dispatch` 这条默认 shadow 主链，已经开始默认复用 local-process worker executor，而不是只在专项测试里才触发 local-process 路径
  - 现有 daemon 路由闭环测试保持通过，说明默认主链切到 local-process 后，route 级 dispatch / extraction / followup consumption 仍然成立
- 验证：
  - `cargo test -p magi-daemon router_session_action_auto_extraction_is_consumed_on_followup_dispatch -- --nocapture` 通过
  - `cargo test --workspace` 通过
- 当前结论：
  - 后端默认 shadow 执行主线现在又向真实执行器靠近了一步，不再依赖 compare executor 作为 daemon 默认路径
  - 当前切换没有动 recovery path；默认执行器更真实了，但统一切换前的 provider / MCP / TS 接线仍然是后续工作

### 2026-04-17 第 15 轮：session.action 写回计划统一 + bridge / read-model 护栏补强

- 负责：`magi-api`、`magi-bridge-client`、`magi-event-bus`、`docs`
- 目标：继续把 `/session/action` 的 memory 写回从 route / helper 特例推进到更稳定的统一计划生成，同时补强 bridge 管理面和 read model 上下文持久化的切换前护栏
- 实际产出：
  - `magi-api` 已把 `session.action` 的 extraction payload 生成下沉到 `SessionActionRequestDto::shadow_memory_extraction_request(...)`，`shadow_execution.rs` 现在只负责在 dispatch 成功后按计划写回，不再自己拼接 extraction 内容
  - `magi-api` 已补空文本回归：当 `/session/action` 只有 skill / mode 元信息、没有有效文本时，默认 shadow 路径不会生成多余 extraction，也不会把空写回带进后续 context 消费
  - `magi-bridge-client` 已为 `JsonRpcMcpManagerClient` 补完整 typed lifecycle round-trip 测试；通过仅测试用的 stateful registry transport，覆盖 `register -> list -> describe -> start -> update_health -> stop -> deregister` 的连续状态和 typed response
  - `magi-event-bus` 已补回归，锁定 `mission.execution.overview.context` 在后续不带 `context` 的 follow-up overview 到来后，`knowledge_source_paths` 与 `memory_extraction_refs` 不会被新的增量事件冲掉
- 验证：
  - `cargo test -p magi-api` 通过
  - `cargo test -p magi-bridge-client` 通过
  - `cargo test -p magi-event-bus` 通过
  - `cargo test --workspace` 通过
- 当前结论：
  - `/session/action` 这条默认 shadow 主线现在又少了一层 route 级特例，memory 写回语义开始更稳定地收口到“请求生成计划 + dispatch 成功后执行计划”的模式
  - bridge 管理面和 read model context 持久化都多了一层切换前护栏，但真实外部 manager 进程与更广 execution runtime 调用方统一写回仍然是后续重点

### 2026-04-17 第 16 轮：pipeline 通用 writeback 入口 + refusal-only provider 护栏

- 负责：`magi-api`、`magi-bridge-client`、`docs`
- 目标：继续把 post-dispatch writeback 从 `/session/action` 专用 helper 收口到可复用 pipeline 接缝，同时补强 `openai-compatible` 对上游 refusal-only 成功包的兼容
- 实际产出：
  - `magi-api` 已新增 `shadow_writeback.rs`，把 post-dispatch 写回收成 `ShadowExecutionWritebackPlans`
  - `ShadowExecutionPipeline` 已新增统一的 `execute_dispatch_with_writebacks(...)`；`shadow_execution.rs` 现在不再直接调用 runtime hook，而是复用 pipeline 层通用接缝
  - `SessionActionRequestDto` 仍只负责构造本入口自己的 extraction request；统一的是“成功后如何执行 writeback plan”，不是“各入口如何生成 payload”
  - `magi-api` 已补 `ShadowExecutionWritebackPlans` 的闭环测试，验证空计划是 no-op、memory extraction 计划会落成一致的 extraction linkage
  - `magi-bridge-client` 已补 refusal-only 成功包护栏：当 `openai-compatible` 上游成功响应没有 `message.content`、只有 `message.refusal` 时，桥接层现在会把 refusal 作为稳定可消费的 `content`
  - 并行只读探索确认：除 `/session/action` 外，下一条最可能需要同类 writeback 接缝的真实执行入口是 `execute_recovery(...)`
- 验证：
  - `cargo test -p magi-api` 通过
  - `cargo test -p magi-daemon` 通过
  - `cargo test --workspace` 通过
- 当前结论：
  - post-dispatch writeback 现在已经从 route helper 再向上收口到 pipeline 级复用接口，后面新入口不需要再直接碰 runtime hook
  - 下一条最值得继续打的统一接缝，不是再造新的 API 写入口，而是 recovery 执行链

### 2026-04-17 第 17 轮：recovery success hook 入口补齐

- 负责：`magi-orchestrator`、`docs`
- 目标：把 recovery 执行链也补成与 dispatch 同形的 success hook 接缝，为后续 recovery 级 writeback plan 挂载做准备
- 实际产出：
  - `magi-orchestrator` 已把 recovery 主链抽成内部 `execute_recovery_flow(...)`
  - 在此基础上新增 `execute_recovery_then(...)`，让 recovery 执行成功后也能复用统一 success hook 语义，而不是只能裸跑 `execute_recovery(...)`
  - 已补两条 recovery 级回归：成功路径会触发 hook，缺失 recovery support 的失败路径不会触发 hook
- 验证：
  - `cargo test -p magi-orchestrator` 通过
  - `cargo test -p magi-api` 通过
  - `cargo test --workspace` 通过
- 当前结论：
  - dispatch 和 recovery 这两条真实 runtime 执行入口现在都已经有 success hook 接缝
  - 但 recovery 还没有实际挂上 writeback plan；当前完成的是“接缝到位”，不是“写回已接完”

### 2026-04-17 第 18 轮：recovery writeback plan 真正挂载

- 负责：`magi-api`、`magi-orchestrator`、`docs`
- 目标：把 recovery 从“只有 success hook 接缝”推进到“真的能执行统一 writeback plan”，为后续接 API/daemon 入口做准备
- 实际产出：
  - `magi-api` 的 `ShadowExecutionWritebackPlans` 已新增 `from_recovery_resume_input(...)`，现在能基于 `RecoveryResumeInput` 生成 recovery 级 memory extraction writeback plan
  - recovery writeback 语义已进一步收紧：memory 正文只保留 `diagnostic_summary`，而 `recovery_id / snapshot_id` 则收口到 `recovery://<recovery_id>/snapshot/<snapshot_id>` 形式的 source_ref
  - `ShadowExecutionPipeline` 已新增 `execute_recovery_with_writebacks(...)`，让 recovery 成功后可以直接复用同一套 writeback 计划执行语义
  - 已补 API 级集成测试，验证带 recovery support 的 pipeline 在 recovery 成功后会真实落盘 recovery extraction linkage
  - 并行 bridge 子线补了 `openai-compatible` 的另一层 refusal fallback：当上游返回“空 content + refusal”时，桥接层会优先回退到 refusal，而不是产出空字符串
- 验证：
  - `cargo test -p magi-api` 通过
  - `cargo test -p magi-bridge-client` 通过
  - `cargo test -p magi-orchestrator` 通过
  - `cargo test --workspace` 通过
- 当前结论：
  - recovery 现在不再只是“接缝准备好了”，而是已经能执行真实 writeback plan
  - 当前仍未完成的是：虽然 recovery writeback path 已经存在，但还没有接到真实 API/daemon 入口，也还没有统一到所有 execution runtime 调用方

### 2026-04-17 第 19 轮：recovery API/daemon 真入口 + MCP lifecycle 恢复护栏

- 负责：`magi-api`、`magi-daemon`、`magi-bridge-client`、`docs`
- 目标：把 recovery writeback 从“pipeline/单测里成立”推进到“真实 API/daemon 入口可调用”，并继续补一层 MCP lifecycle 恢复护栏
- 实际产出：
  - `magi-api` 已新增 `/recovery/resume` 路由，并补 `RecoveryResumeRequestDto / RecoveryResumeResponseDto`；该入口会基于 `WorkspaceStore::build_recovery_resume_input(...)` 构造恢复输入，复用 `run_shadow_recovery_resume(...)` 与 `ShadowExecutionPipeline::execute_recovery_with_writebacks(...)` 真执行 recovery
  - `magi-api` 已补两条路由回归：已知 recovery handle 会真正执行 writeback 并把 session sidecar 置为 `Resumed`；未知 recovery handle 会稳定返回 `RECOVERY_NOT_FOUND`
  - `magi-api` 默认测试态 shadow pipeline 现已按生产装配接入 recovery support，而不是只挂 context runtime
  - `magi-daemon` 默认 router 现在也已接入 recovery support，并新增 `session/action -> recovery/resume -> session/action` 三跳闭环回归：recovery 写回的 extraction 能在后续 dispatch 中被 context 真实消费
  - `magi-bridge-client` 已补 `mcp.update_health` 的生命周期恢复护栏：health 从 `unavailable -> healthy` 或 `disabled -> healthy` 后会按 `enabled + health` 重新归一化 `lifecycle_state`，不再卡在旧的 `failed / stopped` 状态
  - `magi-bridge-client` 已补 typed manager client 回归，锁定 health 恢复后的 lifecycle 对外 contract
- 验证：
  - `cargo test -p magi-api` 通过
  - `cargo test -p magi-daemon` 通过
  - `cargo test -p magi-bridge-client` 通过
  - `cargo test --workspace` 通过
- 当前结论：
  - recovery 写回已经不只停留在 pipeline 测试里，而是有了真实 API/daemon 可调用入口
  - memory extraction 的统一化仍未完成；现在打通的是 `/session/action` 与 `/recovery/resume` 两条默认影子主线，不是所有 execution runtime 调用方
  - bridge 的 typed MCP manager 又稳了一层，但真实 provider / MCP / TS 接线仍然是切换前硬阻塞

### 2026-04-17 第 20 轮：recovery 状态护栏 + read-model active 语义收紧

- 负责：`magi-api`、`magi-bridge-client`、`docs`
- 目标：把 `/recovery/resume` 的错误语义从“状态机后置失败”收紧成稳定的 4xx，并修正 read model 对 active recovery 的外部表述；并行继续补一层 MCP manager 幂等护栏
- 实际产出：
  - `magi-api` 已为 `/recovery/resume` 增加前置状态校验：只有 `Ready` recovery 才允许执行；`Prepared` 与 `Consumed` 现在都会稳定返回 `400 INPUT_INVALID`，不再落成 `INTERNAL_ASSEMBLY_ERROR`
  - `magi-api` 已补两条 recovery route 回归，锁定上述 `prepared / consumed` 状态错误语义
  - `magi-api` 已修正 `/runtime/read-model` 的 `active_recovery_ids`：`Consumed` recovery 仍保留在 `summaries` 里，但不再被计入 active 集合，外部语义与 workspace 真相源一致
  - `magi-api` 已补 read model 回归，锁定 “ready 仍 active、consumed 不再 active” 的导出语义
  - 并行 bridge 子线补了 `mcp.update_health` 的幂等 no-op 护栏：当 health 没变化时，响应不会错误回放旧的 `lifecycle_event`；当前操作与历史事件现在被明确分离
- 验证：
  - `cargo test -p magi-api` 通过
  - `cargo test -p magi-bridge-client` 通过
  - `cargo test --workspace` 通过
- 当前结论：
  - recovery API 现在不仅能执行，而且对 `prepared / ready / consumed / missing` 四类状态已经开始有稳定外部语义
  - runtime read model 对 recovery 的 “active” 表述已和底层状态机对齐，后续前后端联调时歧义更少
  - extraction 自动回写的“更广 execution runtime 调用方统一化”仍未完成；真实 provider / MCP / TS 接线仍然是后续主线

### 2026-04-17 第 21 轮：recovery worker 真相源对齐 + manager enable/disable 幂等护栏

- 负责：`magi-orchestrator`、`magi-api`、`magi-bridge-client`、`docs`
- 目标：修复 recovery resume 在请求 `worker_id` 与存量 ownership `worker_id` 不一致时的执行/响应/sidecar 漂移，并继续补 MCP manager mutating 操作的 no-op 幂等护栏
- 实际产出：
  - `magi-orchestrator` 已把 recovery 执行链统一为“以实际执行的 `worker_id` 为真相源”：`execute_recovery(...)` 不再只在 `decision.worker_id` 为空时回填，而是始终把最终执行 worker 回写进 `decision`
  - `magi-orchestrator` 已补回归，锁定“调用方显式传入 override worker 时，`decision.worker_id`、dispatch intent 与 session sidecar ownership 使用同一 worker”
  - `magi-api` 已补 `/recovery/resume` 路由回归，锁定显式 `worker_id` 请求下响应体与 session sidecar 仍保持一致，不会回退成 recovery ownership 中的旧 worker
  - `magi-api` 已修正 `/runtime/read-model` 的 workspace recovery sidecar 合并策略：若事件聚合侧已经给出 recovery summary 的 `worker_id`，则不再被 workspace sidecar 的旧 ownership worker 覆盖回去
  - `magi-api` 已补 read model 回归，锁定“event-sourced recovery worker 优先于 stale workspace sidecar worker”的导出语义
  - 并行 bridge 子线已继续补 `JsonRpcMcpManagerClient::enable_server / disable_server` 的 no-op 幂等护栏：重复 enable/disable 不再回放旧的 `lifecycle_event`
- 验证：
  - `cargo test -p magi-orchestrator` 通过
  - `cargo test -p magi-api` 通过
  - `cargo test -p magi-bridge-client` 通过
  - `cargo test --workspace` 通过
- 当前结论：
  - recovery resume 现在在“实际执行 worker / API response / session sidecar / runtime read model”四条线上已经对齐到同一个真相源
  - MCP manager mutating 操作的 no-op 语义又稳了一层：`update_health` 与 `enable/disable` 都不会把历史 lifecycle event 误报成当前操作结果
  - extraction 自动回写对更广 execution runtime 调用方的统一化仍未完成；真实 provider / MCP / TS 接线仍然是后续主线

### 2026-04-17 第 22 轮：recovery ready 原子性下沉 + MCP unavailable 路由收紧

- 负责：`magi-workspace`、`magi-orchestrator`、`magi-bridge-client`、`magi-daemon`、`docs`
- 目标：把 recovery 的 `Ready` 校验下沉到 workspace/runtime 真入口，避免非 API 调用方先污染 sidecar 再失败；并把 MCP 的 `enabled + unavailable` 从“看起来失败但仍可路由/调用”收紧成真正不可路由/不可调用
- 实际产出：
  - `magi-workspace` 已新增 `ensure_recovery_ready(...)`，`build_recovery_resume_input(...)` 现在只接受 `Ready` recovery；`Prepared` 与 `Consumed` 会在 workspace contract 层直接被拒绝
  - `magi-workspace` 已补回归，锁定“Prepared recovery 先拒绝，标记为 Ready 后才允许构建 resume input”
  - `magi-orchestrator` 已把 recovery `Ready` 校验下沉到 `execute_recovery_flow(...)` 最前面：当调用方绕过 API 直接走 runtime 时，也会在任何 session/workspace sidecar 写入前失败
  - `magi-orchestrator` 已补回归，锁定 direct runtime 调用遇到 `Prepared` recovery 时不会把 session sidecar 提前写成 `RecoveryLinked/Resumed`，workspace recovery 也不会被提前消费
  - `magi-daemon` 的 recovery sidecar 增量持久化回归已同步到新 contract：先 `mark_recovery_ready(...)` 再构建 resume input
  - `magi-bridge-client` 现已把 `unavailable` 视为 `non-routable`：空白 selection 不再把 `enabled + unavailable` server 当默认 fallback target，显式 `mcp.call_tool` 也会直接返回 `server unavailable`
  - `magi-bridge-client` 已补两条 MCP integration 回归，锁定 blank selection 与 explicit call 在 `enabled + unavailable` server 下都会稳定失败
- 验证：
  - `cargo test -p magi-workspace` 通过
  - `cargo test -p magi-orchestrator` 通过
  - `cargo test -p magi-bridge-client` 通过
  - `cargo test -p magi-api` 通过
  - `cargo test -p magi-daemon` 通过
  - `cargo test --workspace` 通过
- 当前结论：
  - recovery 的 `Ready` 现在不只是 API 路由级护栏，而是已经收紧成 workspace/runtime 共用的原子性边界
  - MCP manager/catalog/default route/实际 `call_tool` 对 `unavailable` server 的外部语义已经统一
  - extraction 自动回写对更广 execution runtime 调用方的统一化仍未完成；真实 provider / MCP / TS 接线仍然是后续主线

### 2026-04-17 第 23 轮：workspace recovery sidecar 对齐实际恢复结果

- 负责：`magi-workspace`、`magi-orchestrator`、`magi-api`、`magi-daemon`、`docs`
- 目标：把 workspace recovery sidecar 从“保留恢复前 ownership 快照”推进到“消费 recovery 时同步成实际恢复结果”，避免 bootstrap/read-model/sidecar 持久化继续暴露旧的 todo/worker/chain
- 实际产出：
  - `magi-workspace` 已新增 `consume_recovery_with_ownership(...)`，允许在消费 recovery 时把 `ExecutionOwnership` 更新为实际恢复结果，而不只是切状态
  - `magi-workspace` 已补回归，锁定 consumed recovery export 会保留更新后的 `mission_id / todo_id / worker_id / execution_chain_ref`
  - `magi-orchestrator` 在 `execute_recovery_flow(...)` 中消费 workspace recovery 时，现已把 `decision` 解析出的 `mission / todo / worker / execution_chain_ref` 一并写回 workspace recovery sidecar
  - `magi-orchestrator` 已补回归，锁定显式 override worker 的恢复执行结果不只会进入 `decision` 与 session sidecar，也会进入 `workspace_recovery.ownership`
  - `magi-api` 与 `magi-daemon` 相关回归已通过，说明这次 sidecar 真相源收口没有破坏现有 `/recovery/resume`、bootstrap 与 daemon 持久化链路
- 验证：
  - `cargo test -p magi-workspace` 通过
  - `cargo test -p magi-orchestrator` 通过
  - `cargo test -p magi-api` 通过
  - `cargo test -p magi-daemon` 通过
  - `cargo test --workspace` 通过
- 当前结论：
  - recovery 执行后的 worker/todo/chain 现在不只存在于事件流与 session sidecar，也已经回写到了 workspace recovery sidecar 真相源
  - 这让 bootstrap、runtime read model、sidecar 持久化在 recovery 结果层面进一步对齐
  - extraction 自动回写对更广 execution runtime 调用方的统一化仍未完成；bridge 侧下一处候选则是 `default_server` 在 no-route 场景下的元数据漂移

### 2026-04-17 第 24 轮：recovery 结果保真 + MCP no-route 默认路由元数据收口

- 负责：`magi-api`、`magi-bridge-client`、`docs`
- 目标：避免 consumed workspace recovery sidecar 快照把 event-sourced recovery outcome 降级回 `consumed`；同时修复 MCP manager 在没有可路由默认 server 时仍把 manager 名字暴露成 `default_server`
- 实际产出：
  - `magi-api` 已修正 runtime read model 的 workspace recovery sidecar 合并策略：当事件聚合侧已经给出 recovery outcome 与时间戳时，consumed sidecar 只再做缺失字段补洞，不再回写 `current_status / latest_occurred_at`
  - `magi-api` 已补 read model 回归，锁定 consumed workspace recovery sidecar 不会把 event-sourced `mission_resumed / worker_resumed` 降级成 `consumed`
  - `magi-daemon` 相关 recovery 闭环测试已同步到新语义：恢复成功后的 read model status 继续以事件流 outcome 为准
  - `magi-bridge-client` 已把 MCP manager 的 `default_server` 改成真实可空语义：当不存在 routable default route 时，catalog/manager descriptor 不再把 `shadow-mcp-manager` 伪装成默认 server
  - `magi-bridge-client` 已补 crate 内与 loopback 集成回归，锁定无默认路由时：
    - service catalog 的 `default_server / default_server_health / default_server_selection_key` 为 `None`
    - capability 层仍稳定暴露 `default_server:<none>` 与 `default_server_health:unavailable`
    - blank selection 的 `default server unavailable` 错误体会返回 `default_server = null`、`default_route_target = "<none>"`
  - `magi-bridge-client` 现已补 `echo.describe` happy path 断言，锁定在正常 default route 存在时仍稳定导出真实 `default_server`
- 验证：
  - `cargo test -p magi-bridge-client` 通过
  - `cargo test -p magi-api` 通过
  - `cargo test -p magi-daemon` 通过
  - `cargo test --workspace` 通过
- 当前结论：
  - recovery read model 现在不会再被 consumed sidecar 快照降级，bootstrap/read-model/daemon recovery 闭环对外语义更稳定
  - MCP manager 的 no-route 元数据现在已经与 `default_route_status = unavailable`、`default_route_target = <none>` 对齐，不再把 manager 名称误报成真实默认 server
  - extraction 自动回写对更广 execution runtime 调用方的统一化仍未完成；真实 provider / MCP / TS 接线仍然是后续主线

### 2026-04-17 第 25 轮：writeback 能力下沉到 runtime 公共层

- 负责：`magi-orchestrator`、`magi-api`、`docs`
- 目标：把 dispatch / recovery 成功后的 writeback 从 `magi-api` 私有 helper 下沉到 `OrchestratedExecutionRuntime` 公共层，避免后续新增调用方从一开始就绕开统一写回约定
- 实际产出：
  - `magi-orchestrator` 已新增 `ExecutionWritebackPlans`，把 memory extraction / recovery diagnostic extraction 的计划构建与应用逻辑从 API 私有模块提升为 runtime 可复用能力
  - `magi-orchestrator` 已新增 `execute_dispatch_with_writebacks(...)` 与 `execute_recovery_with_writebacks(...)`，让 dispatch / recovery 侧的 success hook 与 memory writeback 在 runtime 公共层统一收口
  - `magi-orchestrator` 已补 4 条回归：writeback plan 自身的 no-op / closed-loop / recovery builder 语义，以及 dispatch success / failure 下 writeback 执行与跳过语义
  - `magi-api` 已删除私有 `shadow_writeback.rs`，`ShadowExecutionPipeline` 现在直接复用 `magi-orchestrator::ExecutionWritebackPlans` 与 runtime 级 writeback 接缝
  - `magi-api` 现有 `/session/action` 与 `/recovery/resume` 行为保持不变，但 writeback 单元测试已迁到 orchestrator 统一管理，API 侧只保留 pipeline / route 行为回归
- 验证：
  - `cargo test -p magi-orchestrator` 通过
  - `cargo test -p magi-api` 通过
  - `cargo test --workspace` 通过
- 当前结论：
  - writeback 现在已经不只是 API shadow pipeline 约定，而是被压到了 runtime 公共层；未来新调用方只要复用 `OrchestratedExecutionRuntime`，就不必再复制 API 私有逻辑
  - 当前尚未完成的部分是“新的真实调用方是否已经全部切到这层公共接缝”；这仍然需要后续在 daemon / bridge / TS 接线阶段继续验证

### 2026-04-17 第 26 轮：daemon 接上真实 bridge probes 与 dispatch clients

- 负责：`magi-daemon`、`docs`
- 目标：让 daemon 的 `/bridges/services` 不再退回空快照，并让 daemon 内部 `SkillDispatchRuntime` 与对外 bridge snapshot 使用同一套 loopback bridge 真相源
- 实际产出：
  - `magi-daemon` 的 `build_api_state(...)` 现已为 `host / model / mcp` 三类 bridge 构造真实 loopback transport，并同时注入 `BridgeDispatchRuntime` 与 `ApiState::with_bridge_probe_transport(...)`
  - `magi-daemon` 已新增 `bridge_loopback_transport(...)` / `bridge_loopback_executable(...)` helper，优先复用 `CARGO_BIN_EXE_*`，否则按当前 test binary 同目录 sibling executable 定位 loopback 二进制
  - `SkillDispatchRuntime` 现在不再使用裸 `BridgeDispatchRuntime::new()`；daemon 内部 runtime dispatch 与 `/bridges/services` 对外快照已收口到同一套 `host / model / mcp` loopback bindings
  - `magi-daemon` 已补路由回归，锁定真实 daemon `/bridges/services` 会稳定导出非空的 shadow host / model / mcp catalogs
- 验证：
  - `cargo test -p magi-daemon daemon_router_bridge_services_exports_shadow_model_host_and_mcp_catalogs -- --nocapture` 通过
  - `cargo test -p magi-daemon` 通过
  - `cargo test --workspace` 通过
- 当前结论：
  - daemon 对外 bridge 快照现在已经不再是空壳；`/bridges/services` 与内部 runtime dispatch 至少在 shadow loopback 层面对齐到了同一真相源
  - 这一步把切换前“对外协议看起来有 bridge，但 daemon 实际 runtime 没 client”的 split-brain 风险收掉了一大块

### 2026-04-17 第 27 轮：session.action writeback builder 下沉到 runtime 层并收窄 raw 执行入口

- 负责：`magi-orchestrator`、`magi-api`、`docs`
- 目标：把 `/session/action` 仍留在 API DTO 层的 extraction payload 生成继续下沉到 runtime 公共层，并收窄 `execute_dispatch(...)` / `execute_recovery(...)` 这两个无 writeback 约束的 raw 入口，减少后续新调用方误走旁路
- 实际产出：
  - `magi-orchestrator` 已新增 `DispatchMemoryExtractionInput`，并在 `ExecutionWritebackPlans::from_session_action_input(...)` 中统一生成 `session.action` 的 extraction ids、`timeline://...` provenance、skill/deep-task 扩展内容与空文本跳过语义
  - `magi-orchestrator` 已补两条 writeback plan 回归，锁定：
    - 正常 `session.action` 输入会稳定生成 closed-loop extraction linkage
    - 空文本即使带 `skill_name / deep_task` 也不会生成伪 extraction
  - `magi-api` 现已删除 `SessionActionRequestDto::shadow_memory_extraction_request(...)`；`shadow_execution.rs` 只负责把规范化后的 dispatch 输入交给 runtime 公共层，不再直接拼 `MemoryExtractionApplyRequest`
  - `magi-orchestrator` 的 raw `execute_dispatch(...)` / `execute_recovery(...)` 现已收窄为 crate 内可见，默认外部调用面继续聚焦到 `execute_*_with_writebacks(...)` / `execute_*_then(...)`
- 验证：
  - `cargo test -p magi-orchestrator` 通过
  - `cargo test -p magi-api` 通过
- 当前结论：
  - `/session/action` 的 writeback payload 生成现在已经不再留在 API DTO 真相源里，而是与 recovery 一样继续向 runtime 公共层收口
  - 这一步没有新增新的真实调用方，但把 future caller 误复制 route 级 extraction 拼装逻辑的风险继续压低了一层

### 2026-04-17 第 28 轮：新增 bridge preflight smoke 出口并补 bootstrap 消费者验证

- 负责：`magi-api`、`magi-daemon`、`docs`
- 目标：在保留 `/bridges/services` 被动 catalog snapshot 的同时，新增 API/daemon 可直接执行的 bridge smoke/preflight 出口；并补一条 daemon 级 bootstrap 消费者验证，证明 `/bootstrap` 会真实带出 runtime read model 的上下文摘要
- 实际产出：
  - `magi-api` 已新增 `BridgePreflightSnapshotDto`、`BridgePreflightSnapshotProvider` 与 `GET /bridges/preflight`
  - preflight provider 现已能对三类 bridge 执行最小真实 smoke：
    - `host`: `vscode.workspace_roots`
    - `model`: `shadow-model` prompt
    - `mcp`: `mcp.list_servers` + `shadow-mcp.echo.inspect`
  - `magi-api` 已补 DTO/provider 单测与 route 回归，锁定 fake transport 下的 smoke 结果会稳定序列化并保留错误语义
  - `magi-daemon` 真实 loopback router 现已补 `/bridges/preflight` 集成回归，证明 daemon 装配出的 `host / model / mcp` loopback bindings 不只是能导出 catalog，也能执行最小真实调用
  - `magi-daemon` 现已补一条 bootstrap 消费者验证：follow-up `session/action` 后，`/bootstrap.runtime_read_model` 与 `/runtime/read-model` 保持一致，bootstrap 不会丢失 mission context 摘要
- 验证：
  - `cargo test -p magi-api bridge_preflight_route_executes_smoke_results_from_registered_transports -- --nocapture` 通过
  - `cargo test -p magi-daemon daemon_router_bridge_preflight_executes_shadow_host_model_and_mcp_smokes -- --nocapture` 通过
  - `cargo test -p magi-daemon daemon_bootstrap_exports_session_action_context_summary_after_followup_dispatch -- --nocapture` 通过
  - `cargo test --workspace` 通过
- 当前结论：
  - Rust 后端现在已经有了独立于静态 `/bridges/services` 之外的主动 smoke 出口，后续做 provider / MCP / TS cutover smoke 时有了现成 API 支点
  - bootstrap 这条真实消费者出口也已被 daemon 级测试锁住，至少在 follow-up `session/action` 场景下不会丢上下文读模型
  - 统一切换前仍未完成的硬项，已经进一步收敛到“更真实的 provider / MCP 外部接线”和“更多非 shadow 消费面验证”

### 2026-04-17 第 29 轮：补齐 openai-compatible ready preflight 覆盖并把 recovery bootstrap 消费者验证推进到 daemon 级

- 负责：`magi-api`、`magi-daemon`、`docs`
- 目标：把 `/bridges/preflight` 在 model 侧针对 `openai-compatible ready` 的分支覆盖补到 unit/route 两层，并补一条 daemon 级 recovery bootstrap 消费者验证，证明 `/bootstrap` 不只会带出普通 `session/action` follow-up，也能带出 recovery 写回与后续消费后的上下文摘要
- 实际产出：
  - `magi-api` 已在 `bridges.rs` 新增 `preflight_snapshot_provider_includes_openai_compatible_smoke_when_model_catalog_is_ready()`，锁定 model catalog 中只要 `openai-compatible` 标成 `ready`，preflight provider 就会在 `shadow-model` 之外再执行一跳 `openai-compatible` invoke
  - `magi-api` 已在 `routes.rs` 新增 `ProviderAwareModelTransport` 与 `bridge_preflight_route_executes_ready_openai_compatible_smoke()`，锁定 `/bridges/preflight` 对外 JSON 输出在 ready 分支下会稳定包含 `openai-compatible` smoke 结果
  - `magi-daemon` 真实 loopback 集成回归现已补成条件一致性校验：如果 `/bridges/services` 中 `openai-compatible` 的 `service_health == ready`，则 `/bridges/preflight` 必须执行对应 smoke；如果不是 `ready`，则 preflight 不应误跑这条调用
  - `magi-daemon` 已新增 `daemon_bootstrap_exports_recovery_context_after_resume_and_followup_dispatch()`：在真实 daemon router 上完成 `session/action -> recovery/resume -> session/action` 后，`/bootstrap.runtime_read_model` 与 `/runtime/read-model` 保持一致，且 follow-up mission 会带出 recovery extraction ref
- 验证：
  - `cargo test -p magi-api` 通过
  - `cargo test -p magi-daemon` 通过
  - `cargo test --workspace` 通过
  - `cargo test --workspace -- --list | rg '(^test |: test$)' -c` = `331`
- 当前结论：
  - `/bridges/preflight` 现在不只具备 shadow-model / host / mcp 的最小 smoke，还已经把 model 侧最关键的 `openai-compatible ready` 分支补进了稳定覆盖
  - daemon 级 bootstrap 消费者验证现在也不再只覆盖普通 follow-up；recovery 写回经由 `/bootstrap` 暴露给消费者的证据已经补齐到真实 router 组合态

### 2026-04-17 第 30 轮：把 bridge snapshots 并入 bootstrap 统一消费者出口

- 负责：`magi-api`、`magi-daemon`、`docs`
- 目标：把已经独立存在的 `/bridges/services` 与 `/bridges/preflight` 纳入 `/bootstrap`，让 bridge catalog 与 smoke 结果也进入统一消费者出口，为后续 TS cutover / preflight 提供单一读取面
- 实际产出：
  - `magi-api` 的 `BootstrapDto` 现已新增 `bridge_services` 与 `bridge_preflight` 字段，`BootstrapDto::from_state(...)` 会直接复用 `ApiState::bridge_services_dto()` 与 `ApiState::bridge_preflight_dto()`
  - `magi-api` 的 `ShadowExecutionPipeline` 现已新增派生 helper；`shadow_execution.rs` 不再直接构造 `ExecutionWritebackPlans`，而是把 `session.action` / `recovery.resume` 的 plan 生成继续下沉到 pipeline 层
  - `magi-api` 已补 route 回归，锁定 `/bootstrap` 导出的 `runtime_read_model / audit_usage_ledger / bridge_services / bridge_preflight` 与对应独立路由完全一致
  - `magi-daemon` 已补真实 loopback 集成回归，锁定 daemon `/bootstrap` 现在会稳定带出与 `/bridges/services`、`/bridges/preflight` 一致的 shadow host / model / mcp snapshots
- 验证：
  - `cargo test -p magi-api` 通过
  - `cargo test -p magi-daemon daemon_bootstrap_exports_bridge_services_and_preflight_snapshots -- --nocapture` 通过
  - `cargo test --workspace` 通过
  - `cargo test --workspace -- --list | rg '(^test |: test$)' -c` = `332`
- 当前结论：
  - bridge catalog 与 smoke 现在已经不再只是“额外可读的独立路由”，而是正式进入了 `/bootstrap` 统一消费者出口
  - 这一步把后续 TS cutover / dashboard / preflight 类消费者读取 bridge 状态时需要拼多路 API 的复杂度又压低了一层

### 2026-04-17 第 31 轮：新增 `/bridges/cutover-smoke` 独立 cutover 契约视图

- 负责：`magi-api`、`magi-daemon`、`docs`
- 目标：把已有 `services + preflight + loopback contract` 收成一条可判定 blocking 的独立只读资源，专门服务切换前桥接准备检查；同时明确它不并入 `/bootstrap`，避免把 bootstrap 再扩成第二类 cutover 专用真相源
- 实际产出：
  - `magi-api` 已新增 `BridgeCutoverSmokeSnapshotProvider` 与 `/bridges/cutover-smoke` route，输出 `host / model / mcp` 三类 cutover checks
- `BridgeCutoverSmokeSnapshotDto` 现已补顶层 `overall_ok / checked_service_count / blocking_check_count / blocking_services` summary，cutover 调用方不再需要手工遍历所有 checks 才能判断是否 block
- `BridgeCutoverServiceDto` 现已补 `service_ok / blocking_check_count / blocking_targets` service-level summary；调用方现在可先看顶层 `overall_ok`，再按 service summary 下钻，不需要手工重算每个 service 的阻塞项
- `MCP` service 现已再补 `mcp_default_route_gate.route_status / route_target / resolved_server / contract_ok`，调用方不必继续钻进 `checks[0].mcp_contract` 也能稳定读取默认路由 gate
- `BridgeCutoverSmokeSnapshotDto` 现已再补顶层 `blocking_issues` 与稳定 `reason_code`：cutover 调用方现在不只知道“有没有 block”，还知道“为什么 block”
- `blocking_issues.reason_code` 现已补齐 host / model / MCP 关键失败分支覆盖，并在 route / daemon 两侧新增 parity 断言，锁定 `summary = checks` 的纯投影关系，不引入第二真相源
  - model 侧 cutover check 现已收口到 bridge 成功包契约：`shadow-model` 稳定接受非空 plain-text payload，`openai-compatible` 仅在 catalog `service_health == ready` 时执行 contract check，并接受 `plain_text` 与 `structured_json` 两类成功形态；`structured_json` 则要求至少具备 `content` 或 `tool_calls`
  - MCP 侧 cutover check 现已收口到 `mcp.list_servers -> mcp.describe_server(default_route_target) -> blank-selection mcp.call_tool(echo.describe)` 的 default-route contract，并把 `ready / fallback-only / unavailable` 三种 route 状态稳定导出到 API 层
  - `magi-daemon` 已补真实 loopback 集成回归，锁定 `/bridges/cutover-smoke` 在 daemon 组合态下会稳定导出 host 合约、shadow-model 合约，以及按实际 route 状态分支的 MCP default-route contract
  - `docs` 已同步把 `/bridges/cutover-smoke` 记为独立 cutover 证据出口，并明确它当前不进入 `/bootstrap`
- 验证：
  - `cargo test -p magi-api` 通过
  - `cargo test -p magi-daemon` 通过
  - `cargo test --workspace` 通过
  - `cargo test --workspace -- --list | rg '(^test |: test$)' -c` = `337`
- 当前结论：
  - Rust 后端现在已经同时具备：`/bridges/services` 静态 catalog、`/bridges/preflight` 最小 smoke，以及 `/bridges/cutover-smoke` 面向切换评估的 contract 视图
- `/bridges/cutover-smoke` 现在已经不只是“逐项证据面”，而是带顶层 blocking summary 的 cutover gate 视图；调用方可以先看 `overall_ok`，再按 `blocking_services` 下钻
- `/bridges/cutover-smoke` 现在也具备顶层 `blocking_issues`：调用方可先看 `overall_ok`，再直接按 `reason_code` 做机器可读分流，不必先重扫 `services / checks`
- `/bridges/cutover-smoke` 的 service 粒度也已具备稳定 summary：`service_ok / blocking_check_count / blocking_targets` 足以让 TS 或运维在不重扫 checks 的前提下定位阻塞面
- `MCP` 的 service 粒度还进一步具备稳定默认路由 gate：`mcp_default_route_gate.route_status / route_target / resolved_server / contract_ok` 足以支撑第二层 machine-readable cutover 决策
- `bridge_services / bridge_preflight` 继续由 `/bootstrap` 统一导出，而 `/bridges/cutover-smoke` 明确保持独立只读资源，避免 bootstrap 与 cutover 评估面耦合在一起

### 2026-04-17 第 32 轮：补齐 TS 契约层并落最后收口任务树

- 负责：`support/frontend-contract`、`docs`
- 目标：把前端真正会消费的稳定 Rust 出口补进最小 TS 契约层，同时把最后收口阶段的阻塞与执行顺序沉淀成独立任务树
- 实际产出：
  - `support/frontend-contract` 现已把 `BootstrapDto` 补到与 Rust API 当前稳定出口一致：新增 `bridge_services` 与 `bridge_preflight`
  - `support/frontend-contract` 现已补 `/runtime/read-model`、`/ledger`、`/bridges/services`、`/bridges/preflight`、`/bridges/cutover-smoke` 与 `/recovery/resume` 的 DTO 与 client 方法
  - `support/frontend-contract` README 已同步成当前真实覆盖面：最小契约层现在不只覆盖 bootstrap 和 events，也覆盖 bridge cutover gate 与 recovery resume
  - 新增 [30-final-cutover-task-tree.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/30-final-cutover-task-tree.md)，把最后收口阶段拆成 execution runtime、provider/MCP、TS 契约层、M6 放行四条主干
- 验证：
  - `cd /Users/xie/code/magi-rust-rewrite/support/frontend-contract && npm run check` 通过
  - `cd /Users/xie/code/magi-rust-rewrite/support/frontend-contract && npm run build` 通过
  - `cargo test --workspace` 通过
- 当前结论：
  - “TS 实际接线尚未执行”这一阻塞仍然成立，但最小前端契约层已经不再缺失 bridge / recovery / runtime query 出口
  - 后续前后端对接时，TS 不需要再临时补第二套 DTO 真相源；接线将优先围绕 `support/frontend-contract` 与 [30-final-cutover-task-tree.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/30-final-cutover-task-tree.md) 展开

### 2026-04-17 第 33 轮：新增 repo 内可执行 TS smoke 入口

- 负责：`support/frontend-contract`、`docs`
- 目标：把“TS 尚未实际接线”的阻塞继续往前推进一层，先在 repo 内提供一条可复用的最小 smoke 入口，直接读取 daemon 的稳定 HTTP 资源与 bridge gate
- 实际产出：
  - `support/frontend-contract` 新增 `src/smoke.ts`，复用 `RustDaemonClient` 直接读取 `health / version / bootstrap / runtime-read-model / ledger / bridges/services / bridges/preflight / bridges/cutover-smoke`
  - `support/frontend-contract` 新增 `npm run smoke`，支持 `--base-url`、`--json` 与 `--require-cutover-ready`
  - `README` 已补 smoke 用法，明确这条脚手架用于切换前最小 TS 侧验证
  - 基于仓库现状，这轮没有贸然把 `web` 直接接到 `/api/bridges/cutover-smoke`：当前 repo 内尚未看到对应 `/api` 代理实现，先补 repo-level smoke 比“前端能编译但实际 404”的假接线更稳妥
- 验证：
  - `cd /Users/xie/code/magi-rust-rewrite/support/frontend-contract && npm run check` 通过
  - `cd /Users/xie/code/magi-rust-rewrite/support/frontend-contract && npm run build` 通过
  - `cd /Users/xie/code/magi-rust-rewrite/support/frontend-contract && npm run smoke -- --base-url http://127.0.0.1:38123 --json` 通过
  - `cargo test --workspace` 通过
  - `cargo test --workspace -- --list | rg '(^test |: test$)' -c` = `350`
- 当前结论：
  - TS 真实消费面虽然还未正式切到 `web`，但 repo 内已经具备可执行且已实跑通过的 TS smoke 脚手架
  - 后续要继续推进 TS 接线，最短路径会先复用这条 smoke，再补 `web` 的 `/api` 代理与 stats 页消费

### 2026-04-17 第 34 轮：`web` settings 开始真实消费 cutover gate，并补 bridge 只读路由负向护栏

- 负责：`web`、`magi-api`、`magi-daemon`、`docs`
- 目标：把 TS 接线从“repo-level smoke 已成立”继续推进到“真实 `web` 消费面已开始读 Rust cutover gate”，同时锁定 bridge 只读路由不会误触 execution/writeback
- 实际产出：
  - `web/src/web/agent-api.ts` 已新增 `getAgentBridgeCutoverSmoke()`，优先直连 Rust daemon 的 `/bridges/cutover-smoke`，若当前环境仍保留兼容 `/api` 代理则自动回退到 `/api/bridges/cutover-smoke`
  - `web/src/stores/settings-store.svelte.ts` 现已把 cutover smoke 纳入设置面板生命周期：初始 bootstrap 刷新会静默拉取，手动 `refreshConnections()` 也会显式刷新并给出状态反馈
  - `web/src/components/SettingsStatsTab.svelte` 现已显示 cutover smoke banner；settings stats 页不再只看模型连接状态，也开始消费 Rust bridge gate
  - `magi-api` 已新增 `bridge_routes_do_not_touch_shadow_execution_state`，锁定 `/bridges/preflight` 与 `/bridges/cutover-smoke` 不会改写 runtime read model、session/workspace sidecar 或 memory extraction
  - `magi-daemon` 已新增 `daemon_router_bridge_routes_do_not_touch_execution_state`，把同样的“不触发 execution state/writeback”护栏补到真实 daemon 组合态
- 验证：
  - `cd /Users/xie/code/magi-rust-rewrite/web && npm run check` 通过
  - `cd /Users/xie/code/magi-rust-rewrite/web && npm run build` 通过
  - `cargo test -p magi-api bridge_routes_do_not_touch_shadow_execution_state -- --nocapture` 通过
  - `cargo test -p magi-daemon daemon_router_bridge_routes_do_not_touch_execution_state -- --nocapture` 通过
- 当前结论：
  - TS 实际接线已不再是“完全未开始”；`web` 的 settings 刷新链路已经开始真实消费 Rust cutover gate，但这仍只是最小入口，不代表所有前端消费面已切换
  - bridge 只读路由现在有了明确的负向回归，后续继续扩 `/bridges/*` 消费面时，不容易把 execution/writeback 主链意外污染

### 2026-04-17 第 35 轮：cutover smoke 进入前端共享状态层，并保留 MCP describe 底层错误

- 负责：`web`、`magi-api`、`docs`
- 目标：把 cutover smoke 从“settings 局部状态”推进到“前端全局可复用快照”，同时让 `MCP default-route` 在 `describe_server` 失败时保留底层 bridge error，便于更快放行判断
- 实际产出：
  - `web-client-bridge` 现已在 `dispatchSettingsBootstrap(...)` 后异步派发 `bridgeCutoverSmokeLoaded`，`messages store` 新增全局 `bridgeCutoverSmokeSnapshot`，`data-message-handlers` 已接入这条数据消息
  - `settings-store` 现已会优先消费全局缓存的 cutover smoke snapshot，并在收到 `bridgeCutoverSmokeLoaded` 时同步更新本地展示状态；cutover gate 不再只是 settings 自己拉一次、自己保存一次
  - `magi-api` 的 `capture_mcp_cutover_checks(...)` 现在会分别保留 `describe_error` 与 `blank_selection_error`；当 `default_route_target_describe_failed` 发生时，`blocking_issues[i].error` 会保留真实底层 bridge error，而不是只剩泛化 blocking reason
  - `magi-api` 已补 provider 级与 route 级回归，锁定 `/bridges/cutover-smoke` 在 `describe_server` 失败时会稳定带出底层错误消息
- 验证：
  - `cd /Users/xie/code/magi-rust-rewrite/web && npm run check` 通过
  - `cd /Users/xie/code/magi-rust-rewrite/web && npm run build` 通过
  - `cargo test -p magi-api` 通过
  - `cargo test --workspace -- --list | rg '(^test |: test$)' -c` = `353`
- 当前结论：
  - `web` 侧 cutover gate 现在已经进入共享消息流和全局 store，后续扩大 TS 消费面时不必再从 settings 局部状态逆向搬运
  - `MCP default-route` 的 cutover gate 现在不仅能告诉我们“describe 失败了”，也能直接告诉我们“底层哪一层失败、失败消息是什么”

### 2026-04-17 第 36 轮：Header 成为第二真实消费面，并继续细分 MCP blank-selection 原因码

- 负责：`web`、`magi-api`、`docs`
- 目标：把前端 cutover gate 从“settings 可见”推进到“全局顶栏可见”，同时继续把 `MCP default-route` 的 blank-selection 阻塞细分成更稳定的 machine-readable gate
- 实际产出：
  - `web/src/components/Header.svelte` 现已直接消费共享 `bridgeCutoverSmokeSnapshot`，并在顶栏显示 cutover 状态胶囊；它会在冷启动时兜底触发一次 `loadBridgeCutoverSmoke`，不再完全依赖 settings 面板先打开后才有状态
  - `web` 的 cutover gate 现在已经同时存在于 `settings stats` 和 `Header` 顶栏这两个真实消费面，且两者都复用 `messages store` 与 `normalizeBridgeCutoverSmokeStatus(...)`，没有引入第二真相源
  - `magi-api` 已把 `MCP blank-selection` 的默认路由阻塞继续细分为 `mcp_blank_selection_invocation_failed` 与 `mcp_blank_selection_response_not_ok`，并把 `ready` 路径下的原因码推导继续收紧到 `metadata drift / resolved server mismatch` 这类更稳定的 gate 语义
  - `magi-api` 已补 provider 级与 route 级回归，锁定 `/bridges/cutover-smoke` 会稳定导出新的 `reason_code`、对应 `blocking_reason`，以及 `metadata drift / resolved server mismatch` 的序列化结果
- 验证：
  - `cd /Users/xie/code/magi-rust-rewrite/web && npm run check` 通过
  - `cd /Users/xie/code/magi-rust-rewrite/web && npm run build` 通过
  - `cargo test -p magi-api` 通过
  - `cargo test --workspace` 通过
  - `cargo test --workspace -- --list | rg '(^test |: test$)' -c` = `358`
- 当前结论：
  - `web` 对 Rust cutover gate 的真实消费面已经从“settings 单点”扩大到“settings + Header 顶栏”，后续继续扩 TS 消费面时可以围绕共享快照推进，而不是再造局部拉取链
  - `MCP default-route` 的 gate 现在不仅能区分 `describe` 失败，还能继续区分“调用直接失败”“返回 `ok=false`”“metadata drift”“resolved server mismatch”，更适合前端和运维按原因码做放行判断

### 2026-04-17 第 37 轮：BottomTabs 成为第三真实消费面，并补 env-backed smoke 收口

- 负责：`web`、`support/frontend-contract`、`magi-daemon`、`docs`
- 目标：继续扩大真实 TS 消费面，同时把 repo 内 smoke 与 daemon 组合态都往“环境驱动 cutover 判断”推进
- 实际产出：
  - `web/src/components/BottomTabs.svelte` 现已接入共享 `bridgeCutoverSmokeSnapshot`，底栏右侧新增轻量 cutover 状态胶囊；`App -> ThreadPanel -> BottomTabs` 会透传既有 `openSettings` 入口，不会新造第二条请求链
  - `web` 的 cutover gate 现在已经同时存在于 `settings stats`、`Header` 顶栏和 `BottomTabs` 底栏这三个真实消费面，且三者都复用 `messages store + normalizeBridgeCutoverSmokeStatus(...)`
  - `support/frontend-contract` 现已补齐最新 `reason_code` 契约，并把 `smoke.ts` 推进成更适合 CI / env 驱动的 cutover gate：支持 `MAGI_SMOKE_JSON`、`MAGI_REQUIRE_CUTOVER_READY`、`MAGI_FAIL_ON_REASON_CODES`，也支持 `--fail-on-reason-codes` 与 `--help`
  - `support/frontend-contract` 的 smoke JSON 输出现已稳定带出 `blockingIssueCountsByReasonCode`、`blockingIssueCountsByServerKind`、`blockingIssues` 与 `evaluation` 摘要，退出码也已区分 `cutover not ready` 与 `matched blocking reason codes`
  - `magi-daemon` 现已补一条 env-backed 组合态回归：通过测试态 bridge env 注入验证 `/bridges/cutover-smoke` 会正确反映 `openai-compatible` 的环境配置与 `MCP default-route` 的环境默认路由
- 验证：
  - `cd /Users/xie/code/magi-rust-rewrite/web && npm run check` 通过
  - `cd /Users/xie/code/magi-rust-rewrite/web && npm run build` 通过
  - `cd /Users/xie/code/magi-rust-rewrite/support/frontend-contract && npm run check` 通过
  - `cd /Users/xie/code/magi-rust-rewrite/support/frontend-contract && npm run build` 通过
  - `cd /Users/xie/code/magi-rust-rewrite/support/frontend-contract && node dist/smoke.js --help` 通过
  - `cargo test -p magi-daemon` 通过
  - `cargo test --workspace` 通过
  - `cargo test --workspace -- --list | rg '(^test |: test$)' -c` = `359`
- 当前结论：
  - `web` 对 Rust cutover gate 的真实消费面已经从“settings 单点”推进到“settings + Header + BottomTabs”三处共享消费，前端全局可见性明显更强
  - repo 内 smoke 已不只是“能跑”，而是开始具备 CI / 环境变量驱动的 blocking decision 语义；与此同时，daemon 组合态也已经证明 env-backed provider / MCP 会真实反映到 `/bridges/cutover-smoke`

### 2026-04-17 第 38 轮：`cutover-smoke` 补顶层计数摘要，并对齐 API / daemon parity

- 负责：`magi-api`、`magi-daemon`、`docs`
- 目标：把 cutover gate 再往“机器直接消费”推进，减少 TS / CI 自己二次聚合 `blocking_issues` 的成本
- 实际产出：
  - `magi-api` 的 `BridgeCutoverSmokeSnapshotDto` 现已新增顶层 `blocking_issue_counts_by_reason_code / blocking_issue_counts_by_server_kind`
  - `magi-api` 现已补 provider 级与 route 级回归，锁定 ready 快照输出空计数对象，阻塞快照则稳定反映对应 `reason_code` 与 `server_kind` 计数
  - `magi-daemon` 现已补组合态 parity 回归，锁定真实 router 下 `/bridges/cutover-smoke` 的顶层计数字段与 API 语义一致
- 验证：
  - `cargo test -p magi-api` 通过
  - `cargo test -p magi-daemon` 通过
  - `cargo test --workspace` 通过
- 当前结论：
  - `/bridges/cutover-smoke` 现在不只会告诉调用方“哪些项 block 了”，也会直接给出“哪类原因码各出现了多少次、集中在哪类 bridge kind”，更适合作为 TS / CI 的 cutover gate 输入

### 2026-04-17 第 39 轮：InputArea 成为第四个真实 cutover gate 消费面

- 负责：`web`、`docs`
- 目标：把共享 `cutover-smoke` 从“状态展示面”继续推进到“真实发送前决策入口”
- 实际产出：
  - `web/src/components/InputArea.svelte` 现已接入共享 `bridgeCutoverSmokeSnapshot`，会在 bridge `checking / blocked / error` 时展示发送前预检横幅、顶层阻塞问题摘要与手动刷新入口
  - 输入区发送按钮现已根据共享 gate 状态切换提示：`blocked` 时明确提示当前阻塞服务，`error` 时提示先重新检测
  - `web/src/shared/bridges/bridge-cutover-smoke.ts` 继续承载共享归一化与阻塞摘要读取，输入区没有重造第二套 cutover 解析逻辑
  - `web/src/i18n/zh-CN.json` 与 `web/src/i18n/en-US.json` 已补输入区 cutover gate 文案
- 验证：
  - `cd /Users/xie/code/magi-rust-rewrite/web && npm run check` 通过
  - `cd /Users/xie/code/magi-rust-rewrite/web && npm run build` 通过
- 当前结论：
  - `web` 对 Rust cutover gate 的真实消费面现已扩大到 `settings + Header + BottomTabs + InputArea` 四处，其中 `InputArea` 已直接进入发送前决策入口

### 2026-04-17 第 40 轮：`frontend-contract` 支持直连 `cutover-url` 的最小 gate smoke

- 负责：`support/frontend-contract`、`docs`
- 目标：把 repo-level smoke 从“整套 daemon 资源检查”推进到“可直接对独立 cutover 资源做放行判断”
- 实际产出：
  - `support/frontend-contract/src/smoke.ts` 现已支持 `--cutover-url` 与 `MAGI_CUTOVER_SMOKE_URL`：可直接拉取独立 `/bridges/cutover-smoke` 资源，仅执行 cutover gate evaluation
  - `support/frontend-contract/src/client.ts` 已导出 `fetchBridgeCutoverSmokeSnapshotFromUrl(...)`，最小 TS 契约层不再只能经 `RustDaemonClient` 走整套 daemon 资源
  - `support/frontend-contract/src/cutover-gate.ts` 继续承载统一 `reason_code / server_kind` 聚合与 gate evaluation；`--cutover-url` 与 `--base-url` 两种模式共用同一套判定逻辑
  - `support/frontend-contract/README.md` 已补直连 `cutover-url` 的使用示例
- 验证：
  - `cd /Users/xie/code/magi-rust-rewrite/support/frontend-contract && npm run check` 通过
  - `cd /Users/xie/code/magi-rust-rewrite/support/frontend-contract && npm run build` 通过
  - `cd /Users/xie/code/magi-rust-rewrite/support/frontend-contract && node dist/smoke.js --help` 通过
  - 基于本地 HTTP stub，`node dist/smoke.js --cutover-url http://127.0.0.1:38145/bridges/cutover-smoke --fail-on-reason-codes mcp_default_route_status_unavailable --json` 返回退出码 `3`
- 当前结论：
  - repo-level smoke 现在不仅能对完整 daemon 做预检，也能作为独立 cutover gate CLI 被 CI / 脚本直接复用；TS 接线前的放行路径更短了

### 2026-04-17 第 41 轮：settings 直接消费 cutover 顶层计数摘要

- 负责：`web`、`docs`
- 目标：让 settings diagnostics 优先读取后端已经聚好的 `reason_code / server_kind` 计数，而不是前端自己重扫 `blocking_issues`
- 实际产出：
  - `web/src/shared/bridges/bridge-cutover-smoke.ts` 现已补 `blocking_issue_counts_by_reason_code / blocking_issue_counts_by_server_kind` 的读取与 fallback helper，settings 侧可直接消费后端聚合结果
  - `web/src/components/SettingsStatsTab.svelte` 现已新增 `Bridge cutover diagnostics` 聚合区：会稳定展示 `checked services / blocking checks / blocking services` 三项总览，以及按 `bridge kind` 和 `reason code` 聚合的阻塞统计
  - settings 统计面现在优先读取顶层计数摘要，只在缺少这些字段时才回退到前端本地聚合，避免把 cutover gate 重新做成前端第二真相源
- 验证：
  - `cd /Users/xie/code/magi-rust-rewrite/web && npm run check` 通过
  - `cd /Users/xie/code/magi-rust-rewrite/web && npm run build` 通过
- 当前结论：
  - settings 已不只是 cutover banner 的展示面，而是开始直接消费后端 machine-readable gate summary；这让 TS 侧距离真实放行决策又近了一步

### 2026-04-17 第 42 轮：env-backed cutover failure smoke 拆分为稳定 provider / MCP 失败路径

- 负责：`magi-daemon`、`docs`
- 目标：把 daemon 组合态下的 env-backed cutover failure 从“单条混合失败回归”收口成更稳定、更可定位的最小真实 smoke
- 实际产出：
  - `magi-daemon` 现已新增 `daemon_router_bridge_cutover_smoke_surfaces_env_backed_provider_failure_with_ready_mcp_route`，锁定真实 env 配置下 `openai-compatible` 上游 `401` 会稳定反映为 `bridge_invocation_failed`，同时 MCP ready 路由不会被误计入阻塞
  - `magi-daemon` 现已新增 `daemon_router_bridge_cutover_smoke_surfaces_env_backed_mcp_fallback_only_route`，锁定默认 MCP server 处于 `degraded` 时 `/bridges/cutover-smoke` 会稳定导出 `mcp_default_route_status_fallback_only`
  - `magi-daemon` 现已新增 `daemon_router_bridge_cutover_smoke_surfaces_env_backed_mcp_unavailable_route`，锁定默认 MCP server 不可路由且无 fallback 时 `/bridges/cutover-smoke` 会稳定导出 `mcp_default_route_status_unavailable` 与 `<none>` 目标
  - 这三条回归都走真实 `router_with_bridge_env_for_tests(...)`、loopback bridge 二进制与 HTTP stub，不再只停在 fake transport / provider 层单测
- 验证：
  - `cargo test -p magi-daemon daemon_router_bridge_cutover_smoke_surfaces_env_backed_provider_failure_with_ready_mcp_route -- --nocapture` 通过
  - `cargo test -p magi-daemon daemon_router_bridge_cutover_smoke_surfaces_env_backed_mcp_fallback_only_route -- --nocapture` 通过
  - `cargo test -p magi-daemon daemon_router_bridge_cutover_smoke_surfaces_env_backed_mcp_unavailable_route -- --nocapture` 通过
  - `cargo test --workspace` 通过
  - `cargo test --workspace -- --list | rg '(^test |: test$)' -c` = `362`
- 当前结论：
  - daemon 组合态的 env-backed cutover smoke 已经不再只有 happy path；provider 被上游拒绝、MCP fallback-only 默认路由、MCP unavailable/no-route 现在都已有稳定、可定位的真实失败回归

### 2026-04-17 第 43 轮：provider invalid-response、InputArea 真 gate、strict smoke 入口

- 负责：`magi-daemon`、`web`、`support/frontend-contract`、`docs`
- 目标：继续把切换前 gate 从“可见”推进到“可决策”，并补齐更贴近真实兼容层漂移的 provider smoke
- 实际产出：
  - `magi-daemon` 现已新增 `daemon_router_bridge_cutover_smoke_surfaces_env_backed_provider_invalid_response_with_ready_mcp_route`，锁定 `openai-compatible` 在 `HTTP 200` 但响应体不可桥接时，会稳定落成 `bridge_invocation_failed`
  - `web/src/components/InputArea.svelte` 现已把共享 `bridgeCutoverSmokeStatus` 真正接进发送决策：`checking / blocked / error` 时不只是展示 banner，而是会实际阻止发送、保留输入内容，并给出对应 toast 提示
  - `support/frontend-contract/package.json` 现已新增 `npm run smoke:strict`，把 `--require-cutover-ready --json` 固化成默认严格 smoke 入口，后续 CI / cutover preflight 不必每次手拼参数
- 验证：
  - `cargo test -p magi-daemon daemon_router_bridge_cutover_smoke_surfaces_env_backed_provider_invalid_response_with_ready_mcp_route -- --nocapture` 通过
  - `cd /Users/xie/code/magi-rust-rewrite/web && npm run check` 通过
  - `cd /Users/xie/code/magi-rust-rewrite/web && npm run build` 通过
  - `cd /Users/xie/code/magi-rust-rewrite/support/frontend-contract && npm run check` 通过
  - `cd /Users/xie/code/magi-rust-rewrite/support/frontend-contract && npm run smoke:strict -- --help` 通过
  - `cargo test --workspace` 通过
  - `cargo test --workspace -- --list | rg '(^test |: test$)' -c` = `363`
- 当前结论：
  - daemon 组合态现在不仅能看见 provider 被拒绝，也能看见“上游 200 但 payload 契约已漂移”的真实失败面
  - `InputArea` 已从 cutover 预警面推进成真实发送 gate；前端第一个真正发起任务的入口现在已经开始服从 Rust cutover 判断
  - `support/frontend-contract` 现在不仅有灵活 smoke，还有固定严格入口，TS / CI 距离真正 cutover 放行脚本又近了一步

### 2026-04-17 第 44 轮：catalog 中的 degraded provider 不再被 cutover-smoke 跳过

- 负责：`magi-api`、`magi-daemon`、`docs`
- 目标：修掉 `/bridges/cutover-smoke` 的 provider 漏检语义，让 catalog 中已暴露但处于 `degraded` 的 `openai-compatible` 不再被默默跳过
- 实际产出：
  - `magi-api` 现已把 model cutover check 的 `openai-compatible` 选择条件从“仅 `service_health == ready` 才执行”收紧为“只要 catalog 中存在就执行并给出结果”；`preflight` 语义保持不变，仍只在 ready 时跑 real smoke
  - `magi-api` 现已新增 `cutover_smoke_snapshot_provider_does_not_skip_degraded_openai_compatible`，锁定 degraded `openai-compatible` 仍会出现在 checks / blocking issues / blocking targets 中
  - `magi-daemon` 现已新增 `daemon_router_bridge_cutover_smoke_surfaces_cataloged_degraded_provider_with_ready_mcp_route`，锁定“provider 未配置导致 degraded，但 MCP default route ready”时，`/bridges/cutover-smoke` 仍会稳定导出 model 阻塞而不是误判为全绿
  - `magi-daemon` 的通用 `daemon_router_bridge_cutover_smoke_exports_contract_snapshots` 断言已改为按 catalog 中的 `openai-compatible.service_health` 分支判断，不再把 model 一律当成 ready
- 验证：
  - `cargo test -p magi-api cutover_smoke_snapshot_provider_does_not_skip_degraded_openai_compatible -- --nocapture` 通过
  - `cargo test -p magi-daemon daemon_router_bridge_cutover_smoke_surfaces_cataloged_degraded_provider_with_ready_mcp_route -- --nocapture` 通过
  - `cargo test -p magi-daemon daemon_router_bridge_cutover_smoke_exports_contract_snapshots -- --nocapture` 通过
  - `cargo test -p magi-api` 通过
  - `cargo test -p magi-daemon` 通过
  - `cargo test --workspace` 通过
  - `cargo test --workspace -- --list | rg '(^test |: test$)' -c` = `365`
- 当前结论：
  - `/bridges/cutover-smoke` 现在不会再把“catalog 已暴露、但健康状态是 degraded 的 provider”漏成隐式通过
  - cutover gate 对 provider 的 machine-readable 阻塞语义已经和 `401`、invalid payload、MCP fallback/unavailable 这些真实失败面站到同一层级

### 2026-04-17 第 45 轮：web-client-bridge 执行前 preflight 正式接入 cutover gate

- 负责：`web`、`docs`
- 目标：补上“`InputArea` 会拦，但 bridge runtime 层的真实执行入口仍可能绕过”的层级缺口
- 实际产出：
  - `web/src/shared/bridges/web-client-bridge.ts` 现已把 `cachedBridgeCutoverSmokeStatus / cachedBridgeCutoverSmokeError` 收口成桥接层本地状态，`dispatchBridgeCutoverSmoke(...)` 会同步维护这份状态，不再只是向消息流派发 UI 事件
  - `ensureFreshLiveBridge(...)` 现已把 cutover gate 接入真实执行 preflight：若本地没有 `cutover-smoke` 快照或状态不是 `ok`，会先刷新一次 `/bridges/cutover-smoke`；结果若仍为 `checking / blocked / error`，则直接阻止后续执行
  - `executeTask / startTask / resumeTask` 因为都走 `ensureFreshLiveBridge(...)`，现在已不再只依赖 `InputArea` 这一层 UI gate；真实 bridge runtime 入口也开始服从 Rust cutover 结论
- 验证：
  - `cd /Users/xie/code/magi-rust-rewrite/web && npm run check` 通过
  - `cd /Users/xie/code/magi-rust-rewrite/web && npm run build` 通过
- 当前结论：
  - cutover gate 现在不只是 UI 展示和输入框拦截，而是已经进入 `web-client-bridge` 的真实执行 preflight
  - `InputArea` 与 bridge runtime 层不再各自维护两套独立发送判断，前端真实执行主线距离统一 cutover 放行又近了一步

### 2026-04-17 第 46 轮：frontend-contract 补上可选 `/events` smoke，自检脚手架成型

- 负责：`support/frontend-contract`、`docs`
- 目标：把 repo-level TS smoke 从“只读 daemon 资源”继续推进到“可选验证 `/events` 连通性”，同时保持默认行为不变
- 实际产出：
  - `support/frontend-contract/src/client.ts` 现已新增带超时的 `probeEventStream(...)`：会请求 `/events`，读取首个可解析 SSE 事件，并返回 `event_id / event_type / category` 摘要
  - `support/frontend-contract/src/smoke.ts` 现已新增 `--check-events` / `MAGI_CHECK_EVENTS`，启用时会把 `/events` 的首个事件摘要写进 smoke 输出；默认不启用，因此不会改变现有 CI / cutover gate 行为
  - `support/frontend-contract/package.json` 现已新增 `npm run smoke:events` 与 `npm run verify:events`
  - `support/frontend-contract/scripts/verify-events-smoke.mjs` 现已补本地 mock SSE 自检脚本，避免验证 `/events` 探针时强依赖真实 daemon
- 验证：
  - `cd /Users/xie/code/magi-rust-rewrite/support/frontend-contract && npm run check` 通过
  - `cd /Users/xie/code/magi-rust-rewrite/support/frontend-contract && npm run build` 通过
  - `cd /Users/xie/code/magi-rust-rewrite/support/frontend-contract && npm run verify:events` 通过
- 当前结论：
  - repo-level smoke 现在已经不只是 `health / bootstrap / runtime / bridge gate` 的只读汇总，还能在需要时补一条最小 `/events` 连通性验证
  - `/events` smoke 是显式 opt-in 的，不会把现有严格 gate 入口 `smoke:strict` 变成更脆弱的默认脚本

### 2026-04-17 第 47 轮：frontend-contract 补上可选 `session/action` execution smoke

- 负责：`support/frontend-contract`、`docs`
- 目标：把 repo-level TS smoke 再往真实执行主线推进半步，让 TS / CI 在需要时可以显式打一条最小 `POST /session/action`
- 实际产出：
  - `support/frontend-contract/src/smoke.ts` 现已新增 `--check-session-action / MAGI_CHECK_SESSION_ACTION`，启用时会发送一条固定最小 `POST /session/action`
  - 这条 execution smoke 直接复用现有 `submitSessionAction(...)` client，不新增第二套执行契约；返回摘要会稳定导出 `session_id / entry_id / event_id / accepted_at / created_session`
  - `--check-session-action` 与 `--cutover-url` 当前明确互斥，避免把“只读 cutover gate”与“真实执行 smoke”混成一条脚本路径
  - `support/frontend-contract/package.json` 现已新增固定入口 `npm run smoke:execution`
- 验证：
  - `cd /Users/xie/code/magi-rust-rewrite/support/frontend-contract && npm run check` 通过
  - `cd /Users/xie/code/magi-rust-rewrite/support/frontend-contract && npm run build` 通过
  - `cd /Users/xie/code/magi-rust-rewrite/support/frontend-contract && npm run smoke:execution -- --help` 通过
- 当前结论：
  - repo-level TS smoke 现在已经不只是 `health / bootstrap / runtime / bridge gate + /events` 的只读脚手架，还能在需要时补一条最小真实执行请求
  - 本轮本机未发现可直接复用的本地 daemon，故 `--check-session-action` 的 live POST 尚未在本机实跑；当前已确认脚本入口、契约、摘要输出与互斥护栏都已接好

### 2026-04-17 第 48 轮：provider 失败语义不再压平成单一 `bridge_invocation_failed`

- 负责：`magi-bridge-client`、`magi-api`、`magi-daemon`、`support/frontend-contract`、`web`、`docs`
- 目标：把 `/bridges/cutover-smoke` 的 model/provider 失败从“只有 block”推进到“有稳定 machine-readable 原因码”，让前端与 CI 的 `reason_code` 真正可分流
- 实际产出：
  - `magi-bridge-client` 现已在 `BridgeClientError::CallFailed` 保留远端业务错误码，`RemoteBusiness` 不再只剩文本消息
  - `magi-api` 现已把 `BridgeProbeErrorDto` 扩为 `layer + code + message`，并为 model/provider 新增 `model_provider_unavailable / model_provider_misconfigured / model_provider_transport_failed / model_provider_rejected / model_provider_invalid_response`
  - `/bridges/cutover-smoke` 现在会按 remote business code `-32003 ~ -32007` 稳定分流 provider 失败，不再把 `unavailable / rejected / invalid response` 一律压平成 `bridge_invocation_failed`
  - `support/frontend-contract` 与 `web` 现已同步补齐新 reason codes 与 `error.code` 的最小消费类型；`InputArea` 的 gate 文案也已补上 provider 专属说明
- 验证：
  - `cargo test --workspace` 通过
  - `cd /Users/xie/code/magi-rust-rewrite/support/frontend-contract && npm run check` 通过
  - `cd /Users/xie/code/magi-rust-rewrite/support/frontend-contract && npm run build` 通过
  - `cd /Users/xie/code/magi-rust-rewrite/support/frontend-contract && npm run smoke:execution -- --help` 通过
  - `cd /Users/xie/code/magi-rust-rewrite/web && npm run check` 通过
  - `cd /Users/xie/code/magi-rust-rewrite/web && npm run build` 通过
- 当前结论：
  - `/bridges/cutover-smoke` 现在对 provider 失败已经不只是“知道失败了”，而是开始稳定区分“不可用 / 配置错误 / 传输失败 / 请求被拒 / 响应无效”
  - 这轮收益会直接落在当前已经上线消费 `reason_code` 的 TS / web / CI 侧，不需要再额外扩展示位

### 2026-04-18 第 49 轮：真实 daemon 联调 smoke 跑通，默认入口对齐 `38123`

- 负责：`apps/daemon`、`support/frontend-contract`、`docs`
- 目标：把“可以正式对接”的结论补成一轮真实 live 证据，而不是只停在 repo 内单测与脚手架层
- 实际产出：
  - `apps/daemon/src/main.rs` 现已支持通过 `MAGI_HOST / MAGI_PORT / MAGI_SERVICE_NAME / MAGI_STATE_ROOT` 覆盖默认启动配置；daemon app 不再只能绑死在硬编码地址和 state 目录
  - `support/frontend-contract/src/smoke.ts` 的默认 `MAGI_BASE_URL` 已从 `http://127.0.0.1:3000` 对齐到真实 daemon app 默认地址 `http://127.0.0.1:38123`
  - `support/frontend-contract/README.md` 的 smoke 示例也已统一对齐到 `38123`
  - 在本机真实拉起 `cargo run -p magi-daemon-app` 后，先复现了无 provider 配置时 `/bridges/cutover-smoke -> model_provider_unavailable` 的真实阻塞
  - 随后通过本地 OpenAI-compatible HTTP stub + `MAGI_OPENAI_COMPAT_BASE_URL / MAGI_OPENAI_COMPAT_API_KEY / MAGI_OPENAI_COMPAT_MODEL` 重新拉起 daemon，`/bridges/cutover-smoke` 已真实返回 `overall_ok=true`
  - 同一轮 live daemon 上，`npm run smoke:strict`、`npm run smoke:execution`、`npm run smoke:task-execute` 都已通过；`/session/action` 与 `/task/execute` 的最小真实请求已经不只是在帮助文本或 mock 脚手架层成立
- 验证：
  - `curl http://127.0.0.1:38123/health` 通过
  - `curl http://127.0.0.1:38123/bridges/cutover-smoke` 在无 provider 配置时稳定返回 `model_provider_unavailable`
  - `curl http://127.0.0.1:38123/bridges/cutover-smoke` 在本地 provider stub + env 配置下稳定返回 `overall_ok=true`
  - `cd /Users/xie/code/magi-rust-rewrite/support/frontend-contract && npm run smoke:strict -- --base-url http://127.0.0.1:38123` 通过
  - `cd /Users/xie/code/magi-rust-rewrite/support/frontend-contract && npm run smoke:execution -- --base-url http://127.0.0.1:38123` 通过
  - `cd /Users/xie/code/magi-rust-rewrite/support/frontend-contract && npm run smoke:task-execute -- --base-url http://127.0.0.1:38123` 通过
- 当前结论：
  - 真实联调环境里的最大阻塞已从“后端代码主链未接好”收窄成“provider 是否已配置”
  - 当 provider 最小配置存在时，当前 Rust daemon + frontend-contract 已经能在 live HTTP 条件下跑通严格 gate 与最小执行 smoke
