# worker_dispatch 自动分配重构开发计划 v2

更新时间：2025-07-19（实施完成）

## 1. 文档定位

本文档用于指导后续对 `worker_dispatch` 与编排链路的重构实施。

本次改造的目标不是优化报错文案，也不是给现有失败链路再补一层“自动重试”补丁，而是从架构上实现以下产品定位：

- 编排者应根据 worker 分工配置自动完成任务分配
- 编排者应直接生成合法、可执行的 worker 任务
- `ownership mismatch` 一类内部编排错误不应成为用户可见结果
- 重构后主线程任务卡片、worker 卡片、worker 任务分发、刷新恢复仍保持稳定

本文档遵循 `$cn-engineering-standard`，只保留一条稳定主链，禁止双轨兼容。

## 2. 结论先行

### 2.1 当前根因

当前系统已经具备：

- `category -> worker` 的稳定分工配置
- `dispatch` 后的稳定卡片渲染链
- `requestId / workerCardId / timelineAnchorTimestamp` 驱动的稳定时间轴投影

但当前系统缺失的是：

- 基于 worker 分工配置和 orchestrator 语义理解结果的 Assignment 编译层
- 将用户任务意图自动转换为合法 ownership 任务的唯一真相源

所以当前真实链路是：

`用户意图 -> orchestrator 直接写混合 category / 非法 Assignment -> dispatch guard 事后拦截 -> failed`

而产品目标要求的链路应该是：

`用户意图 -> orchestrator 输出任务合同 + ownership_hint/mode_hint -> AssignmentCompiler 做确定性校验/归一化/拆分 -> DispatchRoutingService 选择最终 worker -> 注册任务卡片 -> 执行`

### 2.2 改造判断

这是一次中大型编排链路重构，不是局部修补。

更准确地说，本次需要重构的是：

- `worker_dispatch` 输入协议
- ownership 模型
- routing 前的任务编译层
- dispatch 注册前的 compiled assignment / dispatch-ready assignment 生成逻辑
- orchestrator prompt 对 `category` 的职责定义

但不是推倒前端重来。

前端卡片稳定渲染、worker 任务线、时间轴投影这部分可以复用，前提是后端在“首次注册任务前”就拿到最终 worker 和稳定任务实体 ID。

## 3. 产品目标

本次改造完成后，系统必须满足以下目标：

1. 编排者根据 worker 分工配置自动分配任务，而不是依赖 LLM 手写正确 `category`
2. 跨域任务自动拆分为多个合法 Assignment，不出现 `ownership mismatch`
3. `worker_dispatch` 返回 canonical 派发结果，而不是内部 guard 错误
4. 任务卡片首次渲染即落在正确 worker 与正确时间轴位置，不后漂、不换线
5. 主线程任务卡片、worker 生命周期卡、worker instruction、worker output 使用同一套稳定 ID 体系
6. 页面刷新、会话切换后，卡片和 worker 任务线能够按现有 projection 机制稳定恢复
7. 全链路只有一套实现，不保留旧 `category` 语义兼容模式

## 4. 当前问题定义

### 4.1 语义混用

当前 `category` 同时承担了两类职责：

- ownership 路由
- 工作方式标记

这会导致以下冲突：

- `frontend/backend/integration` 是 ownership
- `test/document/review/debug/refactor/architecture` 更接近执行方式

于是当任务文本同时命中多个主职责域时，`test/document` 会与 ownership 发生语义冲突，最终被 `ownership-guard` 拒绝。

同时，当前 `worker-assignments` 校验逻辑要求所有 `category` 都必须归属某个 worker，这会把 `test/document/review/debug/refactor` 这类执行方式标签错误地纳入 routing 体系，进一步放大语义混用。

### 4.2 分工配置只参与路由，不参与编译

当前 worker 分工配置只参与：

- category 存在性检查
- category 对应 worker 的路由

它没有参与：

- Assignment 自动生成
- ownership 自动识别
- 跨域任务自动拆分

因此系统现状更像“路由校验器”，而不是“自动编排器”。

更准确地说，问题不在于配置“参与太晚”，而在于配置只参与“校验与路由”，没有参与“任务编译与自动拆分”。

### 4.3 失败暴露给用户

当前 `ownership-guard` 的失败会直接返回 `worker_dispatch` 工具结果，并进入 UI 工具卡片渲染。

这不符合产品定位。

此类错误本应由系统内部吸收，而不是暴露给用户。

