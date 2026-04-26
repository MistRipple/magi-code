# 深度模式方案补充设计

本文档是 `deep-mode-delivery-product-design.md` 的补充，针对方案审视中发现的 5 个关键盲区提供具体设计。

经过三方（后端架构、前端集成、落地风险）独立审视后，本文档已整合所有审视意见。

## 0. 审视结论摘要

| 盲区 | 现状 | 影响 |
|---|---|---|
| Runner 同步调用 | `drive_shadow_task_graph` 同步跑 8 轮 `run_single_cycle` | 深度模式无法后台持续推进 |
| Task Graph 生成简陋 | LLM 输出 2-5 个标题，全部平铺 Action | 无 Phase / WorkPackage / Validation 层级 |
| Worker 虚拟角色 | 所有 role 共享同一 `invoke_llm_with_tools` 调用 | 角色差异化名存实亡 |
| TaskPolicy 死代码 | Runner 只消费 autonomy_level(Manual)、repair_limit、validation_profile、escalation_conditions | 大部分 policy 字段无实际效果 |
| 分阶段顺序问题 | Phase 1（policy 映射）和 Phase 3（Runner 持续推进）分离 | policy 写了但没人消费，形成孤岛 |

## 1. Runner 异步化：从同步 8 轮到后台持续循环

### 1.1 现状分析

关键发现：**后台 Runner 基础设施已经存在**。

`RunnerManager::start()`（`state.rs:211`）已实现：

```text
tokio::spawn → loop {
    if cancel → break
    run_cycle(root_id)
    match outcome:
        Continue   → sleep(100ms), 继续
        AllComplete → checkpoint, break
        Blocked    → sleep(500ms), 继续重试
        Error      → checkpoint, break
}
```

但 `drive_shadow_task_graph`（`task_execution.rs:867`）没有使用它，而是：

```text
for _ in 0..8 {
    manager.run_single_cycle(root_task_id)
    match outcome → break/continue/error
}
```

### 1.2 改造方案

不需要重写 Runner 基础设施，只需改变深度模式的调用方式。

#### 普通模式（保持现状）

```text
drive_shadow_task_graph
  → 同步 for 循环
  → 最多 N 轮 run_single_cycle
  → 轮完或完成后立即返回 HTTP 响应
```

适合轻量单轮任务，用户等待时间短。

#### 深度模式（改用 start）

```text
run_shadow_dispatch_submission
  → 创建 Objective + Task Graph
  → manager.start(root_task_id)        // 启动后台 Runner
  → 立即返回 HTTP 202 Accepted
  → Runner 在后台持续推进
  → 前端通过 TaskProjection 观察进度（SSE 只触发刷新）
```

#### 【审视修订】深度模式返回路径

当前 `drive_shadow_task_graph`（`task_execution.rs:907-922`）在 for 循环结束后检查 `action_task_id` 的终态。深度模式用 `start()` 后立即返回，action 还没开始执行，状态是 `Ready`，终态检查会报错。

解决方案：深度模式**不走 `drive_shadow_task_graph`**，而是在更上层的 `drive_shadow_dispatch_submission` 中分支：

```text
fn drive_shadow_dispatch_submission(state, request):
    // ... 创建任务图 ...

    let root_policy = task_store.get_task(root_task_id).policy_snapshot;
    let background = root_policy
        .as_ref()
        .map(|p| p.background_allowed)
        .unwrap_or(false);           // None 视为 false，普通模式不受影响

    if background:
        manager.start(root_task_id)  // 后台 Runner
        return Ok(ShadowGraphDriveResult { runner_started: true })
    else:
        drive_shadow_task_graph(...)  // 同步 for 循环（现有逻辑不变）
```

### 1.3 【审视修订】dispatch 阻塞调用在异步上下文中的线程饥饿

**阻塞性问题**：`ShadowTaskDispatcher::dispatch()`（`task_execution.rs:744`）内部调用 `invoke_llm_with_tools`，这是一个同步阻塞的 LLM 网络调用。当 `start()` 在 `tokio::spawn` 内运行时，阻塞调用会占据 tokio 工作线程，导致异步运行时线程饥饿。

解决方案：将 `dispatch` 的 LLM 调用包裹在 `tokio::task::spawn_blocking` 中：

```text
impl TaskDispatcher for ShadowTaskDispatcher {
    fn dispatch(&self, task, worker, lease) -> Result<(), String> {
        // 将阻塞 LLM 调用移到 blocking 线程池
        let handle = tokio::runtime::Handle::current();
        handle.spawn_blocking(move || {
            invoke_llm_with_tools(...)
        });
        Ok(())
    }
}
```

或者将底层 HTTP 客户端从 blocking 改为 async（使用 `reqwest` 的 async API）。前者改动更小，推荐第一版使用 `spawn_blocking`。

