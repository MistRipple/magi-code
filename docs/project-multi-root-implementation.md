# 项目与多源文件夹：实现合同与验收记录

更新：2026-10-30。本文记录本次实施的源码审计和决策；功能交付以各阶段验证记录为准。

## 工作基线

- 独立 worktree：`/Users/xie/.codex/worktrees/project-multi-root/magi-rust-rewrite`。
- 起点：`907954b15409faaafe4e0ba7f02783d88c674e03`，另完整带入原工作树七个已修改文件及 `.github/releases/v3.0.61.md`。这些改动不是本功能交付。
- 不创建提交、Tag 或发行版。当前工作树保留可审阅 diff。
- 基线 `cargo check -p magi-workspace -p magi-api -p magi-daemon` 已通过。

## P0：调用链与唯一拥有方

| 边界 | 当前源码落点 | 改造职责 |
| --- | --- | --- |
| 项目身份、注册表 | `magi-workspace/{models,registry,identity}.rs` | 同一个 WorkspaceStore 管理成员、主要目录、修订；旧 rootPath 只做单向读取 |
| 注册/移除 HTTP | `magi-api/routes/workspaces.rs` | 管理命令、CAS、持久提交后发 registry 事件，移除沿原 MCP 吊销和 checkpoint |
| 状态恢复与项目 projection | `magi-daemon/daemon/{persistence,runtime}.rs` | 迁移日志、布局写屏障、按 ID 的固定存储、重启恢复 |
| 会话归属与个人会话 | `magi-session-store/{models,store}`、`magi-api/scope_binding.rs` | SessionRecord.workspaceId 保持唯一归属，个人执行根保持独立 |
| 输入/排队/接纳 | `magi-api/{input_service,turn_service,task_dispatch}.rs` | 提交前固定源目录绑定；替换、队列领取及重试不读取当前 UI 选择 |
| Task/执行计划 | `magi-conversation-runtime/{dispatch_submission,task_execution_dispatcher}.rs` | 绑定随原 Task 和恢复检查点持久化，执行前重查授权及资源 |
| 文件/搜索/附件/下载 | `magi-api/{scope_binding,change_projection}.rs`、`routes/changes_files_tunnel.rs` | 项目+会话+目录 ID 统一解析实际路径；相同相对路径不混淆 |
| HTML 与资源预览 | `magi-api/routes/file_site.rs` | 资源凭据绑定原会话和源目录，不借用活动目录 |
| 终端 | `magi-api/routes/terminal.rs` | 源目录绑定解析到执行副本，终端会话保留目录身份 |
| 工具权限与路径 | `magi-tool-runtime/{types,policy,registry}.rs`、`magi-permissions` | cwd 和授权根同时更新，原允许/拒绝/只读及符号链接检查不放宽 |
| Git | `magi-api/git_tool_runtime.rs`、`routes/workspace_vcs.rs`、`magi-git` | 按实际仓库查上下文，不使用 session-only 的任意首条上下文 |
| 会话隔离 | `magi-api/isolation.rs`、`routes/session_isolation.rs`、`magi-session-isolation` | 原 registry 演进多根映射；复制、账本、合入、清理按目录归属 |
| 子代理 | `magi-conversation-runtime/{agent_spawn_preflight,task_execution_dispatcher}.rs`、`magi-api/agent_service.rs` | 父实际目录映射与权限子集，按目录记录产出，保持原委派门槛 |
| 规则与上下文 | `magi-context-runtime`、dispatcher | 项目共享规则和源目录规则分层，主要目录规则不扩展至其他目录 |
| 项目记忆 | `magi-project-memory`、dispatcher | 记忆存储按固定项目 ID 定位，迁移旧路径键，不随主要目录变化 |
| 知识与索引 | `magi-knowledge-store`、`magi-api/routes/knowledge.rs`、`state.rs` | 索引结果包含源目录身份，统一项目读模型 |
| MCP 令牌 | `magi-mcp-server`、`magi-api/{mcp_service,mcp_runtime}.rs`、`routes/mcp_server.rs` | 授权时固定目录 ID 集合，撤销检查当前成员，添加不自动扩权 |
| GPT Web/插件 | `magi-api/{web_model_ops,mcp_service}.rs`、`magi-plugin-*` | 复用原 owner session/active turn 和工具上下文，不新增注册表 |
| App Server | `contracts/app-server/app-server.schema.json`、`magi-api/app_server` | schema 增量修改后统一生成，排队与继续沿原请求合同 |
| Web 导航与项目 UI | `web/src/{web/WebWorkbenchShell.svelte,stores/composer-workspace*,web/agent-api.ts}` | 项目一级会话列表，目录选择只影响新输入 |
| 文件/变更 UI | `ProjectFileTree.svelte`、`RightPane.svelte`、file-reference 与 right-pane stores | 文件引用/预览/变更键携带目录 ID，按目录/仓库展示 |
| Desktop 宿主 | `apps/desktop` 的 context、文件选取和 reveal bridge | 只转发可信本机能力，不持有业务项目或授权事实 |

## P0：固定合同

1. **项目模型**：沿用 workspaceId；sourceFolders 的成员包含 sourceFolderId、HostPath 的展示路径和 pathRef、可选别名及时间。primarySourceFolderId 必须引用现存成员；revision 单调增长，管理命令使用 expectedRevision。字段只有一个事实来源，不同时保存可写 rootPath 和成员列表。
2. **存储**：项目会话 projection 固定到 `<state>/projects/<workspaceId 的 UTF-8 十六进制编码>/.magi/session-projections`，canonical events 继续使用原 state 根。项目规则、记忆等共享内容也按同一身份定位；源目录 AGENTS.md 仍来自各源目录。旧源目录 `.magi` 不再作为新业务写入口。
3. **迁移**：先恢复旧 projection 事务，写入布局升级屏障，再按项目迁移文件，最后原子提交新注册表。迁移有持久清单，失败重启继续；原文件保留为迁移输入/备份，成功后只从新根读写。重复目标内容不同则明确拒绝，不能用覆盖方式选赢家。旧 daemon 遇到新布局或新 record 必须拒绝读取/写入。
4. **目录绑定**：请求用 sourceFolderId 选默认 cwd；目录授权是独立集合。接纳时固定项目修订、默认目录 ID 和允许目录 ID，排队输入也立即固定；任务执行映射由隔离/Git 拥有方管理。每次执行和恢复核对当前目录成员，已撤销目录明确报错。
5. **成员变更**：首版只允许项目空闲时修改成员/主要目录；存在活动 Task、待处理审批、队列输入或未合入副本/子代理成果时拒绝受影响变更。主要目录移除必须同命令显式指定替代目录，最后一个成员不能移除；不自动选第一项。
6. **路径与重叠**：管理入口 canonicalize 并验证真实目录，拒绝与全局状态根重叠。首版新增配置拒绝项目内源目录重叠，以及跨项目引用同一物理目录或祖先/后代目录；符号链接别名按规范路径检查。同仓库不同 worktree 可以作为不同源目录，但仓库元数据操作仍按 git common dir 加锁。旧记录迁移保留身份与历史，旧重叠配置的新增/编辑必须先解决冲突。
7. **缺失目录**：源目录离线不影响项目历史读取；文件操作/执行返回该目录不可用，不回落。恢复不会因为检查 identity 而重建不存在的用户目录。
8. **Git 与隔离**：多仓库租约按规范仓库身份稳定排序获取，获取不全则释放已取得资源再等待；取消释放全部。隔离初始化是多目录操作，失败保留可核对状态，不回落原目录写。普通复制不创建可写共享依赖链接；已有外部链接按原权限规则处理。
9. **规则与外部权限**：项目共享规则来自固定项目存储，源目录规则按目标目录生效；旧 MCP 单根授权迁移为原目录 ID 单元素集合。新增成员不会改变旧令牌/旧 Task 的权限；移除目录即撤销旧快照中的对应访问。
10. **兼容**：单目录旧注册入口继续接受单根请求；无法表达目录身份的旧资源调用遇多目录项目应明确拒绝。UI 的 rootPath 兼容字段只作为主要目录展示投影，不能用作项目存储位置或多根授权。
11. **项目移除**：沿原吊销、执行结算、checkpoint 与取消导航入口；保留源文件、固定项目历史、未合入成果。重新注册/恢复以项目身份为准，不能按主要路径猜出另一个项目身份。

## 阶段进度与验证

以下条目按实施时间追加；早期的“尚未完成 / 未开放”描述只对应当时阶段，当前剩余工作以文末为准。

- P0：已建立调用链映射与上述合同。后续发现调用方差异时修订具体接线，不扩大范围。
- P1：已实现稳定源目录身份、主要目录引用、修订/CAS、管理事务先落盘后发布、旧单根记录单向读取，以及按项目 ID 固定存储的迁移。迁移清单、原文件保留、目标冲突拒绝、重启恢复与旧布局写屏障已接入 daemon。项目记忆也已改为稳定 workspaceId 定位，并纳入旧路径迁移清单。
- P2：已实现 `magi-core::ProjectDirectoryBinding` / `ProjectSourceFolderGrant` 和 WorkspaceStore 的捕获、当前成员校验与目标解析。`sourceFolderId` 已接入 HTTP/App Server 输入；可信入口生成的目录授权写入原 Input payload、accepted pendingDispatch 与 Task.executor_binding，普通 Conversation 则写入原 user item。目录选择参与请求指纹，服务端授权不接受客户端 JSON，也不参与客户端重试指纹。首次资源准备、显式继续、Goal 自动推进和子任务沿原绑定，出队重查成员；尚未完成所有工具多根权限和执行映射，不代表跨目录能力已经交付。
- P3–P6：待实施。当前没有多目录创建/编辑路由或 GUI 入口。

后续验收沿原任务计划矩阵；只记录实际执行的检查。当前没有多目录运行验收或安装包/跨平台通过证据。

### 当前验证记录

