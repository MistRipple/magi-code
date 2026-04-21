# 本地影子 Rust 工作区落地方案

更新时间：2026-04-15

> 本文档用于定义“当前不接入现有运行链路的本地影子 Rust 后端”应如何组织、起盘与初始化。
>
> 目标不是立即上线运行，而是为多 Agent 并行重构提供统一的 Rust 工作区真相源。

---

## 1. 文档目的

本文件要回答以下问题：

1. 本地影子 Rust 工程默认放在哪里
2. `Cargo workspace` 应如何组织
3. 第一批 crate 应按什么顺序创建
4. crate 依赖方向和禁止依赖是什么
5. 通用基础设施约定是什么
6. 当前阶段哪些桥接边界明确不进入 core crate

---

## 2. 默认落地方式

### 2.1 推荐位置

默认推荐使用当前主仓的兄弟目录：

- `/Users/xie/code/magi-rust-rewrite`

推荐原因：

- 不污染当前主仓运行链路
- 不把重构产物误混入当前实现
- 便于独立版本管理和独立验证

### 2.2 若必须位于当前工程目录内

仅允许放在本地忽略路径，例如：

- `/Users/xie/code/magi/.local-rewrite/magi-rust-backend`

附加要求：

- 该目录必须本地忽略
- 不得进入当前主线构建脚本
- 不得被现有 `npm` 流程误引用

### 2.3 当前禁止事项

当前阶段禁止：

- 把影子 Rust 工作区直接纳入当前主仓正式运行链路
- 在现有前端或宿主中临时接入影子后端
- 通过兼容脚本让两套后端同时承担同一职责

---

## 3. Cargo Workspace 顶层结构

推荐的顶层结构固定如下：

- `Cargo.toml`
- `Cargo.lock`
- `crates/`
- `apps/`
- `support/`
- `tmp/`

说明：

- `crates/` 放核心 crate
- `apps/` 只放可执行入口，例如 daemon
- `support/` 放本地开发辅助，不放核心领域逻辑
- `tmp/` 放本地运行输出，不作为长期持久化真相

### 3.1 `crates/` 目录

当前固定的目标 crate 为：

- `magi-core`
- `magi-daemon`
- `magi-api`
- `magi-session-store`
- `magi-workspace`
- `magi-governance`
- `magi-event-bus`
- `magi-tool-runtime`
- `magi-orchestrator`
- `magi-worker-runtime`
- `magi-knowledge-store`
- `magi-memory-store`
- `magi-context-runtime`
- `magi-skill-runtime`
- `magi-bridge-client`

### 3.2 `apps/` 目录

当前只允许出现：

- `daemon`

说明：

- `daemon` 负责可执行入口
- 不在当前阶段创建额外 CLI 或调试壳

---

## 4. 第一批 crate 创建顺序

第一批只允许先创建以下 7 个 crate：

1. `magi-core`
2. `magi-daemon`
3. `magi-api`
4. `magi-session-store`
5. `magi-workspace`
6. `magi-governance`
7. `magi-event-bus`

原因：

- 这 7 个 crate 对应 `M2` 前必须稳定的硬边界
- 它们先成立，后续执行主链和长期能力域才有稳定承载面

### 4.1 每个 crate 的初始化内容

`magi-core`

- 统一 ID 类型
- 领域状态枚举
- 公共错误类型骨架
- 时间、路径、引用对象的公共值对象

`magi-daemon`

- 进程入口
- 配置加载
- 生命周期管理
- 依赖装配顺序

`magi-api`

- 路由骨架
- DTO 边界
- SSE 出口骨架
- 与 runtime/store 的应用层装配接口

`magi-session-store`

- session aggregate 骨架
- timeline / notification store 骨架
- 基础持久化接口

`magi-workspace`

- workspace registry 骨架
- worktree / snapshot / recovery 接口骨架

`magi-governance`

- approval policy
- tool policy
- sandbox policy
- 风险等级与治理决策骨架

`magi-event-bus`

- domain event
- audit event
- usage event
- UI projection event 的分层模型

---

## 5. 依赖方向与禁止依赖

### 5.1 固定依赖方向

推荐的固定依赖方向如下：

- `magi-daemon -> magi-api`
- `magi-api -> magi-core`
- `magi-api -> magi-session-store / magi-workspace / magi-governance / magi-event-bus`
- `runtime/store crates -> magi-core`
- `runtime/store crates -> magi-governance / magi-event-bus / magi-bridge-client` 仅在确有边界需求时允许

### 5.2 强禁止依赖

以下依赖严格禁止：

