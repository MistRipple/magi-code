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

但它同时也**还没有**在 `s05 / s06 / s07 / s11 / s12` 这些关键维度上达到完全成熟。当前最合理的判断是：

- **已明显超过“参数增强态”的早期判断**
- **已落地 deep runtime 主干**
- **仍有 4 个关键缺口没有收口**
  - 纯按需技能注入还不彻底
  - 真正的驻留式 `idle-poll-claim-work` 还没完成
  - resume 仍以 session 恢复为主，不是 ledger-driven mission recovery
  - ledger 的 CAS / reducer 非法转移审计 / 发布约束还没正式闭环

因此，当前 deep 模式已经可以称为：

> **“可用的 deep_v1 控制平面”，而不是“所有关键机制都已收口的最终完成态”。**

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
- 未完成项
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
- `wait_for_workers` 历史结果会折叠成语义占位符
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

### 3.2 半落地能力

#### A. s05 动态技能加载：已进入可用态，但还不够“纯按需”

现在已经有：

- `apply_skill`
- `fetch_project_guidelines`
- 项目知识索引注入
- Worker/Orchestrator 可按需拉取知识

代码依据：

- `src/tools/tool-manager.ts`
- `src/tools/knowledge-query-executor.ts`
- `src/orchestrator/prompts/orchestrator-prompts.ts`

但问题在于：

- 环境上下文仍可能直接展开部分 Skill 正文
- 还没有完全收敛到“system prompt 只放索引，正文只走 tool_result”

代码依据：

- `src/context/environment-context-provider.ts`

所以这一项更准确的状态是：

> **动态技能加载已落地，但还没有完全达到“严格按需注入”的理想状态。**

#### B. s11 自组织调度：已经进入“半自治”阶段

现在 Magi 并不是完全没有 claim 模式。

已经存在：

- `claim_next_todo`
- `TodoManager.findClaimable`
- `TodoManager.tryClaim`
- Worker persona 中明确允许当前 todo 完成后继续 claim 下一个 todo

代码依据：

- `src/tools/orchestration-executor.ts`
- `src/todo/todo-manager.ts`
- `src/orchestrator/profile/builtin/worker-personas.ts`

但它仍不是一种完整成熟的驻留自治模式：

- Worker 常驻
- 空闲自动轮询任务板
- idle timeout 自治理
- 无需 Orchestrator 继续显式驱动

所以这项当前只能定义为：

> **从“纯指令分发”进入了“半自治认领”，但还没有进入真正的驻留式自治 Worker 池。**

#### C. review 闭环已成型，但 evidence / criterion 累计还偏轻

当前 acceptance 已经结构化，review 也能回写运行态。

但和理想形态相比，还缺少更重的“验收账本”维度，例如：

- criterion 级 evidence 列表
- criterion 级 owner / scope
- 每轮 review 的 criterion 演化轨迹
- 哪条 criterion 由哪个 batch / worker 满足

所以这部分目前更像：

> **合同已结构化，账本已存在，但“criterion 级审计粒度”还没完全做厚。**

---

### 3.3 尚未完成的关键缺口

#### A. Resume 仍然主要是 session 续跑，不是 mission-level recovery

当前恢复主轴仍然是：

- `WorkerSessionManager`
- `resumePrompt`
- `resumeSessionId`
- TTL + cleanup

代码依据：

- `src/orchestrator/worker/worker-session.ts`
- `src/orchestrator/core/dispatch-manager.ts`

这适合：

- 短中期失败恢复
- 当前进程上下文里的续跑

但它还不等于：

- phase-aware
- ledger-driven
- deterministic mission recovery

因此，这个结论今天依然成立：

> **当前 deep 的恢复能力仍以 session 恢复为主，还不是产品级 mission persistence。**

#### B. CAS / reducer / terminal sticky 级发布约束还没正式收口

虽然 `schemaVersion / runtimeVersion / revision` 已经落地，且 `PlanLedgerService` 对终态有保护：

- `runtimeVersion`
- `revision`
- `TERMINAL_PLAN_STATUSES`
- session queue 串行写

代码依据：

- `src/orchestrator/plan-ledger/plan-ledger-service.ts`

但更严格的发布级治理能力还没完全体现为正式能力：

