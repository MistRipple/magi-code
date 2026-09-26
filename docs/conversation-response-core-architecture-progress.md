# 消息响应核心架构重构进度

> 本文只记录当前状态、可复核证据和剩余工作；不重新定义架构边界、事件合同或性能口径。
> 架构合同见[目标架构](conversation-response-core-architecture-redesign.md)，轨迹、指标和性能退出条件见[性能计划](conversation-response-performance-plan.md)。

当前快照：2026-09-26，HEAD 为 `ca132230a067130f59b57040870f1667bb7a385f`。工作区包含其他 Agent 的未提交修改；下列结论只适用于声明的切片，不代表干净基线、完整笛卡尔积或整体完成。

## 1. 结论先行

A–F 的声明切片已有直接证据；G 已有可复算的 daemon/Electron 分层 before/after 观察，但仍保持“部分完成”。Electron 压缩现在已有同口径 paired comparison：before/after 各 20 条真实长历史 Turn，并通过同一 Session、SSE 压缩事实、同一 Turn 的 backend/Renderer 阶段和终态序号校验；两端仍来自同一 dirty worktree，因此不能解释为因果性能收益。

当前不得宣称：

- 整体架构或性能目标完成；
- daemon 与 Electron 两组时钟可相加为总延迟；
- dirty worktree 上不同 artifact 的差异就是因果性能收益；
- 任一代表性、mock、timing-only 或 fault-injection 结果覆盖未声明组合。

## 2. 文档分工与状态口径

| 文档 | 只维护什么 |
| --- | --- |
| 目标架构 | 职责边界、事实源、事件/持久化合同、声明场景和整体完成定义。 |
| 本文 | A–G 状态、最新证据、验证结果和剩余缺口。 |
| 性能计划 | 轨迹 schema、阶段、指标、采样、候选预算和性能退出条件。 |

状态只表示对应工作包的声明切片：`已关闭` 表示该切片有直接证据，不表示所有组合；`部分完成` 表示仍缺退出条件；`诊断` 不计入通过统计。

缺少 `session_id`、`turn_id`、`request_id`、唯一终态、source identity，或该切片要求的 `input_hash`/`settlement` 时，证据只能作诊断。旧 artifact 不删除，但不自动进入当前统计。

## 3. 工作包总览

| 包 | 状态 | 已证明 | 关闭前剩余工作 |
| --- | --- | --- | --- |
| A 写入边界与测试夹具 | 已关闭 | HTTP/App Server 共用 `TurnService`；Coordinator/Sink、幂等、序号和 `conversation` 资源隔离有定向测试。 | 相关生产写入路径改变时重开。 |
| B legacy/兼容语义 | 已关闭 | 未发现可达的旧响应或终态双轨；保留的迁移、恢复和协议兼容语义有测试边界。 | 发现可达双轨或无边界兼容写路径时重开。 |
| C 权限与真实副作用 | 已关闭（声明范围） | permission ledger 254 行通过，含三种 AccessProfile、BrowserToolAccess、工具面、作用域、允许/拒绝和副作用事实。 | 新增策略、surface、scope，或授权与副作用断裂时重开。 |
| D 真实 Provider 生命周期 | 已关闭（声明范围） | 正常五类 × 20 轮、failure/empty/idle-timeout、重连、Git 恢复、审批和一个外部 MCP 写入切片均有 derive.v5 或对应真实 evidence。 | 仅在新增 Provider 风险行、终态合同或实现变化时重开；不代表未声明组合。 |
| E 打包 Electron correctness | 已关闭（声明范围） | 高风险 GUI evidence `186/186`，包含真实关闭 settlement。 | 新增 GUI/IPC 场景或终态实现变化时重开。 |
| F Electron 端到端轨迹 | 已关闭（声明范围） | 正常五类 × 20 轮、声明异常和四类 recovery slice 均有同一 Turn 的后端与 Renderer 阶段。 | 新增恢复载体、GUI/IPC 场景或终态实现变化时重开。 |
| G before/after 性能 | 部分完成 | daemon before-v4/after-v4、Electron paired-v5、分层 envelope 以及 Electron 压缩 paired-v2 均可复算，候选预算在声明切片内满足。 | 仍缺独立源码/可执行基线的因果归因，以及逐项退出审计。 |

当前推进顺序只有 G。A–F 的声明范围不因 G 的缺口扩大，也不因已有正常样本而宣称整体完成。

