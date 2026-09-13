# Magi 子代理分配稳定性产品级修复方案

- 文档状态：已完成；阶段 0-6 已完成（最后验证：2026-09-14）
- 适用范围：Magi 主对话中的 `agent_spawn`、`agent_send`、`agent_wait` 以及子代理 Worker 执行链
- 目标读者：负责 Rust daemon、conversation runtime、Web 前端和桌面验收的实现 Agent
- 约束：本方案只收敛到一条正式实现路径，不保留旧关键词开关、旧参数合同或临时兜底实现
- 编写依据：当前仓库代码、现有任务投影和 `cn-engineering-standard`

## 1. 目标与非目标

### 1.1 产品目标

用户在主对话中提出复杂、可拆分或需要独立复核的工作时，Magi 的 root coordinator 应能稳定判断是否使用子代理，并在需要时完成以下闭环：

```text
识别任务 → 判断是否协作 → 选择已注册角色 → 完成派发前预检 → 创建子任务
→ Worker 执行 → 主线等待/收集 → 失败时改派或主线接管 → 如实展示结果
```

用户看到的“已派发”必须代表子任务已经通过所有创建前条件；用户看到的“排队中”必须代表任务仍然存在并将在条件满足后执行；用户看到的“失败”必须包含可操作的失败阶段和原因。

系统内置角色和用户自定义角色必须共用同一注册表、同一 Worker 目录、同一模型绑定规则和同一错误合同。不能为自定义角色增加另一条派发路径。

### 1.2 非目标

本次修复不改变以下产品边界：

- 不允许 Worker 递归创建子代理；只有 root coordinator 可以使用编排工具。
- 不允许通过 `agent_wait` 等待非当前任务直接派发的子任务。
- 不绕过访问策略、SafetyGate、Git 隔离和模型配置检查。
- 不把所有请求强制升级成多代理任务；简单任务仍可由主线直接完成。
- 不新增第二套任务存储、第二套 Worker 注册表或第二套角色格式。

## 2. 当前问题与根因

### 2.1 根因一：入口把“能否协作”错误绑定到用户关键词

`sessions.rs` 的 `session_turn_denied_tools` 会在文本未命中“代理、多代理、subagent”等关键词时，加入：

```text
agent_spawn
agent_send
agent_wait
```

这会把“主线可以自主判断是否协作”变成“用户必须知道内部工具名称才能协作”。例如“完整分析当前项目并独立检查风险”可能被路由到普通 Execute，root coordinator 根本没有 `agent_spawn`。

正式规则应改为：

- 任务路由只决定当前 Turn 是否需要结构化执行记录。
- 协作能力由 root coordinator 的策略字段决定，不由文本关键词直接裁剪工具目录。
- 用户明确说“不要创建/不要派发/单线完成”时，才把协作模式设为 `disabled`。
- 用户明确要求代理协作时，模式设为 `required`，并要求至少产生一次真实 `agent_spawn`。
- 其余可执行任务模式为 `auto`，由 coordinator 根据任务复杂度决定是否派发。

### 2.2 根因二：工具 Schema 与角色能力校验互相矛盾

`builtin_tool_schema.rs` 当前把全局能力 ID 写入 `agent_spawn.capabilities.items.enum`，而 `tool_batch.rs` 又按目标角色执行严格校验。模型因此可以合法地产生一个 Schema 接受、运行时拒绝的组合。

已观察到的实际失败：`desktop-acceptance` 被传入 `desktop` 和 `quality_engineering`，运行时返回 `unsupported_capability`，因为该角色只拥有 `desktop / general_engineering`。

正式规则：

- 角色是能力的唯一事实源。
- `capabilities` 不再要求模型手工猜测角色能力。
- 工具参数中保留可选 `capabilities`，省略时由服务端根据角色配置生成默认激活集合。
- 如果模型显式传入能力，服务端只接受角色允许的能力；不允许把非法能力静默删除。
- 工具 Schema 不再暴露全局能力 enum，避免给模型错误暗示。
- `tool_catalog` 必须为每个角色返回 `capability_ids`，coordinator Prompt 必须明确要求从目标角色的能力集合中选择。

