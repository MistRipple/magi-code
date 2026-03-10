# Deep 模式产品级架构分析与重构方案

## 执行摘要

**核心结论**：当前 deep 模式已具备项目级治理的产品语义，但运行时仍停留在"参数增强态"，尚未升级为"产品级项目执行状态机"。如果要达到产品级可发布标准，必须从架构层面完成一次完整升级。

---

## 一、表象分析

### 1.1 用户关注点

用户要求分析 deep 模式下编排插件的"续航能力"，具体包括：

1. 如何实现持续 loop
2. 如何实现持续 review
3. 当前实现是否合理
4. 是否有遗漏
5. 是否有更好的方案
6. 从架构层面和产品定位分析

### 1.2 当前表象

从产品表现看，deep 模式确实给人"更强续航"的感受：

- 比 standard 模式跑得更久
- 更容易进入"做完再查、查完再补、补完再收口"的循环
- 编排者更像在"持续盯进度"
- Worker 更像在"持续执行 + 自检 + 修正"

但真正的问题在于：**这种续航，到底是"架构级续航"，还是"参数放大后的续航感"？**

---

## 二、机理溯源

### 2.1 Deep 模式的真实机制

基于代码链路分析，当前 deep 模式的"续航能力"由以下几层叠加实现：

#### 第一层：模式切换层

```typescript
// src/orchestrator/core/mission-driven-engine.ts
const planMode: PlanMode = this.adapterFactory.isDeepTask() ? 'deep' : 'standard';
const shouldAskConfirmation = this.interactionMode === 'ask' && planMode === 'deep';
```

- `deepTask` 不是只改一个 UI 开关，而是进入了多个核心路径
- `ask + deep` 才启用计划确认门禁
- plan ledger 会记录 `mode = deep`

#### 第二层：Worker 侧的执行续航

```typescript
// src/orchestrator/worker/autonomous-worker.ts
if (deepTaskEnabled) {
  return { mode: 'project', maxReviewRounds: 8 };
}
return { mode: 'feature', maxReviewRounds: 2 };
```

Worker 真正的 loop 不是"无限 while 自己想多久就多久"，而是：

1. 先执行 assignment 里的 todos
2. 所有 todo 执行完后
3. 对照 `delegationBriefing` 里的 `Acceptance Criteria / 验收标准`
4. 发起一次静默验收检查
5. 如果有 gap，就创建新的 `fix` todo
6. 再继续执行
7. 直到 review round 达到上限（standard: 2, deep: 8）

**关键结论**：deep 模式不是无限循环，而是 **"todo 执行完成后，进入验收复审，再按缺口生成 fix todo 的有界闭环"**。

#### 第三层：Worker 恢复能力

```typescript
// src/orchestrator/worker/worker-session.ts
constructor(options?: { sessionTtlMs?: number; cleanupIntervalMs?: number; autoCleanup?: boolean }) {
  this.SESSION_TTL_MS = options?.sessionTtlMs ?? 30 * 60 * 1000; // 默认 30 分钟
  this.CLEANUP_INTERVAL_MS = options?.cleanupIntervalMs ?? 5 * 60 * 1000; // 默认 5 分钟
}
```

- 默认 TTL 30 分钟
- cleanup interval 5 分钟
- `markAsResumed(...)` / `getByAssignment(...)` / `resumePrompt`

**关键结论**：这是 **"短中期会话续跑"**，不是 **"长期自治任务体"**。

#### 第四层：DispatchManager 做跨轮恢复桥接

```typescript
// src/orchestrator/core/dispatch-manager.ts
const { resumeSessionId, resumePrompt } = this.getResumeContextForWorker(effectiveWorker);
```

- 激活 resume context
- 给对应 worker 注入 `resumeSessionId` 和 `resumePrompt`
- 归档后清理 resume context

**关键结论**：恢复并不是"Worker 自己神奇记住了"，而是 **编排层显式桥接恢复上下文**。

#### 第五层：编排层预算和终止门禁