- `cargo check -p magi-daemon` 通过（包括 API 与工作区消费者）。
- `cargo test -p magi-core --lib paths::tests::`：10 项通过，包含稳定项目 ID 编码冲突边界。
- `cargo test -p magi-project-memory --lib`：15 项通过，包含按项目 ID 并发打开与共享锁。
- `cargo test -p magi-workspace --lib`：33 项通过，包含迁移身份、CAS/写盘失败原子性、重叠目录、旧绑定不扩权与撤销、缺失目录不创建。
- `cargo test -p magi-daemon --lib daemon::persistence:: -- --test-threads=4`：38 项通过，包含新迁移、项目改名/移除原主要源目录后固定历史仍可恢复、原 canonical event/审批/输入/删除/损坏会话边界。
- daemon 全库曾执行 136 项：133 通过，3 项因旧存储路径断言失败；修改对应断言后分别定向复验通过。不是一次最终全库全绿的记录。
- `cargo test -p magi-api --lib routes::workspaces::tests:: -- --test-threads=4`：最新 20 项通过（注册/移除持久事务与现有目录 API）；随后追加旧注册入口拒绝把多目录项目解释为单目录的定向回归，单项通过。
- 追加 `cargo test -p magi-daemon --lib project_storage -- --test-threads=2`：3 项通过，其中项目主要目录移除测试同时验证旧记忆索引/条目迁移与原文件保留。
- 追加 `cargo test -p magi-api --lib workspace_registration_write_failure -- --test-threads=1`：1 项通过，真实阻断落盘路径后不发布项目或 registry 事件，修复后重试保持原稳定身份。
- `cargo test -p magi-conversation-runtime --lib conversation_loop_keeps_reference_context_below_current_task_prompt -- --test-threads=1`：1 项通过，记忆存储接线不改变当前任务高于历史参考的提示词边界。
- `git diff --check` 通过。此记录对应 P1 阶段；后续目录输入协议和检查记录见下文，Desktop 宿主尚未修改或验收。

### 紧接着的实施入口

1. 目录输入的接纳与持久化已经接入；继续核对旧 Task/旧队列的单向补齐：旧单目录来源必须有稳定证据，不能在成员先移除再新增、最终仍为单成员时重解释。当前多目录项目会拒绝没有绑定的旧请求。
2. `TaskExecutionPlan::Dispatch` 继续持有实际 execution_root，授权只在原 Task.executor_binding。首次资源准备已接入冻结默认来源，替换了当前主目录快照初始化入口；Git context 和隔离副本暂时仍为每会话单项，来源不匹配时明确拒绝。P3 必须演进原 registry 为多源映射，不能通过当前主要目录或任意首条上下文兜底。
3. 子代理从父 Task 继承目录授权；TaskStore 插入和持久恢复校验权限子集，普通 Conversation 校验成员。工具批次在账本去重前、各工具执行前、预检审批恢复后与运行时审批恢复后均核对当前成员；主线/子代理的内置、MCP 和插件共用该边界。Goal 自动推进已按 createdByTurnId 定位自己的 Task，Task 被清理时沿原 canonical 请求恢复；不再从当前 active_execution_chain 继承。仍需把这一绑定覆盖完整 Goal 插队/暂停恢复的多目录运行验收。
4. 工具默认权限目前由 `working_directory` 单根推导；多根映射需要和 snapshot hooks、write claims、shell cwd 及每次工具执行的当前授权校验一起接入。
5. 文件/终端/HTML 预览接口已接入显式 sourceFolderId 并共用解析器；旧单根接口遇多目录作用域明确拒绝。仍需接入 UI 文件引用、附件和多根隔离/Git 执行映射，不能把资源路由回归视为整个 P2 已完成。
6. P1 还应审计旧目录在全新 state root 中重新注册的可移植导入入口：新注册记录已写成 canonical 格式，迁移目前只自动处理旧 rootPath 注册表。不能靠 load 时优先旧路径的回退掩盖此问题。
7. 当前 `native_root_path()` 只代表主要目录默认 cwd；所有完整授权和历史存储必须避免沿用它作为单根事实。项目记忆已完成稳定 ID 接线；目录编辑目前只有 Store API，没有可用的 HTTP/GUI 多目录入口，必须等 P2–P4 接线后再开放。

### P2 输入与执行事实接线（2026-10-28）

- 队列编辑不允许改变 sourceFolderId；请求反序列化不能注入 projectDirectories。原 Input payload 单独保存可信绑定，重试、队列还原和 pendingDispatch 恢复保留原授权。
- 主线 Continue 和用户中断后的新 Task 从原 Task 恢复目录绑定；有显式 sourceFolderId 时必须匹配原默认目录。Goal 后台续跑沿创建者 Task/Turn 的绑定，新子任务沿父 Task 继承。
- 运行中补充不改变当前 Turn 的目录或授权，队列改投补充也核对原 Input 绑定。当前 Turn 的合法性和 expectedTurnId 冲突优先于目录校验，保持既有错误合同。
- 资源准备使用冻结源目录解析 Git/账本；隔离副本来源不匹配或已缺失时明确拒绝。删除了按当前主要目录初始化 snapshot 的无调用旧方法。macOS 的 `/var` 与 `/private/var` 别名按 canonical 路径比较，保留原自动隔离与 Git 冲突检查。
- 定向回归：项目主要目录切换后，从原 accepted pendingDispatch 和 Task projection 重载仍选原目录同名文件；队列接纳和重试仍保留原绑定；撤销原成员立即失效。另验证子任务只缩小授权、缺失绑定/新增目录/损坏持久化拒绝。
- 已通过：Workspace 33 项；runtime 669 项；Task 继承/恢复新增回归；队列持久化回归；DTO 可信边界与指纹回归；`cargo check -p magi-daemon`；`npm run protocol:check`；`npm --prefix web run check`（0 错误、0 警告）。Web 依赖由 `npm ci --ignore-scripts --workspace magi-web --include-workspace-root --no-audit --no-fund` 安装，未修改依赖清单。
- API 库第一次全量：993 通过、7 失败、2 忽略；失败来自路径别名比较和引导错误码先后。修正后全部 7 项已各自定向通过，随后运行 `cargo test -p magi-api -p magi-orchestrator -p magi-conversation-runtime --lib -- --test-threads=8 --quiet` 完整复验：API 1000 通过、2 忽略，runtime 669 通过，orchestrator 66 通过，均 0 失败。不将零匹配过滤测试或单项复验记作全库全绿。
- 完整 P2 的多根工具授权、文件/终端/预览目录参数，P3–P6 及安装包/跨平台验收仍未完成。所有成果保持未提交。

- 补充通过：`cargo test -p magi-core --lib -- --test-threads=8` 50 项；Web `test:orchestrator-session-config`、`test:bridge`、`test:agent-api` 均通过；`cargo fmt --all --check` 和 `git diff --check` 通过。
- daemon 最终全库复验：`cargo test -p magi-daemon --lib -- --test-threads=8`，137 项全部通过，包含真实重启、目录迁移、canonical Input、审批与 Task 恢复。
- 全库通过后追加 Goal 来源收敛：`goal_directory_binding` 从 Goal 创建者 Task 读取，必要时从同一创建者的 canonical 请求读取，不跟随当前活动链。相关编译及 Goal 提示词/续跑调度回归已定向通过；这项小改动之后未再次执行全部库测试。
- Goal 来源新增断言复用项目冻结回归：修改主要目录后读取创建者 Task 的原绑定；清理原 Task 后仍从同一 canonical pendingDispatch 恢复。首次夹具缺少 orchestrator thread，补齐真实 SessionStore mission 初始化后，`project_directory_binding_survives_acceptance_reload_and_primary_change` 定向复验通过。

### P2 资源边界与恢复回归（2026-10-28）

- 文件内容、图片原始内容、下载、reveal、目录列表、HTML 预览与终端使用同一项目源目录解析；多目录请求没有 `sourceFolderId` 明确拒绝。双目录同名文件、成员撤销、缺失目录、路径穿越和外部符号链接已有路由执行回归。
- HTML 预览签名覆盖项目、会话、源目录和实际执行根；每个资源重新校验成员与映射。切换主要目录后仍读原文件，隔离映射变化后旧凭据拒绝，不静默转向新副本。删除了无目录绑定的旧预览资源解析分支；凭据只在当前 daemon 生命周期内有效。
- 终端绑定包含源目录身份，同一 Tab 不能改绑另一目录；连接期间在输入前和定时检查中复核原绑定，失效时关闭该终端。没有为目录预览或失效副本重新创建磁盘目录。
- 工具边界检查覆盖账本缓存结果复用之前、每项调用、预检审批以及运行时审批后的再次执行。真实文件删除回归同时覆盖主线/子代理、内置/MCP/插件；批准后撤销目录授权仍保持原文件和零外部副作用。
- 旧无绑定任务仅允许从未编辑且保留旧源身份的单目录项目单向补齐；编辑后即使又只剩一个成员也拒绝猜测原来源。
- 重启保留缺失隔离副本的原映射；快照准备拒绝新建空副本或回落原目录。普通复制不再生成指回源目录的 `node_modules`、虚拟环境或 `target` 链接；写依赖/构建产物也保持副本独立。已有历史共享链接的迁移/处置尚未完成。
- 复制失败只清理本次独占创建的目标；上层不再因 `DestinationExists` 删除已有目录。目录读取错误不再被过滤成部分成功；外部 `.git` 文件或符号链接不进入副本。
- 已通过：runtime 全库 670 项；session-isolation 全库 17 项；资源文件新增回归 1 项、网页预览 6 项、终端 3 项、冻结绑定恢复回归 1 项。后续继续检查记录以最新复验为准。
- API 全库首次复验暴露轻量 harness 的 Dispatcher 与工具边界没有共享 WorkspaceStore 句柄，导致多项工具测试失败；已由 `with_workspace_registry` 同步同一个共享句柄。该轮全库在后续 HTTP fixture 等待时中断，不记为一次完整测试结果；修复后完整复验：1003 项通过、2 项忽略、0 失败。
- 当前仍没有多目录管理路由或可用 GUI 创建入口。多根工具权限/执行副本/变更账本、P3–P6、安装包及 Windows/Linux 验收仍未完成，目标保持进行中；所有代码未提交。

### P2/P3 执行账本推进（2026-10-28）

