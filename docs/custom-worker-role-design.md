# Magi 用户自定义子代理角色设计方案

## 1. 目标与范围

Magi 是面向软件开发工作的本地多代理协作环境。主 Agent 负责理解目标、编排任务和汇总结果，子代理 Worker 负责在明确角色职责内完成探索、设计、实现、测试或评审。

本方案只解决一个问题：在保留现有子代理 Worker 体系、任务分配方式、执行生命周期和设置界面的前提下，允许用户创建、编辑、删除、导入和导出自定义角色，并保证自定义角色可以被主 Agent 识别并正常分配任务。

本方案不新增第二套 Agent 类型，不改变 Worker Runtime，不引入 Agent 市场、团队 Agent 或独立权限平台。

本方案按正式产品能力设计，而不是临时增加几个设置字段。角色创建、保存、加载、导入、导出和任务分配必须形成可长期维护的单一闭环；任何失败都要可见、可恢复，不能以静默丢失配置、重启才能生效或破坏现有内置角色为代价换取“功能可用”。

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

## 3. 现状与差距

实现复用以下既有基础：

- `magi-agent-role` 提供 Markdown role 解析、内置角色和用户角色目录加载；daemon 的实例目录为 `<state_root>/roles`，默认状态根为 `~/.magi`。
- `ProfessionalCapabilityRegistry` 提供角色能力注册和组合。
- `AgentBinding` 提供角色到 Engine 的绑定，以及继承主模型的语义。
- `magi-orchestrator`、`magi-spawn-graph` 和 `magi-worker-runtime` 提供任务分派、父子关系、Worker 生命周期、工具调用和结果回传。
- Agent 设置页已经具备角色列表、角色详情和代理引擎绑定界面。

本次实现收敛了此前的差距：角色注册表、API、前端 registry、Engine 绑定和 spawn 校验现在使用同一份有效角色集合；用户角色的创建、编辑、删除、导入、导出和进程内刷新都进入同一闭环。

## 4. 统一数据模型

### 4.1 角色定义

继续使用现有 `RoleTemplate`/`AgentRole` 语义。实现中的 `AgentRole` 就是后端规范化角色对象，承担唯一角色定义真相源；没有再引入一套并行的 `RoleDefinition` 结构。内置 Markdown 资产和用户 Markdown 都解析为 `AgentRole`，API 从它生成展示 DTO，Worker 目录从它生成调度信息。规范对象至少包含：

- `templateId`：API 使用的全局唯一稳定 ID；对象内部字段名为 `id`；
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

角色定义和绑定配置分开保存。导出角色时只导出可选的非敏感绑定偏好，不嵌入 Engine 的 API Key、连接地址或本机状态。

### 4.3 持久化

- 内置角色：编译期 Markdown 资产，只读；启动时解析为 `AgentRole`。
- 用户角色：daemon 使用 `<state_root>/roles/<templateId>.md`，独立运行时默认使用 `~/.magi/roles/<templateId>.md`；文件采用原子写入，并与内置角色共享 ID 命名空间。
- 用户角色 ID 与任何内置 ID 冲突时必须拒绝，不能再使用当前“同 ID 覆盖 builtin”的加载行为。
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
现有 agent_spawn / 任务分派
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
- spawn graph 和结果回传。

## 6. 产品交互

在现有代理设置页中：

- 内置角色和用户角色显示在同一列表；
- 用轻量来源标识区分“系统内置”和“我的角色”；
- 内置角色可查看和绑定引擎，不可删除；
- 用户角色可创建、编辑、复制、删除、导入和导出；
- 创建时复用当前详情页字段，不要求用户填写底层 JSON；
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

导入必须由后端完成解析和校验。当前 UI 先以 `reject` 策略提交，后端发现用户角色 ID 冲突后返回 409，设置页再让用户选择覆盖或另存为；无论从哪条入口提交，最终写盘前都必须再次经过同一套校验。

1. 读取 Markdown 并校验 schema 版本；
2. 校验 ID、字段长度、枚举和必填字段；
3. 校验 capability 是否存在且适用于该角色；
4. 强制校验 `coordinatorMode`、TaskKind 和可派发性；
5. 检查与内置角色及用户角色的 ID 冲突；
6. 检查与内置角色及用户角色的 ID 冲突；用户角色冲突时支持 `reject`、`overwrite`、`rename` 三种策略；
7. 原子写入用户角色目录；
8. 写盘成功后把新快照原子替换到共享 `AgentRoleRegistry`，并返回新的 `registryRevision`；
9. 角色绑定仍由 `agents` section 解析，未显式绑定 Engine 时通过默认 binding 表达“继承主模型”，不把 Engine 配置复制到角色文件；
10. 前端重新加载 registry 后，后续 Runner 从同一注册表看到角色，并通过现有 spawn 入口分配。

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
- 导入先提交后端校验，冲突返回 409 后展示覆盖或另存为选择；
- 导出通过后端生成的 Markdown 文件下载；
- 保存、导入、删除后刷新 bootstrap/registry 数据。

