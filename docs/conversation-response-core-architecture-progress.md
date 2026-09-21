# 消息响应核心架构重构进度

更新时间：2026-09-21
代码基线：`5fa1176c`（后台进程审批矩阵 artifact 代码基线）
对应方案：[conversation-response-core-architecture-redesign.md](/Users/xie/code/magi-rust-rewrite/docs/conversation-response-core-architecture-redesign.md)

本文只记录尚未满足完成定义的工作包、直接证据和推进顺序。完成一个工作包前，必须同时更新状态、证据路径和验证命令；没有直接证据的内容保持未完成。

## 总览

按方案文档的勾选项统计，当前 57 项中 51 项已完成、6 项未完成。`17.3` 在两个章节重复出现，去重后形成 5 个顶层门槛；按可独立验收的工程工作包拆分，剩余 7 项：

| 编号 | 工作包 | 状态 | 关闭条件 |
| --- | --- | --- | --- |
| A | 17.3：current Turn 写入口和测试夹具边界 | 进行中（底层 fixture 审计完成，待主方案文档同步） | 生产路径只经 `CanonicalTurnEventSink`；剩余底层 fixture 已逐项分类，均为验证原子 mutation/恢复边界的测试，保留理由和测试覆盖已记录，并需同步方案文档的 17.3 勾选状态 |
| B | 17.7：失效 legacy/兼容语义清理 | 进行中（首轮审计完成；未发现可直接删除的生产双轨） | 失效生产双轨、旧注释和无效 fixture 清除；迁移、恢复、协议兼容、旧字段拒绝逻辑保留并有边界说明 |
| C | 完整权限组合矩阵 | 进行中（基础 runtime/API、跨 Turn/Session 隔离和重复审批回放已跑通；完整组合关联未完成） | ReadOnly/Restricted/FullAccess 与 file/shell/process/Git/browser/MCP、workspace 内外、审批生命周期和真实副作用均有证据 |
| D | 真实 Provider 全矩阵 | 进行中（五类后端 20 轮、基础 cancel/reconnect 及 Task restart replay 已有；全矩阵未完成） | Chat/Task/Goal/工具/子代理覆盖 cancel、reconnect、restart、history/replay、权限、Git 冲突和审批阻塞恢复 |
| E | Electron packaged GUI 全矩阵 | 部分完成 | 在现有 55 项单轮基础上补齐 Agent drawer、Goal/Plan 组合、Git/审批错误和 reconnect/history/restart 组合 |
| F | Electron 五类场景各 20 轮端到端 timing | 进行中（最新 100/100 已包含 Task raw tool-call-only 首 delta，仍需与历史后端基线统一统计） | 5 类 × 20 轮均有同轮 accepted、Provider raw/可见首 delta、首 EventBus、Renderer 四阶段和 terminal 关联 |
| G | 性能 before/after 对比 | 待开始 | 有可审计的旧版本 before 数据，并与当前 after 数据使用同一场景、同一指标和同一统计方法 |

## 已有直接证据