## 5. 核心设计原则

### 5.1 单链路

只允许这一条链路存在：

`用户任务 -> orchestrator 生成任务合同 + hint -> AssignmentCompiler 编译 -> DispatchRoutingService 选择 worker -> DispatchEntry 注册 -> 卡片渲染 -> worker 执行 -> timelineProjection 恢复`

禁止保留第二条旧链路，例如：

- orchestrator 直接显式填写旧 `category`
- dispatch 层兼容旧语义并隐式修补
- UI 同时兼容“canonical assignment”和“legacy category failure”

### 5.2 前置编译，后置执行

所有 ownership 判断、任务拆分、worker 决策必须发生在任务注册之前。

不允许：

- 先注册任务卡片
- 后面再调整 worker
- 再靠 UI 更新去修正位置

### 5.3 渲染稳定优先

首次渲染即为最终实体身份。

因此每个 dispatch-ready assignment 在注册前必须一次性确定：

- `taskId`
- `resolvedOwnership`
- `resolvedMode`
- `selectedWorker`
- `requestId`
- `workerCardId`
- `dispatchWaveId`
- `laneId`

### 5.4 无兼容分支

本次改造不保留旧 `category` 兼容模式。

允许一次性迁移，但最终运行态只保留新协议、新字段、新决策链。

## 6. 目标架构

## 6.1 新的职责划分

### Orchestrator

负责：

- 理解用户目标
- 生成结构化任务合同
- 基于上下文给出 `ownership_hint`
- 基于执行意图给出 `mode_hint`
- 判断是否需要分阶段
- 提供依赖关系与上下文

不再负责：

- 手写最终 routing category
- 直接决定最终 worker 归属

### AssignmentCompiler

负责：

- 以 orchestrator hint 为主做确定性校验
- 在 hint 不可靠或显式 `auto/mixed` 时做规则化推导
- 进行 ownership 归一化
- 进行 mode 归一化
- 按 ownership 自动拆分任务
- 生成 compiled assignments
- 生成依赖图与波次信息
- 为后续 dispatch 注册准备稳定 ID 所需的编译结果

它不是纯规则引擎，也不是额外再起一轮 LLM 调用的智能体。

它的定位是：

- `LLM hint + 确定性编译器`

这是本次改造后的唯一 assignment 编译真相源。

### DispatchRoutingService

负责：

- 根据 `resolvedOwnership` 查分工配置
- 选择 owner worker
- 在 worker 不可用时做运行时降级
- 输出最终 `selectedWorker`
- 在 worker 选定后参与构造 dispatch-ready assignment

不再负责：

- 解释用户原始任务
- 推断 category

### OwnershipGuard

负责：

- 校验 AssignmentCompiler 的输出是否违反架构约束
- 作为内部断言与日志守卫

不再作为：

- 用户路径上的业务失败出口

## 6.2 新的数据模型

### 任务合同输入模型

`worker_dispatch` 改造后，输入任务合同只保留任务语义，不要求 orchestrator 直接填最终 category。

其中：

- `ownership_hint` 不是“锦上添花”，而是强约束输入
- 但它不是最终真相，只是 orchestrator 的最佳判断
- 当 orchestrator 无法可靠判断时，必须显式给出 `auto` 或 `mixed`
- worker 分工配置后续只对 ownership 生效，不再要求 mode 归属 worker

建议模型：

```ts
interface WorkerDispatchTaskInput {
  task_name: string;
  goal: string;
  acceptance: string[];
  constraints: string[];
  context: string[];
  requires_modification: boolean;
  ownership_hint: 'frontend' | 'backend' | 'integration' | 'data_analysis' | 'mixed' | 'auto';
  depends_on?: string[];
  mode_hint?: 'implement' | 'test' | 'document' | 'review' | 'debug' | 'refactor' | 'architecture';
  scope_hint?: string[];
  files?: string[];
  contracts?: {
    producer_contracts?: string[];
    consumer_contracts?: string[];
    interface_contracts?: string[];
    freeze_files?: string[];
  };
}
```

### compiled assignment 模型

AssignmentCompiler 的输出不直接绑定最终 worker。

