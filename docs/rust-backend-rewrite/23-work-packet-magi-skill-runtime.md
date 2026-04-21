# Agent 任务单：magi-skill-runtime Skill 扩展运行层

更新时间：2026-04-16

---

## 1. 任务名称

- 名称：`magi-skill-runtime` Skill 扩展运行层任务单
- 编号：`WP-SKILL-001`
- 负责 Agent：Skill Agent

## 2. 写域

- 唯一写域：`crates/magi-skill-runtime`
- 禁止修改范围：
  - 现有 `src/**` 运行代码
  - builtin tool runtime
  - MCP bridge
- 依赖的上游文档：
  - [02-capability-matrix.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/02-capability-matrix.md)
  - [03-semantic-deviation-ledger.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/03-semantic-deviation-ledger.md)
  - [04-module-mapping-and-target-crates.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/04-module-mapping-and-target-crates.md)

## 3. 背景

- 当前能力域：instruction skill、自定义工具、skill prompt 注入、tool allowlist
- 当前实现位置：
  - `src/tools/skills-manager.ts`
  - `src/llm/adapter-factory.ts` 中 skill 相关装配
- 当前问题：
  - skill runtime 仍附着在工具和模型工厂上
  - skill / custom tool / prompt injection 边界不够独立

## 4. 根本原因

1. skill 能力是在工具和模型装配链中自然长出来的
2. 没有形成独立扩展层
3. 如果不单独收口，后续 Rust 后端会继续把 skill 逻辑散落在工具和模型调用之间

## 5. 目标

- 本任务要完成的 Rust 目标结构：
  - 建立 `magi-skill-runtime`
  - 收口 instruction skill、custom tool binding、prompt injection policy
- 本任务不做什么：
  - 不执行 builtin tools
  - 不建立 MCP transport
  - 不直接调用模型 provider
- 与其他 Agent 的边界：
  - skill runtime 是扩展层
  - builtin tool runtime、model bridge、MCP bridge 通过协议协作

## 6. 产出物

- 新增 crate / module：
  - `crates/magi-skill-runtime`
  - `registry`
  - `instruction_skills`
  - `custom_tools`
  - `policy`
- 新增 schema：
  - 若 skill metadata 需要冻结，先补 `schema/tool-protocol`
- 更新文档：
  - 回写 `D-006`
- 必须删除的旧实现或冗余结构：
  - 当前阶段不删主仓运行代码
  - 但禁止继续把 skill runtime 附着在 tool manager / adapter factory 上

## 7. 语义约束

- 本任务涉及的真相源：
  - skill metadata
  - custom tool binding metadata
  - prompt injection policy
- 是否涉及协议变化：
  - 可能影响 skill/tool 元数据 schema
- 是否涉及语义偏差台账登记：
  - 是，需对齐 `D-006`

## 8. 质量要求

必须遵循
[$cn-engineering-standard](/Users/xie/.codex/skills/cn-engineering-standard/SKILL.md)。

额外要求：

- skill runtime 必须作为扩展层独立存在
- 不得重新回到 ToolManager / AdapterFactory 混装结构

## 9. 验收标准

- 编译：
  - `magi-skill-runtime` 可独立编译
- 最小运行验证：
  - instruction skill / custom tool metadata 可加载与约束
- 协议验证：
  - skill 元数据与 tool allowlist 可稳定表达
- 清理验证：
  - crate 内无 builtin tool 执行与 provider 调用混装

## 10. 输出结论

- 已完成内容：
  - 已建立 skill registry、skill definition 和 allowlist 骨架
  - 已补 `SkillRuntime` 与 `SkillSelection`
  - 已补 `SkillPolicyDecision`，支持 allowlist / denylist 解析
  - 已补 prompt priority、custom tool binding 与运行时 resolve
  - 已补 `SkillToolRuntimePlan`，可向 `magi-tool-runtime` 输出统一 `ToolExecutionPolicy`
  - 已补 custom binding 的 `bridge_kind / dispatch_action / bridge_target` 与 `BridgeBindingDispatchPlan`
  - 已补 builtin 请求、bridge-bound 请求与 denied 请求的显式分流
  - 已补 `SkillDispatchRuntime`，统一 builtin / bridge 调度入口
  - 已补 `SkillDispatchObservation` 与标准化观测输出，可直接进入 worker 观测链
  - 已补 builtin / bridge 统一错误与结果语义，拒绝与失败可稳定区分；`SkillDispatchObservation` 现已显式携带 `error_kind / bridge_error_layer / bridge_error_message`，便于区分 routing、transport、protocol 与 remote business 边界
  - 已可作为 orchestrator / worker 模拟执行链中的唯一 skill 分流入口，execution intent 的 custom binding 与 builtin 请求均经由 `SkillDispatchRuntime.dispatch_observed(...)`
- 删除内容：
  - 无
- 未完成边界：
  - 已与 `magi-tool-runtime` 建立统一执行策略与调度装配
  - 已与 `magi-bridge-client` 建立可安全消费的计划级装配
  - 尚未接入真实 host / MCP 服务端与 provider 适配
- 后续依赖：
  - `magi-bridge-client`
