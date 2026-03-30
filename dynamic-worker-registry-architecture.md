# 动态 Agent Registry 架构设计

更新时间：2026-03-31

## 1. 文档定位

本文档用于指导 Magi 从"3 个固定 Worker 槽位架构"重构为"角色驱动的动态 Agent Registry 架构"。

本次设计的目标不是简单把 `claude / codex / gemini` 从 3 个扩成 10 个，而是从架构上完成以下范式切换：

- 系统内置多种角色模板（RoleTemplate），用户从中选用
- 角色与模型解耦：用户独立配置模型引擎（ModelEngine），角色绑定引擎即可工作
- 一个角色模板只能创建一个实例，不存在多实例冲突
- 调度、持久化、时间轴、UI tabs、配置存储统一由 Agent Registry 驱动
- 全链路只保留一套动态实现，不保留固定三槽的长期兼容模式

本文档遵循 `$cn-engineering-standard`，采用"问题表象 -> 链路机理 -> 差距诊断 -> 根因分析 -> 源头设计"的结构展开。

## 2. 结论先行

### 2.1 设计结论

当前项目可以支持"用户选择角色、绑定模型引擎、主线区域动态出现对应角色 Tab、按角色分工自动调度"的产品形态，但前提是必须完成一次架构级改造，而不是做局部扩容。

正确的目标架构为三层模型：

```
RoleTemplate（系统内置角色模板）
    ↓ 用户选用
AgentBinding（角色实例 = 模板 + 引擎绑定 + 启停状态）
    ↓ 引用
ModelEngine（用户配置的模型引擎）
```

其中：

- `RoleTemplate` 是系统内置的只读角色定义，包含 profile、ownerships、默认 UI
- `AgentBinding` 是用户的选用记录：选了哪个模板、绑了哪台引擎、是否启用
- `ModelEngine` 是用户配置的模型基础设施：LLM 配置、运行时策略
- `templateId` 就是运行时唯一身份（agentId），全链路稳定，不需要额外生成 ID
- 调度目标、timeline lane、前端 tab 全部以 `agentId`（= `templateId`）为 key

### 2.2 核心判断

本次改造不是"新增一个配置按钮"，而是以下 6 个子系统的联动重构：

1. 类型系统
2. 配置存储
3. 角色模板与模型引擎分层
4. 调度与运行时
5. 时间轴投影与恢复
6. 前端设置页与条件式角色 Tabs 呈现

## 3. 产品目标

本次架构设计完成后，系统必须满足以下目标：

1. 系统内置多种角色模板，用户在设置页从模板库中选用角色
2. 用户独立配置模型引擎（添加/编辑/删除），角色绑定引擎即可工作
3. 主页 UI 始终保留主线 `thread` tab；当存在就绪角色时，自动出现对应角色 Tab
4. 当用户未启用任何角色时，系统自动退化为"编排者单线程执行"，不暴露失败或空路由
5. 当用户已启用角色时，编排层根据角色模板的分工配置自动路由任务
6. 同一会话中的角色卡片、工具卡片、时间轴位置、角色输出保持稳定
7. 页面刷新、会话切换、应用重启后，动态角色的消息与卡片不丢失
8. 不再依赖固定的 `claude / codex / gemini` 三个槽位名作为系统主语义
9. 不保留多套并存链路，最终运行态只有动态 Agent Registry 一条主链

## 4. 当前问题分析

### 4.1 表象分析

当前系统存在以下直接限制：

- 只能配置 `claude / codex / gemini` 三个 Worker
- 设置页模型配置、分工配置、底部 Worker Tabs 都是三槽硬编码
- 用户无法为"frontend / backend / document / review / test / integration"等更多分工创建独立角色
- 即使用户有十几个明确角色需求，也只能被压缩映射到 3 个固定执行槽位

### 4.2 链路机理

当前主链路实际是：

`固定 WorkerSlot 类型 -> 固定配置文件结构 -> 固定调度目标 -> 固定 projection workerRenderEntries -> 固定前端 tabs`

关键现状如下：

- `WorkerSlot` 在类型层被定义为 `'claude' | 'codex' | 'gemini'`
- `llm.json` 的 `workers` 结构是三路固定 key
- `worker-assignments.json` 也是三路固定映射
- `DispatchManager`、`DispatchRoutingService`、`PolicyEngine` 都依赖固定 Worker 列表
- `SessionTimelineProjection.workerRenderEntries` 是 `Record<WorkerSlot, ...>`
- 前端 `BottomTabs / ThreadPanel / AgentTab / messages.svelte.ts / worker-panel-state.ts` 全部以固定三槽展开

### 4.3 差距诊断

用户希望的产品形态是：

- "角色驱动"的团队
- "配置驱动"的角色数量
- "角色名称驱动"的 UI tab 和调度

而当前系统提供的是：

- "模型品牌驱动"的三槽执行器
- "硬编码驱动"的 UI 和配置
- "三选一槽位路由"的调度模型

二者不是同一抽象层。

### 4.4 根本原因分析

根因不是 Worker 数量少，而是系统把以下 4 个概念混成了 1 个概念：

