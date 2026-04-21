# 统一 Task 编排内核架构升级方案

更新时间：2026-04-16

## 1. 目标

本文档定义 Magi 后续通用编排架构升级方案，解决四个问题：

1. 用一套内核同时支持小任务与大任务
2. 将用户规则落成结构化运行约束
3. 统一 `Objective / WorkGraph / Policy / Runner / Escalation`
4. 放弃 `Todo` 领域模型，统一收口到 `Task`

## 2. 架构拍板

### 2.1 单一主抽象

任务域只保留一个主工作对象：`Task`；`Mission` 与 `Worker` 仍然存在，但它们不是第二套任务模型。

- `Mission` 是上下文与知识容器，位于 `Objective Task` 之上
- `Objective` 不是独立任务系统，而是 `Task.kind=Objective`
- `WorkGraph` 是 `Task` 的父子边与依赖边组成的图
- `Worker` 是带角色与能力声明的执行主体，通过绑定协议领取可执行任务
- UI 层也统一使用 `Task` 术语，所有视图都属于 `Task Projection`

### 2.2 明确废弃

以下内容禁止继续存在或新增：

- `Todo` 领域模型
- `TodoService / TodoStore / Todo API / TodoStatus`
- 小任务专用任务系统
- 大任务专用任务系统
- 绕过 `TaskPolicy` 的临时授权逻辑

### 2.3 统一原则

- 所有用户请求都必须先归一化为一个 `Objective Task`
- 每个 `Objective` 在进入执行前都必须先生成一份最小完备 `Task Graph`
- 小任务与大任务不是两套模式，只是同一图生成机制下的不同规模结果
- 图在运行中只允许细化、重规划或收缩未执行子树，不允许切换到另一套任务模型
- Runner 只执行已经合成出的叶子任务，不负责临时发明第二套结构

## 3. Task 规格

每个 `Task` 至少必须具备以下字段：

- `task_id`
- `mission_id`
- `root_task_id`
- `parent_task_id`
- `kind`
- `title`
- `goal`
- `status`
- `dependency_ids`
- `policy_snapshot`
- `context_refs / knowledge_refs`
- `workspace_scope / write_scope`
- `executor_binding`
- `input_refs / output_refs / evidence_refs`
- `retry_count / repair_count`
- `created_at / updated_at`

### 3.0 Mission 与 Worker

- `Mission` 不是任务节点，而是一次用户目标的上下文容器，承载会话、知识关联、约束背景与长期目标理解
- 一个 `Mission` 可以拥有一个或多个 `Objective Task`；多个 Objective 共享同一 Mission 上下文，但各自维护自己的图与状态
- `Worker` 不是任务模型，而是执行主体，带有 `role`、能力声明、工具权限、工作区偏好与并发上限
- `Assignment` 不再作为独立任务层存在，但其核心语义保留在 `executor_binding` 与运行时 lease 中

### 3.0.1 Worker 绑定协议

- 只有可执行任务才允许带 `executor_binding`，主要是 `Action / Validation / Repair`
- `executor_binding` 至少应包含：`target_role`、`capability_requirements`、`parallelism_group`、`exclusive_scope`
- Planner 在建图时必须参考 Worker 能力画像，避免生成无 Worker 可领取的叶子任务
- Runner 调度时必须在“任务可运行”之外，再做一次“是否存在合格 Worker”的匹配校验
- 多 Worker 并行执行时，写范围冲突、独占作用域冲突和同一并发组上限必须统一裁决

### 3.0.2 Mission 上下文传播规则

- `Mission` 必须持有会话上下文、PKB/ADR/FAQ 等知识引用，以及目标背景摘要
- Planner 在生成 `Objective Task` 时，必须从 `Mission` 中裁剪出当前任务所需的 `context_refs / knowledge_refs`
- `Action` 不直接继承整个 Mission，而是只接收与当前任务相关的上下文切片，避免上下文污染
- 同一 `Mission` 下的多个 `Objective` 共享知识与会话背景，但各自拥有独立图、独立状态机与独立验收闭环
- 若一个 Objective 的产物成为另一个 Objective 的输入，必须通过显式 `output_refs -> input_refs` 关联，而不是隐式共享内存态