### 1.4 停止条件

后台 Runner 已有的停止条件（`AllComplete` / `Error` / `cancel`）需要扩展：

```text
AllComplete          → 全部完成，写 checkpoint，status = "completed"
Error                → 不可恢复错误，写 checkpoint，status = "error"
Cancel               → 用户手动暂停/取消
Decision             → 遇到 AwaitingApproval 的 Decision 任务
                        不应 break，而是 sleep 等待用户 resolve
RepairExhausted      → repair_count 超过 policy.repair_limit 的所有可执行任务
                        创建 Decision 后 sleep 等待
NothingDispatchable  → 当前 Blocked 但仍有非终态任务
                        区分"等待 worker 返回结果"和"真的无法推进"
                        等待结果 → sleep 继续
                        真阻塞   → 如果持续 N 轮仍阻塞 → 创建 Decision 或 Failed
```

#### 【审视修订】Blocked 状态必须有退避和上限

当前 `state.rs:275` 的 Blocked 分支是 `sleep(500ms)` 无条件重试，无上限。在深度模式下如果任务无法解除阻塞（如等待用户 Decision 但用户离线），Runner 会无限空转。

解决方案：增加指数退避和最大重试次数：

```text
Blocked 分支:
    blocked_count += 1
    if blocked_count > MAX_BLOCKED_RETRIES (默认 60，约 30 分钟):
        创建 Decision Task（"任务长时间阻塞，需要用户介入"）
        status = "waiting_decision"
        进入长间隔等待（sleep 30s）
    else:
        sleep(min(500ms * 2^(blocked_count/10), 30s))  // 逐步退避
```

### 1.5 与 Session 生命周期的关系

```text
Session 活跃
  → Runner 正常运行
  → 前端读取 TaskProjection 展示进度，刷新策略由 task-graph-store 统一调度

Session 断连（用户关闭浏览器）
  → Runner 继续运行（深度模式承诺的"后台推进"）
  → 用户重连后读取 TaskProjection 恢复视图

Session 销毁
  → Runner cancel
  → 写 checkpoint
  → 保留任务图状态
```

#### 【审视修订】Session-Runner 联动机制

当前代码中不存在任何机制在 Session 关闭时触发 `RunnerManager::stop()`。

解决方案：在 `RunnerManager` 中维护 `root_task_id → session_id` 映射，Session 层析构时主动调用 `stop()`：

```text
RunnerManager 增加:
    session_runner_index: HashMap<SessionId, Vec<String>>  // session → root_task_ids

    fn bind_session(&self, session_id, root_task_id)
    fn unbind_session(&self, session_id)  // cancel 该 session 所有 Runner
```

Session 清理钩子中调用 `runner_manager.unbind_session(session_id)`。

### 1.6 【审视修订】深度模式进度观察机制

深度模式 HTTP 202 返回后，当前 SSE 流（基于 session turn 的流式输出）已结束。前端不能依赖 SSE 推送来观察后台 Runner 进度。

观察机制收敛为一条主链路：

1. **TaskProjection API**（`/tasks/graph/{root_task_id}`）是唯一状态读取入口，TasksPanel、AgentTab 和恢复视图都只解释这份投影。
2. **SSE 事件总线**只发布 `rustTaskEvent` 作为刷新触发信号，不承载另一套任务状态模型，也不要求前端从 SSE payload 推导 Runner 状态。
3. **定时刷新**属于同一 TaskProjection 读取链路的刷新策略，用于长时间后台任务和断线重连后的状态对齐；它不是第二套观察路径。
4. 前端不得同时维护“基于 SSE payload 的任务状态”和“基于 TaskProjection 的任务状态”。如果两者信息不一致，以 TaskProjection 为准，并通过后端投影补齐缺失字段。

### 1.7 具体改动文件

| 文件 | 改动 |
|---|---|
| `crates/magi-api/src/task_execution.rs` | `drive_shadow_dispatch_submission` 增加 `background_allowed` 分支；`ShadowTaskDispatcher::dispatch` 用 `spawn_blocking` 包裹 LLM 调用 |
| `crates/magi-api/src/state.rs` | `RunnerManager::start` 增加 Blocked 退避逻辑和最大重试；增加 session-runner 映射 |
| `crates/magi-api/src/shadow_execution.rs` | `run_shadow_dispatch_submission` 根据 deep_task 决定同步/异步路径 |

## 2. Task Graph 结构化生成协议

### 2.1 当前问题

`decompose_mission`（`shadow_execution.rs:420`）只做一件事：

```text
LLM prompt: "请将以下任务分解为 2-5 个具体的子任务。每行一个子任务标题"
→ 解析为 Vec<String>
→ 全部创建为平铺 Action
```

