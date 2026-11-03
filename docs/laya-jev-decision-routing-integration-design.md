# Magi Laya/Jev 决策路由接入方案

> 状态：方案草案，尚未进入实现。在线 Provider 的开发以 §16 P0「离线基线与回放」的结论为前置条件；基线没有显示明确的收益方向前，不编写在线接入代码
>
> 更新日期：2026-11-02
>
> 适用范围：通过 HTTP 接入远程或本地 Laya/Jev 兼容服务，用低延迟的类型化决策减少 Magi 对主 LLM 进行路由判断的次数、输入规模和模型成本。

## 1. 摘要

Magi 不内置 Laya/Jev 模型，也不把它实现成插件。用户最多配置一个 HTTP 决策服务；该服务可以是 Laya，也可以是 Jev，二者是同一接入位的互斥实现。未配置时，Magi 完全沿现有模型、工具、权限和任务链路运行；配置后也不默认把所有决策交给 Laya 或 Jev，而是按决策类型逐项启用。

接入目标是**降低 LLM 路由成本**，不是增加一个事后审查模型。Laya/Jev 应在昂贵的主模型调用之前完成有限集合中的类型化选择：是否需要主模型、选择哪个模型等级、开放哪些工具、浏览器下一步动作是什么、是否需要检索或是否值得开放子代理。只有能跳过一次 LLM 调用、改用更便宜的模型、缩小工具/上下文输入时，才计入效率收益。

Magi 仍然拥有全部业务事实和安全边界：Session、Turn、Task、模型执行快照、工具权限、审批、BrowserAuthority、lease、fence、TaskStore、canonical 事件、恢复和终态。Laya/Jev 的输出是外部不可信的决策建议或候选动作，不是权限、事实或终态。

**前置条件。** 本方案的收益目前是假设，不是 Magi 的实测。启用任何在线 head 之前，必须先完成离线基线（§10.4、§16 P0）：用现有用量账本和 canonical 轨迹大致看清各决策点在主模型调用、token 和失败中的占比，并在 Magi 自己的数据上回放评估。收益只需要判断到“值得做 / 不值得做 / 说不准”这个粒度，不追求精确数字。每个 head 还须指出接入后省掉的具体 LLM 请求或输入（§7.8）；没有通过 Magi 场景的 held-out 评估、版本身份核验和缓存收益检查，就没有 `gated`。是否需要专门微调由评测决定，通用 Laya、Jev 和微调模型采用相同准入标准。

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
  HTTP Laya 或 Jev Provider（全局唯一实例）
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

因此本方案不把 Laya/Jev 视为通用“意图分类器”，而把它视为**按 Magi 决策头验证、校准，并按需微调的外部 System-1 Provider**。

### 2.1 公开浏览器结果对 Magi 的实际启示

