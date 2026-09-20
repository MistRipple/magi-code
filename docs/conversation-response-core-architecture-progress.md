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
| B | 17.7：失效 legacy/兼容语义清理 | 待开始 | 失效生产双轨、旧注释和无效 fixture 清除；迁移、恢复、协议兼容、旧字段拒绝逻辑保留并有边界说明 |
| C | 完整权限组合矩阵 | 待开始 | ReadOnly/Restricted/FullAccess 与 file/shell/process/Git/browser/MCP、workspace 内外、审批生命周期和真实副作用均有证据 |
| D | 真实 Provider 全矩阵 | 待开始 | Chat/Task/Goal/工具/子代理覆盖 cancel、reconnect、restart、history/replay、权限、Git 冲突和审批阻塞恢复 |
| E | Electron packaged GUI 全矩阵 | 部分完成 | 在现有 55 项单轮基础上补齐 Agent drawer、Goal/Plan 组合、Git/审批错误和 reconnect/history/restart 组合 |
| F | Electron 五类场景各 20 轮端到端 timing | 待开始 | 5 类 × 20 轮均有同轮 accepted、Provider 首 delta、首 EventBus、Renderer 四阶段和 terminal 关联 |
| G | 性能 before/after 对比 | 待开始 | 有可审计的旧版本 before 数据，并与当前 after 数据使用同一场景、同一指标和同一统计方法 |

## 已有直接证据

- Rust workspace：`cargo test --workspace --all-targets --quiet -- --test-threads=1`，669 passed、1 ignored。
- Web/npm：protocol check、Svelte check/build、npm golden 已通过。
- Electron 单轮回归：`/tmp/magi-electron-dom-regression-9983.json`，55 项检查通过。
- Electron 五类单轮 Renderer timing：`/tmp/magi-electron-dom-timing-9857.json`，覆盖 `personal_chat`、`workspace_chat`、`workspace_tool`、`goal`、`subagent`，每类已有单轮四阶段 timing。
- 真实 Provider 后端五类场景各 20 轮：`/tmp/magi-real-provider-perf-personal20.json`、`/tmp/magi-real-provider-perf-workspace20.json`、`/tmp/magi-real-provider-perf-tool20.json`、`/tmp/magi-real-provider-perf-subagent20.json`。
- 五类 Electron 各 20 轮 timing 的证据文件 `/tmp/magi-electron-dom-timing-only-9984.json` 和 `/tmp/magi-electron-dom-timing-20-9985.json` 当前尚未形成，因此 F 保持未完成。
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

## 关闭规则

任何工作包只有在代码或脚本变更、直接证据、定向测试和文档记录都齐全后，才能从“进行中”改为“已完成”。单轮证据不能替代多轮矩阵；当前 after 数据不能替代 before 数据；生产路径收敛不能替代测试 fixture 边界审计。
