# Magi MCP 服务 · 完整开发计划（交接版）

> 状态：**开发计划**。本文把《[Magi MCP 服务设计](./magi-mcp-server-design.md)》落成可执行的工作包，供后续 agent 直接接手。设计结论、固定规则（M1–M12）与安全边界以设计文档为准；本文只写：当前进度、从代码核对得到的事实、工作包、依赖顺序、验收与安全门槛、待用户拍板项。
> 更新日期：2026-09-30。
> 读法：先读设计文档 §0–§1、§14 和本文 §1–§3，再按 §5 的顺序领取工作包。

---

## 1. 接手指引

| 项 | 内容 |
| --- | --- |
| 分支与工作树 | 分支 `feature/magi-mcp`，工作树 `/Users/xie/code/magi-mcp`（与 GPT Web 线的主工作区 `/Users/xie/code/magi-rust-rewrite` 分开）。不要在主工作区改 MCP 文件。 |
| 基线 | 从 GPT Web 线快照 `e78799e7` 派生；MCP 提交为 `b02b5098`（核心 crate）、`0eac0ac9`（文档对齐）。 |
| 项目规则 | 先读根 `AGENTS.md` 与 `web/AGENTS.md`：中文沟通；先确定行为、状态、持久化与权限分别由谁拥有，沿现有入口做最小完整修改；不复制事实源；只跑与改动相关的检查，未运行的验证如实说明。 |
| 并行约定 | 与 GPT Web 线的文件归属、`harness/` 冻结、接口边界与合并顺序见设计文档 §16。 |
| 每个工作包的检查 | Rust：`cargo check --workspace --tests`、`cargo test -p magi-mcp-server` 及被改 crate 的测试；前端：`npm --prefix web run check` 与对应 `test:*`；涉及协议生成物时 `npm run protocol:check`；提交前 `cargo fmt` 与 `cargo clippy -p <crate> --tests`。 |
| 提交规范 | 小提交、一件事一个提交；提交信息写清“为什么”；涉及共用热点文件（`state.rs`、`routes/mod.rs`、`settings-store`、会话模型）的提交保持最小，先合先赢。 |
| 安全门槛 | 网络模式（WP11）与 OAuth（WP12）交付前必须完成 WP15 的安全评审；开放任何写入 / 执行工具前必须通过 §7 的威胁演练用例。 |

---

## 2. 当前进度

### 2.1 已完成：`crates/magi-mcp-server`（35 个测试通过）

纯逻辑核心，**不依赖**工具运行时、审批注册表与会话模型，由宿主通过 trait 注入。

| 模块 | 内容 | 对外 API 要点 |
| --- | --- | --- |
| `protocol.rs` | JSON-RPC / MCP 消息形状；协议版本 `2025-06-18` | `JsonRpcRequest`、`success` / `error`、`tool_success` / `tool_failure`、`initialize_result` |
| `token.rs` | 令牌生成（256 位随机）、哈希存储、恒定时间比较、过期 / 吊销 / 工作区移除、统一认证失败 | `TokenStore::{issue, authenticate, revoke, revoke_all, revoke_workspace, records, from_records}`；`TokenRecord`（可序列化，不含原文）；`AttributionMode::{External, FollowWebSlot}` |
| `profile.rs` | 权限档 `Profile::{ReadOnly, Edit, EditTrusted, Exec}` 与决策表 | `Profile::decide(ToolClass) -> Disposition::{Deny, Auto, RequireApproval}` |
| `catalog.rs` | 公开名与内部工具名映射（第一批 11 个）、按权限档生成目录、MCP 注解 | `V1_TOOLS`、`build_catalog`、`ToolSchemaProvider`（宿主提供说明与 schema） |
| `path_guard.rs` | 工作区路径守卫：规范化、拒绝 `..` 与符号链接逃逸，拒绝 `.magi` / `.ssh` / `.gnupg`，`.git` 只读 | `PathGuard::{new, check, check_all}`、`PathRequest` |
| `server.rs` | 调用管线：目录 → 归属 → 路径 → 审批处置 → 后端执行 → 审计；参数与结果体积上限 | `McpServer::handle`；宿主接口 `ToolBackend`、`AttributionResolver`、`WorkspaceResolver`、`AuditSink` |
| `http.rs` | Streamable HTTP：Host → Origin → 认证 → 协议版本 → 解析；只提供 `POST /mcp` | `router(HttpState)`、`HttpConfig { allowed_hosts, allowed_origins }` |

