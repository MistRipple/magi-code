# 知识图谱设计与维护

本文描述现有知识图谱的事实来源、维护链路与验收边界。历史分阶段计划和验收记录通过 Git 历史追溯；本次是否通过以当前源码和实际运行结果为准。

## 事实来源

知识图谱连接工作区知识、代码文件和代码符号，不引入外部图数据库。代码依赖来自 magi-knowledge-store 的 DependencyGraph，图谱查询只聚合已有索引；显式关系及审阅结果由 KnowledgeState 持久化。

| 节点 | 稳定身份 |
| --- | --- |
| 工作区 | workspace:{workspace_id} |
| 文件 | file:{normalized_relative_path} |
| 符号 | symbol:{relative_path}:{qualified_name}:{kind} |
| 知识 | knowledge:{knowledge_id} |

文件和符号使用工作区相对路径，行号只作为证据，不作为身份。Project Memory、Session 和 Task 不进入主图谱。

派生关系包括工作区包含文件、文件包含符号及文件依赖文件，不复制到 knowledge.json。持久化关系包括知识到文件/符号的 applies_to、explains、references，以及知识之间的 related_to、supersedes、contradicts。

关系来源为 deterministic_code、explicit_user、explicit_agent、inferred；状态为 active、candidate、dangling、rejected。每条关系保留来源、状态、可选置信度和证据摘要。非法路径、非法置信度及跨工作区关系须拒绝。

## 候选与审阅

自动发现只生成 candidate。用户确认后进入 active，忽略后进入 rejected；修正目标时保留 discoveryKey 和原始证据，后续刷新不能覆盖已审阅结果。稳定身份用于去重，重复刷新不改写未变化关系的时间戳。

未审阅候选的匹配失效时清理候选；目标代码节点消失时图查询标为 dangling。已确认、已忽略和用户修正的关系保留审阅事实，不被自动刷新重新激活。

图谱 UI 默认展示待审阅关系，提供确认、忽略、修正和证据详情；手动新增关系保留在次级入口。局部图支持知识、文件、符号焦点、代码跳转和知识跳转。切换工作区时取消旧请求并清空旧投影，避免跨工作区显示。

## 查询与预算

| 入口 | 约束 |
| --- | --- |
| GET /api/knowledge/graph | 默认深度 1，最大深度 3；最多 120 节点、240 边；支持焦点、方向及节点/关系过滤 |
| GET /api/knowledge/relations | 默认和最大 1,000 条；返回 totalRelations、truncated |
| knowledge_graph_query | 只读 Agent 工具，focus 必填，默认深度 1、最大深度 2；节点、边、字符及估算 token 均有预算 |
| ContextRuntime::select_knowledge_on_demand | 在文本知识命中后查询局部图；正文与图摘要共享知识上下文预算 |

图查询稳定排序并返回索引状态、来源、证据及截断标志。截断时保留焦点，UI 显示截断事实；自动候选每个工作区最多保留 2,000 条。

上下文中确定关系与 candidate/inferred/dangling 线索分段表达，rejected 不进入 prompt。索引未就绪时图扩展不参与上下文，文本知识检索仍按自身语义工作。knowledge.context.selected 只记录选择统计，不写完整知识正文。

## 索引与持久化

watcher、30 秒周期对账、全量重建和按需索引共用 refresh_inferred_relations_for_workspace 的维护逻辑。周期对账重新扫描当前文件集合，补齐 watcher 丢失的新增文件，同时更新工作区的 CodeIndex 投影。空工作区保持可查询的空索引，不写入伪造知识记录。

候选或 CodeIndex 实际变化后，在释放状态锁后触发持久化回调。API 注册回调，由合并 dirty 标记的单写入 worker 落盘；其他入口不各自持久化。未注册回调的独立 Store 保持内存行为。

## 实现入口

- [节点与关系模型](../crates/magi-knowledge-store/src/graph.rs)、[图查询](../crates/magi-knowledge-store/src/graph_query.rs)和[Store/索引维护](../crates/magi-knowledge-store/src/lib.rs)。
- [知识 API](../crates/magi-api/src/routes/knowledge.rs)与[ContextRuntime](../crates/magi-context-runtime/src/lib.rs)。
- [知识面板](../web/src/components/KnowledgePanel.svelte)、[图谱面板](../web/src/components/KnowledgeGraphPanel.svelte)与[Web API](../web/src/web/agent-api.ts)。

## 验证

按改动位置选择检查；文档编辑本身不要求启动服务或全量测试。

```bash
cargo test -p magi-knowledge-store
cargo check -p magi-api
npm --prefix web run check
```

真实 UI 验收从 daemon 主入口 http://127.0.0.1:38123/web.html 进入，覆盖工作区切换、索引空态/失败/加载、截断、dangling、证据、确认/忽略/修正、重建与重启一致性。持久化、工具查询或上下文扩展改变时，再验证各自的回归路径。

不把关系塞进 source_ref 或 tags，不复制依赖图、不增加第二套 watcher、不让模型直接激活推断关系，也不把历史通过记录当作当前验收结果。
