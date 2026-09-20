# 消息响应核心架构重构进度

更新时间：2026-09-20
代码基线：`987f3c3df42f612add074c734eb32d6892dfa144`
对应方案：[conversation-response-core-architecture-redesign.md](/Users/xie/code/magi-rust-rewrite/docs/conversation-response-core-architecture-redesign.md)

本文只记录尚未满足完成定义的工作包、直接证据和推进顺序。完成一个工作包前，必须同时更新状态、证据路径和验证命令；没有直接证据的内容保持未完成。

## 总览

按方案文档的勾选项统计，当前 57 项中 51 项已完成、6 项未完成。`17.3` 在两个章节重复出现，去重后形成 5 个顶层门槛；按可独立验收的工程工作包拆分，剩余 7 项：

| 编号 | 工作包 | 状态 | 关闭条件 |
| --- | --- | --- | --- |
| A | 17.3：current Turn 写入口和测试夹具边界 | 进行中 | 生产路径只经 `CanonicalTurnEventSink`；剩余底层 fixture 已逐项分类，能迁移的已迁移，保留项有精确理由和测试覆盖 |
| B | 17.7：失效 legacy/兼容语义清理 | 进行中（首轮审计完成；未发现可直接删除的生产双轨） | 失效生产双轨、旧注释和无效 fixture 清除；迁移、恢复、协议兼容、旧字段拒绝逻辑保留并有边界说明 |
| C | 完整权限组合矩阵 | 进行中（基础 runtime/API 矩阵已跑通；组合关联未完成） | ReadOnly/Restricted/FullAccess 与 file/shell/process/Git/browser/MCP、workspace 内外、审批生命周期和真实副作用均有证据 |
| D | 真实 Provider 全矩阵 | 进行中（五类后端 20 轮及基础 cancel/reconnect 已有；全矩阵未完成） | Chat/Task/Goal/工具/子代理覆盖 cancel、reconnect、restart、history/replay、权限、Git 冲突和审批阻塞恢复 |
| E | Electron packaged GUI 全矩阵 | 部分完成 | 在现有 55 项单轮基础上补齐 Agent drawer、Goal/Plan 组合、Git/审批错误和 reconnect/history/restart 组合 |
| F | Electron 五类场景各 20 轮端到端 timing | 进行中（Renderer 五类 20/20；后端同轮关联未完成） | 5 类 × 20 轮均有同轮 accepted、Provider 首 delta、首 EventBus、Renderer 四阶段和 terminal 关联 |
| G | 性能 before/after 对比 | 待开始 | 有可审计的旧版本 before 数据，并与当前 after 数据使用同一场景、同一指标和同一统计方法 |

## 已有直接证据

- Rust workspace：`cargo test --workspace --all-targets --quiet -- --test-threads=1`，669 passed、1 ignored。
- Web/npm：protocol check、Svelte check/build、npm golden 已通过。
- Electron 单轮回归：`/tmp/magi-electron-dom-regression-10027.json`，55 项检查通过；此前 `/tmp/magi-electron-dom-regression-9983.json` 同样通过。
- Electron 五类单轮 Renderer timing：`/tmp/magi-electron-dom-timing-9857.json`，覆盖 `personal_chat`、`workspace_chat`、`workspace_tool`、`goal`、`subagent`，每类已有单轮四阶段 timing。
- Electron 五类各 2 轮 timing：`/tmp/magi-electron-dom-timing-2-9999d.json`，`status=passed`、43 项脚本检查通过、28 次 Provider 请求；每类 2 条记录均通过 Renderer 四阶段和 terminal 收口检查。该证据仍未包含同轮后端 accepted/首 delta/首 EventBus/terminal 的结构化关联。
- Electron 五类各 20 轮 Renderer timing 已分别完成，聚合统计：`/tmp/magi-electron-dom-timing-20-summary-10020-10026.json`。原始证据为：
  - `personal_chat`：`/tmp/magi-electron-dom-timing-personal-20-10020.json`；20 条唯一 `turnId`，20 次 Provider 请求；`dom_painted` P50/P95 为 15.5/23.8ms。
  - `workspace_chat`：`/tmp/magi-electron-dom-timing-workspace-chat-20-10021.json`；20 条唯一 `turnId`，20 次 Provider 请求；`dom_painted` P50/P95 为 7.8/20.4ms。
  - `workspace_tool`：`/tmp/magi-electron-dom-timing-workspace-tool-20-10022.json`；20 条唯一 `turnId`，40 次 Provider 请求；`dom_painted` P50/P95 为 11.4/19.3ms。
  - `goal`：`/tmp/magi-electron-dom-timing-goal-20-10025.json`；20 条唯一 `turnId`，120 次 Provider 请求；`dom_painted` P50/P95 为 13.1/27.3ms。
  - `subagent`：`/tmp/magi-electron-dom-timing-subagent-20-10026.json`；20 条唯一 `turnId`，80 次 Provider 请求；`dom_painted` P50/P95 为 9.0/29.6ms。
  以上 P50/P95 是 Renderer timing registry 中各阶段首条记录的 `elapsedMs`，只表示前端事件到对应阶段的局部耗时，不等价于完整 UI 呈现指标，也没有补齐同轮后端 accepted、Provider 首 delta、首 EventBus 和 terminal 关联。