缺失：Phase / WorkPackage / Validation 层级，依赖关系，验证节点。

### 2.2 两阶段图生成

#### 阶段 A：规则化骨架（不依赖 LLM）

对于任何深度模式任务，在拿到 LLM 分解结果后，由代码规则化补全图骨架：

```text
Objective (root)
  ├─ Phase: "规划" (kind=Phase)
  │   └─ Action: "分析需求与拆解方案" (kind=Action)
  ├─ Phase: "执行" (kind=Phase)
  │   ├─ WorkPackage 1 (kind=WorkPackage)
  │   │   └─ Action 1..N (从 LLM 分解结果填入)
  │   └─ Validation: "执行阶段验证" (kind=Validation)
  └─ Phase: "交付" (kind=Phase)
      └─ Validation: "最终验收" (kind=Validation)
```

规则：

1. 深度模式**始终**生成 3 个 Phase：规划 → 执行 → 交付
2. LLM 分解结果放入"执行"Phase 的 WorkPackage 下
3. 每个 Phase 的最后一个子节点必须是 Validation
4. Phase 之间有依赖：执行依赖规划，交付依赖执行
5. 同一 WorkPackage 内的 Action 默认可并行

代码实现位置：`shadow_execution.rs`，在 `decompose_mission` 返回后，用 `build_deep_task_graph` 函数组装。

#### 【审视修订】Phase / WorkPackage 节点初始状态规则

审视发现 `has_blocked_ancestor_inner`（`task_store.rs:421`）只检查 `Blocked` 和 `AwaitingApproval` 两种祖先状态。Phase / WorkPackage 作为结构节点的初始状态需要明确：

```text
Phase / WorkPackage 创建规则：
  - 初始状态：Ready
  - 当首个子任务进入 Running → 父节点自动转为 Running
  - 当所有子任务 terminal → propagate_parent_completion 自动完成
  - 结构节点不 dispatch 给 worker，只做聚合
```

`has_blocked_ancestor_inner` 应增加对 `Draft` 状态祖先的检查，防止子任务在父节点尚未就绪时被错误调度。

#### 【审视修订】Phase 级 Validation 的依赖关系

当前 `create_validation_child`（`task_runner.rs:875`）是在单个 Action 进入 Verifying 时动态创建子级 Validation。但设计要求的是 Phase 级 Validation——该 Phase 下所有 Action 完成后才执行验证。

解决方案：Phase 级 Validation 在骨架生成时就创建，其 `dependency_ids` 包含同 Phase 下所有 Action 任务的 ID：

```text
build_deep_task_graph:
    // 执行 Phase 内部
    let action_ids = actions.iter().map(|a| a.task_id.clone()).collect();

    let phase_validation = Task {
        kind: Validation,
        dependency_ids: action_ids,  // 依赖所有同 Phase Action
        parent_task_id: execute_phase_id,
        ...
    };
```

`get_runnable_leaves`（`task_store.rs:397`）已支持通过 `dependency_ids` 判断前置任务是否完成，无需额外修改调度逻辑。

#### 阶段 B：LLM 细化（后续迭代）

规划阶段的 Action 执行后，产出更详细的分解方案，Runner 可以：

1. 在"执行"Phase 下动态追加 WorkPackage / Action
2. 已完成的节点不可修改
3. 未开始的节点可以 replan

这部分属于后续迭代，第一版用阶段 A 的规则化骨架即可。

### 2.3 LLM 输出协议（阶段 B 使用）

当需要 LLM 生成完整图时，约定 JSON 协议：

```json
{
  "phases": [
    {
      "title": "阶段标题",
      "work_packages": [
        {
          "title": "工作包标题",
          "actions": [
            {
              "title": "具体任务",
              "goal": "任务目标描述",
              "depends_on": ["其他 action 标题"],
              "write_scope": "可选，文件/目录范围"
            }
          ]
        }
      ]
    }
  ]
}
```

校验规则：

1. `phases` 不为空且 ≤ 5
2. 每个 phase 至少有一个 work_package
3. 每个 work_package 至少有一个 action
4. `depends_on` 引用必须指向已定义的 action 标题
5. 校验失败不得静默切换到另一套图生成逻辑；规划节点进入 `Verifying` 或创建 `Decision`，要求 LLM 重新产出合法结构或用户介入修正。

阶段 A 的规则化骨架是第一版深度模式的唯一图生成实现。阶段 B 启用后，LLM JSON 结构化生成必须作为阶段 A 的演进替代路径落地：同一任务提交过程中只允许选择一种图生成器，不能先尝试 LLM 再静默改用规则化骨架。

### 2.4 具体改动文件

