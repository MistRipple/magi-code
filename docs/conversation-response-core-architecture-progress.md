# 消息响应核心架构重构进度

更新时间：2026-09-20
代码基线：`8565e471`（补齐权限与 Provider 验收矩阵）
对应方案：[conversation-response-core-architecture-redesign.md](/Users/xie/code/magi-rust-rewrite/docs/conversation-response-core-architecture-redesign.md)

本文只记录尚未满足完成定义的工作包、直接证据和推进顺序。完成一个工作包前，必须同时更新状态、证据路径和验证命令；没有直接证据的内容保持未完成。

## 总览

按方案文档的勾选项统计，当前 57 项中 51 项已完成、6 项未完成。`17.3` 在两个章节重复出现，去重后形成 5 个顶层门槛；按可独立验收的工程工作包拆分，剩余 7 项：

| 编号 | 工作包 | 状态 | 关闭条件 |
| --- | --- | --- | --- |
| A | 17.3：current Turn 写入口和测试夹具边界 | 进行中 | 生产路径只经 `CanonicalTurnEventSink`；剩余底层 fixture 已逐项分类，能迁移的已迁移，保留项有精确理由和测试覆盖 |
| B | 17.7：失效 legacy/兼容语义清理 | 进行中（首轮审计完成；未发现可直接删除的生产双轨） | 失效生产双轨、旧注释和无效 fixture 清除；迁移、恢复、协议兼容、旧字段拒绝逻辑保留并有边界说明 |
| C | 完整权限组合矩阵 | 进行中（基础 runtime/API 矩阵和跨 Turn 授权隔离已跑通；组合关联未完成） | ReadOnly/Restricted/FullAccess 与 file/shell/process/Git/browser/MCP、workspace 内外、审批生命周期和真实副作用均有证据 |
| D | 真实 Provider 全矩阵 | 进行中（五类后端 20 轮、基础 cancel/reconnect 及 Task restart replay 已有；全矩阵未完成） | Chat/Task/Goal/工具/子代理覆盖 cancel、reconnect、restart、history/replay、权限、Git 冲突和审批阻塞恢复 |
| E | Electron packaged GUI 全矩阵 | 部分完成 | 在现有 55 项单轮基础上补齐 Agent drawer、Goal/Plan 组合、Git/审批错误和 reconnect/history/restart 组合 |
| F | Electron 五类场景各 20 轮端到端 timing | 进行中（100/100 已完成同轮结构化关联，仍需与历史后端基线统一统计） | 5 类 × 20 轮均有同轮 accepted、Provider 首 delta、首 EventBus、Renderer 四阶段和 terminal 关联 |
| G | 性能 before/after 对比 | 待开始 | 有可审计的旧版本 before 数据，并与当前 after 数据使用同一场景、同一指标和同一统计方法 |

## 已有直接证据