**第一批工具（已核对存在于工具运行时）：** `magi.fs.read` / `write` / `patch` / `apply_patch` / `mkdir` / `move` / `copy` / `remove`、`magi.search.text` / `semantic`、`magi.shell.exec`。`magi.git.*` 与 `magi.changes.*` **尚未核对**，不在目录里（WP9）。

### 2.2 未完成（按工作包，见 §4）
宿主适配（外部工具执行入口与审批）、外部工具会话、daemon 集成与令牌持久化、stdio 中继、设置页、跨会话待审批入口、审计展示、真实客户端验收、网络模式、OAuth、GPT Web 接入、安全评审。

---

## 3. 从代码核对得到的事实（实现依据）

以下事实来自对现有代码的阅读，实现时仍以当前源码为准（函数名比行号稳定）。

1. **工具执行可以脱离模型 turn。** `magi-tool-runtime::ToolRegistry::execute_with_policy(input, context, policy)` 只需要 `ToolExecutionContext`（会话、工作区、工作目录、访问档）与 `ToolExecutionPolicy`（允许 / 拒绝路径、工具名、命令模式），不依赖 Task 或 turn。
2. **但“是否允许执行”的判定在 conversation-runtime，且以 Task 的 `policy_snapshot` 为前提。** `tool_batch.rs` 中 `execute_task_tool_call` 依次做：可见性检查 → SafetyGate 评估与审计发布 → `task_tool_preflight_decision`（`access_profile_tool_decision` + SafetyGate，`HardBlock` / `Rejected` 压过 `NeedsApproval`）→ `await_task_tool_approval` → 执行 → 写保护与账本。这些函数是私有或 `pub(crate)`。MCP 必须复用同一套判定，而不是自己再写一份，因此需要在 conversation-runtime 里新增一个**公开的外部调用入口**（WP1）。
3. **已有访问档：** `magi_core::AccessProfile::{ReadOnly, Restricted, FullAccess}` 与 `TaskPolicy { access_profile, allowed_paths, denied_paths, read_only_paths, command_mode, allowed_tools, denied_tools, … }`。MCP 权限档到它的映射见 WP1。
4. **审批注册表可复用，等待循环要新写。** `ToolApprovalRegistry::{request_with_arguments, resolve, cancel, pending_for_session, remove_*}` 把 `task_id` 与 `turn_id` 当不透明字符串使用，可用合成标识；`await_task_tool_approval` 私有且强依赖“会话当前活动 turn + Task 仍在运行”。审批决策枚举含 `AllowOnce` / `AllowForTurn` / `Deny`；有 5 分钟 TTL（`TOOL_APPROVAL_TTL_MILLIS`）。
5. **审批在界面里是会话作用域的。** `GET /session/tool-approvals?sessionId=…` 与 `resolve_session_tool_approval` 按会话；事件 `tool.approval.requested` 由 `web-client-bridge.ts` 消费。外部工具会话不是用户当前打开的会话，因此需要跨会话入口（WP8）。
6. **`SessionRecord` 没有“类型”字段**（`magi-session-store/src/models.rs`：id、标题、状态、时间、`message_count`、`workspace_id`、已读时间）。外部工具会话需要一种可区分的标记（WP3）。
7. **变更账本以会话为单位：** `ApiState::snapshot_session` 与 `synchronize_session_changes(session_id, workspace_id, workspace_root, force)`；待处理变更的批准 / 回退走现有接口。外部写入能否记录到外部会话的账本**待验证**（WP3、WP9）。
8. **工具说明与 schema 可直接从工具运行时取：** `BuiltinToolName::{from_name, description, parameters_schema, is_public_tool_surface}`；`builtin_tool_schema.rs` 中 `public_builtin_tool_definition` 是现成参考。`ToolRegistry::builtin_access_mode` 给出只读 / 可能写 / 显式写。
9. **路径声明可复用：** `magi_tool_runtime::tool_path_access_requests(canonical_tool_name, arguments_json, workspace_root, access_profile)` 返回 `ToolPathAccessRequest { absolute_path, kind }`；`apply_patch` 通过 `apply_patch_declared_paths_from_input` 提取补丁里的路径。宿主的 `ToolBackend::path_requests` 应包装它，crate 的 `PathGuard` 作为纵深防御与黑名单。
10. **本地 socket 中继代码已存在：** `magi-web-model/src/harness/stdio.rs`（`local_endpoint_for(state_root)`、`StdioRelay`、`serve_local_socket`，含 Unix domain socket 与 Windows 命名管道实现）。应上移复用，不要重写。
11. **隧道托管：** `magi-api/src/tunnel.rs` 的 `TunnelManager`（cloudflared 检测 / 安装 / 启动 / 监控，当前用于 Magi 自己的“远程访问”）。网络模式复用其托管能力，**入口规则独立**（设计 M10）。
12. **设置存储：** `magi-settings-store::SettingsStore::{get_section, set_section, upsert_array_entry, …}`；设置页 Tab 定义在 `web/src/lib/settings-tabs.ts`（`SETTINGS_TABS`）。
13. **路由组织：** `magi-api/src/routes/mod.rs` 按功能拆模块（`sessions`、`settings`、`tools`、`browser`、`mcp_skills_repos` 等）；MCP 服务端的管理路由应新建模块，使用新前缀。