### 8.1 已落地的接口契约

| 方法 | 路径 | 请求要点 | 成功响应要点 |
|---|---|---|---|
| `GET` | `/settings/registry/role-templates` | 无 | `{ templates }`，只返回可派发的 builtin/user 角色；每项包含 `source`、`editable`、`deletable`、`roleRevision` |
| `POST` | `/settings/registry/roles/upsert` | `{ role, expectedRoleRevision? }` | `{ role, registryRevision, agents }`；无 revision 表示创建，有 revision 表示基于当前版本编辑 |
| `POST` | `/settings/registry/roles/delete` | `{ templateId, expectedRoleRevision }` | `{ deleted, registryRevision, templates, agents }`；删除前检查活动 Worker 并清理该角色 binding |
| `POST` | `/settings/registry/roles/import` | `{ content, conflict: reject\|overwrite\|rename, newId? }` | 与 upsert 相同；`overwrite` 只允许用户角色，`rename` 必须给出合法 `newId` |
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
- 创建默认继承主模型，导入先由后端校验，冲突明确提供覆盖、另存为或取消。
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
- 导出的文件可被另一实例导入；
- 导入后角色的 prompt、能力和展示信息保持一致；
- 导入不会写入或导出 API Key、工作区路径和运行记录；
- ID 冲突、非法字段、未知能力和不可派发角色均有明确错误；
- 内置角色不能被覆盖或删除；
- 角色变更无需重启 daemon 即可进入后续任务分配；
- 现有内置角色、spawn/wait、Worker Runtime 和模型绑定测试全部保持通过。
- 连续创建、编辑、导入、导出、删除和重启恢复不会丢失角色或绑定。
- 角色在设置页看到的最终信息与主 Agent 实际分配和 Worker 实际执行保持一致。


## 12. 设计复核结论与实施边界

本方案已按实现结果完成复核，下面的结构性结论是后续维护和扩展的边界：

1. **保持单一规范对象。** `AgentRole` 同时承载运行时、产品展示和持久化所需的角色字段；`RoleTemplate` 只是 API/前端 DTO。新增字段先进入 `AgentRole`、解析器、序列化器和校验，再映射到 DTO，不能在 API 单独增加一份 builtin 数组。
2. **保持 builtin 保护。** 用户文件与 builtin 共用 ID 命名空间；冲突时拒绝用户定义并保留 builtin，任何 API 也不能覆盖或删除 builtin。
3. **保持能力闭环。** `capabilities` 属于角色定义，通过 `ProfessionalCapabilityRegistry` 校验；任务携带的激活能力必须是目标角色可用能力的非空子集。
4. **保持可调度一致性。** API 只展示 `is_spawnable_agent_role` 认可的非协调 Worker 角色；工具目录、Runner、Dispatcher 和 spawn 校验从同一共享 registry 读取。
5. **保持 Worker 快照。** Runner 创建时复制 `WorkerInfo`（含角色 prompt、任务类型和并发限制），Dispatcher 执行时按该快照组合任务能力；后续编辑只影响新建 Runner/新任务。

因此，维护顺序仍应是先修改规范对象和加载规则，再接 API、前端和调度，并在同一变更中补齐持久化恢复和回归验证。

## 13. 实现关键点（供后续开发执行）

### 13.1 唯一数据流

实现时必须遵循：角色文件是用户角色定义的真相源，AgentBinding 只保存引擎和显示覆盖，运行中的 Worker 使用 spawn 时生成的不可变快照。前端不得自行拼接一份角色后绕过后端 registry。

### 13.2 保存与刷新顺序

保存必须先解析、规范化、校验，并在内存中构造完整候选 registry，确保候选快照有效后再原子写盘。写盘成功后只执行不可失败的快照指针替换，不得在写盘后再次执行可能失败的全目录扫描。写盘失败时不替换旧快照。这样可以避免“磁盘已更新、内存仍旧、重启后突然生效”的不一致。

### 13.3 导入导出一致性

导入、创建和导出必须共享同一个 parser、normalizer 和 serializer。导入提交时由后端解析、规范化并校验；冲突操作提交前再次检查当前 revision。导出后在干净环境导入，角色的 ID、提示词、能力、任务类型和可派发状态必须保持一致。

### 13.4 调度可追溯性

当前 Worker 任务会携带角色 ID、任务绑定的能力集合和执行设置快照；Runner 的 `WorkerInfo` 再固定角色 prompt、支持的 TaskKind 和并发上限，Dispatcher 按该快照执行。`roleRevision` 用于编辑/删除冲突检测，`bindingRevision` 用于 binding 变化记录。若未来需要审计级 prompt 指纹，应在任务事件模型中增加独立字段，并保持向后兼容的迁移边界，不能把指纹偷偷塞进角色 Markdown。

