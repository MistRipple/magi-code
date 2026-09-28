# 消息响应核心架构重构进度

> 本文只记录当前状态、可复核证据和剩余边界；不重新定义架构合同。
> 职责、事实源、事件与整体完成定义见[目标架构](conversation-response-core-architecture-redesign.md)；
> 轨迹、指标、预算和性能退出条件见[性能计划](conversation-response-performance-plan.md)。

## 1. 当前结论

截至 2026-09-28，HEAD 为 `8e7a8609ae00daa16f04f21ddfb62798dce10e9a`，工作区有 22 个变更路径；其中两个新增 Web 模型浏览器文档属于其他 Agent。按声明的最小场景集，A–G 均已完成逐项审计并关闭。这里的“关闭”只表示文档声明范围已有直接代码、测试或真实运行证据，不表示未经声明的笛卡尔积，也不把两层时钟相加为总延迟。

性能 evidence 在采样时绑定 source commit `8e7a8609…`、worktree fingerprint `4a2b3f4b…` 和 App artifact `78555943…`，采样内均稳定。采样后继续变化的是进度/计划文档；这些文档本身参与 dirty fingerprint，因此当前工作区 fingerprint 与 capture-time fingerprint 不同，不代表采样期间的实现变化。实现路径没有在该批采样后再次修改。

压缩专项的长历史本地准备 P95 为 `52,613 ms`，原因是该区间包含必要的摘要模型调用。它已按性能计划 `magi-performance-budget-applicability.v2` 记录为独立预算例外；该数值不被误报为普通 conversation 本地准备预算达标，也不宣称整体性能目标达标。

## 2. 工作包状态

| 包 | 状态 | 直接证据与边界 |
| --- | --- | --- |
| A 写入边界与测试夹具 | 已关闭（声明范围） | HTTP/App Server 共用 `TurnService`；Coordinator/Sink、幂等、序号和 `conversation` 资源隔离由 `magi-api` turn harness 与 conversation runtime 定向测试覆盖。 |
| B legacy/兼容语义 | 已关闭（声明范围） | 未发现可达的旧响应或终态双轨；迁移、恢复和协议兼容边界由 Rust、协议和 golden 测试覆盖。 |
| C 权限与真实副作用 | 已关闭（声明范围） | `/tmp/magi-permission-ledger-20260924-continuation-v2.json`：`magi.permission.v5`、254 行、0 validation errors；BrowserToolAccess、三种 AccessProfile、工具面、作用域、授权决定和副作用均有记录。 |
| D 真实 Provider 生命周期 | 已关闭（声明范围） | 当前 source 的正常、空流、idle-timeout、SSE/WebSocket 重连、daemon 重启恢复、Git blocked recovery、MCP、审批 recovery/cancel/expiry 均有通过的真实入口 evidence。专用重连/恢复 evidence 使用各自版本化 schema 与 request/facts/log，不被强行伪造成通用 trajectory；逐项 facts 审计见 §4.2。 |
| E 打包 Electron correctness | 已关闭（声明范围） | `/tmp/magi-electron-dom-regression-20260928-current-v3.json`：`186/186 passed`、IPC validation errors 为 0；对应 trajectory 109 条、0 errors，包含取消、重连、Desktop/daemon replay、Git、审批、幂等冲突和关闭 settlement。 |
| F Electron 端到端轨迹 | 已关闭（声明范围） | Renderer 五场景各 20 条，压缩长历史 20 条；普通/压缩 trajectory 分别 1,000/200 条、0 errors；异常与恢复由同一完整 DOM 回归及专用 Git/审批 evidence 覆盖。 |
| G before/after 性能与退出审计 | 已关闭（测量合同范围） | normal daemon/Electron、压缩 sidecar、comparison 和 envelope 均可复算并 `passed`；独立 clean baseline、input hash、source/App identity、异常 outcome、settlement、非加和时钟和压缩预算适用性版本均已审计。压缩长历史 P95 `52,613 ms` 被保留为独立专项观察，不宣称整体性能目标达标。 |

