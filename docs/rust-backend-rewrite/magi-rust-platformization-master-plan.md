# Magi Rust 平台化完整改造总方案

更新时间：2026-04-15

> 本文档定义 `Magi` 从“以 Node.js Local Agent 为核心、可嵌入 VSCode 的多智能体工程编排系统”，升级为“以 Rust 为唯一后端内核、以 Svelte 为统一前端、以 VSCode / IDEA 为当前宿主壳”的完整平台化改造方案。
>
> 本文档不是单一阶段任务单，也不是只讨论某个模块的技术替换，而是后续多 Agent 并行推进时的总设计真相源。

---

## 1. 文档定位

本文档回答以下问题：

1. `Magi` 平台化改造的最终目标是什么
2. 当前后端能力域有哪些，哪些必须纳入改造边界
3. Rust 后端的目标分层、crate 划分与目录重组应该如何设计
4. 迁移顺序如何安排，如何支持多个 Agent 并行推进且避免冲突
5. 后续每一阶段的验收标准是什么

本文档覆盖的能力域包括：

- Agent API / 常驻进程
- 任务系统 / 编排系统 / Worker 执行系统
- 内置工具系统
- MCP 系统
- Skill 系统
- 知识库系统
- 记忆系统
- 上下文系统
- 会话系统
- 工作区隔离 / 快照 / 恢复
- LLM 适配与模型桥接
- 治理 / 审批 / 沙箱 / 审计
- 前端与 Host Bridge

---

## 2. 平台目标

`Magi` 后续的产品定位，不应再定义为“一个 VSCode 插件”，而应定义为：

> 一个独立的、本地常驻的、强隔离的、跨 IDE / 跨宿主的 Agent 平台。

对应的目标形态：

- 后端运行时与任何 IDE 宿主解耦
- 前端 UI 可同时服务 Web、VSCode Webview、IDEA 面板
- 宿主仅提供少量本地能力桥接，不参与后端核心状态计算
- 所有任务、会话、工具、知识、上下文、记忆、审计都以后端为真相源

### 2.1 最终技术方向

- 后端内核：`Rust`
- 统一前端：`Svelte`
- 宿主壳：`VSCode / IDEA`
- 桥接协议：`HTTP + SSE + JSON-RPC`

### 2.2 当前宿主范围

当前改造范围只覆盖两种宿主：

- `VSCode`
- `IDEA`

其他宿主暂不进入本轮实现范围。若后续需要扩展，应通过新增 Host Bridge 接入，而不是反向修改 Core Runtime。

### 2.3 不做什么

本文档明确不采用以下方向：

1. 不继续把 VSCode 宿主能力直接注入后端 core
2. 不维持“Node 后端 + 多宿主 API 直连”的长期形态
3. 不把 Svelte UI 当作业务真相源
4. 不采用“先零散补功能，再慢慢看是否重写”的漂移式方案

---

## 3. 当前代码现实与问题定义

当前核心链路仍然是：

```text
VSCode Extension / WebView
  -> LocalAgentService (Node.js HTTP)
  -> AgentWorkspaceRuntime
  -> MissionDrivenEngine
  -> AutonomousWorker
  -> ToolManager / LLMAdapterFactory / Context / Knowledge / Session
```

当前仓库已经具备“Agent 独立进程 + Web 访问”的雏形，但仍存在以下结构性问题：

### 3.1 运行形态已分离，代码边界尚未彻底分离

- `src/agent/` 已经是独立进程入口
- `src/ui/` 已经有 Web / Webview 形态
- 但 `src/host/runtime-host.ts` 仍直接依赖 `vscode`
- 编排、工具、宿主能力、模型适配仍有较强的进程内耦合

### 3.2 后端能力域高度集中在少数巨型模块

需要重点拆解的高风险模块包括：

- `src/agent/service/local-agent-service.ts`
- `src/orchestrator/core/mission-driven-engine.ts`
- `src/orchestrator/worker/autonomous-worker.ts`
- `src/tools/tool-manager.ts`
- `src/session/unified-session-manager.ts`
- `src/context/context-manager.ts`

### 3.3 当前“后端”不是薄服务，而是产品内核

