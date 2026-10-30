# 脚本与验证入口

界面设计、文案、布局与交互验收同时遵循 [UX/UI 设计与开发规范](ux-ui-guidelines.md)的检查清单，按改动面选择检查及真实运行场景；多语言、触摸与视觉检查的结果不能由静态通过代替。

脚本的执行入口以各 workspace 的 package.json、Cargo target 和 .github/workflows 为准。本文区分离线检查、真实集成验收与发布，不把源码断言当作真实 Chromium 或 Provider 验收。

模型档位与切换：`npm run test:model-reasoning-live` 使用隔离状态、真实 daemon/App Server/Chromium 和本地 Provider，覆盖三/六档、强度实际参数、切模校正、无默认档位重置、设置页覆盖保存、草稿刷新与重启恢复。数据归属和离线回归见[模型推理能力与选择交互](model-reasoning-capabilities.md)；不调用 GPT Web 或用户的外部模型服务。

## 日常开发与构建

| 任务 | 入口 | 条件 |
| --- | --- | --- |
| Web + daemon 开发 | `./scripts/dev-daemon.sh` | daemon 托管 Vite，使用 http://127.0.0.1:38123/web.html；构建缓存清理规则见根 AGENTS.md |
| Web 生产资源 | `npm --prefix web run build` | 使用根 npm lockfile 安装依赖 |
| Desktop 构建/开发 | `npm run build` / `npm run desktop:dev` | 开发宿主连接已有 daemon；见 docs/README.md |
| 安装包 | `npm run desktop:package` | 见 release-process.md；不等于发布 |
| 协议生成与校验 | `npm run protocol:generate` / `npm run protocol:check` | App Server schema 为唯一来源 |
| Browser 工具目录 | `npm run browser-tool-catalog:generate` / `npm run browser-tool-catalog:check` | 从 browser-tool.schema.json 生成 |
| crate 架构图 | `npm run docs:architecture` / `npm run docs:architecture:check` | Cargo metadata 生成 docs/architecture.html；crate 注释和业务流程必须引用实际 workspace 成员；CI 发布预检执行一致性检查 |

## 本地定向回归

无项目会话收口：`npm --prefix web run test:personal-session`，真实 daemon + Chromium 验收用 `npm run test:personal-session-live`。覆盖文件/Shell/执行步骤、项目规则隔离、最近目录、文件预览下载、重启不重放和会话删除回收；只使用独立状态根与本地 Provider。

无项目工具完整矩阵：`npm run test:personal-tools-live` 在同一隔离 daemon/Chromium/App Server 中增加 41 种工具的实际调用与 Provider 回灌，涵盖完整文件操作、图片生成/查看、提问答复、后台进程读写/等待/停止、12 个 Git 工具（remote 为本地 bare 仓库）、Goal/Plan 与代理发送/上下文检索/答复/取消；基础场景另执行 `update_execution_steps`。确认子代理取消已结算且主线继续完成，重启不重放副作用。模型和图片 Provider 均为本地替身，不调用 GPT Web 或用户模型配置。

83 个内置工具按以下口径全量核对，不能把目录检查或模拟宿主当作真实页面验收：

| 工具范围 | 数量 | 验证入口和边界 |
| --- | --- | --- |
| 个人通用、Git、目标/计划、代理和提问 | 42 | `test:personal-tools-live` 的真实 Turn/执行器/本地持久副作用，包含原基础执行步骤场景 |
| `web_search` | 1 | `cargo test -p magi-tool-runtime --lib web_search_live_smoke_returns_json_payload -- --ignored`，真实网络搜索；外网可用性单独报告 |
| `browser_*` | 29 | `personal_session_routes_every_browser_tool` 全量验证无项目/无 cwd 的宿主路由；Worker `src/runtime.test.ts` 验证动作和诊断语义；`test:electron-browser-actions` 使用真实 Chromium 核对点击、输入、悬停、表单、拖拽、frame 和导航失效，其他诊断动作不宣称逐项真实网站验收 |
| 项目专用工具 | 6 | `search_semantic`、`knowledge_query`、`knowledge_graph_query`、`code_symbols`、`memory_write`、`agent_apply` 在个人模型目录隐藏，`personal_session_project_tools` 核对调用拒绝且无持久副作用 |
| 内部进程工具 | 5 | `process_launch/read/write/kill/list` 由工具库进程回归验证，不作为模型工具暴露；个人会话通过公开 `shell_exec` 的后台动作闭环验证 |

`cargo test -p magi-conversation-runtime --lib personal_session_catalog` 逐项核对全部 83 个工具在个人主线和普通子代理中的目录范围。代理补充回归 `agent_send_pending_to_running_race` 验证 Pending → Running 竞态只提交一次、持久失败无半写和无唤醒；API `personal_agent_cancel_preserves` 复用 daemon 的终态通知入口，验证普通子代理的 Killed 只释放子资源，不收口父 Turn。独立 Agent Turn 的既有终态仍由原绑定拥有。

无项目 Git 与工具边界：`cargo test -p magi-api --lib personal_` 覆盖多仓库基线、持久恢复、外部漂移 CAS，以及删除时仅回收干净自有 worktree、保留 dirty/占用资源和外部仓库；`cargo test -p magi-api --lib web_slot_mcp::tests` 经真实 MCP socket 验证个人 GPT Web 的读写、原会话审批与工具记录、Git 查询、权限限制及槽位撤销。`cargo test -p magi-tool-runtime --lib personal_git_permissions` 验证外部仓库及 linked worktree 元数据均在执行前授权；`cargo test -p magi-project-memory --lib personal_memory_write` 验证误注入项目存储时也不能写入项目记忆。上述 live 脚本另经真实 Turn 链路执行 Shell 建仓、Git 查询/切分支和双仓库隔离；Provider 为本地替身，不代表外部模型或已登录 GPT 网站验收。

