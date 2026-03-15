# Deep 模式定档方案（现状、结论与后续方向）

> 定档时间：2026-03-15
>
> 参考对照：
> - https://learn.shareai.run/zh/timeline/
> - 本仓库当前实现
>
> 文档定位：
> - 本文是 deep 模式的唯一正式文档
> - 已吸收此前相关分析文档与设计稿中的有效结论
> - 参考网站仅作为交叉比对样本，不作为 Magi 的目标实现蓝图

---

## 执行摘要

**定档结论**：

当前 Magi 的 deep 模式，已经**不再适合**被定义为“standard runtime + 更大 budget + 更多 round”的单纯参数增强态。

它现在更准确的定性是：

> **一个建立在 `PlanLedger + TerminationSnapshot + Verification Pipeline + Worktree Isolation` 之上的 `deep_v1` 项目级治理运行时。**

截至本次收口，`s05 / s06 / s07 / s11 / s12` 对应的关键治理链路已完成主线落地并通过回归门禁。当前最合理的判断是：

- **已明显超过“参数增强态”的早期判断**
- **已完成 deep runtime 主干与治理闭环**
- **P0-P3 关键修复项已完成并形成单一事实源**
- **当前进入“稳定交付 + 持续优化”阶段**

因此，当前 deep 模式可以定性为：

> **“稳定可交付的 deep_v1 控制平面”，后续以体验增强与治理细化为主。**

---

## 一、文档定位与适用边界

本文统一承接了 3 类内容：

1. 参考平台时间线带来的架构基线
2. 仓库当前实现的真实落地状态
3. 后续继续推进 deep runtime 时仍然有效的工程约束

这里的“参考平台”含义必须明确：

- 它是用来做交叉比对和机制拆解的样本
- 不是要求 Magi 在产品形态上完整复刻的目标
- 我们只应吸收那些能解决当前真实问题、且不破坏 Magi 既有优势的机制

也就是说，后续关于 deep 模式的：

- 架构判断
- 升级结论
- 收口项与持续优化项
- 产品定性

都应以本文为准，不再并行维护平行版本文档或临时性方案稿。

因此，参考平台演进摘要会保留在本文中，但只作为“候选机制来源”；对当前 Magi 的判断与取舍，仍只以本文为准。

早期分析阶段的核心判断有两个局限：

1. 它写作时，deep 模式确实更接近“参数增强态”。
2. 但仓库后续已经沿着 `PlanLedger` 扩展路线完成了一轮实装，旧判断已经部分过时。

尤其是下面这些早期结论，已经不再准确：

- “deep 仍缺少运行态一等对象”
- “acceptance 仍主要依赖文本抽取”
- “review 还不是持久化状态”
- “缺少 runtime 级 wait / replan / review 状态推进”
- “必须单独新建 `MissionLedgerService` 和顶层 `DeepRuntimePhase` 才能升级”

现在更准确的路线已经直接体现在当前实现中：

- `src/orchestrator/plan-ledger/*`
- `src/orchestrator/core/mission-driven-engine.ts`
- `src/orchestrator/core/post-dispatch-verifier.ts`
- `src/workspace/worktree-manager.ts`

也就是说，**早期分析对问题识别是有价值的，但升级路径应以当前实现和本文结论为准。**

---

## 二、参考平台的作用边界是什么

Learn Claude Code 时间线的关键演进，不是“功能越加越多”，而是 runtime 抽象层级不断提升：

| 阶段 | 核心能力 | 对 deep 模式最相关的含义 |
|---|---|---|
| `s05` | `load_skill` / 两级注入 | 知识按需加载，而不是预先塞满 system prompt |
| `s06` | `micro-compact + auto-compact + archival` | 长会话必须有系统级上下文压缩 |
| `s07` | 文件持久化任务图 | 任务状态活在对话外面 |
| `s11` | `idle-poll-claim-work` | Worker 可以自组织认领任务 |
| `s12` | `git worktree` 目录隔离 | 并发写冲突从文件系统层面解决 |

补充说明：

- `s01-s04` 解释的是最小 Agent loop、工具扩展、todo 和记忆外化、一次性子 Agent，这些是背景能力，不是当前 deep 差距判断的主轴
- `s08-s10` 解释的是后台任务、团队协作、协议机制，对 Magi 有参考价值，但不是当前 deep runtime 成熟度判断的决定性指标
- 对当前 deep 模式是否“成立”，真正关键的是 `s05 / s06 / s07 / s11 / s12`

但这里有一个边界不能搞错：

> **我们不是要把 Magi 变成另一个 Claude Code，而是要借它来识别哪些机制值得吸收、哪些机制不应照搬。**

对 Magi 而言，应该保留的既有优势包括：

- Orchestrator 主导的强治理能力
- VS Code / Webview / IDE 深度集成
- 多模型并行调度与异构执行
- 结构化审计、验证与可追踪执行链

因此，用这个参考平台来衡量 Magi，不应该只问“像不像 Claude Code”，而应该问：

1. 我们是否已经有单一事实源？
2. 我们是否已经把 review / acceptance / replan 变成运行时事实？
3. 我们是否已经具备稳定的上下文治理？
4. 我们是否已经有真正的自组织调度？
5. 我们是否已经完成文件系统级隔离？

更进一步，取舍原则应该是：

1. 如果某个参考机制能直接解决 Magi 当前已知问题，就吸收其机制，不复制其外在产品形态。
2. 如果某个参考机制会削弱 Magi 的治理、可视化、审计或 IDE 集成优势，就不照搬。
3. 如果 Magi 当前方案已经更适合自身产品目标，就以现有实现为主，不为了“像参考平台”而重构。

