# Rust 后端重构 Agent 任务单模板

更新时间：2026-04-15

> 本模板用于后续多个 Agent 并行推进 Rust 后端重构时的统一任务单格式。

---

## 1. 任务单模板

### 任务名称

- 名称：
- 编号：
- 负责 Agent：

### 写域

- 唯一写域：
- 禁止修改范围：
- 依赖的上游文档：

### 背景

- 当前能力域：
- 当前实现位置：
- 当前问题：

### 根本原因

使用 `5 Whys` 方式说明：

1. 为什么当前实现不合理
2. 为什么不能继续沿用旧结构
3. 为什么必须在本轮 Rust 重构中处理

### 目标

- 本任务要完成的 Rust 目标结构：
- 本任务不做什么：
- 与其他 Agent 的边界：

### 产出物

- 新增 crate / module：
- 新增 schema：
- 更新文档：
- 必须删除的旧实现或冗余结构：

### 语义约束

- 本任务涉及的真相源：
- 是否涉及协议变化：
- 是否涉及语义偏差台账登记：

### 质量要求

必须遵循
[$cn-engineering-standard](/Users/xie/.codex/skills/cn-engineering-standard/SKILL.md)，并满足：

- 中文沟通
- 根因导向
- 禁止补丁式修复
- 禁止回退逻辑
- 禁止双实现并存
- 完成后清理废弃代码
- 完成“发现-修复-清理-测试-验证”闭环

### 验收标准

- 编译：
- 最小运行验证：
- 协议验证：
- 清理验证：

### 输出结论

- 已完成内容：
- 删除内容：
- 未完成边界：
- 后续依赖：

---

## 2. 使用要求

1. 每个 Agent 只允许使用一张任务单对应一个稳定写域
2. 若任务涉及跨域协议变更，必须先改 `schema`
3. 若发现旧实现存在严重语义错误，必须先登记“语义偏差台账”
4. 若任务结束后未完成清理，不允许进入主线集成

---

## 3. 当前建议

后续实际派工时，应至少优先准备以下任务单：

1. `magi-core` 基础模型任务单
2. `magi-daemon / magi-api` 骨架任务单
3. `magi-session-store` 任务单
4. `magi-workspace` 任务单
5. `magi-tool-runtime` 任务单
6. `magi-orchestrator` 任务单
7. `magi-worker-runtime` 任务单

这些任务单准备好后，再启动多 Agent 并行推进会更稳。

---

## 4. 当前已准备的首批任务单

首批已建议实例化的任务单：

1. [10-work-packet-magi-core.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/10-work-packet-magi-core.md)
2. [11-work-packet-magi-daemon.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/11-work-packet-magi-daemon.md)
3. [12-work-packet-magi-api.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/12-work-packet-magi-api.md)
4. [13-work-packet-magi-session-store.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/13-work-packet-magi-session-store.md)
5. [14-work-packet-magi-workspace.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/14-work-packet-magi-workspace.md)

这些任务单对应当前 P0 阶段最关键的 5 个 crate，可作为后续多 Agent 并行推进的起始任务包。

---

## 5. 当前已准备的完整主线任务单

当前目录下已补齐的主线任务单包括：

### P0：硬边界与基础状态

1. [10-work-packet-magi-core.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/10-work-packet-magi-core.md)
2. [11-work-packet-magi-daemon.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/11-work-packet-magi-daemon.md)
3. [12-work-packet-magi-api.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/12-work-packet-magi-api.md)
4. [13-work-packet-magi-session-store.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/13-work-packet-magi-session-store.md)
5. [14-work-packet-magi-workspace.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/14-work-packet-magi-workspace.md)
6. [15-work-packet-magi-governance.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/15-work-packet-magi-governance.md)
7. [16-work-packet-magi-event-bus.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/16-work-packet-magi-event-bus.md)

### P1：执行主链

8. [17-work-packet-magi-tool-runtime.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/17-work-packet-magi-tool-runtime.md)
9. [18-work-packet-magi-orchestrator.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/18-work-packet-magi-orchestrator.md)
10. [19-work-packet-magi-worker-runtime.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/19-work-packet-magi-worker-runtime.md)

### P2：长期能力域与扩展边界

11. [20-work-packet-magi-knowledge-store.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/20-work-packet-magi-knowledge-store.md)
12. [21-work-packet-magi-memory-store.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/21-work-packet-magi-memory-store.md)
13. [22-work-packet-magi-context-runtime.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/22-work-packet-magi-context-runtime.md)
14. [23-work-packet-magi-skill-runtime.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/23-work-packet-magi-skill-runtime.md)
15. [24-work-packet-magi-bridge-client.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/24-work-packet-magi-bridge-client.md)

这 15 份任务单已经覆盖当前 Rust 后端影子重构的完整主线。