- Web：`npm --prefix web run check` 和 web/package.json 中对应的 `test:*`。`test:transport` 包括实时传输、App Server 客户端、滚动协调和数据加载恢复回归；`test:data-loading` 单独验证临时失败、旧请求、插件修订失效和交互作用域切换。`test:notification-runtime` 覆盖后台目录、全局错误与切换会话后的异步错误归属。
- Desktop：`npm run check --workspace @magi/desktop`、`npm run test --workspace @magi/desktop`。
- Worker：`npm run check --workspace @magi/browser-automation-worker`、`npm run test --workspace @magi/browser-automation-worker`。
- Rust：`cargo check -p <package>` 和受影响的测试目标。Git 工作流用 `cargo test -p magi-git --test workflow`，在临时本地仓库验证分支/worktree，不依赖开发者路径或远端仓库。
- Browser 静态边界：`npm run test:browser-core`，同时检查 guest 注册、唯一控制链及下载清理生命周期。
- 协议与发行边界：`npm run release:guard`，包含生成物、Desktop Browser schema 和发行依赖检查。

提示词强化的真实集成验收运行 `npm run test:prompt-enhance-live`：独立状态根的 daemon 与真实 Chromium 点击，使用本地 HTTP Provider，覆盖个人/项目草稿与会话、强化回填与还原、调用失败/空响应保留草稿、重试及辅助模型未配置。该验收不连接外部模型服务，也不替换用户运行中的 daemon。

`npm test` 运行 Desktop、Worker 和 Web 套件，用于明确要求的整体回归或发布预检；日常只运行受影响的 test:*，不默认执行全套。脚本语法可用 `node --check <script.mjs>` / `bash -n <script.sh>` 检查，语法通过不能替代行为验证。

Rust 摘要、压缩或语言解析依赖升级时，运行 `cargo test -p magi-core -p magi-knowledge-store -p magi-snapshot --locked`。其中旧快照夹具固定由 zstd 0.13.3 生成的字节和内容哈希，验证升级后可读且重复存储不改写原文件；语言解析回归直接调用 AST 提取，不能靠正则回退掩盖语法或查询不兼容。

## 真实集成验收

| 入口 | 覆盖范围与前置条件 |
| --- | --- |
| `npm run test:data-loading-live` | 构建 Web、独立 daemon 和 Chromium、本地 Provider；验证折叠工作区目录预取、慢目录不阻塞、临时失败重试、后台会话更新、跨客户端工作区变化、daemon 重启恢复、文件树加载途中切换隐藏文件补读、Markdown/公式/代码高亮/Mermaid 实际渲染，以及设置页重新显示/focus 刷新、迟到响应隔离与未保存草稿保留；不接管用户已有进程 |
| `npm run test:electron-browser-viewport` | 已打包 Electron、独立状态根与本地 Provider；真实 AI 工具验证固定/自适应视口、窄电脑/手机横屏、DPR/触控、缩放点击、光标常驻/导航/折叠恢复、拖动与网页点击接管后自适应、交还、320px 工具栏/菜单边界和后台页面不改布局 |
| `npm run test:electron-browser-actions` | 从当前源码构建 Worker 与 SurfaceManager，在独立临时状态目录启动真实 Electron webview；验证敏感值、快照预算与视口内优先排序、未返回控件汇总、按 element_ref 缩小范围、点击、输入、焦点、遮挡、按键与滚动的动作证据、页面稳定等待及导航引用失效；需要本机图形环境。input 组在宿主窗口隐藏后有既有的时间预算（离屏 guest 长时间运行后调试器以 target closed 断开，HEAD 代码同样如此），新增检查放在 pointer 组 |
| `npm run test:electron-cursor-presentation` | 从当前 Main、Worker 和 Svelte 光标源码启动真实 Electron guest；验证移动完成确认先于原生点击和输入框焦点、原地点击、隐藏/减少动态效果，以及移动途中用户接管、取消、导航和迟到确认；使用本地测试页与独立临时状态目录，需要本机图形环境 |
| `npm run test:electron-cursor-desktop-live` | 当前源码的完整开发 Desktop + daemon + 原生 Worker，访问真实 Example/IANA、Wikipedia 和 Rust Book，验证对话触发工具、光标滑行先于原生点击、输入/替换/搜索、右栏折叠恢复、移动中接管和交还；使用独立状态目录和本地模型调度，依赖公网和本机图形环境。先构建 Web 与 Desktop JavaScript，脚本通过 `scripts/dev-daemon.sh` 启动当前 daemon；截图与日志默认保留在临时目录，可用 `MAGI_CURSOR_LIVE_ARTIFACTS` 指定输出目录 |
| `npm run test:electron-browser-network-policy` | 开发依赖 Electron 的网络边界验收；验证 iframe、fetch、重定向和 localhost 预览来源 |
| `npm run test:browser-core-live -- --session-id <id>` | 已运行 daemon 的真实会话/Tab；`--app-renderer` 使用已启动 Desktop 的可信 Renderer，`--lifecycle-regression` 覆盖生命周期。环境变量和可写操作开关见 scripts/verify-browser-core-live.mjs |
| `npm run test:daemon-input-queue` | 构建 Web 后，通过 `scripts/dev-daemon.sh` 启动独立 daemon、状态目录和本地 Provider；Chromium 访问 daemon 主入口，验收 task 补充、页面重载、下一轮队列消费、无重复用户消息，以及缺失 usage 时继续执行、实际用量统计和无预算的目标完成；同时覆盖 Web 显式转交原代理、停止父轮、下一轮主线、SIGKILL 重启后点击继续原代理及独立完成和详情展示，不接管用户进程、不使用真实模型。不是打包 Desktop 验收 |
| `npm run test:electron-conversation-dom` | 已打包 Electron 与脚本提供的本地 HTTP Provider；验证 DOM、首轮接纳期间草稿保留、排队与会话归属、停止和继续、后台 Job 事件等待/持久终态/真实文件结果、重启恢复及时序。脚本按当前平台解析目录包、Shell 写入和进程树；应用路径、端口及场景通过 MAGI_ELECTRON_DOM_* 设置，见脚本头部。CI Windows 门禁会构建 `win-unpacked` 目录包并上传 JSON/日志证据 |
| `npm run test:electron-plugins` | 已打包 Electron、独立状态根与本地 Provider；覆盖插件安装、授权激活、设置、隔离视图与资源同步、工具/命令、升级排空与数据转换、插件中心和停用；MCP 覆盖服务生命周期、完整工具目录、询问/开放模式、外部会话排序与审批、拒绝写入、变更账本、审计与内容留存、实际生成的 stdio 配置握手与文件读写、越界及错误令牌拒绝、HTTP 吊销。通过 MAGI_ELECTRON_PLUGINS_* 设置应用路径与端口 |
| `cargo test -p magi-plugin-system --test catalog_tool` | 用 `scripts/plugin-center.mjs` 发布工具签名目录，再由 daemon 的 `PluginCatalog::verify` 校验；需要 Node.js，缺失时跳过 |
| `npm run test:electron-gpt-web` | 已打包 Electron；复制本机已登录的 `persist:magi-web-model` 分区到一次性状态根，向真实 ChatGPT 发送文字和两张图片，验证收发、暂存清理与停用；`MAGI_GPT_WEB_MODE=saved` 验证已保存模式，默认临时模式。`MAGI_GPT_WEB_FRESH=1` 只验证需要登录；`MAGI_GPT_WEB_QUESTION_FIXTURE=1 MAGI_GPT_WEB_LAYOUT_CHECK=1` 验证受控问答与布局；`MAGI_GPT_WEB_REAL_QUESTION=1` 单独验证真账号原生卡片，不能以受控页面替代。未找到登录分区时真实账号场景跳过 |
| `npm run test:electron-browser-permission-matrix` | 打包 macOS Apple Silicon Electron 的 Browser 权限代表场景；需本机图形环境及空闲 daemon/CDP 端口 |
| `npm run test:electron-right-pane-links` | 已打包 Electron、全新状态根与本地 Provider；从右栏未挂载开始，验证链接点击展开、真实页面加载、同 URL 复用、会话绑定、GPT Web 激活时切换、关闭 Tab 和关闭会话后 Browser 资源结算 |

