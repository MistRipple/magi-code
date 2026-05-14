# Task System v2 落地路径（主干 Slice 热换）

更新时间：2026-05-14
状态：设计稿

## 总策略：主干 Slice 热换，每 commit 单实现

v2 不开长分支。每个 commit = 一个 slice 的热换：

1. **不开 feat 分支**：v2 全部改动直接在 main 上推进。分支隔离本身是一种"主干长期看不见 v2、分支长期看不见主干"的隐式双轨，不接受。
2. **slice = 一个 commit**：每个 commit 取一个能独立成立的 slice（一段调用面），同一 commit 内：
   - 写新 v2 实现
   - 删除被替换的 v1 实现
   - 把 v1 调用方切到 v2
3. **主干恒定单实现**：每个 commit 落地后，被该 slice 覆盖的职责在 main 上**只有 v2 一份实现**，不允许"v2 已加 / v1 还在"的并存中间态。
4. **slice 必须自立**：slice 内的 v2 实现不允许依赖任何还没落地的 v2 模块。如果某 slice 需要 v2 模块 X 才能跑，X 必须**先**作为前置 slice 落地。
5. **不可回滚**：每个 commit 落地即定型。任何"切回 v1"的需求都是 bug，不留 feature flag、不留 alt path。

这与 `cn-engineering-standard` 完全一致：不让同一功能长期并存多种实现、不保留回退逻辑、不用过渡模式掩盖问题。

## 为什么不开分支

旧"分支隔离 + Cutover Day"方案表面看保证了主干始终是单实现（v1），但代价是：

- 分支侧承担数千行 v2 代码与主干长期不互见 12+ 周，分支与主干本身就是双源
- Cutover Day 是一次性大手术，单 commit ~10000 行无法 review
- 分支内 v1 与 v2 并存（"P1 之后 task_llm_loop 在分支上仍存在但不再被 wire"）也是双轨，只是被"隔在分支里"
- 任何主干上 12 周内对任务系统的紧急修复都要双写

Slice 热换把 v2 的不可见双轨成本换成主干上"每 commit 都看得见单实现状态"的可审计性。代价是 v2 必须**逐层从外向内**重写，无法先建好整套底座再切换。

## Slice 的合规标准

一个 slice 想合规落地，必须满足：

1. **同一 commit 内**：写新 v2 实现 + 删除被换的 v1 实现 + 切换所有现存调用点
2. **零并存**：commit 落地后，被该 slice 覆盖的职责在 main 上不存在 v2 / v1 两份
3. **零未来依赖**：slice 内不允许 import 还未落地的 v2 模块、不允许引用未来才会存在的 trait
4. **测试随 commit 更新**：v1 测试改写或删除，v2 测试同 commit 提交
5. **commit message 标注**：`feat(v2/slice/<slice-name>)`，slice-name 描述被替换的职责面

## Slice 拆解原则

按"从外到内"拆 slice，外层 slice 先落地：

- **外层** = 调用边界 / 入口（routes、CLI、bridge）
- **中层** = 编排层（task_runner / dispatch_execution / session_turn_execution）
- **内层** = 数据/存储原语（task_store / canonical turn log）

外层切换时，v2 内部可以暂时复用 v1 的中/内层 primitives（v1 仍是单实现，只是被 v2 外层调用）。每次中层 slice 落地，把上一波 v2 外层从"调用 v1 中层"切换到"调用 v2 中层"，同一 commit 删除被废弃的 v1 中层。

这条原则确保**每个 commit 落地时，"该层"在 main 上只有一份实现**——即使整套 v2 还没完整，已落地部分始终是单实现。

## Slice 候选清单

按落地顺序排列。每个 slice 都标注：被换 v1 范围、v2 新增内容、依赖的前置 slice、估算行数。

> ⚠️ 行数估算是上限；实际操作时按"不超过 1500 行 net diff"硬切，超过强制再拆。

### Slice S1 — User Message Mailbox 入口

**被换 v1 范围**：`routes/mod.rs::session_action_route` / `session_turn_route` 中 user input 进入任务系统的那段——把 user message 推进系统的当前路径（runs through `session_turn_execution` → `task_llm_loop`）

**v2 新增**：`magi-conversation-runtime` 新 crate 起手，仅含：
- `Mailbox`（user 类入栈接口、Turn 边界 drain）
- `Conversation` 最小骨架（包一个 SessionId + Mailbox 引用）