### 2.3 根因三：创建后才检查模型和 Git 前置条件

当前流程先在 `register_spawned_local_agent_child` 中插入 TaskStore、SpawnGraph、执行计划和 Thread，Worker 真正启动后才解析角色模型和 Git worktree。于是可能出现：

```text
agent_spawn 返回 started → 子代理卡片出现 → 模型配置不可用或 worktree 创建失败 → 子代理失败
```

这不是异步本身的问题，而是“创建成功”的语义过早。

正式规则：

- 派发前预检必须在创建子任务前完成。
- 预检失败时不得写入 TaskStore、SpawnGraph、执行注册表或 Thread。
- 预检成功后才生成 child_task_id 并进入原子注册。
- 创建后的执行失败仍然允许发生，但必须归类为运行期失败，并通过 `agent_wait` 返回。

### 2.4 根因四：工具执行状态与编排状态混用

当前非法角色可能返回 `status: degraded`，但外层 `ExecutionResultStatus` 是成功；模型可能认为工具调用完成，实际没有子任务。容量拒绝、参数拒绝、运行失败和降级接管没有统一状态机。

正式规则：

- 外层工具执行状态只表示工具协议是否正常处理。
- 内层编排状态表示子任务生命周期。
- `agent_spawn` 创建前拒绝必须是工具失败或明确拒绝，绝不能伪装成 started/succeeded。
- 只有拿到 child_task_id 且预检、注册都成功后，才允许返回 `started` 或 `queued`。

### 2.5 根因五：容量限制与 Prompt 合同不一致

`ExecutionAdmissionLimits` 默认会限制：

- 全局活跃任务数：CPU 并行度限制在 2～8；
- 单会话活跃任务数：最多 4；
- 单角色全局任务数：最多 5；
- 可用内存低于阈值时暂停新任务准入。

但 coordinator Prompt 目前描述为“没有会话级代理总数上限”。这会导致模型发起多个代理后，部分任务进入 blocked/queued，用户却认为分配失败。

正式规则：Prompt、工具返回、Active Agent Center 和 API 投影必须使用同一套容量语义：

- `queued`：任务已创建，等待资源；
- `rejected`：本次未创建任务，调用方需要修改请求；
- `failed`：已创建任务但执行失败；
- `degraded`：代理不可用，主线应改派或接管；
- `completed`：代理完成并可通过 `agent_wait` 收集。

## 3. 唯一目标架构

### 3.1 协作模式

在 root TaskPolicy 中新增唯一字段：

```text
collaboration_mode: auto | required | disabled
```

字段来源：

| 来源 | 模式 |
|---|---|
| 用户明确要求多代理、指定角色或要求并行验证 | `required` |
| 用户明确禁止代理、要求单线处理 | `disabled` |
| 其他结构化任务 | `auto` |
| 普通聊天或一次性公开工具调用 | 不创建 coordinator Task |

`denied_tools` 不再承担协作模式建模，只用于最终权限拒绝。`collaboration_mode=disabled` 时，工具目录仍可保留编排工具定义，但运行时必须拒绝调用并返回明确的 `collaboration_disabled`；这样模型能理解当前策略，而不是把工具当成不存在。

### 3.2 派发前预检服务

在 `magi-conversation-runtime` 内增加唯一的纯逻辑预检入口，建议命名为：

```rust
preflight_agent_spawn(request: AgentSpawnRequest) -> Result<AgentSpawnPreflight, AgentSpawnRejection>
```

预检顺序必须固定，避免不同调用方得到不同结果：