核心集成按改动触发，不接入日常 npm test。Browser actions 负责页面交互和快照，network policy 负责 Chromium 网络边界；DOM 入口负责消息、工具、审批、取消和重启恢复；Browser live 检查当前 Tab 的物理生命周期，权限矩阵验证真实工具授权，MCP 入口验证 HTTP/stdio 对外协议。

DOM 的确定性 Provider 失败场景使用 400，避免混入 5xx 的退避重试预算；并行可写子代理场景使用已提交的 Git 工作区，沿生产隔离规则创建真实工作副本。

Git 漂移/merge conflict 的 Desktop 专项使用 `MAGI_ELECTRON_DOM_GIT_PREFLIGHT_ONLY=1 npm run test:electron-conversation-dom`：普通对话必须到达 Provider，首次 `file_write` 才触发真实工作区准备；拒绝后无副作用，canonical 工具回执包含 `workspace_preparation_failed` 与 `execution_started=false`。恢复专项使用 `MAGI_ELECTRON_DOM_RECOVERY_ONLY=1 npm run test:electron-conversation-dom`，覆盖 SSE/WebSocket 重连、daemon SIGKILL 与完整 Electron 重启。代理验收必须展开真实卡片、核对 child task 并打开详情，不能用 Provider 最终文本替代卡片和任务事实。

DOM 的 recovery、Git preflight、approval expiry、compaction 场景可单独选择（MAGI_ELECTRON_DOM_*）。压缩场景包含历史预填充和 3 个连续 Turn，至少一次 completed；时序原始记录用于诊断。

发布预检仍执行一次 Rust workspace 全量测试；不再紧接着重复执行已覆盖的本机平台测试。CI 的 Windows/macOS 专项任务继续验证对应操作系统差异。

Windows CI 的多命令 PowerShell 步骤启用 `$PSNativeCommandUseErrorActionPreference = $true`，中间的 npm/cargo/Worker 失败必须立即失败该步骤。验收还须核对原始命令输出；仅有 Job 的绿色状态不能证明所有命令通过。插件清理回归应重载并读取当前包，避免只检查管理器打开成功却漏掉启动清理误删当前包。

## 性能测量

只在处理性能问题或明确比较收益时采样，先验证终态、幂等、权限、取消和恢复。后端与 Renderer 使用各自时钟，不拼接为总延迟；辅助模型调用、失败、取消、阻塞和恢复单列。

| 阶段 | 观察范围 |
| --- | --- |
| 接纳 | submit_received → accepted_response_sent |
| 本地准备 | accepted_response_sent → provider_request_started |
| Provider | provider_request_started → provider_first_raw_delta；首个可见文本单列 |
| 后端完成 | accepted_response_sent → canonical_terminal_published |
| Renderer | frontend_event_received → reducer_completed → projection_completed → dom_painted |

每条记录关联 session_id、turn_id、request_id、实际执行 profile、场景和 outcome；事件序号只用服务端事实，缺失阶段保持缺失。比较时记录源码、构建、模型参数、输入和历史形态。P50/P95 另定样本量与口径并保留失败样本，少量行为用例不能作为稳定 P95 的证据。本地 Provider 只证明 Magi 行为，上游可用性与延迟须独立测量。

## 文档与脚本维护

- 架构约束保留在对应设计文档；完成后的阶段计划、一次性进度/验收报告从活动文档移除，历史通过 Git 追溯。
- 尚未交付的产品方案、用户场景和协议规则仍需保留，并标明性质。版本发布说明是历史交付记录，不按日期删除。
- 删除脚本时同步处理 package.json、工作流、文档和调用方；无 package 命令不代表无效，真实集成脚本可能由开发者直接运行。
- 发布工作流仍调用 build-legacy-desktop-bridge.mjs 生成旧客户端升级迁移资产；它有现行发行合同，不能当作无人使用的旧宿主脚本删除。
- 替换测试时保留有效行为覆盖，删除重复的源码匹配和依赖个人环境的旧 fixture；不得通过删除失败断言掩盖产品问题。

### 原生模型 Step/attempt 回归