```ts
interface CompiledAssignment {
  assignmentId: string;
  taskName: string;
  goal: string;
  acceptance: string[];
  constraints: string[];
  context: string[];
  requiresModification: boolean;
  resolvedOwnership: 'frontend' | 'backend' | 'integration' | 'data_analysis';
  resolvedMode: 'implement' | 'test' | 'document' | 'review' | 'debug' | 'refactor' | 'architecture';
  dependsOn: string[];
  phaseIndex: number;
  dispatchWaveId: string;
  scopeHint: string[];
  files: string[];
}
```

### dispatch-ready assignment 模型

DispatchRoutingService 在 worker 可用性、降级策略决策完成后，才生成真正用于注册与渲染的 dispatch-ready assignment。

```ts
interface DispatchReadyAssignment {
  assignmentId: string;
  taskName: string;
  goal: string;
  acceptance: string[];
  constraints: string[];
  context: string[];
  requiresModification: boolean;
  resolvedOwnership: 'frontend' | 'backend' | 'integration' | 'data_analysis';
  resolvedMode: 'implement' | 'test' | 'document' | 'review' | 'debug' | 'refactor' | 'architecture';
  selectedWorker: WorkerSlot;
  dependsOn: string[];
  phaseIndex: number;
  scopeHint: string[];
  files: string[];
  requestId: string;
  workerCardId: string;
  dispatchWaveId: string;
  laneId: string;
}
```

### ownership 与 mode 分层

ownership 只允许承担“路由归属”：

- `frontend`
- `backend`
- `integration`
- `data_analysis`

mode 只允许承担“执行方式”：

- `implement`
- `test`
- `document`
- `review`
- `debug`
- `refactor`
- `architecture`

说明：

- `test`、`document` 不再直接决定 worker
- “前端补测试”应表达为 `frontend + test`
- “后端写文档”应表达为 `backend + document`
- 后续实现中应将当前单一 `category-definitions` 拆分为 `ownership-definitions` 与 `mode-definitions`
- `worker-assignments` 只校验 ownership 到 worker 的映射，不再校验 mode 到 worker 的映射

### 6.3 编译器定位说明

AssignmentCompiler 不是纯关键词规则引擎。

原因很简单：

- 纯正则 / 关键词匹配无法可靠处理真实语义任务
- 但在 dispatch 前再发起一轮额外 LLM 调用，会显著增加延迟、成本和链路复杂度

因此本次采用的不是二选一，而是混合式单链路：

1. orchestrator 结合完整上下文输出 `ownership_hint`
2. compiler 将 `ownership_hint` 作为首要输入
3. compiler 用规则做校验、归一化、拆分、phase 构造
4. 当 `ownership_hint=auto/mixed` 时，compiler 允许退回到规则化推导
5. 当 compiler 仍无法形成合法 assignment 时，才升级为澄清场景

也就是说：

- LLM 负责语义理解
- compiler 负责结构化收口

### 6.4 mode 的行为语义

`mode` 不是纯展示标签。

本次重构要求分两步落地：

1. Phase 1
- `mode` 进入 compiled assignment / dispatch-ready assignment / 卡片元数据

2. Phase 2-3
- `mode` 进入 worker instruction 生成逻辑，影响执行约束

例如：

- `mode=test`
  - 强调优先补测试、避免无关业务改造、明确覆盖边界
- `mode=document`
  - 强调只更新文档/说明/注释，不改变业务行为
- `mode=review`
  - 强调只读审查，不写文件
- `mode=debug`
  - 强调先定位问题、保留证据链、避免盲改

结论：

- `mode` 在最终系统中既服务展示，也服务 worker 执行行为
- 不能只作为 UI 标签保留

## 6.5 自动拆分规则

AssignmentCompiler 必须内建以下规则：

1. 若 `ownership_hint` 可靠且只命中一个主 ownership，则直接生成一个 compiled assignment
2. 若 `ownership_hint=mixed` 或任务同时命中 `frontend + backend`，则自动拆分为多个 compiled assignments
3. `integration` 只能作为后置 phase，不能承接 phase-1 主功能
4. `test/document/review/debug/refactor/architecture` 只能作为 mode，不得抢占 ownership
5. 若用户目标包含“补测试”或“补文档”，则编译为主 ownership 下的 mode 任务，或作为后置补充任务
6. 若任务无法唯一落到一个主 ownership，且也无法安全拆分，则允许编排者向用户澄清，但不能先生成非法 dispatch
7. `dependsOn` 是唯一执行依赖真相源，wave 只做 phase 展示与分组，不承载实际依赖语义
8. 同一波次内允许多个 compiled assignments 并列注册；是否可执行由 `dependsOn` 决定