1. 执行者身份
2. 模型品牌
3. 角色画像
4. UI tab 与运行态容器

在当前实现中，`claude / codex / gemini` 同时承担：

- 运行时身份
- 配置 key
- persona key
- 调度目标
- UI tab id
- 时间轴 worker lane 标识

这导致系统天然无法扩展为"任意数量的用户可选角色"。

## 5. 设计原则

### 5.1 三层分离

系统必须将以下三个概念拆为独立实体：

- **角色模板（RoleTemplate）**：系统内置，定义角色画像、分工归属、UI 默认配置
- **模型引擎（ModelEngine）**：用户配置，定义 LLM 参数、运行时策略
- **角色实例（AgentBinding）**：用户选用记录，绑定模板与引擎

禁止继续将角色画像、模型配置、分工路由混为一个实体。

### 5.2 身份稳定

角色实例的运行时身份就是 `templateId`，不需要额外生成 ID。

一个模板只能创建一个实例，因此 `templateId` 全局唯一且稳定，可直接作为：

- 调度目标
- timeline lane key
- 前端 tab panel key
- adapter 缓存 key
- 消息与卡片中的 agent 标识

### 5.3 配置驱动 UI

前端设置页角色列表、主线中的角色 tab、角色运行态、角色状态面板必须全部由 Agent Registry 驱动，而不是写死按钮和状态结构。

### 5.4 运行态按需激活

允许用户启用 10 到 15 个角色，但运行时不应在启动时一次性创建全部执行实例。

必须采用：

- 配置层可多角色
- 运行时按需创建 Adapter / Session
- 并发数独立治理

### 5.5 历史渲染稳定

即使某个角色之后被禁用，历史会话中已经产生的角色卡片、消息、时间轴 lane 仍必须稳定可读。

因此时间轴消息和卡片元数据中必须持久化角色展示快照，而不是仅保存一个外部引用。

### 5.6 禁止长期兼容双轨

允许一次性迁移旧三槽数据，但迁移完成后：

- 固定三槽链路必须移除
- 运行态只读取新 Registry
- 前后端只维护一套动态模型

## 6. 目标架构总览

### 6.1 总体链路

```
设置页：用户配置引擎 + 选用角色模板 + 绑定引擎
  → 保存 Agent Registry（engines[] + agents[]）
  → Agent Runtime 重载 Registry
  → RoleTemplate 提供 profile/ownerships
  → ModelEngine 提供 LLM 配置
  → Dispatch / AssignmentCompiler 依据 ownerships 路由到 agentId
  → 运行时按 agentId 惰性创建 Adapter（LLM 配置来自绑定的 Engine）
  → Message / Projection 使用 agentId + agentDisplaySnapshot
  → 前端动态构建 thread + 角色 Tabs
```

### 6.2 核心组件

目标架构新增或重构以下核心组件：

1. `RoleTemplateRegistry`（内置角色模板库）
2. `AgentRegistryStore`（用户配置存储：engines + agents）
3. `AgentRegistryLoader`（加载与校验）
4. `AgentRuntimeResolver`（运行时解析：agentId → 完整执行规格）
5. `DynamicAgentRoutingService`（动态路由）
6. `DynamicAgentAdapterFactory`（按需创建 Adapter）
7. `DynamicAgentTabProjection`（前端 tab 投影）

## 7. 数据模型设计

### 7.1 RoleTemplate（系统内置，只读）

```ts
interface RoleTemplate {
  /** 模板 ID，同时也是运行时 agentId */
  templateId: string;
  /** 展示名称（给用户看） */
  displayName: string;
  /** 一句话描述（给用户看） */
  description: string;
  /** 角色画像（给 LLM 看） */
  profile: {
    role: string;
    focus: string[];
    constraints: string[];
    outputPreferences?: string[];
    insightPreferences?: ('decision' | 'contract' | 'risk' | 'constraint')[];
  };
  /** 预绑定的分工分类（系统自动路由用） */
  ownerships: string[];
  /** 默认 UI 配置 */
  defaultUI: {
    colorToken: string;
    icon?: string;
  };
}
```


系统内置示例：

| templateId | displayName | ownerships | 说明 |
|------------|-------------|------------|------|
| `frontend-dev` | 前端开发 | `['frontend', 'style']` | 负责前端代码实现与样式 |
| `backend-dev` | 后端开发 | `['backend', 'api']` | 负责后端逻辑与 API 设计 |
| `reviewer` | 代码审查 | `['review', 'quality']` | 负责代码质量与安全审查 |
| `test-engineer` | 测试工程师 | `['test']` | 负责测试用例与质量保证 |
| `doc-writer` | 文档编写 | `['document']` | 负责技术文档与注释 |
| `debugger` | 调试专家 | `['debug']` | 负责问题定位与修复 |
| `integration-dev` | 集成开发 | `['integration']` | 负责模块集成与联调 |
| `security-analyst` | 安全分析 | `['security']` | 负责安全漏洞与合规 |

### 7.2 ModelEngine（用户配置）