任务运行入口直接使用共同 Step 循环，已撤下旧 `Conversation`/`TurnDriver`/`Turn` 及其专用测试。输入 FIFO、异常关闭和单次派发分别验证实际入口：

- `cargo test -p magi-conversation-runtime --lib`：包含初始 assignment 和补充消息顺序、Provider panic 后同任务可重新进入、运行中信号与引导边界，以及八个 Runner 同时争用同一 Task 只派发一次。
- `cargo test -p magi-api --lib terminal_task_releases_undispatched_input`：代理尚未运行就被取消时清理初始消息，重复结算幂等且不影响兄弟代理。
- `cargo test -p magi-api --lib turn_harness::tests:: -- --test-threads=16`：夹具复用生产环境的异步终态收口及会话锁，覆盖真实 Task/Turn、子代理派发、取消与转交恢复，不绕过锁直接提交终态。

- `cargo test -p magi-session-store --lib model_step`：Step/输入消费原子性、不可变请求与恢复。
- `cargo test -p magi-session-store --lib partial_sidecar_flush`、`cargo test -p magi-daemon --lib removed_workspace_does_not_block`：按实际提交的 Session 确认 checkpoint；离线工作区保留脏版本、其他会话仍更新磁盘、重加后补交并可重启读取，写盘失败和快照期间的新版本不被误确认。
- `cargo test -p magi-daemon --lib daemon::persistence::tests`：包括旧检查点不能覆盖会话选择、撤销已保存的重命名或恢复已删除会话；刷盘期间继续接纳 canonical 工具结果并在重载后保留。SessionStore 原刷盘测试已改用生产环境的增量入口，撤下旧全量持锁入口及其专用阻塞测试。
- `cargo test -p magi-api --lib remove_workspace_checkpoints_before_deregister`：移除工作区先提交检查点，失败时保留注册与选择；成功时仅清除所属工作区的当前选择，会话历史保留。
- `cargo test -p magi-conversation-runtime --lib model_step`：共享调用边界、流中断恢复、transport 重试写盘失败。
- `cargo test -p magi-daemon --lib model_step_journal`：事件检查点恢复与 state layout 升级。
- `cargo test -p magi-api --lib turn_harness::tests::task_profile_`：主线/子代理、工具来源与排队补充的实际调用链。
- `cargo test -p magi-bridge-client --lib retry`：HTTP 流式/非流式重试与回调。

Step 的 `started` 在进程退出后表示发送结果未知，不能作为自动重试或重新执行外部副作用的依据。真实 UI 流程仍使用上文 daemon 输入队列脚本。

### 工作区按需准备与队列创建身份回归

- `cargo test -p magi-api --lib turn_harness::tests::`：实际主线 Provider/Task/Turn 链路；包含纯对话/读取不建快照、首次写入追踪、Git 漂移和冲突在执行前阻止、争用等待、隔离、审批/提问租约和 Continue。
- `cargo test -p magi-api --lib snapshot_lifecycle_install_defers_historical_sessions_until_access`：安装 observer 后的新建和历史 Session 都按需建立快照。
- `cargo test -p magi-api --lib queued_task_without_client_item_id`：原始 HTTP 请求省略 userMessageId，队列消费保留创建指纹、只出现一个用户 item，完成后原请求可幂等重放。
- `cargo test -p magi-api --lib goal_start_turn_failing_before_the_goal_exists_is_resubmitted_after_backoff`：Goal 自动重提补齐 item ID 后也能绑定原持久输入，不反复报身份冲突。

Git 漂移/冲突场景现在允许先请求模型，实际写入/Shell/Git/代理工具仍必须准备成功。旧的“Git 异常时 Provider 请求数必须为零”不再是产品合同；必须验证工具未执行与明确失败回执。

工作流共用 Step 的验证入口：`cargo test -p magi-conversation-runtime --lib workflow_execution`。覆盖持久 Step 成功/目标关闭拒绝、准备失败的明确未执行结果，以及授权引用与工具子集实际传入模型。宿主仍拒绝模型覆盖、越界引用和不在当前授权目录的工具；空工具列表不提供工具。模型发送前核对原待决动作包含完整请求与 Step 身份；失败后写盘/重载保留相同意图，再次运行不询问核心或重发 Provider。版本恢复另运行 `cargo test -p magi-api --lib workflow_acceptance_freezes_the_host_package_in_task_and_pending_request` 与 `cargo test -p magi-plugin-system workflow_checkpoint_cannot_switch_to_an_upgraded_package`，核对宿主固定摘要、原请求重放、磁盘恢复及升级拒绝。`test:daemon-controls` 复用真实插件 Worker 验证主线工作流可调度、检查点推进、模型子集请求及结果回灌、完成摘要展示和停用/升级/重启后的原回执，不以仅构造工厂或完成 attempt 代替产品输出验收。

工作流取消还需覆盖 Task 租约暂未撤销的窗口：`a_closed_turn_stops_the_workflow_while_its_task_lease_is_still_active` 核对关闭 Turn 后不领取运行/完成子任务的待决回执、不重新排队；`a_turn_closed_during_core_decision_cannot_commit_a_new_action` 在决策期间关闭 Turn，核对迟到的完成、检查点和等待用户动作均被拒绝，原任务负载保持不变。关闭前的正常动作继续由同组工作流正向回归验证。

`workflow_model_and_tool_actions_share_initial_and_updated_skill_restrictions` 覆盖工作流初始与运行中 Skill 的模型目录/工具执行一致性；原正向模型夹具同时验证封闭 Skill 允许的工具确实到达模型。`test:daemon-controls` 安装本地封闭 Skill，通过真实插件 Worker 验证初始选择和 `skill_apply` 激活后均拒绝范围外的 `get_goal`，零新增 Step/Provider 请求；允许的 `web_fetch` 定义与模型结果回灌继续通过。主线与工作流共用 Skill 激活持久化入口；`transferred_agent_skill` 回归核对原生代理写盘失败、工具回执提交失败及换轮后限制保持。`active_skill_custom_bindings_are_exposed_in_tool_surface` 复用同一 Task、Skill 名称、访问档和工作区先建立目录，再把该 Skill 收紧为封闭空集合，核对旧自定义工具不再发布；不能换缓存键后再声称已验证失效。