Laya 仓库的浏览器微调报告比通用宣传更适合拿来做工程假设：在约 45 个候选元素、每步三个问题的 held-out 数据上，元素 top-1 约为 0.63–0.66，单步延迟约 17–50 ms；16 个真实任务的三次运行通过率为 50%–62%。同一报告也明确记录了类型后联想、需要先滚动的分页和 Google Flights 等任务连续失败。[浏览器微调报告](https://github.com/NandhaKishorM/laya/blob/main/docs/finetune_browser_agent.md)

这对 Magi 有两个直接结论：

1. **延迟差可能足以支持控制片段。** 如果本地 HTTP 服务保持类似几十毫秒级延迟，而原始主模型轮次是秒级，连续动作段的 wall time 和 ModelStep 数有机会同时下降；但必须把交接、回退和最终核对算进去。
2. **通用模型不能直接宣称可用。** 报告中的收益来自特定浏览器输入格式和专门微调，不是零样本 Laya 的一般能力；Magi 的候选节点、快照裁剪、可信值引用和页面状态必须按自己的轨迹重新训练/校准。报告还显示“候选放在 option 列表、页面正文留在短状态”比单纯增加数据更关键，这与 Magi 的候选表设计方向一致，但仍需在 Magi 数据上复现。

Laya 自己的工具路由说明也给出了与 Magi 直接相关的边界：工具超过约 20 个时建议分层路由；状态窗口很短，约 320 或 768 token；零样本工具准确率在真实工具上未知，必须用真实 agent step 标注；概率在采用前需要温度校准。[Laya 的路由限制与集成说明](https://github.com/NandhaKishorM/laya#limitations-of-decision-model-routing-for-agents)

Magi 已经在外部工具路径上采用了部分相同的确定性分层：基础工具先按策略、Skill、访问档位和宿主能力生成，MCP/插件工具默认延迟到 `tool_catalog` 成功后再加载。对这类路径再增加一个 Laya “选工具”请求，只有在能减少错误的 `tool_catalog`/模型轮次或显著缩短输入时才有增量价值；单纯把现有分层重新分类没有价值。

因此公开结果只能支持“浏览器控制片段值得做 P0”，不能支持“直接在线 gated”。它不能替工具面、知识检索或子代理 head 提供同等强度的收益证据。

上述外部资料是撰写本方案时的引用，未在 Magi 仓库内复现；其中的指标不能作为 Magi 的预期收益。尤其“端到端节省可能显著低于路由器命中率”一条，要求本方案以端到端调用数、token 和缓存命中率为准（§10）。

## 3. Magi 当前架构约束

### 3.1 daemon 是唯一业务内核

`magi-daemon` 拥有 Session、Turn、Task、工具执行、审批、持久化、恢复和终态。UI、Electron Main、Browser Worker 和外部 Provider 只能消费或请求这些事实，不能复制事实或绕过授权入口。

### 3.2 并非所有路由都需要 Laya

Session/Turn 接纳等已经由 daemon 规则完成的路径不消耗 LLM 路由调用；对它们增加 Laya 只会增加一次 HTTP 请求。主模型生成一次响应时也常常同时完成理解、规划、路由和参数构造，不能把其中一项判断替换后就算作少了一次 LLM 请求。

真正适合 Laya 的是主模型目前通过自然语言或工具调用完成的有限选择：

| 决策 | 当前责任 | 是否接入 Laya | 对 Prompt 缓存的影响 |
| --- | --- | --- | --- |
| Session/Turn 基础路由 | daemon 确定性规则 | 不接入 | 无 |
| 模型、推理强度 | 用户在会话中选择；角色显式绑定引擎；`agent_spawn` 只在已绑定模型的能力内选强度 | 暂缓：先要有产品决定（§7.1） | 切换模型会换 lineage，重写前缀 |
| 工具面 | Magi 按策略、Skill、访问档位生成；工具表稳定排序；工具面哈希进入 `cache_epoch` | 暂缓：需通过缓存准入（§7.2、§10.4） | 变更工具表会使前缀失效 |
| 浏览器动作和候选节点 | 主模型逐步选择 | 离线评估优先（§7.3） | 不改前缀；效果是减少主模型步数 |
| 是否派发子代理 | 主模型语义判断，daemon 预检 | 对照实验，仅 advisory；须证明减少后续子任务或模型轮次（§7.5） | 尾部不改变稳定前缀，但增加输入和 HTTP 延迟 |
| 知识/记忆是否检索 | ContextRuntime 和知识查询 | 可评估，规则版优先 | 检索结果进入历史或尾部，不改前缀；须核对注入位置 |
| 重复动作、无进展 | 工具账本已有幂等结果复用、重复失败检测和未知副作用保护；更高层语义停滞尚待评估 | 复用现有规则，只评估未覆盖的语义停滞（§7.6） | 账本本身不改前缀；模型建议和后续动作须实测 |
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

### 3.6 先回答“替代了哪一次模型调用”

在 Magi 当前的执行循环中，一轮通常会构造 `ModelInvocationRequest`，再由 `StepModelClient` 创建并发送一个 canonical ModelStep；模型返回工具调用后，工具执行、结果写回和下一轮模型请求继续循环。这个事实决定了 Laya 接入的价值边界：

- Laya 只有在**阻止一次原本会创建的 `StepModelClient` 请求**时，才算减少主模型调用；
- 在同一个模型请求已经选好工具之后再发一次 Laya HTTP，只是增加一个辅助请求，不能把它记成“省掉了一次 LLM”；
- 一次模型响应可以批量返回多个工具调用，不能把每个工具调用都折算为一个可省的模型请求；
- 如果 Laya 只是把建议追加到下一轮 prompt，主模型仍然会被调用，收益最多是输入缩短或错误率下降，不能宣称减少调用。

因此，接入价值按下列顺序判断，而不是按“分类准确率”判断：

```text
可替代的原 ModelStep
  × 原路径占比
  × Laya 在 Magi 候选格式上的可靠采用率
  − Decision HTTP、交接、回退、重试和恢复开销
  − 缓存失效与额外输入成本
```

这个式子只是 P0 的筛选模型，不是线上计费公式。任何 head 在 P0 中都必须把分子落到真实的 ModelStep/transport 记录，不能从 prompt 中猜测“其中有一部分是在做路由”。

### 3.7 按 Magi 当前代码的价值排序

下面是工程优先级假设，数字不是已测结果；它们用于决定先测什么，不能替代 §16 的回放和对照。

| 方向 | 当前 Magi 是否已有确定性实现 | Laya 能否直接省掉主模型请求 | 实际价值假设 | 当前建议 |
| --- | --- | --- | --- | --- |
| 浏览器有界控制片段 | 没有；当前由对话循环逐轮请求模型，再调用浏览器工具 | **可以**，但必须由主模型先交出有界子目标 | 在同一页面连续点击、填写和翻页时，替代“观察快照→选下一动作”的中间 ModelStep；这是唯一具备明确省调用路径的首选 head | 先做离线回放，再做隔离站点的受控 gated 实验 |
| 大工具面/候选工具族 | 工具面已由策略、Skill、权限和缓存约束生成 | 通常不可以；同一模型响应中仍由主模型选工具 | 可能减少工具定义 token，或在下一轮减少错误工具重试；动态删表会损失缓存 | 只测 Turn/会话边界的稳定缩小；默认不改工具表 |
| 知识/记忆需求 | `ContextRuntime` 已有意图词规则、受治理查询、类型过滤和选择缓存 | 不可以；现有路径没有独立“是否检索”的 LLM 请求 | Laya 可能提高长尾相关性，但会增加 HTTP；只有能减少注入 token 或后续纠错轮次才有价值 | 先与现有规则做增量对照，未证明节省输入就不接入 |
| 模型档位/推理强度 | 模型由用户或角色绑定，当前没有自动选档产品 | 不可以；最多改变后续模型成本 | 只有新建用户认可的档位产品后，才可能用更便宜模型完成简单 Turn | 暂不做在线 head |
| 子代理委派 | 主模型决定，daemon 做完整预检和注册 | **当前不可以**；advisory 不消除当前主模型请求 | 只有后续 child Task、模型轮次或整合成本下降才有净收益，且容易被交接开销抵消 | 只保留对照实验，不作为首个落地点 |
| 语义无进展 | 工具账本已处理幂等、重复失败和未知副作用 | 通常不可以；Laya 仍需触发下一次判断 | 可能减少重复的后续模型轮次，但规则已覆盖的部分没有增量价值 | 先审计规则漏项，默认不调用 Laya |

按这个排序，Laya 对 Magi 的真实定位不是“全局智能路由层”，而是一个只在**高频、闭集、可回执、能跳过 ModelStep**的局部控制器。其余方向只有在实验显示增量收益后才值得投入。

## 4. 目标与非目标

### 4.1 目标

1. 减少主 LLM 进行工具、模型、上下文和浏览器动作路由的次数。
2. 在用户显式开启、并满足缓存与模型归属约束（§3.5、§7.1）的前提下，对简单请求选择更便宜的模型档位；默认不改变用户选择的模型。
3. 对浏览器步骤，在具备领域模型时直接替代主 LLM 的动作选择调用。
4. 缩小主模型可见的工具集合、候选集合和知识上下文。
5. 用统一 HTTP 合同同时支持远程 Jev/Laya 和本地 Laya 服务。
6. 用离线回放和对照实验测量端到端收益，而不是只测分类准确率；`shadow` 只用于观测响应质量、覆盖率和额外延迟（§10.3）。
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

首期只有一种传输类型：`http`，且全局至多保存一份 Provider 配置。地址可以指向远程服务或本机服务，Magi 不区分模型是否本地运行。Laya 和 Jev 都是该接入位的外部实现；协议字段不一致时，由各自的 HTTP adapter 转换到 Magi 的内部 `DecisionRequest` / `DecisionResponse`，业务 runtime 不直接依赖供应商字段。

```text
DecisionRouter
    └── HttpDecisionProvider
          └── Laya adapter 或 Jev adapter（按 protocol 二选一）
                └── 唯一已配置的 HTTP 决策服务
```

Magi 内部使用自己的 `DecisionRequest` / `DecisionResponse`，由 HTTP 适配层转换到外部协议。这样不会让 Magi 的业务代码依赖某个供应商的 JSON 字段，也可以在未来接入其他 HTTP 决策服务。

Laya 和 Jev 不同时配置、不做双 Provider 投票、不做跨 Provider failover。当前配置的 Provider 超时、失联或结果不可采用时，直接回到 Magi 原路径。

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

### 5.4 Provider 兼容与能力准入

本方案要支持的是 **Laya、Jev 这一类 HTTP 决策服务**。Laya 与 Jev 都是首批适配目标；业务 head 使用统一的内部合同，后续同类服务通过适配器扩展。当前仍是方案，源码尚未实现任何在线 Decision Provider，不能把这里的支持目标描述成已经可用。

- HTTP 地址、协议适配和模型能力分别配置、分别验证；同为 `/v1/systemone` 不保证鉴权、字段、版本、错误响应或分数语义完全相同。协议标识指向 Magi 的适配器，不由品牌名或 URL 自动猜测；实际兼容性按服务版本的请求/响应样本验证。
- 文章使用的是 OpenRouter 的 `POST /api/alpha/decisions`，请求包含 `model`、`state`、`questions`，并非 `/v1/systemone` 或聊天接口；[OpenRouter 官方示例](https://openrouter.ai/blog/tutorials/how-to-use-jev/)也说明了这一点。因此单配置需能选择相应 HTTP adapter，并显式指定托管模型 ID；不能只靠替换 baseUrl 假定兼容。
- 对当前配置的 Provider 记录支持的问题类型、一次多问题能力、上下文与候选上限、版本身份和可用的概率字段；能力来自服务说明、显式配置及连接测试，不假设存在统一的能力发现接口。不支持某个 head 的必要能力时，禁用该 head 并回到原路径，不静默丢字段或截断候选。
- 置信度、概率分布、校准参数和采用阈值绑定到具体 Provider、有效模型/服务版本及 head。不能把 Laya 的阈值复制给 Jev，也不能把一个 Provider 的 `confidence` 直接当作另一个 Provider 的正确率。
- Laya 的公开浏览器微调结果只支持对该 Laya 版本的实验假设，不能用来准入 Jev。Jev 也有使用动态 operation/target 候选、仅在需要生成输入文本时调用小 LLM 的公开浏览器示例，但该示例不是 Magi 的实测。[Jev Ultrafast](https://github.com/browser-use/jev-ultrafast)。两者均须在同一组 Magi 任务、候选和执行合同上独立评估；协议兼容不代表所有 head 都适用。
- `shadow` 仍是单个已配置 Provider 的观测模式：与 Magi 原路径对照，不参与真实决策，不表示另行配置一个旁观模型。比较 Laya 和 Jev 时分别运行离线评估，不在一次在线执行中同时调用两者。

### 5.5 借鉴 Jev harness：按生命周期接入三类判断

参考这篇 [Pi SDK + Jev harness 实践](https://wujiaming88.github.io/2026/09/23/pi-sdk-jev-harness.html)及其 [DAIR.AI 原文](https://academy.dair.ai/resources/jev-decisions-in-a-pi-sdk-harness)，可以按模型请求前、工具执行前和交付检查三个边界设计 Router、Gate、Verifier。它提供的是结构参考，文中的示例阈值和运行结果不能直接作为 Magi 的准入证据。下面是结合 Magi 源码提出的采用方式，不是这三类能力已经实现的声明。

| harness 位置 | Magi 对应边界 | 能解决什么 | 是否直接减少主 LLM 请求 | Magi 的采用边界 |
| --- | --- | --- | --- | --- |
| Router | 新 Turn/新 lineage 的 `StepModelClient` 冻结前 | 在用户允许的模型档位中选更便宜或更强的模型 | 不能保证减少次数，主要是降低单次成本；当前模型档位产品尚不存在 | 只在会话首轮或 lineage 本来要切换时运行一次，不能每轮切换模型 |
| Gate | 经 schema 校验的最终工具参数、原审批和副作用执行之前 | 提供动作与可信目标是否一致等语义风险信号 | 当前没有独立 LLM Gate 可直接省掉，新增检查属于质量/安全开销 | 不改权限结论；若采用信号，由原生策略决定是否补查或进入原交互入口，不能由外部 `allow` 覆盖拒绝 |
| Verifier | 已有候选答复和真实证据、Task/Turn 尚未结算的边界 | 提供漏答、证据与论断不一致的有限语义信号 | 只有替代已有昂贵复核调用，或对照确认减少后续返工时才有净收益 | 先满足 completion evidence、required tool chain 等硬合同；语义信号不提交终态、不证明事实为真 |

Magi 可以复用的结构是：Provider 返回类型化信号，由可测试的 `DecisionPolicy` 结合原生事实决定是否采用；同一观测点的问题合并请求；故障回退、版本和实际用量分别记录。不能把路径是否越界、工具是否执行成功等已有精确答案再交给概率模型。直接把多个未校准分数取最大值也不构成联合风险概率。

现有接线也有边界：`extension_hooks.rs` 的工具钩子在账本、权限和审批前接受提案，改写后重新校验；多个改写者存在时，新增语义检查必须看最终参数，之后参数变化使判断失效。`model_step.rs` 的 `model.request` / `model.response` 当前是事实提交后的 observe 通知，不能据此阻止调用、切换模型或重开 Turn。本文仍采用原生 runtime 接入，不因借鉴 hooks 就改成插件实现。

故障回退不能照搬示例：Router 失败沿用户已选模型，不擅自换强模型；可选 Gate 失败回到原生权限和审批，原本需审批的动作仍需审批，不把服务失联扩大成所有工具不可用；Verifier 不可用或不自信时不增加通过标记，保留原完成合同，也不无限补查。同一 Decision HTTP 不自动重试，恢复沿原循环的既有限制。

Verifier 值得增加一个离线研究项：给出可信任务条款、候选答复和带 itemId/状态的有界真实证据，分别问“哪项要求可能遗漏”“当前证据是否足以判断”“是否需要定向复核”，不问笼统的“任务完成了吗”。证据不足、正文截断或低置信度表示无法判断；不把它当作通过或失败。疑点由宿主绑定回原条款/证据，必要时交给原主线作定向补查，不自动创建 reviewer 子代理。若进入采用实验，补查须共用原 completion recovery 上限、不能重放已成功副作用；当前答复可能已经流式展示，需沿原消息追加纠正事实，不能宣称实现了交付前隐藏审核。

本研究项不新增默认在线 head。P0 先核对是否真的存在可替代的额外复核或返工，计入误报造成的补查、漏报、额外 HTTP 和证据准备；现有硬检查已经解决的样本没有增量收益。浏览器片段仍是直接省 ModelStep 的候选，Router 主要省单次模型费用，Gate/Verifier 则分别证明自身的质量收益和净成本，不能把三者合并宣称提速。

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
        "return_to_llm": "停止本段快速选择，把最新事实交还主模型核对"
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
        "return_to_llm": 0.02
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

Provider 返回的 `value` 只能引用对应问题的 `criteria` 中已有的候选 ID。Magi 拒绝不存在的候选、未知问题、非法概率、非有限数字、超时结果和模型/服务版本身份未通过准入的 gated 结果。`return_to_llm` 不使用 `target` 答案，也不表示目标或 Task 完成；其他动作必须同时验证操作与目标的兼容性。

示例是内部归一化合同，不要求外部 `/v1/systemone` 原样返回这些字段。`requestId` 由 HTTP 调用上下文绑定；`abstain` 可以由宿主按 head 策略计算；延迟以宿主端到端测量为准。`modelDigest` 是服务提供真实权重摘要时的可选字段，不是所有托管 Provider 都有的能力，不能用模型展示名或 URL 哈希冒充权重摘要。

准入身份至少关联 Provider 配置 revision、明确的模型/服务版本、head/schema 版本和评估/校准版本。可验证的固定模型版本或服务发布标识可以替代权重摘要；仅有浮动别名、无法识别服务升级时，不开放 gated，保留离线或 shadow。身份变化使原准入失效，旧任务不静默切换到未评估版本。

### 6.3 批量和单次多问题

同一执行点、同一 Provider 需要多个问题时，必须在该 Provider 支持的能力和上限内合并成一次 HTTP 请求；必要的多问题能力缺失时回到原路径，不为适配而逐问题串行追加请求。例如同时判断：

- 是否需要 Browser；
- 浏览器动作类型；
- 是否应交还主模型核对；
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

缓存约束：工具表变化会使前缀失效（§3.5）。因此首选形态是**不改工具表**，把 Laya 的结论作为尾部提示（例如“本轮优先考虑某类工具”）；只有按 §10.4 粗略判断“工具 token 节省明显大于缓存损失”时，才允许缩小工具表，并且只在 Turn 或会话边界、`cache_epoch` 本来就会变化的位置生效。禁止按步骤动态增删工具。

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

1. 先把 canonical 轨迹中的 `element_ref + operation + outcome` 关联到当时快照和子目标，构建待复核样本；执行成功只产生弱标签，按 §11.1 核对目标相关性后才进入评测集；
2. 浏览器步骤要拆开统计：“选择动作/候选”可以被替代，“理解页面内容、判断目标是否达成”仍需主模型；`done` 之类的目标完成判断不由本 head 决定（§15）；
3. 只有模型/服务版本可核验、在 Magi 输入格式上通过 held-out 与真实任务评估后，才进入 gated；通用或专门微调均可，尚未通过准入的模型只参与离线回放或获准的 shadow；
4. 提交、支付、删除、发送等不可逆或有外部副作用的动作不进入 gated，交回主模型和现有授权。节点文本或 Laya 风险分数不能单独证明一次点击可逆；无法确定影响范围时也交回。

#### 7.3.1 主模型交出的是有界子目标

现有 `browser_*` 工具仍执行明确动作，不在 `browser_click` 或 `browser_snapshot` 内暗中扩展成自动控制循环。计划中的快速浏览器控制由原生 conversation loop 编排，进入点位于下一次 `StepModelClient` 请求冻结与发送之前：

```text
主模型理解总目标，提交一个有界 Browser 子目标
    → 原 Task/Turn 固定子目标、允许动作、输入值来源和返回条件
    → 获取当前快照和合法候选
    → HTTP Decision 选择动作和节点（不创建 LLM Step）
    → 原工具调度、账本、权限和 BrowserAuthority 校验并执行
    → 保存真实 outcome；只在原允许范围内继续下一步
    → 到达返回条件或不确定边界
    → 把完整动作回执交回主模型核对和继续
```

该子目标请求是**待设计的原生运行合同**，当前代码没有这个入口。首期 shadow 只旁观已有步骤；进入 gated 前须沿唯一工具 catalog/schema 定义明确的委托入口、参数和回执，并同步 schema、canonical item 与恢复校验，不能通过提示文本、隐藏回调或改写已有工具语义启动。主模型可以在原本就要发送的规划响应中提交子目标；若为建立子目标额外增加了一次 LLM 请求，也计入成本。

子目标最少固定：来源 Task/Turn/工具调用、目标 Tab、允许的操作与页面范围、可信输入值引用、遇到何种状态必须返回、一次控制片段的动作上限和总期限。上限用于限制这一段自动化授权与故障影响，不新增全任务 token/成本预算。片段可能跨越 Provider 的缓存保留时间，控制交回主模型时前缀可能已冷；这是 §10.3 冷热缓存对照要覆盖的实际成本，片段长度的取舍应参考该结果，而不只看动作数。新增用户指令、子目标变化、人工交互或 Task/Turn 结束都使当前片段停止。

#### 7.3.2 Decision Provider 只选择，宿主构造完整工具参数

Decision Provider 不生成任意输入文本、URL、脚本或路径。`type` 的文本来自子目标中已有的可信值引用；`select` 的值来自当前可用选项集合；参数不完整、操作与节点不兼容或需要理解新页面正文时，返回主模型补充。§6 中的多问题答案必须作为一个动作整体校验，不能把独立高分的 operation 和 target 强行拼接。

宿主持有本次候选到真实 `element_ref` 的映射，并绑定 `tab_id`、Surface、`navigationRevision`、`snapshotRevision` 和当前执行租约。绑定与引用不是 Provider 可覆盖的字段；每次快照均使旧节点引用失效。Provider 返回后以及实际派发前，分别重查执行范围和页面代次，迟到结果只记录为过期，不能落到新页面或新租约。

无关站点、新的登录/验证码/审批、需要自由文本推理的页面、影响范围不明的点击、`indeterminate`、`no_visible_change`、无合法候选、低置信度及额度/期限耗尽都结束控制片段。`navigated` 使旧候选失效；只有新页面仍在原子目标范围且只需下一步候选选择时，才重新采集并继续。`return_to_llm` 只表示交还控制，不表示成功或完成。

含密码、令牌、密钥输入控件，或支付、账户安全、凭据生成类页面的快照，整页不构造 Decision 请求，直接结束片段交回主模型，不依赖字段级脱敏（§11.2）。

#### 7.3.3 执行、历史和恢复沿原有事实源

- 所有动作仍通过原工具调度入口执行，沿用 Task/Skill/访问档位、工具账本、审批、BrowserAuthority、取消与用户接管；runtime 不直接调用 CDP，不另建浏览器调度器。
- 在派发动作前，原 Task 检查点和 canonical 工具记录持久化宿主动作 ID、冻结参数、子目标来源及 decision 身份；保存失败则不执行。现有记录若要求模型 Step 来源，须先增加显式的 `decision` 来源并覆盖重建/恢复校验，不能虚构 assistant 响应或成功的 LLM Step 来满足旧约束。
- 动作和结果仍各有原 canonical 记录。控制交回后，主模型读到同一历史投影及一个易变尾部交接说明；不另存一套 transcript，也不让原生动作被序列化为不存在的 Provider 工具调用。历史表示和各模型协议的合法性须在 gated 前验收。
- 崩溃恢复只读取原 Task/工具事实。已完成动作不重做；已派发而结果未知的动作按原未知副作用合同阻塞并核对；未派发的决定在重新验证范围、页面代次和候选之后才能继续。断线或超时不会触发模型版、Laya 版各执行一次同一动作。

#### 7.3.4 哪些 LLM 请求实际消失

可节省的是原循环中仅用于“看快照 → 选择下一动作”的中间请求。目标理解、子目标交付、页面内容分析、异常处理和最终核对仍由主模型完成。若每步都必须把页面交回主模型理解，则不启用快速循环；此时 Laya 只是额外建议，不宣称省调用。

例如同一片段原来有 4 次纯动作选择请求，替换后有 4 次 Decision 请求且省去这 4 次 LLM 请求；如果为交付或交回子目标新增了 2 次 LLM 请求，净减少只能算 2 次。一次模型响应原本能批量提出多个工具时，应与原批量路径比较，不能把每个工具都算成一次被省掉的 LLM 请求。

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

当用户明确要求真实代理时，Decision Provider 不能否决；当用户未明确要求时，Decision Provider 只能以 advisory 提示（写入易变尾部）影响主模型是否派发；不通过增删 `agent_spawn` 工具定义实现，避免改变工具表而使缓存前缀失效（§3.5）。实际 `delegation.reason`、角色、上下文包、Task 注册和生命周期仍由原有入口负责。

该 head 定位为对照实验，不因为记录易取就优先上线。`delegation.reason` 是模型自述，任务终态、产物和重复工作拒绝可用于构造弱标签；整合成本可能需要补采，是否值得委派仍需复核或对照实验（§11.1）。离线评估可与[子代理唤起设计](subagent-delegation-design.md)中的固定回放合并。

只给出 advisory 不消除当前主模型请求：主模型仍须理解目标、判断工作边界、选择角色并构造上下文包。效率收益只来自经对照确认减少的无效 child Task、子代理模型调用和后续收集/整合轮次。若主模型轨迹基本不变，新增 HTTP 和尾部 token 是净开销，应关闭该 head，不以“建议被采纳”计作节省。

### 7.6 `magi.execution_progress.v1`

用途：在多步执行中判断是否出现重复动作、无进展、目标漂移、应切换工具或应升级模型。

该头只能触发“重新评估”或“请求主模型”，不能直接宣布完成、重放未知副作用或自动创建新代理。

先复用 `ToolExecutionLedger` 已有的幂等结果复用、重复失败检测和未知副作用保护；合法的动态页面读取、任务观察和状态轮询不因同参数自动判作停滞。只有这些规则未覆盖的语义无进展，才先建立离线规则对照，再比较 Provider。P0 不为评估新建一套生产循环控制器，规则版已能覆盖的情形不引入 HTTP 调用。

### 7.7 不纳入首期的决策头

- 权限允许/拒绝；
- 审批结果；
- Browser lease 和用户控制权；
- 文件/Shell/网络实际执行；
- Task 完成和失败终态；
- canonical 上下文压缩边界；
- 用户请求的最终语义解释。

### 7.8 每个 head 必须说明省掉的 LLM 工作

| Head | 原路径实际做什么 | 采用后省掉什么 | 仍需 LLM 做什么 | 无收益时的处理 |
| --- | --- | --- | --- | --- |
| `route` | 用户选定模型直接处理；目前没有独立 LLM 选档请求 | 自动选档获准后可降低实际执行模型成本；不默认减少请求次数 | 理解、生成、工具参数和交付 | 对照包含错分后的升级/重做及缓存损失；无净收益则沿用户模型 |
| `tool_surface` | daemon 已按权限生成工具表，主模型在同次响应中选工具 | 真正收窄稳定工具表才可能减少定义 token；纯尾部建议不节省定义 token | 工具选择、参数及任务推理 | 仅缩小“思考空间”不算收益；必须看到计费或延迟下降 |
| `browser_action` | 原循环发送快照，由主模型产生后续动作 | 有界片段内的纯动作选择请求（§7.3.4） | 子目标、复杂页面理解、异常和最终核对 | 不能跳过模型发送就只留离线/shadow，不上 gated |
| `context_need` | 已有规则检索并装配上下文 | 排除无关检索内容时减少后续输入；只有替代实际存在的模型检索轮次才算少调用 | 回答、推理和必要检索 | 以现有规则为基线，补检索及遗漏修复也计成本 |
| `delegation_gate` | 主模型作委派判断并创建/收集子任务 | 有证据减少无效子任务及后续轮次；当前主模型请求仍存在 | 工作包、角色、理由和结果整合 | advisory 不改变后续执行则关闭 |
| `execution_progress` | 账本已拦截部分重复与未知副作用，主模型继续推进 | 只计额外阻止的无效后续模型轮次 | 真实新计划与错误恢复 | 额外唤起主模型的检查也计开销；不胜过规则则不用 |

每个 P0 报告标出原轨迹中拟被省掉的请求、替代后的发送点和保留请求，并区分“少调用”“更便宜模型”“少输入”三种收益。单个 LLM 响应混合了路由和业务推理时，先证明整次请求可被替代；不能按提示词中路由文字的比例折算出不存在的可省调用。优先级由可替代工作量、实现边界和净收益决定，数据易采或 shadow 一致率高不构成上线优先级。

## 8. 运行时接入架构

### 8.1 新增抽象

首期不新增 crate。当前 `magi-conversation-runtime` 已依赖 `magi-bridge-client`，所以 Provider trait 和传输类型与 HTTP 实现一起放在下层；不能把 trait 放在 runtime 再要求 bridge-client 实现它。

```text
magi-conversation-runtime
  decision/
    DecisionRouter
    DecisionPolicy
    BrowserControlSegment（原 Task 内的有界控制片段）
    DecisionTrace 的领域绑定与提交

magi-bridge-client
  decision
    DecisionProvider (trait)
    DecisionRequest / DecisionResponse / DecisionError
    ProviderVersionIdentity
  decision_http
    HttpDecisionProvider
    Laya protocol adapter / Jev protocol adapter / OpenRouter Decisions adapter
    timeout / cancellation / response validation / health

依赖方向：daemon / API → conversation-runtime → bridge-client
          daemon / API → bridge-client
```

决策类型与聊天模型的 `ModelBridgeClient` 分离；复用适用的 HTTP 基础设施，由 daemon 沿现有配置/凭据入口构造客户端。字段选择、脱敏、授权候选、head 策略和 Task/Turn 身份属于 runtime/领域层；bridge-client 不读取 TaskStore，不判断 Browser 权限，不依赖上层 crate。ContextRuntime、ToolRuntime 等更低层消费者如需联动，由 runtime/daemon 装配回调或显式传入结果，不反向依赖 conversation-runtime。当出现真实的独立消费者需求时，再判断是否需要抽出共享 crate。

聊天模型是生成内容和工具调用的流式协议；Laya/Jev 是固定候选上的类型化判断，二者不能共用同一个“模型响应”事实模型。

### 8.2 建议接入点

| 模块 | 接入方式 |
| --- | --- |
| `magi-api` | 读取和保存全局 Decision Provider 配置、连接测试和状态 DTO |
| `magi-bridge-client` | 定义独立 Decision 传输合同与 Provider trait，实现 HTTP 请求；不引用 runtime |
| `magi-conversation-runtime` | 在模型 Step 冻结前决定发送 LLM 还是继续既有 Browser 控制片段；构造最小输入、采用策略并绑定原工具执行/账本 |
| `magi-api/src/browser_tool_runtime.rs` | 保留快照和明确动作的原生执行依赖，由同一工具入口使用；不在模型已选好动作后增加隐藏自动化循环 |
| `magi-conversation-runtime/src/tool_surface_state.rs` | 授权工具表是唯一候选源；advisory 不改表，收窄仅在缓存准入通过的稳定边界发生 |
| `magi-context-runtime` | 原检索和预算入口继续拥有结果；runtime 可传入经过校验的检索选择，不增加反向依赖 |
| `magi-tool-runtime` | 继续执行权限、工具参数和原工具执行；决策来源由上层绑定，不允许 Provider 改权限结果 |
| `magi-session-store` / 原 Task 持久化边界 | 保存版本身份、子目标与动作来源、待决动作及实际结果；新增来源需走原 schema/校验，不保存密钥或另建日志 |

### 8.3 生命周期

```text
读取 Provider 与 head 设置
  → 验证唯一 Provider 的 URL、协议、凭据引用和版本身份
  → 可选健康检查
  → 新 Turn 固定 Provider 和各 head 策略的配置快照
  → 每个决策头生成候选
  → 向唯一已配置 Provider 发 HTTP 请求
  → 校验响应
  → 置信度/abstain/策略门槛判断
  → 采用或回退
  → 写入 DecisionTrace
```

新接纳运行冻结 Provider 配置 revision、head/策略和模型/服务版本身份；已有 Task 的排队、重试和恢复沿原快照，新 activation 再按接纳合同解析。撤权、关闭外发或取消是即时安全边界，旧快照不能继续授权新请求；结束控制片段后回到本任务已冻结的主模型。Provider 消失或版本变化时按该任务策略回退，不静默换服务或模型。

## 9. 配置设计

建议新增全局 `decisionProvider` 设置，至多保存一份服务配置，具体字段以最终 App Server schema 为准：

```json
{
  "enabled": true,
  "transport": "http",
  "baseUrl": "http://127.0.0.1:8765",
  "protocol": "laya_systemone",
  "model": null,
  "credentialRef": null,
  "expectedServiceVersion": null,
  "defaultMode": "off",
  "allowRemoteShadow": false,
  "connectTimeoutMs": 100,
  "requestTimeoutMs": 100,
  "totalBudgetMs": 150,
  "healthCheck": false,
  "heads": {
    "magi.route.v1": {"enabled": false, "mode": "off", "policyRef": null},
    "magi.tool_surface.v1": {"enabled": false, "mode": "off", "policyRef": null},
    "magi.browser_action.v1": {"enabled": false, "mode": "off", "policyRef": null},
    "magi.context_need.v1": {"enabled": false, "mode": "off", "policyRef": null},
    "magi.delegation_gate.v1": {"enabled": false, "mode": "off", "policyRef": null}
  }
}
```

配置约束：

- `enabled=false` 时不发任何请求；
- 示例中所有 head 默认 `off`：head 只有在通过 §10.4 对应准入后才逐项打开；`allowRemoteShadow=false` 时，远程地址不发送 `shadow` 请求，仅 loopback 可用；
- `baseUrl` 必须是 HTTP(S) origin，禁止嵌入凭据、路径穿越和非 HTTP scheme；
- 远程地址默认 HTTPS；loopback 地址允许 HTTP；
- 全局只有一份地址、协议、凭据引用和版本身份，没有 Provider 列表或各 head 的 Provider 选择；未配置时所有 head 均不调用；
- `protocol` 使用拟定的 `laya_systemone`、`jev_systemone` 或 `openrouter_decisions` 标识选择适配器。切换时替换同一份地址、协议、模型 ID 和凭据，不增加第二个实例；这些标识是方案设计，不代表服务已完成兼容验收；
- `model` 是当前决策服务使用的模型 ID，不是 Magi 主 LLM 的配置；OpenRouter adapter 要求显式指定，如文章中的 `typesafe/jev-1.13`，不使用浮动 latest 别名。直连本机固定模型服务可省略，但 gated 仍须验证实际版本身份。`baseUrl` 保持 origin 形式，例如 `https://openrouter.ai`，路径由 adapter 固定为 `/api/alpha/decisions`；
- 每个 head 独立开关和模式，均使用唯一已配置的 Provider；head 关闭或为 `off` 时不调用；
- 同一个 Provider 服务多个 head 时，每个 head 仍须有自己的评估、校准和准入版本，不能因为 Laya 或 Jev 在一个 head 通过就自动授权其他 head；
- `totalBudgetMs` 是单次 Decision 的总期限，连接与读取超时都受它约束（§13）；示例数值仅作示意；
- Provider 的 `expectedServiceVersion` 固定准入评估所针对的模型/服务版本，gated 要求它非空且与实际响应一致，浮动别名不通过（§6.2）；
- 采用阈值和 margin（§12）通过 head 的 `policyRef` 引用随评估发布的策略版本，并核对 Provider、有效模型/服务版本及 head；更换服务或协议使旧策略准入失效，不能沿用原阈值；具体字段以最终 schema 为准；
- 不保存明文密钥；
- 不把 Provider 连接失败伪装为“模型没有做出决定”；
- UI 显示 `disabled`、`configured`、`healthy`、`degraded`、`unavailable`，不只显示一个布尔开关。

## 10. 效率模型

每个 head 必须回答 §7.8 中的具体效率问题。调用数、毫秒、token 和金额分开核算，不能直接相加；以下量均比较同一任务条件下的原路径与实验路径：

```text
net_llm_calls_saved = baseline_llm_calls - experiment_llm_calls
net_latency_saved_ms = baseline_wall_time_ms - experiment_wall_time_ms
net_cost_saved = baseline_total_cost - experiment_total_cost
```

实验总量包含 Decision HTTP、额外交接/主模型核对、fallback、错误路由后的恢复与重做、worker 和辅助模型的全部调用。费用以实际 Provider 用量和有效费率为准；`cache_loss` 是缓存失效增加的输入成本，已包含在实验费用中，不重复扣除。没有费率时报告 token 与延迟，费用标未知，不编造金额。P0 可粗估方向，生产准入仍需质量不下降的实际对照。

要求分两层：P0 只回答“值得做 / 不值得做 / 说不准”，用已有账本和轨迹粗估方向即可；下文 A/B/C 对照、事前声明 margin 和样本量，是进入 `advisory` 采用实验和 `gated` 的要求，不是 P0 的前置工作。

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

如果实验无法减少 LLM 工作或实际成本，且总延迟还上升，该 head 不进入 gated。只降低金额却提高延迟的结果应明确列为权衡，不能声称同时提速。

对照至少区分：A 为当前规则与主模型，B 为只增加相同确定性预筛选，C 为预筛选加 Decision。C 需要相对 B 有增量收益，不能把规则本身的优化归给 Laya/Jev。shadow 只能测试响应质量、覆盖率和额外延迟，不改变行为，不能单独证明省调用或提高成功率；这些结论需离线端到端回放或获准的真实采用实验。

对照固定任务、权限、主/子模型及推理强度、候选构造、可复现的站点/文件状态，并报告冷热缓存、重试和失败样本。合格 margin 和样本量按 head 在实验前声明；P0 可以给出“说不准”，不能在结果出来后挑样本或改阈值来宣称通过。

### 10.5 让“有价值”变成可执行的门槛

为了避免把一次命中率报告误读成产品收益，首轮实验应在开始前写下以下门槛；具体数值可以按 Magi 的基线调整，但不能在看到结果后再调整：

- **浏览器控制片段**：只纳入原轨迹中至少有两个连续“快照后选动作” ModelStep、候选集可重建、动作可逆且页面目标未变化的片段。报告每片段原 ModelStep 数、控制片段中的 Decision 次数、交接 ModelStep 数和净减少数；`net_llm_calls_saved` 必须仍为正，任务成功率相对当前路径不得低于预先声明的非劣 margin，未知副作用和人工接管不能增加。
- **工具面或模型档位**：必须分别报告工具定义 token、缓存读写 token、实际模型费用和错误重做次数。若只减少了候选描述，却没有降低总输入或后续调用，就判定为无收益；若换模型造成缓存重建，缓存损失必须计入同一对照。
- **知识/记忆**：以当前 `ContextRuntime` 的规则、受治理查询和选择缓存为 B 对照，Laya 只能在 C 相对 B 额外减少注入 token、纠错轮次或失败率时保留。相对“完全没有检索”的收益不能归给 Laya。
- **子代理和进展建议**：固定主/子模型与工具面，比较后续 child Task 数、子代理 ModelStep 数、整合轮次和总 wall time；建议被采纳本身不是收益，轨迹不变就关闭该 head。

浏览器首轮可以用一个简单的片段账本核算：

```text
原路径：handoff_model_steps + action_selection_model_steps
实验路径：handoff_model_steps + return_check_model_steps + decision_http_calls
净节省：原路径 ModelStep 数 − 实验路径 ModelStep 数
```

其中 `decision_http_calls` 只进入延迟和费用账本，不冒充 ModelStep；如果实验路径为了建立子目标多发了一次模型请求，也必须加回 `handoff_model_steps`。这个账本能直接回答“Laya 是否真的减少了 Magi 让 LLM 做的工作”。

### 10.4 在线启用准入条件

每个 head 单独、依次满足下列条件，才能进入对应阶段：

| 进入阶段 | 必须满足 |
| --- | --- |
| 编写在线 Provider 代码 | P0 离线基线显示该决策点在主模型调用、token 或失败中占比不可忽略，且粗略判断收益大于成本 |
| `shadow`（线上数据流） | 有明确的数据最小化字段表；loopback 或用户已同意远程外发；有离线评估之外仍需线上数据的理由 |
| `advisory` | 用同一主模型回放有/无建议后的真实执行轨迹；质量不低于预先约定 margin；建议只写易变尾部；除 HTTP 和额外输入外确实减少后续调用/成本，否则仅保留实验 |
| 改变工具表或模型的采用 | 按 §10 的实验总成本（已含 `cache_loss`）粗略判断节省明显大于损失；只在 Turn/会话边界采用；有用户可见的开关 |
| `gated` | 模型/服务版本身份可核验，评估和校准与 head/候选格式绑定；Magi held-out、顺序扰动和真实采用评估通过；相对规则对照有净收益；质量与延迟满足预定门槛；不强制专属微调 |

## 11. 数据、训练和校准

### 11.1 数据来源

优先使用 Magi 已有的真实 canonical 轨迹和执行事实：

- 用户输入与最终完成合同；
- 模型实际工具调用；
- Browser 快照、候选节点和 outcome；
- Task/Turn/Agent 的真实终态；
- 工具失败、重试、取消和未知副作用；
- ContextRuntime 的检索结果和最终采用情况。

标签分成四层，不把执行事实自动等同于正确决策：

| 层次 | 可以证明什么 | 不能据此推断什么 |
| --- | --- | --- |
| 执行事实 | 某动作已发生、收到何种 outcome、Task 以何状态结算 | 点击正确、任务确实满足用户意图、委派值得 |
| 弱标签 | 从事实与规则派生的候选标签，保留来源和置信依据 | 可以当作 held-out 标准答案 |
| 复核标签 | 人工或独立复核结合目标、当时状态和证据给出的判断；分歧明确记录 | 自动证明另一条未执行路径更优 |
| 对照实验 | 固定条件下有/无决策路径的成功率、调用量和完成时间差异 | 单个历史成功样本的普遍因果收益 |

浏览器样本必须能重建当时的候选表、`snapshotRevision`、动作、子目标和结果。可信 click 或 `Succeeded` 不证明目标正确；`no_visible_change`、未知结果和最终没有完成的轨迹也要保留。历史若只保留裁剪结果而无法复原候选，就标为不具备该项回放条件，不凭当前页面补造历史。

`delegation.reason` 只是模型判断的原文，`existing_task_id` 只证明触发了已有去重合同。子任务成功不证明比主线直接完成更快；未派发轨迹也没有“派发后会怎样”的事实。委派价值需要复核独立工作边界，并在相同任务和固定子代理绑定下比较主线与委派执行；启动、等待、重复理解和整合成本均计入。缺失的成本先标未知，需要时离线补采，不宣称无需人工标注。

划分数据集按任务、会话和站点等相关组隔离，避免同一轨迹的相邻快照进入训练与 held-out 两侧。所有标签保留 source、复核方式及版本，弱标签不能与复核标签混报准确率。

### 11.2 数据脱敏

发送到远程 Provider 前：

- 移除密码、令牌、Cookie、API Key、Authorization header 和敏感表单值；
- 默认只发送 origin，不发送完整 URL query；
- 文件路径按工作区相对路径或稳定摘要表示；
- 工具参数按 head 所需字段最小化；
- 页面文本、工具结果和用户消息按 head 的最小字符预算裁剪；
- 请求日志只保存 hash、大小和决策摘要，不保存原始敏感状态。

### 11.3 校准

每个 head、候选规模、语言和模型/服务版本身份独立评估。阈值不能从一个 head 复制到另一个 head，也不能把公开 benchmark 的置信度直接当作 Magi 生产阈值；自有模型可按需微调，托管服务不要求提供权重。

必须有：

- 校准集、held-out 测试集；发生微调时另设训练集，三者隔离；
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
3. 校验候选、概率、模型/服务版本身份和请求绑定
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
- 模型/服务版本身份及 head/schema、校准版本是否已评估；
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
| 模型/服务版本身份变化 | 记录 | 不采用 | 原准入失效，回退并重新评估 |

浏览器写操作一旦已经交给原生 Host，Laya 超时不能让调用方自动重放。未知副作用沿现有 Browser 失败合同处理。

shadow 不阻塞真实工具派发：在线旁观使用原执行拥有方管理的有界并发与取消，过载直接跳过；不能留下无归属后台请求。advisory/gated 的等待计入交互关键路径，超过单次总期限后回到原路径，同一决策点不连续重试 Decision HTTP。参数中的连接、读取超时必须受同一个总期限约束，不能在连接阶段用完后再获得完整读取期限。

## 14. 决策事实与审计

DecisionTrace 是审计和评估投影，不是第二套业务事实。建议包含：

```text
decision_id
request_id
session_id / task_id / turn_id / step_id
head_id / head_version
provider_origin_hash
adapter_id / adapter_version
model_id / provider_version / model_digest（可选）
provider_config_revision / schema_version / evaluation_version / calibration_version
candidate_ids_hash
input_shape_hash
mode
answer / probabilities summary
confidence / abstain
adopted
fallback_reason
control_segment_id / action_id（存在有界控制片段时）
origin_kind / source_model_step_id（可空）
latency_ms
created_at
```

advisory 可以关联实际存在的 canonical Step；被跳过的 LLM 请求没有 ModelStep，`source_model_step_id` 必须可空，不能为统计创建假的模型调用。原生 Decision 动作的执行意图与结果沿 §7.3.3 的 Task/工具事实保存，DecisionTrace 只是从这些来源产生的评估投影。新增来源及 metadata 的持久合同须在实现时同步原 schema、生成物、重建和恢复，不另建平行执行日志。

Trace 不能单独证明工具成功、Task 完成、用户授权或 Browser 副作用已发生。“拟省掉的请求”只在对照报告中关联，实际用量账本只记录真实的 LLM transport 和 Decision HTTP 用量；不按动作次数虚报已避免的调用。

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

### P0：离线基线与回放（不接入 Magi 在线执行，不外发数据）

- 先核实现有 canonical 轨迹能否复原每步的候选表和 `snapshotRevision`：这决定浏览器评估有没有可用数据（§11.1），不能复原的部分直接标为缺口；
- 从用量账本和 canonical Step/ToolCall 轨迹统计：每 Turn 主调用数、浏览器任务步数、各工具调用频次与失败率、`agent_spawn` 次数及后果、模型或工具面变化引起的 `cache_epoch` 变化次数；
- 按 §7.8 为每个 head 标明当前请求、拟消除请求/输入、仍需 LLM 的工作及新增开销；浏览器按整次模型请求区分纯动作选择与混合页面理解，保留原批量工具路径作为基线；
- 按 §11.1 关联 Browser/Agent 事实并建立弱标签和复核集；委派样本与原固定回放合并，缺失候选或反事实结果明确列为缺口；
- 先审计现有工具账本规则；未覆盖的停滞仅做离线规则对照，不为了 P0 改生产行为；
- 以单独的离线回放脚本分别调用用户已有的 Laya、Jev 或其他本机 HTTP 决策服务，Magi 不带模型、不启动推理进程、不接在线链路；报告各 Provider 的 held-out、校准与选项顺序稳定性；
- 对比 A 当前路径、B 规则预筛选、C 规则加 Decision，区分可估算上限与已验证收益；
- 退出条件：至少一个 head 有可省的具体工作、预期相对规则的净收益及可补齐的执行/恢复合同；否则止步。子代理数据易得不构成其首发理由。

### P1：协议与 `shadow`（仅对通过 P0 的 head）

- 按 §8.1 在 `magi-bridge-client` 增加独立 Decision 合同、trait 和 HTTP 实现，以及 Laya/Jev 各自的 adapter；在 `magi-conversation-runtime` 增加策略/领域装配，保持单向依赖；
- 全局配置、凭据引用、连接测试；
- 只支持 `shadow`；远程地址默认关闭，需用户同意（§5.3）；
- DecisionTrace、脱敏、候选 hash；未配置时零请求；
- 退出条件：shadow 能可靠绑定候选与执行事实，没有破坏原路径、取消和缓存；只能证明观测有效，不能据此宣称省掉实际调用。

### P2：受控采用实验（按可省工作选择，不强制各 head 走同一路线）

- `delegation_gate` 仅做有/无尾部建议的回放或获准实验；不改工具表，若无效子任务及总调用没有减少则停止，不进入默认在线路径；
- `context_need` 与现有规则比较，只有实际输入缩减且任务质量不下降才采用；
- `browser_action` 若模型/服务已通过 Magi 场景验证，则先补齐 §7.3 的显式子目标入口、Decision 来源的 canonical/工具合同、模型历史投影和取消/恢复测试，再做隔离站点的有界控制实验。该实验才用于检验是否真的跳过 LLM，无需先强制每步 advisory；
- 退出条件：以 §10 的固定条件对照证明相对规则有收益，并且缓存、任务成功率及恢复语义满足预定门槛。只看到建议采纳率不算通过。

### P3：逐 head 开放 `gated`

- `browser_action`：固定可核验的模型/服务版本，held-out 与真实采用评估通过；净主 LLM 请求数下降，包含交接、fallback 和错误恢复；来源、权限、历史与恢复验收完整。通用 Jev/Laya 和微调模型均可参评；
- `route`（模型档位）：先有产品决定（§7.1），再只在会话首轮采用；
- `tool_surface`（改工具表）：先通过 §10.4 的缓存准入，且只在 Turn/会话边界采用；
- 任何 head 进入 gated 都需发布独立评估报告；`delegation_gate` 在当前方案中仍为 advisory，不因指标改善自动升级成 Task 创建或工具隐藏入口。

### P4：持续评估、校准与按需微调

- 从 Magi canonical 轨迹持续构建 browser、delegation、route、progress 数据集；
- 温度校准、选项顺序稳定性、候选描述改写、相似候选和截断状态测试；
- 对每个 head、候选规模、语言和可核验模型/服务版本分别发布评估；
- 泛化不足时再微调，Magi 不承担模型托管；某个 head/版本通过不能为其他 head、候选格式或后续版本授权。

## 17. 验收标准

### 17.1 协议与配置

- 未配置时零 HTTP 请求；
- 只允许单个决策服务配置，可选择 Laya 或 Jev；所有 head 共用该服务，不支持同时配置两者；
- 同一决策点只向唯一 Provider 发一次请求；`shadow` 仅观测该服务，不额外配置或调用其他决策模型；
- Laya/Jev 的协议 adapter、能力声明、版本身份和校准策略分别验证；切换服务使旧准入失效，当前服务失败时回到 Magi 原路径；
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
- Decision Provider 不能执行未在候选列表中的节点；
- 用户接管、导航、取消和 Guest 崩溃时不会重放旧动作；
- gated 浏览器模型的真实任务成功率没有明显低于当前基线，并且 LLM 步数有可感知的下降；
- 有界片段在原循环的模型发送前分支，真实执行轨迹证明确有 LLM 请求被跳过；不在已选好动作的工具处理器中事后追加 HTTP 冒充收益；
- 子目标委托、Decision 动作来源及回执可持久化重建，不伪造 ModelStep；控制交回时模型读到完整工具事实，原工具/模型历史协议合法；
- 覆盖“Decision 返回前取消”“意图保存失败”“已派发未收结果时崩溃”“用户接管”“控制交回”等路径，未知副作用不自动重放；
- `type`/`select` 只使用可信值引用或现有有限选项；需新内容推理、敏感输入或影响范围不明时交回主模型；`return_to_llm` 不提交完成。

### 17.4 子代理

- 用户明确要求代理时不能被 Laya 否决；
- Decision Provider 建议不能绕过 `agent_spawn` Schema、角色、Skill、工作区、重复工作和生命周期预检；
- Decision Provider 失败不产生虚构的 child Task；
- 子代理建议不自动创建 Task、不隐藏 `agent_spawn`；只有相同主/子模型条件下对照显示无效子任务及总成本下降，才保留 advisory；
- 评测区分弱标签与复核标签，不能以 `delegation.reason`、子任务成功或去重拒绝作为委派价值的直接真值。

### 17.5 回归入口

按实际改动选择验证，以下是可能涉及的现有入口，不把全套命令作为文档修改或每个阶段的默认要求：

```bash
npm run protocol:check
cargo check -p magi-api
cargo check -p magi-bridge-client -p magi-conversation-runtime
cargo test -p magi-conversation-runtime context_authority
cargo test -p magi-browser-authority
cargo test -p magi-tool-runtime
npm --prefix web run check
```

真实 Provider 验收必须使用隔离状态根和脱敏测试数据，分别覆盖本地 HTTP、远程 HTTPS、超时、非法响应、低置信度、候选过期和回退路径。

新增 Decision 的 HTTP 合同和超时/取消定向测试；只有启用 Browser 控制片段时才运行对应 canonical/工具账本、历史协议及恢复回归。浏览器工具 schema/catalog 改动还应运行 `npm run browser-tool-catalog:check`，执行语义改动运行 `npm run test:browser-core`，真实 Chromium 行为按局部规则验收。P0 离线脚本不需要启动 UI 或修改生产循环。

### 17.6 离线基线与缓存

- P0 报告包含：各决策点在主调用、token 和失败中的大致占比，浏览器步骤大致可替代的部分，以及 `cache_epoch` 变化的大致情况；
- 报告明确给出每个 head 是否进入下一阶段及理由，未通过的 head 不留下在线代码；
- 任何会改变工具表、系统前缀或模型的采用路径，都有采用前后缓存命中率的对照；
- 远程 `shadow` 默认关闭，关闭时零外发；开启后外发字段与字段表一致；
- 报告分别给出净 LLM 调用数、端到端延迟和可核算费用；包含规则版对照、额外交接、fallback、失败恢复和冷热缓存；
- gated 不要求私有权重或专属微调，但必须有可核验版本身份和对应 head 的评估/校准记录；仅浮动模型别名不能通过。

## 18. 结论

基于当前代码和调用链，Magi 没有足够理由把 Laya/Jev 做成全局路由层。Session/Turn、Skill/权限工具面、知识意图和重复执行已经有确定性事实源；把这些路径再过一遍 HTTP 决策服务，大多只会增加延迟、外发数据和故障面。

接入仍可能有实际价值，但价值集中在一个很窄的条件：**浏览器任务中存在可预测、连续、低风险的动作选择，并且这些动作目前各自触发一次主模型 ModelStep。** 在这个条件下，配置的 Laya 或 Jev 可以作为有界控制片段的 System-1，替代中间动作选择，保留主模型的目标理解、异常处理和最终核对。它应先以离线回放和隔离站点实验验证，不能因为浏览器模型的分类准确率高就直接上线。

次一级的可能价值是 Turn/会话边界的模型档位或工具面选择，但这依赖尚未存在的产品能力，并且要证明缓存损失小于输入或费用节省。知识路由、子代理 advisory 和语义进展建议目前都不能直接减少一次主模型请求，只能在有端到端对照收益时作为后续实验。

因此本方案的落地结论是：先验证浏览器控制片段是否能产生正的净 ModelStep 节省；若 Magi 的真实轨迹中没有足够长的连续浏览器片段，或者交接、回退和失败恢复抵消了节省，则该 head 不进入在线采用。Laya、Jev 和后续同类 Provider 使用同一验收口径、分别评估；某一个服务表现不足不代表其他服务同样不足。止步于离线结论是有效结果。

在满足上述条件时，最有潜力的路径是：

1. 有界浏览器片段替代纯动作选择的中间模型请求，保留主模型的理解、交接和核对；
2. 获准的稳定边界模型等级和工具面路由，实际降低模型费用或定义 token；
3. 知识、记忆和上下文需求路由，相对现有规则减少无效输入；
4. 子代理和执行进展建议，仅在对照确认减少后续子任务、模型调用或成本后保留。

所有路径都必须遵守同一个原则：**Laya 可以减少 Magi 需要让 LLM 做的路由判断，但不能减少 Magi 自己必须执行的权限、事实、恢复和副作用校验。**

落地顺序上，本方案接受的是“先量化、再接入”：P0 先说明哪次 LLM 请求或哪些输入会消失，再决定工程投入。浏览器片段具有直接省调用的可能，也需要补齐明确执行合同；模型档位和工具面受缓存及用户绑定约束；子代理 advisory 只是对照实验。按净收益排序，不按数据易取排序；若 P0 看不出明确收益，止步是合格的结论。