```ts
interface ModelEngine {
  /** 引擎 ID，小写 slug，创建后不可修改 */
  id: string;
  /** 展示名称 */
  displayName: string;
  /** LLM 配置 */
  llm: LLMConfig;
  /** 运行时策略 */
  runtime?: {
    requestTimeoutMs?: number;
    stallPolicy?: {
      /** 用户可配字段（设置页暴露） */
      maxTotalRounds?: number;
      noOutputWarn?: number;
      noOutputAbort?: number;
      /** 高级字段（默认折叠，按需展开） */
      consecutiveFailThreshold?: number;
      totalFailLimit?: number;
      stallWarnLevel1?: number;
      stallWarnLevel2?: number;
      stallWarnLevel3?: number;
      stallAbortThreshold?: number;
      noOutputForce?: number;
    };
  };
}
```

### 7.3 AgentBinding（用户配置，极轻量）

```ts
interface AgentBinding {
  /** 引用的角色模板 ID（同时也是运行时 agentId） */
  templateId: string;
  /** 绑定的引擎 ID */
  engineId: string;
  /** 是否启用 */
  enabled: boolean;
  /** 排序序号 */
  order: number;
  /** UI 覆盖（可选，不填则使用模板默认值） */
  uiOverrides?: {
    visibleInTabs?: boolean;
  };
  /** 高级覆盖（可选，不填则使用模板默认值） */
  profileOverrides?: {
    focus?: string[];
    constraints?: string[];
  };
}
```

### 7.4 运行时解析类型

```ts
/** 运行时的完整 Agent 身份 */
type AgentId = string;  // 值等于 templateId

/** 运行时执行规格（由 AgentBinding + RoleTemplate + ModelEngine 三方合成） */
interface AgentExecutionSpec {
  agentId: AgentId;
  displayName: string;
  profile: RoleTemplate['profile'];
  ownerships: string[];
  llmConfig: LLMConfig;
  modelFamily: string;
  normalizerFamily: NormalizerFamily;
  stallPolicy: StallDetectionConfig;
  insightPreferences: ('decision' | 'contract' | 'risk' | 'constraint')[];
  ui: { colorToken: string; icon?: string };
}

/**
 * Normalizer 族：对齐 LLM API 协议，而非模型品牌
 *
 * - anthropic: Anthropic 流式事件协议（message_start / content_block_delta 等）
 * - openai:    OpenAI Responses API JSON 事件流（item / delta 结构）
 * - google:    Google AI 混合 JSON/文本格式
 *
 * 由 LLMConfig.provider 自动派生，用户不需要手动指定。
 */
type NormalizerFamily = 'anthropic' | 'openai' | 'google';

/**
 * 停滞检测完整配置
 *
 * 分两层暴露：
 * - 用户可配层：maxTotalRounds / noOutputWarn / noOutputAbort（设置页直接展示）
 * - 系统派生层：其余 7 个字段按 modelFamily 自动派生默认值
 *
 * 用户未显式配置的字段，由 deriveStallPolicy(modelFamily) 填充。
 */
interface StallDetectionConfig {
  consecutiveFailThreshold: number;
  totalFailLimit: number;
  stallWarnLevel1: number;
  stallWarnLevel2: number;
  stallWarnLevel3: number;
  stallAbortThreshold: number;
  maxTotalRounds: number;
  noOutputWarn: number;
  noOutputForce: number;
  noOutputAbort: number;
}
```

### 7.5 展示快照

用于持久化历史渲染快照：

```ts
interface AgentDisplaySnapshot {
  agentId: string;
  displayName: string;
  colorToken?: string;
  icon?: string;
}
```

### 7.6 关键决策

#### 决策 A：一个模板一个实例

每个 RoleTemplate 只允许创建一个 AgentBinding。

这意味着：

- `templateId` 就是 `agentId`，不需要额外 ID 生成
- 不存在同角色多实例的命名冲突
- 不存在同 ownership 多 Agent 的优先级仲裁
- 用户误操作空间最小

#### 决策 B：分工由角色模板内置，不开放用户自定义

用户不需要理解"ownerships"的概念。选了角色，分工自动确定。

系统内部流程：

- 用户选用 `frontend-dev` 模板
- 系统自动获取 `ownerships: ['frontend', 'style']`
- 编排者 prompt 自动注入当前已启用角色的分工全集
- 用户无需手动分配分工

#### 决策 C：engineId 不可编辑

ModelEngine 的 `id` 一旦创建即不可修改。

原因：

- `engineId` 被 AgentBinding 引用
- 如果允许修改，会导致引用断裂

#### 决策 D：角色画像与模型行为策略分层

动态架构下，不能再把角色画像、模型品牌行为矫正、运行时停滞策略、输出 Normalizer 绑定到同一个名称上。

必须拆成两层：

1. **角色层（Role Layer）**
   - 由 `RoleTemplate.profile` 定义
   - 决定该角色的职责、输出偏好、协作方式、分工边界、洞察偏好
   - 例如：`frontend-dev`、`reviewer`、`debugger`