子代理与自动隔离按需资源回归：

- `cargo test -p magi-api --lib turn_harness::tests::`：包含纯文本子代理不建 worktree、只读/可写子代理首次本地工具分配与回收、代理分支产出不污染主会话账本、自动隔离后的派发与变更账本，以及旧目录回执和首次准备取消。
- `cargo test -p magi-api --lib cancelled_isolation_preparation`：取消持有 Turn 提交锁等待准备退出时，复制后的准备及时回收副本，避免锁等待死锁。
- `cargo test -p magi-conversation-runtime --lib task_execution_dispatcher::`：工具面、目录权限投影、Windows JSON 路径/macOS 别名检测、worktree 分配/产出/回收与工作流合同。
- `cargo test -p magi-conversation-runtime --lib tool_execution_ledger::`：切换目录清除旧读取缓存；已知未执行的回执允许恢复后重新调用。

这些回归使用真实本地 Git、文件、Runner/Task/Turn 和权限服务，Provider 为本地替身；不代表已完成打包 Desktop 或 Windows 原生进程验收。

子代理唤起合同与拒绝恢复回归（设计见 [子代理唤起](subagent-delegation-design.md)）：

- `cargo test -p magi-tool-runtime --lib agent_delegation`：四种目的、并行父任务工作、未知字段、空白/null/超长输入及规范化回执。
- `cargo test -p magi-conversation-runtime --lib agent_spawn`：真实工具 Schema、直接入口复用预检与拒绝后无 Task/Thread/计划残留；决策不能恢复未授予的工具，调用方不能提交模型、Provider、引擎或推理强度覆盖。
- `cargo test -p magi-conversation-runtime --lib role_engine_model_config`：按选定角色解析绑定；共用引擎与角色/引擎同名时不混淆身份，未绑定可继承，显式绑定失效返回错误。
- `npm --prefix web run test:active-agent-center`：代理中心按服务端角色身份显示标签，不用引擎 ID 反查角色；覆盖共用模型、角色/引擎同名及 registry 展示路径。
- `cargo test -p magi-conversation-runtime --lib agent_cancel`：取消只影响直接子树，终态与资源结算分开；结算失败保留原回执，再次核对不覆盖产物。
- `cargo test -p magi-conversation-runtime --lib dispatch_quiesce`：按子树等待全部执行退出，不等待调用者或兄弟任务。
- `cargo test -p magi-conversation-runtime --lib task_runner::tests`：执行范围隔离、旧扫描后的领取/失败拒绝、孤儿清理与准入名额；已转交子树不受原 Runner 的终态影响。
- `cargo test -p magi-orchestrator --lib`：包含转交与租约/取消/失败提交竞争、完成后旧候选不触发失败、孤儿清理及写盘失败的原子性；显式入口的端到端回归另见 `transferred_agent`。
- `cargo test -p magi-api --lib transferred_agent -- --test-threads=4`：真实主线停止、Session 关闭/删除与转交 Agent 的问题恢复；验证拥有方不同的终止边界和 canonical 交互收口。
- `cargo test -p magi-api --lib transferred_agent_goal_replay -- --test-threads=2`：真实 Goal 代理经暂停/清除后，相同 operation 读取原转交、错误 owner/Goal/revision 拒绝，不恢复 Goal、不启动 Runner/Step；与创建入口的当前授权分开核对。
- `cargo test -p magi-api --lib state::tests:: -- --test-threads=8`：Task/Browser 资源范围、转交 lease 保留、Session 全树清理、Goal 控制版本撤销及 checkpoint 失败重试。
- `cargo test -p magi-conversation-runtime --lib recovered_agent_cancel_rechecks_settlement`：恢复后显式取消重新核对原 Task，不复用旧结算回执；Shell/进程副作用重放保护不放宽。
- `cargo test -p magi-api --lib agent_cancel_receipt_waits_for_child_process_and_worktree_exit`：Unix 真实进程、Job、Task/Turn、Git worktree；主线收到成功回执前子资源已退出，Provider 使用本地替身。
- `cargo test -p magi-conversation-runtime --lib task_labels_do_not_bypass_model_execution_or_invent_validation_success`：规划无工具任务、规划文本验证、执行结果验证均实际调用模型；任务名称、目标关键词及看似成功的依赖不能替代本次执行或生成固定“通过”。
- `cargo test -p magi-api --lib agent -- --test-threads=4`：实际主子 Task/Turn 的派发、上下文、嵌套、权限、取消、产物与重建；无效决策由主线接管，拒绝回执序列化恢复后不变成未知副作用。
- `cargo test -p magi-api --lib task_profile_ -- --test-threads=4`：普通工作不隐式创建代理、不强制 agent_spawn；成功派发的决策随 canonical 参数、成功回执和创建事件保留。

这些测试不判断自然语言理由是否真实有益；主代理的委派与代理选择决策按设计文档 §9 固定任务矩阵评测，不用本地替身的成功派发率代替。比较主对话模型时固定子代理角色与模型绑定；继承主模型的场景单列，不能把代理派发评测理解为运行时先选模型再找代理。

子代理动态 Skill 与恢复边界：

- `cargo test -p magi-api --lib transferred_agent_skill`：真实主子链、显式转交与主线换轮后实际激活 Skill、收紧工具并保留 canonical 状态；限制提交失败和成功回执提交失败均停止后续模型调用，恢复不扩大工具范围。
- `cargo test -p magi-session-store --lib agent_skill`：Agent 限制经过工具写回和序列化重载仍保留；写盘失败或错误来源不改变 durable、sidecar 与 flush 状态。
- `cargo test -p magi-conversation-runtime --lib mainline_skill_activation`：主线精确更新、陈旧 Turn、其他 Task、关闭后的迟到激活拒绝。

子任务并发发布与失败回滚：

