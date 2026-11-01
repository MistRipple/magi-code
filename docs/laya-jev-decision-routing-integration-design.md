# Magi Laya/Jev 决策路由接入方案

> 状态：方案草案，尚未进入实现。在线 Provider 的开发以 §16 P0「离线基线与回放」的结论为前置条件；基线没有显示明确的收益方向前，不编写在线接入代码
>
> 更新日期：2026-11-01
>
> 适用范围：通过 HTTP 接入远程或本地 Laya/Jev 兼容服务，用低延迟的类型化决策减少 Magi 对主 LLM 进行路由判断的次数、输入规模和模型成本。

## 1. 摘要

Magi 不内置 Laya/Jev 模型，也不把它实现成插件。用户配置一个 HTTP 地址后，daemon 可选地调用外部决策服务。未配置时，Magi 完全沿现有模型、工具、权限和任务链路运行；配置后也不默认把所有决策交给 Laya，而是按决策类型逐项启用。

接入目标是**降低 LLM 路由成本**，不是增加一个事后审查模型。Laya/Jev 应在昂贵的主模型调用之前完成有限集合中的类型化选择：是否需要主模型、选择哪个模型等级、开放哪些工具、浏览器下一步动作是什么、是否需要检索或是否值得开放子代理。只有能跳过一次 LLM 调用、改用更便宜的模型、缩小工具/上下文输入时，才计入效率收益。

Magi 仍然拥有全部业务事实和安全边界：Session、Turn、Task、模型执行快照、工具权限、审批、BrowserAuthority、lease、fence、TaskStore、canonical 事件、恢复和终态。Laya/Jev 的输出是外部不可信的决策建议或候选动作，不是权限、事实或终态。

**前置条件。** 本方案的收益目前是假设，不是 Magi 的实测。启用任何在线 head 之前，必须先完成离线基线（§10.4、§16 P0）：用现有用量账本和 canonical 轨迹大致看清各决策点在主模型调用、token 和失败中的占比，并在 Magi 自己的数据上回放评估。收益只需要判断到“值得做 / 不值得做 / 说不准”这个粒度，不追求精确数字。没有 Magi 专属模型、held-out 评估，也看不出对缓存没有明显负面影响，就没有 `gated`；通用 Laya 最多停留在离线回放或本机 `shadow`。

核心链路如下：

```text
用户输入 / 当前执行状态
            │
            ▼
      Magi 确定性预筛选
      （权限、候选、工具面、上下文预算）
            │
            ▼
      DecisionRouter
            │
            ├─ 未配置 / 未启用 ───────► 现有 Magi 路径
            ├─ shadow ───────────────► 记录结果，不改变行为
            ├─ advisory ────────────► 缩小候选或提示主模型
            └─ gated ───────────────► 高置信度时采用，否则回退
            │
            ▼
     HTTP Laya/Jev Provider
            │
            ▼
   类型化结果 + 置信度 + abstain
            │
            ▼
 Magi 原有模型、权限、执行和恢复链路
```

## 2. 外部依据与适用边界