| 文件 | 改动 |
|---|---|
| `crates/magi-api/src/shadow_execution.rs` | 新增 `build_deep_task_graph` 函数，在 `decompose_mission` 后组装结构化图 |
| `crates/magi-core/src/task.rs` | 无需改动，TaskKind 已有 Phase / WorkPackage / Validation |
| `crates/magi-orchestrator/src/task_store.rs` | `has_blocked_ancestor_inner` 增加 Draft 状态检查 |

## 3. TaskPolicy 消费逻辑补齐

### 3.1 当前消费状态

Runner（`task_runner.rs:1139`）的 `check_policy_allows_dispatch`：

```rust
fn check_policy_allows_dispatch(&self, policy: &TaskPolicy, _task: &Task) -> bool {
    if policy.autonomy_level == "Manual" {
        return false;
    }
    true
}
```

TaskStore（`task_store.rs:172`）冻结 policy 时的默认值：

```rust
if task.policy_snapshot.is_none() {
    task.policy_snapshot = Some(TaskPolicy {
        autonomy_level: "Autonomous",
        approval_mode: "auto",
        retry_limit: 3,
        repair_limit: 3,
        validation_profile: None,
        checkpoint_mode: "auto",
        background_allowed: false,
        ...
    })
}
```

#### 【审视修订】默认冻结策略应改为保守值

审视发现默认冻结值 `autonomy_level: "Autonomous"` 与普通模式应有的 `"Assisted"` 矛盾。默认值应该是保守的（普通模式语义），深度模式通过显式传入 policy 覆盖。

修改默认冻结值为：

```text
autonomy_level: "Assisted"
approval_mode: "Interactive"
background_allowed: false
retry_limit: 1
repair_limit: 1
validation_profile: None
```

#### 【审视修订】policy 冻结绕过问题

审视发现 `make_shadow_task`（`shadow_execution.rs:517`）直接创建 `Ready` 状态 + `policy_snapshot: None`，跳过了 Draft→Ready 的 policy 冻结路径。这意味着通过 `make_shadow_task` 创建的任务永远不会获得正确的 policy。

解决方案：`make_shadow_task` 中直接填充 `policy_snapshot`，使用 `build_policy_for_mode(deep_task)` 的返回值。不改变创建状态（仍为 Ready），因为这符合当前 shadow execution 的语义。

#### 【审视修订】`task_store.rs` 的 `is_none()` 守卫已存在

三方审视确认：`update_status_checked` 中的 `if task.policy_snapshot.is_none()` 守卫**已经存在**，不需要额外改动。只要 `make_shadow_task` 正确填充 policy，默认冻结逻辑就不会覆盖它。

### 3.2 需要消费的 Policy 字段与消费位置

| 字段 | 普通模式值 | 深度模式值 | 消费位置 | 消费行为 |
|---|---|---|---|---|
| `autonomy_level` | `"Assisted"` | `"Autonomous"` | Runner `check_policy_allows_dispatch` | Assisted: Action 完成后暂停等确认；Autonomous: 自动继续 |
| `approval_mode` | `"Interactive"` | `"DecisionOnly"` | Runner `apply_results` Completed 分支 | Interactive: 每步完成后生成确认请求；DecisionOnly: 只在 escalation 时暂停 |
| `validation_profile` | `None` 或 `"Basic"` | `"Required"` | Runner `propagate_parent_completion` | Required: Action 完成后必须创建 Validation 子任务 |
| `checkpoint_mode` | `"Turn"` | `"TaskOrPhase"` | Runner cycle 末尾 | TaskOrPhase: 每个 Task 或 Phase 完成时写 checkpoint |
| `background_allowed` | `false` | `true` | `drive_shadow_dispatch_submission` 入口 | true → 启用后台 Runner；false → 同步 N 轮 |
| `retry_limit` | `1` | `2-3` | Runner `apply_results` | 已有实现，但需确保值来自 policy 而非默认 |
| `repair_limit` | `1` | `2-3` | Runner repair 创建逻辑 | 已有实现 |
| `escalation_conditions` | `["on_failure"]` | `["on_failure", "high_risk", "on_repair_exhausted"]` | Runner `evaluate_escalation` | 已有实现 |
| `allowed_tools` / `denied_tools` | 无限制 | 按角色配置 | Dispatcher 构建 worker prompt | 传入 worker 的可用工具列表 |
| `allowed_paths` / `denied_paths` | 无限制 | 按 workspace 配置 | Dispatcher 构建 worker prompt | 传入 worker 的可操作路径范围 |

### 3.3 `check_policy_allows_dispatch` 扩展设计

```text
fn check_policy_allows_dispatch(policy, task) -> PolicyDispatchDecision:
    if autonomy_level == "Manual":
        return Reject("需要手动触发")

    if approval_mode == "Interactive" && task.kind == Action:
        if task 的同级前序 Action 刚完成:
            return NeedsApproval("交互模式：等待用户确认继续")

    return Allow
```