- `cargo test -p magi-conversation-runtime --lib task_execution_registry::tests::`：TaskStore 发布前已有执行计划、Thread、计划绑定和准入；Task 发布提交失败回滚全部准备结果。
- `cargo test -p magi-api --lib agent_spawn_commit_failure_discards_prepared_context_and_keeps_mainline_running`：真实发布故障后没有运行时信号通道、Task/Thread/名额残留，主线可接管。
- `cargo test -p magi-api --lib concurrent_agent_turns_keep_child_completion_and_parent_results`：12 会话并发，父子完成与结果归属不串线。
- `cargo test -p magi-api --lib interrupt_replacement_commit_failure_retains_input_and_revision_checked_retry_settles_it`：重复结算同一错误不推动 Input revision，后续显式重试/删除仍沿原屏障执行。
- `cargo test -p magi-api --lib queue_request_waiting_for_delete_rechecks_session_scope`：队列读取和删除在提交锁前排队、会话随后删除时，释放锁后重新核对 Session 归属并返回 `SessionNotFound`，不从已删除会话构造投影或触发 panic。

高并发验证使用 `cargo test -p magi-api --lib turn_harness::tests:: -- --test-threads=16`。提问/审批/仓库等待按事件订阅核对原身份和发布，不通过固定 sleep 推测业务阶段；保留原本的超时、终态、权限及一次执行断言。通过的低并发或单项回归不能替代该并发范围。

### 目标用量、预算移除与后台调度生命周期回归

- `cargo test -p magi-session-store --lib`：父子共享用量、迟到结算、重放幂等、暂停恢复、旧预算数据迁移及共享 transport 安全上限。
- `cargo test -p magi-conversation-runtime --lib model_step::tests`：请求身份、写盘失败阻止发送、关闭 Turn 的迟到响应、暂停阻止首次调用和重试。
- `cargo test -p magi-api --lib routes::goals::`：目标编辑及普通暂停恢复，不再要求额度。
- `cargo test -p magi-api --lib high_goal_usage_does_not_stop`：真实 Task 循环不会因累计用量而停止。
- `cargo test -p magi-api --lib background_turn_followup_survives_its_parent_work_returning`：Task 回调结束后派生的下一轮工作仍运行。
- `cargo test -p magi-daemon --lib daemon::persistence::tests`：日志/检查点恢复用量与请求次数；旧 layout 升到当前版本并保留 marker。
- `npm run test:daemon-input-queue`：队列/引导原有六项验收，加未知 usage 时继续执行、大量实际用量只做统计、目标完成及 DTO/界面无预算字段。

上下文窗口、压缩输出限制、防循环和运行时安全上限继续独立验证；任务预算、估算预留、增额恢复已移除。上述验证不等于持续 Agent/后台 Job 已完成。

封闭 Skill 边界：`cargo test -p magi-conversation-runtime --lib live_closed_skill_policy` 覆盖 MCP 执行计数、插件/编排/Plan 的提前拒绝及移除后的迟到调用；`cargo test -p magi-conversation-runtime --lib tool_surface_state` 覆盖已加载插件 schema 随 Skill 收紧；`cargo test -p magi-skill-runtime --lib` 覆盖缺失 Skill 不回退全范围。

### 原生恢复与代理投递边界

- `cargo test -p magi-conversation-runtime --lib`：完整运行时覆盖，含工具事实恢复、连续 Continue、Provider call ID 复用、完成证据、审批提交失败/Skill 撤销阻止真实删除、代理投递与准入竞争。完成结果不经旧 receiver 缓冲或 Runner 测试轮询，迟到 lease 与写盘失败直接验证实际 TaskStore 提交入口。
- `cargo test -p magi-orchestrator agent_context_delivery --lib`：补充与回执单次事务、状态 CAS、提交失败不产生部分接纳、持久化重载。
- `cargo test -p magi-api turn_harness --lib`：主线真实本地 Task/Turn 装配回归；原性能基准按需单独执行。
- `cargo test -p magi-conversation-runtime --lib execution_chain_recovery`：恢复只依据 TaskStore 的实际状态和执行归属，不从 Worker `finish` 投影提交成功或屏蔽 Failed；`cargo test -p magi-api --lib dto::read_model::tests` 核对运行投影复用同一判定，已完成/取消 root 不显示可继续。
- `cargo check -p magi-daemon`：daemon 装配检查。

以上不覆盖发行包与 Windows 原生进程，不能替代对应产品验收。外部 Executor 只留接口，具体实现不在当前验收范围。

### 持续 Agent 身份与持久 Job

- `cargo test -p magi-daemon daemon_restart_recovers_detached_agent_question -- --nocapture`：真实 state 目录/journal/TaskStore projection，两个独立测试子进程分别恢复归档主线后的 Agent 问题、答复及再次启动；确认 Turn 中断、原交互保留、没有新 Step/lease 或重复 Task。
  同一夹具核对 TaskStore 终态先到时的来源解析：陈旧计划、计划已删除、父轮仍运行或已归档，均定位 Agent 原 Turn；归档后状态事件仍保留原会话。
- `cargo test -p magi-session-store independent_agent_restart --lib`：当前主线切换或中断后的独立问题/审批、canonical 写盘失败不发布、Task 来源/终态校验、未知副作用与原 Step 用量不重计。
- `cargo test -p magi-orchestrator restart_reconciles_queued_agent_scope --lib`：没有 Worker 快照/lease 的排队 activation 仍按自己的 execution scope 持久收口，写盘失败不改变 Task，重复恢复幂等。
- `cargo test -p magi-conversation-runtime execution_chain_recovery --lib`：主线 Continue 不恢复、补交或回滚已转交分支。独立继续的实际入口另由下述 App Server 与 Web 验收。
- `cargo test -p magi-api --lib status_callback_and_restart` 与 `cargo test -p magi-conversation-runtime --lib task_completion_notifier`：恢复提交与状态回调共享原执行范围的 restart 锁，旧终态不能消费新执行的绑定。
- `npm run test:daemon-controls`：SIGKILL 后先处理原问题/审批，再通过 `agent/continue` 新建原 Task/Thread 的 Turn；直接继承答复、原批准单次领取、原副本真实删除、重复 operation 与再次重启不重放。嵌套子树预先保留一个已失败后代，核对 GUI 继续投影可用，只恢复中断父子，原失败 Task/Turn、时间和恢复代际均不变。
- `npm run test:daemon-input-queue`：真实 daemon + Chromium 点击原代理继续按钮；保留中断记录与后续主线终态，不重新派发代理。同一验收核对补充等待下一步、进入冻结请求、重载保持在途和收到响应后的使用回执；独立代理的 Step 事件先到也必须沿原 Turn 基线收敛。