2. **模型行为层（Model Behavior Layer）**
   - 由 `ModelEngine.llm` 与 `ModelEngine.runtime` 派生
   - 决定 normalizer、停滞检测、模型风格收敛策略、超时策略
   - 例如：Claude 类模型偏长推理、Codex 类模型偏执行、Gemini 类模型偏发散收敛

#### 决策 E：前端动态面板状态建模

面板键统一抽象为：

- `thread`
- `agent:${agentId}`

并优先使用 `Record<string, ...>` 或带统一 setter 的动态状态结构。

如果内部实现选择 `Map<string, ...>`，则必须满足：

- 不允许原地 `set/delete` 后指望自动渲染
- 每次更新都必须替换引用，确保 Svelte 5 追踪生效
- 持久化前必须有稳定的 `Map -> Record` 序列化

#### 决策 F：主页始终保留 thread 主线 tab，角色 Tabs 按就绪状态条件出现

- 主页 UI 始终有一个固定的主线 `thread` tab
- 当存在就绪角色时，这些角色自动出现在对应 Tabs 中
- 当不存在就绪角色时，主页只显示 `thread` tab

#### 决策 G：无角色模式是合法工作模式

系统必须支持两种等价的产品态：

1. **无角色模式**
   - 用户没有启用任何角色
   - 系统自动进入"编排者即执行者"模式
   - 所有任务由编排模型直接处理

2. **有角色模式**
   - 用户至少启用了一个角色（已绑定就绪引擎）
   - 编排者负责拆解与路由
   - 实际执行交给对应角色

这里"就绪角色"必须同时满足：

- 已启用
- 已绑定引擎
- 绑定的引擎配置完整（模型/API 校验通过）

只有就绪角色才能进入路由候选集合和前端 Tab 列表。

## 8. 配置存储设计

### 8.1 新配置结构

Agent Registry 统一存储在配置文件中：

```json
{
  "version": "3.0",
  "orchestrator": { "...": "..." },
  "auxiliary": { "...": "..." },
  "engines": [
    {
      "id": "claude-main",
      "displayName": "Claude 主力",
      "llm": { "provider": "anthropic", "model": "claude-sonnet-4-20250514", "...": "..." }
    },
    {
      "id": "gemini-fast",
      "displayName": "Gemini 快速",
      "llm": { "provider": "google", "model": "gemini-2.5-flash", "...": "..." }
    }
  ],
  "agents": [
    { "templateId": "frontend-dev", "engineId": "claude-main", "enabled": true, "order": 1 },
    { "templateId": "backend-dev",  "engineId": "claude-main", "enabled": true, "order": 2 },
    { "templateId": "reviewer",     "engineId": "gemini-fast", "enabled": true, "order": 3 }
  ]
}
```

特点：

- Agent 配置极轻量：每条只需 `templateId + engineId + enabled + order`
- 角色画像、分工、UI 配置全部来自系统内置模板，不需要用户填写
- 引擎配置集中管理，多个角色可引用同一引擎

### 8.2 旧配置去留

以下旧配置需要在迁移后退场：

- `~/.magi/worker-assignments.json`
- `~/.magi/llm.json` 中的 `workers.*` 固定三槽结构

保留：

- `orchestrator`
- `auxiliary`

### 8.3 RoleTemplate 存储

角色模板作为系统内置代码存储，不放在用户配置文件中：

```
src/orchestrator/profile/builtin/role-templates.ts
```

这样保证：

- 模板随版本更新自动升级
- 用户不会误改模板
- 新增模板只需发版，不需要用户手动操作

## 9. 后端架构设计

### 9.1 类型层改造

当前 `WorkerSlot = 'claude' | 'codex' | 'gemini'` 必须退场。

目标替换为：

```ts
type AgentId = string;           // 值等于 templateId
type EngineId = string;          // 引擎 ID
type SystemAgentId = 'orchestrator' | 'auxiliary';
type AnyAgentId = SystemAgentId | AgentId;
```

需要同步改造的核心模块：

- `src/types/agent-types.ts`
- `src/types.ts`
- `src/orchestrator/protocols/types.ts`
- `src/task/types.ts`
- `src/todo/types.ts`
- `src/session/*`
- `src/context/*`

### 9.2 Agent Registry Loader

新增 `AgentRegistryLoader`，职责：

- 加载配置文件中的 `engines[]` 和 `agents[]`
- 加载系统内置 `RoleTemplate` 库
- 验证 `engineId` 引用完整性
- 验证 `templateId` 在模板库中存在
- 合成就绪角色列表（`AgentBinding + RoleTemplate + ModelEngine → AgentExecutionSpec`）
- 提供 `ownerships → agentId` 路由映射

### 9.3 RoleTemplate Registry

新增 `RoleTemplateRegistry`，职责：

- 管理系统内置角色模板
- 提供模板查询接口
- 提供分工分类全集（聚合所有模板的 ownerships）

```ts
class RoleTemplateRegistry {
  getTemplate(templateId: string): RoleTemplate | undefined;
  getAllTemplates(): RoleTemplate[];
  getOwnershipMap(): ReadonlyMap<string, string>;  // ownership → templateId
}
```

