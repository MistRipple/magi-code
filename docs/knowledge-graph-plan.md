# 知识图谱开发与验收计划

本文档是 Magi 知识图谱能力的开发基线。实现、评审和最终验收都以本文档为准；未达到阶段退出条件，不进入下一阶段。

## 0. 当前推进状态

- 阶段 0：已落地模型合同，并通过序列化、校验和旧状态兼容验证。
- 阶段 1：已完成工程拓扑图、只读图查询、前端图谱 Tab 和 daemon 入口验收。
- 阶段 2：已完成，已落地显式关系持久化、workspace 校验、关系 CRUD、关系编辑器、文件/符号选择和 dangling 投影，并通过状态恢复与 watcher 删除验收。
- 阶段 2.5：已完成，后端自动候选生成、稳定去重、确认/忽略审阅审计和索引刷新链路已落地。
- 阶段 3A：已完成，新增只读 `knowledge_graph_query` Agent 工具。
- 阶段 3B：已完成，知识命中后的局部图扩展已接入 ContextRuntime；正文和图摘要共享预算，候选/推断/悬空关系保留明确标识，索引未就绪时保持原有文本检索行为。
- 阶段 4：已完成核心维护与治理链路；自动候选刷新、CodeIndex 投影、watcher、周期对账、重建、按需建索引和统一持久化已收敛，并补齐候选失效与 dangling 回归验证。

## 1. 目标与边界

知识图谱用于把工作区中的项目知识、代码文件和代码符号连接起来，支持从知识追踪代码、从代码反查知识，并为 Agent 提供有证据的关系查询。

第一期不引入外部图数据库，不复制代码依赖边，不做全自动事实写入，也不把 Project Memory、Session、Task 直接并入主图谱。

代码依赖的唯一事实源仍是 `magi-knowledge-store` 内已有的 `DependencyGraph`；图谱层只负责统一节点、边协议和查询聚合。

## 2. 团队分工

| 角色 | 负责范围 | 交付物 |
|---|---|---|
| 后端/索引 | 图节点、关系模型、图查询、索引聚合 | Rust domain/API、单元测试 |
| 前端/交互 | KnowledgePanel 图谱入口、筛选、节点详情、代码跳转 | Svelte 组件、Web API、交互测试 |
| 架构/验收 | workspace 隔离、持久化迁移、性能和回归检查 | 阶段评审、验收报告、阻断项 |

## 3. 统一领域模型

### 3.1 节点

第一期支持四类节点：

- `workspace:{workspace_id}`
- `file:{normalized_relative_path}`
- `symbol:{relative_path}:{qualified_name}:{kind}`
- `knowledge:{knowledge_id}`

文件节点只保存工作区相对路径；符号 ID 不使用绝对路径、标题或行号，行号只能作为证据字段。

### 3.2 关系

派生关系由代码索引产生，不进入 `knowledge.json`：

- `workspace -> file`: `contains`
- `file -> file`: `depends_on`
- `file -> symbol`: `contains`

显式关系进入 `KnowledgeState`：

- `knowledge -> file/symbol`: `applies_to`、`explains`、`references`
- `knowledge -> knowledge`: `related_to`、`supersedes`、`contradicts`

### 3.3 来源、状态与证据

关系来源：`deterministic_code`、`explicit_user`、`explicit_agent`、`inferred`。

关系状态：`active`、`candidate`、`dangling`、`rejected`。

推断关系默认只能是 `candidate`；用户确认后才能进入 `active`。每条关系都应携带来源、状态、可选置信度和证据摘要。

## 4. 分阶段计划

### 阶段 0：模型合同

范围：定义节点、关系、来源、状态、证据、稳定 ID、workspace 校验和旧数据迁移规则。

退出条件：

- 模型序列化 round-trip 通过；
- 非法路径、非法置信度、跨 workspace 关系被拒绝；
- 旧知识记录加载后不丢失；
- 现有知识查询行为不改变。

### 阶段 1：工程拓扑图

范围：聚合现有文件依赖、文件包含符号和工作区知识节点，新增只读图查询接口：

```text
GET /api/knowledge/graph
```

查询必须支持 workspace 绑定、焦点、深度、方向、节点/关系过滤、最大节点数和最大边数。

服务端默认深度为 1，最大深度为 3；单次最多返回 120 个节点、240 条边，超过上限必须返回 `truncated: true`。

退出条件：