---

## 三、当前代码的真实状态

### 3.1 已落地能力

#### A. Deep runtime 已经有正式账本，不再只是局部变量

当前 deep 模式已经不是“靠几组 runtime 变量临时拼接”。

`PlanLedger` 已经正式承载：

- `schemaVersion`
- `runtimeVersion`
- `revision`
- `runtime.acceptance`
- `runtime.review`
- `runtime.replan`
- `runtime.wait`
- `runtime.termination`

代码依据：

- `src/orchestrator/plan-ledger/types.ts`
- `src/orchestrator/plan-ledger/plan-ledger-service.ts`

这意味着：

> **deep 的治理事实源已经收敛到 `PlanLedger`，而不是继续漂浮在 prompt / 日志 / 临时变量里。**

这一步，本质上已经完成了“建立一等运行态对象”这一目标，只是实现方式不是另起 `MissionLedgerService`，而是**扩展 `PlanLedger`**。

#### B. Deep phase 已经转成“分层状态模型”，不是必须新建顶层 11 状态

当前代码已经采用“三层状态架构”：

1. `Mission.status` 负责宏观生命周期
2. `PlanRecord.runtime` 负责细粒度治理状态
3. `MissionDrivenEngine` 瞬态变量负责当前轮次工作记忆

代码依据：

- `src/orchestrator/mission/types.ts`
- `src/orchestrator/plan-ledger/types.ts`

因此，下面这条早期路线判断：

> 必须定义一个全新的 `DeepRuntimePhase` 顶层枚举作为唯一真相源

现在已经不再是最佳答案。

更合理的结论是：

> **deep phase 应该是派生视图，不应该成为第二套顶层持久化状态真相源。**

#### C. 结构化 acceptance 合同已经落地

当前 acceptance 已经是正式结构，而不是只有文本 section：

- `AcceptanceCriterion`
- `VerificationSpec`
- `verificationMethod`
- `verifiable`
- `status`

代码依据：

- `src/orchestrator/mission/types.ts`

更关键的是，它已经不只是“定义了类型”，而是进入了实际运行链路：

- plan 创建时写入结构化 acceptance
- worker review prompt 会注入 `verificationSpec`
- project verification 会读取结构化 acceptance

代码依据：

- `src/orchestrator/core/mission-driven-engine.ts`
- `src/orchestrator/worker/autonomous-worker.ts`
- `src/orchestrator/core/post-dispatch-verifier.ts`

这说明：

> **“acceptance 仍然主要依赖文本抽取”这一判断已经失效。**

#### D. review / replan / wait 已经进入 runtime 回写链路

当前 deep 模式已经能够在关键节点推进 runtime facet：

- `review: running / accepted / rejected / idle`
- `replan: none / required / applied`
- `wait: none / external_waiting`

代码依据：

- `src/orchestrator/core/mission-driven-engine.ts`
- `src/orchestrator/core/dispatch-manager.ts`
- `src/orchestrator/plan-ledger/plan-ledger-service.ts`

这意味着：

> **review / wait / replan 已经成为持久化运行态，而不是只存在于一次执行回合中。**

#### E. Project review 已经不是纯“终点闸门”

`post-dispatch-verifier` 现在已经不只是“最后跑一下 verificationRunner”。

它已经能：

- 读取结构化 acceptance
- 执行程序化 `verificationSpec`
- 将未验证项 fail-closed
- 将结果回流到 `PlanLedger.runtime`

代码依据：

- `src/orchestrator/core/post-dispatch-verifier.ts`

这说明下面这条早期判断：

> `post-dispatch-verifier` 只是后置闸门

现在也不再成立。它已经是 deep review 闭环中的正式组成部分。

#### F. s06 方向已经有真实落地

当前 Magi 已经具备 micro-compact 级别的上下文治理：

- Orchestrator 会压缩旧轮次大型 `tool_result`
- `worker_wait` 历史结果会折叠成语义占位符
- 压缩后的内容明确提示“关键信息已提取至 PlanLedger”

代码依据：

- `src/llm/adapters/orchestrator-adapter.ts`

这不是完整等价于 Learn Claude Code 的三层压缩，但已经**不是“没有等价机制”**。

#### G. s12 级别的 worktree 隔离已经落地

这部分当前是最明确的强项之一。

已经存在真实的：

- `WorktreeManager`
- `acquire -> execute in worktree -> merge -> release`
- 工具链的 worktree-aware 路径重定向
- merge conflict fail-fast

代码依据：

- `src/workspace/worktree-manager.ts`
- `src/orchestrator/core/worker-pipeline.ts`
- `src/tools/tool-manager.ts`

所以如果按参考平台做交叉比对：

> **s12 的核心思想，在 Magi 当前代码里已经是“已落地”状态。**

---

### 3.2 本轮补齐并已落地的能力

#### A. s05 动态技能加载：已收敛到严格按需注入

当前已形成“索引常驻、正文按需”的单一路径：

- `system` 上下文仅暴露技能索引与触发方式
- Skill 正文通过工具链显式获取，不再在环境上下文中展开
- Worker/Orchestrator 均通过统一知识查询入口按需拉取

代码依据：

- `src/context/environment-context-provider.ts`
- `src/tools/tool-manager.ts`
- `src/tools/knowledge-query-executor.ts`
- `src/orchestrator/prompts/orchestrator-prompts.ts`

#### B. s11 自组织调度：驻留式 `idle -> poll -> claim -> work -> idle` 已落地

当前已具备边界自治闭环：