当前 Local Agent 不只是 API 转发器，还承担：

- workspace/session 注册与持久化
- bootstrap / SSE
- 编排入口
- Worker 执行闭环
- 工具执行与权限控制
- 上下文组装
- 知识库查询与治理审计
- 变更预览与审批

因此后续迁移不能只迁 `agent service`，必须覆盖所有能力域。

---

## 4. 总体改造原则

### 4.1 Core Runtime 禁止直接依赖任何 IDE SDK

后端内核不得直接依赖：

- `vscode`
- JetBrains 平台 API

这些能力统一通过 `Host Bridge` 注入。

### 4.2 前端只负责呈现，不负责业务真相

Svelte 前端负责：

- 展示 timeline / task / worker / knowledge / settings
- 发送用户交互请求
- 订阅 SSE / 流式事件

Svelte 前端不负责：

- 推断运行态
- 修补后端语义缺失
- 维持“只存在于前端”的执行状态

### 4.3 能力域按平台责任重画边界，而不是按现有文件照搬

本次迁移不是 TypeScript 到 Rust 的逐文件翻译，而是：

1. 先重画平台边界
2. 再迁移到 Rust

### 4.4 支持多 Agent 并行推进，必须以写域隔离为第一原则

后续任务拆分必须按：

- crate / package
- schema
- bridge
- host

这些稳定写域分配，不允许多个 Agent 在同一核心模块内交叉改动。

### 4.5 改造过程中允许并要求做根因级重构

本次平台化改造不是“只迁语言，不动旧结构”。

若在迁移过程中发现原有实现存在以下问题，应允许并要求在所属写域内同步完成结构性重构：

- 架构边界不合理
- 模块职责混杂
- 代码实现明显臃肿
- 存在重复实现或双轨语义
- 为兼容旧逻辑而堆积的临时分支、补丁式分支、无效回退逻辑

重构原则：

1. 必须从根因出发修正结构，不允许只做表层搬运
2. 允许拆模块、收敛接口、删除冗余实现、压缩过大文件
3. 禁止保留“新旧双实现长期并存”的过渡性堆叠
4. 禁止用兼容分支、补丁逻辑、回退逻辑掩盖结构问题
5. 重构完成后必须同步清理废弃代码、无效注释、冗余 wiring

换句话说：

> 平台迁移与结构治理必须同步推进，不能把明显不合理的旧实现原样带入 Rust 新架构。

### 4.6 全过程遵循 `cn-engineering-standard`

本方案后续所有实现任务、重构任务、排查任务，均必须遵循
[$cn-engineering-standard](/Users/xie/.codex/skills/cn-engineering-standard/SKILL.md)。

落地要求如下：

- 全程使用中文沟通、中文说明、中文代码注释
- 任务执行必须遵循“发现-修复-清理-测试-验证”的闭环
- 严格限制改动范围，但对受影响代码必须完整核验
- 发现结构问题时必须追溯根因，不得采用补丁式修复
- 禁止多重实现、禁止回退逻辑、禁止兼容分支掩盖问题
- 重构后必须同步删除废弃代码和冗余实现，保持代码整洁
- 多 Agent 派工时，也必须把该规范作为统一执行准则写入任务单

该规范不是建议项，而是本方案的默认工程约束。

---

## 5. 目标架构

### 5.1 三层模型

```text
Svelte UI
  -> Rust Agent API / SSE
  -> Rust Core Runtime
  -> Host / Model / MCP Bridges
```

### 5.2 详细分层

```text
┌─────────────────────────────────────────────────────────┐
│                   Svelte Frontend                       │
│  Web / VSCode Webview / IDEA Panel                      │
└─────────────────────────────────────────────────────────┘
                          │
                          │ HTTP / SSE
                          ▼
┌─────────────────────────────────────────────────────────┐
│                    Rust Agent API                      │
│  bootstrap / sessions / tasks / settings / knowledge   │
│  changes / events / health / version handshake         │
└─────────────────────────────────────────────────────────┘
                          │
                          ▼
┌─────────────────────────────────────────────────────────┐
│                  Rust Core Runtime                     │
│                                                         │
│  - Orchestrator                                         │
│  - Worker Runtime                                       │
│  - Tool Runtime                                         │
│  - Session Store                                        │
│  - Workspace / Snapshot / Recovery                      │
│  - Knowledge / Memory / Context                         │
│  - Governance / Audit / Usage                           │
└─────────────────────────────────────────────────────────┘
                          │
                          ├─────────────┬─────────────┐
                          ▼             ▼             ▼
                 Host Bridge      Model Bridge     MCP Bridge
               (VSCode/IDEA/...)   (provider)      (servers)
```