- 没看到显式 `expectedRevision` CAS 提交接口
- 没看到正式的 reducer 非法转移拒绝机制
- 没看到“带审计的状态转移约束层”

所以这块必须继续保留为未完成项：

> **版本字段已落地，但并发控制与状态约束的“正式化治理层”还没有完全闭环。**

#### C. Ask 模式下的 replan gate 仍不够完整

当前 deep 的 ask 模式，仍然主要集中在前置计划确认。

现在虽然已有：

- `replan` runtime facet
- 自动修复和自动续跑的状态回写

但还缺少正式、可解释、规则化的二次确认门禁，例如：

- review round 超阈值
- 范围明显膨胀
- 预算跨阈值
- 需要重大 replan

也就是说：

> **replan 状态存在了，但“用户确认治理规则”还没有完全产品化。**

#### D. s06 的 macro-compact / archival 还没有完全等价物

现在的 micro-compact 已经有了。

但和理想的长会话治理机制相比，仍缺：

- 完整的 auto-compact 归档路径
- 明确的 archival transcript + compact summary 机制
- 用户或模型可主动触发的 compact 工具级体验

所以这块属于：

> **局部落地，未完全等价。**

---

## 四、既有判断的保留与修正

### 4.1 仍然成立的判断

这些判断今天依然成立：

- deep 的职责边界是对的：Orchestrator 治理，Worker 执行
- 当前 deep 不是无限循环，而是有界闭环
- 恢复能力仍偏 session 级，不是 mission 级
- ask 模式的中途 replan gate 仍然不足
- 真正的驻留自治 Worker 池还没建成

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
- 半自治 claim 模式
- 按需技能 / 项目知识加载能力

但它还**不具备**：

- 完整驻留式自治 Worker 池
- ledger-driven mission recovery
- 完整发布级 CAS / reducer / 状态迁移审计
- 完整三层 compact / archival 机制
- 正式产品化的 replan 二次确认门禁

因此：

> **当前 deep 模式已经不是“原型级增强模式”，但也还不是所有关键治理机制都已产品化的完整方案。**

---

## 六、最需要继续推进的 4 个方向

### P0. Mission-level Resume

目标：

- 从 `session resume` 升级为 `ledger-driven mission recovery`
- 恢复后明确 phase / review round / acceptance 状态
- 不再主要依赖 `resumePrompt + retryCount`

### P1. 正式化 Ledger 并发与状态约束

目标：

- 增加 CAS / expected revision 提交语义
- 明确 reducer
- 拒绝非法状态转移
- 做审计记录

### P2. Replan Gate 产品化

目标：

- 为 ask + deep 增加中途重大漂移确认
- 把 replan 从 runtime 事实提升为用户可解释治理动作

### P3. 从半自治走向驻留自治

目标：

- 从 `claim_next_todo` 升级到 `idle-poll-claim-work`
- 让 Worker 真的具备有限自组织能力
- 进一步减轻 Orchestrator 的微观控制负担

---

## 七、实施计划与验收标准

本节用于把上述 P0-P3 方向转成可执行的升级基线。

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

完成定义：

- Worker 在完成当前 todo 后可在边界内继续认领后续 todo。
- claim 行为可解释、可审计、可回放。
- 空闲状态有明确退出条件，不形成无限驻留或失控轮询。
- 自治增强不会破坏现有 deep 的可追踪性和治理能力。

验收标准：