### 9.4 Profile Loader

`ProfileLoader` 必须从"固定三 persona 构建器"改成"Registry 派生器"。

它的职责应变为：

- 从每个就绪角色的 `AgentExecutionSpec` 派生运行时画像
- 为 Prompt 注入生成统一的角色列表

不再依赖内置三槽 persona key。

### 9.5 Dispatch 与 Routing

#### 目标

调度目标从 `WorkerSlot` 改为 `agentId`。

#### 路由规则

建议维持：

- `ownership_hint`
- `mode_hint`

但编译器和路由服务的输出改为：

`ownership → agentId`

同时增加一条顶层模式规则：

- 当 `readyAgents.length === 0` 时，不进入角色路由模式，所有 ownership 统一回落到 `orchestrator`
- 当 `readyAgents.length > 0` 时，ownership 只能路由到就绪角色集合中的成员

#### 约束

- 每个 ownership 恰好归属一个角色模板（由系统内置保证，不存在冲突）
- 未启用角色不参与 routing
- fallback 不能再是固定三角矩阵，而要基于就绪角色的 ownerships 和 profile.focus 决定

#### Fallback 原则

- 默认不跨 ownership 自动降级
- 若无合法备选，则返回系统内部错误，由编排层重新规划

### 9.6 AdapterFactory 与 Runtime

#### 当前问题

当前 AdapterFactory、WorkerAdapter、Runtime 状态、执行统计都按三槽展开。

#### 目标

改为：

- 运行时按 `agentId` 惰性创建 Adapter
- Adapter 缓存 key 改为 `agentId`
- 创建 Adapter 时：`agentId → AgentExecutionSpec → 取 llmConfig → createClient`
- 停滞策略从固定预设改为 `AgentExecutionSpec.stallPolicy`

#### Adapter 创建流程

```
agentId
  → AgentRegistryLoader.resolve(agentId)
  → AgentExecutionSpec {
      llmConfig:      来自 ModelEngine.llm
      stallPolicy:    来自 ModelEngine.runtime.stallPolicy（用户配置）
                      + deriveStallPolicy(modelFamily)（系统默认填充）
      normalizerFamily: 来自 llmConfig.provider 自动派生
      profile:        来自 RoleTemplate.profile
    }
  → createLLMClient(llmConfig)
  → createNormalizer(normalizerFamily)
  → new WorkerLLMAdapter(...)
```

#### 设计要求

- `adapter-factory.ts → getStallDetectionPreset(workerSlot)` 退场
- 停滞策略优先读 `ModelEngine.runtime.stallPolicy`
- 若用户未显式配置，则按 `llm.provider + model pattern` 派生默认策略
- `createNormalizer(workerSlot, ...)` 退场，改为按 `normalizerFamily` 选择
- `normalizerFamily` 由 `LLMConfig.provider` 自动派生（`anthropic → ClaudeNormalizer`，`openai → CodexNormalizer`，`google → GeminiNormalizer`）
- 运行时不得再通过 `agentId` 猜测模型品牌

补充说明：

- Orchestrator 的 normalizer 选择不在本次改造范围内，始终使用 `ClaudeNormalizer`
- `GovernanceProfile.workerRoundsMultiplier` 的倍率应用逻辑不需要改动，只需确保 stallConfig 来源从 `getStallDetectionPreset(workerSlot)` 变为 `AgentExecutionSpec.stallPolicy`

### 9.7 ContextSource 与共享上下文归属

`autonomous-worker.ts → mapWorkerTypeToContextSource()` 当前把 Worker lane 归属和模型品牌来源混成了一个枚举。

设计上应改为：

- `ContextSource` 只表达大类：`orchestrator | agent | system`
- 具体归属通过上下文元数据表达：

```ts
interface ContextSourceMetadata {
  source: 'agent' | 'orchestrator' | 'system';
  agentId?: string;
  agentDisplayName?: string;
  modelFamily?: string;
}
```

对应约束：

- `mapWorkerTypeToContextSource()` 退场
- 共享上下文订阅与写入改为按 `agentId` 归属
- 历史显示依赖 `agentDisplaySnapshot`，不依赖品牌名 switch/case

### 9.8 洞察偏好

当前 `autonomous-worker.ts → getDefaultInsightTypes()` 按品牌名硬编码了洞察类型偏好。

正确设计：

- 洞察偏好来自 `RoleTemplate.profile.insightPreferences`
- 例如：`reviewer` 偏向 `['risk', 'constraint']`，`frontend-dev` 偏向 `['decision', 'constraint']`
- `getDefaultInsightTypes()` 退场，改为读取 `AgentExecutionSpec.insightPreferences`

### 9.9 契约提供方选择

当前 `contract-manager.ts → selectProducer()` 按品牌名硬编码优先级。

正确设计：

- 从 `participants`（即 `AgentId[]`）中按角色的 `ownerships` 与 `contractType` 的匹配度排序
- 不再依赖品牌名称做偏好排序

### 9.10 ToolManager 的 Agent 解析