### 5.3 最终产品语义

最终产品真相源必须统一在 Rust Runtime 中：

- Session 真相源
- Task / Todo / Worker 真相源
- Tool call / approval / sandbox 真相源
- Knowledge / memory / context 真相源
- Runtime state / audit / usage 真相源

---

## 6. 能力域全量映射

本节给出当前系统能力域与目标归属的完整映射，确保后续不会遗漏“工具 / MCP / Skill / 知识 / 记忆 / 上下文”等系统。

### 6.1 宿主层

- 当前目录：
  - `src/extension.ts`
  - `src/ui/webview-provider.ts`
  - `src/host/runtime-host.ts`
- 目标目录：
  - `hosts/vscode`
  - `hosts/idea`
- 迁移原则：
  - 宿主只提供桥接，不承载业务真相
  - VSCode 只作为最薄壳层保留
  - IDEA 作为第二宿主接入

### 6.2 前端层

- 当前目录：
  - `src/ui/**`
- 目标目录：
  - `web/svelte-app`
- 迁移原则：
  - 所有宿主复用同一套 UI
  - 所有 runtime 状态由后端 projection 驱动

### 6.3 Agent API / 常驻进程

- 当前目录：
  - `src/agent/**`
- 目标 crate：
  - `magi-daemon`
  - `magi-api`
- 覆盖能力：
  - 启动
  - 健康检查
  - workspace 注册
  - bootstrap
  - SSE
  - settings / status
  - changes / knowledge / files API

### 6.4 任务系统 / 编排系统

- 当前目录：
  - `src/orchestrator/**`
  - `src/task/**`
  - `src/todo/**`
- 目标 crate：
  - `magi-orchestrator`
- 覆盖能力：
  - 请求分类
  - Mission / Assignment / Todo
  - Plan ledger
  - Dispatch
  - Runtime state
  - Resume / recovery
  - Governance gating

### 6.5 Worker 执行系统

- 当前目录：
  - `src/orchestrator/worker/**`
- 目标 crate：
  - `magi-worker-runtime`
- 覆盖能力：
  - Worker 生命周期
  - Todo 执行循环
  - Verification / review
  - Repair / retry / degrade
  - Worker 报告聚合

### 6.6 内置工具系统

- 当前目录：
  - `src/tools/tool-manager.ts`
  - `src/tools/file-executor.ts`
  - `src/tools/search-executor.ts`
  - `src/tools/remove-files-executor.ts`
  - `src/tools/shell/**`
  - 其他 builtin executors
- 目标 crate：
  - `magi-tool-runtime`
- 覆盖能力：
  - tool registry
  - builtin tool names
  - file / shell / process / search / remove
  - diff / preview / policy / approval
  - tool execution context

### 6.7 MCP 系统

- 当前目录：
  - `src/tools/mcp-manager.ts`
  - `src/tools/mcp-executor.ts`
- 目标形态：
  - 短中期：`bridges/mcp`
  - 长期可选：`magi-mcp-runtime`
- 覆盖能力：
  - server registry
  - transport
  - tool discovery
  - prompt discovery
  - call timeout / reconnect / health

### 6.8 Skill 系统

- 当前目录：
  - `src/tools/skills-manager.ts`
  - instruction skill 注入链路
- 目标 crate：
  - `magi-skill-runtime`
- 覆盖能力：
  - instruction skills
  - custom tools
  - skill metadata
  - skill injection policy
  - tool allowlist / disable model invocation

### 6.9 知识库系统

- 当前目录：
  - `src/knowledge/**`
- 目标 crate：
  - `magi-knowledge-store`