- Rust workspace：`cargo test --workspace --all-targets --quiet -- --test-threads=1`，671 passed、1 ignored；本轮包含跨 Turn/Session 审批测试和终态判定收敛。
- Web/npm：protocol check、Svelte check/build、npm golden 已通过。
- Electron 单轮回归：`/tmp/magi-electron-dom-regression-10027.json`，55 项检查通过；此前 `/tmp/magi-electron-dom-regression-9983.json` 同样通过。
- 在最新 Web/Desktop 工作区状态重新打包并运行 Electron DOM 回归：`/tmp/magi-electron-dom-regression-10030.json`，`status=passed`、55 项检查通过、15 次 Provider 请求、5 条 Renderer timing sample；脚本自有 Electron/daemon 已清理。日志仍出现其他 Agent Desktop 修改产生的 `desktop_ipc_invalid:/channel` 和辅助模型未配置提示，不能把该结果扩大为完整 Desktop IPC 验收。
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
- Task profile 的进程内 restart/replay 已补充真实 `TurnService` harness：终态 canonical Turn 在重建 Coordinator、EventBus、dispatcher、TaskStore 后按同一 requestId 返回 replay，Provider 请求数不增加；这只覆盖 harness 的 restart/replay 轴，不能替代 daemon 进程重启和 Provider 全矩阵。
- Electron 五类各 20 轮后端/Renderer 同轮关联已完成：`/tmp/magi-electron-dom-correlated-timing-20-10108.json`，100 条唯一 `turnId`、903 项脚本检查、280 次 Provider 请求；每条记录都包含 `accepted_response_sent`、`runner_started`、`provider_first_delta`、`event_bus_first_event`、`canonical_terminal_published` 和 Renderer 四阶段。证据中的 `sinceAcceptedMs` 由日志时间戳计算，仅用于同轮阶段顺序和分布统计；Provider 首 delta 与终态仍保留各自后端阶段耗时。
- 在重新打包后的当前 Electron/Web 工作区上复验同一 timing-only 五类 × 20 轮：`/tmp/magi-electron-dom-correlated-timing-20-10031.json`，`status=passed`、903 项检查、280 次 Provider 请求、100 条唯一 `turnId`、0 条缺少后端阶段、终态来源全部为 `canonical_terminal_published`。该复验用于确认打包产物和当前 Web/Desktop 状态仍能完成同轮关联；Provider 波动下的分布不能直接替换历史性能基线。
- 应用迟到任务状态写回修复并重新打包后，标准 Electron DOM 回归 `/tmp/magi-electron-dom-regression-10120.json` 仍为 `status=passed`、55 项检查、15 次 Provider 请求和 5 条 Renderer timing sample；该结果只确认修复没有破坏既有单轮 GUI 流程。日志中仍有其他 Agent Desktop 修改产生的 `desktop_ipc_invalid:/channel` 和辅助模型未配置提示，不能扩大为完整 Desktop IPC 验收。
- 在当前打包产物上补充 Restricted 审批 GUI 验收：`/tmp/magi-electron-dom-regression-10142.json` 为 `status=passed`、61 项检查、17 次 Provider 请求。该场景使用已注册工作区中的 `shell_exec` 写入请求，验证审批卡片进入真实 DOM、包含“仅允许本次 / 本轮允许同类操作 / 拒绝并继续”三个操作、允许一次后最终消息恢复以及工作区内真实文件副作用；随后仍通过个人会话、daemon restart、history/reload 和 cancel 既有断言。该证据只补齐 E 的审批阻塞/恢复单场景，不能替代 Git/Goal 失败恢复和 reconnect/history/restart 组合矩阵。日志仍有其他 Agent Desktop 修改产生的 `desktop_ipc_invalid:/channel` 和辅助模型未配置提示。
- 该复验日志观察到并发子任务收口窗口的一次 `任务状态事实写回会话 Turn 失败`（`已有活动轮次`）后续仍由 root finalizer 发布 `canonical_terminal_published`。`session_turn_finalize` 现在会重新读取当前 sidecar：旧 Turn 已切换、缺失或进入终态时丢弃迟到 task status item；同一活动 Turn 的其它 canonical 写回错误仍继续传播。新增回归测试覆盖 running、blocked 和替换 Turn，避免把预期迟到写回记录成生产错误。该修复只收敛已确认的竞态，D/E 的 Provider/GUI 全矩阵仍需继续验证，不能把 100 条 timing 通过扩大解释为全矩阵无错误。
- 该关联证据仍不能关闭 F：它尚未与 `/tmp/magi-real-provider-perf-*.json` 的历史后端 20 轮采样合并，也没有 before 版本，因此性能前后对比和统一目标判定仍未完成。
- 同轮采样脚本现在会保留 stdout/stderr 的跨 chunk 行缓冲，并在 evidence 写入前 flush；缺少后端阶段时该轮直接失败，不会把 Renderer-only 记录当作关联通过。工具调用首轮只有 tool-call block 时，`provider_first_delta` 继续表示首个可见 content/thinking delta，raw tool-call-only chunk 仍单独记录为 `provider_response_received`。

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
| 2026-09-20 | F | 为生产 Conversation、Task、Goal 和子代理路径补充后端阶段日志，并在 Electron 脚本中解析 chunk 行缓冲、按 `turnId` 聚合和等待终态发布；五类各 20 轮完成同轮关联 | `/tmp/magi-electron-dom-correlated-timing-20-10108.json`：100 条唯一 `turnId`、903 checks passed、每条后端五阶段和 Renderer 四阶段齐全；`cargo check -p magi-api -p magi-conversation-runtime`、`node --check scripts/verify-electron-conversation-dom.mjs`、`npm run desktop:package -- --dir` |
| 2026-09-20 | F | 将同轮关联证据写入口径补充到性能计划，并确认脚本在输出前 flush 日志缓冲；保留工具调用 raw tool-call-only 首 chunk 尚未纳入首原始 delta 统计的限制 | `/tmp/magi-electron-dom-correlated-timing-20-10108.json`；`cargo test -p magi-conversation-runtime --lib conversation_loop -- --test-threads=1`：55 passed；`cargo test -p magi-conversation-runtime --lib session_writeback -- --test-threads=1`：34 passed；`cargo test -p magi-api --lib task_turn_finalize -- --test-threads=1`：6 passed；`node --check scripts/verify-electron-conversation-dom.mjs` |
| 2026-09-20 | F | 在当前 timing 埋点、迟到写回收敛和权限验收提交后重新执行 Rust workspace 全量验收，确认新增终态判定不会改变既有架构行为 | `cargo test --workspace --all-targets --quiet -- --test-threads=1`：671 passed、1 ignored；其中 conversation-runtime 533、magi-api 672、magi-daemon 127、magi-tool-runtime 225、magi-session-store 121 均通过 |
| 2026-09-20 | E/F | 在其他 Agent 的 Web/Desktop 修改仍保留的工作区上复验协议、Svelte、生产构建和 npm golden；这些结果只证明当前工作区可构建，不扩大为完整 Electron GUI 或 Provider 全矩阵 | `npm run protocol:check`；`npm --prefix web run check`：0 errors、0 warnings；`npm --prefix web run build`；`npm test`：Desktop 99、Browser Worker 57 及 Web golden 全部通过 |
| 2026-09-20 | E/C | 增加打包 Electron Restricted 审批卡片与真实 shell 写入验收；保留个人会话切回，避免工作区审批场景污染 daemon restart/history 断言 | `/tmp/magi-electron-dom-regression-10142.json`：61 checks passed、17 次 Provider 请求；审批卡片、三种操作、允许一次后的最终消息和真实文件副作用均通过；`node --check scripts/verify-electron-conversation-dom.mjs`、`git diff --check` |
| 2026-09-20 | D/E | 修复并发子任务状态 callback 与 root Turn 终态收口之间的迟到写回竞态；仅对已切换、缺失或终态 Turn 丢弃旧 item，同一活动 Turn 的真实错误继续返回 | `cargo test -p magi-conversation-runtime --lib session_turn_finalize -- --test-threads=1`：7 passed；`cargo test --workspace --all-targets --quiet -- --test-threads=1`：671 passed、1 ignored；对应提交 `b9c13529` |
| 2026-09-20 | C/D | 增加同一 Session 跨 Turn、跨 Session 的 `allow_for_turn` 审批隔离和 Task profile restart/replay 验收；验证真实文件副作用、审批请求数、canonical 终态和 Provider 请求不重复 | `cargo test -p magi-api --lib turn_harness::tests -- --test-threads=1`：50 passed、1 ignored；`cargo test -p magi-daemon --lib daemon::tests::session_turn_persists_without_live_subscriber_and_recovers_after_restart -- --test-threads=1`：1 passed；`cargo test -p magi-daemon --lib runtime_restart -- --test-threads=1`：3 passed；对应提交 `0251c710` |
| 2026-09-20 | A/D | 将 `killed` 与 `superseded` 纳入任务 Turn 终态判定，确保迟到 task status callback 在所有 canonical 终态下都按 stale item 丢弃；不改变同一活动 Turn 的真实错误传播 | `cargo test -p magi-conversation-runtime --lib session_turn_finalize -- --test-threads=1`：7 passed；对应提交 `b9c13529` |

