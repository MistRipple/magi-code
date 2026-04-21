# Agent 任务单：magi-bridge-client 外部桥接客户端层

更新时间：2026-04-16

---

## 1. 任务名称

- 名称：`magi-bridge-client` 外部桥接客户端层任务单
- 编号：`WP-BRIDGE-001`
- 负责 Agent：Bridge Agent

## 2. 写域

- 唯一写域：`crates/magi-bridge-client`
- 禁止修改范围：
  - 现有 `src/**` 运行代码
  - Host 壳实现
  - model bridge 服务端
  - MCP bridge 服务端
- 依赖的上游文档：
  - [02-capability-matrix.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/02-capability-matrix.md)
  - [03-semantic-deviation-ledger.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/03-semantic-deviation-ledger.md)
  - [04-module-mapping-and-target-crates.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/04-module-mapping-and-target-crates.md)

## 3. 背景

- 当前能力域：Rust Core 与 Host / Model / MCP 边界交互客户端
- 当前实现位置：
  - 当前已建立 Rust 侧桥接客户端边界与 model loopback server 验证入口
  - 旧系统中对应语义散落在 `src/host/**`、`src/llm/**`、`src/tools/mcp-*`
- 当前问题：
  - 若没有统一 bridge client，后续各 crate 会直接耦合外部桥
  - 容易重新把宿主、模型、MCP 差异带回 core

## 4. 根本原因

1. 旧系统没有“core runtime 对外桥接”这一层
2. 模型、MCP、宿主能力都直接嵌在 Node 运行时里
3. 如果不单独建立 bridge client，Rust Core 仍会被外部生态污染

## 5. 目标

- 本任务要完成的 Rust 目标结构：
  - 建立 `magi-bridge-client`
  - 定义 host bridge client、model bridge client、mcp bridge client 的统一模式
  - 补齐本地 JSON-RPC over stdio 或等价本地进程协议的最小 transport client，并提供 model bridge loopback server 验证回环
- 本任务不做什么：
  - 不实现具体 bridge 服务端
  - 不承载业务真相源
- 与其他 Agent 的边界：
  - 其他 runtime/store crate 只能通过 bridge client 访问外部桥

## 6. 产出物

- 新增 crate / module：
  - `crates/magi-bridge-client`
  - `JsonRpcStdioTransport`
  - `model_bridge_loopback`
  - `host_bridge_loopback`
  - `mcp_bridge_loopback`
  - `vscode_host_shell`
  - `mcp_manager_server`
  - `host_client`
  - `model_client`
  - `mcp_client`
  - `errors`
- 新增 schema：
  - 若桥接协议需要冻结，先补 `schema/host-bridge` 与相关协议定义
- 更新文档：
  - 回写 `D-001`、`D-007`
- 必须删除的旧实现或冗余结构：
  - 当前阶段不删主仓运行代码
  - 但禁止 Rust runtime crates 各自直接定义桥接方式

## 7. 语义约束

- 本任务涉及的真相源：
  - 无，bridge client 不是业务真相源
- 是否涉及协议变化：
  - 是，桥接协议必须统一冻结
- 是否涉及语义偏差台账登记：
  - 是，需对齐 `D-001`、`D-007`

## 8. 质量要求

必须遵循
[$cn-engineering-standard](/Users/xie/.codex/skills/cn-engineering-standard/SKILL.md)。

额外要求：

- Rust Core 不得绕过 bridge client 直接访问外部桥
- host / model / MCP 三类桥接必须有统一错误边界

## 9. 验收标准

- 编译：
  - `magi-bridge-client` 可独立编译
- 最小运行验证：
  - 三类桥接 client 结构可建立
- 协议验证：
  - host / model / MCP client 的协议输入输出可描述
- 清理验证：
  - crate 内无业务状态与宿主 SDK 混装

## 10. 输出结论

- 已完成内容：
  - 已建立 host / model / MCP 三类 bridge client trait
  - 已建立 `HostBridgeCommand`、`BridgeResponse`、统一错误边界
  - 已补 `BridgeBindingKind / BridgeBindingReference / BridgeBindingDispatchPlan`
  - 已补 `BridgeDispatchAction / BridgeDispatchInput / BridgeDispatchRuntime`
  - 已补 bridge target 显式校验与 incompatible binding/action 拒绝