```typescript
// src/llm/adapters/orchestrator-adapter.ts
private static readonly STANDARD_BUDGET = {
  maxDurationMs: 420_000,    // 7 分钟
  maxTokenUsage: 120_000,
  maxErrorRate: 0.7,
};

private static readonly DEEP_BUDGET = {
  maxDurationMs: 900_000,    // 15 分钟
  maxTokenUsage: 280_000,
  maxErrorRate: 0.8,
};
```

```typescript
// src/llm/adapter-factory.ts
if (deepTask) {
  stallConfig.maxTotalRounds = Math.max(
    stallConfig.maxTotalRounds + 20,
    Math.ceil(stallConfig.maxTotalRounds * 3)
  );
}
```

**关键结论**：deep 模式的核心哲学不是"放开跑"，而是 **"在更大预算里继续受控运行"**。

#### 第六层：编排者角色被收紧

```typescript
// src/llm/adapters/orchestrator-adapter.ts
private static readonly DEEP_MODE_ALLOWED_TOOLS = new Set([
  'dispatch_task',
  'send_worker_message',
  'wait_for_workers',
  'file_view',
  'grep_search',
  'codebase_retrieval',
  'web_search',
  'web_fetch',
  'shell',
  'get_todos',
  'update_todo',
]);
```

**关键结论**：deep 不是"让 orchestrator 更像执行者"，而是 **"让 orchestrator 更像治理者，让 Worker 更像执行者"**。

#### 第七层：编排后的整批验证

```typescript
// src/orchestrator/core/post-dispatch-verifier.ts
await currentBatch.waitForArchive();
await runPostDispatchVerification(currentBatch, this.verificationRunner, this.messageHub);
```

**关键结论**：系统实际有两层 review：
1. Worker assignment 内部 review
2. dispatch batch 结束后的编排级 verification

---

## 三、差距诊断

### 3.1 现在实现的合理部分

#### 合理点一：产品语义基本一致

如果 deep 的产品定位是 **"项目级长任务治理模式"**，那么现在这套实现是基本成立的。

#### 合理点二：职责边界大方向是对的

- **Orchestrator**：计划、调度、等待、决策、终止
- **Worker**：执行、局部自检、修复、回报

#### 合理点三：当前是"有界循环"，不是失控循环

所有关键路径都有边界：
- review round 有上限
- stall round 有上限
- budget 有上限
- external wait 有 SLA
- upstream model error 有连续阈值

### 3.2 当前实现的结构性不足

#### 不足一：deep 更像"参数增强"，不是"状态机升级"

**核心判断**：deep 虽然跨了很多模块，但它的执行内核仍然更像：

> **standard runtime + 更大预算 + 更多复审轮次 + 更严角色约束**

而不是一个真正独立的：

> **deep runtime state machine**

也就是说，目前缺少显式的一等状态，比如：
- planning
- dispatching
- worker_review
- project_verification
- replan
- external_wait
- completed
- failed

现在这些状态分散在多个局部变量和不同模块中。

#### 不足二：review 状态没有成为持久化的一等对象

现在 review 的核心状态主要是：
- `reviewRound`
- `verificationAttempted`
- `warnings`
- 动态生成的 `fix` todos

但缺少一个明确的、可持久化的 review ledger，例如：
- 哪条验收标准已满足
- 哪条未满足
- 未满足归因给哪个 worker / 哪个 batch
- 当前处于第几轮 project review
- 本轮 review 是局部 fix 还是需要 replan

现在这些信息很多是隐含在：
- `delegationBriefing` 文本
- silent verification response
- fix todo 的 reasoning
- 运行日志

这对短链路够用，对 deep 项目级续航不够强。

#### 不足三：Worker review 与 Orchestrator verification 没有统一合同

当前两层 review 分别是：

**Worker review**
- 面向 assignment
- 依据 `delegationBriefing` 中的验收标准文本
- 发现 gap 就生成 `fix` todo

**Orchestrator verification**
- 面向 batch
- 依据 `modifiedFiles`
- 调 `verificationRunner`
- 失败就抛错结束

问题在于：

> **这两层 review 不是同一个"完成合同"的两个阶段，而是两套并列机制。**

这会带来几个问题：
- Worker 通过，不代表项目级通过
- Orchestrator 失败后，也不会自然进入统一的 replan / fix loop
- batch verification 更像"最终闸门"，不是"项目级 review 闭环"