1. JSON 解析和未知字段检查；
2. `task_name` 语法和当前父任务下唯一性；
3. `role` 存在、可派发、支持 `LocalAgent`，禁止 coordinator；
4. 能力解析：省略则按角色默认集合，显式传入则逐项校验；
5. `display_name` 和 `goal` 长度校验；
6. `context_package` 结构和边界校验；
7. 当前父任务存在活跃执行链，mission/root/workspace/write scope 一致；
8. 当前角色容量、全局容量、会话容量和资源状态检查；
9. 角色模型绑定预检：未绑定表示继承 orchestrator，已绑定则验证 engine 和 HTTP 模型配置完整；
10. Git 项目预检：base HEAD、Git service、外部漂移、worktree 根目录和创建前置条件；非 Git workspace 走普通目录执行语义；
11. 预检通过后才进入原子注册。

模型客户端预检必须复用 `resolve_target_for_role`，不得复制 settings 解析逻辑。Git 预检应复用现有 `GitPrecondition` 和 worktree coordinator，不允许通过“先创建、失败后删除”代替预检。

### 3.3 原子创建边界

预检成功后，使用当前 `register_spawned_local_agent_child` 完成以下原子操作：

```text
生成 child_task_id
→ 构造 Task
→ 写入 TaskStore
→ 写入 SpawnGraph
→ 更新 ActiveExecutionChain
→ 写入 TaskExecutionRegistry
→ 创建独占 ExecutionThread
```

任一写入失败必须完整回滚。返回 started 前必须确认以上步骤全部成功。

预检不应修改持久化状态；原子注册只接收不可变的预检快照，避免注册期间重新读取角色或模型配置导致前后不一致。

### 3.4 角色和能力合同

`AgentSpawnRequest` 正式字段：

```json
{
  "task_name": "risk_scan",
  "role": "explorer",
  "capabilities": ["security"],
  "display_name": "安全风险扫描",
  "goal": "检查认证、权限和敏感数据处理风险",
  "task_kind": "validation",
  "context_package": {
    "summary": "当前项目需要进行安全风险检查",
    "constraints": ["只读", "必须提供文件路径和证据"],
    "expected_output": "按严重程度列出风险和证据",
    "references": []
  },
  "plan_item_id": null,
  "parallelism_group": null
}
```

产品规则：

- `task_name` 是机器标识，由模型生成，不能重复；用户不需要填写。
- `display_name` 是用户可见标题，必须保留用户明确指定的名称。
- `role` 必须来自当前工具目录中的角色 ID。
- `capabilities` 可省略；省略时使用角色定义的 `capabilities`，旧内置角色使用注册表推导集合。
- 自定义角色没有能力时，必须在角色保存阶段拒绝，而不是等到任务派发时失败。
- `context_package` 保持结构化对象，不接受新的字符串合同；可以保留一次 JSON 字符串解码能力，但只作为协议解析层能力，不写入第二套业务模型。

### 3.5 模型选择

子代理模型解析规则保持单一入口：

1. 角色绑定 engine：只使用该 engine；配置无效直接返回 `model_preflight_failed`；
2. 角色未绑定 engine：继承当前 orchestrator 模型；
3. orchestrator 也不可用：预检失败，不创建子任务；
4. 不允许从其他角色、默认 executor 或任意可用模型静默回退。

返回中应包含非敏感的 `model_source`：`role_engine` 或 `inherited_orchestrator`，供前端和诊断使用。

### 3.6 Git 和非 Git 工作区

- 非 Git workspace：允许只读和可写子代理沿用 workspace 执行根目录，但必须继续遵守 TaskPolicy 的路径边界。
- Git workspace：只读子代理使用 detached worktree；可写子代理使用基于 session base HEAD 的独立 branch/worktree。
- base HEAD 缺失、session context 漂移、Git service 未注入、worktree 创建条件不满足时，预检直接拒绝。
- worktree 创建失败不能留下 Task、Lease、Thread 或 active branch。
- 已存在的脏 worktree 只能进入清理保留状态，不能被新任务复用。

## 4. 工具返回合同

### 4.1 `agent_spawn` 成功

```json
{
  "tool": "agent_spawn",
  "status": "started",
  "child_task_id": "task-spawn-...",
  "role": "explorer",
  "capabilities": ["security"],
  "title": "安全风险扫描",
  "assignment": { "goal": "...", "role": "explorer" },
  "instruction": "若最终结论依赖该代理，必须使用 agent_wait。"
}
```

