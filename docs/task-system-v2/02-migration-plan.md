# Task System v2 落地路径（单次彻底切换）

更新时间：2026-05-14
状态：设计稿

## 总策略：分支隔离 + Cutover Day

v2 不在主干上渐进切换。具体形态：

1. **隔离分支**：开一条长期 feature branch `feat/task-system-v2`，v2 全部 21 层在该分支上完整实现。
2. **主干冻结结构性改动**：分支存续期间，主干**不允许**对任务系统相关模块（`task_llm_loop` / `task_runner` / `task_store` / `dispatch_execution` / 相关 routes）做结构性改动，仅允许 bug fix。
3. **Cutover Day 单次切换**：v2 在分支上验证完成后，一次性合并到主干，同一个合并 commit 删除老代码。合并完成的瞬间主干上**零双轨、零兼容层、零 feature flag**。
4. **不可回滚**：合并后不保留"切回 v1"的能力。任何主干上残留的 v1 引用都是 bug。

这与 cn-engineering-standard 完全一致：不让同一功能长期并存多种实现、不保留回退逻辑、不用过渡模式掩盖问题。

## 与"9 Step Rollout"模式的对比

旧渐进模式的根本问题：**每个 Step 上线时主干进入 v1+v2 混合状态**，即使每 Step 收尾清理了被替换的代码，Step 之间仍然存在"v2 部分上线 / v1 部分残留"的过渡期。这是隐式的双轨，与工程红线冲突。

单次切换模式把所有过渡状态压到隔离分支内部，主干永远是单实现。

## 分支生命周期

```
main ─────────────────────────────────────────────────────●─────────→
                                                          │ Cutover Day
                                                          │ (squash or single merge)
                                                          │ 删除 task_llm_loop / task_runner /
                                                          │     task_store / 旧 dispatch / 旧 routes
                                                          │
feat/task-system-v2: ─P1─P2─P3─P4─P5─P6─P7─[verify]──────●
                      │  │  │  │  │  │  │
                      │  │  │  │  │  │  └─ Phase 7  Tier 4 全量（L18-L21）
                      │  │  │  │  │  └──── Phase 6  Tier 4 骨架（L15-L17）
                      │  │  │  │  └─────── Phase 5  Tier 3（L10-L14）
                      │  │  │  └────────── Phase 4  SpawnGraph（L5）
                      │  │  └───────────── Phase 3  AgentRole + Permissions（L4 + L7）
                      │  └──────────────── Phase 2  Mailbox + Streaming（L3 + L8）
                      └─────────────────── Phase 1  Conversation + Turn + SessionStore（L1 + L2 + L9）
```

每 Phase 在分支上独立 commit，commit message 标注 `[v2/PX]`。这是为了 review 体验，不是为了上线节奏——所有 Phase 一次性切换。

## Phase 内容明细

### Phase 1 — Conversation / Turn / SessionStore（L1 + L2 + L9）

**位置**：新 crate `magi-conversation-runtime/`

**内容**：
- `src/conversation.rs`：Conversation 主结构 + advance_turn 主循环
- `src/turn.rs`：Turn 状态机（Pending → Modeling → ToolCalling → Done/Failed）
- `src/session.rs`：单会话持久化（取代 `task_store.rs` 中会话相关部分）

**分支内对老代码的操作**：**不删**。`task_llm_loop.rs` 与 `task_runner.rs` 在分支上仍存在，但**不再被 wire**。新 runtime 直接接入 magi-api 的入口处。

### Phase 2 — Mailbox + Streaming（L3 + L8）

**位置**：`magi-conversation-runtime/src/mailbox.rs`、`magi-event-bus/` 扩展

**内容**：
- Mailbox 完整实现（user / agent / system / parent / child 五类 author，五类 kind，trigger_turn 语义）
- user input、guide button、subagent result 三条入口全部接 Mailbox
- 删除 `root_task.context_refs` dead path 的所有引用（**在分支内删，主干上还在**）
- Streaming 统一管道：模型 token / tool event / system signal 同一 stream 派生订阅

### Phase 3 — AgentRole + Permissions（L4 + L7）

