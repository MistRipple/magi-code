<div align="center">

# Magi 多智能体编排系统：全功能与架构白皮书

**突破单体大模型能力边界，专为复杂软件工程设计的新一代智能体协同基座**

[![Status](https://img.shields.io/badge/Status-Active-success.svg)]()
[![Architecture](https://img.shields.io/badge/Architecture-Multi--Agent-blue.svg)]()
[![Security](https://img.shields.io/badge/Security-Local_First-orange.svg)]()

</div>

---

## 目录 (Table of Contents)
1. [核心架构理念：为什么需要 Magi？](#1-核心架构理念为什么需要-magi)
2. [任务驱动与动态 Todo 体系 (Mission & Todo Orchestration)](#2-任务驱动与动态-todo-体系-mission--todo-orchestration)
3. [零冲突并发编辑引擎 (Zero-Collision Edit Engine)](#3-零冲突并发编辑引擎-zero-collision-edit-engine)
4. [三维防御性上下文与记忆系统 (Context & Memory)](#4-三维防御性上下文与记忆系统-context--memory)
5. [动态双模式治理 (Adaptive Mode Governance)](#5-动态双模式治理-adaptive-mode-governance)
6. [纯本地知识检索与沉淀 (Local Knowledge Base)](#6-纯本地知识检索与沉淀-local-knowledge-base)
7. [细粒度快照与时间回溯 (Snapshot & Recovery)](#7-细粒度快照与时间回溯-snapshot--recovery)
8. [全天候扩展能力链：工具、MCP与动态技能 (Extended Toolchain & Skills)](#8-全天候扩展能力链工具mcp与动态技能-extended-toolchain--skills)
9. [双轨视图交互设计 (Dual-Track UI)](#9-双轨视图交互设计-dual-track-ui)

---

## 1. 核心架构理念：为什么需要 Magi？

当前业界主流的 AI 编程助手（如 Copilot, Cursor）大多采用“对话+单点修改”的模式，这种模式在面对**跨模块重构、深度 Bug 追踪、长周期迭代**时，往往会暴露三大致命痛点：
1. **并发覆盖冲突**：AI 不懂锁，多文件修改容易互相覆盖，毁坏代码。
2. **上下文爆炸与失忆**：长对话极易导致 Token 溢出，模型忘记初始需求或陷入死循环。
3. **缺乏执行定力**：单体大模型难以同时兼顾“全局架构规划”与“底层代码逻辑追踪”。

**Magi 的诞生正是为了解决这三大工程难题。** 它不仅是一个简单的交互对话框，而是一个**多智能体工程调度系统**，通过意图洞察、角色分工、底层安全锁和本地化基建，为软件工程提供真正高可靠的自动化保障。

---

## 2. 任务驱动与动态 Todo 体系 (Mission & Todo Orchestration)

Magi 彻底抛弃了发散式的自由对话，引入了基于“合同”与“动态待办”的 **Mission-Driven Architecture (任务驱动架构)**。

### 2.1 Orchestrator 与 Worker 的专业分工
- **编排者 (Orchestrator)**：作为项目经理，负责理解用户意图，查询全局知识，并将大需求拆解为一个个边界清晰的任务合同（Mission Contract：包含目标 Goal、验收标准 Acceptance、上下文 Context）。
- **独立执行者 (Worker Personas)**：作为具体的工程师，接收任务后进入独立的执行沙盒。系统内置多态人格（如偏重深度推理的 `Claude`，与偏重严守纪律高执行力的 `Codex`），根据任务性质智能路由，让专业模型做专业的事。

### 2.2 动态任务拓扑 (Task Dependency Graph)
Magi 并不是简单地把任务丢入队列，而是建立了一套**动态任务拓扑分析图**：
- **自动并发 (Parallel Execution)**：当检测到任务间无依赖关系时（如分别修改前端组件和后端 API），系统会自动拉起多个 Worker 并行处理。
- **依赖阻塞与上下文注入 (Context Injection)**：当任务 B 依赖任务 A 时，B 会自动挂起；任务 A 成功结束后，系统会自动剥离 A 的核心产出和修改结果，将其作为前置 Context 精准注入到 B 的上下文中，实现接力开发。

### 2.3 动态 Todo 树拆解与自演化
真正的工程往往是边做边发现问题的。Magi 构建了极其强大的内部 `TodoManager` 机制：
- **执行中拆解**：Worker 在执行一个大任务时，可以通过工具**动态追加子 Todo**。
- **自适应修复 (Self-Correction)**：如果在编译时发现新错误，Worker 无需向 Orchestrator 汇报失败，它可以直接生成一个 `Fix Build Error` 的衍生 Todo，将自己挂起去执行修复操作后再返回主线，完美复刻了人类工程师的处理逻辑。

---

## 3. 零冲突并发编辑引擎 (Zero-Collision Edit Engine)

Magi 重构了底层的 `FileExecutor`，这是系统能够在高并发多 Agent 环境下保证代码绝对安全的核心支撑。

| 传统 AI 助手痛点 | Magi 的解决方案 |
| :--- | :--- |
| **行号偏移**：依赖 `lines 30-50` 替换，文件一旦被他人改动，直接插错位置。 | **实时读取 (Real-time Read)**：在执行写入动作的瞬间，强制读取磁盘最新状态。 |
| **并发覆写**：两个任务同时修改 `utils.ts`，导致后修改者覆盖前修改者。 | **文件级互斥锁 (File-level Mutex)**：基于底层路径的排他锁，强制同一文件的读写串行化。 |
| **生硬替换**：要求模型输出完整的大段上下文用于字符串匹配，经常匹配失败。 | **意图驱动编辑 (Intent Edit)**：大模型只输出“修改意图”（要在哪个类加什么逻辑），底层工具智能解析定位并重写目标块。 |

**核心结论**：在 Magi 中，即使多个 Worker 同时并发修改工作区，也绝对不会发生任何一处代码的盲目覆盖或错位损坏。

---

## 4. 三维防御性上下文与记忆系统 (Context & Memory)

为了对抗长周期任务中必然出现的“模型失忆”和“Token 爆表”，Magi 构建了三级防御体系：

### 4.1 L1: 保护性即时上下文 (Short-term Defense)
- **预防性截断 (TruncationUtils)**：当工具（如 WebFetch 或终端报错）返回超长无用文本时，系统在进入上下文拼装前会进行智能截断，保证当前核心 `Prompt` 和指令不被淹没。

### 4.2 L2: 会话滚动摘要 (Rolling Summary)
- **告别流水账**：长达几小时的深度 Bug 排查往往包含大量的“试错-报错-再试错”日志。
- **智能提纯**：Magi 会在节点完结时触发 `MemoryDocument` 机制，将冗长日志压缩提纯为“滚动摘要”（只保留关键的决策、排除的死胡同、和当前未决问题），再传递给下游节点。这极大节省了 Token 消耗并维持了模型的注意力。

### 4.3 L3: 项目知识与会话强隔离 (Session Isolation)
- **隔离污染**：底层 `UnifiedSessionManager` 确保不同 traceId/sessionId 下的对话、变量、状态完全物理隔离，防止多个并发会话出现“幽灵记忆”的相互串接。

---

## 5. 动态双模式治理 (Adaptive Mode Governance)

并非所有任务都需要兴师动众地拉起多 Agent 阵列。Magi 提供两套自适应策略，将**速度**与**安全**的裁量权交给用户。

| 特性维度 | 常规模式 (Standard Mode) | 深度模式 (Deep Mode) |
| :--- | :--- | :--- |
| **适用场景** | 简单问答、局部代码修改、轻量重构 | 跨模块重构、深水区 Bug 排查、系统特性开发 |
| **代码修改权限**| **编排者允许直改**（软约束，限制最多修改 3 个文件） | **编排者严格禁改**（硬约束，必须强制委派） |
| **任务流转** | 短链路：意图识别 -> 直接修改 -> 快速返回 | 长闭环：拆解 -> TaskView -> Worker 领流 -> 审查 |
| **执行预算** | 极低（1~2 轮验证） | 极高（最高达 8 轮交互，允许模型反复试错编译） |
| **响应速度** | 秒级到数十秒，主打低延迟交互 | 分钟级，主打极致可靠与工程代码安全 |

---

## 6. 纯本地知识检索与沉淀 (Local Knowledge Base)

抛弃了存在隐私风险和数据滞后问题的第三方云端向量库（如传统的 ACE），Magi 打造了**所见即所得**的全本地知识基建：

### 6.1 三位一体极速检索 (`code_search_semantic`)
在编排者或 Worker 进行局部寻路时，系统会并联调用三大底层引擎：
1. **语义结构层**：基于 `Inverted Index` 和本地生成的抽象语法树（AST / Symbol Index），精准定位函数和类定义。
2. **文本暴力层**：底层集成极速的正则搜索引擎 `Ripgrep`。
3. **防空转过滤 (Jaccard Dedup)**：检索缓存自动计算文件相似度。当发现大模型陷入“搜索不到-换个词再搜”的死循环时，底层直接熔断拦截并警告，防止模型消耗无用算力。

### 6.2 经验提取器 (Wisdom Extractor)
任务结束后，Magi 的生命周期并没有停止。
- 底层的 `WisdomExtractor` 会静默启动，分析刚才 Worker 实际解决的问题和排错路径。
- 自动提取出**高价值的业务根因、架构决策记录 (ADR) 和项目 FAQ**。
- 写入本地持久化的 `Project Knowledge Base (PKB)`。下次用户再遇到同类组件报错，系统可直接调用解决方案。

---

## 7. 细粒度快照与时间回溯 (Snapshot & Recovery)

**大模型写代码必然存在翻车风险，Magi 提供了工程级别的容灾机制。**

- **隐式触发备份**：在 Worker 执行任何实质性写动作（`file_edit`, `file_create`, `remove_files`）前，底层 `SnapshotManager` 会静默地在 `.magi/sessions/{id}/snapshots` 中生成目标文件的物理快照备份。
- **原子级与会话级回溯**：如果一次覆盖数十个文件的大型并发重构以彻底的构建失败告终，用户不必手动处理 Git 灾难。通过 Snapshot 机制，系统可以在对话中接受指令，将特定文件或**整个错误会话的修改**精准回滚至修改前状态，为模型的试错提供“无限生命”。

---

## 8. 全天候扩展能力链：工具、MCP与动态技能 (Extended Toolchain & Skills)

Magi 是一个开放的执行枢纽，其强大的能力可以通过底层插件系统无限延展。

### 8.1 丰富的内置基础工具链 (Built-in Tools)

Magi 的引擎层硬编码了一套专门为复杂工程调度设计的高可靠工具栈：
- **代码修缮组**：`file_view` / `file_create` / `file_edit` / `file_insert` / `remove_files`。
- **环境探索组**：`code_search_regex` (精确正则) / `code_search_semantic` (语义及语法树)。
- **编排控制组**：`worker_dispatch` (创建与下发子任务) / `worker_wait` (阻塞与结果回收) / `todo_list` / `todo_update` (局部任务链自演化)。

### 8.2 动态技能库与指令注入 (Skills Manager)
Magi 的行为不仅被写死在源码里，它实现了一套强大的**动态能力挂载系统**：
- **混合型 Skill 加载**：支持 `Instruction Skill` (动态注入专家级系统提示词) 和 `Custom Tool` (按需加载的新工具)。当处理特定语言或特殊框架时，Worker 可以动态“装备”这些技能。
- **仓库源热更新 (Repository Manager)**：支持从云端的 JSON 库或 GitHub 仓库（甚至官方 Claude 仓库）一键拉取并安装全套专家技能。通过这种方式，Magi 的开发知识永远与世界前沿同步。

### 8.3 真实长时终端守护 (Terminal Executor)
系统不仅能执行 `npm run build` 这样的短时命令，还具备长时轮询管理器：
- **进程持久挂载**：可启动并挂载长期运行的服务（如 Webpack/Vite 后台进程）。
- **无人值守排错**：后台静默监听，一旦截获到热更新抛出编译异常，立即将其提取成独立的子任务交给 Worker 修复，实现“自动改代码 -> 自动热更新 -> 自动排错”的循环闭环。

### 8.4 泛生态接入：MCP 与网络探测 (MCP & Web Executor)
- **MCP 服务联邦 (Model Context Protocol)**：原生支持接入 MCP Manager，仅需配置协议，即可无缝插拔对 Jira 看板、GitHub Issue 追踪、Slack 通知甚至内部私有数据库的读写交互。
- **实时外网嗅探**：通过 `Web Executor` 直接联网搜索，抓取最新的外部框架 API 官方文档和 StackOverflow 答案，彻底解决模型由于训练数据截止而带来的知识盲区。

---

## 9. 双轨视图交互设计 (Dual-Track UI)

在传统工具中，多 Agent 试错的底层日志如果全部推给用户，会导致聊天面板成为无法阅读的“垃圾场”。Magi 在前端 UI 层面做了彻底解耦：

1. **主轨 (Main Chat)**：仅用于展示最核心的用户意图确认、任务分发计划、以及最终的总结汇报。保持极简和专注。
2. **副轨 (TaskBoard)**：在侧边栏独立存在的任务看板。以卡片形式实时流式渲染每一个 Worker 的内部状态。
   - 用户可以点开特定卡片，深入查阅该 Worker 当前正在执行的 `Todo` 树、正在阅读哪些文件、经历了怎样的自我纠错逻辑。
   - 所有底层的锁等待、编译阻塞状态，均通过全局 `EventBus` 实现亚秒级的前端状态同步。

---

> **总结**：Magi 不仅关注“如何生成好代码”，更致力于解决“如何在大规模工程中安全、可靠、可持续地落地 AI 能力”。从防冲突底层、记忆提取体系，再到无界扩展的动态技能与快照兜底，Magi 正在重新定义 AI 编程助手的工程标准。