- Rust workspace：`cargo test --workspace --all-targets --quiet -- --test-threads=1`，681 passed、1 ignored；本轮包含 Browser Host 协议、process 生命周期矩阵、daemon Task Turn 重启回放、Git mutation 审批生命周期、后台进程审批、跨 Turn/Session 审批测试和终态判定收敛。
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
- 新增真实 daemon runtime 重启/replay 验收：`task_turn_replays_after_daemon_restart_without_duplicate_canonical_acceptance` 通过 `DaemonRuntime::restore` 销毁并重建 runtime，验证 Task Turn、canonical 用户 item 的 request/userMessage identity 和 root task 恢复；使用同一 requestId/fingerprint 重提交后返回同一 session/turn/root task，canonical Turn 数量保持为 1。该证据补齐 D 的独立 daemon restart/replay 轴，但仍不覆盖完整权限、Git、审批和 history/replay 组合。
- Electron 五类各 20 轮后端/Renderer 同轮关联已完成：`/tmp/magi-electron-dom-correlated-timing-20-10108.json`，100 条唯一 `turnId`、903 项脚本检查、280 次 Provider 请求；每条记录都包含 `accepted_response_sent`、`runner_started`、`provider_first_delta`、`event_bus_first_event`、`canonical_terminal_published` 和 Renderer 四阶段。证据中的 `sinceAcceptedMs` 由日志时间戳计算，仅用于同轮阶段顺序和分布统计；Provider 首 delta 与终态仍保留各自后端阶段耗时。
- 在重新打包后的当前 Electron/Web 工作区上复验同一 timing-only 五类 × 20 轮：`/tmp/magi-electron-dom-correlated-timing-20-10031.json`，`status=passed`、903 项检查、280 次 Provider 请求、100 条唯一 `turnId`、0 条缺少后端阶段、终态来源全部为 `canonical_terminal_published`。该复验用于确认打包产物和当前 Web/Desktop 状态仍能完成同轮关联；Provider 波动下的分布不能直接替换历史性能基线。
- 应用迟到任务状态写回修复并重新打包后，标准 Electron DOM 回归 `/tmp/magi-electron-dom-regression-10120.json` 仍为 `status=passed`、55 项检查、15 次 Provider 请求和 5 条 Renderer timing sample；该结果只确认修复没有破坏既有单轮 GUI 流程。日志中仍有其他 Agent Desktop 修改产生的 `desktop_ipc_invalid:/channel` 和辅助模型未配置提示，不能扩大为完整 Desktop IPC 验收。
- 在当前打包产物上补充 Restricted 审批 GUI 验收：`/tmp/magi-electron-dom-regression-10142.json` 为 `status=passed`、61 项检查、17 次 Provider 请求。该场景使用已注册工作区中的 `shell_exec` 写入请求，验证审批卡片进入真实 DOM、包含“仅允许本次 / 本轮允许同类操作 / 拒绝并继续”三个操作、允许一次后最终消息恢复以及工作区内真实文件副作用；随后仍通过个人会话、daemon restart、history/reload 和 cancel 既有断言。该证据只补齐 E 的审批阻塞/恢复单场景，不能替代 Git/Goal 失败恢复和 reconnect/history/restart 组合矩阵。日志仍有其他 Agent Desktop 修改产生的 `desktop_ipc_invalid:/channel` 和辅助模型未配置提示。
- 在最新打包产物上补充 Goal Provider 失败后恢复验收：`/tmp/magi-electron-dom-regression-10201.json` 为 `status=passed`、64 项检查、35 次 Provider 请求。脚本先让 Goal 真实进入 Provider 500 失败终态，验证失败事实进入 Renderer DOM，再新建会话重新建立并完成 Goal/Plan，验证恢复后的 Goal 卡片和计划卡片重新出现；该证据只覆盖 Goal 失败/恢复单场景，仍不能替代 Git 错误、Agent drawer 多状态和 reconnect/history/restart 组合矩阵。日志仍有其他 Agent Desktop 修改产生的 `desktop_ipc_invalid:/channel` 和辅助模型未配置提示。
- 在同一打包 Electron 流程上补充 Restricted 审批拒绝验收：`/tmp/magi-electron-dom-regression-10220.json` 为 `status=passed`、68 项检查、36 次 Provider 请求。脚本在独立 workspace session 中展示真实审批卡片，点击“拒绝并继续”后验证拒绝事实进入 DOM，且工作区目标文件没有产生；此前审批允许一次的真实文件副作用仍在同一回归中保留。该证据只补齐审批拒绝单场景，审批过期/取消以及 Git 错误和 reconnect/history/restart 组合仍未完成。
- 在同一打包 Electron 流程上补充 Restricted 审批取消验收：`/tmp/magi-electron-dom-regression-10222.json` 为 `status=passed`、73 项检查、37 次 Provider 请求。脚本在独立 workspace session 中展示审批卡片，点击停止按钮取消等待授权的 Turn，并验证停止后没有工作区文件副作用；审批允许一次和拒绝两条路径仍在同一回归中保留。该证据补齐审批取消单场景，审批过期以及 Git 错误和 reconnect/history/restart 组合仍未完成。
- 为 Task/Goal/子代理路径补充 raw tool-call-only Provider 首 delta：`ModelStreamingDelta.tool_calls` 现在从 bridge streaming accumulator 传递工具调用快照，执行层另记 `provider_first_raw_delta`，而 `provider_first_delta` 继续只表示首个可见 content/thinking delta。单轮证据 `/tmp/magi-electron-dom-raw-tool-1-10230.json` 为 `status=passed`、13 项检查；最新五类 × 20 轮证据 `/tmp/magi-electron-dom-correlated-timing-20-10231.json` 为 `status=passed`、963 项检查、100 条唯一 `turnId`、280 次 Provider 请求，五类各 20 条，60 条 Task/Goal/子代理 sample 全部包含 `provider_first_raw_delta`，且每条仍具备 accepted、runner、可见首 delta、首 EventBus、canonical terminal 和 Renderer 四阶段。该证据只补齐 raw 首 chunk 的同轮观测，不关闭历史 Provider 样本统一和 before/after 性能缺口。
- 该复验日志观察到并发子任务收口窗口的一次 `任务状态事实写回会话 Turn 失败`（`已有活动轮次`）后续仍由 root finalizer 发布 `canonical_terminal_published`。`session_turn_finalize` 现在会重新读取当前 sidecar：旧 Turn 已切换、缺失或进入终态时丢弃迟到 task status item；同一活动 Turn 的其它 canonical 写回错误继续传播。回归测试覆盖 running、blocked、替换 Turn，以及活动 Turn 内 immutable canonical item 冲突，避免把预期迟到写回记录成生产错误，也避免吞掉真实写回错误。该修复只收敛已确认的竞态，D/E 的 Provider/GUI 全矩阵仍需继续验证，不能把 100 条 timing 通过扩大解释为全矩阵无错误。
- 该关联证据仍不能关闭 F：最新证据已包含 raw tool-call-only 首 delta，但尚未与 `/tmp/magi-real-provider-perf-*.json` 的历史后端 20 轮采样合并，也没有 before 版本，因此性能前后对比和统一目标判定仍未完成。
- 同轮采样脚本现在会保留 stdout/stderr 的跨 chunk 行缓冲，并在 evidence 写入前 flush；缺少后端阶段时该轮直接失败，不会把 Renderer-only 记录当作关联通过。`provider_first_raw_delta` 表示首个包含工具调用快照或可见内容/思考的 Provider delta；`provider_first_delta` 仍只表示首个可见 content/thinking delta。工具调用首轮可能没有可见正文，两个阶段因此可以落在不同 Provider round。
- Restricted `shell_exec` 的重复 requestId/fingerprint 回放已补充到真实 `TurnService` harness：待审批期间第二次提交复用同一 `turn_id`/root task，不创建第二个 pending 审批、不重复发布 `tool.approval.requested`、不重复请求 Provider；放行后只产生一次真实文件副作用并完成 canonical Turn。该证据只覆盖 duplicate + approval 组合，不能替代完整工具类型、访问模式、作用域和生命周期矩阵。
- Git 结构化工具现在由 `MagiTurnHarness` 注入与 daemon 相同的 `GitToolRuntime`、Session Git Context、Snapshot 和 runtime persistence；新增 `git_branch_switch` 的 allow once、deny、cancel、expiry、duplicate pending、allow_for_turn 跨 Turn 和跨 Session 共七个 API 层验收。allow once 实际切换到 `approval-target`，deny/cancel/expiry 保持原分支，重复提交只保留一个 pending 审批，allow_for_turn 在下一 Turn/Session 都重新请求审批；测试同时断言 canonical ToolCall、审批 requested/resolved 事件、Provider 请求次数和 Turn/Task 终态，并将七行结果写入 `/tmp/magi-api-git-approval-matrix.json`。这补齐 `git_branch_switch` 的 API 审批生命周期代表格，但不替代其它 Git mutation、GUI、Browser/MCP 外部副作用和统一全组合 JSON 矩阵。
- API Harness 还补充 Restricted `shell_exec(background=true)` 的审批允许路径：通过真实后台进程启动并写入工作区文件，验证审批 requested/resolved、Provider 两轮请求和 canonical Turn/Task completed，结果写入 `/tmp/magi-api-process-approval-matrix.json`。该证据只覆盖 process 的 allow once 代表格，拒绝、取消、过期和跨作用域组合仍未关闭。

## 推进顺序