- `cargo test -p magi-api session_agent_reactivation --lib`：跨三轮 Agent ID 稳定，新 Task/Thread/授权快照，实际读取前序输出/transcript，拒绝过时代际和跨会话来源，TaskStore 重载。
- `cargo test -p magi-orchestrator agent_activation --lib`：并发激活 CAS、写盘失败回滚和非法来源恢复拒绝。
- `cargo test -p magi-api background_job_outputs --lib`：真实受管 Shell、Task/Turn 结束后的完成/取消事实、事件发布、跨轮读取及重载输出。
- `cargo test -p magi-api background_job --lib`：另含 Job 启动事务写盘失败不执行命令、启动期间取消不会留下孤儿进程和 Job metadata 的公开脱敏。 这组现有回归使用宿主原生 Shell（Windows 为 PowerShell），已纳入 Windows CI；包含停止并改做先结算原 Job 再写入工作区，不新增独立 Windows 测试套件。输出恢复夹具等待实际输出及自然退出，不以 Shell 启动速度代替完成事实。
- `cargo test -p magi-session-store background_job --lib`：Job 与 canonical 事务原子提交、重复启动拒绝、重启收敛、同会话子代理资源边界。进程树完成回归还覆盖启动期/异步退出/显式停止、继承/重定向 stdout/stderr、忽略 SIGTERM 与退出码 0/7 的组合，核对先回收后代再等待输出读取线程，真实完成/失败/取消、输出文件及无迟到写入同时成立；该夹具仅在 Unix 运行，不代替 Windows 原生门禁。
- `cargo test -p magi-tool-runtime --lib builtin::process::output_buffer_tests`：用受控读取线程核对停止前等待输出；输出超时保留原句柄供重试，读取/写出失败在重复停止后仍不变成成功，不发布 `resources_settled`。
- `cargo test -p magi-daemon background_job --lib`：原 Turn 终态后的 Job 事实可以从 canonical 事件日志恢复。
- `cargo test -p magi-tool-runtime delegated_background_process_access --lib`：读/写/停/列出均拒绝兄弟与前序 activation 进程。
- `cargo test -p magi-conversation-runtime --lib`：前序未知副作用保护、旧成功不成为新证据，后台启动成功不等于 Job 成功。

Job 由原 Task/activation 拥有；运行中子树转交与独立继续由专用 App Server/Web 回归验收，外部 Executor 不在当前交付范围。输出属于 daemon 管理的 artifact，业务状态仍在 canonical item；state layout 10 对旧版 daemon 建立写入屏障。交互恢复验证见下文。

- `cargo test -p magi-api --lib background_job_wait`：真实 Shell 完成/非零退出、等待返回实际输出、固定三次模型请求及取消停止原进程，无迟到写入。
- `cargo test -p magi-session-store --lib background_job_wait`：成功提交后唤醒、终态早于订阅、超时、作用域、独立取消及没有活动进程连接时的取消。
- `cargo test -p magi-session-store --lib model_step_and_input_consumption_commit_atomically_and_reject_late_results`：输入消费与 Step 原子提交，提交失败不发布回执，响应/未知结果各自只结算一次。
- `cargo test -p magi-conversation-runtime --lib execution_admission`：等待任务按容量重新准入，取消不复活名额，多个等待者不会同时绕过保护。
- `cargo test -p magi-conversation-runtime --lib context_request_reaches_parent_task_channel`：全局只允许一个执行任务时，父代理等待、子代理请求上下文、父代理答复及子代理继续能够完成；复用同一夹具覆盖任务输入关闭、会话删除和父任务结束，分别返回 `task_interrupted` 或 `parent_not_running`，不继续空转至超时成功。Registry 回归另核对开放空队列与关闭通道的不同返回值。
- `cargo test -p magi-conversation-runtime --lib agent_send_`：排队补充进入启动上下文，运行中补充提交后同时唤醒 FIFO 和 agent_wait 的原 TaskStore 等待边界；写盘失败不推进上下文或通知版本，越级发送不投递。

Job 的 read/wait/list 不触发工作区扫描、写锁或 worktree；等待不意味着命令成功，失败/取消/未知状态保留原退出与结果事实。等待期间释放模型执行名额，后台进程仍可能写入时保持仓库资源归属。

`cargo test -p magi-process --all-targets --locked` 的后代回收和所有者隔离用例同时覆盖 Unix 与 Windows；Windows 还核对根进程先退出后，原 Job 的后代仍被终止，活动进程数归零后才返回。同步/异步终止复用同一 Job 查询屏障，查询失败或超时不能报告资源已结算。本机交叉 `check`/`clippy --all-targets` 只验证可编译，Windows 原生执行由现有 Windows 2022 CI 承接。

其中 `async_managed_termination_settles_descendants_after_root_reaped` 使用真实 Unix Shell 创建后代并退出根进程，确认 Tokio 已清除 PID 后分别验证显式终止、Drop 和原 owner 取消仍能回收后代。同步已退出根进程回归同时核对 `wait` 不注销原登记。根进程退出与资源结算是两个边界；不能因 `wait`/`try_wait` 返回终态便失去整棵树的拥有方。

### 原生交互持久恢复

- `cargo test -p magi-session-store --lib approval`：审批恢复、写盘失败原子性、双领取、目录/策略/任务范围和取消/过期/拒绝。
- `cargo test -p magi-conversation-runtime --lib tool_approval`：审批等待/租约与取消、真实删除、重启恢复单次领取、提交失败及 Skill 撤销。
- `cargo test -p magi-daemon --lib approval_decision_and_cross_turn_claim`：原审批决定与跨 Turn 领取从 canonical 日志重放，不重启工具。
- `npm run test:daemon-approval-recovery`：独立状态、本地 Provider、真实 daemon SIGKILL 重启、Chromium 原审批按钮、显式 Continue 与真实文件删除；批准本身无副作用且不二次审批。脚本等待自己创建的 Chromium 退出后清理独立目录。