- `SnapshotManager` 现在按 `sessionId + canonical executionRoot` 管理账本；同一会话在两个源目录的同名文件各有独立 baseline、事件日志、blob 引用和 watcher。旧的无目录读入口在多根会话下返回空，禁止任意选一份账本；删除会话仍一次收口所有目录，隔离切换可只删除指定实际根。
- daemon/harness 的 SnapshotManager 使用固定项目状态根：项目按 workspaceId 编码，个人按 sessionId 编码，实际源目录再用 HostPath 引用哈希分片。旧源目录 `.magi/snapshots` 只作为一次性输入；迁移带版本与来源身份清单，采用 staging + rename + fsync，目标冲突/符号链接/重叠失败不覆盖旧数据。源目录保留原文件。
- 归档不会释放最后一份持久 baseline blob；删除会话前先读取事件并移除自身索引，再检查其它未加载账本的 durable 引用，避免重启后删除另一源目录的回退材料。对应多根同名、重启、归档、按根删除与 blob 保留回归已通过。
- `session-isolation` 的普通复制、缺失副本保留和文件资源边界仍沿上一条记录；实际多根 Git context、工具 cwd/权限映射、子代理 worktree 和 UI 管理入口仍未完成。
- 固定存储增加了真实 daemon/harness 回归：项目和个人账本均写入状态根，不在源目录创建新 `.magi/snapshots`；同项目两目录账本同时恢复，重建其中一目录的 Git baseline 不会清除另一目录的 pending 产物。源目录旧账本迁移身份清单不匹配、嵌套/符号链接旧根和外部链接均拒绝。
- 本轮收口复验：daemon 全库 137 项通过；`cargo check -p magi-daemon`、`cargo fmt --all --check`、`git diff --check`、`npm run protocol:check` 通过；Web 检查 0 错误、0 警告。完整 API 复验后又补了复制目标嵌套/已有目标保护和 `.git` 符号链接断言：session-isolation 17 项全库及对应单项均通过。

### 下一步执行顺序

1. 保持 P2 未完成状态，沿原 `ToolExecutionContext` / `Task.executor_binding` 接入源目录到实际执行根的映射与当前授权校验；显式目录工具参数、相对路径、Shell cwd、审批、snapshot hooks、write claims 必须使用同一映射，不能只追加 allowedPaths。
2. SnapshotManager 已完成 sessionId + 实际根分键与固定状态存储迁移；继续演进原 SessionIsolationRegistry / SessionCodeContextRegistry，并把工具 hooks / 变更投影接到全部获授目录；不能通过合成会话 ID、任意首项或新增平行 registry 模拟多根。存储迁移的进度与验证以“执行账本推进”记录为准。
3. 项目 Git context 当前仍用 Project(sessionId) 单项；必须一次改完 root 寻址、持久恢复、revision CAS、子代理分配/回执/应用和资源回收的调用方，再放开多仓库。不要只给 registry 添加第二项却让原 get(sessionId) 和 restore 覆盖它。
4. 在资源/执行/外部授权完整接线前，继续不开放多目录管理路由和创建 UI。保留所有用户已有改动，不创建 commit。
- 本阶段验证：`magi-snapshot` 24 项库测试、30 项生命周期集成测试全部通过；API 全库 1003 通过、2 忽略；daemon 全库 137 通过；固定项目/个人存储的新增 API 集成单项通过。随后针对未加载会话 blob 保留、释放并发与 start/archive 串行又作了小范围修正，snapshot 全部测试重跑通过；这些最后修正之后尚未再跑 API/daemon 全库。`cargo check -p magi-daemon`、格式检查和 diff 检查通过。
- 下一实际接线：为 `ToolExecutionContext` 增加受信的目录映射（源目录身份、源路径和执行路径），把全部获授根的路径预检、策略投影、相同相对路径、Shell cwd 与 snapshot hooks 使用同一映射；再演进隔离/Git registry 的多根恢复。当前工具执行仍只有单根 cwd，不能对外宣称已支持多目录执行。


### P2/P3 工具映射与隔离分键（2026-10-28）

- 新增 `ProjectDirectoryExecutionMap`：由冻结的 `ProjectDirectoryBinding` 和可信资源解析结果逐项构造，保留源目录 ID、源路径与实际执行路径；缺项、重复根、跨源重叠与归属不匹配拒绝。它是一次执行的派生投影，没有新增可写注册表或持久化入口。
- `ToolExecutionContext` 携带这份投影；内置工具执行前统一解析 `source_folder_id`、默认 cwd、实际根可用性和源路径策略投影。相对 allow/deny/read-only 策略固定按原默认源目录解释，不因选择另一目录重解释。默认写范围包含获授实际根，不包含共同祖先；完全授权也拒绝显式路径指回已隔离源目录。
- 工具真实 Shell cwd、文件读写及写保护使用同一解析结果。Shell 命令的路径预检改为相对其显式 cwd 解析。Shell 变更采集覆盖映射中的各根，`changed_files` 携带源目录 ID、相对路径及绝对 pathRef；同名文件不合并归属。文件系统采集不跟随外部目录链接，也跳过 `.magi`。
- 以上多根工具执行已有直接工具运行回归，但 Task dispatcher 暂只接入原单根执行映射：多根 Task 在缺少完整资源映射时明确失败，不把其它源目录猜成共享根。目录工具 schema 尚未开放选择字段。预检/审批、全部 snapshot hooks、MCP/插件/子代理和多根 Git 仍须继续接线，不能据此宣称端到端多目录任务已交付。
- 原 `SessionIsolationRegistry` 已按会话 + 规范源目录分键，持久化仍是原 `session-isolations.json` 数组；重载保留同会话全部映射及不可用副本。session-only 读取/删除遇多根拒绝任意取首项；按源读取/删除与整会话清理各有明确入口。删除单个副本只删除其自身目录，保留同会话其它副本。
- 文件/HTML/终端统一源目录解析、Task 接纳、Continue 和快照准备沿原隔离登记按来源查找。已隔离会话缺少对应映射、目录失效、归属变化或副本被替换为链接时明确拒绝。旧无目录 ID 的变更与隔离管理请求遇多目录项目拒绝，带明确目录 ID 的文件和预览保持可用。当前多根副本建立/合并的产品入口仍未开放。
- 验证：工具全库 317 通过、1 忽略；core 50 通过；session-isolation 18 通过。新增双源隔离集成回归验证真实复制、同名文件、落盘重载、各自 baseline、按源清理、缺失映射拒绝，已通过。API/运行时完整复验最终为 1005 通过、2 忽略 / 670 通过。中间一轮 API 因旧变更校验误阻断带 ID 的文件/预览有 2 项失败，修正目录参数传递后 14 项项目定向回归和上述完整复验均通过。此全库结果之后追加副本链接替换防护与单项回归，daemon 最新全库结果待下条补记。
- 下一步：使原资源准备合同返回全部获授目录的实际映射与 snapshot，演进原 Git context registry 的逐根寻址、revision、代理分配/回执/应用和仓库租约获取顺序。随后将工具映射接到统一预检、审批恢复、snapshot hooks 与所有外部工具入口；不要从当前主要目录或任意首条 registry 记录补齐缺失事实。P2–P6 目标保持进行中，所有改动未提交。

- 最新收口：daemon 全库 137 项通过，0 失败；补充副本链接防护后定向隔离回归通过。工具库再次完整复验 317 通过、1 忽略。`cargo check -p magi-daemon`、`cargo fmt --all --check` 和 `git diff --check` 通过。本轮没有改协议或 Web，沿用前阶段的协议/Web 验证，不把它们记作本轮重新运行。

### P3 Git 注册表与逐仓库产出（2026-10-28）

- 原 `SessionCodeContextRegistry` 已统一按 `sessionId + canonical worktreeRoot` 寻址，项目和个人会话都保留各工作树基线；同仓库 linked worktree 保留不同上下文，底层仓库锁仍按 common dir 共享。旧 session-only 读和代理修改入口遇多仓库明确拒绝，不任意选择仓库。
- HTTP Git revision CAS、merge preview、模型 Git 查询/修改、代理分配/恢复/产出记录/采用、会话清理已改为按实际目录选择上下文。观测主目录不会丢失活动子代理目录；代理路径和分支名称加入实际工作树身份，避免同任务同时准备多个工作树时碰撞。
- 代理结束逐仓库提交和回收，在原 `output_refs` 中保存完整多仓库结果；终态回执保留每个仓库的 base/head/changed_paths，保持原最终答复，不把多仓库 JSON 误当答复文字。原目录未写入或提交。隔离切换拒绝仍占用或未处理的代理成果，按原目录移除对应 Git context，不清除同会话其它仓库记录。
- `WorkspaceGitOperationCoordinator` 增加原子获取完整仓库集合、按原集合暂停/恢复以及逐仓库释放。完整集合获取失败释放部分占用，等待队列按会话统一排序，避免 A→B 与 B→A 在不同仓库形成不同队首；取消清除原恢复集合。单仓库在途复核失败保留同会话其它正在使用的租约。**完整集合获取尚未接入 Task 资源准备，不能据此宣称跨仓库并发端到端已交付。**
- 真实临时仓库回归覆盖：双仓库及 linked worktree 的基线/revision 独立、同任务跨仓库代理产出分别提交与重载、主目录 dirty 保留、HTTP 操作第二仓库不覆盖第一仓库、普通目录不释放其它仓库租约、逐仓库回收、交叉等待/暂停/恢复/取消。Git 单库 29 项、runtime 672 项、API 1006 项通过（2 项忽略）。API 全库后补充普通目录保持租约与逐仓库释放断言，单项复验通过。
- 下一步仍是原资源准备合同：从冻结授权逐项解析全部实际目录，先取得完整仓库集合，再重观测并准备各根账本；返回完整目录映射和账本集合，贯通预检、审批恢复、snapshot hooks、MCP/插件及子代理。不得把未准备的其它根猜成 live 源目录。目前 `tool_batch` 对缺少完整映射的多目录 Task 仍明确拒绝，HTTP/GUI 多目录管理入口继续不开放。
- P2–P6 整体目标保持进行中；安装包、真实 GUI、Windows/Linux 与真实外部服务尚无本功能验收证据。所有成果保持未提交。
- 本阶段最终 daemon 全库复验 137 项通过，0 失败；编译、格式和 diff 检查通过。全库后仅调整逐仓库释放的等待集合保留，相关 coordinator 4 项定向回归通过；未把前阶段协议/Web 验证记作本轮新运行。

### P2/P3 目录隔离与外部授权收口（2026-10-28）