## 6.6 worker 不可用时的降级策略

本次重构中，worker 不可用的降级策略必须继续保留在 routing / execution 层，而不是下沉到 compiler。

原因：

- compiler 的职责是形成合法 assignment 结构
- worker 可用性是运行时状态，不属于 assignment 语义

因此唯一允许的链路是：

1. compiler 产出 `resolvedOwnership`
2. routing service 根据 worker 分工配置找到 owner worker
3. 若 owner worker 不可用，由 routing service 按现有运行时策略选择 fallback worker
4. 只有在 `selectedWorker` 已确定后，才允许生成 `DispatchReadyAssignment` 与稳定卡片元数据

不允许的做法：

- compiler 直接决定 fallback worker
- 在卡片已渲染后再动态改派 worker
- 把“worker 不可用”回写成新的 ownership 语义

## 7. 任务卡片与 worker 渲染约束

本次改造必须明确保证：重做 `worker_dispatch` 后，卡片与 worker 任务线仍然稳定。

### 7.1 首次渲染固定位置

`DispatchEntry.worker` 必须在任务注册前就是最终 worker。

后续要求：

- `emitSubTaskCard` 第一次发出时即绑定最终 worker
- `workerInstruction` 第一次发出时即绑定最终 worker
- 不允许“先挂到 A worker，再改派到 B worker”作为常规路径

### 7.2 稳定实体 ID

以下字段必须由 AssignmentCompiler 与 dispatch 注册链配合一次性生成：

- `assignmentId`
- `requestId`
- `workerCardId`
- `dispatchWaveId`
- `laneId`
- `timelineAnchorTimestamp`

其中：

- `workerCardId` 决定 worker 生命周期卡实体身份
- `requestId` 与 `timelineAnchorTimestamp` 决定主线程和 worker 面板的稳定排序锚点
- `dispatchWaveId` 用于同一 phase 的展示归组
- `dependsOn` 用于执行依赖，不允许由 `dispatchWaveId` 代替

### 7.3 主线与 worker 分线

主线程显示：

- 子任务状态卡
- 汇总信息

worker 面板显示：

- worker instruction
- worker progress
- worker output
- worker error

改造后不允许因为重编译或改派导致主线与 worker 面板混线。

### 7.4 刷新与会话切换

不得引入新的恢复协议。

继续复用当前：

- `requestId`
- `workerCardId`
- `timelineProjection`
- `sessionTimelineProjection`

严禁为了适配新 `worker_dispatch` 再造第二套恢复链。

### 7.5 波次与依赖的协作规则

本次重构对 wave 与依赖的分工必须明确：

- `dependsOn`
  - 唯一执行依赖真相源
  - 决定任务是 `pending`、`ready` 还是可运行

- `dispatchWaveId`
  - 只表示同一轮或同一 phase 的展示分组
  - 不决定任务可执行性

- `phaseIndex`
  - 由 compiler 基于依赖图推导
  - 用于构建稳定的波次编号和串联展示顺序

因此：

- 同一 `dispatchWaveId` 内允许多个任务并行
- 不同 wave 之间的执行关系仍由 `dependsOn` 驱动
- `integration` 任务如果依赖 `frontend/backend`，必须通过 `dependsOn` 显式表达，不能只靠 wave 顺序暗示

## 8. 实施阶段

## Phase 0：冻结原则与删改边界

目标：

- 冻结旧 `category` 语义
- 明确新协议唯一主链

任务：

- 定稿新 `worker_dispatch` 输入协议
- 定稿 `ownership + mode` 双轴模型
- 定稿 `ownership_hint` 强约束策略
- 定稿 `ownership-definitions` / `mode-definitions` 拆分方案
- 标记所有 legacy `category` 依赖点
- 定稿 prompt 迁移措辞与示例风格
- 定稿旧 prompt 到新 prompt 的样例映射

完成标准：

- 后续代码改造只围绕新协议推进
- 不再继续扩展旧 `category` 混合语义

## Phase 1：AssignmentCompiler 落地

目标：

- 建立 routing 前唯一编译层

建议新增文件：

- `src/orchestrator/core/assignment-compiler.ts`
- `src/orchestrator/core/assignment-types.ts`

任务：

- 实现任务合同到 compiled assignments 的编译
- 以 `ownership_hint` 为首要输入做 ownership 归一化
- 实现 mode 检测
- 实现自动拆分规则
- 实现 phase / wave / dependsOn 协作规则
- 实现 assignment 稳定 ID 生成所需的编译字段

