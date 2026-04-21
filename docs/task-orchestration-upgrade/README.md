# Task Orchestration Upgrade Docs

更新时间：2026-04-16

本目录承载 Magi 后续统一 Task 编排升级的完整指导文档，作为后续设计、实现、迁移与评审的统一入口。

## 阅读顺序

### 1. 架构总纲
- [`unified-task-orchestration-kernel-upgrade-architecture.md`](./unified-task-orchestration-kernel-upgrade-architecture.md)
- 说明为什么要升级、主模型是什么、Mission/Task/Worker/Policy/Runner/Escalation 的总体关系

### 2. 协议与对象定义
- [`task-schema-and-contract.md`](./task-schema-and-contract.md)
- 定义 Mission、Worker、Task、TaskKind、TaskStatus、ExecutorBinding、TaskPolicy、TaskProjection、DecisionTaskPayload

### 3. 运行时协议
- [`runner-runtime-and-escalation-protocol.md`](./runner-runtime-and-escalation-protocol.md)
- 定义 Runner 主循环、Worker 领取与 lease、并发裁决、graph reflection、checkpoint/resume、decision 闭环

### 4. 迁移实施计划
- [`task-orchestration-migration-plan.md`](./task-orchestration-migration-plan.md)
- 定义从现有 Mission/Assignment/Todo 迁移到 Mission + Task Graph + Worker Binding 的阶段化计划

## 使用建议

- 架构评审先看第 1 篇
- 协议/存储/API 设计先看第 2 篇
- 运行时/调度/恢复实现先看第 3 篇
- 项目排期与切换治理先看第 4 篇

## 约束

- 本目录是后续升级指导的唯一文档集合
- 新增编排设计文档优先补充到本目录，而不是散落到 `docs/` 根目录
- `Todo` 不再作为正式模型使用
- 任何兼容层都只能是短期迁移措施，不能变成长期双系统
