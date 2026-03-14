# Claude Code Agent 架构 vs Magi 架构对比分析

> 来源：[Learn Claude Code](https://learn-claude-agents.vercel.app/zh/timeline/) — 从 0 到 1 构建 nano Claude Code-like agent
>
> 对比项目：Magi — AI 多 Agent 编排 VS Code 插件

---

## 一、Claude Code Agent 各阶段总结

### s01 — Agent 循环 (Agent Loop)

| 维度         | 内容                                                           |
| ------------ | -------------------------------------------------------------- |
| **分类**     | 工具与执行                                                     |
| **核心机制** | `while stop_reason == "tool_use"` 循环                         |
| **工具数**   | 4 个基础工具 (bash, read_file, write_file, edit_file)          |
| **关键洞察** | Agent 本质上是一个 "LLM 调用 → 工具执行 → 结果返回" 的无限循环 |

**工作原理：** 最简的 Agent 就是一个 while 循环：向 LLM 发请求，LLM 返回 `tool_use` 时执行工具、把结果追加到 messages，再发一次。直到 LLM 决定不调用工具（`stop_reason != "tool_use"`），循环结束。

---

### s02 — 工具规划与协调 (Tool Planning)

| 维度         | 内容                                                 |
| ------------ | ---------------------------------------------------- |
| **分类**     | 工具与执行                                           |
| **核心机制** | 扩展工具集 + 工具输出规范化                          |
| **工具数**   | 5 个工具 (+ list_dir)                                |
| **关键洞察** | 工具设计决定 Agent 能力边界；好的工具描述 = 好的规划 |

**工作原理：** 在 s01 的基础上增加更多工具，并确保每个工具的 `description` 足够精确，让 LLM 能正确选择和组合使用。

---

### s03 — TodoWrite (结构化任务追踪)

| 维度         | 内容                                             |
| ------------ | ------------------------------------------------ |
| **分类**     | 规划与协调                                       |
| **核心机制** | 内存中的 TodoManager，扁平的 todo 清单           |
| **工具数**   | 5 个 (基础 + todo_write)                         |
| **关键洞察** | 给 Agent 一个 "记事本"，它就能自己拆分和追踪任务 |

**工作原理：** `TodoManager` 是一个内存中的列表，Agent 通过 `todo_write` 工具创建/更新/完成条目。本质是让 LLM 外化思维过程，但缺陷是内存中存活、无依赖关系。

---

### s04 — 子 Agent (Sub-Agents)

| 维度         | 内容                                        |
| ------------ | ------------------------------------------- |
| **分类**     | 规划与协调                                  |
| **核心机制** | 独立 `messages[]` 的一次性子 Agent          |
| **工具数**   | 5 个 (基础 + task)                          |
| **关键洞察** | 子 Agent 有独立上下文，完成后返回摘要即销毁 |

**工作原理：** 父 Agent 调用 `task` 工具 → 生成一个新的 Agent Loop（独立的 `messages[]`）→ 子 Agent 干完活返回文本摘要 → 摘要作为 `tool_result` 注入父 Agent。子 Agent 是 **一次性的**：生成 → 执行 → 返回 → 销毁。

---

### s05 — 技能 (Skills / Load on Demand)

| 维度         | 内容                                                  |
| ------------ | ----------------------------------------------------- |
| **分类**     | 规划与协调                                            |
| **核心机制** | 两层技能注入：系统提示放索引，tool_result 放全文      |
| **工具数**   | 5 个 (基础 + load_skill)                              |
| **代码量**   | 187 LOC                                               |
| **关键洞察** | "用到什么知识, 临时加载什么知识" — 不塞 system prompt |

**工作原理：**

- **第一层（便宜）：** System prompt 中仅列出技能名和简短描述（~100 tokens/skill）
- **第二层（按需）：** 当 LLM 调用 `load_skill("git")` 时，完整的 SKILL.md 内容（~2000 tokens）通过 `tool_result` 注入

`SkillLoader` 扫描 `skills/*/SKILL.md` 文件，解析 YAML frontmatter 获取元数据，完整 body 作为按需内容。

---

### s06 — 上下文压缩 (Context Compression)

| 维度         | 内容                                                  |
| ------------ | ----------------------------------------------------- |
| **分类**     | 内存管理                                              |
| **核心机制** | 三层压缩策略                                          |
| **工具数**   | 5 个 (基础 + compact)                                 |
| **代码量**   | 205 LOC                                               |
| **关键洞察** | "上下文总会满, 要有办法腾地方" — 三层压缩换来无限会话 |

**三层压缩管线：**

```text
第一层: micro_compact (每轮静默执行)
  → 将 3 轮前的 tool_result 替换为 "[Previous: used {tool_name}]"

第二层: auto_compact (token > 50000 自动触发)
  → 保存完整 transcript 到磁盘
  → LLM 摘要当前对话
  → 替换所有 messages 为 [summary]

第三层: compact 工具 (LLM 主动调用)
  → 与 auto_compact 相同机制，手动触发
```

---

### s07 — 任务系统 (Task Graph + Dependencies)

| 维度         | 内容                                                    |
| ------------ | ------------------------------------------------------- |
| **分类**     | 规划与协调                                              |
| **核心机制** | 文件持久化的 DAG 任务图，带 blockedBy/blocks 依赖       |
| **工具数**   | 8 个 (基础 + task CRUD × 4)                             |
| **代码量**   | 207 LOC                                                 |
| **关键洞察** | "状态活在对话外面" — 文件持久化让任务图在压缩后依然存活 |

**工作原理：**

- 每个任务是 `.tasks/task_N.json`，包含 `id, subject, status, blockedBy[], blocks[]`
- 状态：`pending → in_progress → completed`
- 完成一个任务时，自动从其他任务的 `blockedBy` 中移除自己（解锁后续）
- 支持并行（无依赖关系的任务可同时执行）

这是后续所有多 Agent 机制（s08-s12）的协调骨架。

---

### s08 — 后台任务 (Background Tasks)

| 维度         | 内容                                 |
| ------------ | ------------------------------------ |
| **分类**     | 并发                                 |
| **核心机制** | 后台守护线程 + 通知队列              |
| **工具数**   | 6 个 (基础 + background_run + check) |
| **代码量**   | 198 LOC                              |
| **关键洞察** | "慢操作丢后台, Agent 继续想下一步"   |

**工作原理：**

- `BackgroundManager` 维护 `{task_id → {status, result, command}}`
- `run()` 启动守护线程（daemon=True），立即返回 task_id
- 子进程完成后，结果入 `_notification_queue`
- 每次 LLM 调用前 `drain_notifications()`，将结果注入 messages

循环本身保持单线程，只有子进程 I/O 被并行化。

---

### s09 — Agent 团队 (Agent Teams)

| 维度         | 内容                                 |
| ------------ | ------------------------------------ |
| **分类**     | 协作                                 |
| **核心机制** | 持久化队友 + JSONL 文件邮箱          |
| **工具数**   | 10 个                                |
| **代码量**   | 348 LOC                              |
| **关键洞察** | "任务太大一个人干不完, 要能分给队友" |

**与 s04 子 Agent 的区别：**

| 子 Agent (s04)                    | 队友 (s09)                                          |
| --------------------------------- | --------------------------------------------------- |
| 一次性：生成 → 执行 → 返回 → 销毁 | 持久化：spawn → work → idle → work → ... → shutdown |
| 无身份                            | 有名字、角色、状态                                  |
| 无跨调用记忆                      | config.json 持久化                                  |
| 无通信                            | JSONL 收件箱：append-only, drain-on-read            |

**通信机制：**

- `.team/config.json`：团队名册（name, role, status）
- `.team/inbox/{name}.jsonl`：每人一个收件箱
- `send()` → 追加一行 JSON
- `read_inbox()` → 读取全部并清空（drain）

---

### s10 — 团队协议 (Team Protocols)

| 维度         | 内容                                                     |
| ------------ | -------------------------------------------------------- |
| **分类**     | 协作                                                     |
| **核心机制** | request_id 关联的请求-响应模式                           |
| **工具数**   | 12 个                                                    |
| **代码量**   | 419 LOC                                                  |
| **关键洞察** | "队友之间要有统一的沟通规矩" — 一个 FSM 模式驱动所有协商 |

**两个协议，同一个模式：**

```text
关机协议 (Shutdown Protocol):
  Lead --shutdown_req{req_id:"abc"}--> Teammate
  Lead <--shutdown_resp{req_id:"abc", approve:true}-- Teammate

计划审批协议 (Plan Approval Protocol):
  Teammate --plan_req{req_id:"xyz"}--> Lead
  Teammate <--plan_resp{req_id:"xyz", approve:true}-- Lead

共享 FSM: [pending] → approved | rejected
```

---

### s11 — 自主 Agent (Autonomous Agents)

| 维度         | 内容                                              |
| ------------ | ------------------------------------------------- |
| **分类**     | 协作                                              |
| **核心机制** | idle-poll-claim-work 自组织循环                   |
| **工具数**   | 14 个                                             |
| **代码量**   | 499 LOC                                           |
| **关键洞察** | "队友自己看看板, 有活就认领" — 不需要领导逐个分配 |

**队友生命周期：**

```text
spawn → WORK → IDLE (poll 5s×12 = 60s)
                 ├── check inbox → message? → resume WORK
                 ├── scan .tasks/ → unclaimed? → claim → resume WORK
                 └── 60s timeout → SHUTDOWN
```

**身份重注入：** 上下文压缩后 messages 过短时，在开头插入 identity block：

```python
if len(messages) <= 3:
    messages.insert(0, {"role": "user",
        "content": "<identity>You are 'coder', role: backend...</identity>"})
```

---

### s12 — Worktree + 任务隔离

| 维度         | 内容                                                        |
| ------------ | ----------------------------------------------------------- |
| **分类**     | 协作                                                        |
| **核心机制** | Git worktree 实现文件系统级隔离                             |
| **关键洞察** | 每个队友工作在独立的 git worktree 中，完成后 merge 回主分支 |

**工作原理：**

- 认领任务时 → `git worktree add .worktrees/task_N`
- 队友的所有文件操作都在隔离的 worktree 中
- 完成后 → `git merge` 回主分支
- 解决了多 Agent 并行修改同一文件的冲突问题

---

## 二、Magi 架构概览

Magi 是一个 VS Code 插件，采用 **Orchestrator（编排器）+ Worker（工人）** 模式：

```text
┌──────────────────────────────────────────────────┐
│  用户输入                                          │
│       ↓                                           │
│  Orchestrator LLM (编排器)                         │
│  ├─ 分析任务                                       │
│  ├─ 调用 dispatch_workers 工具                      │
│  │    ↓                                           │
│  ├─ DispatchManager                                │
│  │   ├─ DispatchBatch (批次管理)                    │
│  │   ├─ DispatchEntry × N (并行 Worker)             │
│  │   │   ├─ Worker LLM 1 (claude/gemini/codex...) │
│  │   │   ├─ Worker LLM 2                          │
│  │   │   └─ Worker LLM N                          │
│  │   └─ wait_for_workers (收集结果)                 │
│  ├─ 分析 Worker 结果                                │
│  └─ 决定是否继续下一轮                               │
│                                                    │
│  Webview UI (Svelte 5)                             │
│  ├─ Worker 卡片 (SubTaskSummaryCard)                │
│  ├─ 状态覆盖 (waitResult/subTask/runtime)           │
│  └─ 消息流展示                                      │
└──────────────────────────────────────────────────┘
```

### Magi 核心模块

| 模块                      | 文件                          | 职责                                                    |
| ------------------------- | ----------------------------- | ------------------------------------------------------- |
| **Orchestrator Adapter**  | `orchestrator-adapter.ts`     | 编排器 Agent Loop，处理 LLM 响应和工具调用              |
| **Worker Adapter**        | `worker-adapter.ts`           | Worker Agent Loop，独立上下文执行子任务                 |
| **Dispatch Manager**      | `dispatch-manager.ts`         | 批次管理、并行 Worker 分发、结果收集                    |
| **Dispatch Batch**        | `dispatch-batch.ts`           | 一批并行 Worker 的容器                                  |
| **Message Factory**       | `message-factory.ts`          | 构造发往前端的消息（subTaskCard, workerInstruction 等） |
| **Message Hub**           | `message-hub.ts`              | 消息中枢，管理 requestContext                           |
| **Message Pipeline**      | `message-pipeline.ts`         | Worker 执行管线，包含消息收发                           |
| **Adapter Factory**       | `adapter-factory.ts`          | LLM 调用工厂，管理重试、requestContext 切换             |
| **Mission System**        | `mission/`                    | 任务系统：assignment、contract、storage                 |
| **Profile System**        | `profile/`                    | Worker 角色分配（profile-loader, worker-assignments）   |
| **Plan Ledger**           | `plan-ledger/`                | 计划审计系统                                            |
| **Wisdom Extractor**      | `wisdom/`                     | 跨会话经验萃取                                          |
| **Task Dependency Graph** | `task-dependency-graph.ts`    | 任务依赖图                                              |
| **Policy Engine**         | `policy-engine.ts`            | 风险策略引擎                                            |
| **Verification Runner**   | `verification-runner.ts`      | 自动化验证                                              |
| **Recovery Handler**      | `recovery/`                   | 异常恢复                                                |
| **Autonomous Worker**     | `worker/autonomous-worker.ts` | 自主 Worker 模式                                        |

---

## 三、逐项对比

### 3.1 Agent Loop — 基础循环

| 维度           | Claude Code (s01)            | Magi                                                            |
| -------------- | ---------------------------- | --------------------------------------------------------------- |
| **循环结构**   | 简洁的 `while tool_use` 循环 | 双层循环：Orchestrator Loop + Worker Loop                       |
| **退出条件**   | `stop_reason != "tool_use"`  | Orchestrator 有复杂终止决策引擎 (`orchestrator-termination.ts`) |
| **实现复杂度** | ~60 LOC                      | 数千 LOC（分散在多个文件）                                      |
| **单/多 LLM**  | 单一 LLM                     | 多 LLM 并行（Orchestrator 用一个，Workers 可用不同模型）        |

**Claude Code 优势：** 简单透明，易于调试和理解。
**Magi 优势：** 多 LLM 并行能力，异构模型混用（如 Claude 做编排、Gemini/Codex 做执行）。

---

### 3.2 工具系统

| 维度         | Claude Code (s02)                      | Magi                                    |
| ------------ | -------------------------------------- | --------------------------------------- |
| **工具定义** | JSON Schema + lambda dispatch map      | VS Code 扩展点 + 内置工具 + MCP         |
| **工具范围** | 通用文件/Shell 工具                    | 深度 IDE 集成（LSP、编辑器、终端、Git） |
| **工具数量** | 4-14 个（随版本增长）                  | 数十个专用工具                          |
| **核心工具** | `dispatch_workers`, `wait_for_workers` | 编排层工具（非 Agent 直接暴露）         |

**Claude Code 优势：** 工具即函数，无缝扩展。
**Magi 优势：** 深度 IDE 集成，Worker 有完整的开发工具链。

---

### 3.3 任务分解与子 Agent

| 维度                  | Claude Code (s04)                         | Magi                                                |
| --------------------- | ----------------------------------------- | --------------------------------------------------- |
| **模式**              | `task` 工具 → 子 Agent（独立 messages[]） | `dispatch_workers` → 并行 Worker                    |
| **子 Agent 生命周期** | 一次性：生成 → 执行 → 摘要 → 销毁         | 一次性：创建 → 执行 → 结果回传                      |
| **并行性**            | 串行（一个 task 完再下一个）              | **真正并行**（Promise.all 同时执行多个 Worker）     |
| **隔离性**            | 独立 messages[]，但共享文件系统           | 独立上下文，但共享全局 requestContext（有竞态问题） |
| **结果返回**          | 文本摘要注入父 Agent 上下文               | 结构化结果通过 wait_for_workers 收集                |

**Claude Code 优势：** 隔离干净，无共享状态竞态。
**Magi 优势：** 真正的并行执行，异构模型混用，显著提升速度。
**Magi 已知问题：** requestContext 全局单例导致并行 Worker 竞态条件。

---

### 3.4 技能 / 知识注入

| 维度           | Claude Code (s05)                               | Magi                                            |
| -------------- | ----------------------------------------------- | ----------------------------------------------- |
| **机制**       | 两层注入：system prompt 索引 + tool_result 全文 | Profile System + Guidance Injector              |
| **知识定义**   | `skills/*/SKILL.md` (YAML frontmatter)          | `profile/builtin/*.ts` + worker-assignments     |
| **按需加载**   | ✅ load_skill 工具主动加载                      | ⚠️ 角色定义在 dispatch 时全量注入 system prompt |
| **token 效率** | 高（只加载需要的技能）                          | 中等（Worker profile 全量注入）                 |

**Claude Code 优势：** 按需加载，token 效率高。
**Magi 优势：** 角色分配更精确，可基于任务类型自动匹配 Worker profile。

**💡 建议：** Magi 可以借鉴两层注入模式——在 Worker system prompt 中只放 profile 摘要，具体指南按需加载。

---

### 3.5 上下文管理 / 压缩

| 维度              | Claude Code (s06)           | Magi                                                                       |
| ----------------- | --------------------------- | -------------------------------------------------------------------------- |
| **压缩策略**      | 三层：micro → auto → manual | Worker 无压缩（任务短暂）；Orchestrator 有 supplementary-instruction-queue |
| **持久化**        | .transcripts/ 保存完整历史  | VS Code 会话管理 + 消息存储                                                |
| **micro-compact** | 旧 tool_result → 占位符     | 无等价机制                                                                 |
| **auto-compact**  | token 阈值触发 LLM 摘要     | Orchestrator 有类似的上下文管理                                            |
| **完整历史恢复**  | 磁盘 transcript 可回溯      | 会话历史可恢复                                                             |

**Claude Code 优势：** 系统化的三层压缩，支持无限会话。
**Magi 优势：** Worker 任务短暂，天然不需要压缩；Orchestrator 通过轮次管理控制上下文增长。

**💡 建议：** 对于 Orchestrator 层的长对话场景，可借鉴 micro_compact 思路，压缩旧轮次的 Worker 结果。

---

### 3.6 任务依赖图

| 维度         | Claude Code (s07)                 | Magi                                       |
| ------------ | --------------------------------- | ------------------------------------------ |
| **实现**     | `.tasks/task_N.json` 文件持久化   | `task-dependency-graph.ts` 内存/结构化     |
| **依赖模型** | `blockedBy[]` + `blocks[]` 双向   | 依赖图 + mission system                    |
| **状态机**   | pending → in_progress → completed | 更复杂的状态（含 assignment, contract 等） |
| **并行检测** | 无依赖关系的任务自动并行          | dispatch routing service 决定并行策略      |
| **持久化**   | ✅ 文件系统                       | 会话内存 + 可选持久化                      |

**Claude Code 优势：** 简单直接，文件持久化在压缩后存活。
**Magi 优势：** 更丰富的任务模型（mission → assignment → contract 三层抽象），支持更复杂的编排逻辑。

---

### 3.7 并发执行

| 维度           | Claude Code (s08)                  | Magi                                           |
| -------------- | ---------------------------------- | ---------------------------------------------- |
| **机制**       | 后台守护线程 + 通知队列            | Promise.all 并行 Worker + EventEmitter         |
| **并行粒度**   | Shell 命令级（子进程）             | **LLM 调用级**（多个 Worker 同时调用不同 LLM） |
| **通知机制**   | drain_notifications（每轮 LLM 前） | 事件驱动 + wait_for_workers 阻塞等待           |
| **Agent Loop** | 主循环保持单线程                   | Orchestrator 单线程，Worker 并行               |

**Claude Code 优势：** 简单安全，只并行化 I/O。
**Magi 优势：** 真正的多 LLM 并行，大幅提升吞吐量。但引入了 requestContext 竞态等复杂性。

---

### 3.8 多 Agent 协作

| 维度           | Claude Code (s09-s10)                       | Magi                                    |
| -------------- | ------------------------------------------- | --------------------------------------- |
| **Agent 模型** | Leader + 持久化 Teammates                   | Orchestrator + 一次性 Workers           |
| **通信**       | JSONL 文件邮箱（异步）                      | MessageHub / MessageFactory（同步事件） |
| **协议**       | 关机握手 + 计划审批（request_id FSM）       | Worker report 协议 + plan ledger 审计   |
| **生命周期**   | spawn → work → idle → work → ... → shutdown | 创建 → 执行 → 完成/失败                 |
| **身份持久化** | config.json + 身份重注入                    | Worker profile（无跨调用持久化）        |

**Claude Code 优势：**

- 队友持久化，支持跨任务累积上下文
- JSONL 邮箱解耦通信，天然异步
- request_id FSM 协议模式通用可复用

**Magi 优势：**

- Worker 一次性特性让状态管理更简单
- Orchestrator 集中决策，避免分布式协调复杂性
- 结构化的 plan-ledger 审计系统

---

### 3.9 自主 Agent

| 维度         | Claude Code (s11)                  | Magi                             |
| ------------ | ---------------------------------- | -------------------------------- |
| **自治模式** | idle-poll-claim-work 自组织        | autonomous-worker.ts 自主 Worker |
| **任务发现** | 扫描 .tasks/ 看板自动认领          | Orchestrator 分配（非自主发现）  |
| **空闲行为** | 5s 轮询 × 12 = 60s → 自动 shutdown | Worker 完成即退出                |
| **身份恢复** | messages 过短时插入 identity block | Profile 在 dispatch 时注入       |

**Claude Code 优势：** 真正的自组织——队友自己找活干，无需中央分配。
**Magi 优势：** 中央编排更可控，适合需要严格协调的场景。

**💡 建议：** Magi 可以探索自组织模式——让 Worker 在完成任务后自动查看任务队列、认领下一个任务，而非每轮都由 Orchestrator 重新分配。

---

### 3.10 任务隔离

| 维度         | Claude Code (s12)                  | Magi                        |
| ------------ | ---------------------------------- | --------------------------- |
| **隔离机制** | Git worktree（文件系统级隔离）     | 无文件系统隔离              |
| **冲突处理** | 完成后 git merge                   | Worker 可能并行修改同一文件 |
| **代价**     | 磁盘空间（每个 worktree 一份拷贝） | 无额外开销                  |

**Claude Code 优势：** 彻底解决多 Agent 并行修改文件的冲突问题。
**Magi 优势：** 零额外开销，适合文件冲突少的场景。

**💡 建议：** 对于需要多 Worker 并行修改同一仓库的场景，可以考虑引入 worktree 隔离。

---

## 四、架构哲学对比

### 4.1 设计理念

| 维度           | Claude Code                                    | Magi                                                    |
| -------------- | ---------------------------------------------- | ------------------------------------------------------- |
| **核心理念**   | 渐进式复杂度：从最简循环出发，每次只加一个机制 | 工程级编排：一开始就设计完整的 Orchestrator-Worker 架构 |
| **复杂度曲线** | s01(60 LOC) → s12(~600 LOC) 线性增长           | 数万 LOC，模块化但复杂                                  |
| **状态管理**   | 文件即状态（JSON、JSONL）                      | 内存状态 + 事件驱动                                     |
| **通信模型**   | 文件邮箱（异步解耦）                           | EventEmitter + MessageHub（同步事件）                   |
| **LLM 交互**   | 单一 Anthropic SDK                             | 多模型适配层（Protocol Adapters）                       |

### 4.2 扩展维度对比

```text
             简单性          并行能力        IDE集成         可控性
Claude Code  ██████████      ████            ██              ████████
Magi         ████            ██████████      ██████████      ██████████

             多模型支持      自组织能力       上下文管理       工程成熟度
Claude Code  ██              ██████████      ████████        ████████
Magi         ██████████      ████            ██████          ████████████
```

---

## 五、双方优缺点总结

### Claude Code 优势

1. **极致简洁：** 每个机制独立可理解，渐进式复杂度
2. **文件即状态：** 任务图、收件箱、配置全部文件持久化，天然抗压缩
3. **自组织能力：** idle-poll-claim-work 模式让 Agent 自主寻找工作
4. **解耦通信：** JSONL 邮箱天然异步，无锁竞争
5. **按需知识加载：** 两层 Skill 注入高效利用 token
6. **三层压缩：** 系统化解决无限会话问题
7. **协议通用性：** request_id FSM 可套用到任何请求-响应场景

### Claude Code 劣势

1. **无真正并行 LLM 调用：** s08 只并行化子进程，Agent Loop 始终单线程
2. **无多模型支持：** 绑定 Anthropic SDK，无法混用不同 LLM
3. **无 IDE 集成：** 纯 CLI，缺少 LSP、编辑器等深度集成
4. **文件邮箱效率低：** 大量 I/O 操作，不适合高频通信
5. **无结构化风险控制：** 缺少 policy engine、risk assessment

### Magi 优势

1. **真正的 LLM 并行：** 多个 Worker 同时调用不同模型
2. **异构模型混用：** Claude 编排 + Gemini/Codex 执行
3. **深度 IDE 集成：** VS Code 扩展，LSP、终端、Git 一体化
4. **丰富的任务模型：** mission → assignment → contract 三层抽象
5. **结构化审计：** plan-ledger、wisdom-extractor、verification-runner
6. **可视化：** Svelte 5 WebView 实时展示 Worker 状态

### Magi 劣势

1. **全局状态竞态：** requestContext 单例在并行 Worker 下有竞态条件
2. **复杂度高：** 模块间耦合需要深入理解才能维护
3. **无自组织能力：** Worker 不能自主发现和认领任务
4. **无上下文压缩：** Orchestrator 长对话无系统化压缩策略
5. **无文件系统隔离：** 并行 Worker 可能产生文件冲突
6. **技能注入效率：** Worker profile 全量注入 system prompt

---

## 六、产品升级与深度重构蓝图（遵循严格工程标准）

在进行 Magi 的产品升级与系统维护时，必须严格遵守以下工程纪律（Critical Bans）：

- **严禁多重实现**：引入新机制（如 Worktree 隔离）后，旧的串行规避机制必须彻底删除。
- **严禁回退逻辑**：修复上下文传递后，原有的“兜底猜测”或状态强覆盖必须移除。
- **严禁打补丁**：任何问题必须深挖到架构根源，拒绝使用单纯判空、`setTimeout` 错峰等掩盖错误的补丁式修复。

基于以上标准，针对 Magi 当前的系统瓶颈，梳理出以下标准作业程序（SOP）级别的重构计划。

### 6.1 核心重构一：根除 Worker 卡片串台与状态丢失（最高优先级）

**1. [表象分析] (Symptom Analysis)**
用户反馈在发起新的 Dispatch 轮次时，历史遗留的 Worker 卡片会被异常唤醒（显示为 running 且计时器累加），同时 `wait_for_workers` 的完成状态无法正确回写到对应卡片（抛出 `workerWaitResult= MISS`）。

**2. [机理溯源] (Context & Flow)**
理想的逻辑流是：Orchestrator 派发任务时生成唯一的 `requestId` -> UI 渲染卡片 -> Worker 执行期间发送的指令以及最终的执行结果，必须严格携带该 `requestId` -> UI 依据唯一标识精确更新对应卡片的状态。

**3. [差距诊断] (Gap Diagnosis)**
实际运行中，`dispatch-manager` 和 `message-factory` 强依赖 `MessageHub` 上的全局隐式状态 `requestContext` 来获取当前请求 ID。当真正并发拉起多个 Worker 时，由于异步乱序，部分消息的 `requestId` 读取为空或被其他 Worker 覆盖，导致前端生成的 `cardKey` 无法匹配，从而触发 UI 的 Fallback 降级渲染逻辑（误激活历史卡片）。

**4. [根本原因分析] (Root Cause Analysis)**
追问：为什么 `requestContext` 会错乱？

- _Why 1_: 因为多 Worker 运行时，`MessageFactory.requestId` 获取到了未定义的或错误的值。
- _Why 2_: 因为该字段被设计为了全局单例（Singleton）。
- _Why 3_: 因为在 `adapter-factory.ts` 的 `sendOnce` 闭包中，为了规避某些日志污染，开发者主动调用了 `setRequestContext(undefined)`，并试图在 `finally` 中恢复。
- _Why 4_: 在 `Promise.all` 并发执行流下，Worker A 的清除操作直接擦除了 Worker B 正在依赖的状态。**这是底层架构对异步并发状态管理不当导致的竞态条件（Race Condition）漏洞。**

**5. [彻底修复与债清偿] (Fundamental Fix & Cleanup)**

- **源头修复**：彻底废弃隐式的全局 `requestContext`。在 `DispatchBatch` 创建时进行快照生成不可变的 `batchRequestId`，并在 `SubTaskCardPayload` 中增加必填的 `requestId` 字段。通过方法入参显式自顶向下透传到 `emitSubTaskCard` 和相关工厂。
- **拒绝掩盖**：严禁在前端 UI 增加“判断卡片是否属于当前轮次”的补丁代码，严禁在后端加延时错峰发送。必须保证消息 ID 绝对一致。
- **清理债务**：删除 `MessageHub` 和 `MessageFactory` 中所有的 `setRequestContext`、`getRequestContext` 方法及遗留的调试日志；清理前端因此次 Bug 引入的复杂兜底重写逻辑。

---

### 6.2 核心重构二：突破并行写冲突，引入物理沙盒隔离

**1. [表象分析] (Symptom Analysis)**
当 Orchestrator 规划出多个需要修改同一目录下不同文件的并行任务时，执行速度与串行无异，并未体现多模型并发的优势。

**2. [机理溯源] (Context & Flow)**
理想状态下，独立性较强的修改任务（如：A 修改登录页，B 修改鉴权中间件）应完全并发执行，缩短总耗时。

**3. [差距诊断] (Gap Diagnosis)**
实际运行中，`DispatchBatch.resolveFileConflicts()` 扫描到多 Worker 的 `targetFiles` 有交集时，会强制自动注入依赖（`dependsOn`），将并行强行降级为串行（Serialized）执行。

**4. [根本原因分析] (Root Cause Analysis)**
为什么需要强制降级？

- 根因在于所有 Worker 进程都直接操作同一个 `workspaceRoot` 的物理目录。
- 缺乏底层文件系统级别的隔离机制，多模型并发写入会导致严重的 I/O 竞态，破坏代码库甚至导致 AST 混乱。

**5. [彻底修复与债清偿] (Fundamental Fix & Cleanup)**

- **源头修复**：借鉴 Claude Code 的 s12 模式。引入基于 `git worktree` 的沙盒隔离机制。当检测到需要写操作的任务时，调度器为其创建 `.magi/worktrees/task-{id}`。
- **禁止多重实现**：新隔离机制上线后，**严禁**并存两种并发处理模式。必须**彻底删除** `DispatchBatch.resolveFileConflicts()` 的串行降级逻辑。Worktree 隔离成为唯一合法的文件并发修改方案。
- **清理债务**：移除旧代码中为了规避并发写而设定的锁机制、缓冲队列及相关的复杂配置开关。

---

### 6.3 核心重构三：消除 System Prompt 臃肿，实现两级按需注入

**1. [表象分析] (Symptom Analysis)**
Worker 的 LLM 响应时延较高（TTFT长），且随着工程规约增多，Token 消耗巨大；偶尔出现模型未能遵循核心任务指令（注意力稀释）的问题。

**2. [机理溯源] (Context & Flow)**
Worker 的 System Prompt 应高度聚焦于当前派发的具体指派（Assignment），在最小的上下文开销下产出最高质量的代码。

**3. [差距诊断] (Gap Diagnosis)**
目前 `ProfileLoader` 和 `GuidanceInjector` 会在每次派发任务前，将 Worker 的基础设定、优势、根据 Category 推导的弱项，以及全量的工程规约、ADR（架构决策记录）、互检指南等，通过字符串拼接一股脑全部注入 System Prompt。

**4. [根本原因分析] (Root Cause Analysis)**
为什么系统要进行全量预载？

- 系统架构假定“LLM 必须在对话第一轮（System 阶段）掌握可能用到的所有背景知识”，缺乏针对结构化知识库的“按需索引-拉取”动态加载机制。

**5. [彻底修复与债清偿] (Fundamental Fix & Cleanup)**

- **源头修复**：全面对标 Claude Code s05。重构 Profile 注入逻辑，采用“两级加载”。第一层（System Prompt）极度削薄，仅注入“角色名称+安全红线+可用知识库索引”；第二层新增内置工具 `fetch_project_guidelines`，当 Worker 认为要处理关键模块时，主动调用工具拉取特定规约（作为 `tool_result` 返回）。
- **禁止回退逻辑**：一旦实施按需加载，绝不允许保留“如果工具调用超时则退回拼接完整超长 Prompt”的后备逻辑。必须倒逼工具链本身的绝对高可用。
- **清理债务**：删除原有的 `buildUnifiedSystemPrompt` 中各种庞杂文本（如 activeTodosSummary 全量字符、relevantADRs 全量）的拼接逻辑，彻底清偿冗长 Prompt 带来的性能债务。

---

### 6.4 核心重构四：融合 Micro-Compact，优化 Orchestrator 上下文

**1. [表象分析] (Symptom Analysis)**
随着多轮对话和执行，Orchestrator 规划层容易出现 `budget_exceeded`（Token 超出预算）问题，LLM 响应变慢，且有时会因为历史信息过载而出现幻觉。

**2. [机理溯源] (Context & Flow)**
Orchestrator 需要记忆先前的规划和执行结果（通过 `recent_turns` 组装 L2 上下文），以便基于全局状态做出下一步决策。

**3. [差距诊断] (Gap Diagnosis)**
目前的 `ContextAssembler` 会将之前 Worker 返回的大量结构化报告（summary、errors、completedTodos 等）作为历史记录全量保留。经过几轮迭代后，这些长段的 `wait_for_workers` 结果迅速填满了 Orchestrator 的上下文窗口。

**4. [根本原因分析] (Root Cause Analysis)**
为什么需要保留全量结果？

- 架构缺乏对已消费信息（已提取并落库到 PlanLedger 的状态）的折叠机制，系统简单地将所有历史交互视为同等重要。

**5. [彻底修复与债清偿] (Fundamental Fix & Cleanup)**

- **源头修复**：借鉴 Claude Code 的 s06 模式（micro_compact）。增强 `truncation-utils.ts`，精准识别超过 3 个 Turn 之前的 `tool_result`。如果识别出是 `wait_for_workers` 的长返回，则将其正则替换为 `[已折叠：历史派发结果，已提取至 PlanLedger]` 的占位符。
- **禁止回退逻辑**：严禁采用粗暴截断末尾信息的“丢弃式”压缩，必须保留事件发生的“指针”记忆。
- **清理债务**：移除现有的粗粒度 Token 强行截断逻辑，确保上下文即使被压缩也具备语义连贯性。

---

### 6.5 核心重构五：探索“半自治” Worker 调度模式（长期演进）

**1. [表象分析] (Symptom Analysis)**
系统高频地穿梭于 Orchestrator 和 Worker 之间，Orchestrator 处理状态变迁和重试逻辑造成大量中间状态调用，增加了端到端延迟和 API 成本。

**2. [机理溯源] (Context & Flow)**
`mission-driven-engine.ts` 是系统的中枢大脑，所有的任务分配、状态变迁（pending -> running -> verify -> completed）和异常恢复都必须由 Orchestrator 主导。

**3. [差距诊断] (Gap Diagnosis)**
系统极度依赖单一高级模型（如 Claude）做 Orchestrator，使其成了系统瓶颈（"保姆"式管理）。当执行确定性的任务图时，无需高级模型频繁介入分配。

**4. [根本原因分析] (Root Cause Analysis)**
根因在于 Worker 缺乏自组织能力。现在的 Worker 是一次性的、被动的函数调用，而不是持续在线、能主动拉取任务的自治体。

**5. [彻底修复与债清偿] (Fundamental Fix & Cleanup)**

- **源头修复**：对标 Claude Code s11。将 Orchestrator 降级为“项目经理”角色（仅负责分析需求并生成带有 `blockedBy` 依赖的 PlanLedger）。重构 `AutonomousWorker` 引入生命周期循环（`Loop`）。Worker 空闲时主动查询 `get_unclaimed_todos`，基于幂等锁（复用 `DispatchIdempotencyStore`）抢占自己擅长的任务，完成后写回 Ledger 并继续认领。
- **禁止多重实现**：新架构下必须剥离 `mission-driven-engine.ts` 中针对单个子任务的微观状态管理逻辑，交由自治 Worker 闭环。
- **清理债务**：逐步拆解 `mission-driven-engine.ts`（当前 3500+ 行），大幅精简 Orchestrator 的微观控制面代码，最终实现去中心化调度。

---

## 七、结语

Magi 的架构是高度工程化和防御性的（拥有复杂的 Orchestrator、Plan Ledger、Verification Pipeline），这在真实的 IDE 生产级场景下是必不可少的。然而，过度的集中防御也带来了全局状态耦合（如 requestContext 竞态）与并发瓶颈。

产品演进不应是不断堆砌补丁和“if-else”兜底分支。通过对标 Claude Code 演进路径中的精髓，严格执行 SOP 进行大刀阔斧的底层重构（根除隐式状态、引入物理隔离沙盒、实现按需动态知识加载），Magi 可以在坚守工程质量底线的同时，在多 Agent 高效协同领域实现跨越式升级。

---

_文档重构时间：2025 年 1 月_
_依据：Magi 系统源码深度诊断及严格工程标准_
