# 深度模式 × 任务系统最终方案设计

## 0. 核心结论

Magi 的产品心智收敛为：

> Magi 始终是一个编排团队在工作；普通模式是轻量单轮协作策略，深度模式是高自治持续交付策略；团队不是模式，而是 AgentTab 中可观察的执行视图。

因此后续只保留两类执行策略：

```text
普通模式：轻量处理当前问题
深度模式：高自治持续交付目标
```

不再引入：

```text
团队模式
```

团队能力是底层默认存在的编排形态，不是用户需要额外选择的模式。

## 1. 产品概念定义

### 1.1 普通模式

普通模式面向：

- 问答；
- 小修；
- 单轮任务；
- 小范围代码修改；
- 用户希望快速得到结果的场景。

用户心智：

> 我现在有一个问题，你帮我处理这一轮。

系统行为：

- 可使用单 agent，也可使用少量 worker；
- Task Graph 较浅；
- 更快返回；
- 更倾向在当前对话边界结束；
- 用户后续输入默认是普通 follow-up。

### 1.2 深度模式

深度模式面向：

- 从 0 到 1 创建项目；
- TS 项目重构为 Rust；
- 多模块重构；
- 长任务持续推进；
- 用户前期给目标，期望系统持续做到可验收结果。

用户心智：

> 我给你目标、边界和验收标准，你组织团队持续推进，直到完成或遇到必须我判断的地方。

系统行为：

- 生成更完整的 Task Graph；
- 使用更高自治的 TaskPolicy；
- Runner 持续推进未完成任务；
- 自动验证、自动修复、自动 checkpoint；
- 中途尽量不打断用户；
- 只有关键风险、授权边界、方向分歧、修复预算耗尽等情况才生成 Decision Task；
- 最终输出交付包。

### 1.3 团队不是模式

团队是底层执行形态：

```text
Orchestrator / Runner
  → 拆解任务
  → 分配 Worker
  → 执行任务
  → 收集 Evidence
  → 验证
  → 修复
  → 重规划
  → 必要时请求用户决策
```

AgentTab 是团队视图，不是团队模式。

用户打开 AgentTab，是为了看：

- 哪个 agent 在做什么；
- 当前绑定哪个 task；
- 有什么输出；
- 是否阻塞；
- 是否需要用户决策。

## 2. 底层模型坚持任务系统文档

底层不新增新的任务模型。继续坚持文档中的统一内核：

```text
Mission = 上下文与目标容器
Task = 唯一主工作对象
Worker = 执行主体
TaskPolicy = 执行策略
AssignmentLease = Worker 与 Task 的临时绑定
Evidence = 完成依据
Checkpoint = 长任务恢复点
Decision Task = 必要用户决策点
```

禁止新增：

- TeamTask；
- TeamMode；
- Todo 领域模型；
- AgentPlan；
- 独立团队队列；
- 和 Task Graph 平行的执行系统。

所有执行进度都必须回到 Task。

## 3. 总体数据流

最终数据流应该是：

```text
用户输入
  ↓
InputArea 携带 deepTask
  ↓
后端归一化为 Mission / Objective Task
  ↓
根据 deepTask 生成 TaskPolicy
  ↓
生成 Task Graph
  ↓
Runner 计算 Ready 叶子任务
  ↓
WorkerCatalog 匹配 worker
  ↓
AssignmentLease 绑定 worker 与 task
  ↓
Worker 执行
  ↓
写入 output_refs / evidence_refs
  ↓
TaskStore 推进状态
  ↓
Validation / Repair / Replan
  ↓
TaskProjection 更新
  ↓
TasksPanel / AgentTab / 文件快照 / 知识库 / 变更视图展示
  ↓
完成后生成交付包
```

这条链路必须成为唯一主线。

## 4. deepTask 的真实含义

`deepTask` 不能只是 boolean。

它应该表示：

> 当前请求是否以高自治交付策略执行。

当前涉及文件包括：