Laya 官方资料把工具选择、模型等级选择和是否需要复核列为决策模型的典型用途：小模型负责快速路由，复杂步骤再交给强模型。[Agent tool routing](https://laya.studio/use-cases/agent-tool-routing)

Laya 的公开接口支持 `choice`、`score` 和 `noul` 三类类型化问题，并可在一次前向计算中回答多个问题。[Laya GitHub README](https://github.com/NandhaKishorM/laya#readme)

Laya 的浏览器专门微调实验说明，浏览器候选应作为选项列表输入，页面标题、URL、历史和有限页面文本作为状态；报告的模型在候选元素 top-1、动作分类和真实任务成功率上明显优于通用零样本版本，但这是领域微调结果，不能外推为通用 Laya 能力。[浏览器 Agent 微调报告](https://github.com/NandhaKishorM/laya/blob/main/docs/finetune_browser_agent.md)

Laya Guard 展示了另一类接口：在工具执行前判断 `allow`、`ask` 或 `block`，并输出危险性、注入、外传、参数是否符合用户意图等分数；其文档同时建议把确定性规则放在前面，模型只处理规则覆盖不到的长尾。[Laya Guard](https://laya.studio/docs/guard)

公开评测也给出了必须纳入设计的限制：Laya 对选项顺序、相似候选和高基数候选较敏感，零样本模型路由不稳定；如果先筛选再调用 LLM，实际端到端节省可能显著低于只看路由器命中率得到的估计。[Fast Models, Slow Evidence](https://arxiv.org/abs/2610.02267)

因此本方案不把 Laya/Jev 视为通用“意图分类器”，而把它视为**针对 Magi 决策头训练和校准的外部 System-1 Provider**。

上述外部资料是撰写本方案时的引用，未在 Magi 仓库内复现；其中的指标不能作为 Magi 的预期收益。尤其“端到端节省可能显著低于路由器命中率”一条，要求本方案以端到端调用数、token 和缓存命中率为准（§10）。

## 3. Magi 当前架构约束

### 3.1 daemon 是唯一业务内核

`magi-daemon` 拥有 Session、Turn、Task、工具执行、审批、持久化、恢复和终态。UI、Electron Main、Browser Worker 和外部 Provider 只能消费或请求这些事实，不能复制事实或绕过授权入口。

### 3.2 并非所有路由都需要 Laya

当前 `TurnService` 对命令、工作流、恢复、目标模式和 GPT Web 等入口已经有确定性分类。对这些路径增加 Laya 不会减少 LLM 调用，反而增加一次 HTTP 请求。

真正适合 Laya 的是主模型目前通过自然语言或工具调用完成的有限选择：

| 决策 | 当前责任 | 是否接入 Laya | 对 Prompt 缓存的影响 |
| --- | --- | --- | --- |
| Session/Turn 基础路由 | daemon 确定性规则 | 不接入 | 无 |
| 模型、推理强度 | 用户在会话中选择；角色显式绑定引擎；`agent_spawn` 只在已绑定模型的能力内选强度 | 暂缓：先要有产品决定（§7.1） | 切换模型会换 lineage，重写前缀 |
| 工具面 | Magi 按策略、Skill、访问档位生成；工具表稳定排序；工具面哈希进入 `cache_epoch` | 暂缓：需通过缓存准入（§7.2、§10.4） | 变更工具表会使前缀失效 |
| 浏览器动作和候选节点 | 主模型逐步选择 | 离线评估优先（§7.3） | 不改前缀；效果是减少主模型步数 |
| 是否派发子代理 | 主模型语义判断，daemon 预检 | 优先评估，仅 advisory，提示写入易变尾部（§7.5） | 不改工具表则无影响 |
| 知识/记忆是否检索 | ContextRuntime 和知识查询 | 可评估，规则版优先 | 检索结果进入历史或尾部，不改前缀；须核对注入位置 |
| 重复动作、无进展 | 尚无专门机制 | 先做确定性规则（§7.6） | 无 |
| 权限、审批、Browser lease | PermissionEngine、BrowserAuthority | 不接管 | — |
| 工具执行结果和 Task 终态 | 原生执行与 TaskStore | 不接管 | — |
| 上下文压缩边界 | Context Authority | 只提供相关性信号 | — |

### 3.3 BrowserAuthority 不能被绕过

浏览器工具使用节点引用、`navigationRevision`、Surface、lease、fence 和真实 outcome。Laya 只能选择候选动作或候选节点，最终执行必须仍经过 `BrowserToolRuntime`、BrowserAuthority、Desktop Control 和 Automation Worker。

当前浏览器快照默认最多 96 个节点、10 KiB 文本；候选构造必须在该预算内完成，不能把整页原文或敏感输入值发送给外部 Provider。[浏览器运行时设计](browser-runtime-design.md)

### 3.4 子代理合同不能被 Laya 替代

`agent_spawn` 已有 `purpose`、`reason`、`parent_work`、角色、Skill、工作区、模型快照、重复工作和生命周期预检。Laya 可以减少主模型面对的候选空间，但不能直接创建 Task、伪造用户授权或替代运行时预检。[子代理唤起设计](subagent-delegation-design.md)

### 3.5 缓存、模型归属与数据外发约束

以下三条来自现有设计，任何 head 都必须先满足：

1. **Prompt 缓存前缀。** [Prompt 缓存分析与优化](prompt-cache-optimization.md)规定稳定规则、工具面、模型/权限绑定共同计算 `cache_epoch`（工具面哈希见 `prompt_lineage.rs`）。工具表位于历史之前，所以改变工具表、系统前缀或模型的决策会换 lineage、重写前缀。同一文档记录的本机历史样本中，主对话输入 token 命中率约 58%（2026-10-05 至 10-31，含验收请求，不是纯生产流量）；这部分是优化成果，不能被路由决策反复打断。Decision 结果因此只能写入易变尾部（`RoundContext`），或在 epoch 本来就会变化的 Turn/会话边界采用；中途改变工具面或模型必须通过 §10.4 的缓存准入。
2. **模型归用户与角色绑定。** 主对话模型由用户在会话中选择，角色可显式绑定引擎，`agent_spawn` 不接受调用级模型覆盖。Decision 结果不能静默替换用户选择的模型或绑定。
3. **数据外发最小化。** Magi 是本地优先的个人产品。决策请求包含用户目标、页面标题和候选文本，即使只做 `shadow` 也是真实的数据外发，必须有独立的用户同意和字段范围说明（§5.3、§11.2）。

## 4. 目标与非目标

### 4.1 目标

1. 减少主 LLM 进行工具、模型、上下文和浏览器动作路由的次数。
2. 对简单请求选择便宜模型或低推理强度，复杂请求才使用高成本模型。
3. 对浏览器步骤，在具备领域模型时直接替代主 LLM 的动作选择调用。
4. 缩小主模型可见的工具集合、候选集合和知识上下文。
5. 用统一 HTTP 合同同时支持远程 Jev/Laya 和本地 Laya 服务。
6. 用 `shadow` 数据测量真实的端到端收益，而不是只测分类准确率。
7. 让所有决策都可追踪：输入摘要、候选集合、Provider、模型摘要、延迟、置信度、是否采用和回退原因。

### 4.2 非目标

1. 不在 Magi 安装包中携带模型权重、Python、ONNX 或推理服务。
2. 不把 Laya/Jev 注册成普通聊天模型或普通插件工具。
3. 不让 Laya 授予文件、Shell、网络、Browser、MCP 或 Task 权限。
4. 不让 Laya 直接写 TaskStore、SessionStore、canonical 事件或审批记录。
5. 不让 Laya 直接决定不可逆副作用、权限拒绝或任务完成。
6. 不在没有 Magi 领域数据和 held-out 评估时声称“更快”或“更正确”。

## 5. 集成模式

### 5.1 Provider 类型

首期只有一种传输类型：`http`。地址可以指向远程服务或本机服务，Magi 不区分模型是否本地运行。

```text
DecisionRouter
    └── HttpDecisionProvider
          ├── Laya/Jev-compatible /v1/systemone
          └── 可选 provider-specific guard endpoint
```

Magi 内部使用自己的 `DecisionRequest` / `DecisionResponse`，由 HTTP 适配层转换到外部协议。这样不会让 Magi 的业务代码依赖某个供应商的 JSON 字段，也可以在未来接入其他 HTTP 决策服务。

### 5.2 行为模式

| 模式 | 行为 | 适用阶段 |
| --- | --- | --- |
| `off` | 不发请求、不健康检查、不加载 Provider | 默认状态 |
| `shadow` | 请求并记录结果，真实路径不变 | 数据收集、回放、校准 |
| `advisory` | 结果用于缩小候选、提示主模型或选择候选模型 | 低风险试点 |
| `gated` | 高置信度且满足本地规则时采用，否则回退 | 生产低风险路径 |
| `required` | 当前决策必须有 Provider 结果；不可用时停止 | 仅内部实验，不作为默认产品模式 |

`required` 不允许用于权限、审批、不可逆写操作或终态。远程服务不可用时不应导致 Magi 普通聊天整体不可用。

### 5.3 远程和本地地址

- 本地地址默认允许 `http://127.0.0.1`、`http://localhost` 和用户明确配置的 loopback 地址。
- 远程地址默认要求 HTTPS；如果允许 HTTP，UI 必须明确标记为不安全连接。
- API Key 使用现有安全凭据引用，不写入普通设置、日志、请求指纹或 canonical Turn。
- 不允许通过 Provider URL 访问 Magi daemon 的管理端口、内部 API 或 Electron 控制端口。
- 远程地址的 `shadow` 默认关闭：只有用户在设置中明确同意，并看到将发送的字段范围后才发送；loopback 地址不需要额外同意，但同样受 §11.2 的脱敏约束。`shadow` 不产生用户可见收益，不能以“收集数据”为由默认外发。
- Provider 请求使用独立的连接超时、读取超时和总预算；不能继承主模型的长超时。

## 6. 内部决策合同

### 6.1 请求

```json
{
  "schemaVersion": 1,
  "decisionId": "browser_action.v1",
  "requestId": "decision-20261101-0001",
  "mode": "gated",
  "state": {
    "userGoal": "打开设置页面",
    "page": {
      "urlOrigin": "https://example.com",
      "title": "Example",
      "navigationRevision": 7
    },
    "history": ["已打开首页"]
  },
  "questions": {
    "operation": {
      "type": "choice",
      "instructions": "选择下一步浏览器操作",
      "criteria": {
        "click": "点击当前页面中的一个候选元素",
        "type": "向输入控件输入用户要求的文本",
        "select": "选择下拉选项",
        "done": "目标已经完成，不再执行动作"
      }
    },
    "target": {
      "type": "choice",
      "instructions": "选择最符合用户目标的候选元素",
      "criteria": {
        "node_17": "button 设置，当前可见，文本为 Settings",
        "node_23": "a 帮助，当前可见，文本为 Help"
      }
    }
  },
  "candidates": ["node_17", "node_23"],
  "redaction": {
    "removedSecrets": true,
    "removedSensitiveInputs": true
  }
}
```

### 6.2 响应

```json
{
  "schemaVersion": 1,
  "requestId": "decision-20261101-0001",
  "provider": "laya-http",
  "model": "magi-browser-v1",
  "modelDigest": "sha256:...",
  "answers": {
    "operation": {
      "kind": "choice",
      "value": "click",
      "confidence": 0.96,
      "probabilities": {
        "click": 0.96,
        "type": 0.01,
        "select": 0.01,
        "done": 0.02
      }
    },
    "target": {
      "kind": "choice",
      "value": "node_17",
      "confidence": 0.93,
      "probabilities": {
        "node_17": 0.93,
        "node_23": 0.07
      }
    }
  },
  "abstain": false,
  "latencyMs": 24,
  "usage": {
    "inputTokens": 312,
    "questions": 2
  }
}
```

Provider 返回的 `value` 只能引用请求中已有的候选 ID。Magi 拒绝不存在的候选、未知问题、非法概率、非有限数字、超时结果和缺少模型摘要的 gated 结果。

### 6.3 批量和单次多问题

同一执行点需要多个问题时必须合并成一次 HTTP 请求，例如同时判断：

- 是否需要 Browser；
- 浏览器动作类型；
- 是否应使用强模型；
- 是否需要人工复核。

不允许为每个布尔问题各发一次远程请求。批量请求需要保留每个问题的独立结果和阈值，不用一个总分掩盖某个关键问题的不确定性。

## 7. 决策头设计

### 7.1 `magi.route.v1`

用途：在一次 Turn 开始前选择执行路径和模型等级。

候选示例：

```text
direct_small_model
main_model
strong_model
browser_controller
knowledge_first
needs_user_input
```

限制：只能选择用户已经配置、角色允许、当前任务可用的模型档位；不能改变角色绑定、Provider 凭据或已启动 Task 的模型快照。

接入位置：主模型 Step 准备前，早于模型请求冻结。模型选择一旦写入执行快照，本次 Task 后续轮次沿快照执行。

产品前置决定：Magi 目前没有“模型档位”概念，模型由用户选择、由角色绑定。上面的 `direct_small_model` / `strong_model` 只是示意，在产品没有决定之前，本 head **只做离线评估，不进入在线路径**。若决定引入，应满足：

- 是用户显式开启的“自动选档”，档位（如快速/默认/强）由用户在设置里映射到已配置的模型，不自动选用用户没有配置的模型；
- 只在会话首轮或新 lineage 本来就开始的位置采用，已有会话中途不切换，避免丢失前缀；
- UI 如实展示本次实际使用的模型和强度，用量按实际模型归因；
- 低置信度、Provider 不可用时沿用用户当前选择，不降级。

### 7.2 `magi.tool_surface.v1`

用途：缩小主模型可见的工具集合。

候选先由 Magi 根据 Task policy、Skill、访问档位、Browser capability、MCP 和插件激活状态生成；Laya 只能在候选集合中选择一个工具类别或工具子集。

不能使用 Laya 重新加入被策略禁止的工具。工具执行仍经过同一权限、审批和审计入口。

缓存约束：工具表变化会使前缀失效（§3.5）。因此首选形态是**不改工具表**，把 Laya 的结论作为尾部提示（例如“本轮优先考虑某类工具”）；只有离线证明“工具 token 节省 > 缓存损失”（§10.4）时，才允许缩小工具表，并且只在 Turn 或会话边界、`cache_epoch` 本来就会变化的位置生效。禁止按步骤动态增删工具。

### 7.3 `magi.browser_action.v1`

用途：直接替代浏览器步骤中的主 LLM 路由。

输入应采用“短状态 + 候选列表”结构：

- 用户目标的短摘要；
- 页面 origin、标题和 URL 的非敏感投影；
- 最近有限动作历史；
- Worker 生成的候选节点 ID、role、文本、当前状态和可见性；
- 当前 `navigationRevision` 仅作为回执绑定，不作为可被模型修改的动作参数。

输出为 `operation + candidateId + confidence`。Magi 再进行完整的节点引用、revision、lease、访问档位和真实 outcome 校验。

前提条件：Magi 的浏览器表示（默认 96 节点、10 KiB 文本的自有快照格式）与公开微调实验的输入格式不同，公开结果不能外推。因此：

1. 先用 canonical 轨迹中真实成功的 `element_ref + operation` 构建标注集，在 Magi 自己的候选格式上离线评估；
2. 浏览器步骤要拆开统计：“选择动作/候选”可以被替代，“理解页面内容、判断目标是否达成”仍需主模型；`done` 之类的目标完成判断不由本 head 决定（§15）；
3. 在没有 Magi 专属模型和 held-out 成功率没有明显低于基线的证据前，不进入线上 `gated`；通用 Laya 只在离线回放或本机 `shadow` 中使用；
4. 不可逆或带副作用的页面操作（提交、支付、删除、发送）不进入 gated，仍交给主模型和现有授权。

### 7.4 `magi.context_need.v1`

用途：决定是否需要知识、记忆、搜索或额外上下文。

它只决定“查什么”和“是否查”，不决定最终写入模型上下文的内容。ContextRuntime 继续负责知识范围、工作区、源目录、预算和稳定排序。

### 7.5 `magi.delegation_gate.v1`

用途：减少主模型无意义地选择 `agent_spawn`。

候选为：

```text
no_delegate
parallel_work
context_isolation
independent_review
```

当用户明确要求真实代理时，Laya 不能否决；当用户未明确要求时，Laya 只能以 advisory 提示（写入易变尾部）影响主模型是否派发；不通过增删 `agent_spawn` 工具定义实现，避免改变工具表而使缓存前缀失效（§3.5）。实际 `delegation.reason`、角色、上下文包、Task 注册和生命周期仍由原有入口负责。

该 head 是首个建议评估的对象：`agent_spawn` 已有结构化的 `delegation.reason`，任务终态、产物、整合成本和重复工作拒绝（`existing_task_id`）都有真实事实可作为标签，不需要额外人工标注。[子代理唤起设计](subagent-delegation-design.md)也明确要求用固定回放评估不必要派发率；本 head 的离线评估可与该回放合并。

### 7.6 `magi.execution_progress.v1`

用途：在多步执行中判断是否出现重复动作、无进展、目标漂移、应切换工具或应升级模型。

该头只能触发“重新评估”或“请求主模型”，不能直接宣布完成、重放未知副作用或自动创建新代理。

重复动作和无进展先用确定性规则实现（基于 `tool_execution_ledger` 的同工具同参数重复、连续无新证据计数等），这条规则版是模型版必须超过的对照基线；规则版已能覆盖的情形不引入 Provider 调用。

### 7.7 不纳入首期的决策头

- 权限允许/拒绝；
- 审批结果；
- Browser lease 和用户控制权；
- 文件/Shell/网络实际执行；
- Task 完成和失败终态；
- canonical 上下文压缩边界；
- 用户请求的最终语义解释。

## 8. 运行时接入架构

### 8.1 新增抽象

首期不新增 crate。当前工作区已有 30 余个 crate，只有一种传输和一个 Provider 时，再拆两个新 crate 属于过早拆分。建议：

```text
magi-conversation-runtime
  decision/
    DecisionProvider (trait)
    DecisionRouter
    DecisionRequest / DecisionResponse
    DecisionPolicy
    DecisionTrace

magi-bridge-client
  decision_http
    HttpDecisionProvider
    Laya/Jev protocol adapter
    timeout / cancellation / redaction / health
```

决策类型与聊天模型的 `ModelBridgeClient` 分离、不共用响应事实模型；仅复用 `magi-bridge-client` 中已有的 HTTP 客户端、超时和凭据引用能力。当出现第二个 Provider 实现、或第二个独立消费者时，再按真实依赖抽出独立 crate。

聊天模型是生成内容和工具调用的流式协议；Laya/Jev 是固定候选上的类型化判断，二者不能共用同一个“模型响应”事实模型。

### 8.2 建议接入点

| 模块 | 接入方式 |
| --- | --- |
| `magi-api` | 读取和保存全局 Decision Provider 配置、连接测试和状态 DTO |
| `magi-conversation-runtime` | 在模型 Step 前调用 route/tool-surface/context 头；在执行循环中调用 progress 头 |
| `magi-api/src/browser_tool_runtime.rs` | 在候选节点已由 Worker 生成后调用 browser-action 头 |
| `magi-conversation-runtime/src/tool_surface_state.rs` | 生成授权工具面后，允许 Laya 只做候选内缩小 |
| `magi-context-runtime` | 允许 context-need 影响检索入口，不改变知识事实 |
| `magi-tool-runtime` | 记录决策建议和采用结果，不允许 Provider 直接改权限结果 |
| `magi-session-store` | 保存运行快照中的 Provider/model digest 和 adopted decision metadata；不保存 API Key |

### 8.3 生命周期

```text
读取设置
  → 验证 URL、协议和凭据引用
  → 可选健康检查
  → 新 Turn 固定决策配置快照
  → 每个决策头生成候选
  → HTTP 请求
  → 校验响应
  → 置信度/abstain/策略门槛判断
  → 采用或回退
  → 写入 DecisionTrace
```

配置修改只影响新 Turn。已经开始的 Task 继续使用原 Provider 配置、头版本和模型摘要；若运行中 Provider 消失，按该 Task 的回退策略收口，不静默改变事实。

## 9. 配置设计

建议新增全局 `decisionProvider` 设置，具体字段以最终 App Server schema 为准：

```json
{
  "enabled": true,
  "transport": "http",
  "baseUrl": "http://127.0.0.1:8765",
  "protocol": "systemone",
  "credentialRef": "secret:decision-provider/laya",
  "defaultMode": "off",
  "allowRemoteShadow": false,
  "connectTimeoutMs": 300,
  "requestTimeoutMs": 120,
  "healthCheck": false,
  "heads": {
    "magi.route.v1": {"enabled": false, "mode": "off"},
    "magi.tool_surface.v1": {"enabled": false, "mode": "off"},
    "magi.browser_action.v1": {"enabled": false, "mode": "off"},
    "magi.context_need.v1": {"enabled": false, "mode": "off"},
    "magi.delegation_gate.v1": {"enabled": false, "mode": "off"}
  }
}
```

配置约束：

- `enabled=false` 时不发任何请求；
- 示例中所有 head 默认 `off`：head 只有在通过 §10.4 对应准入后才逐项打开；`allowRemoteShadow=false` 时，远程地址不发送 `shadow` 请求，仅 loopback 可用；
- `baseUrl` 必须是 HTTP(S) origin，禁止嵌入凭据、路径穿越和非 HTTP scheme；
- 远程地址默认 HTTPS；loopback 地址允许 HTTP；
- 每个 head 独立开关和模式；
- 不保存明文密钥；
- 不把 Provider 连接失败伪装为“模型没有做出决定”；
- UI 显示 `disabled`、`configured`、`healthy`、`degraded`、`unavailable`，不只显示一个布尔开关。

## 10. 效率模型

每个 head 必须回答一个具体的效率问题：

```text
net_saving
  = avoided_llm_calls
  + cheaper_model_savings
  + input_token_savings
  - decision_http_latency_cost
  - fallback_cost
  - wrong_route_recovery_cost
  - cache_loss
```

其中 `cache_loss` 指因前缀失效而多付的输入成本。这些项只用于判断方向和量级，不要求精确核算；但缓存损失不能被忽略。

不能用“Laya 分类准确率”代替端到端收益。至少记录以下基线和实验指标：

### 10.1 调用和成本

- 每 Turn 的主 LLM 调用次数；
- 每个浏览器任务的 LLM 步数；
- 主模型、worker、辅助模型和决策 Provider 的 p50/p95 延迟；
- 输入、输出、缓存读写 token；
- 小模型和高档模型的占比；
- 每个 head 造成的 fallback 次数。

### 10.2 决策质量

- 候选 top-1 和 top-k；
- 置信度校准误差；
- abstain 后的成功率；
- 错误工具选择率；
- 浏览器真实任务成功率；
- 不必要的子代理派发率；
- 目标漂移、重复动作和错误升级率。

### 10.3 端到端结果

- 用户请求到最终结果的 p50/p95 延迟；
- 任务完成率；
- 未确认副作用数量；
- 重试和人工接管次数；
- Prompt cache 命中变化；
- 失败任务的恢复成本。

如果 `Laya latency + fallback latency` 大于被省掉的 LLM 路由成本，则该 head 不应进入 gated。

### 10.4 在线启用准入条件

每个 head 单独、依次满足下列条件，才能进入对应阶段：

| 进入阶段 | 必须满足 |
| --- | --- |
| 编写在线 Provider 代码 | P0 离线基线显示该决策点在主模型调用、token 或失败中占比不可忽略，且粗略判断收益大于成本 |
| `shadow`（线上数据流） | 有明确的数据最小化字段表；loopback 或用户已同意远程外发；有离线评估之外仍需线上数据的理由 |
| `advisory` | 离线 held-out 上建议整体不比无建议差；建议只写入易变尾部，不改变工具表、系统前缀或模型；缓存命中率没有明显下降 |
| 改变工具表或模型的采用 | 粗略判断“节省明显大于 `cache_loss`”；只在 Turn/会话边界采用；有用户可见的开关 |
| `gated` | Magi 专属模型；held-out 与顺序扰动测试通过；真实任务成功率没有明显低于基线；主 LLM 调用数或成本有可感知的下降；fallback 后总延迟没有系统性变差 |

## 11. 数据、训练和校准

### 11.1 数据来源

优先使用 Magi 已有的真实 canonical 轨迹和执行事实：

- 用户输入与最终完成合同；
- 模型实际工具调用；
- Browser 快照、候选节点和 outcome；
- Task/Turn/Agent 的真实终态；
- 工具失败、重试、取消和未知副作用；
- ContextRuntime 的检索结果和最终采用情况。

决策标签必须来自结果事实，而不是只来自模型文字。例如浏览器标签应来自真实成功的 `element_ref + operation`；子代理标签应来自任务完成时间、独立性、产物和整合成本；工具路由标签应来自工具实际是否解决目标。

### 11.2 数据脱敏

发送到远程 Provider 前：

- 移除密码、令牌、Cookie、API Key、Authorization header 和敏感表单值；
- 默认只发送 origin，不发送完整 URL query；
- 文件路径按工作区相对路径或稳定摘要表示；
- 工具参数按 head 所需字段最小化；
- 页面文本、工具结果和用户消息按 head 的最小字符预算裁剪；
- 请求日志只保存 hash、大小和决策摘要，不保存原始敏感状态。

### 11.3 校准

每个 head、候选规模、语言和 Provider/model digest 独立评估。阈值不能从一个 head 复制到另一个 head，也不能把公开 benchmark 的置信度直接当作 Magi 生产阈值。

必须有：

- 训练集、校准集、held-out 测试集；
- 选项顺序扰动测试；
- 候选描述改写测试；
- 相似候选测试；
- 缺失候选和截断状态测试；
- Provider 超时和错误回退测试。

## 12. 决策采用策略

每个 head 采用同一套通用策略，但阈值由 head 配置：

```text
1. 生成合法候选
2. 调用 Provider
3. 校验候选、概率、模型摘要和请求绑定
4. 检查 abstain、置信度、分布熵和稳定性
5. 检查本地硬规则
6. 满足 gated 条件才采用
7. 否则回退原路径
```

“高置信度”不能只看最大概率。至少可组合：

- 最大候选概率；
- 第一候选与第二候选的差距；
- 选项顺序扰动稳定性；
- 当前候选数量；
- 是否存在截断；
- Provider/model digest 是否已评估；
- 当前动作是否可逆。

低置信度时优先回退到主模型；高风险或不可逆动作不因高置信度自动放行。

## 13. 回退和故障合同

| 故障 | `shadow` | `advisory` | `gated` |
| --- | --- | --- | --- |
| 未配置 | 不调用 | 不调用 | 不调用 |
| HTTP 超时 | 记录 | 忽略建议 | 回退原路径或等待主模型 |
| 连接失败 | 记录 | 忽略建议 | 回退；不伪造通过 |
| 响应非法 | 记录错误 | 忽略建议 | 回退并记录 provider_invalid_response |
| 低置信度 | 记录 | 不采用 | abstain，回退或请求用户 |
| 候选已过期 | 记录 | 不采用 | 拒绝采用，重新获取状态 |
| 模型摘要变化 | 记录 | 不采用 | 需要重新评估或回退 |

浏览器写操作一旦已经交给原生 Host，Laya 超时不能让调用方自动重放。未知副作用沿现有 Browser 失败合同处理。

## 14. 决策事实与审计

DecisionTrace 是审计和评估投影，不是第二套业务事实。建议包含：

```text
decision_id
request_id
session_id / task_id / turn_id / step_id
head_id / head_version
provider_origin_hash
model_id / model_digest
candidate_ids_hash
input_shape_hash
mode
answer / probabilities summary
confidence / abstain
adopted
fallback_reason
latency_ms
created_at
```

它可以作为 canonical Step 的 metadata 或现有事件的附加投影保存，但不能单独证明工具成功、Task 完成、用户授权或 Browser 副作用已发生。

## 15. 安全边界

1. Laya/Jev 的结果全部视为外部输入。
2. Provider 不能访问 Magi 管理 API、审批 API 或 Electron 控制 API。
3. Provider 不能通过响应返回新的工具定义、权限、Task ID 或路径范围。
4. 远程请求必须经过最小化和脱敏；用户明确关闭远程决策时不发送内容。
5. Browser 内容、文件内容和下游工具结果可能包含 prompt injection，不得提升为用户指令。
6. Provider 的 `allow` 不等于 Magi 的权限允许；PermissionEngine 和审批仍是最终入口。
7. Provider 的 `done` 不等于 Magi 的 Task completion；完成合同仍必须验证证据。

## 16. 分阶段实施

阶段按“先证明有收益，再写线上代码”排序。每个阶段有退出条件，未满足则不进入下一阶段，方案可以在任一阶段止步。

### P0：离线基线与回放（不接任何服务，不外发数据）

- 从用量账本和 canonical Step/ToolCall 轨迹统计：每 Turn 主调用数、浏览器任务步数、各工具调用频次与失败率、`agent_spawn` 次数及后果、模型或工具面变化引起的 `cache_epoch` 变化次数；
- 把浏览器步骤拆成“选择动作/候选”与“理解页面”，估算 `browser_action` 理论上可替代的主调用比例；
- 以 `delegation.reason`、任务终态、产物、整合成本和重复工作拒绝为标签，建立子代理委派标注集，并与子代理设计中的固定回放合并；
- 重复动作、无进展先实现确定性规则，作为后续模型必须超过的对照；
- 在本机用可用模型（含通用 Laya 本地实例）对标注集离线回放，报告 top-1、校准误差、选项顺序扰动稳定性；
- 退出条件：至少一个 head 看起来值得做，且缓存损失可接受；否则方案止步，不编写 Provider。

### P1：协议与 `shadow`（仅对通过 P0 的 head）

- 在 `magi-conversation-runtime` 增加 Decision 类型与 trait，在 `magi-bridge-client` 增加 `HttpDecisionProvider`；
- 全局配置、凭据引用、连接测试；
- 只支持 `shadow`；远程地址默认关闭，需用户同意（§5.3）；
- DecisionTrace、脱敏、候选 hash；未配置时零请求；
- 退出条件：线上 `shadow` 与离线结论一致，且没有产生缓存命中率回退。

### P2：`advisory`（影响主模型，不改前缀）

- 优先 `delegation_gate`：建议写入易变尾部，不改 `agent_spawn` 工具定义；
- `context_need`：仅影响检索入口，且先与规则版比较；
- 只有在存在 Magi 专属浏览器模型时才评估 `browser_action` 的 advisory；
- 退出条件：不必要派发率或无效检索下降，缓存命中率、任务成功率没有明显变差。

### P3：`gated` 与专属模型

- `browser_action`：需 Magi 专属模型、held-out 评估与真实任务成功率没有明显低于基线，且主 LLM 步数有可感知的下降；
- `route`（模型档位）：先有产品决定（§7.1），再只在会话首轮采用；
- `tool_surface`（改工具表）：先通过 §10.4 的缓存准入，且只在 Turn/会话边界采用；
- 任何 head 进入 gated 都需发布独立评估报告。

### P4：持续训练与校准

- 从 Magi canonical 轨迹持续构建 browser、delegation、route、progress 数据集；
- 温度校准、选项顺序稳定性、候选描述改写、相似候选和截断状态测试；
- 对每个 head、候选规模、语言和模型摘要分别发布评估；
- 通用 Laya 通过测试不等于 Magi 专属 gated 可以启用。

## 17. 验收标准

### 17.1 协议与配置

- 未配置时零 HTTP 请求；
- 配置错误不会改变普通 Magi 路径；
- 明文凭据不出现在 settings、日志、事件和请求指纹；
- 本地 HTTP 与远程 HTTPS 都能完成连接测试；
- Provider 超时、取消、重启和失联有明确状态。

### 17.2 效率

- 每个启用 head 有独立 baseline 和 shadow 报告；
- gated 需要看到端到端 LLM 调用数、token、延迟或成本有可感知的改善；
- 不能只用 Provider latency 或分类准确率作为成功标准；
- fallback 后的总延迟不能系统性高于原路径。

### 17.3 浏览器

- 过期 `navigationRevision`、lease、fence 和候选引用全部拒绝；
- Laya 不能执行未在候选列表中的节点；
- 用户接管、导航、取消和 Guest 崩溃时不会重放旧动作；
- gated 浏览器模型的真实任务成功率不低于当前基线，并且 LLM 步数确实下降。

### 17.4 子代理

- 用户明确要求代理时不能被 Laya 否决；
- Laya 建议不能绕过 `agent_spawn` Schema、角色、Skill、工作区、重复工作和生命周期预检；
- Laya 失败不产生虚构的 child Task；
- 子代理建议只有在 held-out 轨迹中减少不必要派发时才进入 gated。

### 17.5 回归入口

实现后至少增加：

```bash
npm run protocol:check
cargo check -p magi-api
cargo test -p magi-conversation-runtime context_authority
cargo test -p magi-browser-authority
cargo test -p magi-tool-runtime
npm --prefix web run check
```

真实 Provider 验收必须使用隔离状态根和脱敏测试数据，分别覆盖本地 HTTP、远程 HTTPS、超时、非法响应、低置信度、候选过期和回退路径。

### 17.6 离线基线与缓存

- P0 报告包含：各决策点在主调用、token 和失败中的大致占比，浏览器步骤大致可替代的部分，以及 `cache_epoch` 变化的大致情况；
- 报告明确给出每个 head 是否进入下一阶段及理由，未通过的 head 不留下在线代码；
- 任何会改变工具表、系统前缀或模型的采用路径，都有采用前后缓存命中率的对照；
- 远程 `shadow` 默认关闭，关闭时零外发；开启后外发字段与字段表一致。

## 18. 结论

Magi 接入 Laya/Jev 的价值成立的前提，是它能够替代或缩小真实的 LLM 路由工作。最有潜力的路径不是 Session 基础分类，也不是把 Laya 当作普通工具，而是：

1. 浏览器动作头直接替代每步主 LLM 的动作选择；
2. Turn 边界模型等级和工具面路由，减少高成本模型和工具输入；
3. 知识、记忆和上下文需求路由，减少无效上下文；
4. 子代理和执行进展建议，减少无效委派和重复循环。

所有路径都必须遵守同一个原则：**Laya 可以减少 Magi 需要让 LLM 做的路由判断，但不能减少 Magi 自己必须执行的权限、事实、恢复和副作用校验。**

落地顺序上，本方案接受的是“先量化、再接入”：P0 的离线基线决定哪些 head 值得做、是否值得做。其中模型档位和工具面路由受 Prompt 缓存与用户模型选择约束，优先级低于浏览器动作和子代理委派；若 P0 看不出明确收益，止步是合格的结论。