- 依赖结果与现有 `DependencyGraph` 一致；
- 不新增第二套依赖解析、BFS 或文件监听；
- 文件修改和删除后结果同步；
- workspace 之间不能串数据；
- 重建与重启恢复结果一致；
- API 结果稳定排序且有索引状态。

### 阶段 2：知识关联图

范围：ADR、FAQ、Learning 与文件、符号和其他知识建立显式关系，增加关系查询、创建和删除接口，关系与知识记录在同一份知识状态中原子持久化。

退出条件：

- 支持知识到文件、符号和知识的关系；
- 删除知识后没有悬空活动关系；
- 代码节点消失后关系进入 `dangling`；
- 重启后关系、证据和状态完整保留；
- 旧版 `knowledge.json` 可迁移。

阶段验收记录：

- Web KnowledgePanel 的“图谱”Tab 已提供知识关联编辑器，可创建、编辑和删除 `explicit_user` 关系；来源使用知识库列表，目标使用当前代码图中的文件、符号和知识节点。
- 关系证据按行编辑，关系状态在图谱边和关系列表中显示；代码索引节点消失后，关系列表显示 `dangling`，不自动删除关系。
- watcher 删除事件会复用现有索引对账逻辑即时校正文件集合，避免等待 30 秒周期对账才投影 `dangling`。
- `KnowledgeStore::from_state` round-trip 保留关系、证据和状态；知识删除级联清理关系。
- 已验证：`cargo test -p magi-knowledge-store`（96 个）、`cargo test -p magi-api`（557 个）、`cargo fmt --all -- --check`、`npm --prefix web run check`、`npm --prefix web run build`，以及 daemon 主入口和真实关系 API CRUD。

### 阶段 2.5：自动发现审阅体验

目标：把关系维护从“用户主动编辑”为主，推进到“系统自动发现、用户局部审阅”为主，同时保持候选关系不等同于事实。

范围：

- 系统基于现有知识内容、代码路径、符号索引和确定性代码关系，自动生成知识到文件、符号或其他知识的关系候选；
- 自动生成的关系统一使用 `origin: inferred`、`status: candidate`，默认进入待审阅状态，不得直接进入 `active`；
- 提供以知识节点、文件节点或符号节点为焦点的局部聚焦图，只加载有限深度和数量的相关节点，默认不打开完整工作区图；
- 点击候选关系或图谱边后展示证据详情，包括来源字段、匹配内容或代码路径、符号信息、置信度、生成时间和当前状态；
- 审阅操作必须支持：
  - `确认`：将候选关系转为 `active`，保留自动发现来源和审阅记录；
  - `忽略`：将候选关系转为 `rejected`，后续自动刷新不得立即重复生成同一候选；
  - `修正`：在确认前修改目标节点或关系类型，再以用户确认后的关系保存为 `active`，保留原候选证据和修正记录；
- 手动新增关系保留为低频二级能力：默认不作为首屏主操作，不与自动发现审阅争夺主要视觉入口；只有用户明确进入“添加关系”操作时才展示，并沿用现有 workspace、节点合法性和证据校验；
- 自动发现失败、索引未就绪、结果截断和关系 dangling 都必须在局部图和审阅列表中提供可理解的状态反馈，不得静默显示为空或伪装成有效关系。

产品主流程：

```text
进入知识/图谱
  -> 选择或打开焦点节点
  -> 查看局部聚焦图和“待审阅”候选
  -> 打开关系证据详情
  -> 确认 / 忽略 / 修正
  -> 刷新局部图并保留审阅结果
```

审阅规则：

- `candidate` 是自动发现结果的默认且唯一初始状态；
- 未经用户确认的候选关系不得参与需要确定事实的 Agent 上下文；如被展示，必须明确标注“待审阅”；
- `rejected` 关系必须具备稳定去重依据，避免同一知识、目标和关系类型在每次重建时反复出现；
- `dangling` 关系不能被确认或继续作为有效代码关联，必须先修正目标或等待代码节点恢复；
- 自动发现结果必须稳定排序，优先展示置信度较高且证据完整的候选，但排序不能改变关系状态；
- 局部聚焦图的服务端深度、节点数和边数限制继续沿用阶段 1 的上限，并返回 `truncated: true`。

退出条件：