- 输入区：`web/src/components/InputArea.svelte`
- 前端提交 API：`web/src/web/agent-api.ts`
- 后端任务提交：`crates/magi-api/src/task_execution.rs`
- 任务模型：`crates/magi-core/src/task.rs`
- 任务运行器：`crates/magi-orchestrator/src/task_runner.rs`
- 任务存储：`crates/magi-orchestrator/src/task_store.rs`

## 5. 普通模式与深度模式的 TaskPolicy 差异

### 5.1 普通模式策略

普通模式建议策略：

```text
autonomy_level: assisted
approval_mode: interactive
checkpoint_mode: turn
background_allowed: false
retry_limit: 1
repair_limit: 1
validation_profile: basic / optional
```

行为：

- 优先快速处理；
- 轻量验证；
- 任务失败后可以尽快回到用户；
- 不默认长期后台推进；
- 不强制生成完整阶段图。

### 5.2 深度模式策略

深度模式建议策略：

```text
autonomy_level: autonomous
approval_mode: decision_only
checkpoint_mode: task_or_phase
background_allowed: true
retry_limit: 2-3
repair_limit: 2-3
validation_profile: required
```

升级触发条件：

```text
permission_boundary
irreversible_action
conflicting_requirements
architecture_fork
repair_budget_exhausted
missing_acceptance_criteria
unsafe_or_destructive_action
```

行为：

- 默认继续推进；
- 不按阶段频繁打断用户；
- 每个关键阶段写 checkpoint；
- Action / Validation / Repair 必须产生 evidence；
- 验证失败后自动进入 Repair；
- Repair 超预算才升级为 Decision 或 Failed。

## 6. Task Graph 生成规则

### 6.1 普通模式 Task Graph

普通模式可以生成浅图：

```text
Objective
  └─ Action
      └─ Validation 可选
```

适合小任务。

### 6.2 深度模式 Task Graph

深度模式必须生成最小完备图：

```text
Objective
  ├─ Phase
  │   ├─ WorkPackage
  │   │   ├─ Action
  │   │   └─ Action
  │   └─ Validation
  ├─ Phase
  │   ├─ WorkPackage
  │   │   └─ Action
  │   └─ Validation
  └─ Final Validation
```

但不要一开始生成过度详细的假计划。

正确方式是：

```text
先生成稳定骨架
执行中逐步细化未完成子树
已完成任务不可重写
未完成任务可以 replan
必要时派生新 Objective
```

这符合任务系统文档中的图反思与重规划方向。

## 7. Worker / Agent 分工规则

团队分工由 Task Graph 和 WorkerCatalog 自动决定。

参考关系：

```text
Objective / Phase    → architect
WorkPackage / Action → integration-dev / frontend-dev / backend-dev
Validation           → reviewer
Repair               → debugger
Decision             → 用户决策 / 系统等待
```

当前相关文件：

- Worker 角色目录：`crates/magi-orchestrator/src/task_worker_catalog.rs`
- Runner 调度：`crates/magi-orchestrator/src/task_runner.rs`

用户不需要选择 agent 数量，也不需要手动组队。

用户只管理：

- 目标；
- 边界；
- 验收标准；
- 必要决策。

系统管理：

- 拆解；
- 分工；
- 执行；
- 验证；
- 修复；
- 重规划。

## 8. 深度模式下用户中途输入处理

深度模式运行中，用户继续发送消息时，不能直接当普通聊天追加给当前 agent。

必须经过 Intake 分类。

### 8.1 输入类型

| 用户输入类型 | 系统处理 |
|---|---|
| 补充上下文 | 写入 Mission context 或当前 Task input_refs |
| 修改目标 | 触发 replan，只重规划未完成子树 |
| 新增相关任务 | 当前 Objective 下追加 WorkPackage / Action |
| 新增后续目标 | 同一 Mission 下创建新 Objective |
| 回答系统问题 | resolve 当前 Decision Task |
| 暂停 / 停止 | pause root task tree，保留 checkpoint |
| 无关新任务 | 新建 Objective 或新 Mission，不污染当前执行树 |
| 高风险授权 | 创建 Decision Task 等待确认 |