---

## 4. 工作包

每个工作包包含：目标、主要改动、依赖、验收。规模为粗估（人日，含测试）。

### WP1 外部工具执行入口（conversation-runtime）　〔M，4–6〕
**目标：** 提供一个公开入口，让外部调用走与 Magi 自己 agent 完全相同的判定与执行链，且不需要模型 turn。
**改动：**
- 在 `magi-conversation-runtime` 新增模块（例如 `external_tool.rs`），放在能访问 `tool_batch.rs` 私有函数的位置，导出 `ExternalToolExecutor` 与 `execute_external_tool_call(ctx, call) -> ExternalToolResult`。
- 为每个外部工具会话构造一份**合成 `TaskPolicy`**：`allowed_paths` 为该工作区根，`denied_paths` 含 `.magi` 等；`command_mode` 与 `allowed_tools` 由权限档决定；**MCP 权限档映射**：`ReadOnly → AccessProfile::ReadOnly`；`Edit`、`EditTrusted → Restricted`；`Exec → Restricted`（shell 仍受 Magi 的命令风险判定与 SafetyGate 约束，绝不映射到 `FullAccess`）。
- 复用 `task_tool_preflight_decision` 相同的判定（`access_profile_tool_decision` + SafetyGate），并保持 `HardBlock` / `Rejected` 压过任何审批的语义。
- **审批合并：** MCP 层已经做过人工确认的调用，不得再触发第二次 Magi 层 `NeedsApproval`；但 Magi 层 `HardBlock` / `Rejected` 仍然拒绝（相当于 `approval_granted = true` 的路径）。
- 执行沿用 `ToolRegistry::execute_with_policy_and_progress`，并接入写保护、审计事件发布。
**依赖：** 无（可最先做）。
**验收：** 单元测试覆盖：只读放行、`ReadOnly` 拒绝写、写入需审批决策、路径越界拒绝、SafetyGate 命中拒绝、审批合并不出现双重提示；不引入对 Task / turn 的依赖。