### 4.2 `agent_spawn` 排队

只有任务已经完成原子创建，但执行准入暂时不足时才返回：

```json
{
  "tool": "agent_spawn",
  "status": "queued",
  "child_task_id": "task-spawn-...",
  "queue_reason": "当前会话执行容量已满（4/4）",
  "retry_hint": "等待现有代理进入终态后继续 agent_wait。"
}
```

容量不足不应在创建前伪造 child_task_id，也不应被标记为失败。若产品决定“容量不足不创建”，则必须统一使用 rejected；实现时只能选择一种语义，建议采用上述 queued 语义。

### 4.3 `agent_spawn` 创建前拒绝

```json
{
  "tool": "agent_spawn",
  "status": "rejected",
  "error_code": "unsupported_capability",
  "failure_stage": "input_validation",
  "error": "角色 explorer 不拥有 capability xxx",
  "instruction": "请从 tool_catalog 返回的 explorer 能力集合中重新选择。",
  "diagnostic_ref": "tool_call:<id>",
  "child_task_id": null
}
```

固定错误码至少包括：

```text
invalid_arguments
invalid_task_name
duplicate_task_name
missing_required_fields
agent_role_not_spawnable
unsupported_capability
invalid_context_package
collaboration_disabled
active_execution_chain_missing
agent_capacity_exceeded
model_preflight_failed
git_preflight_failed
workspace_preflight_failed
agent_spawn_registration_failed
```

### 4.4 运行期失败

运行期失败必须保留 child_task_id，并由 `agent_wait` 返回：

```json
{
  "status": "failed",
  "child_status": "failed",
  "child_task_id": "task-spawn-...",
  "failure_stage": "model_invocation",
  "error_code": "model_invocation_failed",
  "error": "代理模型请求未完成",
  "fallback_mode": "mainline_or_reassign"
}
```

内部网络地址、API Key、堆栈和完整 provider 错误继续写日志，但不得进入用户可见输出。用户可见错误必须能区分：角色配置、模型配置、资源排队、Git 隔离、模型执行和任务取消。

## 5. 主线恢复和失败处理

coordinator 收到 `agent_wait` 结果后必须遵守：

- `completed`：读取 `assignment.goal`、`result.final_text`、`summary` 并合并；
- `failed`：根据 `error_code` 决定改派、主线接管或终止；
- `degraded`：不得把它当作成功结果，必须产生改派或主线接管说明；
- `timeout`：保留 pending_task_ids，不能把未完成代理总结成已完成；
- `attention_required`：先处理 `agent_context_request`，再继续等待；
- 等待范围不匹配：不重试同一错误 ID，必须使用最近一次 `agent_spawn` 返回的 child_task_id。

改派规则：

1. 同角色模型配置失败：不要重复派发同一角色，优先选择其他可用角色；
2. 角色能力不匹配：修正参数后重新派发，不创建失败 Task；
3. 容量排队：等待现有任务终态，不重复创建同一目标；
4. Git 隔离失败：提示用户修复 workspace 状态，不能绕过隔离直接执行；
5. Worker 无匹配：使用 `tool_catalog` 重新选择角色，或由主线接管。

## 6. 前端产品行为

### 6.1 主线消息中的代理卡片

`ToolCall.svelte` 和 `ConversationAgentGroup.svelte` 只消费统一状态合同：

- `rejected`：显示“未创建”，显示错误原因和下一步；
- `queued`：显示“排队中”，显示队列原因；
- `started/running`：显示“运行中”；
- `completed`：显示“已完成”；
- `failed`：显示“执行失败”，提供错误阶段；
- `degraded`：显示“代理不可用，主线处理中”或“等待改派”。

不能通过“是否存在 child_task_id”推断所有状态；状态字段是唯一来源。

### 6.2 Active Agent Center

投影 API 必须返回：