1. 先完成 A，收紧剩余测试专用 current Turn 边界并记录不可迁移的底层 canonical fixture。
2. 再完成 B，逐项处理明确失效的 legacy/兼容语义，不触碰真实迁移和恢复职责。
3. 依次补齐 C、D、E，所有矩阵都必须包含真实副作用或明确的拒绝证据。
4. 完成 F 的五类 × 20 轮同轮关联，随后再处理 G 的 before/after 对比。

## A 工作包首轮审计

截至基线提交：

- 生产代码没有 `upsert_current_turn(...)`、`TaskResultReceiver`、`uses_active_completion_sink`、生产 `poll_results(...)` 或旧 `run_dispatch_submission(...)`。
- `CanonicalTurnEventSink` 内部仍调用 SessionStore 的 canonical mutation，这是允许的内部边界。
- `magi-session-store/src/store/tests.rs` 当前有 19 处 `set_current_turn_status_for_test(...)`、2 处 `settle_current_turn_at_for_test(...)` 和 16 处 `upsert_current_turn_item_for_turn(...)` 测试调用（另有 1 处同名测试函数声明）；前两者仅是测试便利包装，后者同时被 Sink 的生产写回使用，不能直接删除。
- 首轮已将仅测试使用的 `update_current_turn_status_for_turn` 包装收敛为 `#[cfg(test)] pub(crate) set_current_turn_status_for_test`；生产编译不再暴露这个 current Turn 写入口。
- `upsert_current_turn_item_for_turn` 仍被 `CanonicalTurnEventSink` 的生产写回使用，不能按测试入口删除；16 处 storage fixture 已逐个判断，均直接验证 SessionStore canonical mutation 的原子性、版本、恢复投影或冲突拒绝，不能改成只验证上层 Coordinator 的黑盒流程。

这 16 处调用目前按职责分为：

- 1 处 Continue 前的活动工具项恢复夹具；
- 2 处 assistant stream -> final 的 canonical item 状态迁移验收；
- 1 处 durable canonical projection 写回验收；
- 1 处 request ID 元数据持久化验收；
- 2 处内部 `agent_wait` / `agent_spawn` item 投影验收；
- 4 处 blocked/killed/terminal item 收口验收；
- 5 处 stale owner、不可变字段、状态回退和迟到旧轮次拒绝验收。

这些测试直接验证 SessionStore canonical mutation 的原子性、版本和冲突规则，不能改成只验证上层 Coordinator 的黑盒流程；它们的合法底层边界已完成分类。`session_turn_finalize::task_status_write_preserves_active_turn_canonical_errors` 另行证明迟到写回判定不会吞掉同一活动 Turn 的真实 canonical 冲突。A 仍需在方案文档同步勾选状态后关闭。

## 变更记录