完成标准：

- 给定用户任务合同，可直接产出一组 compiled assignments
- 不依赖 orchestrator 手写最终 routing category
- 不依赖纯正则独立决定所有 ownership

## Phase 2：dispatch 主链重构

目标：

- 用 compiled assignments / dispatch-ready assignments 接管旧 dispatch 输入链

核心改造文件：

- `src/tools/orchestration-executor.ts`
- `src/orchestrator/core/dispatch-manager.ts`
- `src/orchestrator/core/dispatch-routing-service.ts`

任务：

- `worker_dispatch` 工具 schema 改为接收任务合同
- `dispatch-manager` 在注册任务前调用 AssignmentCompiler
- 以 `resolvedOwnership` 调用 routing service
- 在 routing service 得到 `selectedWorker` 后生成 dispatch-ready assignment
- 在注册 `DispatchEntry` 前写入最终 worker 与稳定元数据
- 保持现有 worker 不可用降级能力只存在于 routing / execution 层

完成标准：

- 任务注册时拿到最终 worker
- `emitSubTaskCard` 不再出现事后 ownership 失败
- worker 不可用时仍能沿当前运行时降级逻辑处理，不与 compiler 冲突

## Phase 3：guard 收口与 prompt 同步

目标：

- 让 guard 退回内部断言
- 让 prompt 不再要求 orchestrator 手写 routing category

核心改造文件：

- `src/orchestrator/profile/ownership-guard.ts`
- `src/orchestrator/prompts/orchestrator-prompts.ts`

任务：

- 删除“用户态 ownership mismatch 失败出口”
- prompt 改为要求 orchestrator 输出任务合同 + `ownership_hint/mode_hint`
- prompt 示例从 `category=frontend/backend/test` 改为 `ownership_hint + mode_hint`
- 将 `ownership_hint` 描述为“最佳判断”而不是“最终路由标签”
- 保留澄清场景，但不允许先派发非法 Assignment
- 将 mode 引入 worker instruction / assignment briefing 生成逻辑
- 将旧 prompt 中所有“你必须显式给出最终 category 并据此路由”的措辞清理为“你必须给出 ownership_hint，系统会据此编译和路由”

完成标准：

- 正常用户路径上不再出现 `ownership mismatch`
- prompt 与后端协议完全一致
- `mode` 对 worker 行为约束开始生效，而不只是展示字段

## Phase 4：前端与时间轴稳定性核对

目标：

- 确保重构后卡片稳定渲染和恢复不受破坏

核心核对文件：

- `src/orchestrator/core/message-factory.ts`
- `src/ui/webview-svelte/src/lib/message-handler.ts`
- `src/session/session-timeline-projection.ts`
- `src/ui/webview-svelte/src/lib/worker-panel-state.ts`

任务：

- 确认新链路继续输出稳定 `workerCardId`
- 确认 `requestId` 与 `timelineAnchorTimestamp` 在新链路完整透传
- 确认主线程与 worker 面板投影不需要新增兼容逻辑

完成标准：

- 无新增兼容字段
- 原有卡片稳定渲染能力保持

## Phase 5：旧语义清理

目标：

- 删除旧 `category` 混合语义残留

任务：

- 删除对 legacy routing category 的双重解释
- 删除对 `test/document` 直接作为 worker routing category 的旧逻辑
- 将 `worker-assignments` 校验从“全部 category 必须归属”收口为“全部 ownership 必须归属”
- 删除仅用于旧链路的错误文案、提示词、守卫分支

完成标准：

- 仓库运行态只剩一套协议

## 9. 涉及文件建议清单

### 新增

- `src/orchestrator/core/assignment-compiler.ts`
- `src/orchestrator/core/assignment-types.ts`
- 可选：`src/orchestrator/core/assignment-compiler.spec.ts` 或同类验证入口

### 重点重构

- `src/tools/orchestration-executor.ts`
- `src/orchestrator/core/dispatch-manager.ts`
- `src/orchestrator/core/dispatch-routing-service.ts`
- `src/orchestrator/profile/ownership-guard.ts`
- `src/orchestrator/prompts/orchestrator-prompts.ts`
- `src/orchestrator/profile/domain-detector.ts`
- `src/orchestrator/profile/builtin/category-definitions.ts`
- 可选拆分为：
  - `src/orchestrator/profile/builtin/ownership-definitions.ts`
  - `src/orchestrator/profile/builtin/mode-definitions.ts`
