# 任务完成报告丢失与卡片显示问题修复规范

## 1. 问题表象
重启插件后，原本显示在 Worker 卡片中的任务完成报告消失了。
同时用户提出需求：希望现有的几个任务卡片能够统一为一个，避免重复实现。如果可以不用 WaitResultCard 就不用，尽量用现有组件展示，且已经做完的配置处理不用回滚。

## 2. 机理溯源
任务状态目前存在多处展示：
1. **主线任务卡片（SubTaskSummaryCard）**：包含任务描述、状态徽章、耗时以及可能的 `WaitResultCard` 嵌套。
2. **WaitResultCard**：专门用来展示 `wait_for_workers` 工具调用产生的结果。
3. **顶部的悬浮视图**：利用 `worker-panel-state.ts` 推导的运行时状态展示当前 Worker 状态。

**状态丢失原因**：
- `workerWaitResults` 是存储在内存中的，不随消息一起持久化。
- 在页面重启后，`message-handler.ts` 的 `rebuildWorkerWaitResultsFromMessages` 方法虽然试图从历史消息恢复状态，但如果是 instruction 或者 task_card 没有持久化某些状态位，导致 `workerWaitResult` 没有恢复，最终 fallback 到全局的 `workerRuntime`，由于重启时全局 runtime 被重置为 idle 或根据未完结任务推导，从而覆盖了已经 completed 的旧卡片状态。
- `MessageItem.svelte` 中的状态优先级是：`waitResultStatusOverride > subTaskStatusOverride > metadataCardStatus > runtimeStatusOverride`。如果 waitResult 被刷掉且 subTask 被覆盖，则导致最终状态改变。

## 3. 差距诊断
- 组件重复：`WaitResultCard` 与 `SubTaskSummaryCard` 都有结果列表和状态展示逻辑，存在重复实现。
- 持久化断层：任务完成结果没有强绑定到可以持久化、能独立恢复的消息节点上。
- 逻辑分散：`MessageItem.svelte` 在外层组装卡片所需的数据，并将 `waitResult` 传入卡片，卡片又包含自身的 `WaitResultCard`。

## 4. 根本原因分析
没有把 `wait_for_workers` 产生的结果集直接打入到卡片的 `metadata.subTaskCard` 结构中保存，而是依赖额外的 store（`workerWaitResults`）。重启后因为重建逻辑瑕疵（或者因为对应的工具调用在流式中被清理或未保存完整）导致状态不能完全恢复。

## 5. 彻底修复与清理
### 5.1 统一实现
- 移除 `WaitResultCard.svelte` 或者是它的独立逻辑，将展示结果的能力合并到 `SubTaskSummaryCard.svelte` 中。让 `SubTaskSummaryCard` 成为唯一且全能的任务卡片组件。

### 5.2 状态固化
- 将完成报告结果固化到 `subTaskCard` 数据结构中（写入 `message.metadata.subTaskCard.results`），使得重启后可以直接从消息元数据恢复展示。

### 5.3 优先级保障
- 简化 `MessageItem.svelte` 中的推导逻辑，强制使用 metadata 中固化的结果，移除脆弱的 `workerWaitResult` 依赖。