这就是一个重要遗漏：

> **项目级 review 闭环没有真正闭合。**

#### 不足四：恢复能力是"会话续跑"，不是"任务持久化"

当前 `WorkerSessionManager` 是内存 `Map` + TTL。
所以它的能力更像：
- 在当前进程生命周期里
- 在一定时间窗口内
- 允许恢复

但这离 deep 模式真正想表达的"续航"还差一层：

> **缺少 durable mission state persistence**

因此当前系统可以说：
- 支持恢复
- 但不支持真正的长期项目持续推进

这在产品表述上要很小心。
否则容易让用户以为它已经是"长期自治代理"。

#### 不足五：验收标准仍然是文本抽取，不是结构化合同

Worker 的 acceptance review 现在从 `delegationBriefing` 里正则提取：
- `## Acceptance Criteria`
- `## 验收标准`

这意味着 project-level review 的基础合同仍是文本块，而不是结构化对象。
这会有几个后果：
- 依赖 prompt 质量和格式稳定性
- 不利于中断恢复
- 不利于多轮累计 review
- 不利于统一 project-level dashboard / explainability

所以 deep 现在是"会复审"，但还不是"有结构化验收账本的复审系统"。

#### 不足六：Ask 模式的确认点偏前置，缺少中途 replan 治理

现在确认门禁是：
- `interactionMode === 'ask' && planMode === 'deep'`

这说明 deep + ask 的主要用户确认点是**计划开始前**。
但如果后续 review 发现：
- 范围扩大
- 方案推翻
- fix todo 累计已经变成 replan
- cross-worker 集成策略需要变化

当前看不到一个明确的"二次确认门禁"。
这对产品来说是一个遗漏：

> 初始计划可确认，不等于中途重大漂移也被治理住了。

#### 不足七：deep 的上下文续航没有明显升级

虽然预算加大了，但 `historyConfig` 仍是：
- `maxMessages = 40`
- `maxChars = 100000`
- `preserveRecentRounds = 6`

这说明 deep 模式的**执行预算**升级了，
但**上下文记忆治理**没有同级升级，只能更多依赖滚动摘要。

这不是 bug，但从"项目级续航"角度看，是一个架构短板。

---

## 八、产品级重构方案（唯一主线）

### 8.1 目标架构定义

> **把 deep 模式从"参数增强模式"升级为"显式阶段型项目执行状态机 + 统一完成合同 + 可恢复 ledger + 统一 review 闭环"的产品级项目执行运行时。**

不是推翻现有实现，而是把现有分散能力收编成一个统一模型。

---

### 8.2 核心架构升级路径

#### 升级一：建立 Deep Runtime State Machine

deep 模式必须有统一 phase，而不是散落在不同模块的局部状态。
建议至少固定这些 phase：

```typescript
enum DeepRuntimePhase {
  PLAN_CREATED = 'plan_created',
  PLAN_CONFIRMED = 'plan_confirmed',
  DISPATCHING = 'dispatching',
  WORKER_EXECUTING = 'worker_executing',
  WORKER_REVIEWING = 'worker_reviewing',
  PROJECT_VERIFYING = 'project_verifying',
  REPLANNING = 'replanning',
  EXTERNAL_WAITING = 'external_waiting',
  COMPLETED = 'completed',
  FAILED = 'failed',
  INTERRUPTED = 'interrupted',
}
```

这样带来的价值非常大：

- 所有继续/暂停/恢复都有统一依据
- 所有 UI 展示都有统一状态源
- 所有终止原因都能挂在 phase 上解释
- 所有 review 都有清晰归属

如果没有这层，deep 永远只是"增强模式"。

---

#### 升级二：建立 Mission Ledger，作为唯一事实源

必须引入任务级 ledger，而不是依赖：

- 内存 session
- 日志
- prompt 文本
- 临时 summary

这个 ledger 至少要记录：