- MCP 令牌现在在签发记录中持久保存 `ProjectDirectoryBinding`，`initialize`、路径检查、执行映射、审批等待和 liveness 共用这份授权；项目新增成员不会扩展旧令牌，项目成员撤销会让在途调用取消且不产生后续副作用。旧无绑定令牌只在仍可证明为原始单目录、未编辑的项目上单向补齐；迁移写盘失败保留原记录，不猜测新主要目录。
- GPT Web 槽位分别保存槽位租约代际和 canonical Turn ID，槽位目录从原 Turn 的冻结绑定解析；Turn 替换或槽位释放会拒绝继续调用，切换主要目录仍沿原 Turn 的目录绑定。外部 MCP、插件和内置工具共用源目录映射、路径检查、审批和变更账本。
- 隔离副本支持一次授权的全部源目录整组创建：源目录身份摘要进入副本路径，复制失败或登记持久化失败只清理本次新建目标，已有目标和其他目录副本保留；旧共享依赖链接被拒绝继续执行。注册表整组持久化保持锁，避免并发登记被旧快照覆盖。
- 会话隔离状态和变更接口携带 `sourceFolderId`；同名文件分别拥有执行根、快照、`snapshotId` 和变更身份。项目变更列表、差异、单文件批准和合入按选中源目录解析；丢弃前检查整组副本是否仍有未合入成果，取消隔离按整组处理；多目录旧入口拒绝任意选择首目录。部分合入保留其他目录副本及未合入成果。
- 本阶段实际通过：MCP API 定向 145 项、独立 MCP 库 66 项、runtime 库 675 项、API 多目录任务（Git + 普通目录 + 隔离 + 子代理）、整组副本创建/失败清理/持久化故障、同名文件变更与按目录合入、daemon 137 项、session-isolation 18 项均通过。`npm run protocol:check`、`npm --prefix web run check`（0 错误、0 警告）、`cargo fmt --all --check` 和 `git diff --check` 通过。端口监听重启测试曾在并行全库中被其他监听占用，单独重跑通过；完整 API 全库随后已以最新代码复验通过（1012 项通过、2 项忽略）。
- 当前仍未开放项目创建/编辑 GUI、跨平台安装包验收和真实外部服务验收；本 worktree 不创建提交。

- 原资源准备合同现已返回全部获授目录映射与 snapshot 集合，Task 执行先获取整组仓库租约，再复核目录身份和准备账本；工具预检、实际 cwd、路径策略、write claims 和 snapshot hooks 使用该映射。跨两个 Git 仓库及普通目录的真实内置工具/Shell 测试同时覆盖主目录和已隔离副本，子代理跨双仓库产出也通过。
- API 完整复验后补充 MCP 变更回退前 liveness 再校验、隔离丢弃与 Turn 提交竞争保护；对应 MCP 写入冻结回归及隔离 8 项定向回归通过。最后小改动之后没有再次把全部 API/daemon 库测试重跑，编译与格式/协议/Web 检查通过。

### 当前剩余工作与下一入口

1. P4 的 KnowledgeStore、知识工具和知识读取 API 已按项目/源目录/实际执行根寻址（证据见下方多源索引记录），daemon 启动也预热全部源目录。下一步继续替换 Git/隔离后的 legacy 后台刷新，完善文件级拒绝策略、符号链接替换、目录关系管理和自动上下文的多根/子代理图谱链路；完整收口前不开放多目录创建/编辑管理路由。
2. 项目共享和获授执行根的 AGENTS.md 已进入共用模型循环，具体证据见下方 P4 规则记录。子目录规则仍按访问路径由代理按需读取，未实现预先扫描整个项目；接下来应结合 GUI/真实任务验收核对该边界。
3. 历史共享依赖链接当前保留原副本和成果，并拒绝执行；需要明确迁移为独立内容的恢复流程。旧目录在新 state root 的可移植导入、成员变更与待审批/队列/未合入成果并发守卫仍需完成后才能开放项目管理。
4. P5 Web/GUI 的创建编辑、目录选择、文件引用与附件、按根文件树/变更/Git/隔离展示还未完成。最终 P6 仍需实际 daemon/GUI、打包宿主与平台验收。保留全部未提交改动和原用户修改，目标继续进行中。

### P4 规则作用域接线（2026-10-28）

- 新增 `magi-conversation-runtime::workspace_rules`，规则只从可信执行入口读取：项目共享 `AGENTS.md` 位于稳定项目状态根（由项目记忆 store 的固定根定位），源目录规则从本轮实际执行根读取；主要源目录不会替代其它目录的规则。
- 主代理、子代理和执行器首轮工具资源准备后的 prompt 更新共用同一加载函数。切换到隔离副本会重新读取对应副本规则；项目规则只作为 developer 指令补充，明确低于本轮用户输入、权限和安全规则。
- 规则文件必须是执行根内的普通文件，拒绝外部/悬空符号链接、目录、单文件超过 32 KiB 或总量超过 64 KiB 的内容；缺失规则不创建目录，也不把未获授目录的规则带入 prompt。只自动加载项目/源目录根的规则，子目录规则要求沿访问路径按需读取，不预先注入整项目文件。
- 回归：规则作用域、隔离执行根、外部/悬空链接、目录文件和超大文件测试通过；`conversation_loop` 相关定向测试通过，runtime 全库 678 项通过，`cargo check -p magi-daemon`、`cargo test -p magi-knowledge-store --lib`（105 项）和 `git diff --check` 通过。
- 代码索引仍需下一步按 `sourceFolderId + executionRoot` 演进 KnowledgeStore、知识 API 和工具返回；当前已接入的规则链路不代表 P4 或完整多目录能力全部交付。项目管理 HTTP/GUI 入口继续关闭。

### P4 多源知识索引与工具接线（2026-10-28）

- `magi-knowledge-store::KnowledgeStore::for_source_folder` 创建共享事实源上的只读索引视图；运行时引擎、watcher、构建状态、根路径和代码索引知识投影均按 `(workspaceId, sourceFolderId, executionRoot)` 分键。视图不拥有项目成员，也不能改写项目授权。
- 代码索引来源和推断关系携带 `CodeIndexScope`；图谱文件/符号节点使用源目录和执行根前缀，双目录的 `src/index.ts` 不会合并。旧单根入口继续使用未带 scope 的兼容视图。
- `search_semantic`、`code_symbols`、`knowledge_graph_query`、`knowledge_query` 在工具执行前从可信目录映射生成对应 KnowledgeStore 视图，结果带 `source_folder_id` 与执行根；未授权目录、隔离副本替换和根路径漂移会在统一工具入口拒绝。
- `/knowledge` 与 `/knowledge/graph` 接受 `sourceFolderId`；多源项目省略目录明确返回错误，目录离线返回失败状态且不会回落到主要目录。后台索引调度复用同一 scoped store，项目知识清空仍按项目事实统一收口。
- 新增回归覆盖两个源目录、同名文件、同一源的隔离副本、重载、目录撤销、工具授权和 HTTP 目录选择；`magi-knowledge-store` 106 项、`magi-tool-runtime` 318 项通过/1 项忽略、知识路由定向 13 项、`cargo check -p magi-daemon`、格式和 diff 检查通过。
- 自动知识上下文选择也接入目录身份和实际执行根，缓存包含二者；有冻结项目绑定但尚无实际目录映射时只注入项目共享知识，不从旧单根图谱推导其它目录事实。ContextRuntime 21 项与 conversation-runtime 678 项全库通过。
- daemon 启动预热已遍历活动项目全部源目录，复用上述索引视图；API 最新全库 1013 项通过、2 项忽略，daemon 137 项全库通过。随后仅调整图谱派生 edge 的前缀（保留持久关系 ID 便于管理），对应多源索引定向回归和 daemon 编译检查通过。
- P4 尚有明确后续工作：Git/隔离后的后台索引刷新仍有 legacy 单根调用；目录关系管理 API 尚未全部接受 scoped relation；文件级拒绝策略与索引快照/符号链接替换的读边界还需回归。自动上下文现只选择已准备的默认执行根，LocalAgent 尚未准备实际目录时不注入旧单根图谱，其余根的图谱按显式工具查询读取；后续应贯通完整实际映射。GUI、管理并发守卫、旧隔离副本迁移和平台验收继续沿原目标推进。

### P4/P5 索引刷新、关系作用域与 Web 项目管理（2026-10-29）

- Git 工具、项目注册和 Git context 接受后的后台索引刷新现在根据实际执行根反查项目源目录；隔离副本使用同一个 `sourceFolderId + executionRoot` scoped KnowledgeStore，无法归属的路径不再写入项目默认索引。
- 知识关系列表、创建、更新和删除接受 `sourceFolderId` 并使用相同 scoped store。关系 ID 不能跨项目或跨源目录重绑定；两个源目录可以拥有同名相对文件和各自独立的关系。Web 知识面板已在列表和 mutation 请求中携带当前目录。
- Web 工作台已经接入项目创建/编辑弹窗、源目录增删、主要目录选择、项目名称和 CAS revision 提交；项目行操作按钮在桌面和触控布局均保持可分离。文件树按源目录分组，输入区、文件引用、预览、终端、变更、Git 和知识面板沿当前目录选择提交 `sourceFolderId`。
- 本阶段定向验证：`cargo test -p magi-api --lib routes::knowledge::tests::knowledge_relations_keep_same_paths_in_distinct_source_folders`、`routes::knowledge::tests::knowledge_relation_routes_round_trip_with_workspace_scope`、`routes::workspaces::project_management::tests::create_and_update_project_preserves_source_ids_and_primary_selection`、`git_tool_runtime::tests::` 均通过；`cargo check -p magi-daemon`、`cargo fmt --all --check`、`git diff --check` 和 `npm --prefix web run check` 通过。
- 仍未宣称完整交付：历史共享依赖链接的独立迁移、跨平台安装包和外部服务验收尚无证据；项目管理 GUI 的真实创建/保存/重启流程仍需在独立 daemon 上做完整操作验收。

### P5 真实 daemon / Web 多目录验收（2026-10-29）