| 日期 | 工作包 | 进展 | 验证 |
| --- | --- | --- | --- |
| 2026-09-20 | F/E | 形成 55 项 Electron 单轮 DOM 回归和五类单轮 Renderer timing | `npm run test:electron-conversation-dom`；证据见上 |
| 2026-09-20 | A | 完成生产 current Turn 写入口审计；将仅测试使用的状态包装改为测试专用命名和可见性，并逐项确认 16 处底层 item fixture 均直接验证 canonical mutation 原子性、恢复投影或冲突拒绝，无可迁移到 Coordinator/Sink 的黑盒替代 | `cargo fmt --all -- --check`；`cargo test -p magi-session-store --lib -- --test-threads=1`：121 passed；`cargo test -p magi-conversation-runtime --lib session_turn_finalize -- --test-threads=1`：8 passed |
| 2026-09-20 | F | 完成五类 timing 各 2 轮，并新增每轮 terminal 收口门槛；20 轮采样在 Goal 第 12 轮暴露上下文隔离问题 | `/tmp/magi-electron-dom-timing-2-9999d.json`：5 类 × 2，43 checks passed；`/tmp/magi-electron-dom-timing-20-9998.json` 未形成完整证据 |
| 2026-09-20 | F | 通过 `MAGI_ELECTRON_DOM_TIMING_SCENARIO` 按场景隔离采样，并对 Goal/子代理轮次切换到独立个人草稿；五类 Renderer timing 各完成 20 轮 | `/tmp/magi-electron-dom-timing-20-summary-10020-10026.json`；5 类各 20 条唯一 `turnId`，每条均有四个 Renderer 阶段和 terminal 收口 |
| 2026-09-20 | C | 跑通权限 runtime/API 基础矩阵并记录组合缺口 | `magi-api turn_harness`：48 passed、1 ignored；`magi-tool-runtime`：228 passed、1 ignored |
| 2026-09-20 | F | 为生产 Conversation、Task、Goal 和子代理路径补充后端阶段日志，并在 Electron 脚本中解析 chunk 行缓冲、按 `turnId` 聚合和等待终态发布；五类各 20 轮完成同轮关联 | `/tmp/magi-electron-dom-correlated-timing-20-10108.json`：100 条唯一 `turnId`、903 checks passed、每条后端五阶段和 Renderer 四阶段齐全；`cargo check -p magi-api -p magi-conversation-runtime`、`node --check scripts/verify-electron-conversation-dom.mjs`、`npm run desktop:package -- --dir` |
| 2026-09-20 | F | 将同轮关联证据写入口径补充到性能计划，并确认脚本在输出前 flush 日志缓冲；保留工具调用 raw tool-call-only 首 chunk 尚未纳入首原始 delta 统计的限制 | `/tmp/magi-electron-dom-correlated-timing-20-10108.json`；`cargo test -p magi-conversation-runtime --lib conversation_loop -- --test-threads=1`：55 passed；`cargo test -p magi-conversation-runtime --lib session_writeback -- --test-threads=1`：34 passed；`cargo test -p magi-api --lib task_turn_finalize -- --test-threads=1`：6 passed；`node --check scripts/verify-electron-conversation-dom.mjs` |
| 2026-09-20 | F | 在当前 timing 埋点、迟到写回收敛和权限验收提交后重新执行 Rust workspace 全量验收，确认新增终态判定不会改变既有架构行为 | `cargo test --workspace --all-targets --quiet -- --test-threads=1`：671 passed、1 ignored；其中 conversation-runtime 533、magi-api 672、magi-daemon 127、magi-tool-runtime 225、magi-session-store 121 均通过 |
| 2026-09-20 | E/F | 在其他 Agent 的 Web/Desktop 修改仍保留的工作区上复验协议、Svelte、生产构建和 npm golden；这些结果只证明当前工作区可构建，不扩大为完整 Electron GUI 或 Provider 全矩阵 | `npm run protocol:check`；`npm --prefix web run check`：0 errors、0 warnings；`npm --prefix web run build`；`npm test`：Desktop 99、Browser Worker 57 及 Web golden 全部通过 |
| 2026-09-20 | E/C | 增加打包 Electron Restricted 审批卡片与真实 shell 写入验收；保留个人会话切回，避免工作区审批场景污染 daemon restart/history 断言 | `/tmp/magi-electron-dom-regression-10142.json`：61 checks passed、17 次 Provider 请求；审批卡片、三种操作、允许一次后的最终消息和真实文件副作用均通过；`node --check scripts/verify-electron-conversation-dom.mjs`、`git diff --check` |
| 2026-09-20 | E | 增加打包 Electron Goal Provider 失败后恢复验收：真实 500 失败进入 DOM，随后新会话重新建立并完成 Goal/Plan | `/tmp/magi-electron-dom-regression-10201.json`：64 checks passed、35 次 Provider 请求；失败事实、恢复后的最终消息和 Goal/Plan 卡片均通过；`node --check scripts/verify-electron-conversation-dom.mjs`、`git diff --check` |
| 2026-09-20 | B | 对 runtime/session-store/api/daemon 的 legacy、compat、fallback、旧路径命中重新审计；523 条命中均归入迁移、恢复、协议/能力状态、配置或路由错误边界，未找到可安全删除的生产双轨 | `rg -n --glob '*.rs' 'legacy|Legacy|fallback|compat|兼容|旧路径|回退' crates/magi-conversation-runtime crates/magi-session-store crates/magi-api crates/magi-daemon`：523 条命中；保留项分类已写入 B 工作包记录 |
| 2026-09-20 | A/D/E | 修复并发子任务状态 callback 与 root Turn 终态收口之间的迟到写回竞态；仅对已切换、缺失或终态 Turn 丢弃旧 item，同一活动 Turn 的 immutable canonical 写回错误继续返回 | `cargo test -p magi-conversation-runtime --lib session_turn_finalize -- --test-threads=1`：8 passed；`cargo test --workspace --all-targets --quiet -- --test-threads=1`：671 passed、1 ignored；对应提交 `b9c13529`，新增 `task_status_write_preserves_active_turn_canonical_errors` |
| 2026-09-20 | C/D | 增加同一 Session 跨 Turn、跨 Session 的 `allow_for_turn` 审批隔离和 Task profile restart/replay 验收；验证真实文件副作用、审批请求数、canonical 终态和 Provider 请求不重复 | `cargo test -p magi-api --lib turn_harness::tests -- --test-threads=1`：50 passed、1 ignored；`cargo test -p magi-daemon --lib daemon::tests::session_turn_persists_without_live_subscriber_and_recovers_after_restart -- --test-threads=1`：1 passed；`cargo test -p magi-daemon --lib runtime_restart -- --test-threads=1`：3 passed；对应提交 `0251c710` |
| 2026-09-20 | C | 补充 Restricted `shell_exec` 重复 requestId/fingerprint 的待审批回放验收，确认 duplicate 不重复创建审批、Provider 请求或真实副作用 | `cargo test -p magi-api --lib turn_harness::tests::restricted_profile_duplicate_request_replays_pending_approval_without_duplicate_side_effects -- --test-threads=1`：1 passed；相关实现位于 `crates/magi-api/src/turn_harness.rs` |
| 2026-09-20 | A/D | 将 `killed` 与 `superseded` 纳入任务 Turn 终态判定，确保迟到 task status callback 在所有 canonical 终态下都按 stale item 丢弃；不改变同一活动 Turn 的真实错误传播 | `cargo test -p magi-conversation-runtime --lib session_turn_finalize -- --test-threads=1`：7 passed；对应提交 `b9c13529` |
| 2026-09-20 | D | 增加真实 daemon runtime 重启后的 Task Turn replay 验收：恢复 canonical Turn 和用户 request identity，并用相同 requestId/fingerprint 重提交验证不重复创建 canonical Turn | `cargo test -p magi-daemon --lib daemon::tests::task_turn_replays_after_daemon_restart_without_duplicate_canonical_acceptance -- --test-threads=1`：1 passed；`cargo test -p magi-daemon --lib -- --test-threads=1`：128 passed |
| 2026-09-20 | C | 将现有权限验收按工具面、AccessProfile、workspace 作用域、生命周期、副作用、审批事件和 Provider 请求次数登记为可复核矩阵；明确 Browser/MCP、process、Git 和跨 Turn/session 的剩余格子 | `cargo test -p magi-api --lib turn_harness::tests -- --test-threads=1`：51 passed、1 ignored；`cargo test -p magi-tool-runtime --lib -- --test-threads=1`：228 passed、1 ignored |
| 2026-09-20 | E | 增加打包 Electron Restricted 审批拒绝验收：真实 DOM 中点击“拒绝并继续”，验证拒绝终态可见且没有工作区文件副作用 | `/tmp/magi-electron-dom-regression-10220.json`：68 checks passed、36 次 Provider 请求；`node --check scripts/verify-electron-conversation-dom.mjs`、`git diff --check` |
| 2026-09-20 | E | 增加打包 Electron Restricted 审批取消验收：真实 DOM 中点击停止按钮取消等待授权的 Turn，验证取消终态和无文件副作用 | `/tmp/magi-electron-dom-regression-10222.json`：73 checks passed、37 次 Provider 请求；`node --check scripts/verify-electron-conversation-dom.mjs`、`git diff --check` |
| 2026-09-20 | A/C/D/E | 在 daemon 重启回放、权限矩阵登记和 Electron 审批取消验收后重新执行 Rust workspace 全量测试，确认新增证据测试没有改变其它 crate 行为 | `cargo fmt --all -- --check`；`cargo test --workspace --all-targets --quiet -- --test-threads=1`：672 passed、1 ignored；新增 daemon 测试所在 crate 为 128 passed，其余 workspace 测试均通过 |
| 2026-09-21 | F | 扩展 `ModelStreamingDelta` 的 raw tool-call snapshot，并在 Task/Goal/子代理路径记录 `provider_first_raw_delta`；重新打包后完成单轮和五类 × 20 轮同轮复验 | `/tmp/magi-electron-dom-raw-tool-1-10230.json`：13 checks passed；`/tmp/magi-electron-dom-correlated-timing-20-10231.json`：963 checks passed、100 条唯一 `turnId`、280 次 Provider 请求、60 条 Task/Goal/子代理 sample 均具备 raw stage；`cargo test -p magi-bridge-client --lib -- --test-threads=1`：252 passed；`cargo test -p magi-conversation-runtime --lib conversation_loop -- --test-threads=1`：55 passed；`npm run desktop:package -- --dir` |
| 2026-09-21 | F | 在 raw delta 代码和最新打包证据后重新执行 workspace 全量测试，确认新增 `tool_calls` 字段和 raw timing stage 没有破坏其它 crate | `cargo test --workspace --all-targets --quiet -- --test-threads=1`：672 passed、1 ignored；`magi-bridge-client`：252 passed；`magi-conversation-runtime`：534 passed；`magi-api turn_harness`：51 passed、1 ignored；`magi-daemon`：128 passed；`magi-tool-runtime`：228 passed、1 ignored |
| 2026-09-21 | C | 将外部 MCP 读写权限组合展开为 ReadOnly/Restricted/FullAccess × read/write 的 6 行 executor side-effect 矩阵，并补充 Git 读/写与 Browser snapshot/navigate × 三种 AccessProfile 的 host executor 到达矩阵；明确 Browser 仍是注入 host executor 边界而非真实 Chromium 外部副作用 | `cargo test -p magi-tool-runtime --lib external_mcp_access_profile_matrix_records_executor_side_effects -- --test-threads=1`：1 passed；`cargo test -p magi-tool-runtime --lib structured_git_access_profile_matrix_blocks_mutations_before_executor -- --test-threads=1`：1 passed；`cargo test -p magi-tool-runtime --lib browser_access_profile_matrix_records_host_executor_side_effects -- --test-threads=1`：1 passed；`cargo test -p magi-tool-runtime --lib -- --test-threads=1`：228 passed、1 ignored |
| 2026-09-21 | C | 增加 process_launch/read/write/kill 的 AccessProfile 生命周期矩阵；ReadOnly 拒绝、Restricted 保持 NeedsApproval，FullAccess 完成真实 cat 进程启动、读取、写入和终止，行级结果以结构化 JSON 断言保留 | `cargo test -p magi-tool-runtime --lib internal_process_access_profile_matrix_records_lifecycle_side_effects -- --test-threads=1`：1 passed；`cargo test -p magi-tool-runtime --lib -- --test-threads=1`：229 passed、1 ignored |
| 2026-09-21 | A/C/D/E/F | 在 Browser Host 协议和 process 生命周期矩阵后重新执行 workspace 全量验收，确认新增测试没有改变其它 crate 行为 | `cargo fmt --all -- --check`；`cargo test --workspace --all-targets --quiet -- --test-threads=1`：673 passed、1 ignored；`magi-api`：673 passed、1 ignored；`magi-tool-runtime`：229 passed、1 ignored；`magi-conversation-runtime`：534 passed；`magi-daemon`：128 passed |
| 2026-09-21 | D | 增加真实 HTTP daemon 实例重启验收：启动第一台 daemon、通过真实 HTTP 接纳 Task Turn，关闭并用同一 state root 启动第二台 daemon，验证 bootstrap/messages 回放、相同 requestId/fingerprint 返回同一 session/turn/root task 且用户 item 不重复 | `cargo test -p magi-daemon --lib daemon::tests::daemon_http_server_restart_replays_task_turn_without_duplicate_acceptance -- --test-threads=1`：1 passed；该测试覆盖真实 HTTP server 生命周期，但仍是同一测试进程内的两个 daemon 实例，不扩大为独立 OS 进程证据 |
| 2026-09-21 | D | 通过真实 `target/debug/magi-daemon-app` 启动两个独立 OS daemon 进程，使用同一 `MAGI_STATE_ROOT` 完成 Task Turn 接纳、SIGTERM、第二进程启动、bootstrap/messages 回放和相同 requestId/fingerprint 重提交 | `/tmp/magi-daemon-independent-process-restart-20260921.json`：`status=passed`，transport=`real independent magi-daemon-app process`，首/回放 Turn 与 root task 相同，userMessageCount=1；两个自启动进程均在验收后退出 |
| 2026-09-21 | C | 增加 BrowserHostClient Unix WebSocket 协议验收：三种 AccessProfile 各执行 snapshot 与 navigate，验证真实 BrowserHostClient 请求经过 Desktop Host 协议、Surface 绑定、页面状态和导航命令，并记录 3 次 snapshot、3 次 navigate 的 Host 命令；Host 仍为确定性协议 double，不扩大为真实 Chromium 进程副作用 | `cargo test -p magi-api --lib browser_tool_runtime -- --test-threads=1`：14 passed；新增 `browser_access_profile_matrix_reaches_real_host_protocol_for_read_and_write` 通过 |
| 2026-09-21 | C/D | 为 API Turn Harness 注入与 daemon 相同的结构化 Git runtime，补齐 Restricted `git_branch_switch` 的 allow once、deny、cancel、expiry、duplicate pending、allow_for_turn 跨 Turn 和跨 Session 七条真实审批生命周期；allow once 切换真实 branch，其他路径保持 branch 不变并收口 canonical Turn/Task | `cargo test -p magi-api --lib turn_harness::tests::restricted_profile_git_branch_switch_ -- --test-threads=1`：7 passed；`cargo test -p magi-api --lib turn_harness::tests -- --test-threads=1`：58 passed、1 ignored |
| 2026-09-21 | C/D | 将 `git_branch_switch` 七条 API 审批生命周期的真实分支副作用、审批事实、Provider 请求次数和 Turn/Task 终态写入结构化 artifact；其它 Git mutation 仍保持未登记 | `/tmp/magi-api-git-approval-matrix.json`：7 行；`cargo fmt --all -- --check`；`cargo test -p magi-api --lib turn_harness::tests::restricted_profile_git_branch_switch_ -- --test-threads=1`：7 passed；`cargo test -p magi-api --lib turn_harness::tests -- --test-threads=1`：58 passed、1 ignored |
| 2026-09-21 | C/D | 增加 Restricted `shell_exec(background=true)` 的真实后台进程审批允许验收，验证工作区文件副作用、审批事件、Provider 请求次数和 canonical Turn/Task 终态，并写入单行 process artifact；拒绝、取消、过期和跨作用域仍未覆盖 | `/tmp/magi-api-process-approval-matrix.json`：1 行；`cargo test -p magi-api --lib turn_harness::tests::restricted_profile_background_shell_approval_runs_real_process_once -- --test-threads=1`：1 passed；`cargo fmt --all -- --check` |
| 2026-09-21 | A/C/D | 后台进程审批测试加入后重新执行 workspace 全量 Rust 验收，确认新增真实进程审批路径未改变其它 crate 行为 | `cargo test --workspace --all-targets --quiet -- --test-threads=1`：681 passed、1 ignored；`magi-api turn_harness`：59 passed、1 ignored；`magi-conversation-runtime`：534 passed；`magi-daemon`：129 passed；`magi-tool-runtime`：229 passed、1 ignored |