关闭范围仍受以下边界约束：未声明的全笛卡尔积、mock 或 timing-only 结果不被扩大解释；fault-injection 只证明声明的 transport 收口；Electron 与 daemon 的局部时钟不构成总延迟。

## 3. 证据索引

### 3.1 Rust、Web、权限和生命周期

- `cargo test --workspace --all-targets --quiet -- --test-threads=1`：`738 passed，1 ignored`，0 failed；其中 `magi-conversation-runtime` 为 `546 passed`，`magi-api` turn harness 为 `112 passed，1 ignored`。
- 当前代码验证：`cargo fmt --all -- --check`、`cargo check -p magi-daemon -p magi-api -p magi-conversation-runtime -p magi-bridge-client` 均通过。
- Web/协议验证：Desktop check、Browser Worker check、Web `svelte-check`（0 error/0 warning）、Web build、`npm run protocol:check`、`npm run browser-tool-catalog:check`、`npm test` 和全部 derive golden 均通过。Web build 的两个既有 Rollup `@__PURE__` 提示不影响退出码。
- 权限 evidence 为 254 行、0 errors；完整 Electron 回归与真实 Provider MCP/Git/审批 evidence 共同覆盖声明的真实副作用、明确拒绝、审批和终态关联。

### 3.2 D：真实 Provider 异常与恢复

以下文件均为当前采样批次 `source.fingerprintSha256=4a2b3f4b…`、`status=passed`；旧版本不进入当前统计：

| 切片 | evidence | 可核对事实 |
| --- | --- | --- |
| 空流、idle-timeout | `/tmp/magi-real-provider-empty-stream-20260928-current-v5.json`、`/tmp/magi-real-provider-idle-timeout-20260928-current-v3.json` | 各 4 条异常样本；各自 trajectory 16 条、0 errors；异常 outcome 和 terminal 收口通过。 |
| SSE/WebSocket 重连 | `/tmp/magi-real-provider-reconnect-20260928-current-v5.json`、`/tmp/magi-real-provider-websocket-reconnect-20260928-current-v5.json` | Provider request 1 次、canonical terminal 1 个、用户消息 1 条、首连接在 terminal 前断开、重连 terminal `completed`、序号递增。 |
| daemon 重启恢复 | `/tmp/magi-real-provider-restart-reconnect-20260928-current-v5.json`、`/tmp/magi-real-provider-restart-websocket-20260928-current-v5.json` | 首 daemon request 1 次、第二 daemon request 0 次、canonical terminal 总数 1、runtime epoch 改变、Session/Workspace identity 稳定；WebSocket 路径使用 `events/resyncRequired`。 |
| Git blocked recovery | `/tmp/magi-real-provider-blocked-recovery-20260928-current-v5.json` | blocked request 未 dispatch、blocked status `failed`、恢复 Turn `completed`、三条匹配用户消息各 1 条、Session identity 稳定。 |
| MCP 外部副作用 | `/tmp/magi-real-provider-mcp-external-20260928-current-v5.json` | MCP server connected、tool call 真实发生、文件副作用存在、唯一 completed terminal；样本 settlement 已观察。 |
| 审批拒绝后恢复 | `/tmp/magi-real-provider-approval-recovery-20260928-current-v5.json` | 拒绝 Turn 无副作用且唯一 failed terminal；允许恢复 Turn 有副作用且 completed；两轮均有 settlement。 |
| 审批取消后恢复 | `/tmp/magi-real-provider-approval-cancel-20260928-current-v5.json` | 唯一 cancelled terminal、无错误伪造和无副作用；取消 terminal 先于恢复接纳，恢复 Turn completed，settlement 已观察。 |
| 审批过期后恢复 | `/tmp/magi-real-provider-approval-expiry-20260928-current-v4.json` | `tool_approval_expired`、唯一 failed terminal、无副作用；恢复 Turn completed，settlement 已观察。 |

