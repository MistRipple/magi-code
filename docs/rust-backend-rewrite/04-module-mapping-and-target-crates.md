# 现有模块到 Rust 目标 Crate 的映射

更新时间：2026-04-15

> 本文档用于建立“当前 `src/` 结构”与“Rust 目标 crate”之间的稳定映射。
>
> 该映射不是 1:1 文件翻译关系，而是平台责任重画后的目标归属关系。
>
> 当前前提：
>
> 1. 先做本地影子 Rust 后端重构
> 2. 当前不接现有前后端运行链路
> 3. 先完成后端内核收口，再统一评估切换

---

## 1. 文档目的

本文件要回答 4 个问题：

1. Rust 后端应该拆成哪些 crate
2. 每个 crate 的职责边界和禁止事项是什么
3. 现有 `src/` 代码应当迁入哪些 crate
4. 后续多 Agent 并行时，哪些 crate 适合先建、先做、先验收

本文件是后续 crate 设计和任务拆分的前置真相源。

---

## 2. Rust 工作区目标结构

建议的 Rust 工作区结构：

```text
repo-root/
  Cargo.toml
  crates/
    magi-core
    magi-daemon
    magi-api
    magi-session-store
    magi-workspace
    magi-governance
    magi-event-bus
    magi-tool-runtime
    magi-orchestrator
    magi-worker-runtime
    magi-knowledge-store
    magi-memory-store
    magi-context-runtime
    magi-skill-runtime
    magi-bridge-client
  apps/
    daemon
```

说明：

- 当前影子 workspace 已实际落在仓库根目录，而不是单独再套一层 `rust/`
- 当前阶段不要求把 `model bridge`、`mcp bridge` 直接写成 Rust crate
- `hosts/vscode`、`hosts/idea` 仍属于宿主壳/桥接层，不进入本 Rust 工作区

---

## 3. Crate 设计总原则

### 3.1 单 crate 单能力域

每个 crate 只负责一个清晰能力域。

禁止：

- 一个 crate 同时承担 API + 存储 + 调度 + 宿主桥接
- 一个 crate 继续复制当前“超级管理器”模式

### 3.2 依赖方向必须单向

推荐依赖方向：

```text
magi-daemon
  -> magi-api
    -> magi-core
    -> 各 runtime / store crates

runtime / store crates
  -> magi-core
  -> 必要时 -> magi-event-bus / magi-governance / magi-bridge-client
```

禁止：

- `magi-core` 依赖任何 runtime crate
- store crate 反向依赖 api crate
- 宿主桥接直接污染 core

### 3.3 crate 内部也必须分模块

crate 本身不是“大文件收容器”。

每个 crate 内建议继续拆分为：

- `types`
- `service`
- `store`
- `policy`
- `read_model`
- `errors`

### 3.4 迁移时允许结构性重构

crate 设计以目标职责边界为主，不以旧文件分布为主。

允许：

- 拆分旧巨型文件
- 收口接口
- 删除重复实现
- 强类型化旧 `unknown` / 透传结构

---

## 4. 各 Crate 职责边界

本节是后续真正开工时最重要的部分。

### 4.1 `magi-core`

职责：

- 领域模型
- 生命周期枚举
- 公共错误码
- 统一 ID 类型
- 跨 crate 共享的协议内核对象

应包含：

- `WorkspaceId`
- `SessionId`
- `MissionId`
- `AssignmentId`
- `TodoId`
- `WorkerId`
- `RuntimeReason`
- `Task / Worker / Session` 的基础状态类型

不得包含：

- 文件系统
- 网络
- 子进程
- IDE SDK
- 具体持久化实现

### 4.2 `magi-daemon`

职责：

- 本地常驻进程入口
- 配置加载
- 生命周期管理
- 信号处理
- 服务启动与关闭

应包含：

- `main`
- daemon bootstrap
- 运行时初始化顺序

不得包含：