## B 工作包首轮审计

当前检索到的 `legacy` / `fallback` 语义按职责分为：

- `magi-session-store` 的 v1 -> v2 转换、旧 sidecar/thread projection 重建和 `legacyMigration` 标记：属于一次性迁移输入，保留。
- `magi-daemon` 的旧目录读取、归档、重引入隔离和损坏状态拒绝：属于恢复与数据安全边界，保留。
- MCP/Browser 的 `fallback-only`、`available_fallback` 和协议兼容状态：属于对外状态合同，保留。
- `agent_runs.fallback_mode`、`openai-compatible` Provider 名称以及旧字段拒绝测试：属于现有协议或兼容性验证，保留。
- daemon 路由 `.fallback(get(...))` 和配置缺失时返回 unavailable：属于 HTTP 路由/配置错误边界，不是消息响应双实现。

首轮审计尚未发现可以直接删除的生产双轨路径；B 仍保持未完成，下一步需要把每个保留项绑定到具体测试，并继续寻找失效注释、无效 fixture 和不再可达的兼容分支。
本轮复核以 `rg -n --glob '*.rs' 'legacy|Legacy|fallback|compat|兼容|旧路径|回退' crates/magi-conversation-runtime crates/magi-session-store crates/magi-api crates/magi-daemon` 得到 523 条命中；排除测试样例、迁移/恢复、协议字段、浏览器/MCP 能力状态和配置/路由错误边界后，未发现新的可删除生产双轨。新增命中仍属于已知边界：daemon 归档目录名中的 `fallback-*`、旧 thread history 重建标记、MCP skill 默认名称、OpenAI-compatible 环境变量配置和 HTTP 路由 fallback。`session_projection_full_fallback_completed` 只处理没有 session 归属的 dirty 标记，`fallback_udp_ip` 只处理本机 UDP 探测失败，均不属于消息响应双实现。