- assignment 内空闲认领（idle claim）
- lane 驻留轮询与超时退出
- claim 幂等与抢占保护
- 与 Orchestrator 治理职责保持单一路径，不形成双真相源

代码依据：

- `src/orchestrator/worker/autonomous-worker.ts`
- `src/tools/orchestration-executor.ts`
- `src/todo/todo-manager.ts`
- `src/orchestrator/core/dispatch-manager.ts`

#### C. criterion 级验收账本：已补齐运行态持久化

当前 acceptance 合同已扩展并进入运行时链路：

- criterion 级 evidence、owner/scope、reviewHistory、batch/worker 归属
- 运行态映射与账本持久化已打通
- post-dispatch verifier 与 runtime 汇总口径一致

代码依据：

- `src/orchestrator/mission/types.ts`
- `src/orchestrator/plan-ledger/plan-ledger-service.ts`
- `src/orchestrator/core/post-dispatch-verifier.ts`

---

### 3.3 原关键缺口的收口结果

#### A. Mission-level Resume：已从 session 续跑收敛到 ledger-driven recovery

当前恢复入口已优先采用 `missionId -> ledger plan`，并具备：

- 恢复缺计划 `fail-closed`
- 恢复后 review/termination 等运行态一致回放
- 恢复链路幂等守卫（避免重复 fix todo / 重复派发）

验证入口：

- `npm run -s verify:e2e:mission-resume-guardrail`
- `npm run -s verify:e2e:plan-ledger-reconcile`

#### B. CAS / reducer / terminal sticky：已形成发布级治理闭环

当前已完成：

- `expectedRevision` CAS 写入语义
- 非法计划状态与 runtime facet 迁移拒绝
- 审计事件记录与终态 sticky 保护

验证入口：

- `npm run -s verify:e2e:plan-ledger-guardrails`
- `npm run -s verify:e2e:plan-ledger-lifecycle`

#### C. Ask + Deep Replan Gate：已产品化

当前已完成：

- budget/scope/acceptance/blocker/stalled 五类治理信号
- 命中 gate 时 ask 模式阻塞直至确认
- confirm/reject 结果持久化并可解释展示

验证入口：

- `npm run -s verify:e2e:replan-gate-ask`
- `npm run -s verify:e2e:recovery-decision-kernel`

#### D. s06 上下文治理：micro/auto/archival 已形成三层闭环

当前已完成：

- micro compact（轮次压缩）
- auto compact（自动压缩归档）
- archival transcript + `context_compact` 手动触发

验证入口：

- `npm run -s verify:e2e:context-governance`

---

## 四、既有判断的保留与修正

### 4.1 仍然成立的判断

这些判断今天依然成立：

- deep 的职责边界是对的：Orchestrator 治理，Worker 执行
- 当前 deep 不是无限循环，而是有界闭环
- deep runtime 事实源应持续保持 `PlanLedger` 单一路径
- ask 模式治理动作必须持久化且可解释
- Worker 自治必须保持有边界、可审计、可回放

### 4.2 需要修正的判断

这些判断现在已经不准确：

- “deep 还不是持久化运行态”
- “review 还不是一等持久状态”
- “acceptance 仍主要靠文本抽取”
- “wait / review / replan 没进入统一 runtime”
- “必须新建独立 `MissionLedgerService`”
- “必须另建一套顶层 `DeepRuntimePhase` 才算升级”

更准确的说法应该是：

> **当前 deep runtime 已经存在，只是实现为 `Mission.status + PlanRecord.runtime + 瞬态变量` 的分层模型，而不是独立的新账本和新顶层状态机。**

---

## 五、当前最合理的产品定性

综合交叉比对结果与当前代码，我认为当前 deep 模式的最准确定性是：

> **Magi 当前 deep 模式 = 已落地的 `deep_v1` 项目级治理控制平面。**

它已经具备：

- 单一账本基础
- 结构化 acceptance
- review / replan / wait facet
- project verification 闭环
- worktree 物理隔离
- 驻留式自治 claim 生命周期
- 按需技能 / 项目知识加载能力

因此：

> **当前 deep 模式已具备稳定交付能力，属于可发布的 `deep_v1` 治理控制平面；后续工作以持续优化为主，不再属于阻断项。**

---

## 六、持续优化的 4 个方向（非阻断交付）

### P0. Mission-level Resume

目标：

- 增强跨版本与长周期任务的恢复稳定性
- 强化恢复可观测性（诊断字段、审计事件、失败分类）
- 继续压缩非必要的 session 级恢复分支

### P1. 正式化 Ledger 并发与状态约束

目标：

- 增加 CAS / expected revision 提交语义
- 持续细化 reducer 约束覆盖面
- 持续扩展非法状态迁移拒绝策略
- 增强审计记录的检索与回放能力

### P2. Replan Gate 产品化

目标：

- 细化触发阈值分层（不同任务类型/风险等级）
- 优化用户确认体验与解释文案

### P3. 从半自治走向驻留自治

目标：

- 优化驻留自治参数（poll/timeout/claim 策略）
- 继续提升自治链路可观测性与故障自愈能力
- 在不破坏治理边界前提下减少 Orchestrator 微观介入
- 为长工具轮中的当前 todo 注意力漂移建立运行时守卫，而不是等完全自治后再补救

---

## 七、实施计划与验收标准

本节用于记录 P0-P3 的升级基线与验收口径。
当前四个主专项已完成主线收口，本节继续作为后续变更的准入与回归标准。

需要先明确两个原则：