### 3.1 TaskKind

`TaskKind` 必须区分为两类：初始图可出现的计划型节点，与执行中动态创建的运行时节点。

#### 3.1.1 计划型节点

- `Objective`：整图根节点，承载用户最终目标、全局规则和最终完成判定
- `Phase`：阶段分组节点，只负责阶段 gate、分组聚合与阶段性汇报
- `WorkPackage`：用户可理解的工作包，是默认的主汇报与主验收单元
- `Action`：最小执行动作，是默认可调度叶子节点，负责读/改/生成/调用 Worker
- `Validation`：客观验收节点，负责测试、比对、检查、校验

#### 3.1.2 运行时节点

- `Repair`：只在失败、偏差或验证不通过后由 Runner 动态创建
- `Decision`：只在命中审批、分歧或策略边界时由 Runner 动态创建

#### 3.1.3 使用边界

- 只有顶层任务能使用 `Objective`
- `Phase` 只在任务需要显式阶段 gate 或独立阶段汇报时使用；普通简单任务不强制创建 `Phase`
- `WorkPackage` 用于表达“用户能理解并愿意单独验收”的工作单元；不是所有任务都必须强制出现
- `Action` 只做一步实际动作；如果一个节点同时承担“修改 + 验证 + 决策”，必须拆开
- `Validation` 必须附着在某个 `Objective / Phase / WorkPackage` 子树下，不能游离成无归属验证
- `Repair / Decision` 禁止出现在初始图中，只能由执行期动态生成

### 3.2 合法父子关系

- `Objective -> Phase | WorkPackage | Action | Validation`
- `Phase -> WorkPackage | Action | Validation`
- `WorkPackage -> WorkPackage | Action | Validation | Repair | Decision`
- `Action / Validation / Repair / Decision` 默认不再挂子节点

补充约束：

- 若任务需要独立汇报、独立验收或独立授权，应显式创建 `WorkPackage`
- 若任务是简单单步目标，可直接使用 `Objective -> Action` 或 `Objective -> Action -> Validation`
- `Repair / Decision` 不得作为初始规划结果预铺在图上

不满足上述关系的结构一律视为非法图。

### 3.3 统一状态机

统一状态机固定为：

- `Draft`
- `Ready`
- `Running`
- `Blocked`
- `AwaitingApproval`
- `Verifying`
- `Repairing`
- `Completed`
- `Failed`
- `Cancelled`
- `Skipped`

### 3.3.1 状态语义

- `Draft`：任务已创建但尚未满足进入调度的前置条件
- `Ready`：任务满足调度条件，可被 Runner 选择
- `Running`：任务已被执行器接管，正在执行主动作
- `Blocked`：任务存在外部阻塞，但不一定需要人工输入
- `AwaitingApproval`：任务被 `Decision` 阻塞，明确等待用户输入
- `Verifying`：任务主动作已完成，正在跑客观验证
- `Repairing`：任务因失败或验证不通过进入修复阶段
- `Completed`：任务按规则完成并已通过必要验收
- `Failed`：任务终局失败且无剩余可用修复路径
- `Cancelled`：用户或系统明确终止
- `Skipped`：任务被上层决策跳过，不再参与完成性要求

### 3.3.2 强制约束

- 不再保留 `Reviewing` 作为正式状态；自检属于执行器内部行为，不单独进入任务状态机
- 需要验收的任务不得从 `Running` 直接写成 `Completed`
- `Decision` 进入 `AwaitingApproval` 后，其下游必须全部不可运行
- `Validation` 未通过前，对应 `WorkPackage` 不得完成

### 3.3.3 状态迁移表

