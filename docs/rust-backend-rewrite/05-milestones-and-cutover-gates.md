# Rust 后端重构里程碑与切换门槛

更新时间：2026-04-16

> 本文档用于定义“本地影子重构”的阶段目标，以及“何时可以统一切换”的门槛。

---

## 1. 总策略

当前采用：

> 并行重构 Rust 后端，运行隔离，最终统一切换。

因此本阶段的核心不是“如何渐进接管”，而是：

1. 如何保证重构不失控
2. 如何判断后端何时算“基本完成”
3. 如何判断是否具备统一切换资格

---

## 2. 里程碑划分

### M1：模型与边界冻结

目标：

- 冻结能力域
- 冻结 crate 映射
- 冻结关键领域模型
- 冻结能力对照表与语义偏差台账
- 冻结协议边界、本地影子工作区规则与验证矩阵

出口条件：

- 能力对照表完整
- 语义偏差台账可用
- crate 映射表稳定
- 协议冻结文档可用
- 本地工作区 bootstrap 文档可用
- 验证矩阵文档可用
- 第一批任务单已准备完成
- 当前不做 UI / Host 接线的边界已明确

阶段产物：

- `02-capability-matrix.md`
- `03-semantic-deviation-ledger.md`
- `04-module-mapping-and-target-crates.md`
- `07-schema-and-contract-freeze.md`
- `08-local-shadow-rust-workspace-bootstrap.md`
- `09-validation-matrix-and-readiness-checklist.md`
- 第一批 Agent 任务单

阶段失败信号：

- 仍以旧巨型模块作为 Rust crate 切分依据
- 仍无法说清“哪些语义要继承，哪些语义要重定义”
- 无法说明 API / SSE / Host Bridge / Tool Protocol 的稳定外形
- 多个 Agent 仍需要各自决定工作区起盘方式

### M2：基础运行骨架完成

目标：

- 建立 `magi-core`
- 建立 `magi-daemon`
- 建立 `magi-api`
- 建立基础 session / workspace 模型

出口条件：

- Rust 工作区可编译
- 基础服务骨架稳定
- API / event / host bridge 基本结构可落地
- `magi-core` 领域模型已形成初版
- `magi-daemon` 与 `magi-api` 边界已代码化，而不是文档化

阶段产物：

- Rust workspace 目录骨架
- `Cargo.toml` workspace 配置
- `magi-core`
- `magi-daemon`
- `magi-api`
- 与 [08-local-shadow-rust-workspace-bootstrap.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/08-local-shadow-rust-workspace-bootstrap.md) 一致的目录与依赖骨架

阶段失败信号：

- `magi-api` 开始承载业务状态机
- `magi-core` 中出现文件系统、网络、进程、IDE SDK 依赖

### M3：后端状态内核完成

目标：

- session / workspace / snapshot / governance / event bus 基础能力具备

出口条件：

- Session / Workspace 真相源结构稳定
- Snapshot / Recovery 语义明确
- Event / Audit / Usage 基础链路可成立
- Session 聚合、projection、execution/recovery sidecar 已拆层
- Workspace / Snapshot / Recovery 不再依赖 UI 或宿主结构

阶段产物：

- `magi-session-store`
- `magi-workspace`
- `magi-governance`
- `magi-event-bus`

阶段失败信号：

- session / workspace 仍在同一对象中承担读模型与 durable state
- 审计 / usage / SSE 事件仍无统一主链

### M4：执行内核完成

目标：

- tool runtime
- orchestrator
- worker runtime

出口条件：

- Mission / Assignment / Todo / Worker 主链闭环
- Builtin tools 与治理策略可闭环
- dispatch / worker / tool 三条执行线已通过稳定接口协作
- 不再依赖旧 Node runtime 语义作为执行真相源

阶段产物：

- `magi-tool-runtime`
- `magi-orchestrator`
- `magi-worker-runtime`

阶段失败信号：

- Rust 执行内核仍大量依赖旧 TS 实现作为运行参考
- Tool / Worker / Dispatch 三者边界继续混装
- 真实外部执行器仍不存在，worker execute 虽已具备带 `execution_mode / affinity / stage_matrix` 的本地子进程能力矩阵，但仍停留在最小 local process 路径

### M5：知识、记忆、上下文完成

目标：

- knowledge
- memory
- context
- skill runtime

出口条件：

- 长期知识、记忆与上下文能力具备
- 关键产品能力覆盖完整
- Knowledge / Memory / Context 三层职责已拆开
- Skill runtime 已与 builtin tool runtime 分层

阶段产物：

- `magi-knowledge-store`
- `magi-memory-store`
- `magi-context-runtime`
- `magi-skill-runtime`

阶段失败信号：

- `knowledge`、`memory`、`context` 继续互相直接持有内部状态
- Skill runtime 继续依附在 ToolManager 式超级对象上

### M6：统一切换评估

目标：

- 评估是否具备替换现有后端的资格

出口条件：