- `magi-core -> 任意 runtime/store/api/daemon crate`
- `magi-session-store -> magi-api`
- `magi-workspace -> magi-api`
- `magi-governance -> 宿主桥接实现`
- `magi-event-bus -> UI 或宿主 SDK`
- 任意 core crate 直接依赖 IDE SDK
- 任意 core crate 直接依赖 `bridges/model`、`bridges/mcp`、`hosts/vscode`、`hosts/idea` 的具体实现

---

## 6. 通用基础设施约定

### 6.1 错误类型分层

错误类型按三层组织：

1. `domain error`
2. `application error`
3. `transport error`

规则：

- `magi-core` 只定义 domain error 基础骨架
- `magi-api` 只做 transport 映射
- 不允许把 HTTP 语义带进 domain error

### 6.2 `serde` 序列化约定

统一要求：

- 对外结构必须使用稳定、可预期的字段命名
- 不直接透传内部聚合对象
- 需要扩展时优先使用显式可选字段或 `metadata`

当前禁止：

- 直接把内部 `enum` 或实现细节暴露为对外协议
- 用非受控 map 结构承载长期稳定字段

### 6.3 `tracing` 日志字段

统一基础字段：

- `workspace_id`
- `session_id`
- `mission_id`
- `assignment_id`
- `todo_id`
- `worker_id`
- `tool_name`
- `event_type`

规则：

- 日志字段名称必须与协议中的稳定关联字段保持一致
- 禁止为同一对象引入多套命名

### 6.4 ID 类型命名

统一使用：

- `WorkspaceId`
- `SessionId`
- `MissionId`
- `AssignmentId`
- `TodoId`
- `WorkerId`
- `ToolCallId`
- `EventId`

禁止：

- 直接使用裸字符串长期横穿多个 crate
- 同一对象在不同 crate 中使用不同命名

### 6.5 时间、路径、状态枚举

时间：

- 统一使用 UTC 存储语义
- 对外展示时再做时区转换

路径：

- 必须区分 `workspace root`、`worktree root`、`file path`
- 不允许路径语义混装

状态枚举：

- 生命周期状态必须显式枚举
- 不允许使用字符串字面量散落在多个 crate 中

---

## 7. 持久化默认策略

### 7.1 当前默认策略

在 `M2` 前，统一采用：

- 文件持久化
- 原子写
- 显式快照或显式日志文件

原因：

- 当前目标是稳定边界与模型
- 不是提前做数据库选型

### 7.2 当前禁止事项

在 `M2` 前禁止：

- 引入额外数据库决定
- 让不同 crate 各自定义不同持久化风格
- 一边做领域建模，一边做数据库抽象漂移

---

## 8. 外部桥接边界

以下边界当前明确视为外部边界：

- `bridges/model`
- `bridges/mcp`
- `hosts/vscode`
- `hosts/idea`

规则：

- 它们不进入 core crate
- Rust core 只允许通过 `magi-bridge-client` 与其交互
- 宿主私有结构、模型 SDK 细节、MCP transport 细节都必须停留在边界外

---

## 9. 代码质量硬门禁

### 9.1 单一职责

- 一个 crate 只承担一个能力域
- 一个 crate 内部也必须继续分模块

### 9.2 模块体积限制

- 普通模块目标不超过 `400` 行
- 超过 `600` 行必须评估拆分
- 超过 `800` 行视为结构异常

### 9.3 严禁 1:1 翻译

禁止把以下旧结构直接平移到 Rust：

- 巨型 service
- 巨型 manager
- 混装 API / 运行态 / 投影 / 审计 / 宿主桥接的超级对象

### 9.4 严禁兼容沉积

禁止：

- 回退逻辑
- 双实现并存
- 兼容分支掩盖边界问题
- “先复制旧逻辑，之后再慢慢清理”

---

## 10. 当前起盘顺序建议

当前推荐的起盘顺序是：

1. 先建立 workspace 根结构与空 crate
2. 先落 `magi-core`
3. 再落 `magi-daemon` 和 `magi-api`
4. 再落 `magi-session-store`、`magi-workspace`
5. 再落 `magi-governance`、`magi-event-bus`
6. 硬边界稳定后，再进入执行主链与长期能力域

---

## 11. 当前结论

本地影子 Rust 工作区的目标不是“尽快跑起来”，而是：

- 先把边界、依赖、命名和基础设施约定冻结
- 让多个 Agent 在统一工作区结构上协作
- 防止后续出现多个起盘方式、多套基础设施约定和不可收敛的 crate 依赖