| From | To | 触发条件 | 触发者 | 证据/审批要求 |
| --- | --- | --- | --- | --- |
| `Draft` | `Ready` | 图合成完成，依赖合法，`policy_snapshot` 冻结 | Planner | 不要求审批 |
| `Ready` | `Running` | 被 Runner 选中并取得执行 lease | Runner | 不要求审批 |
| `Ready` | `Blocked` | 出现外部阻塞、资源锁冲突或前置条件暂失 | Runner/System | 必须写 blocker 信息 |
| `Ready` | `AwaitingApproval` | 命中审批要求或进入 `Decision` gate | Runner/System | 必须等待人工输入 |
| `Running` | `Verifying` | 主动作完成且存在必需验证 | Executor/Runner | 必须写 `output_refs/evidence_refs` |
| `Running` | `Completed` | 主动作完成且不存在必需验证 | Executor/Runner | 必须写输出证据 |
| `Running` | `Repairing` | 执行失败但仍存在可用修复路径 | Runner | 必须生成 repair 证据 |
| `Running` | `Failed` | 执行不可恢复失败，或预算已耗尽 | Runner | 必须写 failure 证据 |
| `Verifying` | `Completed` | 验证通过 | Validator/Runner | 必须写 validation 证据 |
| `Verifying` | `Repairing` | 验证失败但可修复 | Runner | 必须写 validation 证据 |
| `Verifying` | `Failed` | 验证失败且无修复路径 | Runner | 必须写 validation 证据 |
| `Repairing` | `Verifying` | 修复动作完成，进入复验 | Repair Executor/Runner | 必须写 repair 证据 |
| `Repairing` | `Failed` | 修复失败或修复预算耗尽 | Runner | 必须写 repair 证据 |
| `Blocked` | `Ready` | 阻塞解除，或上游 `Decision` 已完成并解除 gate | User/System/Runner | blocker 必须可审计 |
| `AwaitingApproval` | `Completed` | 用户决策完成并写入 decision evidence | User/Runner | 必须写 decision 证据 |
| `Draft / Ready / Blocked` | `Skipped` | 上层决策跳过可选子树 | User/Runner | 必须写 skip reason |
| 任一非终态 | `Cancelled` | 用户或系统明确取消 | User/System | 必须写 cancel reason |

补充约束：

- 终态固定为：`Completed / Failed / Cancelled / Skipped`
- `Ready` 之后禁止静默回到 `Draft`
- 进入 `Cancelled` 或 `Skipped` 后不得再次进入可运行态
- 父任务状态必须在子任务迁移后立刻重新聚合

## 4. WorkGraph 规则

1. 每个 `root_task_id` 只允许一个 `Objective`
2. 依赖边必须无环
3. 父子边与依赖边不能冲突
4. Runner 只能调度叶子节点
5. 所有父节点状态必须由子节点聚合产生，不允许手工写死

### 4.1 可运行判定

一个 `Task` 可被 Runner 选中，必须同时满足：

- `status=Ready`
- 所有依赖已完成
- 父链上不存在 `Blocked / AwaitingApproval`
- `policy_snapshot` 校验通过
- 与当前运行任务不存在写范围冲突

### 4.1.1 完成性与父节点聚合规则

- 每条父子关系都必须带 `required=true|false` 语义；默认 `required=true`
- 父任务进入 `Completed` 的前提是：所有必需直接子任务都已进入成功终态，且必需 gate 已通过
- 成功终态只包括：`Completed`，以及带显式 skip reason 的 `Skipped`
- 任一必需子任务处于 `Blocked / AwaitingApproval`，父任务必须聚合为 `Blocked`
- 任一子任务处于 `Running / Verifying / Repairing`，父任务必须聚合为非终态进行中
- 任一必需子任务 `Failed` 且无剩余修复路径时，父任务必须聚合为 `Failed`
- 可选子任务失败不会直接导致父任务失败，但必须进入父任务风险摘要

### 4.2 图生成、反思与重规划规则

#### 4.2.1 当前最小充分图

所有用户请求都必须先生成一份“当前最小充分 `Task Graph`”，再进入 Runner。这里要求的是当前足够执行，不是假设 Planner 一次就能生成永远正确的完美图。

- 简单只读任务可直接为：`Objective -> Action`
- 简单有副作用任务可直接为：`Objective -> Action -> Validation`
- 只有当任务需要独立汇报、独立验收、独立授权或独立 Worker 角色时，才必须创建 `WorkPackage`
- `Phase` 只有在存在显式阶段 gate、阶段边界或阶段级审批时才创建
- Planner 在建图时必须同时参考 `Mission` 上下文、知识引用与可用 Worker 能力画像

#### 4.2.2 图反思规则

系统不得假设初始图永远正确。以下时点必须触发一次“图反思”：