- 使用本 worktree 的 `./scripts/dev-daemon.sh` 在独立状态根 `/tmp/magi-project-multi-root-acceptance/state` 启动 daemon（端口 `38233`），Vite 由 daemon 托管；状态根、夹具目录和本机确定性 Provider 均未使用用户日常 daemon。daemon 重启后工作区注册表中的项目名称、三个源目录、稳定 `sourceFolderId`、主要目录和 revision 均恢复一致。
- 通过真实 Web 页面为三目录项目创建会话并启用整组隔离副本。任务实际读取三个同名 `src/index.ts`，分别得到 `a`、`b`、`p`；随后分别写入三份副本，并只在仓库 B 调用 `shell_exec` 写入 `shell-result.txt`。每次工具参数保留目标 `source_folder_id`，写入回执的 `changed_files` 也携带该身份，执行路径落在对应隔离副本；源目录在合并前保持不变。
- 真实审批恢复后，变更面板按「仓库 A / 仓库 B / 普通目录」分组显示三个同名文件和仓库 B 的 Shell 新文件；`合并全部` 后三个源目录分别得到 `MULTI_ROOT_0/1/2`，仅仓库 B 得到 `shell-result.txt`。同名相对路径没有串读或串写。
- 修正 Web 文件产物、变更卡片、右栏预览、HTML 预览和原生打开请求的 `sourceFolderId` 传播；文件变更在工具参数、卡片、右栏 code tab、Desktop context menu 和预览 URL 中保持同一目录身份。新增 golden 断言覆盖文件变更卡片保留目录身份。
- 本阶段追加：隔离库 21 项通过；API 隔离库 7 项通过（含旧共享副本恢复），历史分页绑定保留定向通过。此前本阶段通过：`cargo test -p magi-api --lib routes::workspaces::project_management::tests:: -- --test-threads=4`（5 项）、`routes::session_isolation::tests::multi_source_routes_keep_diff_approval_and_merge_on_the_selected_copy`（1 项）、`cargo test -p magi-tool-runtime --lib project_directory_tools_share_selected_cwd_permissions_and_write_claims -- --test-threads=1`（1 项）、`cargo test -p magi-knowledge-store --lib -- --test-threads=8`（106 项）、知识关系两个 scoped 路由回归（各 1 项）、`npm run protocol:check`、`npm --prefix web run check`（0 错误、0 警告）、`npm --prefix web run test:tool-call-display`、`npm --prefix web run test:file-preview-scope`、`cargo fmt --all --check`、`cargo check -p magi-daemon`、`git diff --check`。
- 仍需单独取得 Windows/Linux 安装包和跨平台文件系统证据；本机验收不能替代这些平台。外部真实模型、GPT Web、第三方 MCP 和插件服务未宣称通过；确定性 Provider 只验证执行协议、目录绑定、审批和恢复合同。历史记录中的旧共享依赖链接继续保留并拒绝执行，避免回落源目录；新隔离副本不再创建共享链接。

### 当前交付边界

P1–P5 的主要源码接线、双 Git + 普通目录真实任务及 macOS 独立 daemon/Web 多目录隔离、审批和合入路径已有通过证据，代码保留在未提交 worktree。全部阶段退出条件尚未完成，目标继续进行中；剩余工程工作和环境边界如下。

- 追加双 Git + 普通目录真实 Provider 验收：独立 daemon 中以项目创建 API 注册两个真实 Git 仓库和一个普通目录，App Server `turn/start` 通过冻结的 `sourceFolderId` 接纳任务。三个同名文件分别读写成功，Shell 只在 Git B 创建 `shell-result.txt`；两个 Git 仓库的 dirty 文件均保留，普通目录不获得 Git 语义，三处结果与执行目录均未串用。
- 修复迁移测试在 macOS `/var` 与 `/private/var` 别名下使用未规范化夹具根的问题；`cargo test -p magi-daemon --lib project_storage -- --test-threads=2`（3 项）及 daemon 全库（137 项）随后全部通过。


### 本轮最终回归与剩余工程入口

- 最新完整回归：API 1019 项通过 / 2 项忽略；conversation-runtime 678 项、orchestrator 66 项、daemon 137 项、session-isolation 18 项、knowledge-store 106 项通过。daemon 首轮出现的迁移夹具路径别名失败已定向修复并通过完整复验；保留失败来源与修复证据。
- 消息预览的缺省目录现从原 canonical 用户消息的 `projectDirectories` 或 `pendingDispatch.project_directories` 读取，显式工具目录覆盖该默认值；文件产物卡片、附件引用和工具卡片不跟随当前 UI 目录漂移。canonical / tool-call-display golden 回归和 Web 类型检查通过。实际页面打开仓库 A 文件产物后，预览内容为 `MULTI_ROOT_0`，下载 URL 仍携带仓库 A 的 ID，即使项目当前主要目录为仓库 B。
- 尚需完成的工程工作：旧源目录在全新 state root 重新注册时的可移植历史导入；源码剩余单根消费者清理；从本 worktree 构建的实际 macOS Desktop 安装宿主回归。旧共享依赖链接已经在隔离恢复入口提供带清单的独立迁移，源目录、现有副本成果、外部替换和未知暂存目录均有拒绝/保留回归；迁移登记通过同一 RuntimeStatePersistence 原子写入口保存。
- 跨平台与外部证据：Windows/Linux 文件系统、进程与安装包需对应平台，真实 GPT Web/第三方 MCP/插件服务和真实外部模型尚未验收，不能用本地 Provider 代替。
- 原测试项目「三目录页面验收改名」的 repo-a/repo-b 是普通目录别名；双 Git 事实来自另建的「双 Git 与普通目录验收」真实仓库夹具。两组证据分别记录，不把名称当作 Git 能力。

### 追加：可移植重注册、资源入口收敛与 Desktop 宿主（2026-10-29）

- 旧源目录在全新状态根重新注册后，启动迁移现在也识别已写成新格式的注册记录，沿同一事务把原 `.magi/session-projections` 导入固定项目存储，并从原快照建立 canonical events。只认保留的原源目录 ID；固定项目根一旦存在就不再扫描备份，防止删除的会话复活或覆盖新历史。项目创建入口从唯一旧源继承原项目身份，多个源目录带旧身份时明确拒绝合并；只读 identity 查询不会给新源目录写入 manifest。
- 可移植导入现在也接入注册/创建项目的 daemon 运行中路径：项目注册成功后，在同一 SessionStore 导航锁和 canonical barrier 下校验并追加历史；已有项目历史、当前指针、输入/turn/thread 身份冲突时拒绝且不覆盖。真实 daemon 回归覆盖注册后立即列出历史、创建带第二源目录并切换主要目录、随后创建新会话和重启恢复。迁移清单恢复时使用当前注册表，不能覆盖失败期间其它项目的新注册；runtime 与恢复/迁移共用状态写锁。
- 删除无 session 资源作用域的主要目录猜测与重复解析：diff、文件内容、下载和目录列表现在沿同一个源目录解析入口，多源项目省略 `sourceFolderId` 明确拒绝，返回 binding 保留选定源身份。
- 独立代理继续、主线继续和新任务共用冻结绑定到实际副本的 `bound_project_execution_root`，继续前校验当前成员并持有项目准入守卫。原主要目录切换后仍用原绑定，原成员撤销后明确拒绝；相关恢复断言通过。
- `npm run desktop:package -- --dir` 已从本 worktree 成功构建 macOS arm64 未签名目录包，并在独立状态根 `/tmp/magi-project-multi-root-desktop/state`、端口 `38234` 启动真实 Main 托管 sidecar。项目改名、切换主要目录、退出与重启后名称、两个源目录 ID、主要目录和 revision 一致。随后本地确定性 Provider 在该宿主的隔离会话中分别读写两个同名 `src/index.ts`，只在 B 写入 `shell-result.txt`；原目录保持不变，任务 completed，变更面板按 A/B 分组显示 3 个文件。此轮目录包不构成签名、安装器及 Windows/Linux 的通过证据。
- 本轮通过：项目迁移 5 项、项目管理 6 项、canonical 可移植事件导入 1 项、资源路由 49 项及追加目录断言单项、冻结绑定恢复 1 项；daemon 全库 139 项。编译、协议、Web 检查和 Desktop 类型/构建链检查通过。构建日志和真实任务记录保留在 `/tmp/magi-project-multi-root-desktop/`；本轮所有改动仍未提交。
- 热导入后完整复验：API 1021 项通过 / 2 项忽略，daemon 140 项、session-store 184 项、workspace 34 项通过。之后只增加 shared write lock、项目读取锁和双入口导入夹具，daemon 热注册/项目创建/立即新会话/重启的定向回归、编译、格式、协议、Web 与 Desktop 检查通过；不把此前完整回归描述为这些最后改动后的全库结果。热导入初始测试曾因 fixture 的 current 指针和空会话隐藏规则失败，现已按真实快照/用户历史修正并通过。
- Desktop `合并全部` 已在真实宿主执行完成；A/B 原目录各自得到 `MULTI_ROOT_0/1`，Shell 产物只在 B，变更面板显示无待确认变更。该目录包构建早于热导入接线，热导入目前以 daemon 路由/落盘/重启测试为证据，后续最终包仍需重新构建。
- 目标继续进行中：剩余单根消费者审计与权威设计文档收口仍需推进；跨平台和真实外部模型、GPT Web、第三方 MCP/插件服务仍须对应环境证据。

### P6：遗留 cwd、GPT Web 图片与设计合同收口（2026-10-29）

- 沿实际调用方复查 `native_root_path` / `rootPath`：项目持久化按 ID 定位；剩余主要目录读取主要服务于展示、项目级导航、单目录旧入口及旧数据迁移。删除普通 Task 和 Goal 提交之前的 session-only 隔离/主要目录 cwd 猜测，统一由 `submit_dispatch_submission` 通过冻结绑定解析，并移除无调用的 `session_execution_root_override`、`session_isolation_root`。
- 发现并修复 GPT Web 回复图片仍写入当前主要目录的遗漏。`WebImageSink` 在实际发送前绑定原 canonical Turn、槽位代际、目录授权和实际执行根；保存前与 MCP 共用 owner/Turn liveness 校验并重新解析原副本。主目录切换不会移动图片；原目录、槽位或 Turn 撤销后不能写入，也不能回落个人目录。图片目录链接、已有链接/异内容目标均拒绝，原内容去重保留；不可用的图片作用域不阻止回复文字交付。
- 原 canonical 目录读取合并为 `canonical_project_directory_binding`，由 Goal、MCP 和图片入口共用。知识关系旧请求遇多源项目省略 `sourceFolderId` 时明确拒绝，避免产生不能定位目录的写入；单目录兼容与项目共享文本知识仍保留。
- 更新工程 README、个人会话、Git 工作流、会话隔离及知识图谱文档，补齐稳定身份、状态根、冻结授权、仓库整组租约、逐目录结果和旧共享副本恢复合同。阶段计划仍记录原退出条件，不能把设计说明当作额外验收证据。
- 实际复验：API 全库 1023 项通过 / 2 项忽略，daemon 全库 140 项通过，Web 模型库 63 项通过；新增图片归属/隔离/撤销/迟到/链接回归 2 项通过，知识关系多根省略身份拒绝断言通过。`cargo check -p magi-daemon`、格式与 diff 检查、App Server 协议检查、Web 检查（0 错误、0 警告）及 Desktop 类型/构建链检查通过。API/daemon 全库后又增加图片执行根规范路径的拒绝检查和知识省略身份断言，对应定向回归通过；不把之前完整回归记为最后检查后的全库结果。
- 最新代码已重新构建 macOS arm64 Desktop 目录包：Electron 45.0.0 / Chromium 156.0.8078.48，使用本机已校验 SHA-256 的 Electron 缓存，构建日志为 `/tmp/magi-project-multi-root-desktop/package-cached-oct29.log`，目录包位于 `target/electron-dist/mac-arm64/Magi.app`。在全新状态根 `/tmp/magi-project-multi-root-desktop-final/state`、端口 `38236` 启动后，真实 Main 托管 sidecar 的 `/health` 返回 `status=ok`、构建身份 `907954b...`，`/api/workspaces` 初始为空，可信 Renderer 加载完成；随后已停止该临时宿主。该证据覆盖最新目录包启动和 daemon 托管，不等于签名/安装器、Windows/Linux 原生行为或真实外部服务通过。
- 目录包第一次重建尝试因 electron-builder 下载 Electron 时 TLS 断连失败，日志保留在 `/tmp/magi-project-multi-root-desktop/package-final-oct29.log`；使用本机缓存重试后通过，不能将第一次失败隐藏为成功。