B 的保留语义已绑定到以下测试边界：

| 语义 | 绑定测试或验证 | 保留理由 |
| --- | --- | --- |
| v1→v2 会话、sidecar、thread projection 迁移 | `magi-session-store::store::tests::v2_store_rejects_legacy_todo_list_payload`、`magi-session-store::store::tests::sidecar_rejects_legacy_recovery_ref_json`、`magi-daemon::daemon::persistence::tests::legacy_layout_migrates_once_and_archives_sources`、`interrupted_layout_migration_restarts_from_legacy_source` | 只接收历史输入并转换到 canonical v2；删除会破坏已有状态恢复 |
| 已提交 v2 后的旧目录重引入与孤儿事件隔离 | `committed_v2_layout_quarantines_proven_legacy_orphan_event_log`、`committed_v2_layout_cleans_empty_reintroduced_legacy_files`、`interrupted_layout_migration_restarts_from_legacy_authority` | 防止旧进程迟到写回覆盖新事实或孤儿事件复活 |
| 损坏状态拒绝，不回退空状态 | `corrupted_accepted_journal_is_rejected_without_backup_or_empty_fallback`、`corrupted_durable_state_is_rejected_without_empty_fallback` | 空状态会静默丢失 canonical Turn，必须保持 fail-closed |
| 协议旧字段拒绝和 profile/replay 身份 | `durable_records_reject_legacy_snake_case_fields`、`turn_contract::tests::legacy_canonical_turn_without_request_identity_is_not_replayable`、MCP `instruction_skill_operations_reject_legacy_name_fields` | 这些字段属于对外合同和安全拒绝边界，不是生产双实现 |
| Provider/工具传输恢复 | `magi-bridge-client` 的 `streaming_retries_before_first_delta_only`、`streaming_does_not_retry_after_visible_delta`，以及 `tool_batch` 的重复调用安全重试测试 | 只处理当前唯一执行链中的传输错误，不生成第二条 Turn 事实链 |

上述绑定没有发现可删除的失效生产分支；B 仍保持进行中，下一步只处理能证明不可达的注释或 fixture，不删除真实迁移、恢复、拒绝和传输恢复逻辑。

## C 工作包首轮证据

已运行的基础矩阵：

- `cargo test -p magi-api --lib turn_harness::tests -- --test-threads=1`：58 passed、1 ignored。覆盖 ReadOnly 的 file/shell/Git/image 拒绝和只读读取、Restricted 的 workspace 内外写入、allow once、allow for turn、deny、cancel、expiry、Provider 请求次数与审批事件、跨 Turn/Session 授权隔离、FullAccess 的 workspace 内外写入，以及 Git dirty/branch drift/merge conflict 和 `git_branch_switch` API 审批生命周期。
- `cargo test -p magi-tool-runtime --lib -- --test-threads=1`：229 passed、1 ignored。覆盖全部内置工具策略分类、Browser 与 MCP 读写轴、shell/process/path 边界、workspace 作用域、写保护、取消和真实文件/进程副作用。