返回值从 `bool` 改为枚举：

```text
enum PolicyDispatchDecision {
    Allow,
    Reject(String),
    NeedsApproval(String),   // → 创建 Decision Task
}
```

#### 【审视修订】NeedsApproval 的 Decision Payload 构造

审视指出 `NeedsApproval` 触发 `escalate_to_decision` 时需要 `DecisionTaskPayload`（含 options、risk_notes 等），但方案未定义 payload 来源。

构造规则：

```text
NeedsApproval → escalate_to_decision(parent_task_id, DecisionTaskPayload {
    decision_context: "交互模式下等待用户确认",
    blocked_reason: format!("任务 {} 等待继续执行确认", task.title),
    risk_notes: None,
    options: vec![
        DecisionOption { key: "continue", label: "继续执行" },
        DecisionOption { key: "skip", label: "跳过此任务" },
        DecisionOption { key: "cancel", label: "取消整个任务" },
    ],
})
```

### 3.4 Policy 来源：从 deepTask 到 TaskPolicy

在 `shadow_execution.rs` 的 `make_shadow_task` 中，根据 `deep_task` 参数构建 policy：

```text
fn build_policy_for_mode(deep_task: bool) -> TaskPolicy:
    if deep_task:
        TaskPolicy {
            autonomy_level: "Autonomous",
            approval_mode: "DecisionOnly",
            validation_profile: Some("Required"),
            checkpoint_mode: "TaskOrPhase",
            background_allowed: true,
            retry_limit: 2,
            repair_limit: 2,
            escalation_conditions: vec![
                "on_failure",
                "high_risk",
                "on_repair_exhausted",
            ],
            ..
        }
    else:
        TaskPolicy {
            autonomy_level: "Assisted",
            approval_mode: "Interactive",
            validation_profile: None,
            checkpoint_mode: "Turn",
            background_allowed: false,
            retry_limit: 1,
            repair_limit: 1,
            escalation_conditions: vec!["on_failure"],
            ..
        }
```

#### 【审视修订】`policy_snapshot: None` 的 `background_allowed` 默认行为

审视指出普通模式任务的 `policy_snapshot` 当前为 `None`。在 `drive_shadow_dispatch_submission` 检查 `background_allowed` 时，`None` 必须视为 `false`：

```text
let background = root_policy
    .as_ref()
    .map(|p| p.background_allowed)
    .unwrap_or(false);   // None → false，普通模式不受影响
```

### 3.5 【审视修订】Evidence 完成约束

审视指出 `apply_results` 的 Completed 分支直接转为 Completed，不检查 evidence_refs 是否非空。

深度模式下增加约束：

```text
apply_results Completed 分支:
    if policy.validation_profile == Some("Required"):
        if task.evidence_refs.is_empty() && task.output_refs.is_empty():
            // 不允许直接完成，转为 NeedsVerification
            update_status(task_id, Verifying)
            return
    update_status(task_id, Completed)
```

### 3.6 具体改动文件

| 文件 | 改动 |
|---|---|
| `crates/magi-api/src/shadow_execution.rs` | `make_shadow_task` 填充 policy；新增 `build_policy_for_mode` |
| `crates/magi-orchestrator/src/task_runner.rs` | `check_policy_allows_dispatch` 返回枚举 + NeedsApproval payload 构造；`apply_results` 增加 evidence 检查；`approval_mode` 消费 |
| `crates/magi-orchestrator/src/task_store.rs` | 默认冻结策略改为保守值 |
| `crates/magi-core/src/task.rs` | 无需改动 |

## 4. Worker 差异化执行策略

### 4.1 当前问题

`ShadowTaskDispatcher::dispatch`（`task_execution.rs`）所有 role 调用同一个 `invoke_llm_with_tools`：

```text
dispatch(task, worker, lease):
    invoke_llm_with_tools(
        system_prompt: 通用 prompt,
        tools: 全部工具,
        model: 统一模型
    )
```

Worker 的 role 字段只用于 Runner 匹配，不影响执行行为。

### 4.2 差异化维度

不同 role 的差异应该体现在三个层面：

| 维度 | architect | integration-dev | reviewer | debugger |
|---|---|---|---|---|
| System Prompt | 强调架构决策、全局视角 | 强调代码实现、质量 | 强调审查、验证标准 | 强调问题诊断、修复 |
| 工具集 | 文件读取、搜索 | 文件读写、命令执行 | 文件读取、diff | 文件读写、日志、调试 |
| 模型偏好 | 高推理能力模型 | 标准模型 | 标准模型 | 标准模型 |
| 温度 | 较高（探索性） | 较低（确定性） | 较低 | 较低 |