- 系统可以自动生成至少一类知识到代码或知识到知识的候选关系；
- 自动生成关系默认显示为 `candidate` 和“待审阅”，不会直接写入 `active`；
- 用户可以围绕知识、文件或符号打开局部聚焦图，并在图中定位候选关系；
- 用户可以查看完整证据详情，并分别完成确认、忽略和修正；
- 忽略结果在重新索引或刷新后不会立即重复出现；
- 修正后的目标节点和关系类型可以正确持久化，原始发现证据仍可追溯；
- 手动新增入口存在但处于低频二级位置，且不影响自动审阅主流程；
- 空态、失败态、截断态和 dangling 状态均有明确文案、状态标识和可执行的下一步；
- workspace 切换、刷新、重建和重启后，候选及审阅状态不串 workspace、不丢失、不重复膨胀。

当前前端交付记录：

- 图谱 Tab 已改为“待审阅、聚焦视图、已处理”三种工作模式；默认首屏优先展示 candidate 关系，手动添加关系收进折叠的低频二级入口。
- 关系详情展示来源、状态、置信度、证据、更新时间和局部图；候选关系可确认、忽略或先修正目标/关系类型再确认，状态写入既有关系 CRUD 协议。
- Cytoscape 画布已支持节点和边点击；按知识、文件、符号区分节点样式，按候选、已忽略、悬空区分关系样式，并提供全局视图开关、搜索、节点类型筛选、图例和截断提示。
- 已完成 Web 侧静态验证：npm --prefix web run check、npm --prefix web run build、git diff --check；daemon 主入口的桌面宽度和 390px 宽度均已人工检查，页面无横向溢出。
阶段 2.5 后端交付记录：

- `KnowledgeStore::build_workspace_index` 在代码索引完成后复用现有 `CodeGraphSnapshot` 生成知识到文件/符号的 `inferred + candidate` 关系；知识标题、标签、正文与代码路径/符号名的匹配词、路径、符号和行号均写入证据。
- 自动关系使用稳定的 `discoveryKey` 和基于候选指纹的关系 ID；重复重建、刷新和 watcher 增量更新不会膨胀。
- `rejected` 候选会保留稳定指纹，自动刷新不会重新生成；显式关系不会被自动候选覆盖。
- `reviewedAt` 记录确认/忽略时间；未审阅的 `inferred` 关系只能是 `candidate`，确认后的 `active` 和忽略后的 `rejected` 保留 `origin: inferred` 及原始证据。
- 用户修正目标后保留原始 `discoveryKey`，后续自动刷新不会覆盖修正结果；关系状态和审阅字段兼容旧版序列化状态。
- 已补充自动候选、稳定去重、忽略去重、审阅约束、状态恢复和 workspace 隔离测试。

### 阶段 3：图谱驱动检索

范围：保留 `knowledge_query` 的文本检索语义，新增只读 `knowledge_graph_query`，并将知识命中后的 1～2 层图扩展纳入上下文预算。

阶段 3A 已交付：

- 新增 public、只读、低风险、幂等的 `knowledge_graph_query` Agent 工具，复用 `KnowledgeStore::query_workspace_graph`，不新增第二套图算法或图数据库。
- 工具要求 `focus`，默认深度 1、最大深度 2；节点、边、方向、节点数、边数和估算 token 预算均受限。
- 返回节点、关系、来源、状态、置信度、证据、统计和 `truncated`；候选关系额外返回计数，保留 `candidate/inferred` 标识，不注入为确定事实。
- 工具已接入 builtin catalog、公共工具 schema、注册表、并发安全策略、安全网关、工具健康目录和前端调用显示名。
- 已补充 focus 必填、workspace 隔离、候选关系保留、上下文预算截断和工具目录回归测试。

阶段 3B 已交付：

- `ContextRuntime::select_knowledge_on_demand` 只在已有知识命中后，以 `knowledge:{id}` 为焦点查询 2 层局部图，不打开完整 workspace 图，也不绕 Agent 工具链调用图查询工具。
- 图摘要采用独立的紧凑结构，限制节点、边、证据和字符数量；正文消耗后的剩余知识上下文预算才用于图摘要，整体 `injected_chars` 和 `truncated` 同时反映两者。
- 确定关系、自动推断关系和 dangling 关系分段渲染；`candidate` / `inferred` 明确标注为验证线索，`rejected` 不进入 prompt。
- 图索引未就绪时跳过图扩展，不影响既有文本知识检索和正文注入；workspace 仍由知识查询和图查询共同约束。
- `knowledge.context.selected` 诊断事件新增图焦点、节点、边、候选、推断、dangling、字符和截断统计，但不写入完整图正文。
- 已补充 ContextRuntime 测试：候选关系标注、无代码索引的文本检索保持、workspace 边界、图摘要预算和截断传播。

