# Magi 产品 / 业务层面收口任务清单

起草时间：2026-05-09
范围：仅产品定位、用户心智、业务能力取舍与协议表面收口；**不含大文件拆分**（留到结构化重构阶段）。

---

## 0. 总览

- **当前能力评分**：6.5 / 10
- **目标评分**：8.0 / 10（+1.5）
- **总任务数**：18
- **核心风险**：
  - #10 + #13 联动后，用户没配模型则任务编排不可用 → 需要明确降级策略（建议任务编排不可用，纯 chat 仍可跑）
  - #15 是产品差异化决定（自治 Agent vs 纯工具型 chat）
  - 命名级改动（#3）需全量 `cargo check --workspace` + `npm run check` 验证

### 状态图例

| 标记 | 含义 |
|---|---|
| ⬜ | 待办 |
| 🔄 | 进行中 |
| ✅ | 已完成 |
| ⏭️ | 跳过 / 取消 |
| ⛔ | 阻塞 |

### 进度面板

| 区块 | 总数 | ⬜ | 🔄 | ✅ | ⏭️ |
|---|---|---|---|---|---|
| P0 产品定位锚定 | 3 | 0 | 0 | 3 | 0 |
| P1 用户心智核心抽象 | 4 | 0 | 0 | 4 | 0 |
| P2 业务能力收口 | 5 | 4 | 0 | 1 | 0 |
| P3 任务系统产品表达 | 3 | 2 | 0 | 1 | 0 |
| P4 链路边界收口 | 3 | 2 | 0 | 1 | 0 |
| **合计** | **18** | **8** | **0** | **10** | **0** |

---

## P0 — 产品定位锚定（先决策，不写代码）

### #1 确认产品定位
- **状态**：✅
- **完成时间**：2026-05-09
- **任务**：在「个人本地 Agent 工作台」/「小团队协作工具」/「跨 IDE 平台」中三选一。
- **决策**：**个人本地 Agent 工作台 + 可选 LAN 分享**
  - 砍掉「跨 IDE 平台」长期叙事（不再做 VSCode/IDEA 插件）
  - tunnel/lan-access 保留为高级能力，从主路径退到 Settings 抽屉
  - 后续 P0-#2 将依据本决策清理 IDE 宿主抽象层
- **建议**：定位为 **「个人本地 Agent 工作台 + 可选 LAN 分享」**。砍掉跨 IDE 平台叙事，保留 tunnel 作为高级能力。
  - 理由：当前唯一在跑的形态就是它；其他形态在代码里只剩占位。
- **改后增量**：可演进性 +0.8
- **依赖**：无（这是所有后续任务的锚）
- **代码证据**：单 daemon（个人）+ `/api/tunnel`（小团队）+ host loopback / VSCode prehost（IDE 平台）三套接口并存

### #2 IDE 宿主去留
- **状态**：✅
- **完成时间**：2026-05-09
- **任务**：决定是否真的要做 VSCode/IDEA 插件。
- **决策**：**完全移除 IDE 宿主抽象层**（依据 #1 产品定位）
- **执行结果**：
  - 删除文件（约 2900+ 行）：
    - `crates/magi-bridge-client/src/host_loopback/`（6 个文件）
    - `crates/magi-bridge-client/tests/host_loopback.rs`
    - `crates/magi-bridge-client/src/bin/host_bridge_loopback.rs`
    - `crates/magi-bridge-client/src/bin/vscode_host_shell.rs`
  - 删除类型/trait：`HostBridgeClient` trait、`HostBridgeRequest`/`HostBridgeCommand`/`HostKind`、`JsonRpcHostBridgeClient` impl
  - 删除枚举变体：`BridgeBindingKind::Host`、`BridgeDispatchAction::HostTerminalExec`、`BridgeServerKind::Host`
  - 删除 `BridgeDispatchRuntime::host_client` 字段与 `with_host_client()` builder
  - 删除 `dispatch.rs::resolve_host_kind` 与 Host 路由分支
  - 删除 daemon `host_transport` subprocess（不再起 host_bridge_loopback 进程）
  - 删除 API DTO 中的 `capture_host_cutover_checks`/`capture_host_preflight_checks` 与对应 import
  - 删除 skill-runtime 中以 host binding 为 fixture 的测试
  - 修正 cutover_smoke / preflight_snapshot 的快照测试（services 数量从 3 → 2，去掉 host 断言分支）
  - 修正 daemon runtime 的三个集成测试同步 services 数量
  - 注：保留了 `HostBridgeClient` trait 的设计是 P0-#2 原方案，但代码评估后发现 trait 没有任何 production 实现，product 定位锚定为「个人本地」后亦无未来扩展点，因此完全移除
