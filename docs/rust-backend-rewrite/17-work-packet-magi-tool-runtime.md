# Agent 任务单：magi-tool-runtime 内置工具执行内核

更新时间：2026-04-15

---

## 1. 任务名称

- 名称：`magi-tool-runtime` 内置工具执行内核任务单
- 编号：`WP-TOOL-001`
- 负责 Agent：Tool Agent

## 2. 写域

- 唯一写域：`crates/magi-tool-runtime`
- 禁止修改范围：
  - 现有 `src/**` 运行代码
  - MCP bridge
  - skill runtime
  - governance 决策层
- 依赖的上游文档：
  - [02-capability-matrix.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/02-capability-matrix.md)
  - [03-semantic-deviation-ledger.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/03-semantic-deviation-ledger.md)
  - [04-module-mapping-and-target-crates.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/04-module-mapping-and-target-crates.md)

## 3. 背景

- 当前能力域：builtin tools、文件/搜索/shell/process/diff 执行、tool context、并发写防护
- 当前实现位置：
  - `src/tools/tool-manager.ts`
  - `src/tools/file-executor.ts`
  - `src/tools/search-executor.ts`
  - `src/tools/remove-files-executor.ts`
  - `src/tools/shell/**`
- 当前问题：
  - ToolManager 混装 builtin / MCP / skill / host / policy
  - 工具注册、执行、权限边界过于集中

## 4. 根本原因

1. 当前工具系统是从单管理器持续扩展出来的
2. builtin tools 没有从扩展机制中独立出来
3. 如果不先拆 builtin tool runtime，Rust 执行主链会继续依赖“超级 ToolManager”

## 5. 目标

- 本任务要完成的 Rust 目标结构：
  - 建立 `magi-tool-runtime`
  - 收口 builtin tool registry 和核心执行器
  - 建立明确的 tool execution context
- 本任务不做什么：
  - 不管理 MCP transport
  - 不管理 instruction skill
  - 不承载审批决策
- 与其他 Agent 的边界：
  - governance 只给策略决策
  - skill / MCP 通过桥接或扩展层接入

## 6. 产出物

- 新增 crate / module：
  - `crates/magi-tool-runtime`
  - `registry`
  - `file_tools`
  - `search_tools`
  - `shell_runtime`
  - `process_runtime`
  - `execution_context`
- 新增 schema：
  - 若 tool schema 要对外冻结，先补 `schema/tool-protocol`
- 更新文档：
  - 回写 `D-006`
- 必须删除的旧实现或冗余结构：
  - 当前阶段不删主仓运行代码
  - 但禁止 Rust 侧继续复制 ToolManager 式全能对象

## 7. 语义约束

- 本任务涉及的真相源：
  - builtin tool registry
  - tool execution context
  - tool result semantics
- 是否涉及协议变化：
  - 是，tool schema / tool result 需要稳定定义
- 是否涉及语义偏差台账登记：
  - 是，必须对齐 `D-006`

## 8. 质量要求

必须遵循
[$cn-engineering-standard](/Users/xie/.codex/skills/cn-engineering-standard/SKILL.md)。

额外要求：

- 不允许在一个对象中同时持有 builtin/MCP/skill/host/policy 全部能力
- 并发写防护必须是 runtime 内建能力，不是外部补丁

## 9. 验收标准

- 编译：
  - `magi-tool-runtime` 可独立编译
- 最小运行验证：
  - file / search / shell / process / diff 五类工具最小链路可跑通
  - `ToolExecutionInput.input` 支持原始字符串与 JSON 输入约定，并能稳定解析
  - `shell.exec`、`process.inspect`、`diff.preview` 具备可观察的最小运行语义
- 协议验证：
  - tool schema 与 result 结构可被描述
- 清理验证：
  - builtin tools 与扩展层职责边界清晰

## 10. 输出结论

- 已完成内容：
  - 已建立 builtin tool registry、执行输入输出模型和治理联动入口
  - 已建立 `BuiltinTool` trait 和统一结果状态
  - 已补 `ToolExecutionPolicy`，可在治理前消费 skill runtime 输出的统一工具约束
  - 已补 builtin allow/deny 的真实执行语义，tool runtime 不再默认放行未授权请求
  - 已补 file.read、search.text、shell.exec、process.inspect、diff.preview 五类 builtin 的真实执行器骨架
  - 已补 `ToolExecutionInput.input` 的 JSON/raw 双输入约定，并让 builtin 输出保持统一结果语义
  - 已补 builtin access mode 区分与运行时并发写防护，shell.exec 对同一 `workspace_id` / `todo_id` / `cwd` / 路径 claim 可直接拒绝冲突写入，写冲突沿用统一 `Rejected + SandboxPolicy` 语义
  - 已作为 worker 模拟执行链中的 builtin 唯一执行入口，由 execution intent 的 builtin step 统一调用 `ToolRegistry.execute_with_policy(...)`
  - 已为 builtin 运行期补最小 `usage` 事件发射，usage 事件带有 `tool_name / status / risk_level / approval_requirement` 等最小可观测信息，并由事件总线统一纳入 usage 账本；当前 `usage` 事件与 `audit` 事件同路径发射，保证工具调用的审计与用量可同时回放
- 删除内容：
  - 无
- 未完成边界：
  - 尚未接入 MCP / host_bound 工具族
  - 尚未接入 custom binding 的真实 bridge 执行链
- 后续依赖：
  - `magi-governance`
  - `magi-orchestrator`
  - `magi-worker-runtime`