退出条件：

- Agent 可查询邻居和关系路径；
- 图扩展不突破上下文预算；
- 结果包含关系来源、证据和截断状态；
- 推断关系不会被当作确定事实；
- 现有知识检索和代码检索无回归。

### 阶段 4：持续维护与治理

范围：统一 watcher、增量索引、周期对账、重建和重启路径，补齐关系失效、候选确认、审计和性能治理；阶段 2.5 产生的候选、忽略、修正和确认结果必须纳入同一条维护链路。

退出条件：

- watcher、reconcile、重建、重启结果一致；
- inferred 关系不会自动激活；
- 关系证据可定位；
- 大型工作区不会产生无界响应；
- 迁移失败可恢复；
- 全部现有 Rust/Web/API 测试保持通过。

阶段 4 已交付：

- watcher 增量事件、删除即时对账、30 秒周期对账、全量重建和按需索引统一复用 `refresh_inferred_relations_for_workspace`，不新增第二套关系维护逻辑。
- 代码索引摘要也纳入同一条维护链路：增量事件和周期对账会同步 `KnowledgeState` 中的 workspace-scoped `CodeIndex` 投影；周期对账会重新扫描当前文件集合，补齐 watcher 丢失的新增文件。空工作区保留可查询的运行时空摘要，但不写入伪造的零文件知识记录。
- 未审阅候选会跟随当前索引结果收敛：目标仍存在但匹配已失效时删除，目标代码节点消失时保留审计记录并在图查询中投影为 `dangling`。
- 已确认、已忽略和用户修正的推断关系不被自动刷新覆盖；重复刷新不会改写未变化关系的时间戳，也不会重复生成稳定指纹相同的候选。
- 自动候选集合或 `CodeIndex` 投影实际变化后，在释放状态锁后统一触发持久化回调；watcher、按需图查询、API/Git/daemon 后台建索引不再各自直接写盘。运行态持久化使用 dirty 合并的单写入 worker，避免并发快照互相覆盖。
- API 运行态持久化边界负责注册知识状态回调，watcher 和周期对账产生的关系、代码摘要变化可跨重启恢复；未注册回调的独立 Store 保持纯内存行为。
- 已补充候选收敛、代码删除后 dangling、重复刷新时间戳稳定、按需索引回调、空工作区语义、持久化回调去重与锁释放、状态恢复和 workspace 隔离测试。

性能收口记录：

- 单个 workspace 的自动候选关系最多保留 2,000 条，按候选状态、置信度、更新时间和稳定 ID 排序，避免索引结果无界增长。
- `GET /api/knowledge/relations` 默认及最大返回 1,000 条，并返回 `totalRelations` 与 `truncated`；前端在图谱页明确提示当前展示的是优先级最高的一部分，不把截断伪装成完整结果。
- 大型工作区的图查询继续限制为最多 120 个节点、240 条边；焦点节点在截断时强制保留，避免出现只有节点没有关系的空洞视图。
- 已在真实 daemon 主入口验证：约 17,200 个代码图节点的 workspace 索引可收敛到 `ready`，关系接口稳定返回不超过 1,000 条，图谱页可正常打开且无控制台错误。
- Web 直连与宿主桥接的 `projectKnowledgeLoaded` 读取模型已统一携带关系列表及截断元数据；IDE/桌面路径不再退化为只有代码拓扑、无法审阅自动候选关系的只读页面。

### 最终功能收口记录