- **改后增量**：桥接抽象 +1.0、可演进性 +0.5
- **依赖**：#1
- **代码证据**：`magi-bridge-client/src/host_loopback/`、`local_process_protocol.rs`，仓库内**无任何 VSCode 插件代码**在用它们
- **验证**：`cargo check --workspace` ✓ / `cargo build --workspace --bins` ✓ / `npm --prefix web run check` ✓ / 仅 magi-api 2 个**预存在**测试失败（与本次改动无关）

### #3 Shadow 概念退场
- **状态**：✅
- **完成时间**：2026-05-09
- **任务**：`Shadow*` 命名遗债退场。
- **建议**：批量重命名 → 去掉 `Shadow` 前缀；`SHADOW_MODEL_PROVIDER` → `LOOPBACK_MODEL_PROVIDER`；`SHADOW_MCP_*` 同理。
- **改后增量**：命名一致性 +2.5（**最便宜的高 ROI**，半天完成）
- **依赖**：无
- **代码证据**：`ShadowDaemonRuntime / ShadowTaskDispatcher / ShadowStateRepository / ShadowRuntimeMaintenance / ShadowRuntimeSidecarPersistence / ShadowTaskExecutionRegistry / SHADOW_MODEL_PROVIDER / SHADOW_MCP_TOOL_NAME` 等几十处
- **执行结果**：
  - 18 个 PascalCase 类型去 Shadow 前缀（`ShadowDaemonRuntime`→`DaemonRuntime` 等）；`ShadowTaskDispatcher`→`LlmTaskDispatcher`（避免与 trait `TaskDispatcher` 冲突）；`ShadowWorkerExecutor` trait→`WorkerExecutor`
  - 3 个常量重命名（`SHADOW_*` → `LOOPBACK_*`），值同步从 `"shadow-*"` 改为 `"loopback-*"`
  - 4 个枚举变体：`ShadowDefault`→`Standard`、`ShadowLoopback`→`Loopback`（共三处）
  - 文件 `crates/magi-api/src/shadow_execution.rs` → `dispatch_execution.rs`
  - 数十个 snake_case 函数/测试/模块名去 Shadow（`run_shadow_*`→`run_*`、`drive_shadow_*`→`drive_*`、`shadow_loopback_*`→`loopback_*` 等）
  - Wire 字符串：`"shadow-model"`/`"shadow-mcp"`/`"shadow-loopback"`/`"v0-shadow"`/`"shadow-runtime-v1"` 等全部替换为 loopback/test/runtime 对应名
  - 测试 fixture 名（`session-route-shadow`/`worker-shadow-N` 等）统一改为 `loopback`/`test` 前缀
  - **`cargo check --workspace`** ✓、**`cargo build --workspace --bins`** ✓、**`npm --prefix web run check`** ✓（0 错 0 警）
  - 既有 `magi-context-runtime` 测试编译错误（`canonical_turns` 字段缺失）与 `magi-api` 两个路由测试失败均为**预存在问题**，与本次 rename 无关

---

## P1 — 用户心智的核心抽象