当前 `tool-manager.ts → resolveWorkerSlot()` 通过硬编码猜测当前执行主体。

正确设计：

- 每次工具调用都带显式 `toolExecutionContext.agentId`
- ToolManager 通过 `agentId → AgentRegistry` 查出对应配置
- 不允许任何工具层自行从字符串猜测品牌

### 9.11 并发治理

允许启用 10 到 15 个角色，但应保留独立并发治理：

- 全局最大并发角色数
- 单角色最大并发数
- 写任务串行约束

## 10. 时间轴与持久化设计

### 10.1 Projection 结构

当前 `workerRenderEntries: Record<WorkerSlot, ...>` 必须改为动态结构：

```ts
interface SessionTimelineProjection {
  ...
  agentOrder: string[];
  agentRenderEntries: Record<string, SessionTimelineProjectionRenderEntry[]>;
}
```

其中：

- `agentOrder` 用于稳定恢复角色渲染顺序
- `agentRenderEntries` 使用动态 key（`agentId`）

### 10.2 消息与卡片元数据

角色相关消息、工具卡片、任务卡片、生命周期卡片元数据中必须统一包含：

- `agentId`
- `agentDisplaySnapshot`

原因：

- 当前 Registry 只代表"现在的配置"
- 历史会话需要依赖快照稳定还原过去的展示名称和颜色

### 10.3 会话恢复

页面刷新、切换会话、应用重启后，前端恢复链应按如下方式工作：

- 先从 bootstrap 中拿到当前 Registry
- 再从 timelineProjection 中拿到当前 session 的 agentOrder 和 agentRenderEntries
- 若历史 session 中存在当前 Registry 已禁用的角色，则以卡片中的 display snapshot 继续渲染只读 lane

## 11. 前端架构设计

### 11.1 设置页

#### 目标形态

设置页分为两个管理区域：

**引擎管理（模型配置）：**

```
┌─ 引擎管理 ────────────────────────────────┐
│ [+ 添加引擎]                               │
│                                            │
│ ┌ Claude 主力 ──────────────────────────┐  │
│ │ anthropic / claude-sonnet-4 / ✅ 就绪  │  │
│ └───────────────────────────────────────┘  │
│ ┌ Gemini 快速 ──────────────────────────┐  │
│ │ google / gemini-2.5-flash / ✅ 就绪    │  │
│ └───────────────────────────────────────┘  │
└────────────────────────────────────────────┘
```

**角色管理：**

```
┌─ 角色管理 ────────────────────────────────┐
│ [+ 添加角色]  ← 弹出角色模板选择器         │
│                                            │
│ ┌ 前端开发 ─────────────────────────────┐  │
│ │ 引擎: Claude 主力  ✅ 已启用           │  │
│ └───────────────────────────────────────┘  │
│ ┌ 后端开发 ─────────────────────────────┐  │
│ │ 引擎: Claude 主力  ✅ 已启用           │  │
│ └───────────────────────────────────────┘  │
│ ┌ 代码审查 ─────────────────────────────┐  │
│ │ 引擎: Gemini 快速  ✅ 已启用           │  │
│ └───────────────────────────────────────┘  │
└────────────────────────────────────────────┘
```

#### 添加角色交互

点击"添加角色"后弹出角色模板选择器：

```
┌─ 选择角色 ───────────────────────────────┐
│                                          │
│  ● 前端开发    负责前端代码实现与样式      │
│  ● 后端开发    负责后端逻辑与API设计       │
│  ● 代码审查    负责代码质量与安全审查      │
│  ● 测试工程师  负责测试用例与质量保证      │
│  ● 文档编写    负责技术文档与注释          │
│  ● 调试专家    负责问题定位与修复          │
│  ● 集成开发    负责模块集成与联调          │
│  ○ 安全分析    （灰色 = 已添加）          │
│                                          │
│           [确认]   [取消]                 │
└──────────────────────────────────────────┘
```

已添加的角色模板灰色不可选（一个模板一个实例）。

#### 角色卡片操作

每个角色卡片支持：

- 选择/切换引擎（下拉选择器，只展示已就绪的引擎）
- 启用/禁用开关
- 排序（拖拽或上下箭头）
- 删除（移除该角色实例）
- 高级设置（折叠区，可微调 focus、constraints）

#### 产品约束

1. 当没有就绪引擎时，角色可以添加但无法启用，提示"请先配置引擎"
2. 当角色绑定的引擎被删除时，该角色自动变为未就绪，提示重新选择引擎
3. 引擎删除前检查是否有角色引用，提示用户

### 11.2 主页信息架构

主页 UI 采用条件式 tab 结构：

- `thread` 始终存在
- `readyAgents.length > 0` 时，启用的角色自动追加到 Tabs 中
- `readyAgents.length === 0` 时，仅显示 `thread`

#### 空角色场景

当没有就绪角色时：

- 主线直接呈现编排者执行过程
- 不展示"等待角色"类空状态
- 只保留 `thread` tab
- 不暴露"请先配置角色才能运行"的结构性错误

### 11.3 Store 结构

以下前端状态必须从固定对象改为动态键状态结构：