- 真实 Provider 后端五类场景各 20 轮：`/tmp/magi-real-provider-perf-personal20.json`、`/tmp/magi-real-provider-perf-workspace20.json`、`/tmp/magi-real-provider-perf-tool20.json`、`/tmp/magi-real-provider-perf-subagent20.json`。
- 历史首次 20 轮采样 `/tmp/magi-electron-dom-timing-20-9998.json` 未生成完整证据：曾在 Goal 第 12 轮因单一 Session 的上下文增长触发语义压缩而停止；该文件只作为失败记录保留，不能作为完成证据。
- 上述首次采样问题已通过脚本场景筛选和 Goal/子代理每轮切换独立个人草稿解决；新的五类 20 轮证据仍只关闭 Renderer timing 子项。
- before 版本性能数据当前不存在，因此 G 保持未完成。

## 推进顺序

1. 先完成 A，收紧剩余测试专用 current Turn 边界并记录不可迁移的底层 canonical fixture。
2. 再完成 B，逐项处理明确失效的 legacy/兼容语义，不触碰真实迁移和恢复职责。
3. 依次补齐 C、D、E，所有矩阵都必须包含真实副作用或明确的拒绝证据。
4. 完成 F 的五类 × 20 轮同轮关联，随后再处理 G 的 before/after 对比。

## A 工作包首轮审计

截至基线提交：

- 生产代码没有 `upsert_current_turn(...)`、`TaskResultReceiver`、`uses_active_completion_sink`、生产 `poll_results(...)` 或旧 `run_dispatch_submission(...)`。
- `CanonicalTurnEventSink` 内部仍调用 SessionStore 的 canonical mutation，这是允许的内部边界。
- `magi-session-store/src/store/tests.rs` 仍有 19 处 `update_current_turn_status_for_turn(...)` 和 16 处 `upsert_current_turn_item_for_turn(...)` 测试调用；前者仅是测试便利包装，后者同时被 Sink 的生产写回使用，不能直接删除。
- 首轮已将仅测试使用的 `update_current_turn_status_for_turn` 包装收敛为 `#[cfg(test)] pub(crate) set_current_turn_status_for_test`；生产编译不再暴露这个 current Turn 写入口。
- `upsert_current_turn_item_for_turn` 仍被 `CanonicalTurnEventSink` 的生产写回使用，不能按测试入口删除；下一步逐个判断 16 处 storage fixture 是否能改用接纳、追加或 Sink 测试辅助。

这 16 处调用目前按职责分为：

- 1 处 Continue 前的活动工具项恢复夹具；
- 2 处 assistant stream -> final 的 canonical item 状态迁移验收；
- 1 处 durable canonical projection 写回验收；
- 1 处 request ID 元数据持久化验收；
- 2 处内部 `agent_wait` / `agent_spawn` item 投影验收；
- 4 处 blocked/killed/terminal item 收口验收；
- 5 处 stale owner、不可变字段和状态回退拒绝验收。

这些测试直接验证 SessionStore canonical mutation 的原子性、版本和冲突规则，不能改成只验证上层 Coordinator 的黑盒流程；它们的合法底层边界会保留在 A 的关闭记录中。

## 变更记录