```text
agentRunId
parentTaskId
role
capabilityIds
status
lifecycle
failureStage
failureCode
failureMessage
queueReason
fallbackMode
modelSource
```

其中 `failureMessage` 只返回经过公共脱敏的消息。Active Agent Center 需要把排队和失败分开展示，避免把 `queued` 统计为 attention/error。

### 6.3 设置页和工具目录

角色设置保存时完成：

- role ID、支持 TaskKind、基础 Prompt、能力集合、并发上限、engine 绑定校验；
- 自定义角色必须至少支持 `local_agent` 才能成为可派发角色；
- 能力 ID 必须来自能力注册表；
- 并发上限为空表示不设置角色级上限，但仍受全局、会话和资源准入限制。

`tool_catalog(includeAgentRoles=true)` 返回每个角色的：

```text
role_id
display_name
description
spawnable
supported_kinds
capability_ids
parallelism_limit
model_binding_status
```

## 7. 实现边界和文件责任

| 文件/模块 | 责任 |
|---|---|
| `crates/magi-api/src/routes/sessions.rs` | 只负责路由和协作模式归一化，移除基于关键词裁剪工具的主逻辑 |
| `crates/magi-conversation-runtime/src/prompt_utils.rs` | 描述 `auto/required/disabled` 统一语义，更新容量和失败规则 |
| `crates/magi-conversation-runtime/src/builtin_tool_schema.rs` | 移除全局 capabilities enum，保留结构 Schema |
| `crates/magi-conversation-runtime/src/tool_batch.rs` | 解析请求、调用预检、输出统一状态合同；不直接复制模型/Git校验逻辑 |
| `crates/magi-conversation-runtime/src/task_execution_registry.rs` | 接收预检快照并执行原子注册与回滚 |
| `crates/magi-conversation-runtime/src/task_execution_dispatcher.rs` | 复用统一模型和 worktree 预检；执行期只处理真实运行失败 |
| `crates/magi-conversation-runtime/src/task_runner.rs` | 统一 queued/blocked/retry 行为和准入状态 |
| `crates/magi-api/src/routes/agent_runs.rs` | 输出脱敏后的失败阶段、错误码、排队原因和模型来源 |
| `web/src/components/ToolCall.svelte` | 仅按统一状态渲染代理卡片 |
| `web/src/components/ActiveAgentCenter.svelte` | 区分排队、运行、降级和失败 |
| `web/src/components/ConversationAgentGroup.svelte` | 使用 projection 状态，不自行推断生命周期 |
| `web/src/i18n/zh-CN.json`、`en-US.json` | 补齐所有状态、失败阶段、排队原因和恢复动作文案 |

禁止在前端、API、ToolBatch、Runner 各自实现一套状态映射；公共错误码和生命周期枚举必须只有一个定义源。

## 8. 开发计划与阶段状态

实现 Agent 必须在每完成一个阶段后更新本节状态，并在提交说明中写明验证命令。阶段未完成时不得跳到下一阶段。

### 阶段 0：基线与回归保护

- [x] 固定当前分支和工作区基线，保留无关改动
- [x] 运行 `cargo test -p magi-conversation-runtime agent_spawn --lib`
- [x] 运行 `cargo test -p magi-agent-role --lib`
- [x] 记录当前失败合同、工具目录和角色目录快照
- [x] 状态：已完成（2026-09-13；`cargo check --workspace`、conversation-runtime 481 项、agent-role 33 项通过）

### 阶段 1：协作模式和入口收敛

- [x] 引入 `collaboration_mode`
- [x] 移除普通任务按关键词默认裁剪协作工具的逻辑
- [x] 更新 root coordinator Prompt 和相关单元测试
- [x] 验证普通复杂任务可获得 auto coordinator 工具面
- [x] 状态：已完成（2026-09-13；sessions/dispatcher 相关测试通过）

### 阶段 2：角色能力和 Schema 收敛