这些专用 evidence 的 verifier schema 与原始 daemon log 是其事实来源；通用 trajectory derive 不识别它们的顶层 facts schema，因此不把空 ledger 误报为通过，也不把专用 facts 扩大为完整矩阵。

### 3.3 E/F：打包 Electron

| 切片 | evidence | 统计 |
| --- | --- | --- |
| 完整 DOM 回归 | `/tmp/magi-electron-dom-regression-20260928-current-v3.json` | 186/186 checks passed；`/tmp/magi-trajectory-ledger-electron-regression-20260928-current-v3.json`：109 条、0 errors。 |
| 普通 Renderer 五场景 | `/tmp/magi-electron-dom-renderer-after-20260928-v4.json`、`/tmp/magi-electron-dom-renderer-comparison-20260928-v4.json` | 100 条、五类各 20；280 次 Provider request；trajectory 1,000 条、0 errors；DOM paint 最大 P95 `47 ms`。 |
| 上下文压缩 | `/tmp/magi-electron-context-compaction-after-20260928-v3.json`、`/tmp/magi-performance-comparison-electron-context-compaction-20260928-v4.json` | 长历史 20/20 completed、IPC errors 0；trajectory 200 条、0 errors；DOM paint P95 `45 ms`。 |
| Git/审批专用边界 | `/tmp/magi-electron-dom-git-preflight-20260928-current-v2.json`、`/tmp/magi-electron-dom-approval-expiry-20260928-current-v3.json` | Git 29/29、审批过期 19/19；后者唯一 failed terminal、无副作用、恢复 Turn completed。 |

### 3.4 G：before/after 与可复算统计

| 层 | 原始/派生 evidence | 统计与解释 |
| --- | --- | --- |
| daemon normal | raw `/tmp/magi-real-provider-performance-after-20260928-v11.json`；sidecar `/tmp/magi-performance-metrics-after-20260928-v11.json`；comparison `/tmp/magi-performance-comparison-20260928-v11.json`；audit trajectory `/tmp/magi-trajectory-ledger-real-provider-after-20260928-audit-v1.json` | 100 条、五类各 20、trajectory 600 条、0 errors；conversation accepted→provider request 最大 P95 `24 ms`，task `260 ms`，可计算 Magi 首内容开销最大 P95 `260 ms`。 |
| daemon compression | raw `/tmp/magi-real-provider-performance-compression-after-20260928-v9.json`；sidecar `/tmp/magi-performance-metrics-context-compaction-after-20260928-v9.json`；comparison `/tmp/magi-performance-comparison-context-compaction-20260928-v9.json`；audit trajectory `/tmp/magi-trajectory-ledger-real-provider-compression-after-20260928-audit-v1.json` | 40 条（新建/长历史各 20），长历史 `completed=19 / skipped=1`，trajectory 240 条、0 errors；长历史本地准备 P95 `52,613 ms`，按 `magi-performance-budget-applicability.v2` 单独报告。 |
| Electron normal | after `/tmp/magi-electron-dom-renderer-after-20260928-v4.json`；comparison `/tmp/magi-electron-dom-renderer-comparison-20260928-v4.json`；audit trajectory `/tmp/magi-trajectory-ledger-electron-renderer-after-20260928-audit-v1.json` | 100 条、五类各 20、trajectory 1,000 条、0 errors；Renderer 与 daemon 使用分离时钟。 |
| Electron compression | after `/tmp/magi-electron-context-compaction-after-20260928-v3.json`；sidecar `/tmp/magi-performance-metrics-electron-context-compaction-after-20260928-v3.json`；comparison `/tmp/magi-performance-comparison-electron-context-compaction-20260928-v4.json`；audit trajectory `/tmp/magi-trajectory-ledger-electron-context-compaction-after-20260928-audit-v1.json` | 20 条长历史 Turn，trajectory 200 条、0 errors；before/after source identity distinct；Renderer DOM paint P95 `45 ms`。 |
| 分层 envelope | `/tmp/magi-performance-envelope-20260928-v4.json` | `passed`；input hash、source identity、scenario mapping 和 separate non-additive clocks 均保留；`total_latency_defined=false`。 |