**位置**：`magi-conversation-runtime/src/role.rs`、新 crate `magi-permissions/`

**内容**：
- `~/.magi/roles/` TOML 配置目录 + 默认 role 集
- 所有硬编码 prompt 提到 role 文件
- 工具白名单按 role 生效
- 三维 permission（工具 / 目录 / 命令）+ 模式（default / acceptAll / acceptEdits / plan / bypassPermissions）

### Phase 4 — SpawnGraph（L5）

**位置**：新 crate `magi-spawn-graph/`

**内容**：
- SpawnEdge 持久化（参考 `codex-rs/agent-graph-store/`）
- 父子 Conversation 关系图，open/closed 状态
- 子代理回执路由（child completion → parent.Mailbox）
- 级联停止 + 最大深度 + 最大扇出

### Phase 5 — Tier 3 全量（L10 + L11 + L12 + L13 + L14）

**位置**：新 crate `magi-coordinator/`、`magi-task/`、`magi-todo-ledger/`、`magi-project-memory/`

**内容**：
- CoordinatorPrompt 注入（参考 claude-code 369 行模板）
- Task trait + 7 个变体：`local_agent` / `local_bash` 完整实现，`local_workflow` / `remote_agent` / `monitor_mcp` / `in_process_teammate` / `dream` 实现到 stub trait 满足，stub 内部 `unimplemented!()`
- `Agent` / `SendMessage` / `TaskStop` 三个工具
- SafetyGate 拦截规则集
- TodoLedger（in-session）
- ProjectMemory（`~/.magi/projects/{slug}/memory/MEMORY.md` 自动加载）

### Phase 6 — Tier 4 骨架（L15 + L16 + L17）

**位置**：新 crate `magi-mission/`

**内容**：
- MissionCharter（含 freeze 语义）
- Plan 树 + 节点状态机
- Workspace 初始化（git worktree）
- `magi-cli mission new` / `mission plan` / `mission status`

### Phase 7 — Tier 4 完整（L18 + L19 + L20 + L21）

**位置**：新 crate `magi-knowledge-graph/`、`magi-validation-runner/`、`magi-checkpoint/`、`magi-human-checkpoint/`

**内容**：
- KG：内置 SQLite + symbol_map / decision_log / risk_register 三表，向量索引 trait 化（先 BM25 实现）
- ValidationRunner：按 Task L11 实现
- Checkpoint：fsync + atomic rename + 三方 hash 校验（KG / Plan / Workspace）
- HumanCheckpoint：协议 + 前端配套桩位（前端可以最后做）

## Verify 阶段

Phase 7 完成后进入 Verify。**Cutover Day 前必须全部通过**：

1. **三档冒烟**
   - A 档：单代理对话 100 轮，无 SSE 差异（与 main 录制的 SSE 帧 byte-level 对比）
   - B 档：Coordinator + 3 worker 并行调研一个问题，端到端完整通过
   - C 档：跑通 demo Mission（"重构小工具 X"），含一次主动 Checkpoint + 一次 process kill 后 resume + 一次 HumanCheckpoint
2. **回归测试**：cargo workspace 全量 test 通过
3. **性能验证**：对照 `01-architecture.md` 第 7 节性能预算，关键路径 p95 不退化
4. **行数核查**：分支 net diff 与 `01-architecture.md` 第 8 节预估对齐（±15%）
5. **死代码扫描**：分支上仍存在的老 `task_llm_loop.rs` / `task_runner.rs` / `task_store.rs` 中的对外引用必须为 0（除了即将删除它们的最终 commit）

## Cutover Day 操作清单

Cutover Day 必须在一个工作日内完成，分支不允许"切到一半"。

### D-1（切换前一天）

- [ ] 主干 freeze：宣布 24 小时内不接受任何 PR 合并
- [ ] 分支最后一次 rebase main，解决冲突
- [ ] 跑完整测试套件 + 三档冒烟
- [ ] 准备 cutover commit 草稿（见下方）

### Cutover Day

按顺序在同一个 commit 完成：