## B 工作包首轮审计

当前检索到的 `legacy` / `fallback` 语义按职责分为：

- `magi-session-store` 的 v1 -> v2 转换、旧 sidecar/thread projection 重建和 `legacyMigration` 标记：属于一次性迁移输入，保留。
- `magi-daemon` 的旧目录读取、归档、重引入隔离和损坏状态拒绝：属于恢复与数据安全边界，保留。
- MCP/Browser 的 `fallback-only`、`available_fallback` 和协议兼容状态：属于对外状态合同，保留。
- `agent_runs.fallback_mode`、`openai-compatible` Provider 名称以及旧字段拒绝测试：属于现有协议或兼容性验证，保留。
- daemon 路由 `.fallback(get(...))` 和配置缺失时返回 unavailable：属于 HTTP 路由/配置错误边界，不是消息响应双实现。

首轮审计尚未发现可以直接删除的生产双轨路径；B 仍保持未完成，下一步需要把每个保留项绑定到具体测试，并继续寻找失效注释、无效 fixture 和不再可达的兼容分支。
本轮复核以 `rg -n --glob '*.rs' 'legacy|Legacy|fallback|compat|兼容|旧路径|回退' crates/magi-conversation-runtime crates/magi-session-store crates/magi-api crates/magi-daemon` 得到 477 条命中；排除测试样例、迁移/恢复、协议字段、浏览器/MCP 能力状态和配置/路由错误边界后，未发现新的可删除生产双轨。`session_projection_full_fallback_completed` 只处理没有 session 归属的 dirty 标记，`fallback_udp_ip` 只处理本机 UDP 探测失败，均不属于消息响应双实现。