### #4 删除 `deep_task` 布尔开关
- **状态**：✅
- **完成时间**：2026-05-09
- **任务**：取消用户面的"普通/深度"模式选择，统一交模型决策。
- **建议**：删除 `request.deep_task` 字段与所有路径分支，**完全交给 `classify_session_turn` LLM 分类器判断**。前端只剩输入框，无模式开关。配套删除 `session_turn_requests_task_orchestration` 的 24 条关键字白名单。
- **改后增量**：协议表面 +0.8、领域建模 +0.3
- **依赖**：#10（先确保业务模型可用）
- **代码证据**：`SessionTurnRequestDto.deep_task` + `routes/sessions.rs::session_turn_requests_task_orchestration`
- **执行结果**：
  1. 协议层 / DTO / 内部派发字段全删（`SessionTurnRequestDto`、`DispatchSubmissionRequest`、`ActiveExecutionDispatchContext`、`DispatchMemoryExtractionInput` 等无残留）。
  2. 持久化层（execution_writeback、session-store sidecar、workspace-store recovery）字段全删；旧持久化文件中遗留的 `deepTask` 键由 serde 默认忽略策略自然吸收，无需迁移脚本。
  3. 设置 bootstrap 的独立 `deepTask` 链路（`runtime_settings_from_snapshot` / `runtime.rs`）全删；`runtimeSettings` 只剩 `locale`。
  4. Loopback stub（`model_loopback.rs`、`StaticTestModelBridgeClient`、`StaticModelBridgeClient`）改用 `任务图规划器` 前缀触发深图 stub；所有 fixture prompt 与 fixture 字段同步清扫。
  5. **统一 task policy（autonomous_kind=Autonomous / retry_budget=1 / repair_budget=1 / validation_required=None）替代原 `build_deep_readonly_policy` / `build_deep_no_tool_policy` / `build_deep_action_policy` / `build_policy_for_mode` 四套**——所有 Phase / WorkPackage / Action / Repair / Decision / Validation 节点共享同一 policy。`TASK_MIN_PHASES` 由 3 降为 1，让 LLM 自行决定结构深度。
  6. 重命名退场：`DEEP_TASK_PLAN_TOOL_NAME → TASK_PLAN_TOOL_NAME`、`create_deep_task_plan → create_task_plan`、`DeepTaskGraphPlan/PhasePlan/...` 去 `Deep` 前缀、`build/replan/insert/validate_deep_task_graph → *_task_graph`、`extract_deep_task_goal → extract_task_goal`、`loopback_deep_task_goal → loopback_task_goal`。

### #5 「团队模式」语言收回
- **状态**：✅
- **完成时间**：2026-05-09
- **任务**：从用户面与路由识别词中彻底删除"团队模式"。
- **建议**：删除 i18n、关键字识别、文案里的"团队模式""team mode"。后端 worker 协作能力保留，但用户面不暴露这个名词。`shadow_execution.rs` 文案改用"步骤"代替"分支"。
- **改后增量**：协议表面 +0.3
- **依赖**：#4
- **代码证据**：上次 commit (8467edc) 又把"团队任务模式""team mode"加回了 `session_turn_requests_task_orchestration`，但 AgentTab/BottomTabs 已删——语言在摇摆
- **执行结果**：
  1. 全仓 grep `团队模式|团队任务|team mode|team_mode|teamMode` 在代码层（`.rs`/`.ts`/`.svelte`/`.json`）已为 0。
  2. 后端用户面文案（`dispatch_execution.rs`、`session_turn_writeback.rs`）将"执行分支"统一改为"执行步骤"——分派 summary、worker_spawned 卡片、orchestration mainline summary 等出口已对齐。
  3. 前端 i18n（`web/src/i18n/zh-CN.json`）`dispatchGroupCard.dispatchSummary` 与 `runtimeDiagnostics.recovery.restoredWorkerBranchCount` 文案改为"执行步骤""恢复步骤数"；en-US 中"lanes/branches"暂留（英文术语本就独立，与中文用户面无关）。
  4. 测试 fixture（`routes/dispatch_flow.rs::action_task_title_does_not_repeat_execute_prefix`、`dto/read_model.rs` worker_lane title `"完成分支"`）改为不含"团队模式""分支"的中性描述，避免在测试中复活术语。
  5. 内部代码标识符（task_id `lane-*`、`active_branch_task_ids`、字段名 `branches`）属于写域内部概念，本次保留——与 P1-#7 (Worker 术语弱化) 一并处理。

