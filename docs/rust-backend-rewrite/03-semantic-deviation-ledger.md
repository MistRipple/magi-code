# Rust 后端重构语义偏差台账

更新时间：2026-04-15

> 本文档用于记录“旧实现中不合理、混乱、臃肿或错误的语义”与“Rust 新后端目标语义”的偏差关系。
>
> 该台账的目的不是描述代码风格问题，而是管理重构期间的语义取舍，防止把历史包袱误当成迁移标准。

---

## 1. 使用规则

当满足以下任一条件时，必须登记到本台账：

- 旧实现语义不一致
- 同一状态存在多个真相源
- 旧行为明显属于历史补丁结果
- 旧结构职责混杂，无法直接继承
- 切换协议时，不确定应以旧实现还是新语义为准

---

## 2. 决策优先级

发生语义冲突时，优先级如下：

1. 目标领域模型
2. 稳定对外契约
3. 旧代码实现细节

说明：

- 内部实现以目标模型为主
- 对外协议若已稳定，应单独评估兼容方案
- 明确错误的旧行为不视为契约

---

## 3. 首批高风险偏差条目

| 偏差编号 | 能力域 | 旧实现现象 | 不合理原因 | Rust 目标语义 | 是否影响外部协议 | 决策状态 |
|---|---|---|---|---|---|---|
| D-001 | Host / Core Runtime 边界 | `src/host/runtime-host.ts` 直接依赖 `vscode`，并把 diagnostics、LSP、terminal、git 等宿主能力注入 `RuntimeHostContext`，后端运行时对 IDE SDK 有直接感知 | 核心运行时被宿主污染，后端无法做到 IDE 无关；后续支持 IDEA 时会把宿主差异继续带进 core | Rust Core Runtime 完全不直接依赖任何 IDE SDK；宿主能力只能通过 Host Bridge 注入 | 是，影响 Host Bridge 协议 | 目标语义已确认 |
| D-002 | Agent API / Runtime 责任边界 | `LocalAgentService` 同时承担 HTTP 路由、workspace registry、knowledge API、文件浏览、隧道状态、session bootstrap；`AgentWorkspaceRuntime` 同时承担 runtime 编排、事件绑定、读模型输出、UI bridge 消息投递 | API 层、运行时层、读模型层、桥接层职责混装，导致 API 进程与业务状态难拆 | 拆为 `daemon -> api -> session/workspace/runtime services`；API 只做入口，业务状态由专属 store/service 持有 | 部分影响，优先保持现有 API 形状，后续再统一升级 | 目标语义已确认 |
| D-003 | Orchestrator / Dispatch 超级调度器 | `MissionDrivenEngine`、`DispatchManager` 同时承载请求分类、计划控制、治理评估、恢复、派发、汇总、runtime 控制面等多重职责，形成巨型状态对象 | 边界模糊、状态机隐式、可替换性差，任何变更都容易牵动整条主链 | 拆成显式的 `plan / dispatch / governance / runtime control plane / summary` 子模块，用强类型状态机表达 | 否，优先内部重构 | 目标语义已确认 |
| D-004 | Session 聚合与投影混装 | `UnifiedSession` 同时保存 `messages`、`timeline`、`notifications`、`snapshots`、`timelineProjection`，并以 `executionChains?: unknown`、`resumeSnapshots?: unknown` 透传运行态 sidecar | durable state、派生读模型、运行时 sidecar 混在一个聚合里，且部分字段是 `unknown`，难以维持长期稳定 | Rust 侧拆成：`session aggregate`、`timeline/notification store`、`projection read model`、`execution/recovery stores`，全部强类型化 | 部分影响，影响 bootstrap / projection 结构 | 目标语义已确认 |
| D-005 | 后端运行时与前端投影链耦合 | `AgentWorkspaceRuntime` 直接依赖 `EventBindingService`、`ClientBridgeMessage`、`session bootstrap projection` 等 UI 相关结构，后端读模型直接面向当前前端表达层 | 读模型与传输层、前端消费形状耦合，未来更换前端或宿主时易被当前 UI 反向约束 | 后端只输出领域事件和稳定 read model；UI/宿主通过 API/SSE 消费，不再反向定义后端结构 | 是，影响 SSE / read model schema | 目标语义已确认 |
| D-006 | Tool / MCP / Skill / Host 能力混装 | `ToolManager` 统一持有 builtin executors、MCP executors、skill executor、host capabilities、snapshot、safeguard、tool policy、sandbox policy | 工具注册、执行、权限、扩展、宿主能力全部混在一个运行时对象里，难以按能力域拆分 | Rust 侧拆为 `builtin tool runtime`、`skill runtime`、`MCP bridge`、`governance/policy`、`host bridge client`，只通过协议拼装 | 部分影响，影响 tool protocol 和 tool schema 枚举 | 目标语义已确认 |
| D-007 | LLM 运行时与执行环境混装 | `LLMAdapterFactory` 同时持有 `ToolManager`、`SkillsManager`、`MCPToolExecutor`、`MessageHub`、`RuntimeHookManager`、`RuntimeHostContext`、session memory provider 等 | 模型调用、工具注册、宿主上下文、运行时 hook、memory 注入混杂在同一工厂，后续难以抽成清晰 bridge | Rust 侧将模型调用收口到 `model bridge`，core 只持有 `bridge client` 与领域接口，不再直接持有工具/宿主/技能实现 | 部分影响，优先不改变外部模型调用语义 | 目标语义已确认 |
| D-008 | Context / Memory / Knowledge 责任交织 | `ContextManager` 同时初始化 `SharedContextPool`、`FileSummaryCache`、`LayeredMemoryStore`、`ContextAssembler`，并直接依赖 PKB 与 session memory；`ProjectKnowledgeBase` 同时做索引、搜索、存储、查询、LLM 提取 | context runtime、memory store、knowledge store 三层职责未彻底拆开，长期会相互拖拽 | Rust 侧拆成 `knowledge store`、`memory store`、`context runtime` 三个独立边界，仅通过只读接口协作 | 部分影响，影响 knowledge API 与 context 组装输入 | 目标语义已确认 |
| D-009 | Knowledge Store 内部职责过重 | `ProjectKnowledgeBase` 同时承担文件索引、ADR/FAQ/learning 存储、项目上下文生成、本地搜索、容量淘汰等职责 | 知识索引、存储、查询、抽取与输出格式耦合过重，不利于后续独立演进或替换索引策略 | Rust 侧拆为 `indexer`、`knowledge store`、`query service`、`governed output`，分别建模 | 部分影响，影响 knowledge query 与审计输出 | 目标语义已确认 |
| D-010 | Event / Audit / Usage 主链分散 | `globalEventBus`、`UsageAuthority`、`RuntimeRolloutRecorder`、`EvidenceLedger` 分散在不同模块；部分事件既承担前端通知又承担审计 | 审计、观测、统计、前端通知未形成单一事件主链，回放和切换时难建立统一事件模型 | Rust 侧统一收口到 `magi-event-bus`，区分 domain event、audit event、usage ledger、UI projection event | 是，影响事件模型与 SSE 事件 schema | 目标语义已确认 |