### 最新验收对照（2026-10-29）

此表汇总上文已经执行的检查，避免把早期阶段的待办误当作当前状态。它不将未经独立记录的场景补记为通过。

| 计划验收项 | 当前证据与边界 |
| --- | --- |
| 旧项目与个人会话迁移 | daemon 落盘、迁移重试、固定存储、canonical 历史和注册后热导入回归通过；原文件保留 |
| 双 Git + 普通目录 | 独立 daemon 的真实 Git 夹具与确定性 Provider 完成三根同名文件读写，dirty 文件保留 |
| 同名文件定位 | 工具、变更、按目录合入和 Web 实际预览通过；产物预览不随当前主要目录漂移 |
| 改名与主要目录切换 | 最新 Desktop 目录包以已有隔离验收状态根启动并重启，名称、两个源目录 ID、主要目录及 revision 恢复一致；证据为 `final-workspaces.json`、`desktop-final-restored-oct29.log` 和 `final-health.json` |
| 成员编辑与任务/审批 | 管理入口守卫、队列冻结、审批后撤销与零后续副作用已有定向执行回归；未将其称为所有真实多窗口并发组合通过 |
| 物理目录重复与重叠 | 规范路径、项目内/跨项目重叠、链接别名及缺失目录已有领域与资源回归 |
| 多仓库并发 | 整组租约回归通过；实际 DMG 安装应用中，多会话争用双仓库、自动整组隔离、等待者取消和持有者停止后资源交接通过，见文末并发记录 |
| 隔离与子代理 | 多源实际执行映射、真实 Git 子代理多仓库产出/重载及逐目录合入回归通过；Desktop 隔离任务和合并已实际运行 |
| 部分失败与旧副本 | 整组初始化/登记失败清理、独立迁移、成果保留及未知目录拒绝回归通过；不伪造跨仓库原子成功 |
| 平台与路径 | macOS 本机执行、DMG 挂载/复制安装/双仓库任务、linked worktree、符号链接和普通目录已有证据；Linux core/workspace/isolation 原生回归及 daemon 140 项通过；Windows 原生行为与 Windows/Linux Desktop 安装包未验证 |
| 外部入口与规则 | GPT Web 真实账号 19/19、第三方 Docker MCP + 真实模型三目录任务、插件 Worker/MCP 167/168（最后的非回环网卡连接超时）已有分项证据；本机兼容 Provider 不代表外部模型账号，真实模型自主选择目录仍不作外部服务承诺 |
| 恢复与交互 | daemon/最新 Desktop 重启、冻结任务/队列/审批恢复及 390px 项目设置布局有证据；真实触控、完整键盘流程与多窗口 SSE 一致性尚无完整独立验收记录 |
| 移除与删除 | 原移除结算和固定历史/源文件/成果保留有回归；未将所有真实外部服务吊销组合记为通过 |

- 最新补充检查通过：`cargo check --workspace`、`npm run protocol:check`、`npm --prefix web run check`（0 错误、0 警告）、`npm run check --workspace @magi/desktop` 和 `git diff --check`。
- Windows 目标补充：`cargo check --target x86_64-pc-windows-msvc -p magi-core -p magi-workspace` 通过。daemon 与包含 snapshot 的隔离模块交叉检查被本机缺少 Windows SDK/C 运行库头文件阻断，`aws-lc-sys` / `zstd-sys` 报缺少 `windows.h`、`stdlib.h` 等；日志保留为 `/tmp/magi-project-multi-root-desktop/windows-domain-check.log`。该失败发生于原生依赖构建，既不是业务 Rust 编译通过，也不能证明 Windows 实际运行失败。
- 当前 worktree 仍为起点 `907954b15409faaafe4e0ba7f02783d88c674e03` 的 detached HEAD，未提交。保留原工作树带入的无关改动；验收状态与日志位于 `/tmp`，没有新增仓库验收脚本。未执行签名、Tag 或发布。
- 用户已授权跳过 Windows 原生与 Linux Desktop 安装包；真实 GPT Web、第三方 MCP/插件和三目录真实模型执行证据已在文末追加。剩余未验证边界为 Windows/Linux 安装物、触控设备及真正外部模型账号（本机兼容 Provider 不计入），不再以 macOS 或本地 Provider 替代这些证据。

### 追加：多源导航路径与真实项目设置验收（2026-10-29）

- 修复 `ApiState::workspace_id_for_root_path` 的遗留单根匹配：导航、事件流和兼容 `workspacePath` 请求现在匹配项目全部当前源目录，项目身份仍由注册表唯一拥有；执行 cwd、授权与 `sourceFolderId` 不从路径匹配推导。新增回归覆盖主目录切换、次目录定位和成员移除后的失效。
- 在独立 daemon（端口 `38237`，状态根 `/tmp/magi-project-multi-root-ui-final/state`）通过真实 Web 项目设置弹窗将三目录项目从 A 切换到 B 并保存。页面显示项目名称、三个目录、主要目录；随后重载 daemon，`restart-workspaces.json` 保留项目名、三个稳定目录 ID、B 主要目录和 revision=2。
- 同一运行态对 A、B、Plain 三个 `workspacePath` 请求均返回同一项目身份；B 的响应主要目录为 B，A/Plain 仍只用于项目识别，不改变默认执行根。该证据补齐项目设置保存、主要目录切换、刷新/重启恢复和次目录路径导航的真实链路；不替代窄屏触控、多窗口 SSE 竞态及跨平台验收。
- 新增检查：`cargo test -p magi-api --lib workspace_navigation_path_tracks_all_current_sources_after_primary_change -- --test-threads=1` 通过；`cargo test -p magi-session-isolation --lib -- --test-threads=4` 21 项通过；项目迁移 5 项、知识关系和项目管理定向回归各 1 项通过；修改后 `cargo check -p magi-daemon`、`cargo fmt --all --check`、`git diff --check` 通过。
- 在同一 daemon 的独立 IAB 页面把视口设为 `390×844`，项目设置弹窗完整显示三个源文件夹、主要目录单选、目录别名、添加/移除、取消和保存按钮；截图与 DOM 均确认无水平溢出（弹窗宽度 390、文档滚动宽度 390）。随后恢复默认视口并关闭临时标签。该证据覆盖窄屏布局和键盘可达控件的可见结构，不宣称真实触控设备手势通过。
- 使用本机 OrbStack 的 `rust:1.97.0-bookworm` 容器、只读 worktree 挂载和独立 Cargo target volume 取得 Linux 原生目录边界证据：`magi-core` 50 项、`magi-workspace` 34 项和 `magi-session-isolation` 21 项全部通过。日志分别保留在 `/tmp/magi-project-multi-root-linux-domain.log` 和 `/tmp/magi-project-multi-root-linux-isolation.log`；这验证 Linux 文件/链接/路径语义，仍不等于 Linux Desktop 安装包或 GUI 进程验收。
- 同一 Linux 容器 `cargo check --locked --offline -p magi-daemon` 通过，daemon 项目存储迁移/冲突恢复/可移植导入/主要目录切换 5 项回归通过；日志为 `/tmp/magi-project-multi-root-linux-daemon.log` 和 `/tmp/magi-project-multi-root-linux-daemon-tests.log`。macOS 修改后的 API 全库为 1024 项通过、2 项忽略；Web 导航、Turn 导航、目录引导和文件预览 golden 均通过。
- 当前 worktree 重新构建的 macOS arm64 目录包也已通过：直接使用已校验 Electron 缓存目录 `/Users/xie/Library/Caches/electron/f22ed746d6afe4413723c1ec95408e33d6635bf7d817a70edb1df9220a236ab0`，日志为 `/tmp/magi-project-multi-root-desktop/package-current-cached-oct29.log`，产物为 `target/electron-dist/mac-arm64/Magi.app`。在端口 `38238`、全新状态根 `/tmp/magi-project-multi-root-desktop-current/state` 启动后 `/health` 返回 `status=ok`、当前 build identity，`/api/workspaces` 为空，可信 Renderer 加载完成；已停止临时宿主及 sidecar。普通 `npm run desktop:package -- --dir` 的首次重试因 TLS 断连失败，日志为 `/tmp/magi-project-multi-root-desktop/package-current-oct29.log`，不能把它记为成功。
- Linux daemon 全库第一次容器运行因测试辅助进程找不到 `rustc` 命令而出现 11 项桥接 smoke 失败；补齐容器内 `RUSTC` 后重跑，140 项全部通过，日志为 `/tmp/magi-project-multi-root-linux-daemon-full-retry.log`。因此 Linux daemon Rust 级回归已全绿，但仍不等于 Linux Desktop 安装包或 GUI 进程验收。
- 在端口 `38237` 的独立 daemon 上打开两个 IAB 客户端：客户端 A 通过项目设置弹窗用键盘修改项目名称并保存，客户端 B 的侧栏在无刷新下同步显示新名称和相同主要目录；随后停止并重启 daemon，`final-sync-workspaces.json` 保留新名称、三个稳定源目录 ID、B 主要目录和 revision=3。该证据覆盖项目 registry SSE/跨窗口投影同步、键盘编辑、重启恢复；未宣称复杂并发冲突或真实触控手势通过。
- 同一状态根再进行一次 daemon `SIGKILL` 后重启，重启前后的 `/api/workspaces` 投影字节一致（`reconnect-before.json` 与 `reconnect-after.json`），确认 registry 事务落盘不会因进程强杀丢失项目名称、成员或主要目录。
- 追加 CAS 过期修订回归：对同一 revision 连续提交 `CAS Winner` 与 `CAS Loser`，第一项返回 200/revision=4，第二项返回 409 `项目已变化，请刷新后重试`，最终注册表只保留 Winner 和完整三目录成员集合；这不是同时写入竞态的替代证据。
- CAS 回归后的真实页面重连：客户端保持打开时强制停止 daemon，再以同一状态根重启；页面自动恢复侧栏，显示 `CAS Winner`，项目设置弹窗重新打开并保留三个目录、B 主要目录和 revision=4。临时标签及 daemon 已清理。
- 本轮最后检查通过：`npm run check --workspace @magi/desktop`、`npm --prefix web run check`、`npm run protocol:check`、四个 Web navigation/onboarding/file-preview golden、`cargo fmt --all --check`、`git diff --check`；临时 daemon 已停止，未留下监听端口。
- 最后补充的前端修复让工作区选择回退路径也能匹配任一 `sourceFolder.path`，不再只比较主要目录的 `rootPath`；`npm --prefix web run check`、workspace/session navigation golden 和 Web 构建通过。真实页面用次目录 A 的 `workspacePath` 打开后，地址栏规范化为所属项目 ID 和当前主要目录 B，项目侧栏和输入区正确绑定 `CAS Winner`，默认执行目录仍为 B。
- 基于当前前端修复重新构建 macOS arm64 Desktop 目录包（日志 `/tmp/magi-project-multi-root-desktop/package-ui-path-final.log`），在端口 `38239` 全新状态根启动；`/health` 返回 `status=ok`、Renderer 加载完成且 `/api/workspaces` 初始为空，随后已停止宿主。该目录包仍是未签名目录包，不构成安装器或其他平台证据。

