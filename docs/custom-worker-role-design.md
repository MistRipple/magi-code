# Magi 用户自定义子代理角色设计与维护

## 1. 目标与范围

Magi 是以主对话承接意图、由 Agent 完成工作的本地工作台。主 Agent 负责理解目标、编排任务和汇总结果，子代理 Worker 在获授范围内完成探索、设计、实现、测试或评审；工具范围也包括网页、文件与外部服务。整体演进与尚未完成的生命周期改造见 [Agent 运行时演进方案](magi-agent-runtime-evolution-plan.md)。

当前能力覆盖：在保留现有子代理 Worker 体系、任务分配方式、执行生命周期和设置界面的前提下，允许用户创建、编辑、删除、导入和导出自定义角色，并保证自定义角色可以被主 Agent 识别并正常分配任务。

角色管理不新增第二套 Agent 类型，不改变 Worker Runtime，不引入 Agent 市场、团队 Agent 或独立权限平台。

## 2. 产品原则

- 系统内置角色和用户角色使用同一套角色定义、能力组合、模型绑定和 Worker 调度逻辑。
- 用户定义的是子代理角色，不是独立运行时实例。Worker 实例仍由现有 spawn 和执行链创建。
- 角色配置服务于软件开发协作，字段和交互保持当前代理设置页的语义。
- 导入导出面向角色复用，只携带角色定义，不携带会话、执行记录、工作区绝对路径或模型密钥。
- 内置角色受保护；用户角色可管理，但不能覆盖内置角色 ID。
- 注册表、前端展示、模型绑定和任务分配必须使用同一份有效角色集合，禁止出现“能显示但不能调度”的状态。
- 角色配置是持久化产品数据，必须具备原子保存、版本校验、变更可见、失败可恢复和重启一致性。
- 现有内置角色、主 Agent 编排和 Worker 执行是稳定基线；新增能力不得改变它们的默认行为和性能边界。
- 常用路径优先：创建、复制、编辑、导入、导出和绑定模型都应在现有 Agent 设置体验内完成，不要求用户接触文件系统或内部协议。
- 复杂性只放在实现内部，用户界面只呈现完成角色配置所必需的信息；不能为了覆盖极端场景牺牲日常可用性。

## 3. 当前实现

实现复用以下既有基础：

- `magi-agent-role` 提供 Markdown role 解析、内置角色和用户角色目录加载；daemon 的实例目录为 `<state_root>/roles`，默认状态根为 `~/.magi`。
- `ProfessionalCapabilityRegistry` 提供角色能力注册和组合。
- `AgentBinding` 提供角色到 Engine 的绑定，以及继承主模型的语义。
- `magi-orchestrator` 和 `magi-worker-runtime` 提供任务分派、父子关系（TaskStore 的 `parent_task_id`）、Worker 生命周期、工具调用和结果回传。
- Agent 设置页已经具备角色列表、角色详情和代理引擎绑定界面。

当前实现中，角色注册表、API、前端 registry、Engine 绑定和 spawn 校验现在使用同一份有效角色集合；用户角色的创建、编辑、删除、导入、导出和进程内刷新都进入同一闭环。

## 4. 统一数据模型

### 4.1 角色定义

继续使用现有 `RoleTemplate`/`AgentRole` 语义。实现中的 `AgentRole` 就是后端规范化角色对象，承担唯一角色定义真相源；没有再引入一套并行的 `RoleDefinition` 结构。内置 Markdown 资产和用户 Markdown 都解析为 `AgentRole`，API 从它生成展示 DTO，Worker 目录从它生成调度信息。规范对象至少包含：

- `templateId`：API 使用的全局唯一稳定 ID；对象内部字段名为 `id`。它是系统内部标识，不作为新建表单的用户输入项；创建时由显示名称自动生成，复制和冲突导入时由系统生成唯一副本 ID；
- `displayName`、`description`；
- `systemPrompt`；
- `role`、`focus`、`constraints`、`output_preferences`；API DTO 映射为 `profile.role`、`profile.focus`、`profile.constraints`、`profile.outputPreferences`；
- `ownerships`、`insightPreferences`；
- `capabilities`；
- `color_token`、`icon`；API DTO 映射为 `defaultUI.colorToken`、`defaultUI.icon`；
- `supportedKinds`、`parallelismLimit`、`coordinatorMode`；
- `version`；
- `source`：API 根据 registry 的 builtin ID 集合派生为 `builtin` 或 `user`，不写入角色文件，也不参与调度分支。