- `src/orchestrator/profile/worker-assignments.ts`

### 联动核对

- `src/orchestrator/core/message-factory.ts`
- `src/ui/webview-svelte/src/lib/message-handler.ts`
- `src/session/session-timeline-projection.ts`
- `src/ui/webview-svelte/src/lib/worker-panel-state.ts`

## 9.1 Prompt 迁移示例

旧示例：

```text
worker_dispatch({
  tasks: [{
    task_name: "[Frontend] Add tests",
    category: "test",
    ...
  }]
})
```

新示例：

```text
worker_dispatch({
  tasks: [{
    task_name: "[Frontend] Add tests for login form",
    ownership_hint: "frontend",
    mode_hint: "test",
    ...
  }]
})
```

跨域旧示例：

```text
worker_dispatch({
  tasks: [{
    task_name: "Implement login with docs",
    category: "document",
    ...
  }]
})
```

跨域新示例：

```text
worker_dispatch({
  tasks: [{
    task_name: "Implement login flow",
    ownership_hint: "mixed",
    mode_hint: "implement",
    ...
  }]
})
```

说明：

- 新协议下，跨域不再靠一个错误 category 硬扛
- orchestrator 只需诚实表达“这是 mixed”
- compiler 负责拆分出 frontend/backend/integration/document/test 等合法 assignment 结构

## 10. 风险与约束

### 10.1 最大风险

最大风险不是编译规则写不出来，而是：

- 新协议落地后，旧 prompt 仍在生成 legacy category
- dispatch 注册前没有真正固定最终 worker
- 将 compiler 误做成纯规则引擎，导致大量任务退化为澄清
- 为了“平滑过渡”保留双协议兼容

这些问题只要发生任意一项，本次改造就会重新滑回补丁式修复。

### 10.2 不允许的做法

- 不允许在 `ownership-guard` 失败后再把旧错误包装成更友好的用户提示
- 不允许保留 `legacy category` 和 `new ownership/mode` 双解析链
- 不允许先发卡再改 worker
- 不允许为 UI 新增“临时兼容旧 dispatch failure”的特殊分支

### 10.3 开发期回滚策略

运行态不允许双轨，但开发过程必须有安全网。

建议采用：

1. 独立 feature branch 推进
2. 每个 Phase 独立提交
3. 每个 Phase 结束后打可回退基线 tag
4. 若 Phase 2 或 Phase 3 出现不可预期问题，使用 `git revert` 回退整个 phase 的提交集合

注意：

- 允许“代码级可回滚”
- 不允许“运行时双实现并存”

## 11. 验收标准

## 11.1 功能验收

- [x] 用户给出跨前后端需求时，系统自动拆分出多个合法 assignments
- [x] 用户给出“补测试”“补文档”类需求时，系统按主 ownership 正确挂到对应 worker
- [x] `worker_dispatch` 不再返回 `ownership mismatch`
- [x] 编排者无需显式填写最终 routing category 也能完成正确分配
- [x] orchestrator 输出的 `ownership_hint` 会被 compiler 正常校验、归一化或拆分，而不会直接裸透传

## 11.2 卡片与渲染验收

- [x] 子任务卡首次渲染即出现在最终位置
- [x] worker 生命周期卡首次渲染即落到正确 worker 线
- [x] 后续状态更新只更新已有实体，不换卡、不漂移
- [x] 主线程与 worker 面板不混线

## 11.3 恢复验收

- [x] 页面刷新后任务卡片与 worker 线恢复正常
- [x] 会话切换后原有任务卡与 worker 内容不丢失
- [x] 不引入第二套恢复协议

## 11.4 架构验收

- [x] worker 分工配置成为唯一 routing 真相源
- [x] AssignmentCompiler 成为唯一 assignment 编译真相源
- [x] `ownership-guard` 不再作为用户态失败出口
- [x] 仓库运行态不存在 legacy 与新协议双轨并存
- [x] mode 已进入 worker 行为约束链，而不只是展示字段

## 12. 建议开发顺序

建议严格按以下顺序推进：

1. 定稿新协议和双轴模型
2. 先落 `ownership_hint/mode_hint` 的新 prompt 契约
3. 再落 AssignmentCompiler
4. 再接 dispatch-manager 注册链
5. 再改 guard 与 worker instruction 生成逻辑
6. 最后做前端与恢复回归
7. 最后删除 legacy 逻辑