### #6 TaskKind 用户可见面收口
- **状态**：✅
- **完成时间**：2026-05-09
- **任务**：用户面只暴露用户能识别的几类任务节点。
- **建议**：用户面（`TasksPanel` 主视图）只渲染 **Action / Validation / Decision** 三类。`Phase / WorkPackage / Repair / Objective` 只在"技术明细"折叠区。前端加 `userVisibleKinds` 白名单。
- **改后增量**：领域建模 +0.4
- **依赖**：无
- **代码证据**：`magi-core/src/task.rs::TaskKind`（7 种）+ `TasksPanel.svelte::taskTreeRows`（全暴露）
- **执行结果**：
  1. `web/src/lib/task-labels.ts` 新增 `USER_VISIBLE_TASK_KINDS = ['Action', 'Validation', 'Decision']` 常量与 `isUserVisibleTaskKind(kind)` 守卫；命名按 P1-#7 后续约定避免硬编码字符串。
  2. `TasksPanel.svelte` 主视图新增"执行步骤"section（task-step-list），渲染扁平的用户可见任务（`activeProjectionTasks` 过滤后按 `created_at` 排序），点击行可在 `task-graph-store` 选中节点；列表为空时不渲染（避免空 section）。
  3. `Phase / WorkPackage / Repair / Objective` 等结构性节点继续保留在已存在的 `<details class="task-details-disclosure">` 技术明细折叠区，主视图不再以"任务树+全部 7 类"作为默认呈现。
  4. 顺手清理 P1-#4 残留：`buildDeliveryPackageSummary` 的 `模式：${pkg.execution_mode === 'deep' ? '深度模式' : '普通模式'}` 行删除（deep_task 已退场，不再有"深度/普通"模式概念；`execution_mode` 字段后端清理留待 backend 后续 PR）。
  5. 新增样式（task-step-list / task-step-row / task-step-content / task-step-status）按现有 token 体系着色，hover/selected 状态与 attention section 视觉一致。
  6. **`npm --prefix web run check`** ✓（683 文件 0 错 0 警）；daemon 起服 + 浏览器开 Tasks 面板验证空状态 / Tab 切换无 console 错误。

### #7 Worker 术语弱化
- **状态**：✅
- **完成时间**：2026-05-09
- **任务**：前端文案去 "worker" 化。
- **建议**：前端 `Worker` → `执行者`，`lane` → `执行步骤`。`WorkerBadge.svelte` → `ExecutorBadge.svelte`。后端代码不动（写域隔离）。
- **改后增量**：协议表面 +0.2
- **依赖**：无
- **代码证据**：`WorkerBadge.svelte` + `worker-card-view-model.ts` + `agent-colors.ts`
- **执行结果**：
  1. 组件改名：`WorkerBadge.svelte` → `ExecutorBadge.svelte`，内部类型 `WorkerStatus → ExecutorStatus`、组件错误标识 `'WorkerBadge' → 'ExecutorBadge'`、CSS 类 `.worker-* → .executor-*`、CSS 变量 `--worker-color → --executor-color`；唯一调用方 [MessageItem.svelte:5](web/src/components/MessageItem.svelte:5) 与 [MessageItem.svelte:390](web/src/components/MessageItem.svelte:390) 同步替换。
  2. 保留组件 prop 名 `worker` 与 i18n key 命名空间 `workerBadge.*`（i18n 键是稳定标识符，仅翻译值；prop 名属于内部抽象 worker 角色概念，不外露）。
  3. zh-CN 用户面字面量翻译（11 条）：`agentTab.empty.hint`、`config.toast.workerRefreshFailed`、`edits.empty.hint`、`settings.model.workerModel`、`settings.model.noWorkerConfig`、`settings.profile.workerAssignment`、`settings.profile.userRulesDesc`、`toolCall.displayName.{dispatchTask,sendWorkerMessage,waitForWorkers}`、`toolCall.errorDiagnosis.roleConstraint.hint`、`messageHandler.autoAnswerWorkerQuestion`、`dispatch.lane.{header,description}`、`dispatch.errors.workerWaitUnknownTasks`、`dispatch.notify.routingAdjusted`、`dispatch.waitForWorkers.timeout`、`runtimeDiagnostics.scope.worker`、`runtimeDiagnostics.recovery.restoredWorkerSessionCount` —— 全部统一为「执行者」。
  4. **保留** LLM prompt 模板（`dispatch.phaseBPlus.prompt`、`dispatch.phaseC.{fallbackNotice,progressMessage}`、`dispatch.reactiveSummary.{header,taskLine}`、`dispatch.predecessor.item`）中的 `Worker` —— 这些字符串拼入提示词送给模型，「Worker」是 LLM 已建立的角色术语，与产品用户面无关。
  5. **en-US 保留 "Worker"**（沿用 P1-#5 precedent：英文术语本就独立，与中文用户面收敛策略无关；强行翻成 "Executor" 反而让英文 UI 显得机器化）。
  6. **未做**：`worker-card-view-model.ts` 改名 —— 经核查该文件无任何外部消费者（dead code），与本次「文案弱化」无关，建议作为单独的死代码清理项；`agent-colors.ts` 仅维持「角色 → 颜色」映射，无 Worker 字面量。
  7. 验证：`npm --prefix web run check` ✓（683 文件 0 错 0 警）；daemon 起服 + 浏览器开 `web.html`，console 无错误，i18n 渲染正常。