- 业务优先级仍按 `P0 -> P1 -> P2 -> P3` 理解。
- 工程实施顺序建议按 `P1 -> P0 -> P2 -> P3` 推进，因为 `P1` 是其余三项的账本与状态约束底座。

### 7.1 P1 底座先行：Ledger 并发与状态约束

涉及模块：

- `src/orchestrator/plan-ledger/types.ts`
- `src/orchestrator/plan-ledger/plan-ledger-service.ts`
- `src/orchestrator/core/mission-driven-engine.ts`
- `src/orchestrator/core/dispatch-manager.ts`

实施要求：

- 为单 ledger 写入建立明确的 `expectedRevision` / CAS 语义。
- 为关键 runtime facet 建立统一的状态转移约束层，拒绝非法转移。
- 明确 terminal sticky 规则，进入终态后不得被普通事件回写。
- 为状态转移失败、版本冲突、非法事件建立可审计记录。
- 禁止出现“ledger 失败后退回内存事实源”的兼容路径。

完成定义：

- 并发写入冲突会被确定性拒绝，而不是静默覆盖。
- 非法状态迁移会被拒绝，并留下审计记录。
- `completed / failed / cancelled` 等终态具备稳定粘性。
- deep runtime 的关键事实源只保留 `PlanLedger` 一条主路径。

验收标准：

- 至少补充一组并发写冲突回归用例。
- 至少补充一组非法状态迁移回归用例。
- 所有相关回归与 `release:preflight` 必须通过。

### 7.2 P0 主链路补齐：Mission-level Resume

涉及模块：

- `src/orchestrator/worker/worker-session.ts`
- `src/orchestrator/core/dispatch-manager.ts`
- `src/orchestrator/core/mission-driven-engine.ts`
- `src/orchestrator/plan-ledger/types.ts`
- `src/orchestrator/plan-ledger/plan-ledger-service.ts`

实施要求：

- 恢复入口应以 ledger 中的 runtime 快照为主，而不是以 `resumePrompt` 拼接为主。
- 恢复后必须能重建 `review / replan / wait / acceptance / termination` 等关键状态。
- 中断恢复后不得重复创建同一类 fix todo、重复派发同一 assignment，或破坏既有终态。
- 恢复逻辑必须兼容当前 deep 主链路，而不是引入第二套 mission 真相源。

完成定义：

- 进程中断后可基于 ledger 恢复 mission，而不是依赖人工补 prompt。
- 恢复后 deep phase、review round、acceptance 状态保持一致。
- ask / deep 两种交互路径在恢复后行为一致且可解释。
- 恢复失败时进入受控失败或暂停状态，而不是静默跳过。

验收标准：

- 至少补充一组 mission 中断恢复回归用例。
- 至少覆盖“恢复后继续执行”“恢复后进入 review”“恢复后终态保持”三类场景。
- 所有相关回归与 `release:preflight` 必须通过。

### 7.3 P2 用户治理补齐：Replan Gate 产品化

涉及模块：

- `src/orchestrator/core/mission-driven-engine.ts`
- `src/orchestrator/plan-ledger/types.ts`
- `src/orchestrator/plan-ledger/plan-ledger-service.ts`
- `src/llm/adapters/orchestrator-adapter.ts`
- 用户确认与展示链路

实施要求：

- 明确定义触发 replan gate 的规则，例如 review round 超阈值、范围膨胀、预算跨阈值、重大方案漂移。
- 将 `replan.reason`、风险等级、是否需要确认、确认结果收敛为正式 runtime 数据。
- `ask + deep` 下命中 gate 时必须阻塞继续执行，直到用户确认。
- 用户确认结果必须进入持久化链路，并可在 UI 或消息层解释当前暂停原因。

完成定义：

- 重大 replan 不会在 ask 模式下静默继续。
- 用户能看到“为什么被要求确认”和“确认后会发生什么”。
- replan 决策从 prompt 语义提升为产品级治理动作。
- 自动路径与确认路径不会形成双真相源。

验收标准：

- 至少补充一组 ask 模式下的 replan confirm / reject 回归用例。
- 至少覆盖“范围膨胀”“预算超阈值”“review 超阈值”三类触发源中的两类。
- 所有相关回归与 `release:preflight` 必须通过。

### 7.4 P3 自治增强：从半自治到驻留自治

涉及模块：

- `src/orchestrator/worker/autonomous-worker.ts`
- `src/todo/todo-manager.ts`
- `src/tools/orchestration-executor.ts`
- `src/orchestrator/profile/builtin/worker-personas.ts`
- `src/orchestrator/core/mission-driven-engine.ts`

实施要求：

- 为 Worker 建立有边界的 `idle -> poll -> claim -> work -> idle` 生命周期。
- claim 过程必须具备幂等与抢占保护，避免重复认领同一 todo。
- Worker 的认领能力应受角色、能力或任务类型约束，不能无边界吞任务。
- Orchestrator 应逐步从“逐个子任务微观驱动”退到“计划、治理、验收”主职责。
- 迁移过程中不得让同一 todo 同时存在两条认领真相路径。
- 一级 Todo 必须继续由编排层唯一创建；Worker 只允许在当前 Todo 明显包含多个独立可验证子目标时再拆分二级/三级 Todo。
- `todo_split` 生成的子 Todo 必须携带结构化 `expectedOutput`，若已知目标文件则同时携带 `targetFiles`；禁止把连续动作机械切碎成大量子步骤。
- Todo 必须记录来源（如 `planner_macro`、`worker_split`、`orchestrator_adjustment`、`review_fix`、`system_repair`），并在 UI 中对用户可见。
- `todo_claim_next` 必须受上下文亲和度约束：只允许续领同一 Assignment 或共享目标文件的 Todo；当不存在足够亲和的候选时，必须 fail-closed 并交回编排层调度。