```typescript
interface MissionLedger {
  missionId: string;
  currentPhase: DeepRuntimePhase;
  planVersion: number;
  reviewRound: number;
  acceptanceCriteria: AcceptanceCriterion[];
  activeBatchId?: string;
  workerSessions: Map<WorkerSlot, string>;
  terminationReason?: string;
  budgetUsage: {
    elapsedMs: number;
    tokenUsed: number;
    errorRate: number;
  };
  lastReplanReason?: string;
  requiresUserConfirmation: boolean;
  createdAt: number;
  updatedAt: number;
}
```

这层一旦建立，deep 才真正具备产品能力。

---

#### 升级三：把 Acceptance 升级为结构化合同

不能再让深度任务的闭环建立在 briefing 文本 section 抽取上。

应该把 acceptance 变成一等对象，至少包含：

```typescript
interface AcceptanceCriterion {
  criterionId: string;
  description: string;
  scope: 'worker_local' | 'cross_worker' | 'project_global';
  owner?: WorkerSlot;
  status: 'pending' | 'satisfied' | 'unsatisfied' | 'blocked';
  evidence?: string[];
  lastReviewedAt?: number;
  lastReviewedRound?: number;
}
```

这样做之后：

- Worker review
- project verification
- replan
- resume
- UI 展示
- audit

才能全部共享同一事实源。

---

#### 升级四：统一 Review 闭环，不再让两层 review 并列漂浮

我建议把 review 明确拆成两层，但合同统一：

**Worker Review**
负责：
- assignment 局部完成度
- 本 worker 范围内的 acceptance gap
- gap 转换为 fix todo

**Project Review**
负责：
- 跨 worker 集成结果
- 全局 acceptance contract
- DoD 判定
- 不通过时进入 `replanning`，而不是直接只抛错结束

关键点不是"保留两层"，而是：

> **两层都围绕同一个 acceptance ledger 运转。**

这样 review 才是产品级 review，不是两套机制。

---

#### 升级五：把 Resume 从 Session 恢复升级成 Mission 恢复

如果要产品级发布，恢复必须升级为：

- phase-aware
- ledger-driven
- deterministic

也就是说恢复时系统要明确知道：

- 我恢复到哪个 phase
- 哪些 worker 已完成
- 哪些 criterion 已满足
- 哪些 fix todo 是上一轮 review 生成的
- 当前为什么会恢复
- 恢复后下一步是什么

而不是只知道：

- 有个 sessionId
- 有段 resumePrompt
- retryCount + 1

后者适合工程内部使用，不适合产品级 deep。

---

#### 升级六：增加 Replan Gate

当前 ask + deep 只在开头做确认，这是不够的。

产品级 deep 必须增加二次确认门禁，触发条件建议明确且标准化：

1. `reviewRound` 超过阈值（如 > 5）
2. 新增 `fix todo` 数量超过阈值（如 > 10）
3. project verification 失败后需要 replan
4. scope 比初始计划显著扩大（如新增文件数 > 初始计划 50%）
5. budget 消耗跨过关键阈值（如已用 > 70%）
6. 涉及高风险文件/目录/操作

这样用户看到的 deep 才是"可控深入"，而不是"沉默地越跑越大"。

---

#### 升级七：补齐产品级可观测性

deep 模式要发布，日志还不够，必须有稳定的运行态指标。

至少要能观测：

- 当前 phase
- 当前 review round
- criterion satisfaction rate
- no-progress streak
- budget burn rate
- external wait age
- verification failure count
- replan count
- worker session resume success rate

否则一旦线上出现：

- deep 卡住
- deep 假完成
- deep 持续补 fix 不收敛
- deep 恢复后行为漂移

你很难做运营和排障。

---

### 8.3 哪些现有模块可以直接保留并升级

这次不是推倒重来。
当前代码里有不少东西是正确的，应该被收编，而不是废弃。

#### 可以保留的核心资产

**1. AutonomousWorker 的 fix todo 闭环能力**
这是非常有价值的内核能力。
应该保留，但纳入统一 review/contract 体系。

**2. OrchestratorDecisionEngine**
它已经在做终止治理。
应继续保留，但从"局部门禁器"升级为"phase machine 的守门人"。

**3. DispatchManager**
它已经承担了恢复桥接和 batch 生命周期治理。
这部分可以作为 deep runtime 的 dispatch coordinator 继续沿用。