### 8.2 输入处理链路

```text
用户中途输入
  ↓
Intake 分类
  ↓
判断影响范围
  ↓
写入 Mission / TaskGraph / Decision / Replan
  ↓
Runner 继续推进
```

禁止：

```text
用户中途输入 → 直接塞给当前 worker
```

否则会破坏任务图和多 worker 协作。

## 9. UI 接线方案

### 9.1 InputArea

当前深度开关继续保留。

它只表达：

> 是否以高自治交付策略执行。

不新增：

- 团队模式开关；
- agent 数量选择；
- 手动角色选择；
- 编排方式选择。

当前相关文件：

- `web/src/components/InputArea.svelte`

### 9.2 TasksPanel：交付总览

TasksPanel 是深度模式下的主观察面板。

它不应该只是任务树，而应该先展示交付总览。

当前相关文件：

- `web/src/components/TasksPanel.svelte`
- `web/src/stores/task-graph-store.svelte.ts`
- 后端 projection API：`crates/magi-api/src/routes/tasks_graph.rs`

建议顶部总览字段：

```text
当前目标
当前模式：普通 / 深度
当前状态
当前阶段
总进度
正在执行的任务
阻塞任务
待用户决策
最近输出
最近 Evidence
验证摘要
暂停 / 继续 / 重规划入口
```

任务树继续保留，但变成详情层。

用户第一眼应该知道：

> 当前目标推进到哪里了，系统是否还在正确推进，是否需要我处理。

### 9.3 AgentTab：团队可观察视图

AgentTab 不再只是消息流。

当前相关文件：

- `web/src/components/AgentTab.svelte`

后续应展示每个 worker 当前绑定的 task：

```text
Agent 角色
当前 Task
Task goal
Task status
所属 Phase / WorkPackage
最近输出
最近 Evidence
是否阻塞
下一步动作
```

AgentTab 的产品定义：

> 看团队成员分工与产出。

不是：

> 进入团队模式。

### 9.4 文件快照 / 变更视图

文件快照和变更视图必须挂到 Evidence。

深度模式下用户需要知道：

- 哪个 task 改了哪些文件；
- 哪个阶段产生了哪些 diff；
- 验证基于哪个快照；
- repair 修改了什么；
- 最终交付包包含哪些变更。

这部分数据应通过：

```text
Task.output_refs
Task.evidence_refs
Mission evidence summary
```

串起来，而不是另建一套记录。

### 9.5 知识库 / 上下文引擎

知识库和统一上下文引擎服务 Mission，不是独立流程。

应该接入：

```text
Mission.context_refs
Task.context_refs
Task.knowledge_refs
```

深度模式下：

- 规划阶段读取相关知识；
- 执行阶段沉淀关键发现；
- 验证阶段引用上下文；
- replan 复用 Mission context。

## 10. Runner 运行闭环

深度模式的本质不是 UI，而是 Runner 持续推进。

运行闭环：

```text
1. 加载 Mission / Task Graph / Checkpoint
2. 重建父节点聚合状态
3. 计算 Ready 叶子任务
4. 匹配 Worker
5. 检查并发冲突与权限边界
6. 创建 AssignmentLease
7. 派发 Worker
8. 接收 output_refs / evidence_refs
9. 推进 TaskStatus
10. 执行 Validation
11. 失败则进入 Repair
12. Repair 超预算则 Decision / Failed
13. 必要时 Replan 未完成子树
14. 写 Checkpoint
15. 继续下一轮
```

停止条件：

```text
所有任务完成
用户暂停
用户取消
需要用户 Decision
repair / retry 预算耗尽
无可推进任务且不能自动 replan
```

## 11. Evidence 规则

深度模式是否可信，取决于 evidence。

建议规则：