- 覆盖能力：
  - ADR
  - FAQ
  - learning
  - code index
  - governed audit
  - knowledge API payloads

### 6.10 记忆系统

- 当前目录：
  - `src/context/memory-document.ts`
  - `src/context/layered-memory-store.ts`
  - session memory extraction 相关链路
- 目标 crate：
  - `magi-memory-store`
- 覆盖能力：
  - session memory
  - layered memory
  - memory extraction
  - memory compaction
  - preference memory

### 6.11 上下文系统

- 当前目录：
  - `src/context/context-manager.ts`
  - `src/context/context-assembler.ts`
  - `src/context/file-summary-cache.ts`
  - `src/context/shared-context-pool.ts`
  - `src/context/truncation-utils.ts`
- 目标 crate：
  - `magi-context-runtime`
- 覆盖能力：
  - context assembly
  - token budget
  - recent turns
  - file summary cache
  - compaction flow
  - project context / tool query context

### 6.12 会话系统

- 当前目录：
  - `src/session/**`
- 目标 crate：
  - `magi-session-store`
- 覆盖能力：
  - session lifecycle
  - timeline
  - notifications
  - projection
  - session index persistence

### 6.13 工作区 / 隔离 / 快照系统

- 当前目录：
  - `src/workspace/**`
  - `src/snapshot-manager.ts`
- 目标 crate：
  - `magi-workspace`
- 覆盖能力：
  - workspace registry
  - workspace roots
  - git worktree
  - snapshot
  - merge / reconcile
  - isolation

### 6.14 LLM 适配系统

- 当前目录：
  - `src/llm/**`
  - `src/normalizer/**`
- 目标形态：
  - 短中期：`bridges/model`
  - 长期可选：`magi-model-runtime`
- 覆盖能力：
  - provider adapters
  - protocol adapters
  - retry / timeout / stream
  - normalizer
  - tool-call protocol interop

### 6.15 治理 / 审批 / 沙箱 / 安全系统

- 当前目录：
  - `src/governance/**`
  - `src/tools/tool-policy.ts`
  - `src/tools/shell/sandbox-policy.ts`
  - 相关 approval / safeguard 逻辑
- 目标 crate：
  - `magi-governance`
- 覆盖能力：
  - approval policy
  - tool policy
  - sandbox policy
  - risk gating
  - governance thresholds

### 6.16 可观测性 / 审计 / 用量系统

- 当前目录：
  - `src/usage-authority/**`
  - `src/events.ts`
  - observability / trace / rollout 相关链路
- 目标 crate：
  - `magi-event-bus`
- 覆盖能力：
  - runtime events
  - usage ledger
  - audit trail
  - rollout / diagnostics / metrics

---

## 7. 目标仓库结构

建议的最终仓库结构：

```text
magi/
  Cargo.toml
  crates/
    magi-daemon
    magi-api
    magi-core
    magi-orchestrator
    magi-worker-runtime
    magi-tool-runtime
    magi-skill-runtime
    magi-knowledge-store
    magi-memory-store
    magi-context-runtime
    magi-session-store
    magi-workspace
    magi-governance
    magi-event-bus
    magi-bridge-client
  apps/
    daemon
  web/
    svelte-app
  hosts/
    vscode
    idea
  bridges/
    model
    mcp
  schema/
    api/
    events/
    host-bridge/
    tool-protocol/
    task-protocol/
```

### 7.1 `magi-core` 的定位

`magi-core` 只放领域模型与错误码：

- ID types
- task / worker / session / workspace types
- DTO shared types
- enums / lifecycle states
- canonical error codes

不得在 `magi-core` 中引入：

- 文件系统
- 网络
- 进程
- 宿主 API

### 7.2 `magi-bridge-client` 的定位

Rust core 只通过 `magi-bridge-client` 访问外部桥：

- host bridge
- model bridge
- mcp bridge

这样 Rust core 不会被 JS/TS 生态反向污染。

---

## 8. 协议与真相源设计

### 8.1 单一协议真相源

建立 `schema/` 作为唯一协议真相源，统一定义：