**4. post-dispatch-verifier**
不要删除，但要把它从"后置闸门"升级为"project review phase"。

**5. deep 下 orchestrator 收权机制**
这是产品级边界感的体现，必须保留。

---

### 8.4 产品级发布前的准入标准

如果你要的是"优秀标准化"，那我建议 deep 模式在发布前至少满足下面这组门槛。

#### 架构门槛
- deep 有显式 phase machine
- acceptance 是结构化合同，不依赖 prompt section 解析
- mission ledger 成为唯一事实源
- review / verification / replan 全部挂到统一 runtime

#### 行为门槛
- 每次继续、停止、恢复都有明确 reason
- 不存在"系统自己也说不清为什么还在跑"的情况
- project verification 失败后能进入标准 replan 分支

#### 恢复门槛
- deep 中断后可 deterministic 恢复
- 恢复后 phase、review round、criterion 状态一致
- 不依赖内存 map 作为唯一恢复来源

#### 产品门槛
- 用户可以看到当前阶段和继续原因
- 高风险扩张时有二次确认
- 完成态可解释，不是"模型说完成了所以完成"

#### 运维门槛
- 有 phase / review / budget / verification 的核心埋点
- 可定位卡住、空转、反复修复不收敛等问题

只要这几项没齐，我不会建议把 deep 定义为"产品级成熟能力"。

---

## 九、实施路径与优先级

### 9.1 P0：必须先做（核心架构）

1. **Deep Phase Machine**
   - 定义 `DeepRuntimePhase` 枚举
   - 在 `MissionDrivenEngine` 中引入 phase 状态管理
   - 所有关键决策点（dispatch / review / verification / termination）都基于 phase 驱动

2. **Mission Ledger**
   - 定义 `MissionLedger` 接口
   - 实现 `MissionLedgerService`（可持久化到文件系统或内存）
   - 在 deep 模式下强制使用 ledger 作为唯一事实源

3. **结构化 Acceptance Contract**
   - 定义 `AcceptanceCriterion` 接口
   - 在 plan 阶段从 briefing 提取并结构化
   - Worker review 和 project verification 都读写同一 contract

4. **Worker Review / Project Review 统一闭环**
   - Worker review 只更新 worker-local criterion
   - Project verification 检查 project-global criterion
   - 失败时进入 `replanning` phase，而不是直接抛错

---

### 9.2 P1：紧随其后（产品化）

1. **Mission-level Resume**
   - 从 session 恢复升级为 ledger-driven 恢复
   - 恢复时明确恢复到哪个 phase
   - 恢复后 criterion 状态一致

2. **Replan Gate**
   - 定义二次确认触发条件
   - 在 ask 模式下触发用户确认
   - 记录 replan 原因到 ledger

3. **运行态可观测性**
   - 定义核心指标（phase / review round / criterion satisfaction rate）
   - 通过 `MessageHub` 发送运行态事件
   - UI 展示当前 phase 和 review 进度

---

### 9.3 P2：发布优化（体验提升）

1. **UI Phase 可解释化**
   - 在 UI 中展示当前 phase
   - 展示 review round 和 criterion 满足情况
   - 展示预算消耗和预估剩余时间

2. **产品级 Completion Reason 展示**
   - 完成时展示哪些 criterion 已满足
   - 失败时展示哪些 criterion 未满足及原因
   - 中断时展示中断原因和恢复建议

3. **Review 收敛度和预算消耗的运营指标**
   - 统计 review 收敛轮次分布
   - 统计预算消耗分布
   - 识别异常模式（如持续不收敛、预算异常消耗）

---

## 十、最终判断与建议

### 10.1 当前 deep 模式的准确定性

> **当前 deep 模式是"合理但未完成形态"的项目级长任务治理器。**
> **它不是设计错误，但它也还没进化成真正完整的 deep execution architecture。**

### 10.2 现在做得对的地方

- 方向对
- 角色边界对
- 安全网对
- 不是失控 agent
- 已经具备 project mode 雏形

### 10.3 现在真正的短板

- 还不是显式 deep runtime
- review 不是一等持久状态
- project-level review 闭环没彻底打通
- resume 还是会话级，不是任务级
- ask 模式缺少中途 replan 门禁
- acceptance contract 仍是文本，不是结构化事实源