完成定义：

- Worker 在完成当前 todo 后可在边界内继续认领后续 todo。
- claim 行为可解释、可审计、可回放。
- 空闲状态有明确退出条件，不形成无限驻留或失控轮询。
- 自治增强不会破坏现有 deep 的可追踪性和治理能力。

验收标准：

- 至少补充一组 claim 幂等回归用例。
- 至少补充一组 idle 后自动认领与超时退出回归用例。
- 所有相关回归与 `release:preflight` 必须通过。

补充机制说明：

- 参考 `Learn Claude Code s03 TodoWrite` 的 `nag reminder` 思路是有价值的，但 Magi 不照搬“连续 N 轮不调用 todo 就提醒更新 todo”。
- 当前 Magi 的 todo 真相源仍在 `TodoManager + AutonomousWorker + Orchestrator`，不是让 Worker 模型自行维护一份计划副本。
- 但随着 `todo_claim_next`、idle poll/claim、lane resident 等半自治链路进入主路径，Worker 在单个 todo 内发生注意力漂移的风险已经开始上升。
- 因此更合理的产品化方案是引入 `Worker Attention Guard`：
  - 现阶段先落 `Current Todo Focus Reminder`，在连续多轮只读探索或连续多轮无实质输出时，内部注入“回到当前 todo / 预期输出 / 目标文件”的聚焦提醒。
  - 后续自治进一步增强后，再补 `Todo Boundary Reminder`，用于提醒 `todo_split / todo_claim_next` 等 todo 边界推进动作。
- 该机制应挂在 Worker 运行时循环的决策点，而不是放进 `TodoManager` 或写死到 prompt 模板中。
- Todo 分层本身也应继续遵守“编排控宏观、Worker 控微观”的产品边界：编排层负责一级承诺与治理事实，Worker 负责执行期必要拆分，但新增 Todo 必须是结构化、可审计、可被用户看懂的。

### 7.5 统一发布门禁

无论 P0-P3 推进到哪一步，发布前都必须满足以下统一门禁：

- 不引入第二套 mission / runtime 真相源。
- 不引入“失败时退回旧路径”的回退逻辑。
- 不引入“仅为通过当前问题而存在”的补丁式旁路。
- 新增能力必须绑定最少一条专属回归用例。
- 文档、实现、回归结果三者口径一致，变更后同步回写本文。

### 7.6 主专项之外的完整升级/调整范围

除 P0-P3 四个主专项外，当前仍应纳入 deep 升级范围的内容如下。

| 项目 | 当前状态 | 分类 | 处理建议 | 关联模块 |
|---|---|---|---|---|
| 严格按需技能注入 | 已落地（本轮） | 持续优化 | 环境上下文已移除 Skill 正文展开开关，统一为“只暴露索引 + `/skill-name` 显式触发正文” | `src/tools/tool-manager.ts`、`src/tools/knowledge-query-executor.ts`、`src/context/environment-context-provider.ts`、`src/orchestrator/prompts/*` |
| 三层上下文治理 | 已落地（本轮） | 持续优化 | 已补齐 `auto compact + archival transcript + context_compact(manual)`，形成可审计压缩治理闭环 | `src/context/context-manager.ts`、`src/context/context-auxiliary.ts`、`src/tools/orchestration-executor.ts` |
| criterion 级验收账本 | 已落地（本轮） | 持续优化 | acceptance criterion 已补齐 evidence、owner/scope、review history、batch/worker 归属并进入 runtime 持久化，可继续增强诊断呈现 | `src/orchestrator/mission/types.ts`、`src/orchestrator/plan-ledger/*`、`src/orchestrator/core/post-dispatch-verifier.ts` |
| 决策轨迹与高级诊断面 | 已落地（本轮） | 持续优化 | 已接入 `orchestratorRuntimeDiagnostics` 数据通道并在 Thread 侧展示 decisionTrace + snapshot，可继续增强交互筛选 | `src/adapters/adapter-factory-interface.ts`、`src/orchestrator/core/mission-driven-engine.ts`、`src/orchestrator/core/termination-metrics-repository.ts`、UI 链路 |
| blocker / progress / acceptance 联动治理 | 已落地（本轮） | 持续优化 | 已将预算/范围/验收失败/阻塞/停滞信号统一收敛为治理信号，并进入 replan reason 与 ask 确认解释链路 | `src/orchestrator/core/recovery-decision-kernel.ts`、`src/orchestrator/core/mission-driven-engine.ts` |
| 统一恢复决策内核 | 已落地（本轮） | 持续优化 | 已新增 recovery decision kernel，统一裁决 `auto_repair/auto_governance_resume/ask_followup_confirmation/auto_followup/pause`，主循环改为“决策→动作”单链路 | `src/orchestrator/core/recovery-decision-kernel.ts`、`src/orchestrator/core/mission-driven-engine.ts` |
| worktree 运行保障 | 已落地（本轮） | 持续优化 | 已补 merge 冲突结构化解释、孤儿 worktree/分支对账清理、冲突后修复微任务入口 | `src/workspace/worktree-manager.ts`、`src/orchestrator/core/worker-pipeline.ts` |
| todo_claim_next 上下文亲和度约束 | 已落地（本轮） | 持续优化 | 已收紧 claim 规则：仅允许续领同 Assignment 或共享目标文件的 Todo；不再允许跨 Worker、跨上下文自动跳任务 | `src/todo/todo-manager.ts`、`src/orchestrator/core/claim-next-todo-affinity.ts`、`src/orchestrator/core/dispatch-manager.ts` |
| P3-Worker Attention Guard | 已落地（本轮） | 持续优化 | 已新增当前 todo 聚焦提醒：连续多轮只读探索/无实质输出时，在 Worker 决策点内部注入聚焦提示，不照搬 s03 的“提醒更新 todo”语义 | `src/orchestrator/worker/todo-attention-guard.ts`、`src/orchestrator/worker/autonomous-worker.ts`、`src/llm/adapters/worker-adapter.ts` |
| Todo 分层与来源治理 | 已落地（本轮） | 持续优化 | 已将 Todo 来源收敛为统一结构字段，并收紧 `todo_split` 子任务契约（`expectedOutput` / `targetFiles` / 拆分规模）；任务面板可区分规划/拆分/调整/验收/修复来源 | `src/todo/types.ts`、`src/tools/orchestration-executor.ts`、`src/orchestrator/core/dispatch-manager.ts`、`src/task/task-view-adapter.ts`、UI 链路 |
| 运行态解释与 UI | 已落地（本轮） | 持续优化 | 已展示运行态 reason/finalStatus/snapshot/decisionTrace，可继续补筛选、对比与导出能力 | Webview、消息模型、i18n、运行态透传链路 |
| schema / migration / version 治理 | 已落地（本轮） | 持续优化 | `PlanLedgerService` 已明确 N/N-1 窗口、在线迁移回写与不受支持版本 fail-closed 边界，并补迁移回归 | `src/orchestrator/plan-ledger/plan-ledger-service.ts`、`scripts/e2e-plan-ledger-guardrails.cjs`、发布脚本 |
| 回归与 CI 闸门扩充 | 已落地（本轮） | 持续优化 | 新增 `context-governance` 专项回归并接入 `release:preflight`，其余专项按变更持续扩展 | `package.json`、`scripts/*`、`.github/workflows/*` |
| 文档与变更治理 | 已落地（持续执行） | 持续治理 | 实现变化后同步修订本文与版本说明，避免再次出现平行方案文档 | `docs/*`、版本说明文档 |

