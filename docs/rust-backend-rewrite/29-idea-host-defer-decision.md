# IDEA Host 延后决策

更新时间：2026-04-17

> 本文档用于把 `IDEA host` 在本轮 Rust 后端重构中的决策固定下来。

---

## 1. 决策

当前决策是：

- `IDEA host` 不进入本轮 `M6` 前置实现
- 明确延后到切换后阶段单独推进

---

## 2. 当前固定边界

在进入未来独立任务包前，`IDEA host` 必须持续满足：

- `implementation_source = boundary-placeholder`
- `service_health = unavailable`
- `runtime_mode = boundary-only`
- 所有 `host.call` 显式拒绝

这条边界的目的不是“放弃 IDEA”，而是避免 placeholder 继续伪装成与 `VSCode real-prehost` 对等可用。

---

## 3. 为什么现在延后

1. 本轮切换前最硬的阻塞已经收敛到 `knowledge / memory / provider / TS 接线`，继续强推 `IDEA host` 会分散关键路径。
2. `VSCode real-prehost` 已经足够覆盖本轮 Host Bridge 协议冻结与最小真实宿主前置验证。
3. 真实 `IDEA` 宿主实现需要单独的 SDK、生命周期与测试策略，不适合在当前收口阶段夹带推进。

---

## 4. 重新进入排期的条件

只有在满足以下条件后，才重新为 `IDEA host` 建独立任务包：

1. `T-301 / T-302` 已关闭
2. 真实 provider / MCP / TS 接线已经进入验证阶段
3. 本轮 `M6` 评估结果已经明确

---

## 5. 对当前切换判断的影响

这项决策的含义是：

- 本轮 `M6` 评估不再把“实现真实 IDEA host”作为准入条件
- 但 Host Bridge 当前仍不能被误判为“全部完成”
- `IDEA host` 仍然是后续平台化阶段的独立任务，而不是被删除的范围