- 初始图合成完成后
- 任一 `WorkPackage` 完成后
- Worker 报告粒度不对、角色不匹配或上下文不足时
- 连续修复后仍反复失败时
- `Decision` 改变约束、边界或策略时

图反思的结果只允许是：

1. 接受当前图，继续执行
2. 细化剩余子树
3. 收缩剩余子树
4. 对剩余图重规划
5. 在同一 `Mission` 下派生新的 `Objective`

#### 4.2.3 重规划规则

当执行中发现新约束、新依赖、新理解或新风险时，系统可以重规划剩余图，但必须遵守：

- 保留原 `mission_id`、`root_task_id` 与 `Objective.task_id`
- 已完成任务不得重写，只允许追加证据
- 已开始运行的任务不得被静默替换，只能等其终态后再重规划后续子树
- `Repair / Decision` 只能作为运行时结果生成，不得被回填成“初始规划早已知道”的节点
- 重规划针对“剩余图”，不是切换到另一套任务模型

#### 4.2.4 明确禁止

- 禁止为简单任务和复杂任务分别维护两套任务系统
- 禁止把 `Repair / Decision` 预铺成初始图的常驻节点
- 禁止在运行时引入 `Todo`、`Node` 等第二套主对象来补洞

---

## 5. TaskPolicy 规格

`TaskPolicy` 是用户规范的唯一结构化承载点，至少包含：

- `autonomy_level`：`Manual | SingleStep | ShortAutonomy | LongAutonomy`
- `approval_mode`
- `allowed_tools / denied_tools`
- `allowed_paths / denied_paths`
- `network_mode / command_mode`
- `retry_limit / repair_limit`
- `validation_profile`
- `checkpoint_mode`
- `background_allowed`
- `escalation_conditions`

### 5.1 继承规则

- `Objective.policy` 是整图默认策略模板，不直接等于所有子任务快照
- 子任务创建时，先继承最近祖先的有效策略，再叠加只收紧不放宽的局部约束
- `Phase` 与 `WorkPackage` 可以收紧策略边界，但不得突破根策略上限

### 5.2 冻结规则

- `Task` 在 `Draft -> Ready` 时冻结 `policy_snapshot`
- 进入 `Ready / Running / Verifying / Repairing` 后，`policy_snapshot` 不得被静默修改
- 终态任务的 `policy_snapshot` 永久只读，用于审计与恢复

### 5.3 刷新与变更传播规则

- 父任务策略变更只自动影响仍处于 `Draft` 的子任务，以及尚未物化的新子树
- 已进入 `Ready` 及之后状态的任务，不因父策略变化而自动刷新快照
- 若策略变化需要影响剩余执行路径，必须通过重规划生成新的未执行子树，并为新子树重新冻结快照
- `Blocked / AwaitingApproval` 任务只有在用户明确修改规则且任务尚未重新进入运行时，才允许刷新快照

### 5.4 Runner 执行规则

- Runner 只读取任务上的 `policy_snapshot`，不读取模糊的祖先意图
- 若任务缺少快照，则不得进入 `Ready`
- 若执行前发现快照与当前图结构不匹配，必须先重规划，不得边跑边补授权

## 6. Runner 协议

Runner 不承担“先跑再补结构”的职责。图生成与细化先于执行发生；Runner 只消费当前已合成的 `Task Graph`。

Runner 的标准循环如下：

1. 读取 `root_task_id` 对应的图、策略快照与状态快照
2. 计算当前所有可运行叶子任务
3. 根据调度策略排序：优先同阶段、同工作区、低切换成本、先验证后扩散写入
4. 做并发裁决：写范围冲突任务不得并行
5. 将任务分派到匹配的 Worker 或对应的 `Validation / Repair / Decision` 处理器
6. 执行完成后写入 `output_refs / evidence_refs`
7. 推进任务状态，并重新聚合父任务状态
8. 命中失败且仍有预算时创建 `Repair Task`
9. 命中规则边界或决策分叉时创建 `Decision Task`
10. 持久化 checkpoint，再进入下一轮

### 6.1 Checkpoint / Resume 语义

#### Checkpoint 必须持久化的内容