### 7.7 明确不做或暂不做的调整

为了保证升级稳定性，以下方向应明确视为“不做”或“暂不做”：

- 不新建独立的 `MissionLedgerService` 或第二套 mission 真相源。
- 不新建一套顶层 `DeepRuntimePhase` 作为新的持久化真相源。
- 不在 ledger 或 runtime 失败时退回“内存事实源 + prompt 兜底”的兼容路径。
- 不为了模仿参考平台而削弱 Orchestrator 的治理职责、IDE 集成或结构化审计链。
- 不让 in-flight mission 在运行中热切换 `runtimeVersion` 或控制路径。
- 不在 worktree 已经成为主路径后重新依赖串行降级来规避并发写冲突。
- 不让 Worker 进入无边界自治，导致 claim 范围失控、角色约束失效或治理链断裂。

### 7.8 建议回归与发布清单

当 deep 升级涉及不同子系统时，建议至少按下面的维度执行回归。

基础必跑：

- `npm run -s compile`
- `npm run -s release:preflight`

账本与生命周期相关：

- `npm run -s verify:e2e:plan-ledger-lifecycle`
- `npm run -s verify:e2e:plan-ledger-attempt-lifecycle`
- `npm run -s verify:e2e:plan-ledger-guardrails`
- `npm run -s verify:e2e:mission-resume-guardrail`
- `npm run -s verify:e2e:criterion-ledger-runtime`
- `npm run -s verify:e2e:plan-governance-gate`

派发与自治链路相关：

- `npm run -s verify:e2e:dispatch-protocol`
- `npm run -s verify:e2e:dispatch-idempotency`
- `npm run -s verify:e2e:auto-deep-followup`
- `npm run -s verify:e2e:replan-gate-ask`
- `npm run -s verify:e2e:worker-idle-claim`
- `npm run -s verify:e2e:claim-next-todo-affinity`
- `npm run -s verify:e2e:worker-attention-guard`
- `npm run -s verify:e2e:todo-structure-guardrails`

模式、治理与终止相关：

- `npm run -s verify:e2e:mode-governance`
- `npm run -s verify:e2e:orchestrator-termination`
- `npm run -s verify:e2e:recovery-decision-kernel`
- `npm run -s verify:e2e:worktree-runtime-guardrails`
- `npm run -s verify:e2e:termination-ab-gate`
- `npm run -s verify:e2e:termination-real-sample-gate`
- `npm run -s verify:ci:termination-gate`

知识与技能链路相关：

- `npm run -s verify:e2e:apply-skill`
- `npm run -s verify:e2e:knowledge-learning`
- `npm run -s verify:e2e:context-governance`

补充要求：

- 若本次变更命中了某一专项的“验收标准”条目，应额外执行该专项绑定的专属回归。
- 若本次变更影响发布门禁、终止治理或运行态消息结构，必须同步检查对应 workflow 与脚本文档。

### 7.9 全量修复项落地状态（用于后续验证）

状态定义：

- `已完成`：代码与回归均已落地，可直接验收。
- `部分完成`：主链路已落地，但仍缺专项回归或子场景。
- `未完成`：尚未进入实装或缺少可验收实现。

当前版本状态：

- 本清单已无 `部分完成` 或 `未完成` 条目，均已进入“已完成/已落地（持续执行）”状态。

> 下面清单按“文档中所有需要修复的条目”逐项标记，作为后续验证基线。