### 10.4 我的最终建议

如果你的目标是：

> **deep 模式具备产品级可发布能力，并且在架构层面达到优秀、可标准化、可持续演进的水平**

那接下来不应该再沿着：

- 加 round
- 加 budget
- 加几个 gate
- 加几段 resume 逻辑

这条路继续走。

因为这条路会越来越像"把可用原型堆成复杂系统"。
产品级优秀架构不能这样长。

正确方向只有一个：

> **建立 deep 专属的项目执行状态机、统一 acceptance 合同、任务级 ledger、统一 review/replan 闭环。**

---

## 十一、下一步行动

如果你认可这个判断，下一步我建议我直接帮你产出并落地：

1. **核心数据结构定义**
   - `DeepRuntimePhase`
   - `MissionLedger`
   - `AcceptanceCriterion`

2. **MissionLedgerService 实现**
   - 创建/读取/更新 ledger
   - phase 转换逻辑
   - criterion 状态管理

3. **MissionDrivenEngine 升级**
   - 引入 phase machine
   - 在关键决策点基于 phase 驱动
   - 删除与新模型冲突的旧路径

4. **AutonomousWorker 升级**
   - review 时读写结构化 criterion
   - 删除文本 section 解析

5. **post-dispatch-verifier 升级**
   - 从"后置闸门"升级为"project review phase"
   - 失败时进入 replanning，而不是直接抛错

如果你点头，我下一步就直接按这个路径开始落第一轮核心架构代码。

### 4.1 用 5 Whys 收口

**Why 1：为什么 deep 看起来已经能 loop/review，但仍让人担心不够稳？**
因为它的 loop/review 是分散在多个层里实现的，不是统一 runtime phase。

**Why 2：为什么会分散？**
因为当前 deep 并没有独立执行内核，而是沿用了 standard 的主框架，在局部增强：
- budget
- maxReviewRounds
- maxTotalRounds
- resume bridge
- tool restriction

**Why 3：为什么会采用这种增强式实现？**
因为产品当前更像要一个**可控的项目级治理模式**，而不是一个彻底开放的自治代理。

**Why 4：为什么这会导致"续航感有了，但结构仍偏弱"？**
因为当 deep 进入多轮复审、跨 worker 集成、长链路恢复时，
仅靠局部增强已经不足以表达完整状态。

**Why 5：最终根因是什么？**
我给一个最准确的定性：

> **deep 模式的产品语义已经升级到了"项目级治理"，但执行内核还停留在"标准模式的参数化增强"，尚未升级为"项目级显式状态机"。**

这就是根本原因。

不是代码写得乱。
也不是某个模块设计错。
而是：

> **产品层已经迈到下一层，runtime 抽象还没完全跟上。**

---

## 五、产品级重构方案（唯一主线）

### 5.1 核心目标

> **把 deep 模式从"参数增强模式"升级为"显式阶段型项目执行状态机"，同时保留现有所有安全网。**

不是推翻现有实现，而是把现有分散能力收编成一个统一模型。

### 5.2 架构升级主线

#### 第一件事：把 deep 的 loop 明确成阶段状态机

建议 deep 运行态最少显式化为这些 phase：

1. `plan_confirmed`
2. `dispatching`
3. `worker_execution`
4. `worker_review`
5. `project_verification`
6. `replan`
7. `external_wait`
8. `completed`
9. `failed`
10. `interrupted`

这样做的价值是：
- 续航不再是"靠多个 if/while 拼出来"
- 终止/恢复/展示都有统一状态依据
- 用户和系统都知道"现在为什么还在继续"

#### 第二件事：把验收标准改成结构化合同

不要再让 deep 的核心闭环建立在：
- `## Acceptance Criteria`
- `## 验收标准`

这种文本 section 解析之上。

应该把 acceptance contract 结构化，例如至少具备：
- `criterionId`
- `description`
- `ownerScope`（worker-local / cross-worker / project）
- `status`（pending / satisfied / failed / blocked）
- `evidence`
- `lastReviewRound`

这样后续：
- Worker review
- project verification
- replan
- resume