- 至少补充一组 claim 幂等回归用例。
- 至少补充一组 idle 后自动认领与超时退出回归用例。
- 所有相关回归与 `release:preflight` 必须通过。

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
| 严格按需技能注入 | 半落地 | 建议调整 | 继续收敛到“system prompt 只放索引，正文只经工具注入”，清理环境上下文中的正文直注路径 | `src/tools/tool-manager.ts`、`src/tools/knowledge-query-executor.ts`、`src/context/environment-context-provider.ts`、`src/orchestrator/prompts/*` |
| 三层上下文治理 | 半落地 | 建议调整 | 在已有 micro-compact 基础上补齐 auto-compact、archival transcript、manual compact 能力 | `src/llm/adapters/orchestrator-adapter.ts`、上下文组装与消息持久化链路 |
| criterion 级验收账本 | 半落地 | 建议调整 | 为 acceptance criterion 增加 evidence、owner/scope、review history、batch/worker 归属信息 | `src/orchestrator/mission/types.ts`、`src/orchestrator/plan-ledger/*`、`src/orchestrator/core/post-dispatch-verifier.ts` |
| 决策轨迹与高级诊断面 | 后端已落地，前台未完全消费 | 建议调整 | 将 `decisionTrace`、`blockerState`、termination metrics 暴露到诊断视图或高级运行态展示 | `src/adapters/adapter-factory-interface.ts`、`src/orchestrator/core/mission-driven-engine.ts`、`src/orchestrator/core/termination-metrics-repository.ts`、UI 链路 |
| blocker / progress / acceptance 联动治理 | 部分存在 | 建议调整 | 把 blocker、review、replan、wait、acceptance failure 统一成可解释的治理事实，而不是分散原因串 | `src/llm/adapters/orchestrator-decision-engine.ts`、`src/llm/adapters/orchestrator-termination.ts`、`src/orchestrator/core/mission-driven-engine.ts` |
| 统一恢复决策内核 | 部分存在 | 建议调整 | 将 retry / switch / degrade / finalize 等恢复决策逐步并入统一决策内核，减少分散 if-else | `src/llm/adapters/orchestrator-decision-engine.ts`、`src/llm/adapters/orchestrator-adapter.ts`、`src/orchestrator/core/mission-driven-engine.ts` |
| worktree 运行保障 | 已落地，可继续增强 | 按需优化 | 强化 merge conflict 解释、孤儿 worktree 清理、失败后的修复微任务流 | `src/workspace/worktree-manager.ts`、`src/orchestrator/core/worker-pipeline.ts`、`src/tools/tool-manager.ts` |
| 运行态解释与 UI | 部分存在 | 建议调整 | 展示当前 review / replan / wait facet、暂停原因、恢复来源、验收进度与决策轨迹摘要 | Webview、消息模型、i18n、运行态透传链路 |
| schema / migration / version 治理 | 部分存在 | 建议调整 | 明确 N / N-1 兼容窗口、在线迁移策略、成功加载后回写策略与灰度边界 | `src/orchestrator/plan-ledger/plan-ledger-service.ts`、迁移脚本、发布脚本 |
| 回归与 CI 闸门扩充 | 部分存在 | 必须持续补齐 | 为每个新专项补专属回归，并将稳定后纳入 `release:preflight` 或专项 workflow | `package.json`、`scripts/*`、`.github/workflows/*` |
| 文档与变更治理 | 需要持续执行 | 必须持续补齐 | 实现变化后同步修订本文与版本说明，避免再次出现平行方案文档 | `docs/*`、版本说明文档 |

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
- `npm run -s verify:e2e:plan-governance-gate`

派发与自治链路相关：

- `npm run -s verify:e2e:dispatch-protocol`
- `npm run -s verify:e2e:dispatch-idempotency`
- `npm run -s verify:e2e:auto-deep-followup`

模式、治理与终止相关：

- `npm run -s verify:e2e:mode-governance`
- `npm run -s verify:e2e:orchestrator-termination`
- `npm run -s verify:e2e:termination-ab-gate`
- `npm run -s verify:e2e:termination-real-sample-gate`
- `npm run -s verify:ci:termination-gate`

知识与技能链路相关：

- `npm run -s verify:e2e:apply-skill`
- `npm run -s verify:e2e:knowledge-learning`

补充要求：

- 若本次变更命中了某一专项的“验收标准”条目，应额外执行该专项绑定的专属回归。
- 若本次变更影响发布门禁、终止治理或运行态消息结构，必须同步检查对应 workflow 与脚本文档。

---

## 八、定档结论

本文的最终定档结论如下：

> **当前 Magi deep 模式已经完成了一轮真正的 runtime 升级，主干方向正确，且代码层面已有真实落地。**
>
> **它已经不是“参数增强态”的早期形态，而是可用的 `deep_v1` 项目级治理控制平面。**
>
> **但它在自治调度、恢复治理、上下文治理和发布约束这些关键维度上，仍有明确的工程距离，尚未达到全部关键机制收口后的最终成熟态。**

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