| 日期 | 工作包 | 进展 | 验证 |
| --- | --- | --- | --- |
| 2026-09-20 | F/E | 形成 55 项 Electron 单轮 DOM 回归和五类单轮 Renderer timing | `npm run test:electron-conversation-dom`；证据见上 |
| 2026-09-20 | A | 完成生产 current Turn 写入口审计；将仅测试使用的状态包装改为测试专用命名和可见性 | `cargo fmt --all -- --check`；`cargo test -p magi-session-store --lib -- --test-threads=1`：121 passed |
| 2026-09-20 | F | 完成五类 timing 各 2 轮，并新增每轮 terminal 收口门槛；20 轮采样在 Goal 第 12 轮暴露上下文隔离问题 | `/tmp/magi-electron-dom-timing-2-9999d.json`：5 类 × 2，43 checks passed；`/tmp/magi-electron-dom-timing-20-9998.json` 未形成完整证据 |
| 2026-09-20 | F | 通过 `MAGI_ELECTRON_DOM_TIMING_SCENARIO` 按场景隔离采样，并对 Goal/子代理轮次切换到独立个人草稿；五类 Renderer timing 各完成 20 轮 | `/tmp/magi-electron-dom-timing-20-summary-10020-10026.json`；5 类各 20 条唯一 `turnId`，每条均有四个 Renderer 阶段和 terminal 收口 |
| 2026-09-20 | C | 跑通权限 runtime/API 基础矩阵并记录组合缺口 | `magi-api turn_harness`：48 passed、1 ignored；`magi-tool-runtime`：225 passed、1 ignored |

## B 工作包首轮审计

当前检索到的 `legacy` / `fallback` 语义按职责分为：

- `magi-session-store` 的 v1 -> v2 转换、旧 sidecar/thread projection 重建和 `legacyMigration` 标记：属于一次性迁移输入，保留。
- `magi-daemon` 的旧目录读取、归档、重引入隔离和损坏状态拒绝：属于恢复与数据安全边界，保留。
- MCP/Browser 的 `fallback-only`、`available_fallback` 和协议兼容状态：属于对外状态合同，保留。
- `agent_runs.fallback_mode`、`openai-compatible` Provider 名称以及旧字段拒绝测试：属于现有协议或兼容性验证，保留。
- daemon 路由 `.fallback(get(...))` 和配置缺失时返回 unavailable：属于 HTTP 路由/配置错误边界，不是消息响应双实现。

首轮审计尚未发现可以直接删除的生产双轨路径；B 仍保持未完成，下一步需要把每个保留项绑定到具体测试，并继续寻找失效注释、无效 fixture 和不再可达的兼容分支。

## C 工作包首轮证据

已运行的基础矩阵：

- `cargo test -p magi-api --lib turn_harness::tests -- --test-threads=1`：48 passed、1 ignored。覆盖 ReadOnly 的 file/shell/Git/image 拒绝和只读读取、Restricted 的 workspace 内外写入、allow once、allow for turn、deny、cancel、expiry、Provider 请求次数与审批事件、FullAccess 的 workspace 内外写入，以及 Git dirty/branch drift/merge conflict。
- `cargo test -p magi-tool-runtime --lib -- --test-threads=1`：225 passed、1 ignored。覆盖全部内置工具策略分类、Browser 与 MCP 读写轴、shell/process/path 边界、workspace 作用域、写保护、取消和真实文件/进程副作用。

这些证据证明权限引擎和 Turn Harness 的基础轴已存在，但 C 不能关闭。仍缺少一份可审计的组合表，把 file/shell/process/Git/browser/MCP × 三种 AccessProfile × workspace 内外 × allow once/allow for turn/deny/cancel/expiry/duplicate/cross-turn/session 逐项关联到同一轮的副作用、审批事件和 Provider 请求次数；Electron packaged GUI 和真实 Provider 的权限组合也尚未全部覆盖。

## D 工作包首轮证据

- 真实 Provider/daemon 五类场景各 20 轮后端证据已存在：`/tmp/magi-real-provider-perf-personal20.json`、`/tmp/magi-real-provider-perf-workspace20.json`、`/tmp/magi-real-provider-perf-tool20.json`、`/tmp/magi-real-provider-perf-subagent20.json`。
- `cargo test -p magi-api --lib turn_harness::tests -- --test-threads=1` 的 48 个通过测试已覆盖真实 `TurnService` 链路中的取消、SSE/WebSocket reconnect、duplicate request、Task/Goal/工具/子代理、Provider 重试、Git dirty/branch drift/merge conflict 和部分权限审批恢复。

D 仍不能关闭。缺口是 daemon restart、history/replay、三种 AccessProfile 与 Git/审批场景的 Provider 级同轮证据，以及把这些后端阶段与 Electron Renderer 的 20 轮 `turnId` 逐轮关联；已有后端性能 JSON 不能直接扩大解释为 Provider 全矩阵完成。

## 关闭规则

任何工作包只有在代码或脚本变更、直接证据、定向测试和文档记录都齐全后，才能从“进行中”改为“已完成”。单轮证据不能替代多轮矩阵；当前 after 数据不能替代 before 数据；生产路径收敛不能替代测试 fixture 边界审计。