## 4. 当前证据索引

### 4.1 代码、权限和生命周期

| 切片 | 证据 | 边界 |
| --- | --- | --- |
| A/B 入口和终态 | `cargo test -p magi-api --lib turn_harness::tests -- --test-threads=1`：`112 passed, 1 ignored`；conversation runtime 的 `conversation_loop`、`task_execution_dispatcher`、`session_turn_finalize`、`session_writeback` 分别为 `55`、`56`、`9`、`34 passed`。 | 覆盖真实 TurnService harness、资源隔离、幂等冲突、取消/重连/终态边界；不等于全部历史恢复矩阵。 |
| C 权限 | `/tmp/magi-permission-ledger-20260924-continuation-v2.json`：`passed`，`magi.permission.v5` / `magi-permission-derive.v8`，254 行；`npm run test:permission-ledger` 通过。 | 关闭声明的权限最小集；`not_required` 不等于发生了审批通过。 |
| 真实恢复/审批 | SSE/WebSocket 重连、Git blocked recovery、审批 recovery/expiry/cancel 和外部 MCP evidence 均保留 request、terminal、必要时 settlement；对应 ledger 均 `passed`。 | 关闭各自声明切片，不扩展为完整恢复或工具组合矩阵。 |

### 4.2 真实 Provider（D）

| 切片 | 最新证据与统计 | 边界 |
| --- | --- | --- |
| 正常五类 × 20 | `/tmp/magi-trajectory-ledger-real-provider-normal-5x20-20260925-v5-recheck.json`：`passed`，600 条记录；五类各 20，accepted/provider/raw/visible/event_bus/terminal 各 100。 | 只证明当前 daemon/Provider 阶段；不证明 Electron Renderer 或性能因果。 |
| 短历史 20 | `/tmp/magi-real-provider-performance-short-history-20260926-v2.json` 与 `/tmp/magi-trajectory-ledger-real-provider-short-history-20260926-v2.json`：分别 `passed`、20 个独立 Session、120 条记录、0 validation errors。 | 关闭短历史 correctness 和专项观察；没有标准 paired sidecar。 |
| 压缩 correctness | `/tmp/magi-real-provider-performance-compression-20260926-v5.json` 与 `/tmp/magi-trajectory-ledger-real-provider-compression-20260926-v5.json`：40 个 Turn 全部完成，长历史 `completed=19 / skipped=1`，ledger 240 条、0 errors。 | 关闭 daemon 压缩状态收口；after-only sidecar 不构成 before/after。 |
| 异常、重连、Git、审批、MCP | 六个 failure/empty/timeout ledger、SSE/WebSocket reconnect、Git recovery、approval recovery/expiry/cancel 和 external MCP evidence 均有通过记录。 | fault-injection 只证明 transport 收口；单个 MCP/审批风险行不代表完整矩阵。 |

### 4.3 打包 Electron（E/F）