### 追加：Desktop 断线与导航归属边界（2026-10-29）

- Desktop 开发入口会把 `MAGI_PORT` 同步到 `MAGI_DESKTOP_DAEMON_PORT`；独立端口的 daemon 不会因默认值重新绑定到 `38123`。Web 页面已经把当前 daemon origin 视为实例绑定：断线探测只访问页面来源，不回退到 localStorage、查询参数或默认端口，避免两个状态根之间串读会话和项目。
- 项目身份确认和在途导航比较只以稳定 `workspaceId` 为权威；主要目录变化只更新默认执行路径，不使同项目同会话的正确 bootstrap 变成迟到响应。仍要求会话 ID 与 scope 完全匹配，其他项目或会话继续拒绝。
- Desktop 文件上下文菜单及文件夹定位请求保留原 `sourceFolderId`、session 和项目绑定，右键操作不会从当前主要目录重新推导历史文件所属目录。
- Composer 上下文引用的去重键现在包含 `sourceFolderId`，两个源目录的同名相对文件可以同时引用；生成图片的原图 URL、预览事件和右键操作也保留同一目录身份。
- 新增/更新 golden：`agent-api`（断线不跨端口）、`session-navigation`（主要目录切换）、`personal-session`（次目录/路径引用导航）和 `desktop-context-menu`（源目录定位）；均通过。`npm run test --workspace @magi/desktop`、`npm run test:electron-right-pane-links`（20/20）、`npm --prefix web run check`、受影响 Web golden、`npm run check --workspace @magi/desktop`、`npm run protocol:check`、`cargo check --workspace`、`cargo fmt --all --check` 与 `git diff --check` 通过。
- Web 全量 golden（`npm --prefix web test`）在同步动态文件树、Git 源目录键和 slash/context 引用断言后全绿；最终目录包再次构建，真实 Desktop 导航回归记录在 `/tmp/magi-project-multi-root-desktop/navigation-final-result.json`，使用端口 `38953` 验证 A 请求在 daemon 切换到 B 后仍提交同一项目，并对同一 revision 的并发更新得到一项 200、一项 409，成员集合保持完整。
- 真实双仓库并发验收记录在 `/tmp/magi-project-multi-root-desktop/concurrency-live-result.json`：同一项目两处 Git 仓库由一个会话同时持有；两个后续会话都显示明确阻塞者，停止其中一个后没有迟到写入，停止持有者后剩余会话同时取得两处仓库并完成；另一个会话因争用自动建立两份 `origin=contention` 隔离副本，两个原目录均未被写入，副本内同名文件分别完成。等待列表最终为空，Renderer 无运行时错误。
- 已构建真实 macOS arm64 DMG/ZIP（`target/electron-dist/Magi-3.0.61-mac-arm64.{dmg,zip}`），将 DMG 挂载后复制到临时安装路径启动，独立状态根和端口 `38957` 的 `/health` 返回 `status=ok`、版本 `3.0.61`、当前 build identity；运行日志和响应保留在 `/tmp/magi-project-multi-root-desktop/dmg-runtime.log`、`dmg-health.json`。该证据覆盖 macOS 安装物挂载、复制和 sidecar 启动，不代表签名、公证或 Windows/Linux 安装包。
- DMG 复制安装后的应用继续通过上述九项双仓库并发验证（`dmg-concurrency.log`）。另通过真实项目弹窗和目录选择器创建三个目录的项目，其中两个目录同名 `shared`，指定 B 为主要目录；移除 B 时保存按钮禁用，显式改选普通目录后保存成功，剩余目录 ID 保持，原 B 文件保留，改名/成员/主要目录在 Renderer 重载后恢复。七项交互断言通过，记录为 `project-ui-installed-result.json`；其后的可选 `Page.captureScreenshot` 超时，不能把该脚本整体退出码记为成功，也未生成截图。
- 追加定向检查：Git coordinator 四项、session isolation HTTP 三项、Desktop 构建链/TypeScript、Web 类型检查、协议与 diff 检查通过。没有业务源码变化，未重复全量 Rust 或 Web 套件。已配置的 `xiatian-serv` SSH 连接被远端关闭，尚未获得对应平台 Desktop 安装验收环境。
- 当前 worktree 仍 detached、未提交；目录包构建日志保留在 `/tmp/magi-project-multi-root-desktop/`。这些检查不增加 Windows/Linux 安装包、真实外部模型/GPT Web/第三方 MCP 或触控手势的证据，原验收边界继续有效。

### 追加：MCP 固定目录投影收口（2026-10-29）

- 修复 MCP 设置页仍把令牌显示成当前主要目录的单根投影的问题。令牌 DTO 的 `sourceFolderIds` 和 `defaultSourceFolderId` 现在由 Web 类型完整消费；创建令牌明确提交签发时的全部源目录及默认目录，列表、配置和远程客户端展示固定授权目录。外部审批托盘保留所属项目，去除会误指向当前主要目录的路径。项目后续新增目录不会被 UI 误显示为既有令牌权限。
- 更新中英文文案，明确令牌目录集合和默认执行目录的固定边界；旧脚本断言改为检查授权目录和不扩权说明。`npm --prefix web run check`、`npm --prefix web run build`、MCP server golden、i18n golden、`npm run protocol:check` 均通过。
- 本次仅收口投影与输入合同，未改变 daemon/MCP 权限拥有方；Windows/Linux 安装包、真实第三方服务和真实模型仍按最新验收对照表单独标注。
- 使用本 worktree 构建的最新 macOS arm64 Desktop 目录包，完成七项真实 GUI 检查：创建表单显示 A/B 及默认 A；拦截已构造的创建请求、先由真实 daemon 新增 C 并将主要目录改为 B，最终令牌仍只有 A/B 且默认 A；列表不显示 C；移除 A 后原授权显示不可用并保留默认 A；Renderer 重载后仍一致，运行时错误为零。记录为 `/tmp/magi-project-multi-root-desktop/mcp-directory-ui-result.json` 和 `mcp-directory-ui.log`；临时应用和 daemon 均已停止。
- 最新目录包通过原构建链生成；常规打包的 Electron 下载再次因 TLS 断连失败，改用本机 `node_modules/electron/dist` 的 Electron 45.0.0 发行物后完成解包自检，日志为 `package-mcp-ui-cached.log`。此前 DMG 验收对应前一版 UI；本次新增 MCP UI 使用最新目录包验证，不把旧 DMG 记为包含此次修复。追加 Web/桌面类型检查、i18n/MCP golden 及 diff 检查通过，未重复无源码变更的 Rust 全库测试。
- 用户随后明确没有 Windows/Linux 实体测试机，允许跳过 Windows 原生与 Linux Desktop 安装包验收。这两项在本次范围内不再阻塞收口；原平台记录仍为未验证。真实第三方服务、真实外部模型和触控设备的证据边界保持明确，未据此补记为通过。

### 追加：真实 GPT Web 双目录验收（2026-10-29）

- 使用本机已登录的 `persist:magi-web-model` 分区副本、最新 macOS arm64 Desktop 目录包、一次性状态根和独立端口运行 `scripts/verify-electron-gpt-web.mjs`。修复验收脚本旧的模型菜单文本定位，改用 daemon 投影的 `[data-magi-web-engine-row]`；真实 ChatGPT 页面探测返回 `status=ok`，未使用本地 Provider 处理 GPT Web 对话。
- 真实流程 19/19 通过：双目录项目创建、GPT Web 原生 webview、登录探测、固定入口、临时会话、真实文字回复、红/蓝两张图片识别、图片暂存清理、唯一项目会话、canonical 中保留两个 `sourceFolderId` 和原主要目录、插件停用与资源释放、Renderer 无错误。结果为 `/tmp/magi-project-multi-root-desktop/gpt-web-real-final2-oct29.result`，日志为 `gpt-web-real-final2-oct29.log`。
- 本机可用的真实模型环境是已配置的本地兼容 Provider（`localhost:8317`，23 个模型，当前默认模型可用）；它只用于普通模型/列表支持，不能替代外部模型自主选目录的证据。当前 MCP 配置存在 `sequentialthinking` Docker stdio 和 `computer-use` 本地 stdio，但本轮没有在独立多根 Desktop 验收中启动并执行第三方 MCP；该项仍明确为未验证。
- 随后在同一最新目录包中以已配置的真实 `sequentialthinking` Docker stdio 服务和已配置 Provider 运行三目录任务。真实模型先调用 `mcp__sequentialthinking__start_thinking`，再读取 A/B/Plain 三份同名文件并分别写入逐目录 `receipt.txt`；三份内容与原文件逐字一致，canonical Turn 完成且 `pendingDispatch.project_directories` 保留三项授权。第一次尝试因模型抄错长目录 ID 被权限边界拒绝并停止，保留为失败证据；第二次在提示中提供实际稳定 ID 后四项业务检查通过，结果为 `/tmp/magi-project-multi-root-desktop/real-model-mcp-retry.log` 与 `real-model-mcp-result.json`，Renderer 无错误。这样取得了真实模型与第三方 MCP 的多根执行证据，不把本地确定性 Provider 当作 GPT Web 或第三方 MCP 替身。
- 最新 macOS arm64 Desktop 目录包上，插件系统真实验收最终 `167/168`（`plugins-current-final3-oct29.result`）：本地插件 Worker 工具、stdio/HTTP MCP、审批、取消、令牌吊销、外部会话、项目移除/恢复、历史保留、插件升级与停用清理均通过；最后的非回环网卡访问超时（192.168.31.168），不能记整套全绿。早期 `135/135` 是失败前已执行断言摘要，后续仍超时，不是脚本成功退出。脚本已按现有共享控件更新开发规范返回按钮、MCP 选择器、拆分文件名文本读取，并明确等待外部活动段结算后再移除项目。
- 当前“真实模型”证据使用本机配置的兼容 Provider（`localhost:8317`，默认 `gpt-6-luna-101` 可列出）；该 Provider 的上游账号/模型身份没有独立外部服务证明，因此不把它表述为第三方模型服务验收。GPT Web 真实账号链路和第三方 MCP Docker 服务已有独立证据。