### 4.3 执行配置结构

在 `WorkerInfo` 或新增 `WorkerExecutionConfig` 中承载差异：

```text
struct WorkerExecutionConfig {
    system_prompt_template: String,
    tool_filter: ToolFilter,         // Allow / Deny 列表
    model_preference: Option<String>,
    temperature: Option<f32>,
}

enum ToolFilter {
    AllowAll,
    AllowOnly(Vec<String>),
    DenyOnly(Vec<String>),
}
```

### 4.4 配置来源

配置不应硬编码，而是通过 `WorkerCatalog` 注册时携带：

```text
WorkerCatalog::register_worker(WorkerInfo {
    worker_id: "architect-1",
    role: "architect",
    supported_kinds: [Objective, Phase],
    parallelism_limit: Some(1),
    execution_config: WorkerExecutionConfig {
        system_prompt_template: ARCHITECT_PROMPT,
        tool_filter: ToolFilter::AllowOnly(vec!["read_file", "search", "list_dir"]),
        model_preference: None,
        temperature: Some(0.7),
    },
})
```

### 4.5 Dispatcher 消费配置

`ShadowTaskDispatcher::dispatch` 改造：

```text
dispatch(task, worker, lease):
    config = worker.execution_config   // 或从 catalog 查询

    // 合并 task policy 的工具限制
    tools = apply_tool_filter(
        all_tools,
        config.tool_filter,
        task.policy_snapshot.allowed_tools,
        task.policy_snapshot.denied_tools,
    )

    // 构建角色化 system prompt
    system_prompt = render_prompt(
        config.system_prompt_template,
        task.goal,
        task.context_refs,
        task.input_refs,
    )

    invoke_llm_with_tools(
        system_prompt,
        tools,
        model: config.model_preference.unwrap_or(default_model),
        temperature: config.temperature.unwrap_or(default_temp),
    )
```

### 4.6 实施优先级

第一版（Phase 2）：

- 只做 system prompt 差异化——不同 role 使用不同的 system prompt 模板
- 不做工具过滤和模型差异（这些可以后续叠加）
- 原因：prompt 差异是投入产出比最高的改动，一个好的 reviewer prompt 就能让 Validation 任务产出有意义的审查

后续版本：

- 工具过滤
- 模型差异（根据成本/效果评估决定）

### 4.7 具体改动文件

| 文件 | 改动 |
|---|---|
| `crates/magi-orchestrator/src/task_runner.rs` | `WorkerInfo` 增加 `execution_config` 字段（或平铺 `system_prompt_template`） |
| `crates/magi-orchestrator/src/task_worker_catalog.rs` | 默认 worker 注册时携带 prompt 模板 |
| `crates/magi-api/src/task_execution.rs` | `ShadowTaskDispatcher::dispatch` 消费 worker config |

## 5. 【审视修订】前端集成补充

审视发现前后端集成存在 3 个缺陷和 2 个风险，补充设计如下。

### 5.1 TaskProjectionDto 字段扩展

当前 `TaskProjectionDto` 缺少深度模式必需的字段：

| 缺失字段 | 类型 | 用途 | 消费方 |
|---|---|---|---|
| `execution_mode` | `"normal" \| "deep"` | 区分普通/深度 | TasksPanel 概览标题 |
| `runner_status` | `"idle" \| "running" \| "paused" \| "completed" \| "error"` | Runner 运行状态 | TasksPanel 动态指示 |

后端 `build_projection` 从 `RunnerManager` 查询当前 Runner 状态填入投影。

改动文件：

| 文件 | 改动 |
|---|---|
| `crates/magi-core/src/task.rs` | `TaskProjection` 增加 `execution_mode` 和 `runner_status` 字段 |
| `crates/magi-orchestrator/src/task_store.rs` | `build_projection` 填充新字段（需要 RunnerManager 引用或由上层传入） |
| `web/src/shared/rust-backend-types.ts` | `TaskProjectionDto` 增加对应字段 |
| `web/src/components/TasksPanel.svelte` | 概览区显示模式标识和 Runner 状态 |

### 5.2 Decision 主动通知

深度模式后台 Runner 创建 Decision Task 后，用户如果不在 TasksPanel 页面无法感知。

解决方案：Decision Task 创建时，同时通过 notification 通道推送：

```text
escalate_to_decision 成功后:
    event_bus.publish(Event::DecisionCreated {
        session_id,
        task_id: decision_id,
        context: payload.decision_context,
    })
```

前端 notification 系统已存在（`SessionNotificationItemDto`），接收 Decision 通知后展示 toast 或 badge。

### 5.3 用户中途输入的 Intake 适配