- API DTO
- SSE Event schema
- Host Bridge schema
- Tool protocol
- Task protocol
- 错误码
- version handshake

禁止：

- Rust 手写一份 DTO
- 前端再手写一份 DTO
- Host Bridge 各宿主各自定义不兼容结构

### 8.2 协议建议

- UI ↔ Rust：`HTTP + SSE`
- Rust ↔ Host：`JSON-RPC 2.0 over stdio`
- Rust ↔ Model Bridge：`HTTP / JSON-RPC`
- Rust ↔ MCP Bridge：`HTTP / stdio bridge`

### 8.3 版本约束

所有跨边界协议必须带版本：

- `api_version`
- `event_schema_version`
- `bridge_protocol_version`

不兼容变更必须显式 bump，禁止静默破坏。

---

## 9. 阶段迁移方案

### Phase 0：边界冻结

目标：

- 冻结协议
- 冻结能力域边界
- 冻结后续多 Agent 写域

输出物：

- `schema/`
- 错误码表
- crate/package owner 列表
- 迁移依赖图

完成标准：

- 新增功能不再直接往现有巨型模块继续灌逻辑
- 新改造一律先经过 schema 与边界检查

### Phase 1：Rust 外壳启动

目标：

- 建立 Rust daemon 基座

范围：

- `magi-daemon`
- `magi-api`
- `magi-core`

最小能力：

- `/health`
- workspace register
- bootstrap
- SSE 空流
- version handshake

### Phase 2：状态内核迁移

目标：

- 先迁状态与隔离，不先迁复杂编排

范围：

- `magi-session-store`
- `magi-workspace`
- `magi-event-bus`
- `magi-governance`

完成标准：

- session / timeline / snapshot / worktree / audit 已由 Rust 管理

### Phase 3：执行内核迁移

目标：

- 迁核心执行链

范围：

- `magi-tool-runtime`
- `magi-orchestrator`
- `magi-worker-runtime`

完成标准：

- Rust 已能驱动任务执行主链
- Tool execution / worker lifecycle / runtime state 收口到 Rust

### Phase 4：知识、记忆、上下文迁移

目标：

- 迁长期价值域

范围：

- `magi-knowledge-store`
- `magi-memory-store`
- `magi-context-runtime`
- `magi-skill-runtime`

完成标准：

- 知识 / memory / context 已由 Rust 真相源托管

### Phase 5：桥接与宿主收口

目标：

- 宿主彻底退化为壳与桥

范围：

- `hosts/vscode`
- `bridges/model`
- `bridges/mcp`

完成标准：

- Rust core 不再依赖 `vscode`
- VSCode 只作为 host bridge + webview shell

### Phase 6：IDEA 宿主接入

目标：

- 完成第二宿主接入

范围：

- `hosts/idea`

完成标准：

- IDEA 只需实现 Host Bridge，不修改 core runtime

---

## 10. 多 Agent 并行推进方案

本节是后续派工时的核心执行规则。

### 10.1 写域隔离原则

每个 Agent 只拥有一个明确写域：

- 一个 crate
- 一个 package
- 或 `schema/`

禁止：

- 一个 Agent 同时修改多个核心 crate
- 多个 Agent 共享一个核心 crate 的写入权限

### 10.1.1 多 Agent 执行时的重构权限

在不越过写域边界的前提下，每个 Agent 都有权对所属模块执行必要重构，包括：

- 模块拆分
- 接口收敛
- 冗余逻辑删除
- 巨型文件压缩
- 旧实现替换

但必须遵守以下约束：

1. 重构必须服务于本轮平台化目标，不能脱离主线做无边界重写
2. 重构必须删除旧实现，禁止形成双实现并存
3. 涉及跨边界协议调整时，必须先更新 `schema/`
4. 重构后必须完成清理与验证，不能留下临时适配残留

### 10.1.2 多 Agent 统一工程规范

所有 Agent 在执行各自任务时，必须统一遵循
[$cn-engineering-standard](/Users/xie/.codex/skills/cn-engineering-standard/SKILL.md)。

最低执行要求：