- HTTP 路由细节
- 业务状态机
- IDE SDK

### 4.3 `magi-api`

职责：

- HTTP API
- SSE 事件流出口
- DTO 输入输出校验
- 与 runtime / store 的应用层装配

应包含：

- `/health`
- `/bootstrap`
- `/events`
- workspace / session / knowledge / changes / stats 相关路由

不得包含：

- 任务调度核心逻辑
- 会话持久化核心逻辑
- 工具执行细节

### 4.4 `magi-session-store`

职责：

- session 聚合
- timeline
- notifications
- session index
- session projection 的基础持久化输入

应包含：

- session create / rename / delete / switch
- timeline append / hydrate
- notification persistence

不得包含：

- 读模型渲染逻辑
- orchestrator 业务决策
- UI 投影视图拼装

### 4.5 `magi-workspace`

职责：

- workspace registry
- workspace roots
- worktree
- snapshot
- recovery 关联的工作区级资源

应包含：

- workspace registration
- git worktree isolation
- workspace snapshot / merge / reconcile

不得包含：

- session timeline 语义
- orchestrator 决策逻辑

### 4.6 `magi-governance`

职责：

- approval policy
- sandbox policy
- tool policy
- risk thresholds
- 自动/询问策略判定

应包含：

- 风险阈值
- 权限矩阵
- tool allow / deny / ask
- shell sandbox 语义

不得包含：

- 实际工具执行
- 实际任务调度

### 4.7 `magi-event-bus`

职责：

- domain events
- audit events
- usage ledger
- rollout / diagnostics 事件主链
- read model 的统一事件输入

应包含：

- 事件类型定义
- event append / publish
- usage snapshot rebuild
- audit trail

不得包含：

- UI 特化结构
- Host 专属消息格式

### 4.8 `magi-tool-runtime`

职责：

- builtin tool registry
- 文件/搜索/shell/process/diff 等工具执行
- tool execution context
- 并发写防护

应包含：

- file tools
- search tools
- shell / process runtime
- builtin tool schema

不得包含：

- MCP server 连接管理
- skill prompt 注入
- 宿主桥接逻辑

### 4.9 `magi-orchestrator`

职责：

- Mission / Assignment / Todo 主模型
- 计划与派发控制
- dispatch 控制面
- replan / recovery / summary

应包含：

- request classify
- plan ledger
- dispatch policy
- runtime control plane

不得包含：

- worker 内部执行循环
- tool 执行细节
- API 入口装配

### 4.10 `magi-worker-runtime`

职责：

- worker lifecycle
- todo execution loop
- verification / review / repair
- worker quality gate

应包含：

- worker state transitions
- report aggregation
- verification state

不得包含：

- 任务全局派发规则
- API 路由

### 4.11 `magi-knowledge-store`

职责：

- ADR / FAQ / learning 存储
- 代码索引
- governed knowledge query

应包含：

- indexer
- storage
- query service
- governed output / audit

不得包含：

- context budget
- session memory 提取

### 4.12 `magi-memory-store`

职责：

- session memory
- layered memory
- memory extraction persistence
- memory compaction 结果存储

应包含：

- memory summary
- raw memories
- preference memory

不得包含：

- context assembly 主流程
- PKB 查询逻辑

### 4.13 `magi-context-runtime`

职责：

- context assembly
- token budget
- shared context pool
- file summary cache
- recent turns 收集

应包含：

- budget resolver
- context assembler
- overflow trimming

不得包含：

- LLM provider 调用
- knowledge 存储
- session memory 持久化

### 4.14 `magi-skill-runtime`

职责：

- instruction skill registry
- skill metadata
- custom tool binding
- skill prompt injection policy

应包含：

- skill loading
- custom tool descriptors
- allowed tools / invocation constraints

不得包含：

- builtin tool runtime
- MCP transport

### 4.15 `magi-bridge-client`

职责：