| 条目 | 状态 | 本轮落地说明 | 验证入口 |
|---|---|---|---|
| P1-CAS 提交语义 | 已完成 | `PlanMutationOptions.expectedRevision` 已接入关键写接口并启用冲突拒绝 | `npm run -s verify:e2e:plan-ledger-guardrails` |
| P1-非法状态迁移拒绝与审计 | 已完成 | plan status / runtime facet 迁移约束已统一，拒绝并记录 `audit:*` 事件 | `npm run -s verify:e2e:plan-ledger-guardrails` |
| P1-terminal sticky | 已完成 | 终态保护与对账链路已保持终态粘性 | `npm run -s verify:e2e:plan-ledger-lifecycle`、`npm run -s verify:e2e:plan-ledger-reconcile` |
| P1-禁止失败回退旧事实源 | 已完成 | mission resume 缺计划时改为 fail-closed，不再回退新建草案 | `npm run -s verify:e2e:mission-resume-guardrail` |
| P0-ledger-driven mission recovery 入口 | 已完成 | `execute` 已优先按 `missionId -> ledger plan` 恢复 | `npm run -s verify:e2e:mission-resume-guardrail` |
| P0-恢复失败进入受控失败 | 已完成 | 恢复计划缺失会中止恢复并返回受控错误 | `npm run -s verify:e2e:mission-resume-guardrail` |
| P0-恢复后继续执行/进入 review/终态保持三场景 | 已完成（本轮） | mission resume 回归已补 `review path + terminal sticky` 守卫，恢复后状态推进与终态保持可验收 | `npm run -s verify:e2e:mission-resume-guardrail`、`npm run -s verify:e2e:plan-ledger-reconcile` |
| P0-避免重复 fix todo/重复 assignment 派发 | 已完成（本轮） | 恢复链路已纳入 dispatch 幂等重放阻断守卫，重复派发有专项回归覆盖 | `npm run -s verify:e2e:mission-resume-guardrail`、`npm run -s verify:e2e:dispatch-idempotency` |
| P2-replan gate 触发规则产品化 | 已完成（本轮） | 已补齐 `budget_pressure`、`scope_expansion` 信号并统一进入结构化 `replan.reason` | `npm run -s verify:e2e:replan-gate-ask` |
| P2-confirm/reject 结果持久化 | 已完成 | `awaiting_confirmation -> applied/required` 已回写 runtime.replan | `npm run -s verify:e2e:auto-deep-followup` |
| P2-ask+deep 阻塞直至确认 | 已完成（本轮） | ask+deep 命中门禁时改为真实确认阻塞，确认后再继续/停止 | `npm run -s verify:e2e:replan-gate-ask` |
| P2-用户可解释展示链路 | 已完成（本轮） | `deliveryRepairRequest` 新增 `requestType`，前端区分“交付修复”与“续跑门禁确认” | `npm run -s verify:e2e:replan-gate-ask` |
| P3-idle->poll->claim->work->idle 生命周期 | 已完成（本轮） | Worker assignment 内 idle claim + DispatchManager lane 驻留轮询已形成完整自治闭环，支持跨 assignment 连续执行 | `npm run -s verify:e2e:worker-idle-claim` |
| P3-claim 幂等与抢占保护 | 已完成 | Todo 认领 CAS 与派发幂等保护已落地 | `npm run -s verify:e2e:dispatch-idempotency` |
| P3-todo_claim_next 上下文亲和度约束 | 已完成（本轮） | `todo_claim_next` 已增加 Worker 边界硬过滤与上下文亲和度选择，仅续领同 Assignment 或共享目标文件的 Todo；无亲和候选时 fail-closed | `npm run -s verify:e2e:claim-next-todo-affinity` |
| P3-idle 自动认领与超时退出 | 已完成（本轮） | 已落地 assignment idle claim + lane resident 双层 timeout/poll 配置（`MAGI_WORKER_IDLE_CLAIM_*`、`MAGI_WORKER_LANE_RESIDENT_*`） | `npm run -s verify:e2e:worker-idle-claim` |
| P3-当前 todo 注意力守卫 | 已完成（本轮） | 已新增 `Worker Attention Guard`，连续多轮只读探索或无实质输出时自动注入当前 todo 聚焦提醒；当前实现不把“更新 todo”职责错误下放给模型 | `npm run -s verify:e2e:worker-attention-guard` |
| P3-Todo 分层与来源治理 | 已完成（本轮） | 已将 Todo 来源作为统一事实字段写入主链路，并收紧 `todo_split` 结构化要求、拆分规模与 UI 来源标识，避免 Worker 自治拆分演化成不可解释的任务膨胀 | `npm run -s verify:e2e:todo-structure-guardrails` |
| 7.6-严格按需技能注入 | 已完成（本轮） | 环境上下文已移除 Skill 正文展开路径，仅保留索引提示与 `/skill-name` 显式触发正文 | `npm run -s verify:e2e:apply-skill` |
| 7.6-三层上下文治理（micro/auto/archival） | 已完成（本轮） | 已落地 auto compact、压缩归档 `memory-archival.jsonl`、`context_compact` 手动触发入口，并补专项回归 | `npm run -s verify:e2e:context-governance` |
| 7.6-criterion 级验收账本增强 | 已完成（本轮） | `AcceptanceCriterion` 新增 evidence/owner/scope/reviewHistory/batch/worker 元信息，并完成 runtime 映射与持久化 | `npm run -s verify:e2e:criterion-ledger-runtime` |
| 7.6-criterion 去重策略作用域化 | 已完成（本轮） | criterion 去重改为 `id` 优先、无 id 时按 `(description, scope, owner)` 组合键，避免同文案跨 scope 被误去重 | `npm run -s verify:e2e:criterion-ledger-runtime` |
| 7.6-决策轨迹诊断面（前台消费） | 已完成（本轮） | 新增 `orchestratorRuntimeDiagnostics` 通道并在 Thread 侧消费 `decisionTrace` / `runtimeSnapshot` | `npm run -s compile`、`npm run -s release:preflight` |
| 7.6-blocker/progress/acceptance 联动治理 | 已完成（本轮） | replan gate 信号已统一扩展为 budget/scope/acceptance/blocker/stalled 五类治理事实，并贯通 ask 确认解释与持久化 reason | `npm run -s verify:e2e:recovery-decision-kernel`、`npm run -s verify:e2e:replan-gate-ask` |
| 7.6-统一恢复决策内核 | 已完成（本轮） | 已新增 `recovery-decision-kernel` 统一恢复动作裁决，`mission-driven` 主循环改为单一决策入口驱动 | `npm run -s verify:e2e:recovery-decision-kernel`、`npm run -s verify:e2e:auto-deep-followup` |
| 7.6-worktree 运行保障增强 | 已完成（本轮） | 已实现 merge 冲突摘要+修复建议、孤儿 worktree/分支清理、冲突后 fix todo 微任务补偿 | `npm run -s verify:e2e:worktree-runtime-guardrails`、`npm run -s verify:e2e:dispatch-protocol` |
| 7.6-worktree 写任务隔离 fail-closed | 已完成（本轮） | 写任务在缺少可用隔离 worktree 时立即失败，不再回退共享 workspace 执行，消除并发写污染窗口 | `npm run -s verify:e2e:worktree-runtime-guardrails` |
| 7.6-事件链路 mission 维度账本回写 | 已完成（本轮） | PlanLedger 事件回写改为按事件 `missionId` 定位 `sessionId/planId`，不再依赖全局 `currentSessionId/currentPlanId` | `npm run -s verify:e2e:plan-ledger-guardrails`、`npm run -s verify:e2e:dispatch-idempotency` |
| 7.6-关键守卫脚本行为化 | 已完成（本轮） | `mission-resume`、`worker-idle-claim` 回归从静态 includes 升级为“运行时行为 + 结构守卫”组合门禁 | `npm run -s verify:e2e:mission-resume-guardrail`、`npm run -s verify:e2e:worker-idle-claim` |
| 7.6-deep 自动续跑门禁收紧 | 已完成（本轮） | 自动续跑仅对“未完成 required todo 或结构化 `runtime.nextSteps` 任务步骤”生效；已移除正文“下一步建议”文本解析，非任务对话/能力描述不再触发续跑；用户侧输出改为简洁摘要，降低信息噪声 | `npm run -s verify:e2e:auto-deep-followup` |
| 7.6-运行态解释与 UI | 已完成（本轮） | 前台新增统一运行态诊断面，展示 `runtimeReason/finalStatus/snapshot/decisionTrace`；主对话区 `system_section` 多行输出默认折叠，降低信息噪声 | `npm run -s compile`、`npm run -s release:preflight` |
| 7.6-schema/migration/version 治理 | 已完成（本轮） | 已落地 schema N/N-1 兼容窗口、在线迁移自动回写、迁移事件记录与不受支持版本 fail-closed，迁移专项回归已补齐 | `npm run -s verify:e2e:plan-ledger-guardrails` |
| 7.6-回归与 CI 闸门扩充 | 已完成（本轮） | 新增 `plan-ledger-guardrails`、`mission-resume-guardrail`、`replan-gate-ask`、`criterion-ledger-runtime`、`worker-idle-claim`、`context-governance`、`recovery-decision-kernel`、`worktree-runtime-guardrails` 并接入 `release:preflight` | `npm run -s release:preflight` |
| 7.6-文档与变更治理 | 已落地（持续执行） | 本文已作为唯一主文档持续回写状态与验证入口，并纳入发布前复核清单 | 文档评审 |