- 中文沟通
- 根因导向
- 拒绝补丁式修复
- 拒绝回退逻辑
- 拒绝双轨实现
- 完成后清理废弃代码
- 做到“发现-修复-清理-测试-验证”闭环

若某个 Agent 的方案违反上述要求，集成阶段应直接退回，不进入主线合并。

### 10.2 推荐 Agent 拆分

#### Agent A：Schema Owner

- 负责：
  - `schema/`
- 输出：
  - API schema
  - event schema
  - host bridge schema
  - tool/task protocol schema
- 限制：
  - 不得改业务 crate

#### Agent B：Core / API

- 负责：
  - `magi-core`
  - `magi-api`
- 输出：
  - 领域模型
  - DTO 映射
  - 错误码对齐

#### Agent C：Daemon

- 负责：
  - `magi-daemon`
- 输出：
  - 进程入口
  - 配置
  - 生命周期

#### Agent D：Session

- 负责：
  - `magi-session-store`
- 输出：
  - session / timeline / notifications / persistence

#### Agent E：Workspace

- 负责：
  - `magi-workspace`
- 输出：
  - workspace registry
  - snapshot
  - worktree
  - merge / reconcile

#### Agent F：Governance / Event Bus

- 负责：
  - `magi-governance`
  - `magi-event-bus`
- 输出：
  - approval / sandbox / audit / usage / runtime events

#### Agent G：Tool Runtime

- 负责：
  - `magi-tool-runtime`
- 输出：
  - builtin tools
  - policy-aware tool execution

#### Agent H：Orchestrator

- 负责：
  - `magi-orchestrator`
- 输出：
  - task system
  - dispatch
  - plan ledger
  - runtime state

#### Agent I：Worker Runtime

- 负责：
  - `magi-worker-runtime`
- 输出：
  - worker lifecycle
  - todo loop
  - verification / repair / review

#### Agent J：Knowledge

- 负责：
  - `magi-knowledge-store`
- 输出：
  - ADR / FAQ / learning / audit

#### Agent K：Memory / Context

- 负责：
  - `magi-memory-store`
  - `magi-context-runtime`
- 输出：
  - memory
  - context assembly
  - compaction / budget / retrieval

#### Agent L：Skills

- 负责：
  - `magi-skill-runtime`
- 输出：
  - skill registry
  - prompt injection
  - custom tool binding

#### Agent M：MCP Bridge

- 负责：
  - `bridges/mcp`
- 输出：
  - MCP server registry
  - transport
  - tool/prompt discovery

#### Agent N：Model Bridge

- 负责：
  - `bridges/model`
- 输出：
  - provider adapters
  - streaming
  - normalizer bridge

#### Agent O：VSCode Host

- 负责：
  - `hosts/vscode`
- 输出：
  - openFile / diff / diagnostics / symbol / terminal 桥接

#### Agent P：Svelte Frontend

- 负责：
  - `web/svelte-app`
- 输出：
  - 统一前端

#### Agent Q：Integration Owner

- 负责：
  - 集成、依赖对齐、兼容修复
- 限制：
  - 不得长期接管其他 Agent 的主写域

### 10.3 合并顺序

必须按以下顺序合并：

1. `schema`
2. `magi-core / magi-api / magi-daemon`
3. `magi-session-store / magi-workspace / magi-governance / magi-event-bus`
4. `magi-tool-runtime`
5. `magi-orchestrator / magi-worker-runtime`
6. `magi-knowledge-store / magi-memory-store / magi-context-runtime / magi-skill-runtime`
7. `bridges/model / bridges/mcp`
8. `hosts/*`
9. `web/svelte-app`

### 10.4 防冲突硬规则

后续并行推进时必须遵守：

1. 每个 Agent 使用独立 git worktree
2. `schema/` 只有 Schema Owner 可写
3. generated code 目录只允许 codegen agent 写入
4. 所有跨边界接口变更必须先改 schema
5. 宿主 Agent 不得修改 Rust core
6. 前端 Agent 不得补后端语义缺口
7. 集成 Agent 不得擅自重构业务边界

---

## 11. 当前目录到目标结构的映射表