原生提问/答复同样复用 SessionStore 交互事务。当前 state layout 为 18；外部客户端调用不承诺跨重启重放，发行包与 Windows 原生验收需独立执行。

会话删除还要清除进程内提问等待注册表：`cargo test -p magi-api --lib delete_session` 包含真实持久提问夹具的删除回归，不能只核对 SessionStore 主记录已删除；`cargo test -p magi-conversation-runtime --lib user_question` 核对有 Task/无 Task 的等待方均断开、其他会话仍可正常答复。

### Executor 能力与历史恢复

当前仅验收原生模型、GPT Web、现有插件及通用接口/历史读取边界；具体外部执行器和专用验收已撤下，不属于当前产品交付。Magi CLI 已废弃，手机使用同一 Web 工作台。

定向入口：`cargo test -p magi-bridge-client executor_capabilities --lib`、`cargo test -p magi-plugin-system session_engine --lib`、`cargo test -p magi-conversation-runtime model_step --lib`、`cargo test -p magi-conversation-runtime executor_owned_source --lib`、`cargo test -p magi-api unsupported_executor_preserves --lib`。持久化入口：`cargo test -p magi-daemon model_step_journal_replays --lib` 与 `cargo test -p magi-daemon prior_state_layouts_upgrade --lib`。

覆盖发送前拒绝、完整插件文字历史、唯一终态、Step 能力重载、历史外部工具不重复执行、切换模型不能绕过来源恢复限制，以及不支持引导时保留队列。队列 Web 投影还必须保留服务端 revision、workflow/plugin 选择、停止并改做元数据和 `unsupported_executor` 原因；否则编辑/重试会丢 CAS 身份，排队输入会退化成普通消息。`cargo test -p magi-conversation-runtime --lib conversation_loop_streams_tool_round_content_like_main_chat` 核对原生模型最终签名经最终化和 Thread 重建保留，不能被之前的工具响应签名替换。Web 同时运行 check、bridge golden 与 protocol:check；`npm run test:daemon-input-queue` 从真实 daemon + Chromium 复验原生引导、队列消费与无预算 Goal。GPT Web 的离线 Driver 回归不代表已登录网站、打包 Desktop 或 Windows 原生验收。

### 受控两层代理与资源归属

`cargo test -p magi-api nested_agents --lib` 覆盖真实 Task/Turn 链路的两层派发、父级未提交快照、逐级采用和 worktree 回收，以及 Unix 后台进程等待中取消整树。`cargo test -p magi-api agent_apply --lib` 校验直接产出应用、越级拒绝与缺失调用身份拒绝。`cargo test -p magi-orchestrator nested_agent_delegation --lib` 覆盖委派深度/授权、持久恢复与提交失败。`cargo test -p magi-conversation-runtime tool_definition_cache_separates --lib` 验证委派授权不会通过缓存混淆。布局升级继续使用 `cargo test -p magi-daemon prior_state_layouts_upgrade --lib`（2–13 升到 14）。

这些验收不覆盖真实外部模型、发行包或 Windows 原生进程行为；运行中转交的 Session/Goal 入口由 `transferred_agent` 与 App Server 控制回归覆盖。

### 转交持久边界

`npm run test:daemon-controls` 从开发脚本启动独立 daemon，使用本地 Provider 和真实 App Server WebSocket；覆盖无效委派拒绝及重载、问题答复/跳过、双客户端竞争、SIGKILL 后原审批/问题恢复、提前 Continue 无突变、批准后显式恢复、文件只删除一次、直接/嵌套代理与原 worktree 读回、父轮仍运行时代理原 Turn 完成、转交版本与幂等、停止原父轮、Goal 暂停/更新/恢复/完成/清除。客户端请求只做单次发送，不实现 CLI、持久队列或自动重发。失败可设置 `MAGI_KEEP_TEST_FIXTURE=1` 保留隔离状态目录。

`replaceTurnId` 语义是编辑后重发；停止并改做通过独立的 `interruptTurnId` 持久输入和取消结算屏障实现。state layout 2–17 的升级与备份使用 `cargo test -p magi-daemon prior_state_layouts_upgrade --lib`。

相关回归：`cargo test -p magi-api routes::goals::tests --lib`（包括真实 Shell 的暂停隔离）、`cargo test -p magi-api app_server::session_controls --lib`、`cargo test -p magi-session-store retired_task_budget --lib`。队列合同改动同时运行 `npm run protocol:check`、Web check、Bridge 与 agent API golden，以及会话路由/停止并改做故障回归。 队列版本回归复用 `queued_plain_text_can_be_guided_once_and_removed_atomically` 与 `test:daemon-input-queue`：编辑前旧版本在绑定后仍冲突；同次补充在绑定、首次消费和页面重载后可幂等读取，普通下一轮领取不算补充成功；零版本由共享应用服务拒绝。App Server 强制显式版本，HTTP 保留 schema 规定的旧兼容入口，GUI 不使用无版本兼容或最新快照代填。

工具范围收敛使用 `cargo test -p magi-tool-runtime --lib task_tool_scope_intersection` 与 `cargo test -p magi-conversation-runtime --lib task_catalog_and_execution_share_explicit_tool_restrictions`：封闭空集合、互不相交白名单和重复合并不得扩大权限；主线及子代理读取目录与实际调用遵守同一 Task 允许/拒绝范围。`active_skill_custom_bindings_are_exposed_in_tool_surface` 复用原 Skill 夹具核对 Task 白名单/明确禁止也过滤自定义定义；`task_labels_do_not_bypass_model_execution_or_invent_validation_success` 在可用 Browser 宿主下核对真实模型请求，`no_tools` 不因每轮能力刷新重新收到工具。委派合同还要求 `parent_work` 只出现在 `parallel_work`，避免用无关字段伪造并行收益。原 Skill 撤销、派发与嵌套取消回归继续保留。