---

## P2 — 业务能力收口

### #8 知识库三分类合并
- **状态**：⬜
- **任务**：ADR / FAQ / Learning 合并为统一 KnowledgeItem。
- **建议**：合并为 `KnowledgeItem { kind: "adr"|"faq"|"learning", ... }`，CRUD 接口从 18 个端点 → 6 个（`/knowledge/items{,/search,/add,/update,/delete}` + `/knowledge`）。前端用 tag 区分。
- **改后增量**：协议表面 +0.5、领域建模 +0.3
- **依赖**：无
- **代码证据**：`routes/knowledge.rs` 目前每类 6 个端点，三类共 18 个

### #9 Skill 并入 Custom Tool
- **状态**：⬜
- **任务**：合并 skill 与 custom tool 的扩展机制。
- **建议**：保留 MCP 独立（真外部协议）。Skill 本质是带 instruction 的 tool —— 把 `magi-skill-runtime` 合并进 `magi-tool-runtime`，用户面统一为「自定义工具」。`SettingsToolsTab` 同时管理两者。
- **改后增量**：crate 切分 +1.0、领域建模 +0.3
- **依赖**：无
- **代码证据**：`magi-skill-runtime`（4 个 mod）独立 crate + `magi-tool-runtime` 独立 crate

### #10 删除双模型客户端
- **状态**：✅
- **完成时间**：2026-05-09
- **任务**：取消"业务模型"与"任务规划模型"的双轨架构。
- **建议**：删除 `task_planning_model_client = JsonRpcModelBridgeClient::new(model_transport)`。所有模型调用走 `business_model_client`（HTTP）。用户没配模型时任务编排显式失败（但 chat 仍能跑），不再用 loopback 假装能编排。loopback 留作单测 stub。
- **执行结果**：
  - 删除 `ApiState::task_planning_model_client` 字段、`with_task_planning_model_bridge_client` setter 与 `task_planning_model_client()` getter
  - 删除 daemon runtime 中 `task_planning_model_client = JsonRpcModelBridgeClient::new(model_transport)` 实例化与 `state.with_task_planning_model_bridge_client(...)` 装配
  - 全仓 `state.task_planning_model_client()` → `state.model_bridge_client()`；`with_task_planning_model_bridge_client` → `with_model_bridge_client`
  - **保留 loopback 作为未配置 fallback**：`business_model_client` 在 `direct_http_probe_result.is_none()` 时退化为 `JsonRpcModelBridgeClient(model_transport)`，并打 warning（生产应配置 MAGI_OPENAI_COMPAT_BASE_URL）
  - 补 `classifier_payload_for_prompt` 的 stub 返回字段（`confidence/reasonCode/routeReason/taskEvidence`），让 task route 能通过 `creation_evidence` 校验
- **改后增量**：桥接抽象 +0.5、可演进性 +0.4
- **依赖**：#13（合并使用同一识别策略）
- **代码证据**：`crates/magi-daemon/src/daemon/runtime.rs::build_api_state_with_options` 里 `business_model_client` 与 `task_planning_model_client` 双客户端
- **验证**：cargo check ✓ / cargo build ✓ / npm check ✓ / magi-daemon 52/52 通过 / magi-api 仅 2 个**预存在**失败