这些证据证明权限引擎和 Turn Harness 的基础轴已存在。新增 `restricted_profile_allow_for_turn_requires_new_approval_on_next_turn` 覆盖同一 Session 跨两个 Turn 及新 Session 的 `allow_for_turn` 隔离、三次真实文件副作用和三条审批请求；新增 `external_mcp_access_profile_matrix_records_executor_side_effects` 将外部 MCP 读/写工具 × 三种 AccessProfile 展开为 6 行，验证真实 executor 只接收允许的 4 行，并保留 Restricted 写工具的 `NeedsApproval` 与 ReadOnly 写工具的硬拒绝；新增 `structured_git_access_profile_matrix_blocks_mutations_before_executor` 将 Git 读/写 × 三种 AccessProfile 展开，验证三次 Git 读调用和仅一次 FullAccess 写调用到达 executor；新增 `browser_access_profile_matrix_records_host_executor_side_effects` 将 Browser snapshot/navigate × 三种 AccessProfile 展开为 6 行，验证每行均经过带 session/workspace 上下文的 host executor，并保留 Browser capability 对读写动作的分类断言；新增 `browser_access_profile_matrix_reaches_real_host_protocol_for_read_and_write` 通过 Unix WebSocket 接收真实 `BrowserHostClient` 的 `CreatePage`、`RestorePage`、`EnsureSurface`、`Snapshot`、`UpdateControl` 和 `Navigate` 请求；新增 `internal_process_access_profile_matrix_records_lifecycle_side_effects` 覆盖 process 的启动、读取、写入、终止四个阶段，并把 ReadOnly/Restricted/FullAccess 的 launch 结果登记为结构化 JSON 行；新增 Git API 层七条审批生命周期测试，验证结构化 Git mutation 经过真实 runtime executor 后的 branch 副作用、重复请求幂等、跨 Turn/Session 授权隔离或 fail-closed 结果。C 仍不能关闭：Browser Host 证据使用确定性协议 double，尚不等价于真实 Chromium 外部副作用；仍缺少一份可审计的组合表，把 file/shell/process/Git/browser/MCP × 三种 AccessProfile × workspace 内外 × allow once/allow for turn/deny/cancel/expiry/duplicate/cross-turn/session 逐项关联到同一轮的副作用、审批事件和 Provider 请求次数；其它工具类型、重复请求和 Electron/真实 Provider 权限组合也尚未全部覆盖。

当前组合覆盖按轴记录如下：

| 轴 | 已有直接证据 | 当前缺口 |
| --- | --- | --- |
| file | ReadOnly 读/写拒绝、Restricted 工作区写入与审批、FullAccess 写入；`file_remove`、`file_patch`、`file_mkdir`、`file_copy`、`file_move` 有真实副作用断言 | 各工具在三种 AccessProfile 与 workspace 内外的完整生命周期组合尚未逐项导出 |
| shell/process | ReadOnly 显式只读 shell、写 shell 拒绝；Restricted shell 审批/允许/拒绝/取消/过期；process 规则在 runtime 矩阵中覆盖 | process 真实副作用和 duplicate/cross-session 仍主要依赖 runtime/API 分散测试 |
| Git | dirty、branch drift、merge conflict 的 Provider 不调用或终态失败证据已有；`structured_git_access_profile_matrix_blocks_mutations_before_executor` 验证 Git 读/写在三种 AccessProfile 下的 executor 到达边界；API harness 七条 `git_branch_switch` 审批测试覆盖 allow once/deny/cancel/expiry、duplicate、cross-turn 和 cross-session 的真实分支副作用与 canonical 终态，并登记到 `/tmp/magi-api-git-approval-matrix.json` | 其它 Git mutation 的统一 JSON 登记、GUI 可见错误和真实 Electron/Provider 组合尚未合并 |
| browser/MCP | `browser_access_profile_matrix_records_host_executor_side_effects` 覆盖注入 host executor；`browser_access_profile_matrix_reaches_real_host_protocol_for_read_and_write` 覆盖 BrowserHostClient Unix WebSocket 的 snapshot/navigate 请求、Surface 绑定与页面状态；`magi-tool-runtime` 已覆盖能力分类、读写策略和协议状态 | 真实 Chromium Browser/MCP 外部副作用、三种 AccessProfile 的同轮 Provider 计数尚未完成 |
| AccessProfile / scope | ReadOnly、Restricted、FullAccess；workspace 内外；`allow_for_turn` 跨 Turn/Session 隔离已有 | 每个工具类型尚未形成统一的行级矩阵和可复核 JSON |
| lifecycle | allow once、allow for turn、deny、cancel、expiry、duplicate 待审批回放均有代表性测试 | 组合表尚未覆盖每个工具类型的所有生命周期格子 |

因此 C 当前新增的是 duplicate 审批回放证据，剩余工作是把已有分散测试整理为统一可审计矩阵，并补齐 Browser/MCP、process 和 Git 的真实组合缺口。

### C 组合矩阵登记（当前已覆盖项）

下面的登记把已有测试按同一组审计字段展开：工具面、AccessProfile、workspace 作用域、生命周期动作、真实副作用、审批事件和 Provider 请求次数。`—` 表示该路径按设计不应产生对应事实，不表示缺少断言。