---

## 八、定档结论

本文的最终定档结论如下：

> **当前 Magi deep 模式已经完成了一轮真正的 runtime 升级，主干方向正确，且代码层面已有真实落地。**
>
> **它已经不是“参数增强态”的早期形态，而是稳定可交付的 `deep_v1` 项目级治理控制平面。**
>
> **当前版本已完成恢复治理、上下文治理与发布门禁的关键机制收口，达到可稳定交付的升级基线；后续重点转为持续优化与体验增强。**

---

## 九、附：本次复核的代码依据与维护约束

核心依据文件：

- `src/orchestrator/mission/types.ts`
- `src/orchestrator/plan-ledger/types.ts`
- `src/orchestrator/plan-ledger/plan-ledger-service.ts`
- `src/orchestrator/core/mission-driven-engine.ts`
- `src/orchestrator/core/dispatch-manager.ts`
- `src/orchestrator/core/post-dispatch-verifier.ts`
- `src/orchestrator/worker/autonomous-worker.ts`
- `src/todo/todo-manager.ts`
- `src/tools/tool-manager.ts`
- `src/tools/orchestration-executor.ts`
- `src/workspace/worktree-manager.ts`
- `src/llm/adapters/orchestrator-adapter.ts`

本次复核同时参考了：

- Learn Claude Code Timeline：`s05 / s06 / s07 / s11 / s12`
- 当前仓库已通过的主回归链与 `release:preflight`

后续维护约束：

- 本文作为 deep 模式唯一正式文档持续更新
- 如实现发生变化，应直接修订本文，不再新增平行方案稿
- 若确需临时设计稿，应在落地后回收并合并回本文