### WP2 外部审批（等待循环与注册表用法）　〔M，3–4〕
**目标：** 外部调用的人工确认，复用 `ToolApprovalRegistry` 与现有事件，不复用会话 turn 的存活判据。
**改动：**
- 新的等待循环 `await_external_tool_approval`：`PendingToolApproval` 使用合成 `task_id` / `turn_id`（形如 `external:<token_id>`），存活判据改为“令牌仍有效且连接未断”；只支持 `AllowOnce` / `Deny`，不使用 `AllowForTurn`。
- 发布与 Magi 现有一致的 `tool.approval.requested` 事件，并在 payload 里带**来源客户端名称与令牌前缀**。
- 挂起时限默认 90 秒（可配置）；超时或拒绝在同一个 `tools/call` 返回工具错误，且**不在稍后补执行**。
- 令牌被吊销、连接断开时，取消所有该令牌的待审批。
**依赖：** WP1。
**验收：** 测试覆盖：允许 / 拒绝 / 超时 / 吊销时取消 / 重复请求；`remove` 与 `shell.exec` 永远逐次确认。

### WP3 外部工具会话与变更账本　〔M–L，4–6〕
**目标：** 每个 `external` 令牌对应一个由 Magi 管理的会话，承载调用、审批与变更。
**改动：**
- 在 `magi-session-store` 为会话增加**类型标记**（例如 `kind: user | external_tool`，缺省为 `user`，不影响既有数据），并在创建、列表、删除、归档路径上区分；用户不能在外部工具会话里发起模型对话。
- 令牌创建时惰性建立会话（标题体现客户端名称与工作区），令牌吊销后会话保留用于审计并可归档。
- **阶段内先验证：** 外部写入是否被该会话的 `SnapshotSession` 记录为待处理变更，并可被批准 / 回退（`snapshot_session` / `synchronize_session_changes`）；不成立则补齐，而不是绕开账本。
**依赖：** WP1（执行上下文需要会话）。
**验收：** 外部写入的文件出现在该会话的待处理变更里，批准 / 回退与 Magi 自己 agent 的改动行为一致；会话列表独立呈现该类型。
**注意：** 触及会话模型（共用热点），提交保持最小；模型变更遵守“不做兼容”，缺省值保证旧数据可读。

### WP4 宿主适配器与 daemon 集成（magi-api）　〔L，5–7〕
**目标：** 把 crate 的 trait 接到 Magi 的真实能力，并随 daemon 启停。
**改动：**
- 新建 `crates/magi-api/src/mcp_service.rs`（不改老文件的结构）：
  - `ToolBackend`：`schemas()` 用 `BuiltinToolName` 提供说明与 schema；`path_requests()` 包装 `tool_path_access_requests`；`invoke()` 调用 WP1 / WP2。
  - `WorkspaceResolver`：来自 `workspace_registry` 的工作区根。
  - `AuditSink`：写入现有审计事件流（新增 `mcp.tool.call` 类事件，字段见设计 §9，不含令牌原文与文件正文）。
  - `AttributionResolver`：`External` 用默认实现；`FollowWebSlot` 由 GPT Web 线提供实现（WP13），此处只保留注入点。
- 服务生命周期：开启时在 `127.0.0.1` 随机高端口启动 `magi_mcp_server::http::router`，端口写入 `state_root` 端口文件；关闭时停止；daemon 关闭时释放。
- 只在 `ApiState` 上增加**一处**最小挂载点。
**依赖：** WP1、WP2、WP3。
**验收：** 用 crate 的 HTTP 路由在集成测试里跑通：`initialize` → `tools/list`（随权限档变化）→ `tools/call`（只读放行、写入进入审批、越界拒绝）；服务关闭后端口不可达。