```text
Action 完成必须有 output_refs 或 evidence_refs
Validation 完成必须有 evidence_refs
Repair 完成必须说明修复对象和验证结果
Objective 完成必须聚合子任务 evidence
Final Validation 必须生成最终验收 evidence
```

普通模式可以宽松一点。

深度模式必须严格。

否则会出现：

> UI 显示完成，但用户不知道凭什么完成。

## 12. 最终交付包

深度模式完成后，不能只输出“完成了”。

必须生成交付包。

交付包内容：

```text
目标
范围
非目标，如有
完成阶段
主要文件变更
关键实现说明
验证命令
验证结果
失败与修复记录
关键决策
Evidence 列表
遗留风险
后续建议
```

第一版可以不做成新文档。

可以先由：

```text
TaskProjection summary
+ final assistant response
+ evidence_refs
+ snapshot/diff refs
```

组合出来。

后续再产品化为“交付报告”。

## 13. API / 后端能力缺口

当前已有基础：

- 任务图 API：`crates/magi-api/src/routes/tasks_graph.rs`
- 任务中断 API：`crates/magi-api/src/routes/tasks_interaction.rs`
- 任务模型：`crates/magi-core/src/task.rs`
- 任务存储：`crates/magi-orchestrator/src/task_store.rs`
- Runner：`crates/magi-orchestrator/src/task_runner.rs`
- Worker catalog：`crates/magi-orchestrator/src/task_worker_catalog.rs`

需要补齐：

1. `deepTask → TaskPolicy` 映射；
2. 深度模式 Task Graph 生成；
3. Runner 根据 policy 调整持续推进、repair、validation、checkpoint；
4. evidence 完成约束；
5. worker-task lease 状态暴露给前端；
6. 深度模式中途输入 intake；
7. 最终交付包聚合。

## 14. 前端能力缺口

当前已有基础：

- 深度模式开关：`web/src/components/InputArea.svelte`
- 前端提交 API：`web/src/web/agent-api.ts`
- Task Graph store：`web/src/stores/task-graph-store.svelte.ts`
- TasksPanel：`web/src/components/TasksPanel.svelte`
- AgentTab：`web/src/components/AgentTab.svelte`

需要补齐：

1. TasksPanel 顶部交付总览；
2. AgentTab 展示 worker 当前绑定 task；
3. 深度模式运行中的用户输入状态表达；
4. pending decision 的更明确处理；
5. evidence / snapshot / diff 与任务节点的可见关联；
6. 完成后的交付包入口。

## 15. 分阶段实施计划

### Phase 1：概念与策略接线

目标：

> 让 deepTask 从 boolean 变成真正的 TaskPolicy 差异。

范围：

- 普通 / 深度概念收敛；
- 不再引入团队模式；
- 后端根据 `deepTask` 生成不同 TaskPolicy；
- TaskProjection 暴露当前执行策略；
- TasksPanel 显示当前是普通还是深度交付。

验收：

```text
用户打开深度模式提交任务后，后端生成的任务 policy 明确不同于普通模式。
```

### Phase 2：深度 Task Graph

目标：

> 让深度模式生成更完整的交付任务图。

范围：

- 普通模式保持轻量图；
- 深度模式生成 Objective / Phase / WorkPackage / Action / Validation；
- Worker role 自动匹配；
- Runner 只执行叶子任务；
- Validation 成为深度模式必备节点。

验收：

```text
深度模式任务在 TasksPanel 中能看到阶段、工作包、执行任务、验证任务。
```

### Phase 3：Runner 持续推进

目标：

> 深度模式能自动推进、验证、修复，而不是每阶段停下。

范围：

- 按 policy 控制 retry / repair；
- validation 失败自动 repair；
- repair 超预算生成 Decision 或 Failed；
- phase/task checkpoint；
- replan 未完成子树。

验收：

```text
深度模式下任务失败后，系统能自动生成修复任务并继续推进。
```

### Phase 4：TasksPanel 交付总览

目标：

> 让用户看懂深度任务推进状态。

范围：