- 阶段 0～4 的功能链路已完成：模型与迁移、工程拓扑、显式关系、自动发现审阅、图谱驱动检索、统一维护与治理均已落地；本次只补齐最终交互和渲染收口，不新增第二套图谱实现。
- 图谱默认以“待审阅”为主入口，系统自动生成的关系保持 `candidate`，用户只需查看证据并确认、忽略或修正；手动新增关系仍保留在二级入口，不要求用户维护整张图。
- 局部图点击节点会通过服务端按焦点重新查询，支持知识、文件和符号三类节点；文件/符号可跳转代码，知识节点可回到对应知识条目，并在 workspace 切换时取消旧请求、清空旧图状态。
- 图形化问题的根因是 Cytoscape 节点使用固定尺寸但 CJK 文本仍按空格换行，长文本因此溢出节点卡片。现已统一设置任意字符换行、140px 文本宽度、160×64px 节点尺寸和居中排版，移除依赖 `height: label` 的不稳定布局；节点和边状态样式也统一收敛到同一渲染器。
- 聚焦图不再依赖未指定根节点的力导向布局：以当前知识/焦点节点为中心使用同心层布局，节点按关系距离分层，连接线置于卡片下方并默认隐藏关系文字，仅选中边显示文字；切换审阅队列与独立聚焦视图时通过 `ResizeObserver` 重新适配画布，避免图形偏移或挤在一角。
- 图谱卡片只显示压缩后的可读标题，完整内容仍保留在节点详情和关系证据中；全局图继续作为缩略总览，聚焦图作为主要阅读入口，避免将 120 个节点强行以同一缩放比例展示。
- 本次收口已通过 `git diff --check`、`npm --prefix web run check`、`npm --prefix web run build`、知识库与 API 相关 Rust 测试，以及 daemon 主入口的真实图谱局部查询和浏览器验收。
- 完整 Web golden 测试仍有一个既有基线阻断：`web/scripts/right-pane-golden.mjs` 仍期待旧的 `record.host.addChildView(record.view, 1)` 调用，而当前桌面 Surface 改动已采用新的挂载顺序；该失败与知识图谱改动无关，未回退其他 agent 的桌面改动。

## 5. 首批实现文件

阶段 0/1 预计涉及：

- `crates/magi-knowledge-store/src/graph.rs`
- `crates/magi-knowledge-store/src/graph_query.rs`
- `crates/magi-knowledge-store/src/lib.rs`
- `crates/magi-knowledge-store/src/state.rs`
- `crates/magi-knowledge-store/src/local_search_engine.rs`
- `crates/magi-api/src/routes/knowledge.rs`
- `web/src/components/KnowledgePanel.svelte`
- `web/src/components/KnowledgeGraphPanel.svelte`
- `web/src/web/agent-api.ts`
- `web/src/i18n/zh-CN.json`
- `web/src/i18n/en-US.json`

桌面 bridge 只有在桌面模式实际需要图谱请求时同步扩展，必须保持 Web 和桌面两条链路协议一致。

## 6. 验证与最终验收

阶段验证命令：

```bash
cargo fmt --all -- --check
cargo check -p magi-knowledge-store
cargo check -p magi-api
cargo test -p magi-knowledge-store
npm --prefix web run check
npm --prefix web run build
```

最终验证还必须使用 daemon 主入口：

```text
http://127.0.0.1:38123/web.html
```

验收工作区切换、索引加载、空态、失败态、截断态、dangling 状态、缩放平移、节点详情、代码跳转、知识跳转、桌面 bridge、一致性和控制台错误。

自动发现审阅专项最终验收：

- 系统自动生成的关系默认是 `candidate`，并在局部聚焦图和待审阅列表中明确标识；
- 候选关系不得在未经确认时被当作 `active` 事实写入或注入 Agent 确定性上下文；
- 局部聚焦图支持知识、文件、符号三类焦点，深度和数量受服务端限制，超限显示“结果已截断”；
- 证据详情能展示关系来源、匹配内容或代码路径、符号信息、置信度、生成时间和状态；
- `确认` 会激活关系并保留审阅记录，`忽略` 会拒绝关系且不会在下一次刷新中立即重复，`修正` 会保存修正后的目标或关系类型并保留原始证据；
- 手动新增关系可以完成，但入口处于低频二级位置，不成为默认发现路径；
- 空态能说明“暂无待审阅关系”并提供返回或重新扫描入口；
- 失败态能说明自动发现或索引失败原因，并提供重试入口；
- 截断态能说明结果受限，并支持缩小范围或继续展开；
- dangling 关系明确显示失效原因，不可伪装为有效关联，并能进入修正流程；
- 重建、刷新、重启和 workspace 切换后，候选、确认、忽略、修正和 dangling 状态保持正确且不跨 workspace 泄漏。

## 7. 禁止事项

- 不把关系塞进 `source_ref` 或 `tags`；
- 不把代码依赖复制进 `KnowledgeState`；
- 不使用行号作为节点主 ID；
- 不默认返回完整工作区图；
- 不让 LLM 直接写入 `active` 推断关系；
- 不新增第二套索引监听或依赖图实现；
- 不以兼容分支、回退路径或临时开关掩盖协议问题。