| 当前目录 / 文件 | 目标归属 |
|---|---|
| `src/agent/**` | `magi-daemon` + `magi-api` |
| `src/orchestrator/**` | `magi-orchestrator` + `magi-worker-runtime` |
| `src/tools/**` | `magi-tool-runtime` + `magi-skill-runtime` + `bridges/mcp` |
| `src/session/**` | `magi-session-store` |
| `src/workspace/**` + `src/snapshot-manager.ts` | `magi-workspace` |
| `src/context/**` | `magi-memory-store` + `magi-context-runtime` |
| `src/knowledge/**` | `magi-knowledge-store` |
| `src/llm/**` + `src/normalizer/**` | `bridges/model` |
| `src/governance/**` | `magi-governance` |
| `src/usage-authority/**` + events / observability | `magi-event-bus` |
| `src/ui/**` | `web/svelte-app` |
| `src/extension.ts` + `src/ui/webview-provider.ts` + `src/host/**` | `hosts/vscode` |

---

## 12. 关键风险与处理策略

### 12.1 风险：先迁语言，后补边界，导致重复返工

处理：

- 先完成 Phase 0
- 任何 Agent 开始写 Rust 前，必须先确认所属边界

### 12.2 风险：前端继续补语义，导致后端真相源失控

处理：

- 明确 UI 只做 projection render
- 所有状态含义必须由 Rust 后端统一输出

### 12.3 风险：MCP / LLM 生态拖慢主线

处理：

- 短中期先走 bridge，不强求一步 Rust 化
- 平台主线优先于生态收口

### 12.4 风险：多 Agent 并行引发接口震荡

处理：

- schema owner 先行
- 版本握手与兼容测试前置
- Integration Owner 只做收口，不做边界重画

### 12.5 风险：宿主能力重新污染 core runtime

处理：

- 将 Host Bridge 作为硬边界
- 新增 IDE 能力时只能扩 bridge schema，不得扩 core 直接依赖

---

## 13. 验收标准

当本次平台化改造完成时，必须满足以下标准：

### 13.1 平台级验收

1. 后端启动不依赖 VSCode
2. Rust Runtime 内无任何 IDE SDK 直接依赖
3. 前端连接的是 Rust daemon，而不是宿主进程内对象
4. 新宿主接入只需实现 Host Bridge，不需要修改 core

### 13.2 运行时验收

1. Session / task / worker / tool / approval / runtime state 全部以 Rust 为真相源
2. 两个以上 worker 并发执行时，隔离与写入边界可控
3. 刷新 / 重启 / 恢复后，任务与会话状态可正确重建
4. Knowledge / memory / context 能在长任务中稳定工作

### 13.3 能力域验收

1. builtin tools 能在 Rust 中完整注册与执行
2. MCP / Skill / knowledge / memory / context 都有稳定边界
3. task system、记忆系统、上下文系统不再散落在前端或宿主中补逻辑
4. governance / audit / usage 形成统一可观测主链

### 13.4 多宿主验收

1. VSCode 壳正常工作
2. IDEA 宿主可正常接入
3. Web 前端与 IDE 前端共享同一套运行时视图模型

---

## 14. 近期执行建议

如果按现实推进节奏，我建议先做以下 4 件事：

1. 建立 `schema/` 与版本策略
2. 抽离 `Host Bridge`，停止 core 直接依赖 `vscode`
3. 建立 Rust `magi-daemon / magi-api / magi-core` 空骨架
4. 先迁 `session / workspace / governance / event bus`，再迁执行内核

---

## 15. 最终结论

`Magi` 的这次改造，不应被理解为“把 Node 后端改写成 Rust”，而应被理解为：

> 将 `Magi` 从一个带 IDE 壳的多智能体工程产品，升级为一个独立的、本地常驻的、跨宿主的 Agent 平台内核。

因此，本次改造必须覆盖：

- Agent API
- 任务系统
- Worker 系统
- 内置工具
- MCP
- Skill
- 知识库
- 记忆系统
- 上下文系统
- 会话系统
- 工作区隔离
- 治理 / 沙箱 / 审批
- 审计 / 用量 / 观测

只有把这些能力域全部纳入统一平台边界，Rust 平台化才是完整的，而不是局部语言替换。
