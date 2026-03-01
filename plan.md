## 表象分析 (Symptom Analysis)

用户询问除了 `update_todo` 工具外，其他的 todo /编排相关工具是否也需要进行类似的优化处理，也就是是否面临类似“单体操作低效”的问题。需要确保 LLM 的调用效率和 Token 利用率最大化，减少不必要的多次单独 Tool Calls。

## 机理溯源 (Context & Flow)

我们查阅了整个 `src/tools/orchestration-executor.ts`，发现目前系统暴露了以下编排相关工具：
1. `dispatch_task`
2. `wait_for_workers`
3. `send_worker_message`
4. `split_todo`
5. `get_todos`
6. `update_todo`

这些工具构成了 LLM 分配和追踪任务的核心 API。其中 `wait_for_workers`、`split_todo` 和 `update_todo` 都是接受数组的接口。

## 差距诊断 (Gap Diagnosis)

- **`split_todo`**：
  - 目前的设计：已经要求 `subtasks: array` 并且至少 2 个元素。一次拆分可以将所有子步骤通过一个 Tool Call 传递。没有每次只能添加一个子任务的低效情况。
- **`update_todo`**：
  - 目前的设计：接收 `updates: array`，支持批量更新状态。
- **`get_todos`**：
  - 这是一个检索和查询工具，并不涉及重复的单体写操作。
- **`wait_for_workers`**：
  - 目前的设计：接受 `task_ids: string[]`，支持并发阻塞等待。
- **`dispatch_task`**：
  - **当前问题点**：这个工具的 `input_schema` 被设计为**派发单个任务**。即便大模型能通过 Parallel Tool Calls 同步下发多次，但这仍会在每次请求中重复提交大量的 `input_schema` key 名称（如 `task_name`, `goal`, `acceptance`, `constraints` 等），浪费 Token 并且可能触发 LLM 工具调用数量限制的瓶颈。这与之前单体处理的情况有些相似。

## 根本原因分析 (Root Cause Analysis)

当初设计工具时，可能从对象实体的抽象出发，认为 `dispatch_task` 就对应一次单一动作。但在多 Worker 协同时，如果一个任务被分解为 3-5 个必须并行的独立子任务，编排器会自然地想要“批量下发”。如果是串行的，那么可以通过 `split_todo` 将任务切分给当前 Worker，而如果要给不同 Worker 派发，就必须多次调用 `dispatch_task`。

## 彻底修复与债清偿 (Fundamental Fix & Cleanup)

目前的 `split_todo`、`update_todo`、`wait_for_workers` 等都已经达到了批量处理的工程要求。唯独 **`dispatch_task` 可以考虑进行批量化重构（改为 `dispatch_tasks` 或增加 `tasks: array` 支持）**。然而在此次任务范围内，鉴于我只处于规划和代码审查阶段且没有编辑权限，我将向用户报告这一诊断结果。