- `scrollPositions`
- `scrollAnchors`
- `autoScrollEnabled`
- `agentRuntime`（原 `workerRuntime`）
- `modelStatus`
- `agentRenderEntries`（原 `workerRenderEntries`）

全部使用 `Record<string, ...>` 或等价的动态键状态结构。

#### 补充约束：runtime map 必须基于"有效角色集合"

当前 `deriveWorkerRuntimeMap()` 固定构造三路。

正确的运行态来源应为：

`effectiveAgentSet = 当前 Registry 就绪角色 + 当前 session 历史 lane`

其中：

- 当前 Registry 就绪角色：决定当前应展示哪些可交互 tabs
- 当前 session 历史 lane：保证禁用后的角色历史内容仍可回看

#### 补充约束：滚动状态的动态化

面板状态键必须使用稳定的 `panelId`：

- 主线面板：`thread`
- 角色面板：`agent:${agentId}`

### 11.4 视觉标识与卡片组件统一

建立统一的角色展示解析层：

`AgentDisplayResolver(agentId, agentDisplaySnapshot) → { label, icon, colorToken, colorValue }`

要求：

- 所有角色相关组件都必须走同一个展示解析器
- 全局 CSS 不再为 `claude/codex/gemini` 写死专属变量
- 改为按角色动态注入 CSS 变量，例如 `--agent-accent-${agentId}`
- 历史卡片优先使用消息中持久化的 `agentDisplaySnapshot`

### 11.5 组件改造范围

重点改造组件包括：

- `SettingsPanel.svelte`（两级管理：引擎 + 角色）
- `BottomTabs.svelte`（`thread + ready agent tabs`）
- `ThreadPanel.svelte`（主阅读面板）
- `AgentTab.svelte`（角色 tab 内容容器）
- `MessageList.svelte`
- `WorkerBadge.svelte` → `AgentBadge.svelte`
- `SubTaskSummaryCard.svelte`
- `stores/messages.svelte.ts`
- `lib/worker-panel-state.ts` → `lib/agent-panel-state.ts`
- `lib/message-classifier.ts`
- `lib/message-utils.ts`

## 12. Prompt 与编排层设计

### 12.1 Orchestrator Prompt

重构后应注入：

- `agentId`
- `displayName`
- `role`
- `focus`
- `assignedOwnerships`
- `engineLabel`（模型信息作为辅助元数据）

推荐的角色表格式：

| agentId | displayName | role | ownerships | engineLabel | availability |
|---------|-------------|------|------------|-------------|--------------|

并明确告诉编排者：

- 角色名称不是模型品牌
- 角色数量是动态的
- dispatch 的目标是 `agentId`
- 编排者做路由决策时优先看 ownership / role / focus / availability，不优先看品牌名

### 12.2 编排语义

建议继续保留当前的 `ownership_hint` 和 `mode_hint`，因为这套抽象对任务编译仍然有效。

但最终执行归属不再是三选一槽位，而是：

`ownership_hint → RoleTemplateRegistry.ownershipMap → agentId`

### 12.3 Persona 与模型行为矫正

拆成两层：

#### Persona Template（角色模板）

来自 `RoleTemplate.profile`，回答：

- 这个角色扮演什么职责
- 该角色如何协作
- 该角色偏好怎样的输出与交付方式
- 该角色默认产出的洞察类型偏好（`insightPreferences`）

例如：

- `frontend-dev` → `insightPreferences: ['decision', 'constraint']`
- `reviewer` → `insightPreferences: ['risk', 'constraint']`
- `debugger` → `insightPreferences: ['risk', 'decision']`

#### Model Behavior Policy（模型行为策略）

来自 `ModelEngine` 的 `llm.provider` 和 `runtime`，回答：

- 是否需要更强执行约束
- 是否需要收敛式提示
- 是否需要更严格的停滞检测
- 是否需要特定 normalizer 与输出解析规则

推荐实现为：

`finalAgentPrompt = personaTemplate(roleProfile) + modelBehaviorPolicy(modelFamily) + toolPolicy + taskContract`

产品价值在于：

- 用户换引擎不需要重新选择角色
- 同一角色绑不同引擎时，系统行为仍然合理
- 编排系统保持"角色驱动"，而不是"品牌驱动"

## 13. 迁移策略

### 13.1 一次性迁移

允许一次性迁移旧数据，但禁止长期双读双写。

迁移方式建议为：

1. 读取旧 `llm.json` 中的 `workers.claude / codex / gemini`
2. 读取旧 `worker-assignments.json`
3. 为每个旧 Worker 创建对应 ModelEngine：
   - `claude` → engine `claude-legacy`
   - `codex` → engine `codex-legacy`
   - `gemini` → engine `gemini-legacy`
4. 根据旧分工配置，推断应选用哪些角色模板并创建 AgentBinding
5. 启动后仅使用新 Registry

### 13.2 迁移后处理

迁移成功后建议：

- 将旧配置文件重命名为 `.bak`
- 或写入迁移标记并停止继续读取

关键原则是：

