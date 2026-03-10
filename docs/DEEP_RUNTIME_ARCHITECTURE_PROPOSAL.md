# Deep 运行时架构升级方案（控制平面扩展版）

> 目标不是另起一套 deep runtime，而是在现有 `TerminationSnapshot + PlanLedger + MetricsRepository` 基线上，补齐产品级可发布所需的项目级治理能力。

**版本**: 2.0  
**日期**: 2026-03-10  
**状态**: 待评审

---

## 一、[表象分析] 旧版 proposal 的问题

旧版 proposal 的方向基本正确，但存在 5 个关键偏差：

1. 把 `MissionLedger` 写成了独立账本，和现有 `PlanLedger` 形成重复建设风险。
2. 把 `WORKER_EXECUTING`、`WORKER_REVIEWING` 提升到 mission 顶层状态，造成状态层级混乱。
3. 把 `TerminationSnapshot` 写成待新建能力，而仓库中已有正式实现与治理基线。
4. 双轨期设计里出现“切回老路径”表述，容易制造双真相源和运行中热切换风险。
5. 未把 Acceptance 迁移、并发控制、错误处理、版本管理写成正式约束。

这些问题的直接后果不是“方案不可用”，而是：

- 复用边界不清；
- 控制平面职责重复；
- 发布策略不可控；
- 后续代码落地容易演变成第二套运行时。

---

## 二、[机理溯源] 现有控制平面已经具备的基础能力

Deep 模式真正需要的是：**项目级长任务治理**，包括 review、replan、resume、termination、audit 的统一闭环。

而仓库当前已经存在一套可复用的控制平面骨架：

| 现有能力 | 当前位置 | 结论 |
|---|---|---|
| 终止权威快照 | `orchestrator-termination.ts` / `ORCHESTRATION_TERMINATION_DESIGN.md` | **直接复用** |
| Plan/Todo/Attempt 统一状态模型 | `docs/ORCHESTRATION_TERMINATION_DESIGN.md` + `PlanLedgerService` | **直接复用并扩展** |
| 账本持久化 / 事件日志 / session 串行写 | `src/orchestrator/plan-ledger/plan-ledger-service.ts` | **作为唯一账本基础设施** |
| 终止指标落盘 | `termination-metrics-repository.ts` | **仅作观测 sink** |
| Mission/Assignment/Todo 回写 ledger | `mission-driven-engine.ts` 现有绑定 | **继续沿用** |

因此，本方案的正确姿势不是“新建 deep 基础设施”，而是：

> **在现有控制平面之上增加 deep runtime 需要的结构化合同、层级状态、恢复语义、并发约束和发布约束。**

---

## 三、[差距诊断] 现有实现距离产品级 deep 还缺什么

当前缺口不在“有没有控制平面”，而在“控制平面是否已经表达 deep 所需语义”：

1. `PlanLedger` 还没有正式承载 `schemaVersion`、`runtimeVersion`、`revision` 等版本边界。
2. `acceptanceCriteria` 在多处仍是 `string[]`，缺少统一的结构化合同收口模型。
3. mission 级治理状态、item/todo/attempt 执行状态、review/wait/replan 治理状态还没有严格分层。
4. 双轨发布缺少“单写主路径 + 影子只读 + mission 绑定版本”的正式约束。
5. 并发写入、非法 phase 转移、ledger 故障、schema 升级尚未被定义为 fail-closed 的运行规则。

---

## 四、[根本原因分析] 旧版 proposal 为什么会偏离

根因不是某个字段没设计好，而是抽象边界发生了漂移：

1. **把产品需求误翻译成基础设施重建**  
   deep 需要更强治理，不代表要新建第二套 ledger / snapshot / phase 底座。

2. **把局部执行状态抬升为 mission 顶层状态**  
   `worker_reviewing` 这种语义属于局部执行环节，不应污染顶层治理生命周期。

3. **把观测、裁决、账本混在一起**  
   `TerminationMetricsRepository` 是观测输出，不是账本；`TerminationSnapshot` 是裁决真相源，不是 metrics 扩展字段。

4. **把双轨发布写成运行期回退逻辑**  
   产品级发布允许“停止给新任务分配新 runtime”，但不允许“正在运行的 mission 在中途切控制路径”。

---