- Rust Core 与外部桥的统一客户端层
- host bridge client
- model bridge client
- mcp bridge client

应包含：

- 协议客户端
- 请求/响应映射
- 错误边界统一

不得包含：

- bridge 服务端实现
- 业务真相源

---

## 5. 推荐依赖关系

建议依赖图：

```text
magi-daemon
  -> magi-api

magi-api
  -> magi-core
  -> magi-session-store
  -> magi-workspace
  -> magi-governance
  -> magi-event-bus
  -> magi-tool-runtime
  -> magi-orchestrator
  -> magi-worker-runtime
  -> magi-knowledge-store
  -> magi-memory-store
  -> magi-context-runtime
  -> magi-skill-runtime
  -> magi-bridge-client

magi-orchestrator
  -> magi-core
  -> magi-governance
  -> magi-event-bus
  -> magi-tool-runtime
  -> magi-session-store
  -> magi-workspace
  -> magi-context-runtime
  -> magi-knowledge-store
  -> magi-memory-store
  -> magi-bridge-client

magi-worker-runtime
  -> magi-core
  -> magi-tool-runtime
  -> magi-governance
  -> magi-event-bus
  -> magi-bridge-client
```

说明：

- `magi-api` 是装配层，可以依赖多个 crate
- `magi-core` 必须处于依赖图底部
- `magi-bridge-client` 是外部桥接入口，不反向持有业务状态

---

## 6. 现有模块到目标 Crate 的映射

### 6.1 入口与运行时装配

| 当前目录 / 文件 | 当前职责 | Rust 目标归属 | 迁移说明 |
|---|---|---|---|
| `src/agent/main.ts` | 独立进程入口 | `magi-daemon` | 迁为 daemon main |
| `src/agent/service/local-agent-service.ts` | HTTP 服务、workspace/session/knowledge/files/changes API 混装 | `magi-api` + `magi-daemon` + 局部 service crate | 必须拆开 |
| `src/agent/service/agent-runtime-service.ts` | runtime 装配、读模型输出、桥接消息混装 | `magi-api` + `magi-orchestrator` + `magi-event-bus` | 禁止整体照搬 |

### 6.2 任务与执行主链

| 当前目录 / 文件 | 当前职责 | Rust 目标归属 | 迁移说明 |
|---|---|---|---|
| `src/orchestrator/core/mission-driven-engine.ts` | 编排总控、治理、恢复、汇总混装 | `magi-orchestrator` | 必须拆为多个 service / state modules |
| `src/orchestrator/core/dispatch/dispatch-manager.ts` | dispatch、pipeline、resume guard 等混装 | `magi-orchestrator` | 作为调度控制面重构 |
| `src/orchestrator/worker/autonomous-worker.ts` | worker 执行循环、verification/review/repair | `magi-worker-runtime` | 独立成 worker lifecycle crate |
| `src/task/**` + `src/todo/**` | 任务视图与 Todo 状态 | `magi-orchestrator` | Todo 真相源必须收口 |

### 6.3 工具、治理与外部能力

| 当前目录 / 文件 | 当前职责 | Rust 目标归属 | 迁移说明 |
|---|---|---|---|
| `src/tools/tool-manager.ts` | builtin/MCP/skill/host/policy 混装 | `magi-tool-runtime` + `magi-skill-runtime` + `magi-bridge-client` + `magi-governance` | 必须解耦 |
| `src/tools/file-executor.ts` / `search-executor.ts` / `shell/**` | builtin tools | `magi-tool-runtime` | 可优先迁移 |
| `src/tools/mcp-manager.ts` / `mcp-executor.ts` | MCP transport / discovery / execute | `bridges/mcp` | 当前阶段桥接，不进 core |
| `src/tools/skills-manager.ts` | custom tool / instruction skill | `magi-skill-runtime` | 与 builtin tools 拆开 |
| `src/governance/**` + `src/tools/tool-policy.ts` + `src/tools/shell/sandbox-policy.ts` | 审批 / 风险 / sandbox | `magi-governance` | 必须单独收口 |