### WP5 令牌持久化与管理 API　〔M，3–4〕
**目标：** 令牌元数据可持久化、可管理，原文只出现一次。
**改动：**
- 设置存储新增 section（例如 `mcpServer`），保存 `TokenRecord` 列表与服务开关；读不出来视为无令牌（设计 M11），绝不让 daemon 起不来；每次变更后原子写回。
- 新路由模块（新前缀，例如 `/mcp/*`，放在新文件）：`GET /mcp/status`、`GET /mcp/tokens`、`POST /mcp/tokens`（返回一次性原文）、`DELETE /mcp/tokens/{id}`、`POST /mcp/tokens/revoke-all`、`PATCH /mcp/tokens/{id}`（调整权限档 / 有效期，调整后断开旧连接）、`GET /mcp/config-snippets`（Claude Desktop / Cursor 的 stdio 与远端 JSON，令牌用占位符或一次性展示，**不回显已有令牌原文**）。
- 令牌管理接口只对可信本机入口开放；网络客户端无法调用管理面（设计 M8）。
- 创建令牌时校验：工作区已注册；权限档 `edit_trusted` 需要显式确认字段；网络令牌默认权限档不高于 `edit`。
**依赖：** WP4。
**验收：** 重启后令牌仍可用（原文不落盘）；吊销即时断开；损坏的设置文件不影响启动；创建响应之外任何接口都不出现原文。

### WP6 stdio 中继二进制　〔M，3–4〕
**目标：** 本机客户端通过配置直接拉起 `magi-mcp --stdio`，令牌校验在 daemon 侧。
**改动：**
- 把 `magi-web-model/src/harness/stdio.rs` 里的本地 socket 中继（`local_endpoint_for`、`StdioRelay`、服务端 `serve_local_socket`）**上移到 `magi-mcp-server`**，并加上连接握手：中继首先发送令牌，daemon 认证后才进入 JSON-RPC 转发；令牌来源为 `--token-file` 或环境变量，**不接受命令行明文参数**（避免出现在进程列表）。
- 新增二进制（放在 `magi-mcp-server` 的 `bin/`，名称 `magi-mcp`）。
- 本地 socket 仅当前 OS 用户可访问（Unix 权限 0600 / Windows 命名管道 ACL）；不监听 TCP、不写端口文件。
- 与 GPT Web 线的 harness 迁移协调（设计 §16）：MCP 线新增，GPT Web 线的旧 harness 保持冻结，直到 WP14 的原子迁移。
**依赖：** WP4。
**验收：** 用官方 MCP 调试客户端 / 自写 Node 脚本通过 stdio 连接，令牌错误被拒，令牌正确可列出并调用只读工具；进程列表里看不到令牌。

### WP7 设置页“MCP 服务”　〔M，4–5〕
**目标：** 用户可以启用服务、创建 / 吊销令牌、复制配置片段、查看活动。
**改动：**
- 新组件（新文件，例如 `web/src/components/SettingsMcpServerSection.svelte`）挂到现有设置页的合适分区（如“能力”），不新增顶层 Tab；文案沿用现有 i18n 字典（中英文 key 一一对应）。
- 内容：总开关与状态（是否运行、端口、活动连接数）；客户端与令牌列表（名称、工作区、权限档、归属模式、有效期、最近使用）；创建令牌对话框（一次性展示原文并提示保存）；配置片段复制；权限档说明与风险提示；“一键撤销全部”。
- 前端只消费 WP5 的 API，不自造状态。
**依赖：** WP5。
**验收：** 增加对应 `test:*` golden（源码断言 + 行为断言）；`npm --prefix web run check` 通过；`edit_trusted` 有醒目标记与撤销入口。

### WP8 跨会话待审批入口　〔M，3–4〕
**目标：** 外部审批用户看得见、能处理，不依赖当前打开的会话。
**改动：**
- 新增跨会话查询与解析接口（例如 `GET /mcp/approvals`、`POST /mcp/approvals/resolve`，在新模块），只列外部审批并带来源客户端；解析仍调用 `ToolApprovalRegistry::resolve`。
- 前端：通知中心 / 全局待办中出现待审批项（客户端名称、工具、路径摘要、超时倒计时），提供“允许一次 / 拒绝”；系统通知；页面未打开时的行为（超时失败）在设置页说明。
- `web-client-bridge.ts` 对 `tool.approval.requested` 的处理要区分外部审批，避免出现在错误会话里。
**依赖：** WP2、WP5。
**验收：** 外部写入触发审批，在任意打开的会话里都能看到并处理；无人处理 90 秒后客户端收到明确超时错误。