运行态不能继续保留"若新文件不存在则回退读旧文件"的永久兼容分支。

## 14. 实施顺序建议

### Phase 0：Schema 与 RoleTemplate 库

目标：

- 定义 `RoleTemplate`、`ModelEngine`、`AgentBinding` 类型
- 实现 `RoleTemplateRegistry`（系统内置角色模板库）
- 实现 `AgentRegistryLoader`（加载与校验）
- 实现 `AgentRuntimeResolver`（合成 `AgentExecutionSpec`）

### Phase 1：后端运行态与调度改造

目标：

- Dispatch / Routing / AdapterFactory / Runtime 改用 `agentId`
- 执行统计与运行态状态改为动态 Agent Map
- ContextSource / ToolManager / ContractManager 退场旧依赖

### Phase 2：时间轴与 projection 改造

目标：

- projection 改为动态 agentRenderEntries
- 消息元数据写入 agent display snapshot

### Phase 3：前端 stores 与 tabs 改造

目标：

- 消息 store 改为动态键状态结构
- 主线中的角色卡片与状态区域改为动态渲染
- 角色 runtime panel 改为动态渲染

### Phase 4：设置页与用户交互改造

目标：

- 引擎管理 UI（添加/编辑/删除）
- 角色管理 UI（从模板选用/绑引擎/启停/排序/删除）
- 角色模板选择器

### Phase 5：迁移与旧代码清理

目标：

- 迁移旧三槽配置
- 移除所有固定三槽常量和旧结构

## 15. 风险与约束

### 风险 1：运行时资源飙升

若启用 15 个角色并全部预创建 Adapter / Session，会放大内存、上下文缓存、执行统计结构、连接测试成本。

约束：

- 必须按需创建
- 必须有并发治理

### 风险 2：UI 过载

角色数量上升后，tabs、状态面板、设置页会变得拥挤。

约束：

- tabs 必须可滚动
- 设置页必须列表化

### 风险 3：历史会话失真

如果只保存 `agentId`，而不保存展示快照，当角色被禁用后，历史 session 可能失去可读性。

约束：

- 卡片与消息必须持久化 AgentDisplaySnapshot

### 风险 4：动态架构做成"表面动态、底层仍是三槽"

这类风险最隐蔽，底层以下位置仍然可能偷偷依赖三槽：

- stall detection preset
- normalizer 选择
- context source 映射
- ToolManager 模型解析
- agent runtime state 派生
- CSS 颜色与卡片展示
- persona 行为模板
- 洞察类型偏好（`getDefaultInsightTypes` 按品牌 switch/case）
- 契约提供方选择（`selectProducer` 按品牌名排优先级）

约束：

- 所有"按 worker 名 switch/case"的位置都必须进入本次改造清单
- 改造验收标准不能只看"能不能选用角色"，还要看"选用后的执行、渲染、恢复是否一致"

### 风险 5：中途双轨运行

如果改造期间同时保留固定三槽链和动态 Agent 链，会引入调度结果不一致、前端状态结构分叉、projection 恢复不稳定。

约束：

- 只允许短期迁移代码存在于开发分支
- 最终主干必须删除旧固定槽位链路

### 风险 6：引擎被删除但角色仍引用

用户删除引擎后，绑定该引擎的角色变为未就绪。

约束：

- 删除引擎前提示受影响的角色列表
- 角色自动变为未就绪状态，不允许静默保留不可执行的绑定

## 16. 验收标准

以下标准全部满足，才视为设计目标达成：

1. 系统内置多种角色模板，用户可从模板库中选用
2. 用户可添加/编辑/删除模型引擎
3. 角色绑定就绪引擎后，在主线区域自动出现对应 tab
4. dispatch 输出目标为 `agentId`，不再依赖固定三槽
5. projection / persistence / restore 支持动态角色
6. 页面刷新、会话切换、应用重启后，动态角色消息和卡片不丢失
7. 历史会话中的已禁用角色仍能以快照形式稳定展示
8. 无角色模式下系统正常工作（编排者单线程执行）
9. 代码库中不再存在固定三槽的运行态主链结构

## 17. 最终建议

本次改造应被定义为：

`固定三槽 Worker 架构 → 角色驱动的动态 Agent Registry 架构`

而不是：

`在现有三槽基础上继续打补丁扩容`

核心范式转换：

- **旧**：Worker = 模型品牌 = 角色 = 配置 = Tab
- **新**：RoleTemplate（系统内置角色） + ModelEngine（用户配置引擎） + AgentBinding（选用记录）

用户操作极简：

1. 配引擎（几台机器）
2. 选角色（从模板库选）
3. 绑引擎（选一台机器）
4. 启用 → 完成

如果后续进入实施阶段，建议先产出一份"按模块拆分的实施清单"，逐项列出：

- 类型替换清单
- RoleTemplate 库设计清单
- 配置迁移清单
- Dispatch / Runtime 改造清单
- Projection / Session 改造清单
- 前端 Store / Tabs / Settings 改造清单

然后按 Phase 0 到 Phase 5 单链推进，不允许中途回到固定三槽模型。