### #11 Settings 面板按用户角色重排
- **状态**：⬜
- **任务**：从 10 个并列 tab 收成 4 大区。
- **建议**：
  - 「快速开始」：模型 + 工作区
  - 「能力扩展」：自定义工具（含旧 skill）+ MCP
  - 「我的偏好」：用户规则 + 安全策略
  - 「使用统计」：stats
  - 其余（registry engines、agents、worker config 等）折叠到「高级」抽屉
- **改后增量**：协议表面 +0.4
- **依赖**：#9（skill 并入 tool 后才能合 tab）
- **代码证据**：`SettingsModelTab/AgentsTab/ToolsTab/RulesTab/StatsTab` 五个组件 + `routes/settings.rs` 2055 行 / 数十端点

### #12 tunnel / lan-access 收到高级抽屉
- **状态**：⬜
- **任务**：弱化主路径上的局域网共享入口。
- **建议**：保留全部能力，UI 移到 Settings 「高级 → 网络」。默认关闭。
- **改后增量**：协议表面 +0.1
- **依赖**：#1（确认产品定位是个人本地）
- **代码证据**：`routes/changes_files_tunnel.rs::start_tunnel/stop_tunnel/lan_access_status`

---

## P3 — 任务系统的产品表达

### #13 任务路由识别全部交给 LLM
- **状态**：✅
- **完成时间**：2026-05-09
- **任务**：删除关键字白名单驱动的任务路由识别。
- **建议**：删除 `session_turn_requests_task_orchestration`（24 关键字）和 `session_turn_requests_current_project_analysis`（30+ 关键字）。完全依赖 `classify_session_turn` 的模型决策。
- **执行结果**：
  - 删除 3 个关键字检测函数与对应的 `normalize_session_turn_decision` 覆盖块：
    - `session_turn_requests_task_orchestration`（24 关键字 → 强制 task 路由）
    - `session_turn_requests_diagram_render`（31 关键字 → 强制 execute + diagram_render）
    - `session_turn_requests_current_project_analysis`（30+ 关键字 → 强制 execute + shell_exec）
  - 删除关联的 `diagram_render_tool_intent` / `current_project_tool_intent` 工具意图生成器
  - 删除 5 个对应的单元测试（`normalizes_diagram_creation_*` / `mind_map_creation_*` / `normalizes_current_project_*` / `normalizes_current_workspace_identity_*` / `explicit_team_task_mode_*`）
  - **保留** `session_turn_requested_public_builtin_tools`（不是关键词分类，是用户显式工具名引用，如 `请调用 file_read`）与对应的工具 intent helpers
  - LLM `classify_session_turn` 分类器 prompt 已包含所有相关指引（diagram_render、execute vs task 等），现由 LLM 单独决策
- **改后增量**：领域建模 +0.3、协议表面 +0.2
- **依赖**：#10（业务模型可用作为前置）
- **代码证据**：`routes/sessions.rs` 里两份关键字白名单共 50+ 条
- **验证**：cargo check ✓ / cargo build ✓ / npm check ✓ / magi-daemon 52/52 / magi-api 仅 2 个 pre-existing 失败

### #14 任务状态机用户面三态化
- **状态**：⬜
- **任务**：把 5 态 runner_status 在前端压成 3 态。
- **建议**：
  - `running / blocked` → 「执行中」（blocked 显示等待原因 tooltip）
  - `stopped` → 「已停止」
  - `completed / error` → 「已完成」（error 用红色 badge）
  - 后端状态保留不动
- **改后增量**：协议表面 +0.3
- **依赖**：无
- **代码证据**：`TasksPanel.svelte::proj.runner_status` 五态分支

### #15 Decision Task 实现或删除（二选一）
- **状态**：⬜
- **任务**：消除"后端定义、前端不渲染"的死代码。
- **建议**：**选实现**——前端补 `DecisionCard.svelte`，渲染 `decision_payload.options`，调用 `/api/tasks/{id}/decision` 提交用户选择。给一个 demo 触发路径（如"网络写权限"触发 Decision）。Decision 是产品差异化关键能力，删了就没自治叙事。
- **改后增量**：领域建模 +0.5、可演进性 +0.3
- **依赖**：#1（产品定位决定是否需要"自治 Agent"叙事）
- **代码证据**：`magi-core/src/task.rs::DecisionTaskPayload` 定义完整 + `/api/tasks/{task_id}/decision` 路由存在 + 前端无 `DecisionCard` 组件