## 五、[彻底修复与债清偿] 正式架构决策

### 5.1 复用 / 扩展 / 禁止重复建设边界

| 主题 | 决策 | 约束 |
|---|---|---|
| `TerminationSnapshot` | 直接复用 | 禁止新建第二套 termination snapshot 抽象 |
| `PlanLedgerService` | 扩展为 deep runtime 唯一账本服务 | 禁止引入独立 `MissionLedgerService` |
| `MissionLedger` | 仅保留为架构视角名词 | 代码实现上必须收敛到 `PlanLedger` 扩展视图 |
| `TerminationMetricsRepository` | 继续复用 | 仅作指标/审计 sink，不参与真相裁决 |
| `PlanStatus / Todo / Attempt` | 直接复用 | 不再另建一套平行状态机 |

**正式结论**：

> `MissionLedger` 不作为物理账本存在；它只是 `PlanLedger` 在 deep runtime 场景下的治理视图。

---

### 5.2 目标分层

```
产品平面
  └─ 展示当前阶段、继续原因、风险确认、完成解释

治理平面
  └─ 预算、风险阈值、审批门控、replan 决策

控制平面
  ├─ PlanLedger（唯一账本）
  ├─ TerminationSnapshot（唯一终止裁决输入）
  ├─ BlockerRegistry（阻塞真相源）
  └─ Deep Runtime Reducer（由账本与治理状态导出阶段视图）

执行平面
  └─ MissionDrivenEngine / Dispatch / Worker / Verification
```

这个分层的关键点是：

- **账本负责记事实**；
- **快照负责做裁决**；
- **指标仓储负责审计**；
- **UI 阶段展示来自 reducer 导出的视图，而不是第二套持久化真相源**。

---

### 5.3 Deep Phase Machine 改为“层级状态模型”，不再新建顶层 11 状态

旧版 proposal 的问题是把所有状态都塞进 mission 顶层。正式修正如下：

#### A. 顶层生命周期：直接复用 `PlanStatus`

`draft / awaiting_confirmation / approved / executing / partially_completed / completed / failed / cancelled / superseded`

这条轴只表达 **mission/plan 的生命周期**，不表达 worker 局部执行细节。

#### B. 局部执行状态：继续复用 item / todo / attempt

- item / todo：负责任务拆解与依赖推进；
- attempt：负责单次执行尝试、超时、失败、取消、重试边界。

#### C. 治理 facet：单独建模，不污染顶层状态

在 `status=executing` 的前提下，deep runtime 通过 facet 表达当前治理语义：

- `reviewState`: `idle | running | accepted | rejected`
- `replanState`: `none | required | awaiting_confirmation | applied`
- `waitState`: `none | external_waiting`
- `acceptanceState`: `pending | partially_satisfied | satisfied | blocked`

#### D. 产品展示阶段：由 reducer 推导，不作为第二套持久化状态

示例：

- `status=executing` + `reviewState=running` → UI 展示“复审中”
- `status=executing` + `waitState=external_waiting` → UI 展示“等待外部输入”
- `status=executing` + `replanState=awaiting_confirmation` → UI 展示“等待确认重新规划”

**结论**：

> deep phase 是**派生视图**，不是第二套顶层状态真相源。

---

### 5.4 账本模型：以 `PlanRecord` 扩展承载 deep runtime 元数据

本方案不新建 `MissionLedger` 接口文件，而是扩展 `PlanRecord`：

```typescript
interface PlanRecordVNext extends PlanRecord {
  schemaVersion: number;
  runtimeVersion: 'classic' | 'deep_v1';
  revision: number;
  runtime?: {
    acceptance?: {
      criteria: AcceptanceCriterion[];
      summary: 'pending' | 'partial' | 'satisfied' | 'blocked';
    };
    review?: {
      round: number;
      state: 'idle' | 'running' | 'accepted' | 'rejected';
      lastReviewedAt?: number;
    };
    replan?: {
      state: 'none' | 'required' | 'awaiting_confirmation' | 'applied';
      reason?: string;
    };
    wait?: {
      state: 'none' | 'external_waiting';
      reasonCode?: string;
    };
    termination?: {
      snapshotId: string;
      reason: string;
    };
  };
}
```

约束：