- 当前 `Task Graph` 结构与版本
- 所有任务状态、依赖边、父子边
- 所有任务的 `policy_snapshot`
- `input_refs / output_refs / evidence_refs`
- `retry_count / repair_count`
- 未完成的 `Decision` 与 blocker 信息
- 最近一次调度游标与聚合结果

#### 触发时机

- 任一任务状态迁移后
- 图细化或重规划后
- `Decision` 完成后
- 后台挂起前
- Runner 正常退出前

#### Resume 规则

- 恢复时必须先重建图与聚合状态，再重新进入调度
- 崩溃前处于 `Running / Verifying / Repairing` 的任务，不得假定已经成功完成
- 若缺少足够证据证明其完成，这些任务必须回到 `Ready` 或转入 `Blocked` 等待恢复判断
- 恢复永远基于已持久化证据，不基于内存态猜测副作用结果

## 7. Escalation 协议

系统必须在以下情况创建 `Decision Task`：

- 需要使用未授权工具或路径
- 发现两个以上同等合理的架构分支
- `retry_limit / repair_limit` 已耗尽
- 验证结果与用户规则冲突
- 缺少关键事实导致无法继续

每个 `Decision Task` 必须包含：

- `decision_context`
- `blocked_reason`
- `options`
- `risk_notes`
- `recommended_option`
- `required_user_input`

### 7.1 Decision 完成与下游解锁规则

- 用户输入一旦被接受，`Decision Task` 必须写入 `decision evidence` 并进入 `Completed`
- 所有直接依赖该 `Decision` 的下游任务必须重新计算依赖满足性
- 若用户输入只是在既有边界内做选择，下游任务可由 `Blocked / AwaitingApproval` 重新进入 `Ready`
- 若用户输入改变了路径范围、工具范围、风险预算或阶段边界，系统必须先对剩余子树重规划，再决定哪些任务进入 `Ready`
- 若用户决策是否定性终止，则受影响子树必须显式进入 `Cancelled` 或 `Skipped`，不得静默消失

## 8. 视图规则

### 8.1 术语规则

- UI 正式术语统一为 `Task`
- 所谓“当前待处理项”只允许作为自然语言描述，不再成为正式视图名、模型名或协议名
- 所有列表、本体详情、过滤器、分组与看板都基于 `Task Projection` 生成

### 8.2 Task Projection 最小结构

每个投影视图至少应输出：

- `root_task`：当前 Objective 摘要
- `current_phase`：当前活跃 Phase
- `running_tasks`：当前运行中的叶子任务
- `blocked_tasks`：当前阻塞任务
- `pending_decisions`：待用户输入的 Decision 任务
- `workpackage_summaries`：各 WorkPackage 的内部聚合状态、显示状态、进度比例、最近证据、最近问题
- `validation_summary`：通过/失败/待运行的验证概况
- `progress_summary`：整图完成度、剩余任务数、失败任务数、部分成功任务数

### 8.3 投影语义规则

- 内部状态聚合仍以 `TaskStatus` 为准，这是运行时真相源
- 面向用户的显示语义不得直接等同于内部聚合状态
- 例如某个 `WorkPackage` 内部聚合为 `Failed` 时，投影仍可显示“80% 已完成，1 项失败待修复”
- `Task Projection` 必须显式区分 `aggregate_status` 与 `display_status`
- 叶子任务过滤列表只是 `Task View` 的一个子投影，任何视图都不得引入 `Todo` 作为额外对象

## 9. 最终约束

### 必须执行

- 统一主工作对象为 `Task`
- `Mission` 作为上下文与知识容器继续保留
- 顶层目标使用 `Task.kind=Objective`
- WorkGraph 只由 `Task` 组成
- 可执行任务必须通过 `executor_binding` 绑定到角色/Worker 能力集合
- 用户规则只落在 `TaskPolicy`
- Runner 只调度叶子 `Task`
- 决策升级统一落到 `Decision Task`

### 明确禁止

- 继续保留 `Todo` 领域模型
- 为小任务和大任务分别实现两套任务系统
- 在 UI、协议、存储、事件中继续使用 `Todo` 作为正式术语
- 运行时绕开 `TaskPolicy` 做临时授权