### 13.5 禁止的实现捷径

不得新增第二套 Agent schema、第二套 registry 或第二个 spawn 入口；不得把导入文件直接复制到角色目录而跳过校验；不得找不到用户角色时静默改派内置角色；不得把 API Key、绝对路径、会话或运行记录写入导出文件。

### 13.6 产品验收场景

必须验证：创建后无需重启即可出现在设置页和后续任务分配；绑定指定 Engine 后 Worker 按该 binding 解析模型；编辑只影响后续新建 Runner；导出后在干净目录导入仍可执行；同 ID 导入支持拒绝、覆盖和另存为；非法能力、非法 ID 和协调器角色被拒绝；活动 Worker 使用时删除被阻止；删除中途故障可由启动恢复；并发编辑不会互相覆盖；导出内容不含敏感信息。

### 13.7 热更新必须覆盖所有持有者

当前 `ApiState`、`LlmTaskDispatcher`、`TaskRunner` 和 daemon 的角色目录 provider 都持有或捕获角色注册表。仅替换 `ApiState.agent_role_registry` 不会让其他持有者更新。实现必须由 daemon 注入同一个共享注册表句柄，例如 `Arc<RwLock<Arc<RoleRegistrySnapshot>>>`；读取时只在短读锁内克隆快照，不能持锁执行模型请求。API、目录 provider、后续新任务和 Runner 从同一共享句柄读取，Worker 已启动时持有旧不可变快照。必须验证同一个已存在会话在新增角色后也能发现并调用它。

### 13.8 能力归属必须闭环

当前 capability 的适用范围通过 `supported_roles` 判断，自定义 ID 不会自动获得仅限内置 ID 的能力。实现不能只在 UI 添加能力多选框。规范化角色应持有明确的可用 capability ID 集合；内置集合从现有能力归属转换，保持结果一致；用户从可配置的领域能力目录选择。任务实际激活的能力必须是目标角色可用集合的非空子集。注册和 spawn 都通过这一条规则，不能同时再用旧的 builtin ID 归属规则否决自定义角色。能力 prompt 和工具权限仍由现有系统控制。

### 13.9 版本语义

- `version` 是 Markdown 格式版本，不是角色编辑次数。
- `roleRevision` 是单角色内容修订标识，用于编辑冲突和任务快照；由后端生成。
- `registryRevision` 是整个有效目录的修订标识，用于刷新和缓存失效。
- `bindingRevision` 只跟踪模型绑定变化。
- 导出不携带当前机器的 registry/binding revision；导入创建新的本地 roleRevision。

### 13.10 开发前必须关闭的事务边界

新建角色的默认继承绑定由现有 `default_agent_binding` 动态生成，不需要同时写角色文件和 settings。编辑角色不顺带修改模型绑定，两种保存分别提交。删除角色涉及角色文件和已有绑定：实现必须提供一个可恢复的删除事务，记录角色文件与绑定前像、持久化提交状态，启动时先完成恢复再发布角色目录；不能先删文件后尝试删 binding，并把后半步失败忽略。运行中的任务不读取这份可变绑定，因此恢复不应改变已接纳任务。事务记录只用于本地一致性恢复，不参与导出，也不是第二份角色真相源。

## 14. 工程规范对照

### 根本原因

原有实现把运行时 `AgentRole`、API 层内置 `RoleTemplate`、用户角色文件和 `AgentBinding` 分散在不同入口，导致用户角色即使能被文件加载，也可能无法在设置页展示、绑定 Engine 或进入 spawn 校验；同时同 ID 用户文件可以覆盖 builtin，删除角色也没有完整的跨文件恢复边界。

### 解决方案

以 `AgentRole` 作为唯一角色定义，统一内置和用户角色的解析、规范化、校验、序列化、展示和调度入口；以内部不可变 registry 快照贯通 API、前端、Dispatcher、Runner 和 spawn；以原子持久化、`roleRevision`、`registryRevision` 和 `Prepared/Committed` 恢复事务保证一致性；以 schema version 1 的单一 Markdown 格式避免长期双轨。

### 与 cn-engineering-standard 的对应关系

- 已先建立项目结构、调用链、数据流和职责边界；
- 方案明确最小充分改动：复用现有 Worker、Role、Capability、Binding 和 spawn，不新建 Agent 平台；
- 明确禁止第二套 schema、registry、spawn 入口、静默回退和临时兼容分支；
- 明确发现、修复、清理、测试、验证、迭代闭环，以及每阶段状态记录方式；
- 明确现有内置角色的回归边界、错误路径、恢复路径和产品验收场景；
- 当前实现已覆盖代码、自动化测试和构建验证；阶段 5 的 daemon/浏览器产品验收与最终提交状态以开发计划文档的最新记录为准。
