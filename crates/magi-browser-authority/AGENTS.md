# Browser Authority 规则

本文件补充仓库根目录 [AGENTS.md](../../AGENTS.md)。职责和故障语义以 [Browser Runtime 设计](../../docs/browser-runtime-design.md) 为准；Rust 后端总览见 [工程约束与运行入口](../../docs/README.md)。

## 权威状态

- Browser Authority 位于 `magi-daemon` 业务内核，拥有逻辑 Browser Tab 身份、URL/标题、Session 归属、Surface binding、Primary、租约、控制权和导航代次等逻辑事实。
- Electron Main 拥有物理 guest `WebContents`、partition、CDP 和页面生命周期。Authority 通过既有 Browser Host/Desktop Control 协议协调物理 Surface，不直接操作 Electron 对象，也不把物理运行状态当作持久化逻辑状态。
- Web、Renderer、Automation Worker 和查询投影不得成为 Browser 实体的第二事实源。视图恢复元数据遵循设计中的 `sessionStorage`/`localStorage` 限制。
- 新增或修改状态时明确唯一 owner、提交点、持久化范围和重启恢复行为。不要以缓存、事件投影或前端乐观状态代替权威写入成功。

## 身份、租约与失效

- 沿用现有 `desktopEpoch`、`workerEpoch`、`surfaceRevision`、`navigationRevision` 和 lease fence 的职责：每个字段只负责其定义的生命周期；不得省略身份检查或把旧结果重定向到新 Surface。
- Primary、导航、取消、用户接管和任务终态推进按设计更新相应失效边界。旧读结果可以按调用方策略显式重试；输入、提交等写操作不可在新 Surface 上自动重放。
- 资源释放只影响精确匹配的 binding、epoch 和 revision。迟到 close/unregister 事件不得清除新 Primary，也不得复活已关闭 Tab。
- 变更 API、DTO、事件或持久化字段前检查相关 schema、所有调用方、重连/replay 路径和兼容策略；协议约束见 [`contracts/AGENTS.md`](../../contracts/AGENTS.md)。

## 验证

- 基础检查：`cargo check -p magi-browser-authority`。
- 状态机、租约、重连、Primary 切换或陈旧事件行为变化时，运行 `cargo test -p magi-browser-authority` 和改动覆盖的跨层 Browser 验证。
- 按 [Browser 设计](../../docs/browser-runtime-design.md) 检查故障恢复矩阵；仅验证 Authority 单 crate 不能证明 Desktop guest/Worker 链路完整。