- 已补本地 JSON-RPC over stdio 的最小 transport client，并可由 `BridgeDispatchRuntime` 真正消费 dispatch plan
- 已补 model / host / MCP 三类 loopback server，其中：
  - model 仅支持 `model.invoke`
  - host 已统一覆盖 `WorkspaceRoots` / `OpenFile` / `RevealDiff` / `ReadDiagnostics` / `ReadSymbols` / `TerminalExec`
  - host 进一步区分了 `VSCode real-prehost` 与 `IDEA boundary-placeholder`，并在 `describe_services` / payload 中稳定暴露 `implementation_source / capability_profile / workspace_roots_source`
  - MCP 已从单工具 registry 推进到最小 manager 语义，至少覆盖：
    - `shadow-mcp-manager`
    - `shadow-mcp`
    - `shadow-mcp-observability`
    - `echo.inspect`
    - `echo.describe`
  - MCP 进一步稳定暴露 `manager_version / registry_profile / registry_manifest / selection_strategy / default_server / default_server_health / default_server_selection_key / selection_targets / server_manifest / capability_profile / selection_key`
  - MCP manager 现已进一步补齐 `service_health / service_health_reason / default_route_status / default_route_target`，并支持通过环境变量驱动最小 registry 配置：
    - `MAGI_MCP_MANAGER_DEFAULT_SERVER`
    - `MAGI_MCP_MANAGER_ENABLED_SERVERS`
    - `MAGI_MCP_MANAGER_DISABLED_SERVERS`
    - `MAGI_MCP_MANAGER_SERVER_HEALTHS`
  - `mcp.call_tool` 已支持显式选择、selection key 路由、default server fallback，以及“无可用默认 server”时的显式远端业务错误
- 已补统一本地进程协议层：
  - 固定 `server_kind`
  - 固定 `bridge.handshake`
  - 固定 `bridge.health`
  - 固定 `bridge.describe_services`
  - 三类 loopback 复用同一套 JSON-RPC request/response 骨架与错误面
  - 本地进程协议现已从“单业务方法”扩成“多业务方法”调度，同时保留原有单方法兼容入口
- 已补 `JsonRpcBridgeServerProbeClient`，可在不改顶层 bridge contract 的前提下探测 loopback server 的握手、健康状态与服务目录
- 已补最小 service shim / service catalog：
  - `model` 以 provider 级 shim 暴露最小能力描述
  - `host` 以 `vscode / idea` 两类 shadow host shim 暴露命令能力，并把 OpenFile / RevealDiff / ReadDiagnostics / ReadSymbols 纳入统一描述
  - `host` 进一步补出 `shell_manifest / session_descriptor / workspace_context`，让 `describe_services` 和 host payload 都能稳定表达宿主壳标识、最小版本/能力版本与 workspace/session 语义
  - `host` 进一步补出 `shell_profile / command_capability_profiles / context_resolution_boundary`，把宿主壳 profile、命令能力 profile 与 session/workspace 上下文解析边界前置到 `describe_services` 与 `host.call` payload
  - `host` 进一步把 `VSCode` 标记为 `real-prehost`，`IDEA` 标记为 `boundary-placeholder`，并通过 `workspace_roots_source` 与 `capability_profile` 明确区分
  - `MCP` 以 manager + server descriptor 暴露最小 registry 语义，并把 server enabled / health / tool count / tool list 纳入统一描述
  - `MCP` 进一步补出 `manager_version / registry_profile / registry_manifest / selection_strategy / default_server / default_server_health / default_server_selection_key / selection_targets`，以及 `server_version / server_manifest / capability_profile / selection_key`
  - `MCP manager` 当前还会显式导出 `service_health / service_health_reason / default_route_status / default_route_target`，把 registry 健康态与默认路由可用性收成稳定目录语义
  - `MCP` 的 `mcp.call_tool` 继续复用现有 `server_name` 字段，但已支持 manager-side `selection_key` 解析与 blank selection 的 default server fallback
  - `MCP` 的 server descriptor 已补 `command_capability_profiles / context_resolution_boundary`，把最小 capability profile 和 registry 选择边界显式化
  - `MCP manager` 现已能真实处理 `mcp.list_servers / mcp.describe_server / mcp.enable_server / mcp.disable_server / mcp.register_server / mcp.start_server / mcp.stop_server / mcp.deregister_server / mcp.update_health`
  - 上述 lifecycle / registry 方法的返回值会显式带 manager/server descriptor 与 lifecycle event 信息，便于上层直接消费
- 已补 host / model / MCP 三类 client 的端到端回环测试
- 已补 host shell manifest 与 workspace/session context 相关测试：
  - `describe_services` 能看到 `shell_id / minimum_version / capability_version`
  - host payload 能稳定表达 `host_session / workspace_context`
  - `describe_services` 与 payload 能稳定表达 `implementation_source / capability_profile / workspace_roots_source`