### 6.4 状态、恢复与审计

| 当前目录 / 文件 | 当前职责 | Rust 目标归属 | 迁移说明 |
|---|---|---|---|
| `src/session/**` | session / timeline / notifications / projection | `magi-session-store` | aggregate 与 projection 要分层 |
| `src/workspace/**` | workspace / worktree | `magi-workspace` | 工作区级边界 |
| `src/snapshot-manager.ts` | snapshot manager | `magi-workspace` | 与 recovery 一起治理 |
| `src/usage-authority/**` + `src/events.ts` + observability | usage / event / rollout / audit | `magi-event-bus` | 形成统一事件主链 |

### 6.5 知识、记忆与上下文

| 当前目录 / 文件 | 当前职责 | Rust 目标归属 | 迁移说明 |
|---|---|---|---|
| `src/knowledge/**` | PKB、ADR/FAQ/learning、governed query | `magi-knowledge-store` | index / store / query 分层 |
| `src/context/**` 中 memory 相关 | layered memory / memory summary | `magi-memory-store` | 与 context runtime 解耦 |
| `src/context/**` 中 assembler / cache / pool 相关 | context assembly / budget / shared context | `magi-context-runtime` | 单独收口 |

### 6.6 模型与宿主桥接

| 当前目录 / 文件 | 当前职责 | Rust 目标归属 | 迁移说明 |
|---|---|---|---|
| `src/llm/**` + `src/normalizer/**` | provider adapters / protocol / stream | `bridges/model` | 当前阶段不强制 Rust 化 |
| `src/host/**` | VSCode 宿主能力桥接 | `hosts/vscode` / `hosts/idea` | 从 Rust Core 中剥离 |

---

## 7. 第一批建议建立的 Crate

### P0：先立硬边界

1. `magi-core`
2. `magi-daemon`
3. `magi-api`
4. `magi-session-store`
5. `magi-workspace`
6. `magi-governance`
7. `magi-event-bus`

原因：

- 先把入口、状态、恢复、治理、审计的硬边界立住
- 这些 crate 一旦确定，后续执行主链才不会继续混装

### P1：再立执行内核

1. `magi-tool-runtime`
2. `magi-orchestrator`
3. `magi-worker-runtime`

原因：

- 这是后端真正的执行闭环
- 必须建立在 session/workspace/governance/event 基础之上

### P2：最后收长期能力和扩展层

1. `magi-knowledge-store`
2. `magi-memory-store`
3. `magi-context-runtime`
4. `magi-skill-runtime`
5. `magi-bridge-client`

原因：

- 这些能力必须建立在主链与边界稳定之后
- 否则容易被旧上下文结构反向污染

---

## 8. Crate 级质量门禁

后续新建 crate 必须满足：

1. 一个 crate 只承担一个主责任
2. crate 内部不得再出现新的“超级管理器”
3. 普通模块目标 `<= 400~600` 行
4. 超过 `800` 行必须拆
5. 不得把旧 `unknown` / 透传结构直接照搬进 Rust
6. 不得把宿主能力重新引入 core runtime

这部分和 `cn-engineering-standard` 一致：

- 根因导向
- 禁止双实现
- 禁止回退逻辑
- 发现-修复-清理-测试-验证闭环

---

## 9. 当前结论

如果后续发现现有 `src/` 结构存在更深层次的职责污染，应优先修订本映射表，再进入实现。

也就是说：

> crate 映射是重构前提，不是重构后的事后总结。

当前第一版 crate 设计已经足以支撑下一步：

1. 建立本地 Rust 工作区骨架
2. 准备 `magi-core / magi-daemon / magi-api / magi-session-store / magi-workspace`
3. 按 crate 写域拆分后续多 Agent 任务单