- [x] 移除全局能力 enum
- [x] 实现角色能力默认补齐和显式能力严格校验
- [x] 更新 tool_catalog 的角色能力输出
- [x] 增加内置角色和自定义角色的成功/失败测试
- [x] 状态：已完成（2026-09-13；tool schema、tool catalog 与角色能力测试通过）

### 阶段 3：派发前统一预检

- [x] 实现唯一 `agent_spawn` preflight
- [x] 接入模型配置预检
- [x] 接入 Git/workspace 预检
- [x] 接入容量和资源检查
- [x] 确保预检失败不产生 Task、Lease、Thread 或 SpawnGraph 边
- [x] 状态：已完成（2026-09-13；preflight 与无副作用拒绝测试通过）

### 阶段 4：原子注册和运行期状态

- [x] 让原子注册只接收预检快照
- [x] 统一 `started/queued/rejected/failed/degraded/completed`
- [x] 校验异常、取消、租约过期和 dispatcher panic 的状态收口
- [x] 限制 `agent_spawn` 只能由 root coordinator 发起，Worker 不得递归创建子代理
- [x] Runner 重启等待 root 维度全部异步 dispatch 退出，避免 LLM 线程和执行计划残留
- [x] worktree 绑定执行租约，迟到的旧 dispatch 不得清理恢复后的新一轮 worktree
- [x] 验证容量释放后 queued 任务由 Runner 自动恢复，不重复创建 Task、Thread 或 SpawnGraph 边
- [x] 状态：已完成（2026-09-13；TaskRunner、admission、registry 回滚、递归防护、dispatch quiesce 与 worktree 租约测试通过）

### 阶段 5：agent_wait、改派和前端展示

- [x] 更新 agent_wait 结果合同和主线 Prompt
- [x] 输出脱敏失败阶段、错误码、排队原因
- [x] 更新 ToolCall、Agent Center、代理分组和中英文国际化
- [x] 增加 `agent_wait` 严格参数解析、直接子代理与作用域校验，拒绝静默过滤非法 task id
- [x] `agent_send` 仅允许向当前任务直接子代理发送且校验 mission/root/workspace/write scope
- [x] 状态：已完成（2026-09-13；conversation-runtime 相关测试、Web check/build/test 通过）

### 阶段 6：真实验证和发布前检查

- [x] Rust 相关测试全部通过：`cargo test --workspace --all-targets`（2580 项通过、0 失败、1 项忽略），并通过 `cargo fmt --all -- --check`、`git diff --check`
- [x] `npm --prefix web run check`
- [x] `npm --prefix web run build`
- [x] 使用 daemon 入口完成真实主对话派发、等待、运行失败归类和改派/主线接管指令验证
- [x] 使用本地 Electron/Desktop 包验证角色配置、任务识别、子代理执行和导入导出不回归
- [x] 检查工作区无意外修改，提交完整变更
- [x] 状态：已完成（2026-09-14；代码、daemon 和 Electron/Desktop 闭环验证完成）

### 阶段 6 验证记录

#### 代码与构建

- `cargo test --workspace --all-targets`：2580 项通过、0 失败、1 项忽略。
- `cargo fmt --all -- --check`：通过。
- `git diff --check`：通过。
- `npm --prefix web run check`：通过。
- `npm --prefix web run build`：通过。
- `npm test`：通过。
- `cargo check -p magi-daemon`：通过。

#### daemon 主对话真实链路

- 正常派发：root `task-local-agent-1789320262904` 创建 explorer 子代理 `task-spawn-task-local-agent-1789320262904-1789320292828-4`，`agent_spawn` 返回成功，`agent_wait` 收集到 `completed`，子代理结果为 `STAGE6_CHILD_OK`。
- 运行失败：子代理 `task-spawn-task-local-agent-1789320487206-1789320518246-5` 保留 child task，投影为 `lifecycle=failed`、`failureStage=model_invocation`、`failureCode=model_invocation_failed`，公开错误已脱敏并带有 `mainline_or_reassign` 指令。
- 重启恢复：root `task-local-agent-1789320882393` 重启后验证终态任务不会残留 active agent、lease 或执行线程。
- `Continue` 限制：在模型失败场景调用 Continue 当前返回“当前会话没有活跃任务”；该路径未标记为通过，当前产品行为仍应由主线改派或接管，不对 Continue 成功作出声明。