- 真实模型第二次运行的脚本在 canonical 断言误读 `metadata.projectDirectories` 而非 Task 路由的 `metadata.pendingDispatch.project_directories` 后退出失败；保留原失败日志。随后对原 canonical 记录和磁盘结果的六项独立复核全部通过，证据为 `real-model-mcp-post-run-audit.json`。未重新请求模型，也不将原脚本退出码改写成成功。

### 追加：多源 UI 规范修正（2026-10-29）

- 对照主工作区 `docs/ux-ui-guidelines.md` 修正本功能 UI。当前 worktree 尚无该规范文件，未修改主工作区。原问题是功能验收覆盖了目录绑定和保存，但未同步检查入口、键盘、触摸热区与目录辨识。
- 恢复侧栏「选择文件夹」直接打开既有目录选择器与原注册/导航流程。项目名称、成员和主要目录编辑改为所属项目行下的内联表单，不再新增项目设置 Modal；复用共享设置样式、按钮、输入框与 SelectField。移除主要目录后显示明确说明，保存保持禁用，要求显式选择替代目录。保存/取消后焦点回到原编辑按钮；添加目录选择器关闭后回到添加按钮。
- 执行目录保留向上弹出的选择器，超过十项提供名称/路径搜索；方向键、Home/End、Enter、Esc 与焦点返回已接通。浮层按可见视口横向校正，保持既有弹出方向；390px 实测曾发现右侧超出12px，修正后范围为58–378px。过滤不改变当前目录，选择仍只更新新输入的视图选择，不改 daemon 的在途绑定。
- 项目设置、文件树分组和目录选项同时展示名称与实际路径；路径使用 LTR 隔离并允许换行。触控能力查询下目录选择、项目操作与文件树行使用至少44px实际热区，工具栏空间不足时换行；操作间8px间距，侧栏移除操作与新建/编辑分开。图标视觉12–16px，44px图标容器含透明padding，未添加覆盖相邻按钮的伪热区。点击按下有即时底色反馈。
- 真实独立 daemon 验收：`38240`，状态根 `/tmp/magi-multi-root-ux/state`，Vite由 `scripts/dev-daemon.sh` 托管至3040。IAB已验证同名双目录显示、390×844无横向溢出（文档390px、编辑器285px）、目录方向键/Enter选择和焦点返回、十二目录搜索仅返回目标项及Esc关闭、项目改名与主要目录切换保存返回成功、移除主要目录后保存禁用。截图 `/tmp/magi-multi-root-ux/screenshots/project-settings.jpg`。临时标签及daemon已停止。
- 当前检查：Web类型检查0错误/0警告；workspace-onboarding、session-navigation、personal-session、file-preview-scope、panel-layout及i18n golden通过；`git diff --check`通过。未将源码样式或390px桌面视口记为真实触摸设备通过；RTL、200%缩放、德语/泰语等文字压力和软键盘真机专项未在本轮完成，不宣称全界面已合规。代码保持未提交。

### 提交前整理（2026-10-30）

- 按源目录身份复查了项目绑定、执行根、知识索引、变更投影、文件预览、终端、MCP/GPT Web、文件引用和右栏页签；删除/替换了沿当前主要目录猜测执行根的旧路径，保留旧格式仅用于单向迁移与明确兼容入口。项目成员、任务、审批和令牌没有新增第二套事实源。
- 修复多目录同名文件的右栏差异页签键和同步逻辑：`sourceFolderId + filepath` 作为页签身份，目录 A 的更新不会清除目录 B 的同名文件；对应 Web golden 已加入回归。修复严格 Clippy 发现的新增告警，并补齐 Skill 测试夹具的 `project_directory_map` 字段。
- 用户后续确认的最终 UI 替代上节的早期内联编辑：新增/编辑共用 Modal；输入区按目录数量显示项目及独立工作目录入口，两个浮层宽度基准为 280px，路径单行省略并保留完整提示，不重复解释性文案。已清理前端变更面板中重复的自定义按钮样式，统一使用共享 `.btn` 令牌；按钮颜色和圆角从皮肤令牌派生。项目同步时清理已移除项目/目录的页面选择，保留会话间独立选择。golden 使用独立临时 Vite 缓存并在退出时清理，不干扰正在运行的 daemon 页面。
- 最终检查：`cargo fmt --all --check`、`git diff --check`、`cargo check --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`、Rust 受影响库全量测试（API 1024 项、conversation-runtime 678 项、daemon 140 项、knowledge-store 106 项、tool-runtime 318 项、session-store 184 项等）、`npm run protocol:check`、`npm --prefix web run check`（0 错误/0 警告）、Web 全量 golden、`npm run check --workspace @magi/desktop` 与 Desktop 测试（160 项）均通过。首轮 Rust 测试有 5 个知识工具回归失败，已修正 macOS 路径别名规范化及相关夹具，并由后续全量复验覆盖。全库复验后仅有 Clippy 等价整理和测试夹具补齐，Git/知识/Skill/工具库、snapshot lifecycle 及 GPT Web 图片/槽位定向回归再次通过；不把较早的完整回归描述为最后一次编辑后的全库结果。
- 当前工作树仍未提交；保留 `.github/releases/v3.0.61.md` 等起点已有改动和全部源码/文档 diff，未生成新的临时验收脚本或提交。Windows 原生、Linux Desktop 安装包、真实触控设备、RTL/200% 缩放及完整外部模型账号仍按前文边界标记为未验证。

后续可独立优化、但不阻塞本次代码收口的内容：

- `WebWorkbenchShell.svelte` 的项目表单、`InputArea.svelte` 的项目/目录菜单可按现有组件边界抽取，减少大文件的维护成本；应保留统一的 daemon 提交入口和页面状态拥有方，避免抽取时复制请求逻辑。
- 代码检索的逐文件授权检查可在同一次查询内复用已构造的读取策略，减少重复规范路径检查；不能跨请求缓存授权结果，也不能绕过读取前的链接/权限复核。
- 完整 RTL、200% 缩放、长译文与真实触控/软键盘交互适合作为独立 UI 验收；本轮的静态检查和桌面窄视口不能替代这些证据。

日志集中在 `/tmp/magi-precommit-*.log`；最终右栏多目录回归、格式/diff、Web 类型检查和全目标严格 Clippy 已再次通过。本轮未重新构建包含最终 UI 的安装包；此前安装物证据仍对应其原构建版本。当前用户检视页面可访问且本轮读取的浏览器错误日志为空。

### 最新 macOS 客户端复验（2026-10-30）

- 使用当前 worktree 原始打包入口 `npm run desktop:package -- --dir` 重新构建 macOS arm64 客户端；本次 Electron 下载、解包自检和 Main 托管 sidecar 均成功。产物为 `target/electron-dist/mac-arm64/Magi.app`，版本 3.0.61 / Electron 45.0.0。未签名目录包，不将它描述为本轮重建 DMG、签名或公证通过。
- 在一次性状态根与目录夹具、独立端口 `38978` / CDP `9378` 上执行真实客户端验收，最终 **41/41** 项通过：GUI 新建/编辑/改名、目录选择不提前注册、取消不保存草稿、同名源目录的完整路径、280px 项目/目录浮层、两个源目录同名文件的真实工具写入与独立差异页签、主目录移除后的显式替换、成员身份保留、源文件保留、单源目录自动处理主要目录并隐藏冗余目录入口、重启恢复均通过。任务使用仅限本机的确定性 Provider，不代表外部模型质量验收。
- 截图复查发现同名差异页签的悬停提示只含相对路径。已在原 `RightPane.svelte` 提示函数中补上该文件绑定的源目录路径，再经原入口重新打包；最终 41 项包含两个页签提示路径不同的真实 DOM 断言。文件身份和授权仍由既有 payload/daemon 提供，没有新增项目状态。
- 皮肤覆盖内置浅色/深色及带壁纸自定义皮肤的浅色/深色 × clear/translucent/immersive，共 8 组。主按钮文字/背景均不透明，实测最小对比度约 11.49:1；弹窗无横向溢出。已人工查看浅色、沉浸壁纸、390px 弹窗与目录面板截图。390px 验收还确认面板在视口内、Esc 关闭并回焦。该证据仍不是实体触控或软键盘验收。
- 新增提示修复后 Web 检查为 0 错误/0 警告，right-pane 与 file-preview-scope golden 通过。项目迁移/身份/冲突恢复定向 5 项再次通过，升级前备份与回退操作边界已补入工程 README。打包内的 199 个 Web 静态资源与当前 `web/dist` 一致，内置 daemon 与本轮 release 二进制一致；改动文件清单与 SHA-256 记录已保存，新增源码仍未暂存或提交。
- 记录位于 `/tmp/magi-final-client-acceptance/result.json`、`source-manifest.json` 和同目录截图；最终执行日志 `/tmp/magi-final-client-acceptance-complete.log`，构建日志 `/tmp/magi-final-client-package-tooltip.log`，迁移日志 `/tmp/magi-final-migration-tests.log`。早期脚本的主题夹具错误、重载/动画同步错误保留原失败记录，最终脚本已等待新文档与动画结束后再验收，不能把早期截图当作最终画面。临时客户端、sidecar 和 Provider 均已停止，测试端口已释放，用户已有客户端与数据未改动。