不要反过来。

如果先改 prompt、先改 UI、先改报错文案，而编译层还没建立，只会继续放大系统不稳定性。

## 13. 结束标准

满足以下条件，才视为本次重构完成：

- 用户输入的任务意图可以被系统自动编译为合法 worker assignments
- `worker_dispatch` 成为 canonical assignment 派发工具，而不是 category 校验工具
- task card 与 worker card 在首帧即锁定正确位置与正确 worker
- 刷新与会话切换不丢失任务链路
- 仓库中不存在 legacy category 混合语义的运行态主链

## 14. 实施记录（2025-07-19 补充）

本节记录实际实施过程中超出原始计划的架构决策和实现细节。

### 14.1 跨域任务自动拆分（Auto-Split）

**设计变更**：原计划 6.5 条款 6 允许"编排者向用户澄清"，实际实施中改为**全自动拆分**，不向用户抛出错误。

**实现位置**：`assignment-compiler-impl.ts` → `compileSplitAssignments()`

**机制**：

1. 当 AssignmentCompiler 检测到多个 ownership 域同时命中（如 `frontend + backend`），不再拒绝
2. 自动按每个域生成独立的 `AssignmentCompilationItem`，每项包含 `suggestedTaskTitle` 和 `suggestedGoal`
3. 输出格式：`{ items: AssignmentCompilationItem[], autoSplit: true }`
4. `dispatch-manager.ts` 检测到 `autoSplit && items.length > 1` 时，递归为每个 item 创建独立的 dispatch entry

**设计原则**：编译层的拆分结果是确定性的，不依赖 LLM 二次推导。

### 14.2 Mode 行为约束注入链

**设计变更**：原计划 11.4 要求"mode 已进入 worker 行为约束链"，实际实施中建立了完整的约束定义与双注入点架构。

**约束定义**：`task-taxonomy.ts` → `MODE_CONSTRAINTS`

每个 mode 包含：

- `description`：模式描述（注入 worker prompt）
- `behavioralConstraints`：行为约束条目列表
- `readOnly`：是否只读模式（review / architecture 为 true）
- `allowedFilePatterns`：允许操作的文件模式（test 模式限定 `*.test.*` / `*.spec.*`）

**注入点 1**：`dispatch-manager.ts` → `buildDelegationBriefing()`

- 当 `taskContract.mode !== 'implement'` 时，在 delegation briefing 中追加 mode 约束段
- 这是 Worker 接收到的主要任务指令文本

**注入点 2**：`prompt-builder.ts` → `buildWorkerPrompt()`

- 当 `InjectionContext.mode` 存在且非 `implement` 时，在 worker system prompt 中追加约束
- 这是 Worker 的角色级系统提示词

**设计原则**：`implement` 为默认模式，不注入额外约束，避免噪声。只有非默认模式才注入行为限制。

### 14.3 涉及文件清单（实际变更）

| 文件 | 变更类型 | 说明 |
| ------ | --------- | ------ |
| `src/orchestrator/profile/task-taxonomy.ts` | **新增** | ownership × mode 双轴类型系统 + MODE_CONSTRAINTS |
| `src/orchestrator/profile/assignment-compiler.ts` | **新增** | AssignmentCompiler 接口定义 |
| `src/orchestrator/profile/assignment-compiler-impl.ts` | **新增** | AssignmentCompiler 实现（含 auto-split） |
| `src/orchestrator/core/dispatch-manager.ts` | **重构** | 注册链路、auto-split 处理、mode 约束注入 |
| `src/orchestrator/core/dispatch-batch.ts` | **重构** | DispatchTaskContract: category → ownership + mode |
| `src/orchestrator/core/dispatch-routing-service.ts` | **重构** | 方法/接口重命名，category → ownership |
| `src/orchestrator/prompts/orchestrator-prompts.ts` | **重构** | hint 协议、身份常量、示例代码 |
| `src/orchestrator/profile/prompt-builder.ts` | **重构** | mode 约束注入到 worker prompt |
| `src/orchestrator/profile/types.ts` | **重构** | InjectionContext 新增 mode 字段 |
| `src/tools/orchestration-executor.ts` | **重构** | 工具 schema: category → ownership_hint + mode_hint |
| `src/orchestrator/profile/ownership-guard.ts` | **重构** | 降级为内部断言 |