1. **删除整批老代码**（预计 ~12000 行）：
   - `crates/magi-api/src/task_llm_loop.rs`
   - `crates/magi-api/src/dispatch_execution.rs` 中 v1 任务编排相关
   - `crates/magi-orchestrator/src/task_runner.rs`
   - `crates/magi-orchestrator/src/task_store.rs`（保留 schema 中被 v2 复用的部分）
   - `crates/magi-core/src/task.rs` 中 v1 Task 类型
   - `crates/magi-api/src/session_turn_execution.rs` 中老路径
   - 旧 routes 中只服务 v1 的 endpoint

2. **`docs/task-orchestration-upgrade/` 整个目录删除**（其 README 已标记废弃）

3. **`MEMORY.md` 同步**：移除指向旧任务系统的所有 reference 类记忆

4. **`docs/README.md` 移除对 `task-orchestration-upgrade/` 的引用**

5. 提交 + 推 main：commit message `feat(task-system-v2): cutover from v1 to v2 — single switch, no dual track`

6. 跑一遍主干 CI：必须全绿

7. 发版本号 tag `task-system-v2-cutover-YYYYMMDD`

### D+1

- [ ] 主干 CI 持续 24 小时无回归
- [ ] 解除主干 freeze
- [ ] 监控真实流量 48 小时（如适用）

## 主干 freeze 范围

分支存续期间（预计 12-16 周），主干上**禁止**改动：

- `crates/magi-api/src/task_llm_loop.rs`
- `crates/magi-api/src/dispatch_execution.rs`
- `crates/magi-orchestrator/src/task_runner.rs`
- `crates/magi-orchestrator/src/task_store.rs`
- `crates/magi-core/src/task.rs`
- `crates/magi-api/src/session_turn_execution.rs`
- 涉及上述模块的 routes

主干上**允许**改动：

- 前端、web、UI
- 协议 DTO（如必须，需先在分支上同步）
- 其他 crate（`magi-event-bus` 与 v2 集成的部分例外，需 case-by-case 评审）
- bug fix 类小改（不动结构）

冻结违规由 PR review 强制。`feat/task-system-v2` 分支负责人对所有违规 PR 有否决权。

## 风险与缓解

| 风险 | 缓解 |
|------|------|
| 长分支 rebase 地狱 | 每周 rebase 一次；冻结主干结构改动；分支负责人保护权 |
| Cutover Day 在生产环境炸 | Verify 阶段三档冒烟 + 性能预算 + 死代码扫描三道闸 |
| 业务团队"等不及 v2" | 主干 freeze 仅限任务系统结构，其他改动正常推进 |
| 中途团队成员变更 | 设计文档（本目录）覆盖足够细节，新成员可接手 |
| Phase 提交过粗导致 review 不动 | 单 Phase commit 超过 3000 行强制拆 |
| Cutover commit 巨大无法 review | Verify 阶段团队提前过完分支，Cutover commit 只是"按下按钮" |
| 老代码删除后发现遗漏 | Verify 第 5 项死代码扫描；万一漏掉则单独 hotfix PR |

## 与 P7 / canonical turn log 的依赖关系

v2 落地前必须完成：

1. **P7 单信号契约收敛**（[`../p7-compliance-collapse.md`](../p7-compliance-collapse.md)）：thread/worker 双轨残余必须先清，否则污染 v2 的 Conversation 抽象。
2. **canonical turn log 重构**（[`../canonical-turn-log-refactor-plan.md`](../canonical-turn-log-refactor-plan.md)）：v2 的 L2 Turn 直接复用其 schema。

这两项在主干上独立完成，**不进入 v2 分支**。完成后才开 v2 分支。

## 时间估计

- 前置 P7 + canonical turn log：2-3 周（主干上完成）
- Phase 1-4（Tier 1 + L7/L9）：4 周
- Phase 5（Tier 3）：3 周
- Phase 6（Tier 4 骨架）：2 周
- Phase 7（Tier 4 完整）：3 周
- Verify + Cutover Day：1 周

**总预期：15-17 周**

中间不允许"提前合并一部分"——要么 Cutover Day 一次切完，要么 v2 分支整个废弃重来。