- 能力覆盖达标
- 语义偏差已收口
- 切换影响面可评估
- 对外协议已有明确切换计划
- Host / UI 接线改造只剩收口，而非反向定义后端

阶段产物：

- 切换清单
- 风险清单
- 回归验证清单
- 统一切换执行窗口建议

阶段失败信号：

- 核心能力域仍有大面积“待补充”
- 仍无法说明切换后 API / SSE / Host Bridge 的稳定形状
- bridge transport 虽已有 model / host / MCP 最小可验证服务端路径，且已具备 host shell 元信息与 MCP manager / server 目录语义，但真实 host / MCP 服务端、provider 适配与宿主壳仍未完成

---

## 3. 统一切换门槛

只有在以下条件全部满足时，才允许进入统一切换评估：

1. 能力对照表中关键能力域达到“已覆盖”或“待验证”
2. 高风险语义偏差已明确收口决策
3. 核心运行态具备清晰真相源
4. 没有依赖旧实现的临时补丁层
5. 关键数据模型与协议已稳定
6. 第一批关键 crate 已达到“可独立演进”的程度
7. Host / UI 只需要接线，不需要继续定义后端核心语义
8. 高风险偏差条目至少完成首批收口：
   - `D-001`
   - `D-002`
   - `D-003`
   - `D-004`
   - `D-006`
   - `D-010`

---

## 4. 当前禁止提前切换的情形

出现以下任一情况时，禁止推进统一切换：

1. 能力对照表仍有核心能力域未建模
2. 语义偏差台账中的关键问题尚未决策
3. Rust 重构版仍严重依赖旧实现作为逻辑参考运行
4. 仅完成了基础服务壳，但执行内核尚未闭环
5. 关键 crate 只是目录存在，但职责仍未稳定
6. 切换后仍需要保留大规模兼容分支或回退逻辑

---

## 5. 建议的阶段验证方式

### M1 验证

- 文档评审
- 能力覆盖核对
- 偏差条目与 crate 设计一致性核对
- 协议冻结文档、工作区 bootstrap 文档、验证矩阵文档三者一致性核对

### M2 验证

- Rust workspace 编译通过
- 基础配置加载与服务启动通过
- API / event 基本结构检查通过
- 工作区结构、crate 依赖方向、基础设施约定与 [08-local-shadow-rust-workspace-bootstrap.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/08-local-shadow-rust-workspace-bootstrap.md) 一致

### M3 验证

- session / workspace / snapshot 领域模型自洽
- 恢复与审计主链可解释
- 状态与投影分层清晰

### M4 验证

- Mission / Assignment / Todo / Worker 主链跑通
- builtin tools 可执行
- 治理策略能进入执行链

### M5 验证

- knowledge / memory / context / skill 各层职责边界稳定
- 长任务关键语义可保留
- 能力对照表已接近覆盖完成

### M6 验证

- 统一切换清单完整
- 与 [09-validation-matrix-and-readiness-checklist.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/09-validation-matrix-and-readiness-checklist.md) 的切换就绪清单逐项对齐
- 风险清单完整
- 高风险偏差条目已收口
- 切换后不依赖兼容补丁

---

## 6. 当前进度结论

当前已经明显越过 `M1-M2` 的准备阶段，整体状态可概括为：

1. `M1` 已完成：文档体系、能力矩阵、语义偏差台账、协议冻结、影子工作区规则与验证矩阵均已建立
2. `M2` 已完成：影子 Rust workspace 与首批 crate 已建立，workspace 级 `cargo check` / `cargo test --workspace` 已通过
3. `M3-M4` 已进入实质覆盖：session / workspace / recovery、worker / orchestrator / tool 主链均已形成稳定骨架
4. `M5` 已进入集中收口阶段：knowledge / memory / context / skill runtime 边界已明显收紧，runtime query contract 已收口为稳定 schema
5. `M6` 预检资料已成形：[26-m6-precheck-checklist.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/26-m6-precheck-checklist.md)、[27-ts-cutover-wiring-checklist.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/27-ts-cutover-wiring-checklist.md)、[28-m6-cutover-evaluation-package.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/28-m6-cutover-evaluation-package.md) 与 [29-idea-host-defer-decision.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/29-idea-host-defer-decision.md) 已可直接作为切换前收口入口

### 6.1 当前 M5 收口结果

本轮集中推进后，M5 已具备以下结果：

- `magi-knowledge-store`
  - 已形成 `indexer / query / governed output` 三层结构
  - 已支持 tags、source_ref、评分查询与稳定排序
- `magi-memory-store`
  - 已形成 `session / preference / extraction / compaction history` 四层真相源
  - extraction 与 compaction 已形成可查询历史
- `magi-context-runtime`
  - 已固定六类运行时来源：
    - knowledge store
    - memory store
    - shared context pool
    - file summary store
    - session recent turns
    - project recent turns
  - recent turns 已具备双路限额、去重与来源优先级治理