### WP9 `magi.changes.*` 与 `magi.git.*`（先核对，再实现）　〔M，3–5〕
**目标：** 把 Magi 的差异化能力（变更账本、git）以标准工具开放。
**改动：**
- **先核对：** 现有 git 工具与变更账本的对外形态（工具名、参数、只读 / 变更分类、是否依赖会话）。核对结论写回设计 §6。
- 在 `catalog.rs` 增加映射与类别：`changes.list` / `diff` 为只读；`changes.approve` / `revert` 为 `Write`（逐次确认）；git 只读能力为 `Read`，变更类为 `Write` / `Destructive`（`push` 永远逐次确认）。
- 需要新封装的，作为宿主工具适配，不复制业务逻辑。
**依赖：** WP3、WP4。
**验收：** 外部客户端可查看待处理变更与差异，批准 / 回退需要人工确认；git 只读工具自动放行。

### WP10 审计与活动展示　〔S–M，2–3〕
**目标：** 每次调用可查。
**改动：** 审计事件持久化沿用现有审计账本；设置页“活动与审计”列出最近调用（令牌前缀、客户端名称、工具、路径摘要、审批结果、时间）；提供按客户端过滤与保留期限制。**不记录令牌原文，不记录文件正文。**
**依赖：** WP4、WP7。
**验收：** 越权、拒绝、超时、成功都有记录；日志与诊断输出中搜不到令牌原文。

### WP11 网络模式（Cloudflare 命名隧道）　〔L，6–9〕
**前置门槛：** WP15 的威胁模型评审已通过；WP1–WP10 已稳定。
**改动：**
- **隧道托管：** 扩展 `TunnelManager`（或在其旁新增托管类型）支持**命名隧道**：隧道 token 按文件引用（用户私有权限），不进命令行、日志、settings、`state_root`；入口规则只转发到 MCP 服务的独立回环端口，**不转发到 Magi 主应用端口**（M10）。
- **服务端：** `HttpConfig.allowed_hosts` 写入隧道主机名（网络模式下不再放行回环名称之外的未知主机）；`allowed_origins` 默认空。
- **crate 增强：** 每令牌限流（调用频率与并发）、连续认证失败按来源退避、请求体与响应体上限（已有）、可配置挂起时限。
- **默认与提示：** 网络模式默认关闭；开启前展示风险说明并要求确认；网络令牌默认权限档不高于 `edit`，且不得默认 `edit_trusted`；提供“一键撤销全部令牌并停止隧道”。
- **设置页：** 开关、风险说明、隧道方案与状态、公网地址、复制、启动 / 停止。
**依赖：** WP4、WP5、WP7、WP15。
**验收：** 远端客户端经隧道使用同一套工具；未开启时公网不可达；撤销令牌与停止隧道立即生效；主应用端口经隧道不可达（用请求探测验证）；限流与退避可复现。

### WP12 OAuth 2.1（可选）　〔L，5–7〕
**目标：** 支持要求 OAuth 的客户端（授权码 + PKCE、动态客户端注册），Magi 作为授权方，授权页在 Magi 内完成并落到同一套令牌与权限档。
**依赖：** WP11。**注意：** 第一版不做；做之前先确认目标客户端（ChatGPT 连接器、Claude 远端连接器）的实际要求，避免过度实现。

### WP13 GPT Web 接入（follow_web_slot）　〔M，3–5，属 GPT Web 线协作〕
**目标：** GPT Web 连接器作为普通客户端使用 MCP 服务，调用内联显示在 Web 会话的 turn 里。
**改动：**
- GPT Web 线实现 `AttributionResolver`：槽位拥有者有进行中的 turn 时返回 `WebSlotTurn { session_id, turn_id }`，否则拒绝（设计 W9）。
- 令牌签发：槽位建立时为该连接器生成 `follow_web_slot` 令牌，工作区取自拥有者会话的项目；槽位释放即吊销。
- 连接器自动配置命令写入令牌与地址（站点适配层）。
- 真实通道 spike：ChatGPT 连接器对 `tools/list`、挂起 `tools/call`、审批耗时的实际行为。
**依赖：** WP4、WP5；GPT Web 阶段 0–2 已稳定。
**验收：** 见 GPT Web 文档 §13 阶段 3。