当前 `InputArea.svelte` 的 `sendMessage` 和 `agent-api.ts` 的 `submitSessionTurn` 都只携带 `deepTask: boolean`，无法区分用户输入意图（新任务 / 补充上下文 / 回答 Decision / 暂停）。

解决方案（Phase 5 Intake 阶段实施）：

1. `SessionTurnRequestDto` 增加可选的 `context_task_id` 字段，标明用户输入时的上下文任务
2. 后端 Intake 分类器根据 `context_task_id` + 当前 Runner 状态 + 消息内容自动判断意图
3. 前端 InputArea 在深度模式运行中调整 placeholder 提示

## 6. 修订后的分阶段实施计划

### 原方案问题

原方案 Phase 1（policy 映射）和 Phase 3（Runner 持续推进）分离，导致：

- Phase 1 做完后 policy 写了但没人消费
- Phase 2（Task Graph）做完后图生成了但 Runner 不知道怎么推进
- Phase 3 才开始让 Runner 消费 policy，此时前两个阶段的产出才真正生效

### 【审视修订】Phase 0 拆为 0a + 0b

审视建议 Phase 0 范围过大，拆为两个子步骤降低风险。

#### Phase 0a：最小闭环（~40 行改动，2 个文件）

目标：

> 深度模式能后台推进，普通模式不受影响。

范围：

1. `shadow_execution.rs`：`build_policy_for_mode(deep_task)` 生成差异化 policy
2. `shadow_execution.rs`：`make_shadow_task` 填充 `policy_snapshot`
3. `task_execution.rs`：`drive_shadow_dispatch_submission` 根据 `background_allowed` 分支——deep → `manager.start()`，non-deep → 现有同步循环

验收（可自动化）：

```text
测试 1：普通模式提交 → run_single_cycle 被调用 → 同步返回（现有测试不红）
测试 2：深度模式提交 → RunnerManager::start() 被调用
         → RunnerHandle.status == "running"
         → 最终 "completed" 或 "error"
测试 3：policy_snapshot 为 None 时 → 走同步路径（不 break 普通模式）
```

#### Phase 0b：策略消费细化

目标：

> Runner 根据 TaskPolicy 区分行为——approval_mode、checkpoint_mode、evidence 约束。

范围：

1. `task_runner.rs`：`check_policy_allows_dispatch` 返回枚举，消费 `approval_mode`
2. `task_runner.rs`：`apply_results` 增加 evidence 检查（validation_profile == Required）
3. `state.rs`：Runner 后台循环增加 Blocked 退避和上限
4. `task_store.rs`：默认冻结策略改为保守值

验收（可自动化）：

```text
测试 1：approval_mode == "Interactive" + Action → PolicyDispatchDecision::NeedsApproval
测试 2：approval_mode == "DecisionOnly" + Action → PolicyDispatchDecision::Allow
测试 3：validation_profile == "Required" + evidence_refs 为空 → 状态转为 Verifying
测试 4：Blocked 超过 MAX_BLOCKED_RETRIES → 创建 Decision Task
```

#### Phase 1A：规则化 Task Graph 骨架

目标：

> 深度模式生成 Objective / Phase / WorkPackage / Action / Validation 层级图。

范围：

1. `shadow_execution.rs`：新增 `build_deep_task_graph`
2. 固定 3 Phase 骨架：规划 → 执行 → 交付
3. LLM 分解结果放入"执行"Phase
4. 每个 Phase 末尾自动添加 Validation（带 dependency_ids）
5. Phase/WorkPackage 初始状态为 Ready

验收（可自动化）：

```text
测试 1：深度模式生成的任务图包含 ≥ 3 个 Phase 节点
测试 2：每个 Phase 最后一个子节点 kind == Validation
测试 3：执行 Phase 的 Validation 的 dependency_ids 包含所有同级 Action
测试 4：Phase 之间有依赖关系（执行依赖规划）
```

#### Phase 1B：LLM 结构化图生成（后续迭代）

目标：

> 规划阶段 Action 完成后，根据产出动态细化执行图。

范围：

1. LLM JSON 协议定义
2. 校验规则
3. Runner 支持动态追加 WorkPackage / Action
4. replan 未完成子树

验收：

```text
规划阶段完成后，执行阶段自动细化为更详细的子任务。
```

#### Phase 2：Worker 差异化

目标：

> 不同 role 的 worker 使用不同的 system prompt，产出差异化。

范围：

1. `WorkerInfo` 增加 `system_prompt_template`
2. 默认 worker catalog 注册时携带角色 prompt
3. `ShadowTaskDispatcher::dispatch` 消费 prompt 模板

验收（可自动化）：

```text
测试 1：Validation 任务 dispatch 时 system prompt 包含 reviewer 角色指令
测试 2：Repair 任务 dispatch 时 system prompt 包含 debugger 角色指令
测试 3：Action 任务 dispatch 时 system prompt 包含 integration-dev 角色指令
```