**v1 -> v2 切换**：routes 收到 user input 后，先 `conversation.mailbox.push(MailboxItem::user(...))`，然后由 conversation 调起一个调用 v1 task_llm_loop 的 thin adapter（v1 仍是真正的执行机）。

**这一步替换的是"user 信号入栈姿势"，task_llm_loop 这层不动。**

**估算**：~600 行新增（Mailbox + Conversation 骨架 + adapter），~200 行 v1 删除（直接调 task_llm_loop 的入口路径），~150 行测试。

**前置依赖**：无。

### Slice S2 — Turn 状态机 + 单 Conversation 不并发不变式

**被换 v1 范围**：`task_llm_loop` 中"一轮对话的状态推进"代码段（Pending → Modeling → ToolCalling → Done/Failed 的等价逻辑）

**v2 新增**：`magi-conversation-runtime/src/turn.rs` Turn 状态机；Conversation::advance_turn 主循环；同 Conversation 不并发 Turn 不变式（lock guard）

**v1 -> v2 切换**：task_llm_loop 中的状态推进段被 advance_turn 整体替换。task_llm_loop 仅保留模型 IO + 工具 IO 段。

**估算**：~1200 行新增，~800 行 v1 删除。

**前置依赖**：S1。

### Slice S3 — Streaming 统一管道

**被换 v1 范围**：`session_turn_writeback.rs` + `dispatch_execution.rs` 中模型 token 流、工具事件流的分支化写入

**v2 新增**：单一 stream 派生订阅（模型 token / tool event / system signal 同一来源派生）

**估算**：~700 行新增，~500 行 v1 删除。

**前置依赖**：S2。

### Slice S4 — AgentRole 拉到 ~/.magi/roles/

**被换 v1 范围**：所有硬编码 role prompt（task_llm_loop / dispatch_execution 中按硬编码 role 选择 system prompt 的分支）

**v2 新增**：`role.rs` + TOML 加载器 + 默认 role 集打包

**估算**：~500 行新增，~400 行 v1 删除。

**前置依赖**：S2。

### Slice S5 — Permissions 三维模型

**被换 v1 范围**：当前散落的 read-only 判定、工具白名单 if-else

**v2 新增**：`magi-permissions` crate，三维 permission（工具 / 目录 / 命令）+ 五种模式

**估算**：~800 行新增，~300 行 v1 删除。

**前置依赖**：S4。

### Slice S6 — SpawnGraph + 子代理回执路由

**被换 v1 范围**：`dispatch_execution.rs` 中 worker 派发与父子关系记录的零散逻辑、worker final 回到主线的胶水代码

**v2 新增**：`magi-spawn-graph` crate；父子 Conversation 关系；child final → parent.Mailbox 的统一回执路由

**估算**：~1000 行新增，~700 行 v1 删除。

**前置依赖**：S2、S3。

### Slice S7 — CoordinatorPrompt + Task trait 双变体

**被换 v1 范围**：B 档（多代理）协调的 v1 实现（如果有专门的代码段；否则这一 slice 是纯加，跳过删除）

**v2 新增**：CoordinatorPrompt 注入；Task trait + `local_agent` / `local_bash` 完整实现；`Agent` / `SendMessage` / `TaskStop` 三个工具

**估算**：~1500 行新增，~500 行 v1 删除。

**前置依赖**：S6。

### Slice S8 — SafetyGate

**被换 v1 范围**：现存散落的命令拦截规则

**v2 新增**：SafetyGate 规则集 + 拦截器接入点

**估算**：~400 行新增，~200 行 v1 删除。

**前置依赖**：S5、S7。

### Slice S9 — TodoLedger（in-session）

**被换 v1 范围**：当前 task graph 中 Todo 相关字段 / 接口（如有）

**v2 新增**：TodoLedger trait + 实现

**估算**：~600 行新增，~300 行 v1 删除。

**前置依赖**：S7。

### Slice S10 — ProjectMemory

**v1 范围**：纯加（v1 无对应）

**v2 新增**：`~/.magi/projects/{slug}/memory/MEMORY.md` 自动加载、auto-save 接口、prompt 注入

**估算**：~600 行新增，几乎无 v1 删除。

**前置依赖**：S7。

### Slice S11~S15 — Tier 4 (C 档) 五个 slice