### WP14 harness 迁移（原子提交）　〔S–M，2–3〕
**目标：** 把 GPT Web 专用的 harness 收敛，工具能力只保留一条路径。
**改动：** 待 WP13 完成后，用**一次原子提交**删除 `magi-web-model/src/harness/{mod,http,mcp}.rs` 里被 MCP 服务取代的部分（inventory / call 包装、turn 令牌、挂起表），保留 OpenAI Tunnel 托管等 GPT Web 专有的通道代码；调整 `web_model_harness.rs` 只剩连接器与令牌联动。迁移前两线都不在原位置做结构性改动。
**验收：** GPT Web 工具能力完全经 MCP 服务；旧 harness 路径无引用。

### WP15 安全评审与威胁演练　〔M，3–4，**网络模式前必做**〕
**输出：** 一份评审记录（放在 `docs/`）与可重复的测试。
**必须覆盖：**
1. **令牌：** 原文不落盘、不进日志与诊断；哈希比较恒定时间；吊销即时断开挂起调用；泄露演练（用泄露的令牌验证只能做该工作区、该权限档允许的事）。
2. **越权与逃逸：** `..`、符号链接（含创建符号链接再读写）、大小写与 Unicode 变体路径、Windows 路径分隔与保留名（若支持 Windows）、超长路径、TOCTOU（校验后被替换）。
3. **注入与滥用：** 参数里的命令注入（`shell.exec`）、`apply_patch` 里越界路径、超大参数与结果、并发风暴、慢速连接。
4. **网络面：** DNS 重绑定（Host / Origin 组合）、无令牌与错误令牌的统一 401、隧道入口只到 MCP 端口、主应用端口不可达、CORS。
5. **审批链：** 无法绕过审批（直接调用内部名、大小写变体、别名）；预授权不豁免删除 / 推送 / 执行；超时后不补执行。
6. **Windows / 跨平台**（若发布）：本地 socket 的命名管道权限。
**验收：** 所有用例有自动化测试或可复现步骤；发现的问题全部关闭或明确接受并记录。

### WP16 真实客户端验收与文档收尾　〔M，3–4〕
**内容：**
- 端到端脚本（例如 `scripts/verify-mcp-server.mjs`，不引入新运行时依赖，用原生 Node 说 JSON-RPC）：stdio 与 HTTP 两条链路；覆盖 §6 场景。
- 用至少两个真实 MCP 客户端做兼容验证（官方调试客户端 + Claude Desktop 或 Cursor），记录版本与结论。
- 更新 `docs/README.md`、设计文档状态、GPT Web 文档 §8；把已完成的工作包与验证结论写回本文 §2。

---

## 5. 依赖关系与建议顺序

```text
WP1 ──► WP2 ──► WP3 ──► WP4 ──► WP5 ──► WP7 ──► WP10
 │                       │        │
 │                       │        └────► WP6（stdio）
 │                       └────► WP8（待审批入口，也依赖 WP2、WP5）
 └────────────────────────────► WP9（先核对，可与 WP4 后段并行）

阶段门槛：WP1–WP10、WP16（本机部分）通过 ──► WP15 安全评审 ──► WP11 网络模式 ──► WP12（可选）
GPT Web 线稳定 + WP4、WP5 ──► WP13 ──► WP14
```

| 里程碑 | 包含 | 出口标准 |
| --- | --- | --- |
| **M-A 本机只读** | WP1、WP4（只读）、WP5、WP6、WP16 的 stdio 部分 | 官方调试客户端通过 stdio 列出并调用只读工具；越权被拒；审计可查 |
| **M-B 本机可写** | WP2、WP3、WP8、WP7、WP9、WP10 | 外部写入经 Magi 界面审批，变更进账本可回退；设置页可管理令牌 |
| **M-C 网络模式** | WP15、WP11（WP12 可选） | 安全评审通过；远端客户端经隧道可用；一键撤销与限流验证通过 |
| **M-D GPT Web 接入** | WP13、WP14 | GPT Web 会话经 MCP 使用工具，旧 harness 移除 |