---

## P4 — 链路边界收口

### #16 合并 `/api/session/intake` 与 `/api/session/turn`
- **状态**：⬜
- **任务**：取消两个相似入口。
- **建议**：保留 `/api/session/turn`，合并 `intake` 逻辑进去。前端只调 `submitTurn`。
- **改后增量**：协议表面 +0.2
- **依赖**：无
- **代码证据**：`routes/sessions.rs::submit_session_turn` vs `routes/tasks_interaction.rs::handle_intake`

### #17 一个 session 同一时刻只跑一个 root task
- **状态**：⬜
- **任务**：明确产品语义为单任务串行。
- **建议**：`RunnerManager.session_runner_index` 物理上支持多个，但产品上加约束：提交新任务前如果 session 有运行中任务，弹"停止当前任务并开始新任务？"对话框。
- **改后增量**：领域建模 +0.2
- **依赖**：无
- **代码证据**：`RunnerManager.session_runner_index: HashMap<SessionId, Vec<String>>`

### #18 .gitignore 兜底
- **状态**：✅
- **完成时间**：2026-05-09
- **任务**：避免验收产物每次出现在 git status。
- **建议**：加 `.codex-acceptance-current/` 与 `*.pid` 进 `.gitignore`。
- **改后增量**：可忽略
- **依赖**：无
- **代码证据**：`git status` 长期挂着 `.codex-acceptance-current/`
- **执行结果**：`.gitignore` 增加 `*.pid` 与 `.codex-acceptance-*/` 模式；同时清理上一次 commit 中误带入的验收目录

---

## 1. 改后能力评分

| 维度 | 当前 | 改后 | Δ | 主要贡献项 |
|---|---|---|---|---|
| 领域建模 | 8.0 | 9.0 | +1.0 | #6 #8 #9 #15 #17 |
| 桥接抽象 | 7.0 | 8.5 | +1.5 | #2 #10 |
| crate 切分 | 5.0 | 6.0 | +1.0 | #9 |
| 模块内聚 | 4.0 | 4.0 | — | （留到大文件拆分阶段） |
| 命名一致性 | 5.0 | 7.5 | +2.5 | #3 |
| 协议表面 | 6.0 | 8.0 | +2.0 | #4 #5 #7 #8 #11 #14 #16 |
| 前端约束 | 8.0 | 8.0 | — | |
| 错误边界 | 7.0 | 7.0 | — | |
| 测试浓度 | 6.0 | 5.5 | −0.5 | 大量删减需要重写测试 |
| 可演进性 | 5.0 | 7.0 | +2.0 | #1 #2 #10 #15 |
| **整体** | **6.5** | **8.0** | **+1.5** | |

---

## 2. 推荐执行顺序（4 周节奏）

| 周 | 内容 | 阶段目标分 |
|---|---|---|
| W1 | P0 全部（#1 决策 + #2 删 host loopback + #3 改名）| 6.5 → 7.2 |
| W2 | P2-#10 + P3-#13 + P1-#4（双模型 → 单模型 + LLM 路由 + 删 deep_task）| 7.2 → 7.5 |
| W3 | P2-#8 #9 #11 + P1-#6 #7（业务能力收口 + 用户面术语收敛）| 7.5 → 7.8 |
| W4 | P3-#14 #15 + P4-#16 #17 #18 + 测试补完 | 7.8 → 8.0 |

---

## 3. 维护约束

- 完成一项任务时：
  1. 修改对应 `状态` 为 `🔄`（开始）/ `✅`（完成）/ `⏭️`（取消并写明原因）
  2. 同步更新 §0 进度面板的计数
  3. 完成项追加一行 `**完成时间**：YYYY-MM-DD`，可附 commit 短哈希
- 出现新的产品/业务级问题时，按 P 级别追加新条目，不覆盖已结案项。
- 不要在本文档内记录代码细节实现（那是 PR 描述与代码注释的事）；这里只承载**产品层面的取舍与决策结果**。