- `magi-skill-runtime`
  - 已形成 `tool_policy / routing / bridge_dispatch_plan` 三段式运行时计划
  - 已具备统一 `SkillDispatchRuntime`
  - builtin / bridge / denied 请求已显式分流
- `magi-bridge-client`
  - 已形成安全消费 `BridgeBindingDispatchPlan` 的客户端边界
  - 已补本地 JSON-RPC over stdio 的最小 transport client
  - 已补 model bridge loopback server 验证回环
  - 但仍未进入真实 host / MCP 服务端或 provider 适配
- `magi-tool-runtime`
  - 已补 `file.read / search.text / shell.exec / process.inspect / diff.preview` 五类 builtin 的真实执行器骨架
  - 已支持 `ToolExecutionInput.input` 的 JSON / raw 双输入约定
  - 已补 builtin access mode 与并发写防护，可按 `workspace / todo / cwd / path` 阻断冲突写入
- `magi-worker-runtime`
  - 已补队列式 `WorkerRuntimeLoop`
  - 已可按 `execute / review / verify / repair / finish / fail` step 推进
  - 已接入治理结果消费，可显式表达 allow / needs approval / rejected / blocked / repair retry
  - 已补模拟外部执行器与 execution intent，Execute 主链可闭环驱动 builtin tool invocation、skill dispatch 和 final report
- `magi-orchestrator`
  - 已补显式 `OrchestratorControlPlane`
  - 已具备 `command enum / command result / command error`
  - 已补 mission / assignment / todo 三层治理摘要下钻
  - 已补 orchestrator execution runtime，可把 dispatch decision 推进到 execution intent 与 worker execute 主链
  - `magi-event-bus + magi-api`
  - runtime query contract 已冻结为 `meta / overview / details / operations / recovery`
  - 已具备 validation、freeze、freeze_gate、freeze_evidence、freeze_report、freeze_consistency、freeze_closure 全链路
  - 已补 audit / usage ledger 的导入、导出、文件落盘与上层接线
  - runtime read model 已补最小 ledger 状态，以及 governance blocked / approval required / rejected 的统一汇总
  - `meta.ledger` 已继续补齐 `is_persist_healthy / last_persisted_at / pending_flush`
  - daemon 启动恢复 ledger 后已发布 `system.ledger.ready`，运行期可稳定暴露 `persistence_path / last_persist_error / is_persist_healthy`
  - `magi-session-store + magi-workspace`
  - 已补 sidecar store 独立结构
  - 已补稳定 sidecar 导出视图，可统一导出 ownership / execution_chain_ref / recovery_ref / current_status / last_update
  - 已补 sidecar dirty 跟踪与统一 flush hook，可对刷新的 sidecar 做细粒度增量落盘
  - 已补 flush metadata，可稳定暴露 `last_dirty_reason / last_dirty_at / next_flush_hint / last_flush_at`

### 6.2 为什么还不能进入 M6

当前虽然已经具备进入 `M6` 评估前的核心基础，但还不满足统一切换条件，原因主要是：

1. host / model / MCP 已具备最小 transport client、loopback server 回环与 service catalog；host 已稳定暴露 `shell_manifest / session_descriptor / workspace_context`，且 `VSCode real-prehost` 已可基于本地文件系统返回真实前置结果；MCP 已具备最小 manager + 多 server + `enabled / health / tool_count` 目录语义，但真实 host / MCP 服务端、provider 适配与宿主壳仍未实现
2. builtin tool 的并发写防护虽然已落地，但更广的执行主链集成与更多写类工具仍未补完
3. worker loop 与 orchestrator control plane 已建立治理闭环，且本地子进程执行器已补 probe / health / capability、执行器身份/版本、`execution_mode / affinity / stage_matrix`、step 支持集与三层失败分层，但真实外部执行器和更完整的 repair / retry 策略仍未补完
4. audit / usage ledger 已进入上层接线，并具备 `system.ledger.ready` 运行期信号与显式 maintenance policy/config/state/report；`system.runtime.maintenance.status` 也已进入统一 runtime read model 的 `meta.maintenance`，但仍未进入最终切换链路与更大范围的运行期消费
5. session / workspace 的 sidecar store 与稳定导出面已补齐，且已支持增量 flush 与自动调度前置元数据，但恢复消费主链和更完整持久化仍未补完
6. 现有 TS 运行链路仍未进入接线或替换阶段
7. `IDEA host` 已明确延后到切换后阶段，不再作为本轮 `M6` 准入前提

### 6.3 当前建议

当前最合理的下一阶段不是讨论切换，而是：

1. 继续把 `M5` 从“已集中收口”推进到“更多能力域转为待验证”
2. 把 builtin tool、worker loop、orchestrator control plane 从“硬化完成”推进到“更完整执行语义可验证”
3. 继续补齐真实 bridge server / provider 接线、recovery 消费主链与 ledger 更大范围运行时消费
4. 在不接现有 TS 链路的前提下，先把 Rust 影子内核打磨到可直接进入 `M6` 评估