1. `PlanRecord.version` 继续表示**计划版本**，不再混用为 schema version。
2. `revision` 用于并发控制，不代表 plan 语义升级。
3. `runtimeVersion` 在 mission 创建时绑定，mission 生命周期内不变。

---

### 5.5 Acceptance Contract 迁移路径

#### 5.5.1 迁移目标

把以下入口统一收敛到结构化合同：

- `PlanRecord.acceptanceCriteria`
- `DispatchEntry.acceptance`
- `ExecutionPlan.acceptanceCriteria`
- assignment 上已经引入的结构化 `acceptanceCriteria`

#### 5.5.2 迁移原则

1. **允许自动结构化壳迁移，不允许猜业务语义**。
2. **新写路径只写结构化合同，不再新增 `string[]` 真相源**。
3. **迁移失败时阻塞确认/执行，不提供长期文本兜底**。

#### 5.5.3 最小结构模型

```typescript
interface AcceptanceCriterion {
  id: string;
  description: string;
  status: 'pending' | 'satisfied' | 'unsatisfied' | 'blocked' | 'waived';
  evidenceIds: string[];
  required: boolean;
  waiverApproved: boolean;
}
```

说明：

- 先收敛到最小可运行结构；
- 不在迁移第一阶段强行引入 `owner/scope/dependencies` 等高语义字段；
- 后续增强只在结构化模型上演进，不再回到文本数组。

#### 5.5.4 迁移步骤

1. **加载时迁移**：读取旧 `string[]` 时包裹成最小结构对象。
2. **保存时重写**：一旦以新 schema 成功加载，立即按新结构写回。
3. **创建时单写**：新 mission / dispatch / plan 全部只生成结构化 acceptance。
4. **治理门禁**：若 acceptance 为空、损坏或重复，阻塞确认/重规划并显式报错。

---

### 5.6 双轨期状态同步规范

双轨期允许灰度，但不允许双真相源。

#### 正式约束

1. **主写路径只有一条**：`PlanLedgerService`。
2. **影子路径只读**：shadow 只做 reducer 对比、指标采样、结果审计。
3. **mission 创建时绑定 `runtimeVersion`**：`classic` 或 `deep_v1`。
4. **运行中 mission 禁止热切换控制路径**。
5. **灰度开关只影响新建 mission 的版本分配**。

#### 结果

- 可以停止向新 mission 分配 `deep_v1`；
- 但不能让正在运行的 `deep_v1` mission 中途切回 `classic`；
- 也不能让 `classic` mission 中途接管 `deep_v1` 的控制状态。

---

### 5.7 并发控制策略

多个 Worker、多个事件源同时更新 ledger 是 deep runtime 的常态，不是例外。

#### 原子性语义

单个 ledger 变更必须满足：

1. 基于同一 `revision` 读取；
2. 事件追加与快照更新要么同时成功，要么整体失败；
3. 提交成功后 `revision + 1`。

#### 并发约束

1. **单 ledger 串行写**：从 `session queue` 升级到 `ledgerId queue`。
2. **CAS / revision 校验**：写入时校验预期版本，不允许盲写覆盖。
3. **事件幂等**：assignment/todo/attempt 更新必须带 `eventId` 或幂等键。
4. **terminal 粘性**：进入 `completed/failed/cancelled` 后，不允许再被普通事件改回非终态。
5. **非法转移拒绝提交**：phase reducer 命中非法状态转移时，直接拒绝写入。

#### 冲突解决规则

1. 先按 `revision` 判断是否已过期；
2. 再按事件类型执行 deterministic reducer；
3. 终态冲突时遵循终止原因优先级与 terminal 粘性；
4. 冲突事件只记录审计，不生成隐式回退逻辑。

---

### 5.8 错误处理矩阵（fail-closed）

| 错误类型 | 处理策略 | 用户影响 |
|---|---|---|
| `PlanLedger` 读写失败 | 暂停 mission，标记控制平面故障，等待修复后重试 | 当前任务暂停，不丢状态 |
| reducer 非法转移 | 拒绝提交该事件，保留上一已提交 revision，输出审计错误 | 当前轮次失败，可诊断 |
| acceptance 迁移失败 | 阻塞确认/执行，要求修复合同数据 | 任务不能继续，但不会带病运行 |
| `TerminationSnapshot` 生成失败 | 禁止做终止裁决，保持 mission 为受控暂停态 | 不会误判完成/失败 |
| metrics sink 写入失败 | 告警 + 异步补写，不影响账本真相源 | 指标可能延迟，但状态不受影响 |