#### Phase 3：TasksPanel 交付总览 + TaskProjection 扩展

依赖 Phase 0a + 1A 完成。

增加范围（来自审视）：

1. `TaskProjectionDto` 增加 `execution_mode` 和 `runner_status`
2. TasksPanel 概览区显示模式标识和 Runner 状态
3. TasksPanel 增加暂停/继续操作入口

#### Phase 4：AgentTab 任务绑定视图

不变，依赖 Phase 0a 完成。

#### Phase 5：深度模式 Intake

不变。增加 `SessionTurnRequestDto.context_task_id` 字段。

#### Phase 6：交付包

不变。

### 修订后的依赖关系

```text
Phase 0a (最小闭环：policy填充 + 异步分支)
  ↓
Phase 0b (策略消费细化)    Phase 1A (规则化Task Graph)    Phase 2 (Worker差异化)
  ↓                            ↓
Phase 3 (TasksPanel + Projection扩展)
  ↓
Phase 1B (LLM图生成)       Phase 4 (AgentTab)
                              ↓
                           Phase 5 (Intake)
                              ↓
                           Phase 6 (交付包)
```

Phase 0a 是唯一前置条件。Phase 0b、Phase 1A、Phase 2 可以并行。

## 7. 【审视修订】并发安全补充

### 7.1 状态变更回调持锁执行风险

`update_status_checked`（`task_store.rs`）中的 `on_status_change` 回调在持有写锁的情况下执行。如果回调中触发了需要读取 TaskStore 的操作，会导致死锁。

建议：将回调执行移到写锁释放之后。在持锁期间收集需要触发的回调参数，释放锁后再批量执行。此改动不阻塞 Phase 0，但应在多 Runner 并发场景前修复。

### 7.2 全局 Lease 管理跨 Runner 干扰

`collect_expired_leases` 和 `heartbeat_lease` 作用于 TaskStore 中的所有 lease，不区分 root_task_id。多 Runner 场景下可能互相干扰。

建议：在 `AssignmentLease` 中增加 `root_task_id` 字段，lease 操作按 root_task_id 隔离。此改动在多用户场景前修复。

## 8. 与主方案的关系

本补充设计不修改主方案（`deep-mode-delivery-product-design.md`）中的：

- 产品概念定义（普通/深度两策略）
- 底层模型（Mission/Task/Worker/Policy/Evidence）
- UI 接线方案
- Evidence 规则
- 交付包定义
- 禁止事项

本补充设计修订了主方案中的：

- 分阶段顺序（Phase 1+3 合并为 Phase 0，再拆为 0a/0b；Phase 2 拆为 1A/1B）
- 新增 Runner 异步化的具体架构（阻塞调用处理、返回路径、停止条件、Session 联动）
- 新增 Task Graph 两阶段生成策略 + 节点初始状态规则 + Validation 依赖关系
- 新增 Worker 差异化的具体维度和优先级
- 新增 TaskPolicy 每个字段的消费位置和行为定义 + NeedsApproval payload
- 新增前端集成补充（TaskProjection 扩展、Decision 通知、Intake 适配）
- 新增并发安全补充（回调持锁、Lease 隔离）

## 9. 三方审视结论汇总

### 阻塞性问题（P0，Phase 0a 前必须解决）

| # | 问题 | 解决方案 |
|---|---|---|
| 1 | `dispatch()` 阻塞 LLM 调用在 `tokio::spawn` 中导致线程饥饿 | 用 `spawn_blocking` 包裹（§1.3） |
| 2 | `make_shadow_task` 绕过 policy 冻结路径 | 直接填充 `policy_snapshot`（§3.1） |
| 3 | 深度模式返回路径未定义（action 终态检查报错） | 在更上层分支，跳过 `drive_shadow_task_graph`（§1.2） |
| 4 | Blocked 状态无限重试无上限 | 指数退避 + 最大重试 + 自动创建 Decision（§1.4） |

### 高优先级（P1，Phase 0b 期间解决）

| # | 问题 | 解决方案 |
|---|---|---|
| 5 | Session-Runner 无联动机制 | session_runner_index 映射 + unbind_session（§1.5） |
| 6 | `approval_mode` 无消费逻辑 | `apply_results` Completed 分支消费（§3.2） |
| 7 | TaskProjectionDto 缺模式/Runner 状态字段 | 增加 execution_mode + runner_status（§5.1） |
| 8 | Decision 无主动通知 | event_bus 推送 + notification 通道（§5.2） |
| 9 | Phase/WorkPackage 初始状态未定义 | Ready + 子任务驱动父节点状态（§2.2） |
| 10 | 默认冻结策略与普通模式矛盾 | 改为保守值（§3.1） |