**并行建议：** WP1（执行入口）与 WP5（令牌管理 API）、WP6（stdio）相对独立，可由不同 agent 并行；WP3 与 WP1 都触及运行时核心，建议同一 agent 顺序完成。

---

## 6. 端到端验收场景（M-A 起逐步启用）

1. **只读读取：** 令牌 `read_only`；`tools/list` 只含读取 / 搜索工具；`magi.fs.read` 读工作区内文件成功；读 `../` 与 `.magi/` 下文件被拒。
2. **写入审批：** 令牌 `edit`；`magi.fs.write` 触发 Magi 界面审批；允许后文件写入并出现在外部会话的待处理变更；拒绝 / 超时返回明确错误且文件未变。
3. **删除与执行：** `magi.fs.remove` 与 `magi.shell.exec`（`exec` 档）永远逐次确认；`edit_trusted` 下写入免确认但删除仍确认。
4. **归属：** `external` 调用记录在该令牌的外部会话；`follow_web_slot` 没有进行中的 turn 时拒绝。
5. **吊销：** 吊销令牌后，已建立连接的下一次调用与挂起中的调用都以错误收口，待审批被取消。
6. **重启：** daemon 重启后令牌仍可用、外部会话与审计仍在、挂起调用清空。
7. **网络（M-C）：** 经隧道调用成功；主应用端口经隧道不可达；错误令牌与未知 Host 被拒；限流生效。

---

## 7. 待用户拍板的问题

| # | 问题 | 我的倾向 |
| --- | --- | --- |
| Q1 | 外部工具会话在侧栏怎么呈现：独立分区、还是混在最近会话里并带类型标识？ | 独立分区，可折叠，默认折叠 |
| Q2 | 令牌默认有效期：不过期，还是默认 90 天？ | 默认 90 天，创建时可改；网络令牌强制有效期 |
| Q3 | 第一版是否开放 `magi.shell.exec`？ | 代码支持但默认不出现在目录，需要用户在令牌上显式选择 `exec` 档 |
| Q4 | 网络令牌能否授予 `edit_trusted`？ | 默认不允许，需要在该令牌上单独确认并显示醒目警告 |
| Q5 | `magi.git.*` / `magi.changes.*` 的范围 | 先只读 + 变更批准 / 回退，git 变更类等 WP9 核对后再定 |
| Q6 | OAuth 是否进入本轮范围 | 不进入；网络模式先用 Bearer 令牌，等真实客户端需要再做 |
| Q7 | Cloudflare 命名隧道由谁创建 | 用户在自己的账号创建并提供隧道 token 文件；Magi 只托管，不代建；Connect 就绪后再评估托管地址 |

---

## 8. 完成定义（DoD）

一个工作包算完成，必须同时满足：
1. 代码与测试提交，`cargo check --workspace --tests` 与被改 crate 的测试通过；前端改动跑 `npm --prefix web run check` 与对应 `test:*`。
2. 对应的验收场景（§6）有自动化测试或可复现的手动步骤，并记录结果。
3. 没有新增第二事实源：令牌、审批、账本、审计都只有一个 owner。
4. 文档同步：设计文档与本文的进度、结论、待办已更新；不留“文档说 A、代码做 B”。
5. 涉及安全面的工作包（WP4 写入相关、WP5、WP6、WP11）已按 §1 的安全门槛与 WP15 的相关用例通过。
6. 如实说明未运行的验证与已知限制。

**整个 MCP 功能算完成**：M-A 到 M-C 全部通过（M-D 属 GPT Web 线协作，单独验收），WP15 评审记录已归档，`docs/README.md` 与设计文档状态更新为“已实现”。