| 切片 | 最新证据与统计 | 边界 |
| --- | --- | --- |
| 高风险 GUI | `/tmp/magi-electron-dom-regression-20260925-highrisk-v9.json`：`passed`、`186/186`；对应 ledger 109 条记录。 | 关闭声明的 GUI 风险切片；其中 restart/reload 是历史恢复断言，不等于同一 Turn recovery phases。 |
| 正常与异常 | `/tmp/magi-electron-dom-renderer-after-20260926-v5.json`：`passed`、100 个 Turn、五类各 20、280 次 Provider request；对应 ledger 1,000 条记录、10 个阶段各 100。异常另由 high-risk/approval-expiry evidence 覆盖。 | 关闭声明的正常/异常子项，不扩展为完整 Electron 笛卡尔积。 |
| recovery | `/tmp/magi-electron-dom-recovery-20260925-v8.json`：`passed`、44 项检查、4 个唯一 Turn；重派生 ledger 38 条、0 errors。 | 覆盖 SSE/WebSocket cancel 和 daemon/Desktop replay；replay 未新增 Provider request。 |
| 压缩 paired-v2 | before evidence `/tmp/magi-electron-context-compaction-before-20260926-v2.json`、after evidence `/tmp/magi-electron-context-compaction-after-20260926-v4.json`：均 `passed`、各 20/20 长历史 Turn `completed`、IPC errors `0`；before/after sidecar 分别为 `/tmp/magi-performance-metrics-electron-context-compaction-before-20260926-v1.json`、`/tmp/magi-performance-metrics-electron-context-compaction-after-20260926-v4.json`；paired comparison `/tmp/magi-performance-comparison-electron-context-compaction-20260926-v2.json`：`passed`；trajectory 分别为 `/tmp/magi-trajectory-ledger-electron-context-compaction-before-20260926-v1.json`、`/tmp/magi-trajectory-ledger-electron-context-compaction-after-20260926-v5.json`，各 200 条、10 个阶段各 20、0 errors。 | 同一 fixture、窗口 `16000`、长历史 `4000`、sampleCount `20`、source commit/worktree/executable 和非加和时钟均校验通过；App artifact 不同。before `accepted→terminal` P95 `139 ms`、`dom_painted` P95 `42.5 ms`，after 分别为 `145 ms`、`31.4 ms`；同一 dirty worktree 仍只能作为可复算 paired 观察，不能宣称因果收益。 |
| Electron before 能力探针（诊断） | 使用独立打包产物 `/private/tmp/magi-electron-package-original-before-current/electron-dist/mac-arm64/Magi.app`（App artifact SHA-256 `b4964bda4113a0297752d0077bea0874e7dc13d0839991e810652650bd6de6e4`，内置 daemon SHA-256 `4f4a76fc715cbd75959908f3351991ac0b84b5dd35ae83e49b7f19856f24b252`）按同一 20 条入口尝试；运行日志 `/tmp/magi-electron-context-compaction-before-20260926-v1.run.log`。旧产物缺少 `provider_first_raw_delta`，验证器在第 0 条样本的 backend timing 关联处超时并退出 1，没有生成通过的 evidence。 | 即使旧 daemon 日志出现 context checkpoint，也不能补造当前 trajectory 所需的 raw 阶段、同 Turn 身份或压缩终态；不计入 before/after，继续保持 G 未关闭。 |

Renderer 使用页面局部 `elapsedMs`，后端使用 `sinceAcceptedMs`；两者不可相加。Electron 压缩样本的 source 在采样内稳定：HEAD 为 `ca132230...`，worktree fingerprint `65fa73e878492fbaf3d9e6769b4294daf01e1f9d60f52320d45492403f6e73cc`，App artifact SHA-256 为 `eee8dd9c1918f6a1161807d31e61b8cd3b26eeec57cdac1963f51f7e79a3a61d`。

### 4.4 G 的普通 before/after

| 层 | 证据 | 当前解释 |
| --- | --- | --- |
| daemon | before-v4/after-v4 evidence、各自 trajectory、`magi.performance.v1` sidecar 和 `/tmp/magi-performance-comparison-20260926-v2.json` 均 `passed`；五类各 20。 | 可复算 accepted、准备、Provider TTFT、Magi overhead 和 terminal；after task/subagent overhead 最高 P95 `297 ms`，不等于因果收益。 |
| Electron Renderer | before/after-v5、各自 trajectory、`/tmp/magi-electron-dom-renderer-comparison-20260926-v2.json` 均 `passed`；各 100 Turn、各 1,000 条记录。 | DOM paint after 最大 P95 `28.3 ms`，低于候选 `50 ms`；场景方向不一致，不能宣称整体收益。 |
| 分层关系 | `/tmp/magi-performance-envelope-20260926-v1.json`：`passed`。 | 明确 daemon `personal_long_history` 与 Electron `goal` 未配对；只绑定输入 hash、source identity 和非加和时钟。 |

普通 daemon/Electron before/after 仍来自 dirty worktree 的不同 artifact。它们证明可复算和预算观察，不单独证明重构带来的因果性能提升。

## 5. G 剩余工作与最终审计

必须按以下顺序推进：