- 顶部总览；
- 当前阶段；
- running / blocked / decision；
- 最近 evidence；
- validation summary；
- pause / resume / replan 操作。

验收：

```text
用户不看任务树细节，也能知道当前交付推进到哪里、是否需要自己处理。
```

### Phase 5：AgentTab 任务绑定视图

目标：

> 让团队可观察，但不作为模式存在。

范围：

- 展示 worker 当前 lease；
- 当前 task；
- task goal；
- task status；
- output / evidence；
- blocked reason；
- 最近产出。

验收：

```text
用户打开 AgentTab 能看到每个 agent 正在负责哪个任务，而不是只看到消息流。
```

### Phase 6：深度模式 Intake

目标：

> 用户中途输入不会污染当前执行树。

范围：

- 识别补充上下文；
- 识别修改目标；
- 识别新增子任务；
- 识别新 Objective；
- 识别 Decision answer；
- 触发 append / replan / resolve decision。

验收：

```text
深度模式运行中，用户发送“顺便加一个验证”会进入任务图，而不是直接塞给当前 agent。
```

### Phase 7：交付包

目标：

> 深度模式完成后可验收、可回看。

范围：

- 聚合 completed tasks；
- 聚合 output_refs；
- 聚合 evidence_refs；
- 聚合 validation summary；
- 聚合 snapshot / diff；
- 生成最终交付摘要。

验收：

```text
深度模式完成后，用户能看到完成范围、变更、验证结果、证据和遗留风险。
```

## 16. 明确禁止的错误方向

后续推进中禁止：

1. 新增团队模式。
   - 团队是执行形态，不是模式。
2. 新增 TeamTask / Todo / AgentPlan。
   - 所有工作对象必须回到 Task。
3. 让 AgentTab 成为新任务系统。
   - AgentTab 只能展示 worker-task 绑定状态。
4. 深度模式只做 UI。
   - 必须接入 TaskPolicy / TaskGraph / Runner。
5. 用户中途输入直接塞给当前 agent。
   - 必须先 intake 分类。
6. 无 evidence 就标记深度任务完成。
   - 深度模式必须可验收。
7. 让用户手动管理 agent。
   - 用户管理目标，系统管理团队。
8. 阶段性频繁打断用户。
   - 深度模式只在真正需要 Decision 时打断。

## 17. 第一优先级

后续真正开始实现时，第一步应该做：

> deepTask → TaskPolicy → TaskProjection 可见状态

这是最小但最关键的闭环。

原因：

- 不重开产品；
- 不新增模型；
- 直接接现有任务系统；
- 能让深度模式从“开关”变成“执行策略”；
- 后续 TaskGraph、Runner、TasksPanel、AgentTab 都能围绕它扩展。

第一阶段目标不是一次性完成深度模式，而是先把根接对。

## 18. 最终一句话版本

后续所有设计和实现都按这句话校验：

> 普通模式处理当前问题；深度模式持续交付目标；团队始终是底层编排形态，通过 AgentTab 可观察；所有执行进度、分工、验证、修复和交付都必须落到统一 Task 系统。

## 19. 补充设计

针对方案审视中发现的 5 个关键盲区（Runner 同步问题、Task Graph 生成简陋、Worker 虚拟角色、TaskPolicy 死代码、分阶段顺序），已输出补充设计文档：

→ [supplementary-design.md](supplementary-design.md)

补充设计的核心修订：

1. **Runner 异步化**：`RunnerManager::start()` 基础设施已存在，深度模式只需调用 `start()` 替代同步 `for` 循环
2. **Phase 合并**：原 Phase 1（policy 映射）+ Phase 3（Runner 持续推进）合并为 Phase 0，确保 policy 写了就有人消费
3. **Task Graph 两阶段生成**：先用规则化骨架（固定 3 Phase），后续再叠加 LLM 结构化图
4. **TaskPolicy 消费逻辑**：明确每个 policy 字段的消费位置和行为差异
5. **Worker 差异化**：第一版只做 system prompt 差异化，工具过滤和模型差异后续迭代