所有 comparison 使用独立 clean baseline `b2cee410…` 与当前 after 的 source/App identity；可报告声明 fixture 的 before/after 观察，不能把未配对场景或 dirty artifact 差异扩大解释为完整性能收益。

## 4. 整体完成定义逐项审计

| 目标架构 §7.1 | 审计结论 | 证据 |
| --- | --- | --- |
| 1. `conversation` 不创建 TaskRun 等 task 资源 | 通过声明范围审计 | conversation runtime 的 profile/resource isolation 测试、turn harness。 |
| 2. 单 Coordinator 与统一 TurnService | 通过声明范围审计 | `magi-api` turn harness、HTTP/App Server 入口测试。 |
| 3. Canonical Turn Log/TaskStore 唯一事实源 | 通过声明范围审计 | Sink、TaskStore、projection 与 writeback 定向测试；无独立 projection 写入口。 |
| 4. accepted/delta/终态/cancel/reconnect/restart | 通过声明范围审计 | normal trajectory、D recovery evidence、Electron DOM regression；唯一 terminal 与序号规则均通过。 |
| 5. durable Task commit + notifier、迟到结果拒绝 | 通过声明范围审计 | task dispatcher、session finalizer、late callback 和 settlement 测试。 |
| 6. 有界 delta 写回、终态和恢复一致 | 通过声明范围审计 | session writeback/runtime tests、normal/compression trajectory、Electron replay checks。 |
| 7. 权限/Git/MCP/Browser/process/Agent 副作用边界 | 通过声明范围审计 | permission ledger 254 行、Git 29/29、审批 19/19、MCP external、Electron regression。 |
| 8. Rust/Web/daemon/Electron/real Provider 最小场景集 | 通过声明范围审计 | §3.1–§3.3 的真实入口与定向回归；不扩大为未声明组合。 |
| 9. before/after、硬门槛、预算解释 | 通过声明范围审计 | §3.4 comparison/envelope 和普通预算通过；压缩例外已由 `magi-performance-budget-applicability.v2` 版本化记录适用范围、原因和独立报告规则，不静默删除实际 P95，也不宣称整体性能目标达标。 |
| 10. 不可达旧双轨、同步提交和轮询终态 | 通过声明范围审计 | legacy/compatibility directed tests、协议 golden 和 runtime finalizer tests。 |

## 5. 验证记录与后续边界

本轮已完成最终审计；不再有关闭前的验证工作。后续如果要优化压缩等待，可另开性能优化工作包，不能把它作为本轮已达标结果。

本轮最终执行并通过：

```text
cargo fmt --all -- --check
cargo check -p magi-daemon -p magi-api -p magi-conversation-runtime -p magi-bridge-client
cargo test --workspace --all-targets --quiet -- --test-threads=1
npm test
npm run test:trajectory-ledger
npm run test:permission-ledger
npm run test:electron-renderer-comparison
npm run test:context-compaction-metrics
npm run test:electron-context-compaction-metrics
npm run test:performance-envelope
npm run protocol:check
npm run browser-tool-catalog:check
npm run check --workspace @magi/desktop
npm run check --workspace @magi/browser-automation-worker
npm --prefix web run check
npm --prefix web run build
git diff --check
```

构建只有既有 Rollup 注释 warning，无 error；没有运行中的验证进程，也没有终止其他 Agent 的 daemon（PID `33434` 仍由其他 Agent 使用）。失败或 source 不稳定的旧 artifact 保留作诊断，不进入通过统计。

后续只有在实现路径、协议/derive 合同、工具面或声明场景集改变时才需要重开相应包并重新采样；其他 Agent 的无关修改不应被覆盖、暂存或提交。