1. 取得能产生 `context_compaction=completed` 或合法 `skipped` 的独立 daemon before；旧可执行文件探针 `/tmp/magi-real-provider-performance-compression-before-probe-20260926-v1.json` 已按合同 `failed`，长历史只有 `running/running/skipped`。新建隔离 HEAD baseline `/private/tmp/magi-baseline-ca132` 后，release daemon 构建成功；其 source fingerprint 为 `f0b8bb239aba527b842c0ca42a04d698bab7e83204844bf7e19d4bacb34fd158`，daemon SHA-256 为 `6b4b0135f265a6ee640250cbc87b8034a55eeb13851a7706db6f9cca5a8c8fdd`，探针 `/tmp/magi-real-provider-performance-compression-before-probe-20260926-v2.json` 仍 `failed`：长历史 Turn `completed`，但压缩只有 `running/running/skipped`，`completed=0 / skipped=1`。因此当前 HEAD baseline 可证明独立构建身份，却不能作为当前压缩合同的通过 before。
2. 为 Electron 压缩 paired-v2 补充独立源码或可执行文件身份的 baseline；当前 before/after 虽有不同 App artifact 且 paired derive 通过，但 source commit、worktree fingerprint 和 daemon executable 相同，只能证明可复算、预算观察和采样稳定性。现有更旧产物探针因缺少 `provider_first_raw_delta` 已 fail-closed。
3. 对 G 逐项复核：终态唯一性、内容恢复、幂等/隔离、取消 settlement、恢复、权限/Git/审批/真实副作用、source/input hash、异常 outcome、候选预算和统计口径。
4. 若基线能力不足，记录失败原因和版本化边界，不用 `running` 代替压缩终态，不补造阶段，不把局部证据改写为整体完成。

目标架构 §7.1 的场景硬门槛中，第 8 项仍为“部分完成”，第 9 项的压缩专项 paired 证据已具备但独立 baseline 仍不足；在 A–G 和硬门槛逐项关闭前，不得宣称整体完成或调用 `update_goal(status="complete")`。

## 6. 验证记录与维护

本轮实际新增的 Electron 压缩复采命令为：

```bash
MAGI_ELECTRON_DOM_CDP_PORT=10257 \
MAGI_ELECTRON_DOM_COMPACTION_ONLY=1 \
MAGI_ELECTRON_DOM_COMPACTION_SAMPLES=20 \
MAGI_ELECTRON_DOM_COMPACTION_PREFILL_TURNS=12 \
MAGI_ELECTRON_DOM_EVIDENCE_PATH=/tmp/magi-electron-context-compaction-after-20260926-v4.json \
npm run test:electron-conversation-dom

node scripts/derive-electron-context-compaction-metrics.mjs \
  --role after \
  --input /tmp/magi-electron-context-compaction-after-20260926-v4.json \
  --output /tmp/magi-performance-metrics-electron-context-compaction-after-20260926-v4.json
node scripts/derive-magi-trajectory-ledger.mjs \
  --output /tmp/magi-trajectory-ledger-electron-context-compaction-after-20260926-v5.json \
  /tmp/magi-electron-context-compaction-after-20260926-v4.json
node scripts/derive-electron-context-compaction-metrics.mjs \
  --compare /tmp/magi-performance-metrics-electron-context-compaction-before-20260926-v1.json \
  /tmp/magi-performance-metrics-electron-context-compaction-after-20260926-v4.json \
  --output /tmp/magi-performance-comparison-electron-context-compaction-20260926-v2.json
```

本轮 before/after 复采与全部派生命令均为 `passed`；两端 evidence 各 261 项检查，sidecar 各 20 条 backend/Renderer metric，trajectory 各 200 条记录、0 validation errors，paired comparison `passed`。运行日志保留在 `/tmp/magi-electron-context-compaction-before-20260926-v2.run.log` 和 `/tmp/magi-electron-context-compaction-after-20260926-v4.run.log`，不作为新的事实源。

独立 baseline 构建验证（隔离 worktree `/private/tmp/magi-baseline-ca132`）为：`cargo fmt --all -- --check`、`cargo build --release -p magi-daemon-app` 和 `npm run desktop:package -- --dir` 均退出码 0；目录包 App artifact SHA-256 为 `5da206e08051076083a2d07ab23feb902e6670d8961644e854a53cf3114082db`。该 artifact 只证明可独立构建和身份稳定，不因压缩 probe fail 而进入性能统计。

当前已记录的通用验证包括：`cargo test --workspace --all-targets --quiet -- --test-threads=1` 退出码 0（无失败，保留少量 ignored）；`npm test` 退出码 0；`npm run protocol:check`；`npm --prefix web run check` 为 0 error/0 warning；`npm --prefix web run build` 和 Electron Browser permission matrix 通过；trajectory/permission/renderer/envelope/context-compaction golden tests 通过。Web build 仅有 Rollup `@__PURE__` 注释提示，不影响退出结果，也不作为架构证据。代码、schema、脚本或 derive 合同改变后，旧 evidence 只能作诊断；重新采样必须使用新的 run-id、空闲端口并保留失败 artifact。
