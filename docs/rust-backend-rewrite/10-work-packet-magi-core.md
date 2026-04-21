# Agent 任务单：magi-core 基础模型

更新时间：2026-04-15

---

## 1. 任务名称

- 名称：`magi-core` 基础模型任务单
- 编号：`WP-CORE-001`
- 负责 Agent：Core Agent

## 2. 写域

- 唯一写域：`crates/magi-core`
- 禁止修改范围：
  - 现有 `src/**` 运行代码
  - 其他 Rust crate
  - 宿主壳与前端
- 依赖的上游文档：
  - [magi-rust-platformization-master-plan.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/magi-rust-platformization-master-plan.md)
  - [02-capability-matrix.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/02-capability-matrix.md)
  - [03-semantic-deviation-ledger.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/03-semantic-deviation-ledger.md)
  - [04-module-mapping-and-target-crates.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/04-module-mapping-and-target-crates.md)

## 3. 背景

- 当前能力域：后端统一领域模型
- 当前实现位置：
  - `src/types.ts`
  - `src/types/**`
  - `src/protocol/**`
  - `src/task/**`
  - `src/todo/**`
  - `src/session/**` 中基础状态定义
- 当前问题：
  - 关键 ID 与状态分散在多个目录
  - 多处运行态、任务态、会话态定义重复
  - 存在 `unknown` 透传与弱约束结构

## 4. 根本原因

1. 当前 TypeScript 后端是边演进边加能力，公共领域模型没有先被收口
2. 旧实现更偏“模块内部自带类型”，没有形成真正的平台级核心模型层
3. 如果不先立 `magi-core`，后续每个 Rust crate 都会自定义一套状态结构，重演当前问题

## 5. 目标

- 本任务要完成的 Rust 目标结构：
  - 建立 `magi-core` crate 骨架
  - 建立统一 ID 类型、生命周期枚举、公共错误码、基础领域对象
  - 建立跨 crate 可共享的最小领域模型层
- 本任务不做什么：
  - 不做 IO
  - 不做 API 路由
  - 不做持久化
  - 不做工具执行
- 与其他 Agent 的边界：
  - 只输出公共模型
  - 不定义具体 store / runtime 实现

## 6. 产出物

- 新增 crate / module：
  - `crates/magi-core`
  - `ids`
  - `errors`
  - `session`
  - `task`
  - `worker`
  - `runtime`
- 新增 schema：
  - 无，若涉及跨边界 DTO 调整，需先回到 `schema/`
- 更新文档：
  - 如有模型取舍，更新能力对照表和语义偏差台账
- 必须删除的旧实现或冗余结构：
  - 当前阶段不删除主仓运行代码
  - 但禁止在 `magi-core` 内复制旧 TS 式多重定义

## 7. 语义约束

- 本任务涉及的真相源：
  - Session 基础状态
  - Task / Assignment / Todo / Worker 基础状态
  - Runtime 基础原因与生命周期状态
- 是否涉及协议变化：
  - 否，当前阶段先立内部模型
- 是否涉及语义偏差台账登记：
  - 若发现旧状态无法收敛，必须更新 `D-003`、`D-004`

## 8. 质量要求

必须遵循
[$cn-engineering-standard](/Users/xie/.codex/skills/cn-engineering-standard/SKILL.md)，并满足：

- 中文沟通
- 根因导向
- 禁止补丁式修复
- 禁止回退逻辑
- 禁止双实现并存
- 完成后清理废弃代码
- 完成“发现-修复-清理-测试-验证”闭环

额外要求：

- `magi-core` 不得依赖文件系统、网络、进程、IDE SDK
- 模块尺寸必须受控，禁止再造超级核心文件

## 9. 验收标准

- 编译：
  - `magi-core` 可独立编译
- 最小运行验证：
  - 基础模型可被其他 crate 引用
- 协议验证：
  - 若引入跨边界结构，必须说明为何不应先落 `schema/`
- 清理验证：
  - `magi-core` 内无重复定义、无回退结构

## 10. 输出结论

- 已完成内容：
  - 待实现
- 删除内容：
  - 待实现
- 未完成边界：
  - 待实现
- 后续依赖：
  - `magi-daemon`
  - `magi-api`
  - `magi-session-store`
  - `magi-orchestrator`
