任务结束的判定机制：
1. **OrchestratorLLMAdapter** 中的循环结束条件是**大模型不再调用工具**。在 `streamAnthropicMessage` 返回的 `toolCalls` 为空时，循环 `break`，标记 `terminationReason = 'completed'`。
2. 即使有大模型自主判断，系统还设置了兜底的**轮次上限**（常规模式 50 轮，深度模式 150 轮）。当 `loopRounds > MAX_ORCHESTRATOR_ROUNDS` 时也会终止。
3. 如果工具连续出错 5 次会提示大模型停止探索，累计 25 次报错则强制中断。

关于 `dispatch_task` 和 `wait_for_workers`：
- `wait_for_workers` 是用来阻塞等待一个批次的 `dispatch_task` 完成。
- 在常规模式下，批次执行完会自动触发 `Phase C` 汇总；如果显式调用了 `wait_for_workers`（反应式编排），则由大模型拿到所有 `task` 的执行结果自己决定下一步，它会拿到诸如 `success`, `summary`, `modifiedFiles`，甚至如果代码被破坏还会带有 `audit` 审计阻断结果。
- Orchestrator 拿到结果后，只有当它认为需求已完全满足时，才不再输出任何 `toolCall`，从而真正退出主循环，标志着整个任务完成。

关于 Plan Ledger 进度追踪：
- 这是从外围（前端或状态流转上）维护计划树的，主要通过 `MissionOrchestrator` 将底层事件（`todoCompleted`, `assignmentCompleted` 等）转发。
- 但是这**并不改变 LLM 驱动引擎的本质**：编排者依然是靠 prompt 和返回数据去判断任务进度，它只是通过 LLM 的决策不再调度新任务来结束引擎运行。