| 工具面 | AccessProfile / 作用域 | 生命周期 | 副作用与审批事实 | Provider 请求 | 直接测试 |
| --- | --- | --- | --- | ---: | --- |
| `file_write`、`file_copy`、`file_move`、`file_patch`、`file_mkdir`、`file_remove` | ReadOnly / workspace 内 | deny | 无文件副作用；不发布审批 | 1 或 0（按拒绝阶段） | `read_only_profile_rejects_explicit_*` |
| `file_write`、`file_patch`、`file_mkdir`、`file_copy`、`file_move` | Restricted / workspace 内 | auto allow | 真实文件副作用；无 pending approval | 2 | `restricted_profile_auto_allows_*_inside_workspace` |
| `shell_exec`、`file_remove` | Restricted / workspace 内 | allow once | 真实写入或删除；发布 requested/resolved | 2 | `restricted_profile_approval_allows_original_write_tool_once`、`restricted_profile_file_remove_allow_once_deletes_target_without_retry` |
| `shell_exec` | Restricted / workspace 内 | duplicate pending | 只保留一个 pending approval；只执行一次副作用 | 1→2 | `restricted_profile_duplicate_request_replays_pending_approval_without_duplicate_side_effects` |
| `shell_exec`、`file_remove` | Restricted / workspace 内 | deny / cancel / expiry | 无副作用；Turn 失败或取消；pending 收口 | 1 | `restricted_profile_approval_denial_*`、`restricted_profile_pending_approval_is_cancelled_with_turn_without_side_effect`、`restricted_profile_expired_approval_rejects_without_side_effect` |
| `file_*`、`apply_patch`、`shell_exec` | Restricted / workspace 外 | reject | 不写入工作区外；不进入执行器或不创建审批 | 0 或 1 | `restricted_profile_rejects_*_outside_workspace`、`registry_rejects_outside_shell_path_before_approval` |
| `file_*`、`shell_exec`、Git 写工具 | FullAccess / workspace 内外 | auto allow | 允许的真实副作用；不发布常规审批 | 2 或按 Git 前置失败 | `full_access_profile_*`、`workspace_task_with_git_*` |
| Browser 读写能力 | ReadOnly / Restricted / FullAccess | Host protocol side effect | snapshot/navigate 六行均到达带 session/workspace 上下文的 Host；Unix WebSocket 协议层记录 3 次 snapshot 与 3 次 navigate；能力快照仍按读写分类 | — | `browser_access_profile_matrix_keeps_read_and_write_capabilities_distinct`、`browser_access_profile_matrix_records_host_executor_side_effects`、`browser_access_profile_matrix_reaches_real_host_protocol_for_read_and_write` |
| 外部 MCP 读写能力 | ReadOnly / Restricted / FullAccess | deny / allow | ReadOnly 在 executor 前阻断，Restricted 写工具返回 NeedsApproval，FullAccess 和所有读工具产生真实 mock executor side effect；6 行 profile/tool 组合已在同一测试中登记 | — | `external_mcp_access_profile_matrix_records_executor_side_effects`、`external_mcp_*_profile*` |
| process 内部工具 | ReadOnly / Restricted / FullAccess | deny / allow / cancel | ReadOnly 拒绝、Restricted NeedsApproval；FullAccess 真实 cat 进程完成 launch/read/write/kill，并保留跨 session 隔离与取消清理；API Harness 另以 `shell_exec(background=true)` 验证 Restricted allow once 的真实文件副作用和审批事件，结果写入 `/tmp/magi-api-process-approval-matrix.json` | API Harness allow once 为 2；其它 runtime 行不涉及 Provider | `internal_process_access_profile_matrix_is_fail_closed`、`internal_process_access_profile_matrix_records_lifecycle_side_effects`、`process_tools_do_not_cross_sessions_with_workspace_only_context`、`restricted_profile_background_shell_approval_runs_real_process_once` |

该登记仍不是关闭矩阵：Browser/MCP 的真实外部副作用、process 的完整审批生命周期、其它 Git mutation 的统一 JSON 登记，以及每一行统一 JSON 产物仍需补齐。当前 Git artifact 覆盖 `git_branch_switch` 七行代表格，process artifact 只覆盖后台进程 allow once 一行。最近一次定向验证为 `magi-api turn_harness`：59 passed、1 ignored；`magi-tool-runtime`：229 passed、1 ignored；`magi-api browser_tool_runtime`：14 passed。

## D 工作包首轮证据

- 真实 Provider/daemon 五类场景各 20 轮后端证据已存在：`/tmp/magi-real-provider-perf-personal20.json`、`/tmp/magi-real-provider-perf-workspace20.json`、`/tmp/magi-real-provider-perf-tool20.json`、`/tmp/magi-real-provider-perf-subagent20.json`。
- `cargo test -p magi-api --lib turn_harness::tests -- --test-threads=1` 的 59 个通过测试已覆盖真实 `TurnService` 链路中的取消、SSE/WebSocket reconnect、duplicate request、Task/Goal/工具/子代理、Provider 重试、Git dirty/branch drift/merge conflict、`git_branch_switch` 审批生命周期、后台进程审批、Task restart replay 和部分权限审批恢复；`magi-daemon` 的 session restart/history 测试也分别通过既有恢复测试、3 个 runtime restart 测试和新增的 daemon Task Turn restart/replay 测试。

D 仍不能关闭。新增 `task_profile_restart_replays_completed_turn_without_provider_reexecution`、真实 HTTP daemon 实例重启和独立 `magi-daemon-app` OS 进程重启后的 Task Turn 回放；API harness 现在还覆盖 Git `git_branch_switch` 审批 allow once/deny/cancel/expiry、duplicate、cross-turn、cross-session 的 Provider 请求次数、审批事件、真实 branch 副作用与 canonical 终态。缺口仍是 history/replay 的更多场景、三种 AccessProfile 与 Git/审批场景的 Provider 级同轮证据，以及把这些后端阶段与 Electron Renderer 的 20 轮 `turnId` 逐轮关联；已有后端性能 JSON 不能直接扩大解释为 Provider 全矩阵完成。

## E 工作包首轮证据

最新打包 Electron/CDP 回归 `/tmp/magi-electron-dom-regression-10201.json` 为 `status=passed`，64 项检查通过、35 次 Provider 请求；此前 `/tmp/magi-electron-dom-regression-10142.json` 为 61 项检查通过。现有覆盖包括个人/工作区 Chat、Task 工具卡片和工具组展开、Goal/Plan 成功卡片和二级展开、Goal Provider 失败后重新建目标恢复、子代理工具卡片与 `child_task_id`、代理运行中心、ReadOnly 写入拒绝、Restricted 审批卡片与允许一次后的真实工作区副作用、daemon restart、Renderer reload、history/session switch 和 cancel。

E 仍保持部分完成。已补齐 Goal/Plan 的一个失败/恢复场景和 Restricted 审批允许/拒绝/取消场景；尚缺 Agent drawer 的更多状态组合、Git dirty/drift/conflict 的可见错误、审批过期的 GUI 组合以及 reconnect/history/restart 的组合矩阵；73 项回归不能替代这些组合。

## 关闭规则

任何工作包只有在代码或脚本变更、直接证据、定向测试和文档记录都齐全后，才能从“进行中”改为“已完成”。单轮证据不能替代多轮矩阵；当前 after 数据不能替代 before 数据；生产路径收敛不能替代测试 fixture 边界审计。