#### Electron/Desktop 包真实链路

- 包路径：`/Users/xie/code/magi-rust-rewrite/target/electron-dist/mac-arm64/Magi.app`。
- daemon 健康检查：`http://127.0.0.1:38123/health` 返回 `status=ok`，`runtimeEpoch=runtime-1789322546680-9580-1`。
- 构建标识：`79cd230f091bc5c105e698c9c24687073fc134c5`。
- 角色闭环：创建用户角色 `stage6-desktop-role`，导出 Markdown，再以 `stage6-desktop-role-imported` 导入；注册表同时识别两个用户角色，并验证自定义角色默认能力补齐、主对话任务识别和子代理执行。
- 验证结束后已删除上述临时角色及 `stage6-failing-engine`，注册表无残留测试绑定。

## 9. 必须覆盖的验收场景

### 9.1 正常路径

1. 普通复杂请求，未出现“代理”关键词，模式为 auto，coordinator 能自主派发 explorer。
2. 用户明确要求两个不同角色并行，两个 `agent_spawn` 都返回 started，随后 `agent_wait` 返回两个 completed。
3. 用户自定义角色只配置 `desktop`，模型省略 capabilities，系统自动补齐并成功执行。
4. 用户自定义角色绑定独立 engine，模型来源显示 `role_engine`。
5. 非 Git workspace 中的只读子代理可以正常执行。

### 9.2 预检拒绝

1. role 不存在或为 coordinator：返回 rejected，无 child_task_id，无 TaskStore 记录。
2. 能力不属于角色：返回 unsupported_capability，无副作用。
3. context_package 类型错误、字段超长：返回 invalid_context_package，无副作用。
4. 角色模型 engine 不存在或配置不完整：返回 model_preflight_failed，无子任务。
5. Git session 没有 base HEAD 或发生外部漂移：返回 git_preflight_failed，无子任务。
6. 用户明确禁止协作：返回 collaboration_disabled，不创建子任务。

### 9.3 排队和运行失败

1. 会话容量满：新代理显示 queued，显示具体原因；已有代理完成后自动进入 running。
2. 子代理模型请求超时：agent_wait 返回 failed 和 model_invocation_failed，主线可改派或接管。
3. Worker 在启动线程中 panic：任务进入 failed，返回脱敏错误，不残留活跃 lease。
4. 子代理 worktree 有未提交改动：任务完成但 worktree 正确进入 retained/inactive，后续任务不复用。
5. agent_wait 使用非直接子任务 ID：返回 scope_mismatch，不泄露其他任务结果。

### 9.4 用户体验

1. 主线卡片不会出现“已成功”但没有 child_task_id 的状态。
2. 排队、拒绝、运行失败、降级接管在中文和英文界面均有明确文案。
3. 错误文案不暴露 API Key、完整 URL、堆栈和内部路径。
4. 代理最终结果必须在主线明确引用并吸收；未等待或未吸收时不得结束主线答复。

## 10. 完成定义

只有同时满足以下条件，才可以将本方案标记为完成：

- 入口、Schema、预检、原子注册、Runner、agent_wait 和前端使用同一套状态合同；
- 任何创建前失败都不产生孤儿 Task、Lease、Thread、SpawnGraph 边或 worktree；
- 自定义角色和内置角色走完全相同的派发链路；
- 普通复杂表达不再因为缺少“代理”关键词而失去 coordinator 能力；
- 明确禁止协作的请求仍然严格单线执行；
- 模型配置、能力、容量、Git 隔离失败均可定位到明确阶段；
- Rust、Web、daemon 和 Electron/Desktop 真实验证全部通过；
- 实现 Agent 完成阶段状态更新、测试记录和最终提交说明；
- 不保留旧关键词裁剪、旧全局能力 enum、伪成功状态或第二套回退实现。