正式要求：

- 不允许 `ledger 不可用 -> 内存模式`；
- 不允许 `phase 异常 -> 回退老路径`；
- 不允许 `acceptance 失败 -> 文本兜底继续跑`。

---

### 5.9 Ledger 版本管理

必须引入三个彼此独立的版本概念：

| 字段 | 含义 | 是否可变 |
|---|---|---|
| `PlanRecord.version` | 业务计划版本 | 可随 replan 增长 |
| `schemaVersion` | ledger 存储结构版本 | 随 schema 升级变化 |
| `runtimeVersion` | mission 绑定的运行时实现版本 | mission 创建后固定 |
| `revision` | 单 ledger 并发修订号 | 每次成功写入递增 |

#### 升级策略

1. **离线迁移脚本**：用于历史冷数据批量升级。
2. **加载时迁移器**：支持 N-1 → N 的在线兼容读取。
3. **成功加载后重写**：避免旧结构长期滞留。
4. **明确兼容窗口**：只承诺 N / N-1，不做无限向后兼容。

---

## 六、实施顺序（正式版）

### Phase 0：文档与模型收敛

- 收敛本 proposal 到单一路径；
- 明确 `MissionLedger` 只是 `PlanLedger` 深度视图；
- 定义 `schemaVersion / runtimeVersion / revision`。

### Phase 1：PlanLedger 扩展

- 扩展 `PlanRecord` deep runtime 元数据；
- 加入 schema migrator 与 revision 机制；
- 保持现有 `Plan/Todo/Attempt` 状态模型不分叉。

### Phase 2：Acceptance 收敛

- 把所有新写路径收敛到结构化 acceptance；
- 清理 `string[]` 作为真相源的旧写入口；
- 将治理门禁挂到结构化合同校验结果上。

### Phase 3：Reducer / 并发 / 错误治理

- 实现 deep runtime reducer；
- 引入 ledgerId 串行写、CAS、幂等键、terminal 粘性；
- 将控制平面错误处理收敛为 fail-closed。

### Phase 4：Shadow 灰度与发布闸门

- 单写主路径 + 影子只读对比；
- mission 创建时绑定 `runtimeVersion`；
- 只对新建 mission 做版本分配控制。

---

## 七、发布前必须满足的准入标准

### 7.1 架构准入

- [ ] 不存在独立 `MissionLedgerService`
- [ ] `TerminationSnapshot` 仍是唯一终止裁决输入
- [ ] `TerminationMetricsRepository` 不承担账本职责
- [ ] deep phase 为派生视图，不是第二套顶层状态真相源

### 7.2 数据准入

- [ ] 所有新写路径只写结构化 acceptance
- [ ] `schemaVersion / runtimeVersion / revision` 已正式落地
- [ ] 旧 `string[]` 仅允许读取迁移，不再允许继续作为写入真相源

### 7.3 运行准入

- [ ] 单 ledger 写入具备 CAS 与幂等保证
- [ ] terminal 粘性生效，终态不可被普通事件回写
- [ ] reducer 非法转移会被拒绝并审计

### 7.4 发布准入

- [ ] 双轨期只有单写主路径
- [ ] in-flight mission 不发生控制路径热切换
- [ ] metrics sink 故障不会改变控制平面判定

---

## 八、最终结论

这次升级后的 deep runtime 正式定义为：

> **一个建立在现有 `TerminationSnapshot + PlanLedger` 控制平面之上的项目级长任务治理系统。**

它具备：

- 单一真相源；
- 层级状态模型；
- 结构化 acceptance 合同；
- 可恢复、可审计、可灰度的发布路径；
- 对并发、错误、版本演进有正式约束。

它明确**不具备**的东西也同样重要：

- 不另建物理 `MissionLedger`；
- 不另建第二套 termination snapshot；
- 不搞运行期回退老路径；
- 不保留文本兜底作为长期兼容分支。

---

**文档所有者**: 架构组  
**审核状态**: 待审核  
**下次更新**: Phase 1 设计完成后