## C 工作包首轮证据

已运行的基础矩阵：

- `cargo test -p magi-api --lib turn_harness::tests -- --test-threads=1`：50 passed、1 ignored。覆盖 ReadOnly 的 file/shell/Git/image 拒绝和只读读取、Restricted 的 workspace 内外写入、allow once、allow for turn、deny、cancel、expiry、Provider 请求次数与审批事件、跨 Turn/Session 授权隔离、FullAccess 的 workspace 内外写入，以及 Git dirty/branch drift/merge conflict。
- `cargo test -p magi-tool-runtime --lib -- --test-threads=1`：225 passed、1 ignored。覆盖全部内置工具策略分类、Browser 与 MCP 读写轴、shell/process/path 边界、workspace 作用域、写保护、取消和真实文件/进程副作用。

这些证据证明权限引擎和 Turn Harness 的基础轴已存在。新增 `restricted_profile_allow_for_turn_requires_new_approval_on_next_turn` 覆盖同一 Session 跨两个 Turn 及新 Session 的 `allow_for_turn` 隔离、三次真实文件副作用和三条审批请求；C 仍不能关闭。仍缺少一份可审计的组合表，把 file/shell/process/Git/browser/MCP × 三种 AccessProfile × workspace 内外 × allow once/allow for turn/deny/cancel/expiry/duplicate/cross-turn/session 逐项关联到同一轮的副作用、审批事件和 Provider 请求次数；其它工具类型、重复请求和 Electron/真实 Provider 权限组合也尚未全部覆盖。

## D 工作包首轮证据

- 真实 Provider/daemon 五类场景各 20 轮后端证据已存在：`/tmp/magi-real-provider-perf-personal20.json`、`/tmp/magi-real-provider-perf-workspace20.json`、`/tmp/magi-real-provider-perf-tool20.json`、`/tmp/magi-real-provider-perf-subagent20.json`。
- `cargo test -p magi-api --lib turn_harness::tests -- --test-threads=1` 的 50 个通过测试已覆盖真实 `TurnService` 链路中的取消、SSE/WebSocket reconnect、duplicate request、Task/Goal/工具/子代理、Provider 重试、Git dirty/branch drift/merge conflict、Task restart replay 和部分权限审批恢复；`magi-daemon` 的 session restart/history 测试也分别通过 1 个恢复测试和 3 个 runtime restart 测试。

D 仍不能关闭。新增 `task_profile_restart_replays_completed_turn_without_provider_reexecution` 覆盖进程内重建 runtime 后的 Task canonical replay，且既有 daemon restart/replay 测试继续通过。缺口是独立 daemon 进程 restart、history/replay 的更多场景、三种 AccessProfile 与 Git/审批场景的 Provider 级同轮证据，以及把这些后端阶段与 Electron Renderer 的 20 轮 `turnId` 逐轮关联；已有后端性能 JSON 不能直接扩大解释为 Provider 全矩阵完成。

## E 工作包首轮证据

最新打包 Electron/CDP 回归 `/tmp/magi-electron-dom-regression-10120.json` 为 `status=passed`，55 项检查通过、15 次 Provider 请求；此前 `/tmp/magi-electron-dom-regression-10027.json` 同样通过。现有覆盖包括个人/工作区 Chat、Task 工具卡片和工具组展开、Goal/Plan 卡片和 Goal 展开、子代理工具卡片与 `child_task_id`、代理运行中心、ReadOnly 写入拒绝、daemon restart、Renderer reload、history/session switch 和 cancel。

E 仍保持部分完成。尚缺 Agent drawer 的更多状态组合、Goal/Plan 的失败与恢复、Git dirty/drift/conflict 的可见错误、审批阻塞/恢复以及 reconnect/history/restart 的组合矩阵；55 项单轮回归不能替代这些组合。

## 关闭规则

任何工作包只有在代码或脚本变更、直接证据、定向测试和文档记录都齐全后，才能从“进行中”改为“已完成”。单轮证据不能替代多轮矩阵；当前 after 数据不能替代 before 数据；生产路径收敛不能替代测试 fixture 边界审计。