依次：MissionCharter / Plan / Workspace / KnowledgeGraph / ValidationRunner。

每个 slice 都是 C 档专用，A/B 档代码路径不动。

**前置依赖**：S10。

### Slice S16~S17 — Checkpoint + HumanCheckpoint

C 档闭环最后两步。

**前置依赖**：S11~S15。

### Slice S18 — v1 收尾删除

最后一个 slice，把 main 上残留的 v1 task system 代码（如有）整体清除。理想情况下这一步是空 commit，因为前面所有 slice 都已经按规则在落地时删除被换 v1。

实际可能残留：v1 类型的孤儿、未被引用但仍存在的死代码、`task-orchestration-upgrade/` 整个文档目录。

**估算**：纯删除，~500-1500 行。

## 每 slice commit 模板

```
feat(v2/slice/<slice-name>): <slice 一句话描述>

被换 v1：
- <文件:函数> / <文件:函数> ...

v2 新增：
- <crate/模块/类型/接口> ...

合规自检：
- [ ] 同 commit 写新 + 删旧 + 切调用点
- [ ] 落地后 main 上该 slice 职责无 v1/v2 并存
- [ ] slice 内零未来依赖
- [ ] 测试随 commit 更新
- [ ] net diff ≤ 1500 行（否则继续拆）

验证：<具体跑过的命令>
```

## 验证（每 slice commit 都跑）

按 `cn-engineering-standard`：每 slice 落地前必须跑过：

1. `cargo build --workspace`（含新 crate）
2. `cargo test -p <被改 crate> --lib`
3. 若改 web：`cd web && npm run check`、`cd web && npm run test:canonical`
4. 受影响场景的 smoke（按 slice 涉及面定义，至少含 A 档单代理对话）

任何一项不过，slice **不算落地**，继续修，不允许"先合再补"。

## 主干禁止

slice 推进期间，main 上**禁止**：

- 同时存在 v1 与 v2 两份实现某职责（哪怕只有一行）
- 把"待后续 slice 清理"作为注释延期 v1 删除
- 引入 feature flag / `cfg!(v2)` / `if config.use_v2` 类二选一开关
- 引入 alt path / fallback path / compat shim
- 引入 `Option<V2Field>` 的 v2 字段（直接非 Option 加入；测试 fixture 同 commit 更新）

任何上述模式出现一次，本次 slice 视为合规失败，需在同 commit 修正后才能合并。

## 时间预期

- 一个 slice 视复杂度 0.5–2 周
- 17 个 slice 约 12–20 周
- 时间预算与原 branch + cutover 方案相同，但收益是：**每周都能交付一个主干清亮的 commit**，而不是 16 周后一次性吞下一个万行 commit

## 风险与缓解

| 风险 | 缓解 |
|------|------|
| Slice 切不动（v2 实现需要 v1 中层但 v1 中层下一波才换） | 落地前先识别依赖链；如某 slice 必须依赖未来 slice，则该 slice 强制延后 |
| Net diff 失控 | 单 slice 不超过 1500 行硬切；超过强制再拆 |
| 落地 slice 引入回归 | 每 slice 验证清单必须跑；回归即 slice 内修复，不准 "下个 slice 修" |
| v1 中层在新外层切完前被改 | 该层 slice 落地前主干仍可正常迭代 v1（不冻结） |
| 多个 slice 紧邻冲突 | 串行落地，不允许并发 v2 slice PR |

## 与原"branch + Cutover"方案的对照

| 维度 | 原 branch 方案 | Slice 热换 |
|------|---------------|-----------|
| 主干始终单实现 | 是（v2 在分支） | 是（每 commit 切换） |
| 分支侧 v1+v2 并存 | 是（不可避免） | 否（无分支） |
| 大 cutover 风险 | 一次性万行 | 无（拆为 17 slice） |
| 主干 review 体验 | 12 周看不见 v2 | 每周一个新 slice |
| 紧急修复双写 | 是 | 否 |
| 总时长 | 15-17 周 | 12-20 周 |

## 与 P7 / canonical turn log 的依赖关系

主干前置已完成（见 `docs/p7-compliance-collapse.md` 与 `docs/canonical-turn-log-refactor-plan.md`）：

- P7 单信号契约收敛已落地
- canonical turn log 重构阶段 0-6 已落地

v2 slice 直接以当前 main 为起点推进，不依赖任何额外前置。