`coordinatorMode` 继续由系统控制。用户角色默认只能作为可派发的非协调 Worker，不能通过导入或编辑把自己变成主线协调器。

### 4.2 绑定配置

继续使用现有 `AgentBinding`：

- `templateId`；
- `engineId`，空串表示继承主模型；
- `bindingRevision`；
- `order`；
- `uiOverrides`；
- `profileOverrides`（如果现有 binding DTO 继续暴露该字段，仅属于绑定显示覆盖，不属于角色 Markdown）。

运行时先判断是否需要委派，再按职责和能力选择代理角色，最后解析该角色的模型绑定。模型是代理的执行配置，不是用来反查代理的身份；多个角色可以共享同一引擎。`agent_spawn` 不接受临时模型覆盖；显式绑定失效不能静默继承主模型。完整顺序见[子代理唤起设计 §2.1](subagent-delegation-design.md#21-决策顺序与模型归属)。

角色定义和绑定配置分开保存。导出角色时只导出可选的非敏感绑定偏好，不嵌入 Engine 的 API Key、连接地址或本机状态。

### 4.3 持久化

- 内置角色：编译期 Markdown 资产，只读；启动时解析为 `AgentRole`。
- 用户角色：daemon 使用 `<state_root>/roles/<templateId>.md`，独立运行时默认使用 `~/.magi/roles/<templateId>.md`；文件采用原子写入，并与内置角色共享 ID 命名空间。
- 用户角色 ID 与任何内置 ID 冲突时必须拒绝，加载和 API 写入都不得覆盖 builtin。
- 用户绑定覆盖：继续使用设置存储中的 `agents` section。
- 导入、编辑、删除成功后，更新用户角色文件并刷新进程内 `AgentRoleRegistry`。

角色文件格式继续采用 front matter + Markdown body，front matter 使用**扁平字段**，与 `magi-agent-role` 的解析器和导出器保持一致；不能保存一套嵌套 `profile/default_ui` 文件格式，再由 API 另行解释。规范字段包括 `id`、`display_name`、`description`、`supported_kinds`、`parallelism_limit`、`coordinator_mode`、`version`、`role`、`focus`、`constraints`、`output_preferences`、`ownerships`、`insight_preferences`、`capabilities`、`color_token` 和 `icon`，body 对应 `system_prompt`。本机持久化文件还会写入 `role_revision`；该字段只服务并发编辑检测。缺省的展示字段只允许使用统一默认值。

导出的最小示例：

```markdown
---
id: data-analyst
display_name: "数据分析师"
description: "负责清洗、分析和验证数据"
supported_kinds: [local_agent]
parallelism_limit: 2
coordinator_mode: false
version: 1
role: "数据分析与验证"
focus: ["读取,清洗", "指标核对"]
constraints: ["保留证据"]
output_preferences: ["数据口径", "验证结果"]
ownerships: ["分析结论"]
insight_preferences: ["decision", "risk"]
capabilities: ["general_engineering", "data_engineering"]
color_token: "agent-data-analyst"
icon: "bar-chart"
---
你是数据分析师，先核对数据契约，再输出可复现结论。
```

`role_revision` 只存在于本机角色文件，用于并发编辑检测；导出时必须省略。`profile`、`defaultUI`、`roleRevision` 等 API 字段不能直接写入角色 Markdown。

## 5. 统一运行链路

```text
内置角色 / 用户角色
        ↓
AgentRoleRegistry 有效角色集合
        ↓
前端角色列表与 AgentBinding
        ↓
主 Agent 读取可派发角色
        ↓
判断委派价值，按独立工作包选择角色
        ↓
现有 agent_spawn / 任务分派
        ↓
按所选角色解析 AgentBinding → Engine → 模型配置并预检
        ↓
WorkerRuntime 执行
        ↓
工具、Skill、模型调用
        ↓
Worker 报告回传主 Agent
```

所有角色都必须经过同一套：

- TaskKind 支持性校验；
- capability 校验和 prompt 组合；
- Engine 绑定解析；
- Worker 生命周期和并发限制；
- governance、permissions、safety gate；
- 父子关系（TaskStore）和结果回传。

角色提示和专业能力不授予额外权限，也不因为名为 explorer/reviewer 就自动形成硬只读边界。子任务默认继承父 TaskPolicy；用户访问模式、显式工具/路径约束、Skill 封闭工作流和资源范围继续收紧有效权限。Skill 的多份显式封闭限制取交集，未声明限制时继承。

默认由主线直接完成工作。代理仅用于用户明确要求，或有显著并行、上下文隔离、必要独立复核价值的自包含工作包；Task/Goal、多文件和角色匹配本身不触发代理创建。角色提示不能无条件禁止已获运行时委派授权的代理，也不能自行授予委派权；获授 `allow_delegation` 后仍应先判断收益。参考项目比较和当前实现边界见 [演进方案 §2.1](magi-agent-runtime-evolution-plan.md#21-代理唤起策略复审2026-10-17)。

主线和子代理共用原生 `conversation_loop`、Step 调用记录、工具目录范围和执行预检。子代理可通过带来源身份的 `ask_user_question` 提问，并查询自己的执行目录（`git_status / git_branch_list / git_worktree_list`），不能更新主会话 Git context。共享计划/Goal/长期记忆和主分支变更仍由主线控制；主线可通过 `allow_delegation=true` 显式允许直接子代理派发自己的子任务，最多两层；该授权独立于角色的 coordinatorMode，不授予共享状态或主分支权力。产出通过 agent_apply 逐级合入直接父任务的工作树，嵌套身份也只由同一父 Agent 继续。`agent_spawn(lifetime=session/goal)` 可显式保留身份，之后用最新已完成的 `resume_task_id` 创建新 activation；角色与作用域不变，权限取当前父任务且能力不扩大。未转交子树随父 Turn 结束；显式转交给 Session/Goal 的子树由新拥有方管理，重启后从原代理详情显式继续，沿原 Task/Thread 创建新 Turn，不改变当前主线；旧输出/transcript 按来源链读取，不能冒充新执行证据。后台进程限制为本 activation 的资源，持久 Job 状态和输出与执行结束分别保存。任务预算已移除，用量只做统计。

## 6. 产品交互

在现有代理设置页中：

- 内置角色和用户角色显示在同一列表；
- 用轻量来源标识区分“系统内置”和“我的角色”；
- 内置角色可查看和绑定引擎，不可删除；
- 用户角色可创建、编辑、复制、删除、导入和导出；
- 新建和编辑直接占用设置面板的角色工作区，不使用额外弹出层；表单提供返回角色列表的入口；
- 创建时复用当前详情页字段，不要求用户填写底层 JSON 或角色 ID；显示名称保存时自动生成合法且唯一的内部 ID，中文或无法转写为 ASCII 的名称使用 `custom-role` 系列 ID；
- 编辑已有角色时保持其内部 ID 不变，避免已有绑定、任务记录和导出文件失效；复制角色时自动生成 `-copy`、`-copy-2` 等唯一 ID；
- 新建角色默认继承主模型、使用安全的可派发 Worker 设置；
- 保存后立即出现在角色列表，并可被主 Agent 选择。

创建和编辑表单只暴露当前角色模型已有字段：基本信息、角色专长、约束、输出偏好、信号偏好、核心职责、领域能力和代理引擎。权限和 Worker 执行机制不另造一套编辑模型。

## 7. 导入与导出

### 7.1 导出

导出一个可独立阅读的 Markdown 角色文件，包含角色定义和能力引用。默认不包含：

- API Key、OAuth token 和其他凭据；
- Engine 完整连接配置；
- session、task、Worker 实例和 checkpoint；
- workspace 绝对路径；
- 运行统计和历史结果。

导出文件再次导入后，角色行为和提示词应保持一致；Engine 默认回到继承主模型，用户可在设置页重新绑定本地 Engine。

### 7.2 导入

导入必须由后端完成解析和校验。当前 UI 先以 `reject` 策略提交，后端发现用户角色 ID 冲突后返回 409，设置页再让用户选择覆盖或自动另存为；无论从哪条入口提交，最终写盘前都必须再次经过同一套校验。用户不需要填写新 ID；`rename` 未提供 `newId` 时，后端以原角色 ID 生成唯一的 `-copy`、`-copy-2` 等副本 ID。API 仍接受合法的显式 `newId`，用于脚本和已有集成。

1. 读取 Markdown 并校验 schema 版本；
2. 校验 ID、字段长度、枚举和必填字段；
3. 校验 capability 是否存在且适用于该角色；
4. 强制校验 `coordinatorMode`、TaskKind 和可派发性；
5. 检查与内置角色及用户角色的 ID 冲突；用户角色冲突时支持 `reject`、`overwrite`、`rename` 三种策略；
6. 原子写入用户角色目录；
7. 写盘成功后把新快照原子替换到共享 `AgentRoleRegistry`，并返回新的 `registryRevision`；
8. 角色绑定仍由 `agents` section 解析，未显式绑定 Engine 时通过默认 binding 表达“继承主模型”，不把 Engine 配置复制到角色文件；
9. 前端重新加载 registry 后，后续 Runner 从同一注册表看到角色，并通过现有 spawn 入口分配。

## 8. API 与模块职责

### `magi-agent-role`

- 以 `AgentRole` 定义并序列化唯一的规范角色对象；
- 统一加载 builtin 与 user roles；
- 提供角色来源、可派发性和 schema 校验；
- 提供单角色序列化、保存、删除和 registry reload；保存前先规范化/校验，成功后原子替换内存快照；
- 保持 capability 组合逻辑唯一。

### `magi-api`

在现有 registry 路由基础上增加角色管理操作。API 不再维护第二份 builtin 展示模板，而是把 `AgentRole` 转换为前端 `RoleTemplate`：

- 查询全部有效角色；
- 创建/更新用户角色；
- 删除用户角色；
- 导入角色文件内容；
- 导出角色文件内容；
- reload 后返回新的 registry revision。

API 不允许通过用户输入修改内置角色定义，也不允许导入凭据字段。

### `magi-settings-store`

继续保存 AgentBinding；角色正文和元数据由角色目录作为真相源，避免同一角色同时有文件版本和 settings JSON 版本。

### 前端

- 使用 API 返回的统一角色列表；
- 展示来源和管理操作；
- 创建/编辑表单提交角色定义；
- 导入先提交后端校验，冲突返回 409 后展示覆盖或自动另存为选择；
- 导出通过后端生成的 Markdown 文件下载；
- 保存、导入、删除后刷新 bootstrap/registry 数据。

### 8.1 已落地的接口契约

| 方法 | 路径 | 请求要点 | 成功响应要点 |
|---|---|---|---|
| `GET` | `/settings/registry/role-templates` | 无 | `{ templates }`，只返回可派发的 builtin/user 角色；每项包含 `source`、`editable`、`deletable`、`roleRevision` |
| `POST` | `/settings/registry/roles/upsert` | `{ role, expectedRoleRevision? }` | `{ role, registryRevision, agents }`；无 revision 表示创建，有 revision 表示基于当前版本编辑 |
| `POST` | `/settings/registry/roles/delete` | `{ templateId, expectedRoleRevision }` | `{ deleted, registryRevision, templates, agents }`；删除前检查活动 Worker 并清理该角色 binding |
| `POST` | `/settings/registry/roles/import` | `{ content, conflict: reject\|overwrite\|rename, newId? }` | 与 upsert 相同；`overwrite` 只允许覆盖已存在的用户角色；`rename` 默认自动生成唯一副本 ID，也可提供合法 `newId` |
| `GET` | `/settings/registry/roles/export?templateId=<id>` | 角色 ID | `{ templateId, fileName, content, registryRevision }`；`content` 是 schema v1 Markdown |
| `GET` | `/settings/registry/agents` | 无 | `{ agents }`；每个角色都有 binding，`engineId: ""` 表示继承编排模型 |
| `POST` | `/settings/registry/agents/upsert` | 顶层 `{ templateId, engineId, ... }` | `{ agents }`；非空 `engineId` 必须已存在于 engines registry |

角色保存和导入使用以下硬校验：ID 为 1-64 个小写字母、数字或单个连字符；显示名为 1-80 个字符；描述不超过 300 个字符；system prompt 为 1-50000 个字符；`version` 只接受 1；必须包含 `local_agent`、不能启用 `coordinatorMode`，并发上限（如有）必须大于 0；至少选择一个已注册 capability；`insightPreferences` 只能使用 `decision`、`contract`、`risk`、`constraint`。导入正文超过 100000 个字符直接拒绝。解析器遇到未知 front matter 字段、未知 TaskKind 或未知 capability 时返回明确错误，不写入任何文件。

## 9. 一致性与错误处理

- registry 是当前进程有效角色集合的唯一来源。
- 每次保存都先在内存构造并校验候选快照，再原子写文件；文件写入成功后只替换已经验证过的快照，避免“磁盘已更新、内存仍旧”的状态。
- 不存在的 capability、无效 TaskKind、空 system prompt、非法 ID 和重复 ID 必须拒绝。
- 自定义角色不可覆盖 builtin ID。
- 角色被运行中的任务使用时，编辑只影响后续新任务；当前 Worker 使用启动时的角色快照。
- 删除角色前检查 active Worker；若有运行中的 Worker，返回冲突并保持角色和 binding 不变。
- 删除角色和 binding 清理使用可恢复事务日志：先记录删除前像（`Prepared`），再清理 binding 和角色文件；两者完成后写入 `Committed`，最后清理日志。daemon 启动时，`Prepared` 恢复角色文件与 binding，`Committed` 只清理遗留日志。
- 角色保存、导入、删除操作应使用 revision 防止并发覆盖。

## 10. 稳定性、可用性与兼容性基线

- 角色写入采用原子替换；reload 失败时保留上一个有效快照并明确提示。
- Worker 启动时固定角色快照，编辑不会影响运行中的 Worker。
- 创建默认继承主模型，导入先由后端校验，冲突明确提供覆盖、自动另存为或取消。
- 内置角色的 ID、提示词、能力、排序和默认行为保持不变。
- 当前只接受 schema version `1`；未知版本明确拒绝并保留现有有效快照。未来升级必须提供一次性迁移后再提升 `CURRENT_ROLE_SCHEMA_VERSION`，运行时不长期保留双格式或双字段语义。
- 常用操作不依赖手动改文件或重启 daemon。

## 11. 验收标准

- 用户能在现有代理设置页创建一个自定义角色；
- 重启 Magi 后角色仍存在；
- 自定义角色与内置角色显示同样的详情和引擎绑定控件；
- 主 Agent 的可派发角色列表包含自定义角色；
- 主 Agent 能把任务分配给自定义角色，Worker 能完成执行并回传报告；
- 自定义角色可以编辑、删除和复制；
- 新建角色不要求输入角色 ID，系统根据显示名称生成稳定且唯一的内部 ID；
- 导出的文件可被另一实例导入；
- 导入后角色的 prompt、能力和展示信息保持一致；
- 导入不会写入或导出 API Key、工作区路径和运行记录；
- ID 冲突、非法字段、未知能力和不可派发角色均有明确错误；
- 内置角色不能被覆盖或删除；
- 角色变更无需重启 daemon 即可进入后续任务分配；
- 现有内置角色、spawn/wait、Worker Runtime 和模型绑定测试全部保持通过。
- 连续创建、编辑、导入、导出、删除和重启恢复不会丢失角色或绑定。
- 角色在设置页看到的最终信息与主 Agent 实际分配和 Worker 实际执行保持一致。


## 12. 维护约束

### 12.1 唯一数据流

维护时必须保持：角色文件是用户角色定义的真相源，AgentBinding 只保存引擎和显示覆盖，运行中的 Worker 使用 spawn 时生成的不可变快照。前端不得自行拼接一份角色后绕过后端 registry。

### 12.2 保存与刷新顺序

保存必须先解析、规范化、校验，并在内存中构造完整候选 registry，确保候选快照有效后再原子写盘。写盘成功后只执行不可失败的快照指针替换，不得在写盘后再次执行可能失败的全目录扫描。写盘失败时不替换旧快照。这样可以避免“磁盘已更新、内存仍旧、重启后突然生效”的不一致。

### 12.3 导入导出一致性

导入、创建和导出必须共享同一个 parser、normalizer 和 serializer。导入提交时由后端解析、规范化并校验；冲突操作提交前再次检查当前 revision。导出后在干净环境导入，角色的 ID、提示词、能力、任务类型和可派发状态必须保持一致。

### 12.4 调度可追溯性

当前 Worker 任务会携带角色 ID、任务绑定的能力集合和执行设置快照；Runner 的 `WorkerInfo` 再固定角色 prompt、支持的 TaskKind 和并发上限，Dispatcher 按该快照执行。`roleRevision` 用于编辑/删除冲突检测，`bindingRevision` 用于 binding 变化记录。若未来需要审计级 prompt 指纹，应在任务事件模型中增加独立字段，并保持向后兼容的迁移边界，不能把指纹偷偷塞进角色 Markdown。

### 12.5 禁止的实现捷径

不得新增第二套 Agent schema、第二套 registry 或第二个 spawn 入口；不得把导入文件直接复制到角色目录而跳过校验；不得找不到用户角色时静默改派内置角色；不得把 API Key、绝对路径、会话或运行记录写入导出文件。

### 12.6 热更新覆盖所有持有者

API、Dispatcher、Runner 与 daemon 角色目录 provider 使用同一个共享注册表句柄，读取时只在短读锁内克隆不可变快照，不持锁执行模型请求。保存成功后后续任务与已存在会话的角色发现读取最新目录；已启动 Worker 继续持有派发时的快照。仅替换 API 自己的指针不能构成注册表热更新。

### 12.7 能力归属闭环

规范角色持有明确的可用 capability ID 集合，用户角色从可配置的领域能力目录选择。注册与 spawn 都校验任务实际激活的能力是目标角色可用集合的非空子集；不能再用旧的 builtin ID 归属规则否决自定义角色。能力 prompt 和工具权限仍由现有系统控制。

### 12.8 版本语义

- `version` 是 Markdown 格式版本，不是角色编辑次数。
- `roleRevision` 是单角色内容修订标识，用于编辑冲突和任务快照；由后端生成。
- `registryRevision` 是整个有效目录的修订标识，用于刷新和缓存失效。
- `bindingRevision` 只跟踪模型绑定变化。
- 导出不携带当前机器的 registry/binding revision；导入创建新的本地 roleRevision。

### 12.9 删除与绑定的事务边界

新建角色的默认继承绑定由现有 `default_agent_binding` 动态生成，不需要同时写角色文件和 settings。编辑角色不顺带修改模型绑定，两种保存分别提交。删除角色涉及角色文件和已有绑定：实现必须提供一个可恢复的删除事务，记录角色文件与绑定前像、持久化提交状态，启动时先完成恢复再发布角色目录；不能先删文件后尝试删 binding，并把后半步失败忽略。运行中的任务不读取这份可变绑定，因此恢复不应改变已接纳任务。事务记录只用于本地一致性恢复，不参与导出，也不是第二份角色真相源。

### 12.10 已验证的实现边界

以下边界属于产品契约，后续实现不能放宽，否则会重新引入“能显示但不能调度”或“重启后才生效”的问题：

- 用户角色文件的 `id` 必须与文件名 stem 完全一致；front matter 省略 `id` 时才允许从文件名推导。这样角色绑定、导出文件名和 reload 结果始终指向同一个 ID。
- `overwrite` 只表示覆盖已存在的用户角色。内置角色和不存在的目标都返回冲突，不能把覆盖请求静默解释为创建。
- 生产 Runner 在每次匹配尚未派发的任务时读取共享 registry 的最新 Worker catalog；成功派发后固定当次 `WorkerInfo` 快照，角色编辑不会改写运行中的 Worker。
- `parallelismLimit` 是角色级真实并发限制。设置为正整数时按该值准入；省略时不增加角色级限制，但全局和会话级执行准入仍有效。不能用固定默认值覆盖用户配置。
- 角色文件写入、registry 快照替换、binding 清理和错误恢复必须保持原子边界；任何失败都要保留旧有效状态并向 API 返回可识别错误。

## 13. 实现与验证入口

- `crates/magi-agent-role/src/lib.rs`：解析、规范化、builtin 保护、原子 registry 刷新与持久化。
- `crates/magi-api/src/routes/settings.rs`：角色管理、revision 冲突与删除恢复事务。
- `crates/magi-conversation-runtime/src/agent_spawn_preflight.rs`、`task_execution_dispatcher.rs`：统一可派发角色与不可变 Worker 快照。
- Web 代理设置与 `test:settings-store`：创建、编辑、导入冲突、导出、引擎绑定与目录刷新。

按改动运行 `cargo test -p magi-agent-role`、受影响的 API/调度测试，以及 Web check 和对应回归。涉及产品行为时从 daemon 主入口验证 §11 的场景；通过记录绑定本次源码与构建，不在本文保留一次性实施进度。