才能共享同一事实源。

#### 第三件事：把两层 review 合并成一个统一的"完成合同闭环"

现在不是删除其中一层，而是要明确职责：

**Worker review**
只负责：
- 本 assignment 的局部完成度
- 本 worker 责任范围内的 fix 闭环

**Project verification**
负责：
- 跨 worker 的集成完成度
- 最终 Definition of Done
- 不通过时触发 replan / new dispatch，而不是只 throw

也就是说：

> `post-dispatch-verifier` 不该只是终点闸门，而应该成为 deep 状态机中的一个正式 phase。

#### 第四件事：把 resume 从 session 恢复升级为 mission 持久化

至少 deep 模式下，应该把这些状态变成 durable ledger：
- 当前 phase
- 当前 review round
- 哪些 criterion 未满足
- 哪些 fix todo 是 review 生成的
- 当前 worker session 映射
- 当前预算消耗
- 最近一次终止/中断原因

否则 deep 的"续航"仍然主要是进程内续跑，不够项目级。

#### 第五件事：补一个"中途重大漂移确认门禁"

在 `ask + deep` 下，不应该只在起始计划确认一次。
当发生这些情况时，应该触发二次确认：
- scope 明显扩大
- review 轮次跨过阈值
- 生成 fix todo 数量持续增加
- project verification 失败后需要 replan
- 预算即将从 feature 级进入 project 级消耗

这才符合"深度任务"的产品可信度。

---

### 5.3 哪些东西应该保留

这些我认为是对的，应该保留：

#### 1. 保留 orchestrator 收权
deep 下不应该让 orchestrator 直接变成大执行者。
它应该继续做治理者。

#### 2. 保留所有终止门禁
不要为了"更续航"就放松：
- stalled
- budget
- external wait
- upstream model error

这会直接把系统推向失控。

#### 3. 保留 Worker 内部 fix todo 机制
这个机制是对的，说明 Worker 已经具备"做完再查、查完再补"的自修复能力。
只是要把它纳入统一状态机，而不是散落在局部循环中。

---

### 5.4 从产品定位看，当前 deep 到底是什么

我给一个很明确的产品判断：

**如果 deep 的定位是：**
> **项目级长任务治理模式**

那现在的实现是**基本合理的**，而且方向对。

**如果 deep 的定位是：**
> **能够长期自主循环直到真正完成的自治代理**

那现在的实现**还不够**，差的不是几个常量，而是 runtime 抽象层级。

所以从产品文案和预期管理上，我建议明确：

> **deep 不是无限自治代理，而是"更强计划治理 + 更长执行预算 + 多轮复审 + 可恢复"的项目级执行模式。**

这一定义和现有代码是一致的。
如果你非要把它表述成"持续 autonomously loop until done"，那产品承诺会跑在架构前面。

---

## 六、最终判断

我最后给你一个不拐弯的判断：

### 现在 deep 模式做得对的地方
- 方向对
- 角色边界对
- 安全网对
- 不是失控 agent
- 已经具备 project mode 雏形

### 现在 deep 模式真正的短板
- 还不是显式 deep runtime
- review 不是一等持久状态
- project-level review 闭环没彻底打通
- resume 还是会话级，不是任务级
- ask 模式缺少中途 replan 门禁
- acceptance contract 仍是文本，不是结构化事实源

### 我的总定性
> **当前 deep 模式是"合理但未完成形态"的项目级长任务治理器。**
> **它不是设计错误，但它也还没进化成真正完整的 deep execution architecture。**

---

## 七、下一步建议

如果你的目标是：

> **deep 模式具备产品级可发布能力，并且在架构层面达到优秀、可标准化、可持续演进的水平**

那我给你的结论会更严格一些：

> **现状方向是对的，但还没有达到"产品级可发布"的优秀标准。**
> **它现在更像"可工作的 deep 原型内核"，还不是"标准化的 deep 产品架构"。**

如果你愿意，下一步我可以继续做两件事中的一个：

1. **把我上面的结论落成一版 deep 模式架构重构方案图**
2. **直接基于现有代码，给你列出最小改造路径，按优先级拆到具体模块和类**