---

## 4. 单条记录模板

每条偏差分析建议使用以下结构：

### D-XXX：偏差标题

- 能力域：
- 旧实现位置：
- 旧实现现象：
- 为什么不合理：
- 根本原因：
- Rust 目标语义：
- 是否影响 API / SSE / Host Bridge：
- 最终应以哪边为准：
- 后续处理动作：

---

## 5. 当前要求

在 Rust 重构过程中，若发现原有实现不合理：

1. 不得直接照搬
2. 不得先复制、后遗忘
3. 必须先进入本台账
4. 再决定是否需要同步调整能力对照表、crate 映射或阶段门槛

---

## 6. 首批优先处理偏差

按当前风险排序，建议最优先处理：

1. `D-001` Host 与 Core Runtime 边界
2. `D-002` Agent API / Runtime 责任边界
3. `D-003` Orchestrator / Dispatch 超级调度器
4. `D-004` Session 聚合与投影混装
5. `D-006` Tool / MCP / Skill / Host 能力混装
6. `D-008` Context / Memory / Knowledge 责任交织
7. `D-010` Event / Audit / Usage 主链分散

这些偏差不先收口，后续 Rust 后端即使写出来，也很容易只是“旧复杂度的语言迁移版”。

---

## 7. 当前结论

本台账是后续“并行重构但统一切换”模式下的关键治理工具。

没有这份台账，后续极易出现：

- 旧实现继续主导新设计
- 新语义无法落地
- 最终切换时对齐标准失控

当前首批条目已经足以支撑下一步 crate 设计与任务拆分。