- 已补 host shell profile / command capability profile / context resolution 相关测试：
  - `describe_services` 能看到 `shell_profile`
  - `describe_services` 能看到 `command_capability_profiles`
  - host payload 能稳定表达 `host_shell_profile / host_command_capability_profile / context_resolution_boundary`
- 已把 `VSCode real-prehost` 从纯 payload 回环推进到最小真实前置实现：
  - `WorkspaceRoots` 基于当前工作目录返回真实 roots
  - `OpenFile / RevealDiff / ReadDiagnostics / ReadSymbols` 基于本地文件系统与静态扫描返回真实前置结果
  - 上述文件类命令当前都已收口到 workspace roots 边界内，不再接受越过 workspace roots 的任意绝对路径
  - `VSCode prehost` 现在已支持通过 `MAGI_VSCODE_PREHOST_WORKSPACE_ROOTS` 显式指定 workspace roots，并把 roots 解析结果、service health、runtime mode、terminal exec mode 收进统一协议语义
  - 当显式配置的 workspace roots 全部无效时，`VSCode prehost` 不再静默回退到 `current_dir`，而是显式进入 `service_health=unavailable`
  - `IDEA` 现已从“假装可执行的 shadow shim”收口为显式未实现边界：`service_health=unavailable`、`runtime_mode=boundary-only`，所有 `host.call` 都统一返回远端业务错误
  - `TerminalExec` 默认仍在 `VSCode prehost` 路径上显式返回 remote business error
  - 但在显式开启 `allowlisted` 模式且命中允许命令时，`TerminalExec` 现已可执行受控本地命令
  - `TerminalExec` 只允许：
    - 明确开启的 `MAGI_VSCODE_PREHOST_TERMINAL_MODE=allowlisted`
    - 命中 `MAGI_VSCODE_PREHOST_ALLOWED_COMMANDS`
    - `working_directory` 落在当前 workspace roots 内
  - 超出 allowlist、空命令或越过 workspace roots 的请求都会继续返回显式 remote business error
- 已补 transport / protocol / remote business 三层错误边界，标准 JSON-RPC 协议错误码已显式分层，且可由 skill dispatch 观测层显式保留
- 已补 host 新命令回环测试：
  - `OpenFile`
  - `RevealDiff`
  - `ReadDiagnostics`
  - `ReadSymbols`
- 已补 MCP registry / 多 server / enable-disable / 多工具回环测试：
  - `shadow-mcp-manager`
  - `shadow-mcp`
  - `shadow-mcp-observability`
  - `echo.inspect`
  - `echo.describe`
- 已补 MCP manager/server 元信息与 selection key 回环测试：
  - `describe_services` 能看到 `manager_version / registry_profile / registry_manifest / selection_strategy / default_server / default_server_health / default_server_selection_key / selection_targets`
  - server descriptor 能看到 `server_version / server_manifest / capability_profile / selection_key`
  - `mcp.call_tool` 已可通过 `selection_key` 路由到 canonical server
  - `mcp.call_tool` 已支持 blank selection 回落到 default server
- 删除内容：
  - 无
- 未完成边界：
- 已可安全消费 dispatch plan 并构造 host/model/MCP 请求
- 已形成更稳定的本地进程协议、服务目录与 shim 边界，并将 `VSCode` 宿主壳推进到 `real-prehost` 前置形态、将 MCP 推进到带 `manager/default fallback` 的前置语义
- 尚未接入真实 VSCode 扩展壳、真实 IDEA 宿主、真实 MCP 外部生态
- host 目前已到“宿主壳 profile / 命令能力 / 上下文解析边界前置”的 shadow/real-prehost 混合形态，但还没有真实编辑器 UI 接线
- host 目前还没有真实编辑器 UI 接线；当前 `VSCode real-prehost` 仅推进到“workspace roots 可配置、service health 可判定、roots 无效时显式 unavailable”的前置宿主壳形态
- MCP 目前已到“manager 前置一层 + env-configurable registry + default server fallback + registry manifest / selection target / default route readiness 稳定暴露”的 shadow 形态；默认 server 配错、enable/disable 指向未知 server、health override 非法时，也已能显式进入 `degraded / unavailable`，并且 lifecycle / registry JSON-RPC 已可真实调用，但还没有真实外部 server 生命周期管理、注册发现与长驻 manager
- 尚未实现长驻 server、多请求复用与真实宿主/provider 生命周期管理
- 后续依赖：
  - `magi-api`
  - `magi-tool-runtime`
  - `magi-orchestrator`
  - `magi-worker-runtime`
