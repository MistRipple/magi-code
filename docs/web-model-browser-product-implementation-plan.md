# Magi Web 模型浏览器 · 实现计划

> 状态：**实现计划**（未实现）。本文承接《[设计基线](./web-model-browser-design.md)》：设计结论、决策、状态所有权与安全边界以设计基线为准；本文只放阶段划分、实测清单、文件级工单、接口契约、错误码、验收、代码索引与历史修订记录。
> 更新日期：2026-09-28（已完成三轮四路联合审核（R1–R46）；第四轮为用户裁决 **R47「A10 改判：Connect 优先、未就绪时以 OpenAI Tunnel 正式交付」** 与 **R48「删除 T1」**）
> 相关文档：[设计基线](./web-model-browser-design.md)、[内置浏览器完整设计](./browser-runtime-design.md)、[上下文压力与压缩架构](./context-pressure-compaction-architecture.md)、[Magi Connect 与移动端方案](./magi-connect-mobile-plan.md)
>
> 编号约定：A / S 是设计决策与场景编号；§5.x 指向《设计基线》的详细设计；C1–C42 只在本附录保留，作为历史修订记录。

---

## 1. 分阶段实施

| 阶段 | 内容 | 完成标志 |
| --- | --- | --- |
| 阶段 0：验证 | 按 §2 清单逐项实测：**持久分区下的登录态跨重启**、各登录方式、冷启动延迟、可用窗口标定、o200k 误差（借外部参考实现）、composer 上限、模型菜单、隐藏生成、审批耗时分布、临时对话跨情形行为（连接器自动配置与 T3 挂起时限移为阶段 4 前置 spike） | 形成可复现的实测结论，决定并发参数、空闲阈值、上下文窗口与 composer 上限；任一关键假设不成立时回到设计基线 §0.2，由用户重新决策 |
| 阶段 1：应用级会话与显示面 | BrowserAuthority app scope 与 `session_id → owner` 替换（不迁移，C40）、**owner 分支的孤儿回收 / 资源回收 / 配额**、应用级创建入口、持久分区 `persist:magi-web-model`（A21，含宿主 partition 白名单）、浏览器 durable state 加载路径改为可重建（C40）、推理页面容器与 `WebModelTabContent`、右栏第三项与 `appTabs`、A4 关闭语义与自动恢复（C12）、跨会话例外规则、浏览器工具不可见（C10）；同步修改 `docs/browser-runtime-design.md`（C11） | 用户登录一次后，跨项目/会话切换仍可用；**重启 Magi 后登录态仍在**；损坏或版本不认识的浏览器状态不会让 daemon 起不来 |
| 阶段 2：发现通道 | 只读探测接口、`ModelEngine.origin`、设置页"连接 / 刷新" | Web 模型出现在模型列表并明确标注来源 |
| 阶段 3：推理通道 | `BrowserWebModelBridgeClient`、`chatgpt_web` 传输标识、应用级绑定存储（C39、§5.12）、原子写入与观察命令、并发上限（含队列上限与等待超时）、推理页面按需创建与空闲释放、取消/超时/接管与绑定所有权状态机（§5.8）、可用性投影与首次说明、Web 计数器与上限表（o200k，含依赖或随包词表的落地选择）、对话实例与 epoch、续轮封装与工具结果去重账本、增量与重锚块、累计账与前缀指纹、发送前硬校验与超限映射（设计基线 §5.6、§5.9） | Web 引擎可正常收发消息，turn/item/SSE/UI 不改；工具档位为 T0 |
| 阶段 4：工具能力（必需） | 递进交付 T2 文本协议回路 → T3 MCP 桥接（`magi-web-harness`、turn 令牌、挂起式调用与回填两条路径、连接器自动配置、站点侧确认处理）；**T1 已按用户决定删除**（R48）；T3 通道为 Magi Connect 优先、Connect 未就绪时以 OpenAI Tunnel 正式交付（A10、R47） | Web 模型可真实读写文件、跑命令、调用 MCP / skill；T2 无新增组件与对外暴露面；T3 由 Connect 或 OpenAI Tunnel 交付，用户不需要手动配置 ChatGPT 侧连接器（OpenAI Tunnel 的 Tunnel 与 API 密钥需用户在 OpenAI 平台创建，Magi 只引导） |
| 阶段 5：打磨 | 额度与状态展示、诊断与自检；**分片发送另立专项**（设计基线 §5.9.6，本阶段只保留重建路径所需的最小分片） | 长上下文与异常场景有明确行为与提示 |

阶段 1 与阶段 2 之间不要插入阶段 3 的半成品：Web 引擎在推理通道可用之前必须显式标注为不可用。

阶段 4 是**必需阶段**，不是收尾可选项。T2 不新增组件、不新增对外暴露面，可以立即推进；T3 是多步项目级开发的必要条件，与 T2 并行立项，且通道有两条正式形态，不被 Connect 排期阻塞。阶段 4 的任何档位都不改变阶段 1–3 的交付形态。

阶段 1 的验收以 §9.2 真实桌面验收第 1–5 条 **加上第 18 条**（登录一次后跨项目/会话可用、重启后登录态仍在、损坏状态不影响启动）。

---

## 2. 阶段 0 实测清单（开工前必须完成）

每一项都要留下可复现的记录并在文末标注日期。**统一记录模板**：环境（`MAGI_STATE_ROOT` 固定隔离目录、构建版本、Desktop/Electron 版本、daemon 端口）＋ 执行步骤 ＋ 原始证据（截图 / 日志 / 脚本输出路径）＋ 结论与阈值。任一项结论不利都要回到设计基线 §0.2 重新决策，不得带着未验证的假设进入阶段 3。

| # | 实测项 | 方法 | 产出 |
| --- | --- | --- | --- |
| 0.1 | 登录态跨重启保留（A21） | 在 `persist:magi-web-model` 分区登录后关闭并重启 Desktop；**同时确认应用级 `browserSessionId` 稳定、未被重新创建**，以及 `magi-desktop:clear-browser-data` 未被触发 | 明确「重启后仍登录」；记录实际分区目录与 cookie 文件存在性 |
| 0.2 | 各登录方式可用性（C9） | 邮箱 / Google / Microsoft / Apple 逐一试，记录 popup 是否被阻止 | 可用方式清单；结论写进 Magi 登录提示条（不注入 ChatGPT 页面） |
| 0.3 | 临时对话冷启动延迟 | 连续新建临时对话并计时（≥30 次） | 中位数与 P95，用于定空闲阈值（§5.6） |
| 0.4 | 可用窗口标定（C24） | 首尾标记法，按 账号等级 × 模型族 × effort × 是否带连接器 四维取值；每格重复 ≥3 次取稳定值；明确列出参与标定的账号等级与 4–6 个模型族 | 上限表初值：`input_token_budget` / `response_reserve` / `limit_behaviour` |
| 0.5a | o200k 计数误差标定（C25） | **用外部固定版本的 o200k 参考实现**（工具名 + 版本 + 哈希记录在案）对 0.4 的样本计数，与实测可容纳量比对 | 误差系数（< 1 的安全系数）或确认无偏；该系数在阶段 3 计数器落地后被 0.5b 复核 |
| 0.5b | 计数器落地后复核 | 阶段 3 的 Rust 计数器对同一批样本重算 | 复核结论；不一致即回到阶段 0 重标定 |
| 0.6 | composer 单条上限与超限表现 | 递增长度写入并回读 | 字符上限；超限是报错、转附件还是静默截断 |
| 0.7 | 模型菜单可读性 | 打开站点模型选择器读取 | `family → effort` 映射表与站点显示名 |
| 0.8 | 隐藏页面能否持续生成 | 折叠右栏、切会话、切窗口时观察 | 确认非活动推理页面可持续生成与读取（本方案不设预热池，见设计基线 §5.6），以及保活还是每次释放重建更合适 |
| 0.11 | 审批耗时分布（A18） | 统计真实任务的工具轮数与人工审批耗时（≥20 个工具调用样本） | 同回复续接与回填的比例，据此定挂起时限 |
| 0.12 | 临时对话跨重载 / 重启 / 崩溃的实际行为（C18） | 三种情形各试一次 | 只用于给默认空闲阈值与预期额度消耗定价，不是 go/no-go |

**移出阶段 0 的两项**（它们的前置件在后续阶段才存在，留在阶段 0 会构成循环依赖）：

| # | 实测项 | 前置件 | 位置 |
| --- | --- | --- | --- |
| 0.9 | 连接器支持性与自动配置（C36） | 需要先有连接器自动配置能力与 T3 通道（OpenAI Tunnel 或 Connect） | 阶段 4 前置 spike（§4.3 4.6/4.7/4.9） |
| 0.10 | T3 挂起时限 | 需要 T3 通道可用（正式通道：OpenAI Tunnel 或 Connect；Quick Tunnel 只用于对照） | 阶段 4 前置 spike（用正式通道实测，A10） |
| 0.13 | OpenAI Tunnel 端到端可用性（A10、R47） | 按参考项目形态创建 Tunnel 与仅含 **Tunnels Read + Use** 权限的 API 密钥，启动本地 harness（stdio），在托管浏览器内创建 **Tunnel 类型**连接器（Authentication: None）并回读确认；实测一次同回复工具调用与一次回填 | 端到端成功证据；确认不需要公网地址与入站端口；记录 `tunnel-client` 固定版本与 SHA-256 校验；确认凭据未出现在命令行 / 日志 / 诊断输出 |

## 3. 阶段 1 开工清单（文件级）

阶段 1 的完成标志（设计基线 §5.1–§5.4）需要下面这张清单全部完成；只做前 5 项交不出完成标志。

| 顺序 | 改动 | 文件 | 验证 |
| --- | --- | --- | --- |
| 1 | `BrowserSession.session_id` → `owner: BrowserSessionOwner`；推进 `BROWSER_DURABLE_STATE_SCHEMA_VERSION`，不写迁移 | `crates/magi-browser-authority/src/domain.rs`、`authority.rs` | `cargo test -p magi-browser-authority` |
| 2 | `load_browser_authority` 改为全函数（结构反序列化失败 / 未知版本 → 按「无状态」重建并覆盖写回，不报错，C40），并**删除** `restore_durable` 的 `PREVIOUS_BROWSER_DURABLE_STATE_SCHEMA_VERSION` / `LEGACY_BROWSER_DURABLE_STATE_SCHEMA_VERSION` 分支与 `migrate_legacy_tab_order`（A19 不做迁移）；`session_for_magi_session` 只匹配 `Session` 分支；App 级会话的创建、恢复与「不因无对应会话被当孤儿清除」 | `crates/magi-api/src/state.rs`（`load_browser_authority`、`reconcile_browser_sessions_with_session_store`、`close_browser_session_for_magi_session`）、`routes/browser.rs` | `cargo test -p magi-browser-authority`、`cargo check -p magi-daemon` |
| 3 | App 级创建入口：**固定为新增 `POST /browser/sessions/app`**（不改既有 `CreateBrowserSessionRequest` 形状）；创建前先查已有 App 级会话，保证 `browserSessionId` 稳定；请求/响应字段见 §7.5 | `crates/magi-api/src/routes/browser.rs` | `cargo test -p magi-daemon` |
| 4 | 配额与回收按 owner 分支：`is_reclaimable_tab`、`live_tab_count_for_session`、`MAX_BROWSER_TABS_*` 判定不吃 App 级会话；`/browser/resources/reclaim` 的可回收快照同步 | `crates/magi-browser-authority/src/authority.rs`、`crates/magi-api/src/routes/browser.rs` | `cargo test -p magi-browser-authority` |
| 5 | 持久分区（A21）：应用级会话使用 `persist:magi-web-model`，宿主 partition 白名单、分区配置**与分区注册表过滤规则（`readPartitionRegistry` / `persistPartitionRegistry`）**同步放行；**Renderer 侧是第三处派生点**：`BrowserTabContent.svelte` 现自行拼 `magi-browser-<id>`，新的 `WebModelTabContent.svelte` 对应用级会话必须使用固定分区 `persist:magi-web-model` | `apps/desktop/src/main/browser-webview-security.ts`、`browser-surface-manager.ts`（`browserPartitionId`、`configurePartition`、`readPartitionRegistry`、`clearBrowsingData`）、`web/src/components/tabs/WebModelTabContent.svelte` 及相关测试 | `npm run test --workspace @magi/desktop` |
| 6 | 跨会话例外与自动恢复：切换项目 / 会话不释放 App 级 guest；`Suspended` 收到推理调用时自动恢复并重新物化，Tab 回后台不抢焦点（C12） | `apps/desktop/src/main/browser-surface-manager.ts` | `npm run test --workspace @magi/desktop` |
| 7 | 右栏第三项与 app 级 Tab：`RightPaneTabKind` 增加 `webSession`；store 新增与 `perSession` 并列的 `appTabs`（窗口级、仅进程内，由 daemon 投影重建）；`webSession` 不进 `perSession`，也不加进 `tabsForPersist` / `isRestorableTab`；`addablePaneKinds` 项改为「始终渲染 + `enabled` + `disabledReason`」；关闭按钮只做本地隐藏（不调 `closeBrowserTab`） | `web/src/web/RightPane.svelte`、`web/src/stores/right-pane.svelte.ts` | `npm --prefix web run check`、`npm --prefix web run test:right-pane` |
| 8 | 新增内容组件 `web/src/components/tabs/WebModelTabContent.svelte`（同一内容槽承载主页与当前会话推理页面；切换只切可见宿主） | `web/src/components/tabs/WebModelTabContent.svelte` | `npm --prefix web run check` + 真实桌面验收 |
| 9 | App 级会话不暴露给任何会话的 `browser_*` 工具（C10） | `crates/magi-api/src/routes/browser.rs`、浏览器工具目录投影 | `npm run browser-tool-catalog:check`、`npm run test:browser-core` |
| 10 | 新增文案（含 Tab label / icon、禁用原因、后台推理指示） | `web/src/i18n/zh-CN.json`、`en-US.json` | 两套字典 key 一一对应（见 §6 的 i18n 清单） |
| 11 | 同步「应用级资源例外」（C11）：持久分区、关闭语义、多窗口 Primary 规则（A24） | `docs/browser-runtime-design.md` | 文档评审 |

## 4. 阶段 2–5 实现工单

文件级清单只列“本方案必须动到”的入口；具体函数以当前源码为准，不把易腐行号写进文档。

### 4.1 阶段 2：发现通道与模型来源

| # | 工单 | 主要文件 / 边界 | 验证 |
| --- | --- | --- | --- |
| 2.1 | 新增 ChatGPT 站点只读探测命令：登录态、账号能力、模型菜单、effort、连接器支持性 | `contracts/desktop-browser/desktop-control.schema.json`、`browser-automation-worker/src/runtime.ts`；只读，不进入模型可见浏览器工具目录 | `npm --workspace @magi/desktop-browser-contracts run check`、`npm run check --workspace @magi/browser-automation-worker` |
| 2.2 | 新增 `POST /browser/web-models/discover`，只返回候选，不落库 | `crates/magi-api/src/routes/browser.rs`（或同域新模块）；与 `/settings/models/fetch` 同构 | `cargo check -p magi-daemon` |
| 2.3 | Web 引擎条目形状与 `apiProtocol = chatgpt_web`：`ModelApiProtocol` 新增 `ChatGptWeb` 变体，`to_http_protocol` / `to_http_model_client` 显式拒绝；`upsert_engine` / `normalize_engine_entry` 放行不写 `llm` 的 Web 引擎；settings-store 规范化不得对 Web 引擎补 `openai_chat` | `crates/magi-conversation-runtime/src/model_config.rs`、`crates/magi-api/src/routes/settings.rs`、`crates/magi-settings-store/src/lib.rs`、`web/src/shared/types/agent-types.ts` | `cargo test -p magi-conversation-runtime`、`cargo test -p magi-settings-store`、`cargo check -p magi-daemon`、`npm --prefix web run check` |
| 2.3b | `ModelEngine.origin`：落在 settings `engines`（Rust 侧无类型 JSON）与 `web/src/shared/types/registry-types.ts`；**不涉及 App Server schema，无生成物变更** | `web/src/shared/types/registry-types.ts`、settings 读写路径 | `npm --prefix web run check`、settings 路由测试 |
| 2.4 | 「设置 → 浏览器 → GPT Web 模型」分区：连接 / 刷新入口、来源标注、只读窗口展示、全部非 `available` 状态卡片与主行动 | `web/src/components/SettingsBrowserTab.svelte`（或同级新组件）；i18n 文案 | `npm --prefix web run check` |
| 2.5 | 未登录 / 登录过期时隐藏 Web 模型；会话内选择器只展示 daemon 投影，不在前端保留旧列表 | 会话内主模型选择器（`web/src/components/InputArea.svelte` 及其选择器组件）与设置分区 | `npm --prefix web run check` |

### 4.2 阶段 3：推理通道与上下文一致性

| # | 工单 | 主要文件 / 边界 | 验证 |
| --- | --- | --- | --- |
| 3.1 | 新建 `magi-web-model` crate，落地 `BrowserWebModelBridgeClient` 的流式、可取消、显式错误语义 | 新 crate；`crates/magi-daemon/src/daemon/runtime.rs` 装配；`apiProtocol = chatgpt_web` 白名单 | `cargo test -p magi-web-model`、`cargo check -p magi-daemon` |
| 3.2 | 应用级绑定存储：绑定键、页面身份、累计账、前缀指纹、最近一次已接受发送、所有权状态、账号提示（记录 schema 见 §7.4） | `<state_root>/web-model/bindings.json`，复用 `crates/magi-api/src/state.rs` 的 `RuntimeStatePersistence::save_json`（该函数是 `pub(crate)`：owner 落在 `magi-api` 内可直接复用，否则必须先提供等价的 `pub` 包装，不允许另起写路径）；**不新增 `state-layout` 登记项、不推进其版本**；**不改** `SessionRuntimeSidecar` / `SessionDurableState` | `cargo test -p magi-daemon`（读写与损坏重建）、`cargo test -p magi-web-model` |
| 3.3 | 页面原子纯文本写入、回读校验、短轮询观察命令（**新命令**，不复用既有 `typeText`：它走 CDP `Input.insertText`，没有回读校验） | `contracts/desktop-browser/desktop-control.schema.json`（`worker-ipc.schema.json` 只引用，无需单独改）、`browser-automation-worker/src/runtime.ts` | `npm --workspace @magi/desktop-browser-contracts run check`、`node scripts/verify-browser-contracts.mjs`、`npm run test --workspace @magi/browser-automation-worker` |
| 3.4 | 对话实例与 epoch、新建 / 续轮 / 重建三路径、续轮封装与重锚块（含**重锚封顶**：≤ `effective_request_limit` 5% 且 ≤ composer 5%，按优先级裁剪 `constraints`） | `crates/magi-web-model`；渲染与解析唯一实现在站点适配层 | `cargo test -p magi-web-model` |
| 3.5 | 累计账、前缀指纹、发送前硬校验、`ContextLengthExceeded` 归一；累计账作为**传输层锚点**接入上下文压力快照 | `crates/magi-web-model`、`crates/magi-conversation-runtime/src/context_authority.rs`、`crates/magi-usage-authority/src/context_pressure.rs` | `cargo test -p magi-web-model`、`cargo test -p magi-conversation-runtime`、`cargo test -p magi-usage-authority` |
| 3.6 | Web 引擎 o200k 计数器与预算接入：`ContextAuthority` 引入按引擎选择的 TokenCounter，`estimate_chat_messages_tokens` / `estimate_tool_definition_tokens` 改为接收计数器并同步全部调用点；上限表数据文件（路径、字段 schema、owner、校验命令） | `crates/magi-conversation-runtime/src/context_authority.rs`、`conversation_loop.rs`、`session_turn_execution.rs`、`browser-automation-worker` 上限表数据 | `cargo test -p magi-conversation-runtime` |
| 3.7 | 可用性与工具档位投影、首次说明 consent、`refresh_required` | daemon 投影 + 设置页 / 会话内展示 | `cargo check -p magi-daemon`、`npm --prefix web run check` |
| 3.8 | 推理页面按需创建 / 复用与空闲释放、并发上限、队列深度与等待超时 | `crates/magi-web-model`；复用 BrowserAuthority 配额与租约语义 | `cargo test -p magi-web-model` |
| 3.9 | 取消、超时、用户接管、`MagiOwned` / `UserOwned` / `Invalidated` 状态机 | `crates/magi-web-model`、`crates/magi-browser-authority` 租约边界 | `cargo test -p magi-web-model`、`cargo test -p magi-browser-authority` |
| 3.10 | 引擎级开关：工具能力 + 「每轮新建对话」（默认关闭） | settings `engines` 字段、设置页、i18n | `cargo check -p magi-daemon`、`npm --prefix web run check` |
| 3.11 | GPT Web Tab 的「重置为 Magi 对话」与「清除数据」（二次确认、先取消进行中推理、按应用级分区粒度清 partition、失效绑定、引擎转 `login_required`；不误伤其他 partition） | `web/src/components/tabs/WebModelTabContent.svelte`、`apps/desktop/src/main/browser-surface-manager.ts`（`clearBrowsingData`）、daemon 绑定存储 | `npm run test --workspace @magi/desktop`、`cargo test -p magi-daemon` |
| 3.12 | 会话内阶段投影（等待 Web 引擎 / 浏览器生成中 / 排队中 · 第 N 位）与失败卡片主行动 | 现有运行时投影通道、`web/src/components/TurnRuntimeIndicator.svelte`、`ModelFailureCard.svelte`、i18n | `npm --prefix web run check`、真实桌面验收 |

### 4.3 阶段 4：工具能力（T2 → T3）

> **T1 已删除（R48）**：不接受"上下文内联"作为交付档位。编号保留空洞（从 4.2 起），以兼容既有引用。

| # | 工单 | 主要文件 / 边界 | 验证 |
| --- | --- | --- | --- |
| 4.2 | T2 文本协议：固定模板、`magi-tool-call` 解析、轮数上限、错误码 | `crates/magi-web-model` | `cargo test -p magi-web-model`、真实桌面验收 |
| 4.3 | T3 `magi-web-harness`：**两种传输共用同一实现**——Streamable HTTP（127.0.0.1 随机高端口 + `state_root` 端口文件，供 Connect 转发）与 **stdio**（`--stdio`，供 `openai/tunnel-client` 以子进程拉起）；固定桥接工具、turn 令牌 | `crates/magi-web-model`（harness 模块） | `cargo test -p magi-web-model`、真实桌面验收 |
| 4.4a | **T3 v1**：立即应答 + 同一对话下一轮回填（不含挂起对齐），等价参考项目的 `codex_tool_call`；`provider_context.pending_tools` 契约见 §7.3 | `crates/magi-web-model`、`crates/magi-conversation-runtime/src/conversation_loop.rs` | `cargo test -p magi-web-model`、`cargo test -p magi-conversation-runtime` |
| 4.4b | **T3 v2**：`tools/call` 应答挂起 + 同回复续接 + 挂起时限与回填切换 | 同上 | `cargo test -p magi-web-model`、`cargo test -p magi-conversation-runtime`、真实桌面验收 |
| 4.5 | 工具结果去重账本：以 `turn_id + tool_call_id` 为准，不重复执行 | `crates/magi-web-model`；canonical 事实由 loop 写入 | `cargo test -p magi-web-model` |
| 4.6 | 连接器自动配置、支持性探测、回读确认、失败降级 T2（含阶段 0 移入的 0.9 spike）；按当前通道写入配置：Connect 设备地址 / 凭据，或 OpenAI Tunnel 的 **Tunnel 类型 + Authentication: None**；记录 `origin.connector = { id, channel, revision, configuredAt }` | `browser-automation-worker` 站点适配命令；引擎 `origin.connector` | `npm run check --workspace @magi/browser-automation-worker`、`npm run test --workspace @magi/browser-automation-worker`、真实桌面验收 |
| 4.7b | T3 挂起时限 spike（阶段 0 移入的 0.10）：站点侧与隧道侧超时实测，本地收口必须更早 | 正式通道实测（OpenAI Tunnel 优先；Connect 就绪则用 Connect）；Quick Tunnel 只做对照 | spike 记录；结论写回设计基线 §5.7.3 |
| 4.8 | 多窗口：非 Primary 窗口提示「本窗口未承载当前推理，切换窗口即可接管查看」，不复制第二份用于推理的 Surface（A24） | `web/src/components/tabs/WebModelTabContent.svelte`、`web/src/stores/right-pane.svelte.ts` | `npm --prefix web run check`、真实桌面验收 |
| 4.7 | Magi Connect 侧稳定公网 MCP 地址与可吊销设备凭据（**Connect 就绪时优先采用；未就绪不阻塞 T3 交付**） | 外部依赖；接口见 §7.2 的 A 表，需求写回 Connect 方案 | Connect 评审 + 端到端 spike |
| 4.9 | **OpenAI Tunnel 通道交付**（Connect 未就绪时的正式路径）：`openai/tunnel-client` 固定版本 + SHA-256 校验、按需启动 / 停止、stdio 拉起 `magi-web-harness --stdio`、凭据按文件引用（仅 Tunnels Read + Use，不进命令行 / 日志 / 诊断）、MCP 设置页引导用户创建 Tunnel 与 API 密钥、连接失败 fail closed 并降级 T2 | `crates/magi-web-model`（tunnel 托管模块；可参照 `crates/magi-api/src/tunnel.rs` 的 cloudflared 托管模式，不复用其协议与鉴权）；宿主 sidecar 规则 | `cargo test -p magi-web-model`、真实桌面验收（§9.2 #34） |

### 4.4 阶段 5：打磨与诊断

| # | 工单 | 主要文件 / 边界 | 验证 |
| --- | --- | --- | --- |
| 5.1 | 额度与状态展示：按实际发送次数计数，账号级额度与会话预算分离 | `crates/magi-usage-authority`、Web 状态展示 | `cargo test -p magi-usage-authority`、`npm --prefix web run check` |
| 5.2 | 自检与诊断：登录探测、站点结构、接近上限标记回读、T3 通道连通性；入口为设置分区与失败卡片 | daemon 诊断入口；只读，不写 canonical | `cargo test -p magi-web-model`、真实桌面验收 |
| 5.3 | 分片发送单独立项；本期只保留超限压缩后重建 | 设计边界见设计基线 §5.9.6 | 设计评审 |


---

## 5. 工作量与周期预估

估算口径：**一名熟悉本仓库的 Rust 工程师全职 + 0.5 名前端工程师**，复用现有内置浏览器、隧道托管与上下文架构；不含 Magi Connect 侧的工作量，也不含阶段 0 的账号 / 环境等待成本。OpenAI Tunnel 在本仓的托管与凭据引导已计入阶段 4（T3）。数字是量级判断，不是排期承诺。

| 阶段 | 主要产出 | 粗估（人日，含自测） | 相对初版 |
| --- | --- | --- | --- |
| 阶段 0 | 实测结论与上限表初值 | 8–15 | ↑（初版 3–5 未计入四维标定、外部计数器借用与逐项记录成本） |
| 阶段 1 | app scope、`owner` 替换、owner 分支的孤儿 / 回收 / 配额、应用级创建入口、持久分区、加载路径可重建、右栏第三项与 `appTabs`、`WebModelTabContent`、A4 语义 | 8–14 | ↑ |
| 阶段 2 | 站点适配只读命令、发现接口、引擎条目形状与 `apiProtocol` 放行、`origin`、设置分区 | 5–8 | ↑ |
| 阶段 3 | 推理通道、原子写入与观察命令、完成谓词、绑定存储、状态机、计数器与累计账、可用性投影 | 28–45 | ↑ |
| 阶段 4（T2） | 文本协议回路、轮数上限与可见状态（T1 已删除） | 6–10 | ↓（R48） |
| 阶段 4（T3） | `magi-web-harness`（HTTP + stdio）、turn 令牌、挂起与回填、连接器自动配置、站点侧确认、**OpenAI Tunnel 托管与凭据引导** | 28–44 | ↑（R47 含通道落地） |
| 阶段 5 | 诊断、额度与状态展示、分片发送立项 | 5–9 | ≈ |

判断：

- **不含 T3** 的主链路（阶段 0–3 + T2）约 **47–77 人日**；**含阶段 5、不含 T3** 约 **52–86 人日**；**含 T3（含 OpenAI Tunnel 托管，不含 Connect 侧）** 约 **80–130 人日**。
- **T3 是全案最重的一块**。通道有两条正式形态（A10、R47）：Connect 就绪则优先复用，未就绪时用 OpenAI Tunnel 正式交付，因此 **Connect 排期不再是 T3 的阻塞关键路径**；OpenAI Tunnel 的额外工作量（`tunnel-client` 托管、凭据引导、连接器 Tunnel 形态）已计入上表。
- 阶段 1 与阶段 2 可以并行；阶段 3 依赖阶段 1 的 app scope，但可以与阶段 2 并行推进。
- 最大不确定性来自阶段 0 的三项实测：临时对话可用窗口标定、持久分区下的登录态跨重启、现有 popup 规则下各登录方式的可用性（C9）。任一项结论不利都会反过来改设计，因此阶段 0 必须先做完再全面投入阶段 3——阶段 0 的 8–15 人日是**设计冻结成本**，不是纯测量开销。

## 6. 协议与接口变更清单

| 变更 | 位置 | 生成物 / 验证 |
| --- | --- | --- |
| 新增原子纯文本写入 / 页面观察等 ChatGPT 站点适配命令 | `contracts/desktop-browser/desktop-control.schema.json`（命令联合），沿用 `worker_command` 转发链路；不进入模型可见的浏览器工具目录 | `npm --workspace @magi/desktop-browser-contracts run check`，按 `contracts/AGENTS.md` 执行 |
| `ModelEngine` 增加来源标注 `origin` | settings `engines`（Rust 侧无类型 JSON）＋ `web/src/shared/types/registry-types.ts`；**不涉及 App Server schema，无生成物变更** | `npm --prefix web run check` ＋ settings 路由测试 |
| 新增模型传输标识 `apiProtocol = chatgpt_web` | 四条同时改：① `crates/magi-conversation-runtime/src/model_config.rs`（`ModelApiProtocol` 增 `ChatGptWeb`，`to_http_protocol` / `to_http_model_client` 显式拒绝）；② `crates/magi-settings-store/src/lib.rs` 的模型配置规范化（**不得**对有连接字段但缺 `apiProtocol` 的 Web 引擎补 `openai_chat`）；③ `crates/magi-api/src/routes/settings.rs` 的 `upsert_engine` / `normalize_engine_entry`（放行不写 `llm` 的 Web 引擎）；④ `web/src/shared/types/agent-types.ts` 的 `ModelApiProtocol`；`HttpModelBridgeProtocol` 不新增取值 | `cargo test -p magi-conversation-runtime`、`cargo test -p magi-settings-store`、`cargo check -p magi-daemon`、`npm --prefix web run check` |
| `to_http_protocol` / `api_protocol()` 的返回类型必须能表达「非 HTTP」 | `crates/magi-conversation-runtime/src/model_config.rs`：两个函数现为非 `Option` 的具体枚举，调用方（如 `crates/magi-api/src/routes/settings.rs` 的穷尽 match）依赖它——只「不新增枚举取值」无法在编译期拦住 `chatgpt_web` 被当作 HTTP 协议处理。改为 `Option` / `Result` 并同步全部调用方 | `cargo test -p magi-conversation-runtime`、`cargo check -p magi-daemon` |
| `normalize_engine_entry` 必须保留 Web 引擎的顶层字段 | `crates/magi-api/src/routes/settings.rs`：现白名单只保留 `id / displayName / llm / runtime`，会丢弃 `apiProtocol` / `origin` / `contextWindowTokens` / `efforts` 与引擎级开关（工具能力、每轮新建对话），且缺 `llm` 时写入空对象 `{}`。需按 `apiProtocol = chatgpt_web` 分支保留这些字段 | `cargo test -p magi-daemon`、settings 路由测试 |
| 会话内选择器的数据源与强度档位 | `web/src/components/InputArea.svelte`：现选择器只列 provider `/settings/models/fetch` 的模型名（`pickerModels: string[]`），强度是固定四档（low / medium / high / xhigh）。需改为「provider 模型列表 ∪ daemon 投影的 Web 引擎条目」，强度按所选引擎的 `efforts` 渲染、其余置灰；Web 引擎切换强度会推进 epoch 并多消耗 1 条账号消息 | `npm --prefix web run check` |
| 会话级主模型覆盖必须保留 `engineId`（**阻塞项**） | `crates/magi-settings-store/src/lib.rs` 的 `canonicalize_session_orchestrator_section` 现用 `retain` 只保留 `model` / `reasoningEffort`，而所有会话 section 写入路径都会调用它：不修则用户选中的 Web 引擎在持久化时被静默剥离、重启即失效且不报错。同时明确 `engineId` **不进入** `ORCHESTRATOR_SESSION_DEFAULTS_SECTION` | `cargo test -p magi-settings-store`、`cargo test -p magi-conversation-runtime` |
| Web 引擎的 client 构造不得回退到 `default_client`（**阻塞项**） | `crates/magi-conversation-runtime/src/task_execution_dispatcher.rs`：`build_orchestrator_client` 现直接 `to_http_model_client()`，且返回 `None` 时 `resolve_target_for_role` 会回退到 daemon 注入的 `default_client`。需新增 `chatgpt_web` 分支构造 `BrowserWebModelBridgeClient`，并在该分支禁止任何回退（宁可报错，不得静默换 HTTP 模型）；`resolve_orchestrator_model_config` 产出的 `baseUrl / apiKey` 在该分支必须被忽略 | `cargo test -p magi-conversation-runtime`、`cargo check -p magi-daemon` |
| 角色 / 子代理继承编排模型的行为 | `crates/magi-conversation-runtime/src/task_execution_dispatcher.rs`（`resolve_model_client_for_task`：角色 `engineId` 为空时显式继承 orchestrator）、`agent_spawn_preflight.rs`：本方案**支持继承**（子代理同样由浏览器 client 承载，沿用并发与队列规则），并在前置检查里拒绝「继承到的 Web 引擎当前不可用」 | `cargo test -p magi-conversation-runtime` |
| 分区注册表过滤规则 | `apps/desktop/src/main/browser-surface-manager.ts` 的 `readPartitionRegistry` / `persistPartitionRegistry` 现按 `magi-browser-<id>` 过滤，`persist:magi-web-model` 会被静默丢弃 → 无 guest 挂载时「清理浏览数据」漏清该分区 | `npm run test --workspace @magi/desktop` |
| 上下文锚点键与测量来源 | `crates/magi-usage-authority/src/context_pressure.rs`：`ContextMeasurement` 现只有 Provider / Estimated / Compacted，需新增传输层取值；锚点匹配键需扩展为含 `epoch`；并把口径写回 `docs/context-pressure-compaction-architecture.md`（§6.1 与 DTO 章节），避免第二套事实源 | `cargo test -p magi-usage-authority`、`cargo test -p magi-conversation-runtime` |
| Web 侧上限表数据文件 | 随应用发布的数据文件（建议 `browser-automation-worker/src/web-model-limits.json`）：字段见设计基线 §5.9.2（含 `single_submission_token_budget`）、owner 为 `browser-automation-worker`、由站点适配命令回传 `limitsRevision`；消费方把可用窗口写入引擎**顶层** `contextWindowTokens`（Web 引擎不写 `llm`），并新增一条 Web 引擎解析路径把它交给 `ContextAuthority`——不能复用 `NormalizedModelConfig::from_settings_value`（该函数只读 `engines[*].llm`） | `npm run test --workspace @magi/browser-automation-worker`、`cargo test -p magi-conversation-runtime` |
| 站点适配新命令的 payload 契约 | 「原子纯文本写入 + 回读校验」与「短轮询观察」两条新命令进入 `contracts/desktop-browser/desktop-control.schema.json` 的 `$defs/command`；**字段契约以 §7.7 为准**，错误码见 §8 的 `web_write_not_confirmed` | `npm --workspace @magi/desktop-browser-contracts run check`、`node scripts/verify-browser-contracts.mjs` |
| Web 引擎在主对话的绑定路径（A22） | 会话级主模型覆盖新增引擎绑定字段：`crates/magi-api/src/routes/settings.rs`（`orchestrator_session_override_request` 接受 `engineId`）、`crates/magi-conversation-runtime/src/model_config.rs`（`resolve_orchestrator_model_config` 解析引擎）、`crates/magi-conversation-runtime/src/session_turn_execution.rs` 与 `crates/magi-api/src/dto/read_model.rs`（取模型的入口）、`task_execution_dispatcher.rs`（`apiProtocol = chatgpt_web` 时改走浏览器 client 工厂）；`ModelInvocationRequest` 不变 | `cargo test -p magi-conversation-runtime`、`cargo check -p magi-daemon` |
| 应用级持久分区（A21） | `apps/desktop/src/main/browser-webview-security.ts`（partition 白名单放行 `persist:magi-web-model`）、`browser-surface-manager.ts`（`browserPartitionId` / `configurePartition` / `readPartitionRegistry` / `persistPartitionRegistry` / `clearBrowsingData` 按应用级分区处理）、**`web/src/components/tabs/WebModelTabContent.svelte`（Renderer 侧第三处派生点，必须直接用固定分区）** 及其测试 | `npm run test --workspace @magi/desktop` |
| 应用级 Browser Session | BrowserAuthority 领域模型与 `/browser/*` 路由；含 `load_browser_authority` 的加载路径可重建化（C40），并删除 `restore_durable` 的 `PREVIOUS_/LEGACY_` 版本分支与 `migrate_legacy_tab_order`（A19 不做迁移） | `cargo test -p magi-browser-authority` 及跨层验证；补一条"durable state 损坏 / 版本不认识 → 重建且不报错、且不再走旧版本迁移分支"的回归 |
| 发现接口 `POST /browser/web-models/discover` | daemon HTTP 路由 | 与 `/settings/models/fetch` 保持同构 |
| 新 crate `magi-web-model` | 发现通道、`BrowserWebModelBridgeClient`、**T2 渲染与解析**（T1 已删除）、`magi-web-harness`（HTTP + stdio）、工具结果去重账本；由 `magi-daemon` 装配 | `cargo test -p magi-web-model` |
| 连接器自动配置（支持性探测、写入、回读确认） | 站点适配层命令，复用 §6 第一条的页面写入 / 观察命令；配置记录写进引擎的 `origin.connector = { id, revision, configuredAt }`，随 `engines` 持久化，不新增第二处存储 | `npm --workspace @magi/desktop-browser-contracts run check` |
| 引擎可用性与工具档位投影 | daemon | `cargo check -p magi-daemon` |
| 右栏第三项 Tab 与 payload | `web/src/stores/right-pane.svelte.ts`（`RightPaneTabKind` 增加 `webSession`、`RightPaneTabPayload`、与 `perSession` 并列的顶层 `appTabs`、`tabsForPersist` / `isRestorableTab` 白名单）；`web/src/web/RightPane.svelte`（`addablePaneKinds` 项改为「始终渲染 + `enabled` + `disabledReason`」）；新增内容组件 `web/src/components/tabs/WebModelTabContent.svelte`；文案进 `web/src/i18n/zh-CN.json` / `en-US.json` | `npm --prefix web run check`、`npm --prefix web run test:right-pane` |
| 来源标注、档位与可用性展示 | `web/src/shared/types/registry-types.ts`（`ModelEngine.origin`）、会话内主模型选择器、`web/src/components/SettingsBrowserTab.svelte` 的 GPT Web 分区、`ModelFailureCard.svelte` 的主行动按钮 | `npm --prefix web run check` |
| 会话内 Web 阶段投影 | 现有运行时投影通道新增可选字段（不新增 canonical 字段、不改 App Server schema）：等待 Web 引擎 / 浏览器生成中 / 排队中 · 第 N 位；承载组件 `TurnRuntimeIndicator.svelte` | `npm --prefix web run check`、真实桌面验收 |
| T3 的 `provider_context` 取值 | **不新增协议条目**：`ModelProviderContext.data` 是无类型 `Value`，`chatgpt_web/pending_tools` 由适配器自行校验 | 无需生成物；以 `cargo test -p magi-web-model` 覆盖回放与校验 |
| Web 引擎的 o200k 计数器 | `ContextAuthority` 引入按引擎选择的 TokenCounter；`estimate_chat_messages_tokens` / `estimate_tool_definition_tokens`（现为自由函数）改为接收计数器，并同步 `conversation_loop.rs`、`session_turn_execution.rs` 的 10+ 调用点；Rust 侧词表实现随 daemon 构建，引入依赖前按仓库依赖策略评审；其他引擎走 `magi-core::estimate_text_tokens` 不变 | `cargo test -p magi-conversation-runtime` 覆盖估算与阈值 |
| 传输层计数的上下文锚点（累计账） | `crates/magi-usage-authority/src/context_pressure.rs`（`ContextMeasurement` 现只有 Provider / Estimated / Compacted，需新增传输层锚点取值或字段）＋ 推理通道经响应用量字段报告 ＋ 按对话实例 epoch 失效 | `cargo test -p magi-usage-authority`、`cargo test -p magi-conversation-runtime` |
| 对话绑定记录（绑定键、页面身份、累计账、前缀指纹、最近一次已接受发送、所有权状态、账号提示） | **daemon 应用级绑定存储**（`<state_root>/web-model/bindings.json`，复用 `crates/magi-api/src/state.rs` 的 `RuntimeStatePersistence::save_json`，按绑定键索引；不新增 `state-layout` 登记项、不推进其版本）；**不改会话 sidecar**（C39：`SessionRuntimeSidecar` / `SessionDurableState` 带 `deny_unknown_fields`，写入会让新旧版本互读失败并造成写放大）；不写 settings，不进入 App Server schema。文件缺失或读不出来即按"无绑定"重建，不写迁移逻辑（设计基线 §6、A19） | `cargo test -p magi-daemon`（绑定存储读写与损坏重建）、`cargo test -p magi-web-model` |
| Web 引擎的身份绑定 | 见上方「Web 引擎在主对话的绑定路径」：引擎与 effort 由会话级覆盖决议后闭包进 client；`resolve_target_for_role` 只覆盖角色绑定，不作为 orchestrator 的引擎来源；`ModelInvocationRequest` 不变 | `cargo test -p magi-conversation-runtime` |
| Web 侧超限映射为 `ContextLengthExceeded` | `BrowserWebModelBridgeClient` 的错误归一 | `cargo test -p magi-web-model` |
| T3 通道 A：Magi Connect 设备连接层（优先） | 与 Magi Connect 共同定义（§5.7.4 与 §7.2 的 A 表）：稳定公网 MCP 地址 + 可吊销设备凭据 | 随 Connect 方案评审 |
| T3 通道 B：OpenAI Tunnel（Connect 未就绪时的正式交付） | `crates/magi-web-model`（tunnel 托管模块，可参照 `crates/magi-api/src/tunnel.rs` 的托管模式）：`openai/tunnel-client` 固定版本 + SHA-256 校验、按需启动 / 停止、**stdio 拉起 `magi-web-harness --stdio`**、凭据按文件引用（仅 Tunnels Read + Use，不进命令行 / 日志 / 诊断）；MCP 设置页引导用户创建 Tunnel 与 API 密钥；连接器（**Tunnel 类型**、Authentication: None）自动配置与回读；**不写 settings、不进 `state_root` 绑定存储** | `cargo test -p magi-web-model`、真实桌面验收（§9.2 #34） |

**i18n key 清单（zh-CN / en-US 必须一一对应，缺 key 会在界面直接显示 key 原文）**：`rightPane.addPanelWebModel`、`rightPane.webModelTabLabel`、`rightPane.addPanelWebModelDisabled`、`webModel.consent.title/body/confirm/cancel`、`webModel.status.{available,toolDegraded,refreshRequired,loginRequired,consentRequired,desktopRequired,siteBlocked,quotaExhausted}`、`webModel.badge.fromWeb`、`webModel.toolTier.{t0,t2,t3}` 与 `webModel.toolTier.degradedReason.*`、`webModel.tunnel.{channel,status,setupGuide,credentialMissing,clientMissing,identityMismatch}`、`webModel.quota.notice`、`webModel.quota.sentCount`、`webModel.turnStage.{waitingEngine,generating,queued}`、`webModel.takeover.notice`、`webModel.background.running`、`webModel.window.secondaryOnly`、`webModel.action.{login,logout,refresh,openHome,retry,resetConversation,switchModel,clearData,runDiagnostics,raiseRoundLimit,configureTunnel}`、`webModel.discovery.previewTitle/confirm/cancel`、`webModel.close.hiddenNotice`、`webModel.action.stopTurn/openView/focusMainWindow/cancelQueued`、`webModel.connector.configuring`、`settings.browser.clearDataWebNotice`、`settings.browser.webModel.*`。注意 `tabIcon` / `tabTooltip` 需要新增 `webSession` 分支，不属于文案键。

约定：任何协议字段变更都先改 schema 再生成，不手改生成文件；不随 UI 需求临时扩字段。§8 的错误码是 daemon 内部类型，集中定义在 `magi-web-model`，不进入对外 schema 与 App Server 生成物。
---

## 7. 接口契约

### 7.1 发现接口 `POST /browser/web-models/discover`

接口契约（`POST /browser/web-models/discover`，与 `/settings/models/fetch` 同构，只返回不落库）：

```jsonc
{
  "status": "ok | login_required | consent_required | desktop_unavailable | refresh_required | site_blocked | quota_exhausted | tool_degraded | failed",
  "reason": "selectors_drift | risk_page | quota | connector_unsupported | null",  // 可选；site_blocked / tool_degraded 时给出
  "accountHint": "plus | pro | free | unknown",
  "limitsRevision": "<上限表版本>",
  "engines": [
    {
      "id": "chatgpt-web/<family>",
      "displayName": "<站点显示名>",
      "apiProtocol": "chatgpt_web",          // Web 引擎不写 llm：无 baseUrl / apiKey / model
      "contextWindowTokens": 90000,          // 由上限表按账号等级 × 模型族写入，只读
      "efforts": ["low", "medium", "high"],  // 该族的取值域，会话内选择器据此置灰
      "origin": {
        "kind": "web",
        "browserSessionId": "...",
        "discoveredAt": 0,
        "accountHint": "plus",                      // 可选；发现时账号等级
        "connector": {                              // 可选；仅 T3 配置成功后写入
          "id": "...", "revision": "...", "configuredAt": 0
        }
      }
    }
  ]
}
```

缺省规则：没有 `origin` 的旧引擎等价于 `{ "kind": "user" }`；`accountHint` / `connector` 缺省表示未记录，不表示失败。返回的条目是**候选**，落库仍走既有 `engines` 写入路径：Web 引擎条目不写 `llm`，由 `upsert_engine` 的 `chatgpt_web` 分支放行。

未登录 / 登录过期时必须返回 `status = "login_required"` 且 `engines = []`；调用方不得把上一次的 Web 模型列表当作本次发现结果，也不得让 Web 模型进入会话内模型选择器。

**接口状态 ↔ 引擎状态映射**（唯一枚举以设计基线 §5.11 为准）：

| 接口 `status` | 引擎状态 | 选择器可见性 |
| --- | --- | --- |
| `ok` | `available` 或 `tool_degraded`（后者带 `reason = connector_unsupported`） | 可见、可选 |
| `refresh_required` | `refresh_required` | 可见但置灰 |
| `login_required` | `login_required` | 不可见 |
| `consent_required` | `consent_required` | 不可见 |
| `desktop_unavailable` | `desktop_unavailable` | 不可见 |
| `site_blocked`（`reason = selectors_drift` / `risk_page`） | `site_blocked` | 不可见 |
| `quota_exhausted` | `quota_exhausted` | 不可见 |
| `failed` | 不改变引擎状态（单次探测失败，可重试） | 维持原状态 |

### 7.2 T3 通道接口

T3 有两条正式通道（A10、R47）：**A. Magi Connect（优先）** 与 **B. OpenAI Tunnel（Connect 未就绪时的正式交付）**。字段名可调整，语义不得变。

**A. Magi Connect 设备连接层（草案，供 Connect 评审）**

| 项 | 草案 |
| --- | --- |
| 地址 | `https://<magi-connect-host>/mcp/<deviceId>`，每台 Desktop 一个且稳定，不随进程重启变化 |
| 传输 | Streamable HTTP MCP 单端点，至少支持 `tools/list` 与 `tools/call` |
| 鉴权 | `Authorization: Bearer <device-credential>`；设备凭据可吊销、可审计，并叠加 turn 令牌（§5.7.3） |
| 上游 | 只转发到本机 `magi-web-harness` 端口；经该入口不可达 daemon 的任何 `/api/*` |
| 生命周期 | 随 Desktop Connector 在线；离线返回 503，不由 ChatGPT 侧触发重试风暴 |
| 观测 | 每次调用记录 deviceId、tool、turn 令牌指纹（不记明文）与结果状态，供审计 |

**B. OpenAI Tunnel（按参考项目形态）**

| 项 | 形态 |
| --- | --- |
| 启动 | 按需启动 `openai/tunnel-client`（固定版本 + SHA-256 校验），由它以 **stdio** 拉起 `magi-web-harness --stdio`；随应用退出停止，不常驻 |
| 凭据 | OpenAI 平台 API 密钥，仅需 **Tunnels Read + Use**；以用户私有权限存储、**按文件引用**，绝不进命令行参数 / 日志 / 诊断输出；可吊销 |
| Tunnel | 用户在自己的 OpenAI 账号创建 Tunnel；Tunnel 与 API 密钥必须与 ChatGPT 工作区**同一账号** |
| 连接器 | 托管浏览器内自动创建 / 配置 **Tunnel 类型**连接器，Authentication: None，权限按参考项目形态设为「Allow all actions」并回读确认（只决定 ChatGPT 是否发动调用，不替代 Magi 审批） |
| 上游 | 只桥接到本机 `magi-web-harness`；不暴露 daemon 的 `/api/*` |
| 网络 | 纯出站 HTTPS，不暴露公网 IP、不开入站端口、不需要路由器转发 |
| 时限 | 隧道侧命令—响应上限约 2 分钟；本地 90 秒收口（§5.7.3） |
| 失败 | `tunnel-client` 缺失或校验失败、凭据缺失 / 无 Tunnels 权限、Tunnel 不存在、账号不一致、Developer Mode 或工作区策略不允许 → fail closed 降级 T2，并显示**具体缺哪一项**（§8 `web_tunnel_unavailable`） |

### 7.3 续轮封装格式与 `pending_tools` 契约

站点适配层唯一实现的续轮封装（设计基线 §5.6）：

```text
<<<magi-anchor v1
protocol: t2 | t3
tools_revision: <协议版本号>
goal: <当前任务目标，单行，可省略>
constraints:
<关键约束，逐行>
magi-anchor>>>

<<<magi-user
<本轮新增的用户消息原文>
magi-user>>>

<<<magi-tool-result
turn_id: <turn_id>
tool_call_id: <tool_call_id>
tool: <工具名>
status: ok | error
bytes: <正文长度>
---
<工具结果正文，长度与 bytes 一致；正文内不得出现未转义的定界符>
magi-tool-result>>>
```

| 规则 | 约定 |
| --- | --- |
| 定界 | 只有出现在行首的 `<<<magi-*` 视为块开始；块外内容一律忽略 |
| 包裹 | `magi-user` / `magi-tool-result` 正文按 `bytes` 长度读取，正文内的定界符必须转义（前缀插入零宽分隔或改用长度截取）；解析层拒绝任何嵌套块 |
| 工具调用块（响应侧） | ```` ```magi-tool-call ```` 内必须含 `turn_id`，与本次发送一致，否则整块忽略；`tool_call_id` 由推理通道生成为 `web-<turn_id>-<block_index>` |
| 未闭合 | 块开始后 3 个读取周期内仍未闭合 → `web_tool_protocol_invalid`，不写入 canonical |
| 重锚封顶 | ≤ `effective_request_limit` 的 5% 且 ≤ `composer_char_limit` 的 5%，超出按优先级裁剪 `constraints` |

`provider_context.pending_tools`（T3 续接凭据，`ModelProviderContext.data` 内，无类型 `Value`，不进入任何 schema 生成物）：

| 字段 | 类型 | 语义 |
| --- | --- | --- |
| `pageId` | string | 承载该对话实例的推理页面标识 |
| `turnToken` | string | 本次发送的 turn 令牌（不落日志、不落 UI） |
| `callIds` | string[] | 本批次工具调用 ID，**顺序即批次顺序**；续接时结果数量与顺序必须一致 |
| `prefixFingerprint` | string | 该临时对话当前持有内容的指纹（设计基线 §5.9.7） |
| 校验失败 | — | 指纹不一致或结果数量不符 → 推进 epoch 重建，不部分续接 |

### 7.4 绑定存储记录 schema

`<state_root>/web-model/bindings.json`：

```jsonc
{
  "schema_version": 1,
  "bindings": {
    "<session_id>|<engine_id>|<effort>|<epoch>": {
      "session_id": "...", "engine_id": "...", "effort": "medium", "epoch": 3,
      "page_id": "...", "page_url": "https://chatgpt.com/?temporary-chat=true",
      "ownership": "magi_owned | user_owned | invalidated",
      "account_hint": "plus | pro | free | unknown",
      "accumulated_tokens": 41234,
      "prefix_fingerprint": "...",
      "last_accepted_send": { "turn_id": "...", "sent_at": 0 },
      "updated_at": 0
    }
  }
}
```

- 只存指针与计数，**不存对话正文**；读不出来或版本不认识 → 视为「无绑定」并重建为空。
- 会话删除时按 `<session_id>|` 前缀清理；清理失败只记日志，不阻断会话删除。
- 引擎 / effort / epoch 任一变化即新键，旧键由空闲回收或会话删除清理。

### 7.5 App 级浏览器会话创建接口

`POST /browser/sessions/app`（`require_desktop_browser_capability` 不变）：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| 请求 | `{}` | 无参数；服务端先查已有 App 级会话，存在则直接返回，不新建 |
| 响应 `.session` | object | 与既有 `POST /browser/sessions` 的会话对象同构，`owner = "app"` |
| 响应 `.created` | bool | 本次是否新建（用于阶段 0 验证 `browserSessionId` 稳定） |
| 失败 | `browser_session_limit` / `desktop_unavailable` | 沿用既有错误形状 |

### 7.6 绑定所有权状态机（事件 → 迁移）

| 当前 | 事件 | 迁移 | 推进 epoch |
| --- | --- | --- | --- |
| （无绑定） | 首次发送、新建对话成功 | → `magi_owned` | 否（新键即新 epoch） |
| `magi_owned` | 同一 epoch 内的续轮 / T3 续接 | `magi_owned` | 否 |
| `magi_owned` | 用户在推理页面操作 / 页面模型或 effort 与档位不一致 | → `user_owned` | 下次发送时推进 |
| `magi_owned` | 登录态变化、页面被导航走、页面崩溃且状态不确定、账号级能力变化 | → `invalidated` | 是 |
| `user_owned` | 用户点「重置为 Magi 对话」 | → 旧键 `invalidated`，新建键 `magi_owned` | 是 |
| `invalidated` | 任意后续发送 | 旧键保留至清理，新建键 `magi_owned` | 是 |
| 任意 | 空闲释放、daemon / Desktop 重启、压缩安装检查点、切换档位 | 旧键退场，新建键 | 是 |

只有 `magi_owned` 可续轮；其余状态一律全量重放，且不尝试修补 web 侧对话。

### 7.7 站点适配命令 payload 契约（新增命令）

两条新命令进入 `contracts/desktop-browser/desktop-control.schema.json` 的 `$defs/command`（`worker-ipc.schema.json` 的 `worker_command` 只引用，无需单独改）；均不进入模型可见的浏览器工具目录。

| 命令 | 请求字段 | 响应 / 语义 | 失败 |
| --- | --- | --- | --- |
| `webWriteText`（原子纯文本写入 + 回读校验） | `binding`、`selector`、`text`、`mode: "replace" \| "append"`、`expectTextDigest`（sha256，可选）、`timeoutMs` | 写入后回读输入框文本并比对摘要；返回 `{ confirmed: true, digest, charCount, becameAttachment }`。`becameAttachment = true` 表示站点把内容转成附件 | 回读不一致或转为附件 → `web_write_not_confirmed`（设计基线 §5.9.6 的超限映射依赖 `becameAttachment`） |
| `webObserve`（短轮询观察） | `binding`、`selector`、`fields: ("text" \| "html" \| "existence" \| "attribute")[]`、`attribute?` | 单次快照，返回 `{ revision, nodes: [{ found, text?, html?, attributes? }] }`；`revision` 单调递增，供推理通道判断内容是否变化 | selector 不可达 → `found: false`，不报错（由完成谓词决定是否算失败） |

约定：`binding` 沿用既有 surface 绑定；两条命令都按 `binding.surface_id` 走既有命令 lane，因此长等待会占用该 surface，观察必须短轮询而不是长 `wait_for`（设计基线 §5.6 末）。`expectTextDigest` 的算法（sha256 / 归一化方式）在实现前冻结，写入与回读必须用同一算法。

---

## 8. 错误码与状态对照

错误码集中定义在 `magi-web-model`（daemon 内部类型，不进入对外 schema、不进入 App Server 生成物），避免各层各起一套名字。**凡是会到达用户的错误码，都必须在这里给出主行动**，并渲染在会话内失败卡片（现有 `ModelFailureCard` 只有「复制诊断」，需新增动作区）；无动作的错误不得只显示 code。

| 错误码 | 触发 | 呈现与主行动 | 是否重试 |
| --- | --- | --- | --- |
| `ContextLengthExceeded` | 发送前硬校验不通过 / 输入框转附件 / 页面提示过长（设计基线 §5.9.6） | 内部触发压缩与重建，**不出现在失败卡片** | 由 Magi 恢复状态机自动重试一次 |
| `web_send_rejected` | 写入后回读未确认、提交未被站点接受 | turn 失败 +「重试」 | 可重试；此时无副作用 |
| `web_write_not_confirmed` | 原子写入命令回读校验未通过（内容被转为附件或被改写） | turn 失败 +「重试」 | 可重试；尚未提交，无副作用 |
| `web_tool_protocol_invalid` | T2 工具块 JSON 不合法 / name 不在 `request.tools` / 块未闭合 | turn 失败 +「重试」（不自动纠错） | 不重试同一发送（避免协议污染） |
| `web_tool_round_limit` | T2 超过轮数上限（默认 20） | turn 失败 +「提高轮数上限并重试」 | 用户调整上限或拆分任务后重发 |
| `web_tunnel_unavailable` | T3 通道不可用：Tunnel 未创建 / API 密钥缺失或无 Tunnels Read + Use / `tunnel-client` 缺失或校验失败 / Connect 未就绪 | **不是 turn 失败**：引擎降级 T2，失败卡片与设置分区主行动「配置 OpenAI Tunnel」或「查看 Connect 状态」，并说明**具体缺哪一项** | 不自动重试；用户处理后重新探测 |
| `web_queue_full` | 并发队列达到深度上限（默认 16） | turn 失败 +「重试」 | 可重试 |
| `web_queue_timeout` | 排队等待超过该 turn 剩余时限 | turn 失败 +「重试」 | 可重试 |
| `magi_tool_timeout` | T3 挂起调用超过挂起时限 | **不是 turn 失败**：同一对话下一轮回填（§5.7.3） | 由回填路径继续 |
| `web_selectors_drift` | 站点改版导致 selector / 完成谓词失效 | turn 失败 +「运行诊断」+「重试」；**引擎转 `site_blocked`（`reason = selectors_drift`）**，不得停留在 `available` | 不自动重试 |
| `web_login_expired` | 登录探测失败或登录过期 | 引擎转 `login_required`；失败卡片主行动「重新登录」 | 用户处理后重发 |
| `web_site_blocked` | 风控 / 验证页 | 引擎转 `site_blocked`；主行动「去 GPT Web 主页处理」 | 不自动重试 |
| `web_quota_exhausted` | 账号额度用尽 | 引擎转 `quota_exhausted`；主行动「切换其他模型」，说明额度重置后可重新出现 | 不自动重试 |
| `web_desktop_unavailable` | 无可信 Desktop 连接 | 发送前拒绝，引擎转 `desktop_unavailable`；主行动「了解详情」 | 用户恢复 Desktop 后重发 |

本表与设计基线 §5.11 的引擎状态是两套东西：**§5.11 描述「引擎能不能用」（长期状态），本表描述「这一次调用为什么失败」（单次结果）**；上表中标注「引擎转 / 降级」的行会把引擎推进到对应状态，其余只影响该次 turn。两者都要投影到 UI，不得互相替代。

## 9. 验收

### 9.1 基础检查（按改动面）

先跑仓库级前置（`docs/browser-runtime-design.md` §11）：`cargo check --workspace`、`git diff --check`。

| 改动面 | 命令 |
| --- | --- |
| Web | `npm --prefix web run check`；右栏 Tab 行为变化时 `npm --prefix web run test:right-pane` |
| Desktop | `npm run check --workspace @magi/desktop`、`npm run test --workspace @magi/desktop`（partition 白名单与 `clearBrowsingData`） |
| Worker | `npm run check --workspace @magi/browser-automation-worker`、`npm run test --workspace @magi/browser-automation-worker` |
| Desktop Browser schema | `npm --workspace @magi/desktop-browser-contracts run check`、`node scripts/verify-browser-contracts.mjs`（新增命令的 payload 约束） |
| Browser Authority | `cargo test -p magi-browser-authority`（owner 分支、配额、`is_reclaimable_tab`、durable state 损坏 / 版本不认识 → 重建且不报错） |
| 模型配置与解析 | `cargo test -p magi-conversation-runtime`（`chatgpt_web` 的 HTTP 拒绝、引擎绑定解析、计数器接入）、`cargo test -p magi-settings-store`（不得补 `openai_chat`） |
| 上下文与额度 | `cargo test -p magi-conversation-runtime`、`cargo test -p magi-usage-authority`（传输层锚点） |
| `magi-web-model` | `cargo test -p magi-web-model`（含每个错误码的触发条件、主行动与是否可重试） |
| OpenAI Tunnel 托管（R47） | `cargo test -p magi-web-model`（`tunnel-client` 缺失 / SHA-256 校验失败 / 凭据缺失或权限不足的失败分支）、真实桌面验收（§9.2 #34） |
| daemon | `cargo check -p magi-daemon`、`cargo test -p magi-daemon`（App 级会话创建 / 恢复 / 绑定存储缺失 / 损坏 → 按「无绑定」处理） |
| App Server schema / 生成物 | 只有确实变更 App Server 契约时跑 `npm run protocol:check`（必要时先 `npm run protocol:generate`）；`origin`、`apiProtocol`、`provider_context` 与绑定存储都**不进入**这里 |
| Browser 核心静态契约断言 | `npm run test:browser-core`（`scripts/verify-browser-core-acceptance.mjs` 是源码契约断言，不是行为验证） |
| Web 回答同步与站点适配（§9.4） | `npm run test:web-model-dom`（**阶段 3 起才有该脚本**，交付前以 §9.4 为准）、`npm run test --workspace @magi/browser-automation-worker` |

### 9.2 真实桌面运行验收

必须的真实桌面运行验收（沿用 `docs/browser-runtime-design.md` §11 的口径）：

1. 首次使用：点击 GPT Web → 进入登录页 → 完成登录 → 页面可用；首次说明只出现一次。
2. 关闭该 Tab 再打开：视图恢复、无需重新登录；关闭期间进行中的推理不中断；重新打开可看到最新状态，只有显式取消才停止 turn。
3. 切换项目、切换会话、切换工作区：登录态与可用性不变。
4. 关闭 Magi 并重启：登录态仍在；页面可重新物化。
5. 资源回收操作不会破坏该应用级会话。
6. 模型发现：登录且发现成功后才出现 Web 模型清单并标注来源；未登录 / 登录过期时常规模型列表不出现任何 Web 模型，设置页只显示登录入口与“已隐藏”状态；站点改版导致的失败有明确报错。
7. 推理：发送、流式展示、取消、超时、用户接管都有确定行为；同一会话的多轮复用同一条临时对话，不同会话互不共享；ChatGPT 历史中没有新增记录。
8. 并发：达到上限时排队并有可见状态，不触发风控；队列满或等待超时以明确错误收口；推理页面隐藏时仍能完成推理，用户可切换查看并接管；查看对话实例不新建页面。
9. Web / 手机 Web / 无头 daemon：该能力不可用且给出明确原因，发送前拒绝。
10. 会话的浏览器工具看不到、也无法操作应用级会话。
11. T2：读文件、改文件（触发审批）、跑命令、调用 MCP、使用 skill 均成功，canonical 工具事实与 HTTP 引擎一致；协议不合法与超过轮数上限时明确报错。
12. T3（Magi Connect 与 OpenAI Tunnel 各跑一遍）：一个回复内的多次工具调用在同一条临时对话中完成；并行调用按批次返回；审批超过时限时在同一对话的下一轮回填结果，工具不重复执行；缺少或伪造 turn 令牌、请求 `request.tools` 以外的工具一律被拒绝；经通道访问 daemon 的 `/api/*` 不可达；重启 Magi 后连接器仍可用；回填使用 §5.6 的续轮封装；同一 `turn_id + tool_call_id` 重复到达时返回账本既有结果，不重复执行。
13. 上下文一致性：多轮对话接近标定窗口时，最早一轮的标记仍能被模型复述；累计账随每轮增长，与对话实际内容一致（旧工具结果在 Magi 视图中被缩减后，累计账不减少）；累计占用接近主动阈值时，Magi 先压缩再新建对话实例；分别制造发送前校验不通过、页面提示过长、输入框转为附件三种超限，Magi 都自动压缩并重建后成功；T3 中一次很大的工具结果触发压缩后，不再续接旧回复，工具结果随压缩后的上下文进入新的对话实例，工具不重复执行；在 Magi 一侧回退或编辑一条已发送消息后，下一轮推进 epoch 重建；账号等级变化后引擎显示需要刷新。
14. 所有权状态：用户在推理页面操作后绑定转入 `UserOwned`，下一次发送推进 epoch 并全量重放；登录态换号或账号级能力变化后绑定判为 `Invalidated`，不沿用旧累计账；"重置为 Magi 对话"后回到 `MagiOwned` 并正常续轮。
15. 默认档位与降档：连接器可用且 T3 通道就绪时引擎取 T3；账号 / 套餐不支持、连接器自动配置或回读失败、T3 通道不可用时自动落到 T2，并在引擎处显示档位、**具体缺哪一项**与额度影响，不静默切换。
16. 连接器自动配置：首次启用 T3 时 Magi 先确定通道（Connect 优先 / OpenAI Tunnel），再由站点适配层在托管浏览器会话中完成 ChatGPT 侧连接器配置并回读确认，用户没有手动进入 ChatGPT 设置；OpenAI Tunnel 的 Tunnel 与 API 密钥需用户先在 OpenAI 平台创建（Magi 引导，不可代做）；配置失败 fail closed 并降级 T2，显示具体缺哪一项。
17. 两条路径：自动放行的读操作在挂起时限内完成，走同回复续接；需要人工审批的写操作或命令超过挂起时限后走同一对话的下一轮回填，任务继续、工具不重复执行、额度按实际发送次数计数。
18. 可重建状态的加载（C40）：把 `browser/state.json` 写成损坏内容、或把 `schema_version` 改成不认识的版本，daemon 仍能正常启动，浏览器状态重建为空，partition 里的登录态不受影响；Web 对话绑定文件整体删除或损坏时，该会话照常可用，只是重新新建一条临时对话。
19. 用户资产不被改动（C39）：使用 Web 模型完成若干轮推理后，`SessionRuntimeSidecar` 与 `SessionDurableState` 的字段集合与未使用该功能时一致；会话与 canonical 事实不因本功能出现新字段或新写入路径。

20. **持久分区（A21）**：登录后重启 Desktop 一次，登录态仍在、无需重新登录；确认实际使用的是 `persist:magi-web-model` 分区目录；确认应用级 `browserSessionId` 未被重新创建。
21. **主对话绑定（A22）**：在会话内选择器里选中 Web 模型并发送成功；切换会话后各自的引擎选择互不污染；选择器里不出现 provider 连接（`orchestrator` 段）被改写的情况。
22. **清理与多窗口**：点击既有「设置 → 浏览器 → 清理浏览数据」后，进行中的 Web 推理被取消、Web 模型被隐藏、需要重新登录，且其他浏览器 Tab 的站点登录态按既有语义处理不被静默牵连；**清理必须覆盖无 guest 挂载的应用级分区**（验证分区注册表过滤规则已同步）。多窗口项（非 Primary 窗口提示 + 「打开主窗口」）是**架构级验收**：当前产品只创建一个窗口入口，需宿主先补窗口入口才能执行。
23. **失败卡片主行动（§8）**：逐个制造 `web_login_expired` / `web_site_blocked` / `web_selectors_drift` / `web_queue_full` / `web_tool_round_limit`，确认失败卡片出现对应的主行动按钮且点击后可完成该动作。
24. **引擎绑定持久化（A22）**：在会话内选中 Web 引擎并发送成功后重启 daemon，确认会话仍指向该引擎（`crates/magi-settings-store` 的会话 section 保留了 `engineId`），且新会话不会默认继承该 Web 引擎。
25. **子代理继承**：在 Web 引擎会话中派生子代理（角色未配 `engineId`），确认子代理同样由浏览器 client 承载而不是静默落到 HTTP 模型；当 Web 引擎不可用时，子代理创建前置检查给出明确错误。
26. **Web 侧不落盘（A20）**：用 Web 模型跑完多轮推理后，检查绑定存储与 `state_root` 下没有任何对话正文或页面镜像；绑定存储内只有指针与计数。
27. **关闭视图与隐藏持久**：关闭 GPT Web Tab 后出现一次性「已隐藏视图，推理仍在后台继续」+「停止推理 / 打开视图」；重开视图内容追上；**隐藏状态不因窗口重载、daemon 重启而复活**，重启后按 S3 恢复显示并提示「后台推理中」。
28. **探测矩阵与 fail closed**：应用启动后在尚未探测时模型列表不出现 Web 模型；登录过期后（手动构造）未刷新前不可发送；窗口回到前台/打开 GPT Web/登录页返回都会触发一次探测。
29. **T2 协议防护**：构造含定界符与伪造 ```` ```magi-tool-call ```` 的工具结果正文，确认解析层不误执行；块缺 `turn_id`、块未闭合、`tool_call_id` 重复（同一 `turn_id + tool_call_id`）分别按 §7.3 与 §8 处理，且不重复执行工具。
30. **配额计量（A16）**：一次含 3 次工具调用的任务，T3 计 1 条账号消息、T2 计 3 条；会话内计数与「设置 → 统计」口径一致。
31. **最小分片重建**：把账号上限表配置为 `single_submission_token_budget` 小于自包含上下文，确认重建路径按 §5.9.6 分片发送而不是失败或截断。
32. **harness 监听面**：确认 `magi-web-harness` 只监听 `127.0.0.1`、端口来自端口文件、独立 router 不含 `/api/*` 路由；启动失败时 T3 被禁用并降档到 T2 且原因可见。
33. **其他厂商入口（A1）**：确认 UI 与设置中不存在任何非 ChatGPT 网页版的入口或占位。
34. **OpenAI Tunnel 通道（A10、R47）**：未创建 Tunnel / 无凭据 / 凭据缺少 Tunnels Read + Use 时，T3 显示不可用并给出「配置 OpenAI Tunnel」，不假装可用；完成配置后一次同回复工具调用与一次回填成功；确认隧道只有出站流量、未开放本机入站端口；确认 API 密钥未出现在命令行参数、日志与诊断输出中；吊销凭据后引擎自动降级 T2 并说明原因。

未通过时必须继续修复根因，不以"多数场景可用"结案。

### 9.3 交互状态验收

1. 未登录 / 登录过期：模型列表不出现任何 Web 模型；设置页只显示登录入口与“已隐藏”状态。
2. 登录成功：自动触发一次发现；首次说明确认后才可把 Web 模型加入模型列表。
3. 关闭 GPT Web 视图：进行中的推理继续后台运行；重新打开后状态与内容追上；显式取消才停止。
4. 用户接管：在推理页面手动操作后，设置页 / 会话内提示已接管；下一次发送前按设计推进 epoch 并重建。
5. 模型 / 会话切换：登录态与 GPT Web Tab 不随项目、会话切换丢失；Tab 切到目标会话的临时对话。
6. 工具档位：默认取可用最高档；T3 不可用或配置失败时显示降级到 T2 的原因与额度影响；用户关闭工具后按 T0 行为。
7. 清除数据：二次确认后隐藏 Web 模型、失效绑定、关闭应用级会话；重开 GPT Web 进入登录页。
8. 异常状态：队列满 / 超时、登录过期、站点风控、selector 漂移、额度用尽都有明确提示与主行动按钮，不静默重试。
9. 多窗口 / 非 Desktop：每个窗口各有一份独立物理 guest、登录态共享；非 Primary 窗口显示「本窗口未承载当前推理，切换窗口即可接管查看」，不复制第二份用于推理的 Surface；Web / 手机 Web / 无头 daemon 不展示 Web 模型，「设置 → 浏览器 → GPT Web 模型」说明需要 Magi Desktop。
10. 关闭视图后的后台可见性：关闭 GPT Web Tab 后，「新增」菜单与设置分区显示「GPT Web（后台推理中 · N）」并可点回；推理结束后指示清除。
11. 模型选择入口：Web 模型出现在**会话内主模型选择器**并带「来自 Web」标识、账号提示与当前档位；不支持的 effort 档位置灰；切换强度时提示会重建对话并多消耗 1 条账号消息。
12. 首次使用闭环：点「新增 → GPT Web」→ 说明确认 → 登录 → 自动发现 → 候选确认 → 模型出现在选择器 → 首条消息发送成功；每一步失败都有停留位置与重试入口。
13. 错误主行动：§8 列出的每个可达错误码都渲染出主行动按钮（见 §9.2 第 23 条）。
14. 用量口径：会话内显示本次任务的账号消息条数；「设置 → 统计」中 Web 引擎不参与 token 聚合、单独按条数展示并标注「不计入会话预算」。
15. 探测与可见性：未探测 / 探测中 / 登录过期时不出现 Web 模型；已选引擎变为不可用时按钮置灰并给出「切换其他模型」。
16. 排队可取消：排队期间 turn 行有取消入口且取消后立即出队，不留下「还在排队」的残留状态。
17. 拒绝首次说明：点取消后停在 `consent_required`，模型不出现；再次点「新增 → GPT Web」可重新触发说明。

---

### 9.4 自动化验收（必须交付）

人工验收不足以作为回归防线：ChatGPT 站点结构随时可能变化。下列自动化验收是阶段 3–5 的交付物，不是可选项。

| 目标 | 做法 | 命令 |
| --- | --- | --- |
| Web 回答同步正确性 | 复用 `scripts/verify-electron-conversation-dom.mjs` 的「隔离状态根 + 本地假 Provider + 真实 App Renderer CDP 断言」模式，新增 `scripts/verify-web-model-dom.mjs`：以**本地假 ChatGPT 页面 fixture** 驱动登录探测 → 发现 → 发送 → 流式 → T2/T3 回路 → 取消 → 接管 → epoch 重建，断言 canonical turn/item 与界面展示一致（`content` / `thinking` / `tool_calls`，以及流式末值与终值一致） | `npm run test:web-model-dom` |
| 站点适配回归 | 站点适配层建立「录制 DOM 快照 fixture + 单测」：selector、模型菜单映射、effort 映射、完成谓词、原子写入回读，作为改版回归防线 | `npm run test --workspace @magi/browser-automation-worker` |
| Browser 契约 | 新增命令进入 `desktop-control.schema.json` 后必须通过 payload 约束校验 | `node scripts/verify-browser-contracts.mjs` |
| 重启与重放 | 复用既有 daemon 重启 / 重放断言，覆盖「绑定丢失 → 全量重放且工具不重复执行」 | `node scripts/verify-real-provider-restart-replay.mjs`（按现有脚本约定扩展） |

---

## 附录 A：代码位置索引

| 主题 | 位置 |
| --- | --- |
| 右栏新增菜单 | `web/src/web/RightPane.svelte`（`addablePaneKinds`、`canCreateBrowserPane`） |
| 右栏 Tab 类型与持久化 | `web/src/stores/right-pane.svelte.ts`（`RightPaneTabKind`、`perSession`、`tabsForPersist`、`activateRightPaneSession`） |
| Browser Authority 领域模型 | `crates/magi-browser-authority/src/domain.rs`（`BrowserSession`）；`authority.rs`（`MAX_BROWSER_TABS_PER_SESSION`、`MAX_BROWSER_TABS_TOTAL`、`is_reclaimable_tab`） |
| Browser HTTP 路由与 Desktop 能力门禁 | `crates/magi-api/src/routes/browser.rs`（`require_desktop_browser_capability`） |
| Partition 派生、持久化与后台节流 | `apps/desktop/src/main/browser-surface-manager.ts`（`browserPartitionId`、`configurePartition`（现为 `session.fromPartition(id, { cache: false })`，**无 `persist:` 前缀**）、`clearBrowsingData`、`setBackgroundThrottling(false)`）；`apps/desktop/src/main/browser-webview-security.ts`（`BROWSER_PARTITION_PATTERN`，需放行 `persist:magi-web-model`） |
| popup 单一决策链与非活动 Tab 保活 | `docs/browser-runtime-design.md` |
| Browser Worker 命令与输入 | `contracts/desktop-browser/desktop-control.schema.json`（`$defs/command`）、`worker-ipc.schema.json`（`worker_command`）；`browser-automation-worker/src/runtime.ts`（`typeText` 走 CDP `Input.insertText`，**不是**本方案要的原子写入 + 回读校验；需新增命令） |
| 模型传输边界 | `crates/magi-bridge-client/src/types.rs`（`ModelBridgeClient`、`ModelInvocationRequest`、`ModelResponse`、`ModelResponseStatus::RequiresToolExecution`、`ChatToolCall`、`ModelProviderContext`、`ModelStreamingDelta`） |
| `provider_context` 持久化与回放、工具执行 | `crates/magi-conversation-runtime/src/conversation_loop.rs` |
| daemon 组装业务模型客户端 | `crates/magi-daemon/src/daemon/runtime.rs`（`SettingsBackedBusinessModelBridgeClient`） |
| Web 引擎身份绑定与按会话解析模型 | `crates/magi-conversation-runtime/src/task_execution_dispatcher.rs`（`resolve_target_for_role`、`build_orchestrator_client`）；`crates/magi-conversation-runtime/src/model_config.rs`（`resolve_orchestrator_model_config`） |
| 会话 sidecar 存储（**本方案不修改**） | `crates/magi-session-store/src/store/sidecar.rs`（`upsert_runtime_sidecar`）；`crates/magi-session-store/src/models.rs`（`SessionRuntimeSidecar` 与其 `deny_unknown_fields` —— Web 对话绑定不得写入，C39） |
| 浏览器 durable state 的加载与 schema 版本 | `crates/magi-api/src/state.rs`（`load_browser_authority`，当前用 `?` 冒泡，需按 C40 改造；同文件还有 App 级会话必须绕开的 `reconcile_browser_sessions_with_session_store`、`close_browser_session_for_magi_session`，以及绑定存储要复用的 `RuntimeStatePersistence::save_json`）；`crates/magi-browser-authority/src/authority.rs`（`BROWSER_DURABLE_STATE_SCHEMA_VERSION`、`restore_durable`）；`crates/magi-daemon/src/daemon/persistence.rs`（`state-layout.json` 管道） |
| 引擎协议与上下文窗口 | `web/src/shared/types/agent-types.ts`（`ModelApiProtocol`、`LLMConfig`）；`crates/magi-conversation-runtime/src/model_config.rs`（`ModelApiProtocol` 枚举、`to_http_protocol`、`to_http_model_client`；`contextWindowTokens` 是 settings wire 字段，Rust 侧为 `context_window_tokens`） |
| 上下文压力、预算与超限恢复 | `docs/context-pressure-compaction-architecture.md`；`crates/magi-conversation-runtime/src/context_authority.rs`（`ContextAuthority`、`estimate_chat_messages_tokens`、`estimate_tool_definition_tokens`）；`crates/magi-usage-authority/src/context_pressure.rs` |
| 通用 token 估算 | `crates/magi-core/src/token_estimate.rs`（`estimate_text_tokens`） |
| 引擎清单与既有模型拉取先例 | `web/src/shared/types/registry-types.ts`（`ModelEngine`）；`crates/magi-api/src/routes/settings.rs`（`upsert_engine`、`/settings/models/fetch`） |
| skill 指令注入 | `crates/magi-skill-runtime/src/lib.rs`（`SkillPromptInjection`、`ResolvedSkillContext`） |
| canonical turn/item | `crates/magi-session-store/src/models.rs` |
| Magi MCP 客户端 | `crates/magi-bridge-client/src/mcp_client.rs`、`mcp_loopback.rs`；`crates/magi-tool-runtime/src/types.rs`（`ExternalMcpServerCatalogEntry`） |
| Cloudflare 隧道托管 | `crates/magi-api/src/tunnel.rs`（`TunnelManager`） |
| 公网请求鉴权与路径白名单 | `crates/magi-api/src/routes/mod.rs`（`enforce_public_tunnel_auth`、`is_public_tunnel_request`、`is_protected_remote_path`） |
| OpenAI Tunnel 参考实现 | [miuuyy/codex-chatgpt-web](https://github.com/miuuyy/codex-chatgpt-web) 的 README 与 `docs/security-model.md`：纯出站 HTTPS Secure MCP Tunnel、不开公网监听、不需要入站规则；运行时密钥仅需 Tunnels Read + Use、按文件引用、不进命令行 |
| Magi Connect 连接面与设备凭据 | `docs/magi-connect-mobile-plan.md` §6.2、§8、§9、§10、§13 |
| 工具运行时与策略 | `crates/magi-tool-runtime/src/registry.rs`、`policy.rs`；`crates/magi-api/src/routes/tools.rs` |

---

## 附录 B：修订与审核记录（C1–C42、R1–R46）

> 本节只保留“相对早期草稿”的修订过程，便于追溯为什么最终设计如此；它不是理解设计的前提，设计基线正文不再引用 C 编号。


以下修正只处理原方案中的缺陷或空白，不改变 §0.1 与 §0.2。

| 编号 | 原方案问题 | 修正 | 位置 |
| --- | --- | --- | --- |
| C1 | T2 被设计成独立的"回路状态机"，与现有 conversation loop 重复，形成第二条工具执行路径 | 推理通道把文本协议解析为 `ModelResponse.tool_calls` 并返回 `RequiresToolExecution`；执行、审批、canonical 全部由现有 loop 完成 | §5.7.2 |
| C2 | T3 在一次模型调用尚未返回时由外部回调执行工具，位于 loop 之外，违反 A7/A9 | 改为挂起式调用：MCP 调用到达即结束本次模型调用，把调用交给 loop 执行；loop 下一次调用时把结果交回，续读同一个 ChatGPT 回复 | §5.7.3 |
| C3 | T3 组件在原 §5.7.3 写成 stdio MCP server，在原 §5.7.4 又要求 Streamable HTTP 入口 | 统一为 daemon 内独立监听端口的 Streamable HTTP MCP 入口；通道只指向该端口 | §5.7.3 |
| C4 | `magi_turn_complete` 与站点适配层的完成谓词是两套收口来源；`magi_skill_read` 与已有能力重复 | 收口只以完成谓词为准，删除 `magi_turn_complete`；skill 指令已由上下文编译注入、skill 工具已在工具目录中，删除 `magi_skill_read` | §5.7.3 |
| C5 | warm pool 与并发上限需要多个页面，但只规定了一个右栏 Tab，页面宿主不明确 | 推理页面是应用级会话下的隐藏页面，复用现有"非活动 Tab 保活"机制（受管 guest 已关闭后台节流），计入总配额（**该方案后来被 R38 取代：预热池已删除**） | §5.2、§5.6 |
| C6 | 未规定 Desktop 未运行、没有窗口、无头 daemon 时 Web 引擎的行为 | daemon 投影引擎可用性，在模型选择与发送前明确拒绝 | §5.11 |
| C7 | "外部不留痕"表述过强 | 改为"不进入用户的 ChatGPT 历史记录"；临时对话内容仍由 OpenAI 处理 | §1.2 |
| C8 | 称 T3"复用" Magi Connect，但 Connect 现方案只覆盖自家设备连接，没有面向 ChatGPT 的公网入口 | 明确为对 Connect 的新增需求：每台 Desktop 一个稳定的公网 MCP 入口与可吊销凭据，需与 Connect 负责人共同评审 | §5.7.4 |
| C9 | 未考虑 Magi 现有 popup 单一决策链对第三方登录弹窗的影响 | 阶段 0 实测各登录方式；不为登录新增宿主形态 | §5.4 |
| C10 | 会话的 `browser_*` 工具可能读到已登录的 ChatGPT 页面 | 应用级会话不暴露给任何会话的浏览器工具 | §5.3 |
| C11 | 关闭 Tab 不销毁会话是对现有"关闭即全局关闭"规则的例外，但未同步权威文档 | 阶段 1 同步修改 `docs/browser-runtime-design.md` | 阶段 1 |
| C12 | 应用级会话处于 `Suspended` 时收到推理调用的行为未定义 | 自动恢复并重新物化推理页面；GPT Web Tab 在后台回到 Tab 条，不抢焦点 | §5.1 |
| C13 | 代码引用使用行号，易失效 | 改为符号名 | 附录 B |
| C14 | 原方案要求"每次发送一条新临时对话 + 全量上下文"，带来每轮固定导航开销、重复消耗，并使 T3 的挂起超时致命化 | 改为**每个 Magi 会话一条临时对话、多轮续接**；仅在失效或重建时全量重放 | §0.2、A5、§5.3、§5.6 |
| C15 | A5 原以"外部零状态 ⇒ 任意项目/会话共享同一浏览器天然安全"为理由，该理由随 C14 失效 | 拆分为"登录态与宿主应用级"+"对话实例按 Magi 会话隔离" | §5.3、§5.6 |
| C16 | T3 的挂起式调用在超时后要求重开临时对话，与"工具调用与结果留在同一回复内"的定义冲突 | 改为**工具结果作为同一对话的下一轮消息回填**；超时只影响该轮时序，不再降级为全量重发 | §5.7.3、§5.10、§9 |
| C17 | "压缩在 web 侧、Magi 不需要考虑压缩"的前提不完整：对话重建时没有任何一方能替 Magi 产出可发送的上下文 | 明确分工：对话内已发送的内容无法改写，续轮本身不做压缩；**压缩由 Magi 按对话累计占用触发，压缩后必须重建，由 Magi 产出自包含的单条上下文**；重锚只作为意外截断时的保险，不作为常规手段（A12） | §5.9 |
| C18 | 原方案把"临时对话能否跨页面重载 / 重启恢复"列为阶段 0 的 go/no-go，隐含"web 侧状态需要被保住" | 明确**不依赖**其可恢复性（A20）：web 侧不落盘，临时会话按定义短暂；可恢复只是省下一条消息额度，不可恢复按 canonical 全量重放重建。阶段 0 仍实测真实行为，但只用于给默认空闲阈值与预期消耗定价，不再作为 go/no-go | 阶段 0、§5.6 |
| C19 | 工具协议、skill 指令与任务约束只在首条消息存在，web 侧一旦截断，模型会静默失去协议且 Magi 无法察觉 | 引入**重锚**：关键约束按轮次重新附加在每条发送内容中 | 设计基线 §5.6、§5.9 |
| C20 | 额度按档位分类计数，与多轮模式下的实际发送次数不符 | 改为按**实际发送次数**计数，并区分同回复内的工具往返 | §5.10 |
| C21 | 对话身份只按 Magi 会话绑定，未纳入引擎、effort 与上下文重建代数 | 对话绑定键 = `Magi 会话 × 引擎 × effort × 上下文 epoch`（对齐参考项目的 `task/model/effort/compaction epoch`） | §2、§5.3、§5.6 |
| C22 | 认为"压缩发生在 web 侧、Magi 不需要考虑压缩" | **压缩仍由 Magi 决策**：阈值来自 Magi 统一的 `ContextBudgetPolicy`（窗口用 Web 实测值，占用按对话累计账计量）；摘要由 Magi 现有压缩流程产出（A13）；压缩后推进 epoch、新建对话实例；超限映射为 `ContextLengthExceeded`，自动压缩后重建，绝不静默截断 | 设计基线 §5.6、§5.9 |
| C23 | 多轮续接未定义"新建会话发什么、续轮发什么"，也未提供隔离工具能力丢失的手段 | 双路径：**新建/重建发全量上下文；同一对话续轮只发上次助手回复之后的增量 + 重锚块**；另提供默认关闭的"每轮新建对话"开关，用于隔离保留对话上的工具能力丢失 | §5.6、§5.11 |
| C24 | 原 §5.9 只要求按 Web 侧上限标定，没有规定标定条件；网页端上限随账号等级、模型、档位变化，也不同于 API 的模型窗口 | 按账号等级 × 模型族 × 档位在临时对话中实测（T3 附带连接器）；写入引擎 `contextWindowTokens` 且只读；账号等级变化时要求刷新 | §5.9.2 |
| C25 | Web 引擎没有 provider usage，只能依赖 Magi 的通用估算器；它跳过空白字符、非中文字符按每 4 个算 1 token，会明显低估代码与 JSON，导致 Magi 以为未满而网页端已超出 | Web 引擎绑定 o200k 词表计数器，预检、累计账与发送前校验使用同一口径 | §5.9.3 |
| C26 | 多轮续接后，对话里保存的是当初发出的完整内容，而 Magi 的模型视图会随时间缩减旧工具结果；按 Magi 当前视图估算会低于对话实际占用 | 每个对话实例维护"已发送 + 已收到"的累计账，作为该会话上下文压力的锚点；epoch 变化时失效 | §5.9.4 |
| C27 | 原 §5.9 的"明确失败 + 引导压缩"与 Magi 上下文架构的超限恢复不一致 | 发送前硬校验不通过、页面提示过长、输入框把文本转为附件，统一映射为 `ContextLengthExceeded`，由 Magi 现有恢复状态机压缩后重建（A13） | 设计基线 §5.9.6 |
| C28 | 对话实例只能追加、不能改写；Magi 一侧压缩、回退或修改已发送内容后，对话与 Magi 不再一致，原先的"漂移"只靠事后语义判断 | 用前缀指纹确定性判定：已发送部分在 Magi 一侧发生变化即推进 epoch 重建；T3 续接同一回复前做同样核对 | §5.9.7、§5.7.3 |
| C29 | T2 请求侧仍按 API 语义回放整段对话记录与 `role=tool`，与 C23 的"续轮只发增量"冲突，也假设网页端能接收角色化消息 | 续轮统一发送**增量 + 重锚块**；工具结果用网页端可读的**续轮封装**（`magi-tool-result` 块，含 `turn_id`、`tool_call_id`）表达，T2 与 T3 的回填路径共用；去重以 `turn_id + tool_call_id` 执行账本为准 | §5.6、§5.7.2、§5.7.3 |
| C30 | 对话绑定键虽已定义，但没有说明推理通道如何获得 `Magi 会话 / 引擎 / effort / epoch`，也没有指定绑定记录落在哪个存储 | 身份在构造 client 时绑定（按 `session_id` 解析出的引擎与 effort 闭包进 `BrowserWebModelBridgeClient`，epoch 由 client 自持）；绑定记录持久化在 daemon 应用级绑定存储（存储位置由 C39 修正，不进会话 sidecar）；`ModelInvocationRequest` 不变，§6 补条目 | §5.1、§5.6、§6 |
| C31 | 用户接管、页面内改模型或 effort、换账号之后，对话绑定的后续状态未定义 | 引入 `MagiOwned / UserOwned / Invalidated` 三态与转换规则；只有 `MagiOwned` 可续轮；提供"重置为 Magi 对话"操作 | §5.6、§5.8 |
| C32 | 排队只有并发上限，没有队列深度与等待上限，慢等待会无限堆积 | 队列上限默认 16、可配置；等待上限取该 turn 剩余时限；队列满或超时以 `web_queue_full` / `web_queue_timeout` 明确失败 | §5.6 |
| C33 | Web 侧上限记录只有窗口、字符数与推理预留，缺少标定口径版本与时间；o200k 与网页端不一致时也没有回退 | 记录补齐 `tokenizer_revision` / `limit_behaviour` / `measured_at` / `measured_by`；计数器误差按标定记录给安全系数，不按估算"看似未满"就发送 | §5.9.2、§5.9.3 |
| C34 | §5.2 的"查看当前会话的对话实例"没有说明是否复用推理页面 | 查看即激活（re-parent）该会话已有的那一条推理页面，不新建页面、不新增第二份 Surface；查看不改变所有权 | §5.2 |
| C35 | 原方案把 T3 的挂起式同回复续接当唯一正常路径、把超时回填当异常降级 | 挂起时长受用户审批支配，回填会是常态路径；两条路径都是一等公民，且回填路径与 T2 的发送形态同构，可先行实现（A18） | §5.7.3 |
| C36 | 原方案要求用户自行在 ChatGPT 侧添加自定义连接器，并默认套餐一定支持 | 由 Magi 在托管浏览器会话中自动配置连接器 + 一次授权说明；账号 / 套餐不支持或配置失败时自动降级为 T2 并在 UI 说明原因（A17） | §5.7.3、§5.11 |
| C37 | 档位差异只按"有没有工具"描述，没有按消耗衡量 | 明确档位价值指标：完成一个任务消耗的 ChatGPT 消息条数（T3 ≈ 1，T2 ≈ 工具轮次数），并作为默认档位选择的产品依据（A16） | §5.7.0、§5.10 |
| C38 | 兼容性只写"按仓库兼容策略走"，隐含为旧数据保留迁移与运行期分支 | 明确不做旧浏览器状态迁移、不支持应用降级；本功能不往用户资产的结构里新增字段，旧格式直接丢弃重建；迁移与兼容不得成为性能或形态的约束条件（A19） | §5.1、§5.3、设计基线 §6 |
| C39 | C30 把 Web 对话绑定写进会话 sidecar，但 `SessionRuntimeSidecar` 与 `SessionDurableState` 都带 `deny_unknown_fields`：写入后新旧版本互读会直接失败；且每次绑定或累计账变化都要重写整份会话状态，属明确的写放大 | 绑定记录（绑定键、页面身份、累计账、前缀指纹、最近一次已接受发送、所有权状态、账号提示）改落在 daemon 应用级绑定存储（独立文件，走现有 `state-layout` 管道），按绑定键索引，随会话删除清理；`SessionRuntimeSidecar` 与 `SessionDurableState` 完全不改（A19、设计基线 §6） | §5.1、§5.6、§6 |
| C40 | BrowserAuthority 的 durable state 经 `load_browser_authority` 用 `?` 冒泡：文件损坏、字段不认识或 schema 版本不认识都会让 daemon 加载失败；而这份状态只是可重建的浏览器指针 | 该类状态按"可重建状态"处理：加载路径改为全函数，解析失败或版本不认识一律视为"无状态"重建并覆盖写回；同时**不写** `session_id → owner` 的迁移，只推进 schema 版本（A19、设计基线 §6） | §5.3、设计基线 §6 |
| C41 | "对话实例内容不持久化（C14）"与"对话内容落 canonical"并列时，容易被读成"从 web 读回的内容不在 Magi 落盘" | 明确持久化边界：**Magi 展示的一切都来自 canonical 且已落盘**（助手正文、thinking 项、工具调用与结果、状态与错误）；不落盘的只有 web 侧副本（页面 URL、DOM、composer 草稿、ChatGPT 侧历史）。这也正是重建与压缩能全量重放的前提（C17） | §5.1、§5.9.1 |
| C42 | C18 的 go/no-go 口径与 A20 冲突：把 web 侧恢复当成实现前提，等于承认外部状态需要被保住 | 降级为普通实测项（只影响默认空闲阈值与额度预期）；实现上**永不依赖** web 侧恢复，任何不可恢复情形一律推进 epoch、按 canonical 全量重放重建（A20） | §5.6、§5.8、阶段 0 |

### R1–R21 联合审核修订记录（2026-09-28）

四路联合审核：架构与状态所有权、产品交互、工具桥与安全、实现可行性。审核结论按「与代码事实不符」优先处理，逐条核对源码后落笔；下表是采纳的修订。

| 编号 | 审核发现 | 修订 | 位置 |
| --- | --- | --- | --- |
| R1 | 文档承诺「登录态像浏览器缓存一样跨重启保留」，但现有分区 `magi-browser-<id>` 没有 `persist:` 前缀，在 Electron 中是**内存会话**，进程退出即丢；宿主 partition 白名单也拒绝 `persist:` | 新增固定决策 A21：应用级会话使用 `persist:magi-web-model`；宿主 partition 白名单、`configurePartition` 与 `clearBrowsingData` 同步；阶段 0 用一次真实重启验收 | 设计基线 §0.2 A21、§5.1、§5.4、§5.12；计划 §3 第 5 项、§9.2 第 20 条 |
| R2 | 文档断言主对话按 `session_id × engine_id × effort` 解析，但 `resolve_orchestrator_model_config` 只合并 orchestrator 段与会话级 `model` / `reasoningEffort`，**全程不读 `engines`、不产出 `engine_id`**：Web 模型既没有可选入口，也没有绑定的落点 | 新增固定决策 A22：Web 引擎进 `engines`，**选择入口是会话内主模型选择器**，会话级主模型覆盖新增引擎绑定；写明 orchestrator 侧三处改动点 | 设计基线 §0.2 A22、§5.6、§5.13；计划 §6 |
| R3 | `engines[*]` 现为 `{ id, displayName, llm: {...} }`，`upsert_engine` / `normalize_engine_entry` 强制 `llm` 且要求 `baseUrl / apiKey / model`，没有 baseUrl / apiKey 的 Web 引擎会被直接拒绝 | Web 引擎**不写 `llm`**：来源与协议写顶层；`upsert_engine` / `normalize_engine_entry` 按 `chatgpt_web` 放行；发现接口示例同步 | 设计基线 §5.5、§5.12；计划 §4.1 2.3、§7.1 |
| R4 | 只「白名单加一项」会让 `to_http_model_client()` 把 `chatgpt_web` 落回 HTTP 客户端，形成第二条推理通道；`magi-settings-store` 还会对缺 `apiProtocol` 的引擎静默补 `openai_chat` | `ModelApiProtocol` 新增 `ChatGptWeb` 变体，`to_http_protocol` / `to_http_model_client` 显式拒绝；settings-store 规范化不得对 Web 引擎补 `openai_chat`；`HttpModelBridgeProtocol` 不新增取值 | 设计基线 §6；计划 §4.1 2.3、§6 |
| R5 | 「复用 `RuntimePersistence::save_json`（`crates/magi-daemon/src/daemon/persistence.rs`）」引用了不存在的类型与文件；`state-layout` 也没有文件登记表 | 更正为 `crates/magi-api/src/state.rs` 的 `RuntimeStatePersistence::save_json`；明确不新增 `state-layout` 登记项、不推进其版本 | 设计基线 §5.12；计划 §4.2 3.2、§6 |
| R6 | App 级会话没有对应 Magi 会话，会被 `reconcile_browser_sessions_with_session_store` 当孤儿转 `Closed`；`is_reclaimable_tab` 与 `/browser/resources/reclaim` 只看生命周期与租约，App 级会被回收；`CreateBrowserSessionRequest` 没有 App 分支 | 补 owner 分支：孤儿回收、资源回收、配额、创建入口（App 级创建前先查已有会话，保证 `browserSessionId` 稳定）；App 级会话不暴露给 `browser_*` 工具 | 设计基线 §5.3；计划 §3 第 2–4、9 项 |
| R7 | 「关闭只隐藏」没有接口落点：现有关闭按钮调 `closeBrowserTab` 走全局关闭；文档又写「不同的确认文案」，而隐藏视图本不该有确认 | 明确 Web 模型 Tab 的关闭只做右栏本地隐藏、不调 `closeBrowserTab`、不弹确认；Tab 内提供「仅隐藏（保留后台推理）」与「停止推理」两个动作 | 设计基线 §5.2、§5.13；计划 §3 第 7 项 |
| R8 | 「不进入 `perSession`」在现有实现里等于「既不渲染也不恢复」：Tab 集合、持久化与恢复全部以 `perSession` 为单位；且现有内容槽是一个 Tab 一个 `<webview>`，放不下主页 + 推理页面 | 右栏 store 新增与 `perSession` 并列的顶层 `appTabs` 并加白名单；新增内容组件 `WebModelTabContent.svelte` 承载同一内容槽内的两个受管 guest | 设计基线 §5.2；计划 §3 第 7–8 项、§6 |
| R9 | §5.1 写「能重新挂载就沿用累计账与前缀指纹」，但协议里没有任何回读临时对话既有内容的命令，无法重建续轮增量，与 A20 冲突 | 统一为：daemon / Desktop 重启**一律推进 epoch 并按 canonical 全量重放**；「最近一次已接受发送」的用途收窄为发送去重与崩溃恢复判定 | 设计基线 §5.1、§5.6、§5.12；计划 §9.4 |
| R10 | 状态机（§5.11，7 态）、发现接口 `status`、错误码表三套枚举互不对齐；selector 漂移没有引擎状态，站点改版后模型仍显示可用 | 以 §5.11 为唯一枚举：发现接口补 `consent_required` / `desktop_unavailable` / `refresh_required` / `quota_exhausted` / `tool_degraded` 并给出映射表；新增 A23 要求 selector 漂移推进 `site_blocked`（带 `reason`）；额度用尽独立为 `quota_exhausted` | 设计基线 §0.2 A23、§5.11；计划 §7.1、§8 |
| R11 | 「每个错误都有主行动按钮」但唯一的错误面 `ModelFailureCard` 只有「复制诊断」，错误码表也没有动作列 | 错误码表新增「呈现与主行动」列，逐码给出按钮与动作；失败卡片新增动作区；`ContextLengthExceeded` 不出现在卡片 | 设计基线 §5.13；计划 §8、§4.2 3.12 |
| R12 | 四个「不进入模型列表」的状态只有一个被隐藏的入口，文档没说是设置页哪一屏 | 统一落点：「设置 → 浏览器 → GPT Web 模型」分区，承载全部非 `available` 状态的卡片与主行动 | 设计基线 §5.5、§5.11、§5.13；计划 §4.1 2.4 |
| R13 | 关闭 Tab 后后台仍在跑的推理没有任何界面指示，用户会以为任务被取消 | 关闭视图期间，「新增」菜单项与设置分区显示「GPT Web（后台推理中 · N）」并可点回 | 设计基线 §5.2、§5.13；计划 §9.3 第 10 条 |
| R14 | 既有「设置 → 浏览器 → 清理浏览数据」会清掉所有已知 partition 的 Cookie，等于静默登出并使进行中的 turn 失败；文档新增的「清除 Web 数据」与它几乎同名却互不关联 | 合并为同一入口、不新增第二个按钮；文案追加「包含 ChatGPT 登录态…」；执行时先取消进行中的 Web 推理，再推进 `login_required` 并失效绑定 | 设计基线 §5.13、§5.3；计划 §4.2 3.11、§9.2 第 22 条 |
| R15 | 「登录页只展示可用登录方式」不可实现（那是 ChatGPT 自己的 DOM）；popup 被阻止时用户几乎看不到原因 | 改为 Magi 在 GPT Web 主页叠加登录提示条与 popup 被阻止的持久提示条，不向第三方页面注入内容 | 设计基线 §5.4、§5.13 |
| R16 | §5.13 要求非 Primary 窗口「请在主窗口使用」，与 §5.3「不加例外」及既有 Surface 规则冲突 | 新增 A24：每个窗口各一份独立物理 guest、登录态共享，任一时刻只有一个 Primary 参与推理，非 Primary 窗口给出可接管提示 | 设计基线 §0.2 A24、§5.3、§5.13；计划 §9.3 第 9 条 |
| R17 | 「等待 Web 引擎 / 浏览器生成中 / 排队中」没有 UI 载体（现有运行指示只看 canonical 阶段） | 明确走现有运行时投影新增可选字段，承载在会话 turn 的运行指示行，并给出 i18n key | 设计基线 §5.13；计划 §4.2 3.12、§6 |
| R18 | 阶段 0 的 0.5 / 0.9 / 0.10 依赖阶段 3 才有的计数器与阶段 4 才有的通道，构成循环依赖；0.4 缺样本量与判定阈值 | 0.5 拆为 0.5a（借外部 o200k 参考实现）+ 0.5b（阶段 3 计数器落地后复核）；0.9 / 0.10 移为阶段 4 前置 spike；0.4 补账号等级与模型族清单、重复次数 | 计划 §2、§1 |
| R19 | 阶段 1 清单小于 §1 的完成标志，缺 owner 分支、创建入口、持久分区、`appTabs`、新内容组件、i18n、`browser_*` 工具可见性 | 阶段 1 清单扩到 11 项并各配验证命令；§1 阶段 1 描述同步 | 计划 §1、§3 |
| R20 | 验收全为人工：没有真实 Electron 自动化、没有站点 DOM 回归防线、没有回答同步断言；仓库既有的三套 harness 一个都没引用 | 新增 §9.4 自动化验收：`verify-web-model-dom.mjs`、站点 DOM fixture 单测、`verify-browser-contracts.mjs`、重启重放断言；§9.1 补全强制前置命令，并把 `test:browser-core` 正名为静态契约断言 | 计划 §9.1、§9.4 |
| R21 | 工作量与周期预估偏低（未计入四维标定、owner 分支改造面、计数器接入面、`magi-usage-authority` 锚点、连接器自动配置的 DOM 脆弱性）；附录 A 有三处不精确引用 | §5 预估改为：主链路（阶段 0–3 + T1/T2）49–80 人日、不含 T3 含阶段 5 约 54–89、含 T3（不含 Connect 侧）79–129；附录 A 修正 `typeText`（不是原子写入命令）、`contextWindowTokens`（settings wire 字段）、`RuntimePersistence`（应为 `RuntimeStatePersistence`），其余路径经逐条核对全部存在 | 计划 §5、附录 A |

### R22–R35 第二轮联合审核复核记录（2026-09-28）

第二轮审核（架构与状态所有权 / 工具桥与安全 / 产品交互 / 文档一致性）以**源码逐符号核对**为主，以下是采纳的修订。

| 编号 | 复核发现 | 修订 | 位置 |
| --- | --- | --- | --- |
| R22 | 设计文档 §3 架构图写「partition `magi-browser-<browserSessionId>`（登录态持久）」，与 A21/§5.4 直接矛盾（该分区没有 `persist:` 前缀，是内存会话） | 架构图改为 `persist:magi-web-model` | 设计基线 §3 |
| R23 | §5.8 仍写「能重新挂载则连同累计账与前缀指纹继续使用」，与 §5.1/§5.6/§5.12 的「一律推进 epoch 全量重放」冲突 | 该行改为「一律推进 epoch、按 canonical 全量重放」 | 设计基线 §5.8 |
| R24 | 「没有任何回读临时对话内容的命令」的理由不准确：Desktop Control 有 `snapshot` 命令，能读页面可见文本；缺的是**带 canonical 消息标识的语义回读** | 三处措辞改为「没有带 canonical 消息标识的语义回读命令」，结论不变 | 设计基线 §5.1、§5.6、§5.12 |
| R25 | A24 与 §5.3 写「全局只有一个 Primary」，但 `primary_surfaces` 以 `BrowserTabId` 为键：正确语义是「同一逻辑 Tab 全局只有一个 Primary」 | A24/§5.3/§5.13 统一为 per-tab Primary | 设计基线 §0.2 A24、§5.3、§5.13 |
| R26 | 产品当前只创建一个窗口入口（`index.ts`），A24 的多窗口交互与验收项当前不可执行 | §5.13 与 §9.2#22 标注「架构级验收，需先补窗口入口」；A24 边界列说明现状 | 设计基线 §0.2 A24、§5.13；计划 §9.2 |
| R27 | 设计声称 `upsert_engine` 强制 `llm` 与 `baseUrl/apiKey/model`、settings-store 会给 Web 引擎补 `openai_chat`：与源码不符（前者只解析不强制；后者只作用于 `llm` 子对象） | 改为真实约束：`normalize_engine_entry` 只保留 `id/displayName/llm/runtime` 且缺 `llm` 写空对象；`NormalizedModelConfig` 对未知 / 缺失 `apiProtocol` 硬报错；Web 引擎不写 `llm` 就不进入规范化分支 | 设计基线 §5.5、§6；计划 §6 |
| R28 | **阻塞项**：`canonicalize_session_orchestrator_section` 只保留 `model` / `reasoningEffort`，会话级 `engineId` 会被静默剥离（写入成功但重启即丢） | 计划 §6 增条目：该函数必须保留 `engineId`，且 `engineId` 不进入新会话默认值 | 计划 §6、§9.2#24 |
| R29 | `build_orchestrator_client` 现直接 `to_http_model_client()`，且返回 `None` 时 `resolve_target_for_role` 会回退到 `default_client`（HTTP 模型）——「不得静默降级」在这条路径上没有落实 | §5.6 与计划 §6 明确：新增 `chatgpt_web` 分支且禁止任何回退；`baseUrl/apiKey` 在该分支被忽略 | 设计基线 §5.6；计划 §6 |
| R30 | `to_http_protocol` / `api_protocol()` 返回非 `Option` 的具体枚举，调用方做穷尽 match：只「不新增枚举取值」无法在编译期拦住 `chatgpt_web` 被当作 HTTP 处理 | 计划 §6 增条目：签名改为 `Option` / `Result` 并同步调用方 | 计划 §6 |
| R31 | 角色 `engineId` 为空时会显式继承 orchestrator，因此子代理会继承 Web 引擎——§5.6 原写「与 Web 引擎无关」不成立 | §5.6 改为明确**支持继承**并说明并发/队列/前置检查；计划 §6、§9.2#25 同步 | 设计基线 §5.6；计划 §6 |
| R32 | 分区注册表 `readPartitionRegistry` 用 `magi-browser-<id>` 正则过滤，`persist:magi-web-model` 会被静默丢弃 → 无 guest 挂载时「清理浏览数据」漏清应用级分区 | §5.4 与计划 §3#5、§6、§9.2#22 补该过滤规则 | 设计基线 §5.4；计划 §3、§6、§9.2 |
| R33 | 「站点适配层」归属表述冲突（架构图画在 Worker，计划 3.4 又把文本协议放在 `magi-web-model`） | §5.5 明确两半：DOM 事实在 Worker，文本协议在 `magi-web-model` | 设计基线 §5.5 |
| R34 | T2 的 `tool_call_id` 未定义（`ChatToolCall.id` 必填、去重账本以 `turn_id + tool_call_id` 为键）；工具结果正文原样透传可注入伪造工具块；harness 未写监听地址与令牌规格 | §5.7.2 增 `tool_call_id = web-<turn_id>-<block_index>`、解析防护与未闭合时限；§5.7.3 增 loopback 监听、独立 router、令牌规格与 90 秒默认时限；续轮封装改为带 `bytes` 长度包裹，契约移入计划 §7.3 | 设计基线 §5.6、§5.7.2、§5.7.3；计划 §7.3、§8 |
| R35 | 参考项目把「单条提交预算 ≪ 模型窗口」与分片发送当作一等传输；设计只留 `composer_char_limit` 并把分片整体推到阶段 5，这类账号的重建路径无解；A17 也缺少「前置条件不可由 Magi 保证」的限定 | §5.9.2 增 `single_submission_token_budget`；§5.9.6 把最小分片纳入重建路径；§5.7.3 明确 A17 只自动化页面操作、其余前置不满足即降档；§5.7.4 显式写明 T3 排期后果 | 设计基线 §5.7.3、§5.7.4、§5.9.2、§5.9.6 |

第二轮同时确认（无需修改）：参考项目确以 stdio MCP + `openai/tunnel-client` + turn 令牌 + 同回复续接实现工具，隧道侧约 2 分钟、本地 90 秒收口；`provider_context` 可承载续接凭据且跨轮回放（`types.rs`、`conversation_loop.rs`）；`SessionRuntimeSidecar` / `SessionDurableState` 为 `deny_unknown_fields`；`load_browser_authority` 用 `?` 冒泡、`BROWSER_DURABLE_STATE_SCHEMA_VERSION = 6`；`is_reclaimable_tab` / `live_tab_count_for_session` 无 owner 维度；`MAX_BROWSER_TABS_PER_SESSION = 32`、`MAX_BROWSER_TABS_TOTAL = 64`；右栏 `handleTabClose` 对 browser 调 `closeBrowserTab`、`addablePaneKinds` 不可用即不渲染；`ContextMeasurement` 现有 Provider / Estimated / Compacted 三个取值；`estimate_chat_messages_tokens` / `estimate_tool_definition_tokens` 为自由函数；`docs/browser-runtime-design.md` §4.1 的 per-tab Primary 与「Agent Tab 只关闭观察视图」先例。

审核同时确认（无需修改）：`ModelInvocationRequest` / `ModelProviderContext` 的形状可承载本方案且不需要扩字段；`provider_context` 确实会写入 canonical 并跨轮回放；`SessionRuntimeSidecar` / `SessionDurableState` 确为 `deny_unknown_fields`，绑定存储不能写入；`estimate_text_tokens` 确实会低估代码与 JSON；`TunnelManager` 与公网鉴权白名单、`require_desktop_browser_capability`、`upsert_engine` / `/settings/models/fetch`、`is_reclaimable_tab`、`MAX_BROWSER_TABS_*`、`typeText`、`load_browser_authority` 等被引用的能力与符号均真实存在。

### R36–R46 第三轮联合审核修订记录（2026-09-28）

第三轮四路审核（架构与状态所有权 / 工具桥与安全 / 产品与前端交互 / 文档一致性与逐符号核对）以**当前源码逐符号复核**为主。前两轮的 R1–R35 已在正文落地，本轮只列新发现并在正文修正。

| 编号 | 复核发现 | 修订 | 位置 |
| --- | --- | --- | --- |
| R36 | 设计 §5.1 生命周期矩阵写「关闭窗口 → 释放该窗口 Surface；关闭最后一个窗口时进行中的 Web 推理按 turn 中断收口」，与真实产品行为不符：关闭主窗口只是隐藏到托盘（`apps/desktop/src/main/window-manager.ts` 的 `window.on("close")` + `shouldHideWindowOnClose`），只有 shutdown 才 `closeAll()` | 拆成两行：「关闭主窗口（默认行为）」= 隐藏到托盘、不释放 Surface、推理继续；「退出 Magi / 操作系统退出」= 释放全部 Surface、按 turn 中断收口 | 设计基线 §5.1 |
| R37 | §5.2/§5.13 的关闭语义自相矛盾：「隐藏是持久状态」「重连与 daemon 重启都不复活」「重启后恢复为显示」三者不能同时成立；且 `appTabs` 与 `tabsForPersist` / `isRestorableTab` 的关系写反了（两个白名单只作用于 `perSession`） | 统一为：隐藏只在当前窗口进程内有效；`appTabs` 由 daemon 投影重建、不做窗口级持久化；`webSession` 不进 `perSession`，也不加进两个白名单 | 设计基线 §5.2；计划 §3#7 |
| R38 | warm pool 没有宿主落点：§5.2 规定内容槽只有「主页 + 当前会话推理页」两条宿主且「其余会话的推理页面不注册 Surface」，而 §5.3/§5.6/§8 又要求预热页面计入 `MAX_BROWSER_TABS_TOTAL`——两者不能同时成立，A3 也禁止新增宿主形态 | **删除预热池**：推理页面按需创建、同一会话复用；§5.3/§5.6/§8 与计划阶段 0 0.8、§1 阶段 3、§4.2 3.8 同步改写 | 设计基线 §5.3、§5.6、§8；计划 §1、§2、§3、§4.2 |
| R39 | 「durable state 损坏 / 版本不认识会让 daemon 加载失败」不准确：`load_browser_authority` 的错误在加载处被捕获，结果是浏览器能力进入 `BrowserHostStatus::Failed` 且**禁止覆盖原文件**（浏览器功能不可用），daemon 仍启动；另外 `restore_durable` 仍接受 `PREVIOUS_/LEGACY_` 版本并走 `migrate_legacy_tab_order`，与 A19「不做迁移」冲突 | 病因句改为「浏览器持久状态不可用且不会被覆盖写回」；加载路径改为全函数并**显式删除**旧版本分支与 `migrate_legacy_tab_order` | 设计基线 §5.3、§6；计划 §3#2、§6 |
| R40 | Web 引擎的 `contextWindowTokens` 写成引擎**顶层**字段，但既有读取路径 `NormalizedModelConfig::from_settings_value` 只读 `engines[*].llm`，Web 引擎不写 `llm` → `ContextAuthority` 拿不到窗口 | 新增说明：顶层 `contextWindowTokens` 由新增的 Web 引擎解析路径读取后交给 `ContextAuthority`，不经过 `NormalizedModelConfig` | 设计基线 §5.9.2；计划 §6 |
| R41 | 绑定存储「复用 `RuntimeStatePersistence::save_json`」未说明可见性：该函数是 `pub(crate)`，跨 crate 直接调用编译不过 | 明确 owner 落点：落在 `magi-api` 内可直接复用，否则先提供等价 `pub` 包装；不允许另起写路径 | 设计基线 §5.12；计划 §3.2 |
| R42 | A21 的文件清单漏了 **Renderer 侧第三处 partition 派生点**：`web/src/components/tabs/BrowserTabContent.svelte` 自行拼 `magi-browser-<id>`，新组件若沿用该规则会在 guest 绑定校验处被拒 | 计划 §3#5、§6 补 `WebModelTabContent.svelte` 必须使用固定分区 | 计划 §3、§6 |
| R43 | 会话内选择器的数据源与强度档位没有落到实现：现 `InputArea.svelte` 的 `pickerModels` 来自 provider `/settings/models/fetch`（与 `engines` 无关），强度是固定四档 | §5.13 明确「provider 模型列表 ∪ daemon 投影的 Web 引擎条目」；计划 §6 新增「选择器数据源与强度档位」条目 | 设计基线 §5.13；计划 §6 |
| R44 | 绑定记录与 `provider_context.pending_tools` 都带 `pageId`，但没说清谁是权威，存在把页面标识当成第二事实源的风险 | 明确二者都只是 BrowserAuthority 逻辑 Tab / 页面的只读引用；Surface / Primary / `surfaceRevision` 一律以 BrowserAuthority 为准 | 设计基线 §5.12 |
| R45 | 「站点适配新命令」只有「要改 schema」一句，没有字段定义，工程师无法开工；A17 的配置触发时机（「启用 T3 时」）也不明确 | 新增计划 §7.7（两条命令的请求/响应/失败契约）；§5.11 把触发时机写定为「引擎可用性投影首次判定最高档为 T3 时」 | 计划 §7.7；设计基线 §5.11 |
| R46 | §0.2 声明「正文只引用编号，不重复展开理由」，与 §5.7.0/§5.9.5/§6 复述 A16/A13/A19 的做法相反；§0.3「由保留的临时对话自己产出压缩 checkpoint」容易被读成与 A13 冲突 | 措辞放宽为「确需解释时只补充与本决策直接相关的取舍」；该否决项明确为「复用被保留的会话对话自身产出 checkpoint」，与 A13（一次性临时对话产出摘要 + 压缩后重建）区分开 | 设计基线 §0.2 前言、§0.3 |

第三轮同时确认（无需修改，均为逐符号核对后成立）：`ModelInvocationRequest` / `ModelProviderContext` / `ChatToolCall` 形状可承载 T2/T3 且不需扩字段；`provider_context` 写入 canonical 并跨轮回放；`SessionRuntimeSidecar` / `SessionDurableState` 为 `deny_unknown_fields`；`BrowserSession.session_id`、`session_for_magi_session`、`is_reclaimable_tab`、`live_tab_count_for_session`、`MAX_BROWSER_TABS_*`、`reconcile_browser_sessions_with_session_store`、`close_browser_session_for_magi_session`、`CreateBrowserSessionRequest` 与文档描述一致；`browserPartitionId` 无 `persist:` 前缀、`BROWSER_PARTITION_PATTERN` 与 `readPartitionRegistry` 均拒绝 `persist:`；`canonicalize_session_orchestrator_section` 会剥离未知字段、`build_orchestrator_client` 的 `None` 回退到 `default_client`、`to_http_protocol` 返回非 `Option` 枚举 —— 这三条阻塞项已分别由 R28/R29/R30 记录并写入 §6；`ContextMeasurement` 仍只有 Provider / Estimated / Compacted；`typeText` 走 CDP `Input.insertText` 且无回读校验；`ModelFailureCard` 只有一个按钮；`TunnelManager` 与公网鉴权白名单存在，而 Connect 的设备身份与稳定 MCP 入口在仓库中尚不存在（T3 排期风险与 A10 的取舍已在 §5.7.4 写明）。

### R47–R48 第四轮（用户裁决）修订记录（2026-09-28）

第四轮不是审核发现，而是用户对两项悬置问题的明确裁决；下表记录改判内容与落地位置。

| 编号 | 用户裁决 | 修订 | 位置 |
| --- | --- | --- | --- |
| R47 | **「Connect 未就绪时以 OpenAI Tunnel 正式交付」，按参考项目进行**（原 A10 只允许 Connect 就绪前做开发态 spike） | A10 改判为「Connect 优先；未就绪时以 OpenAI Tunnel（`openai/tunnel-client`）正式交付」；§5.7.4 由「已评估未采用的备选」改为两条正式通道对比 + OpenAI Tunnel 事实与代价；`magi-web-harness` 增加 **stdio** 传输（`--stdio`，由 `tunnel-client` 拉起）以便端口不暴露；§5.11 / §5.12 / §5.13 / §7 风险表、计划阶段 4 工单（4.3 / 4.6 / 4.7 / 4.7b / 新增 4.9）、§7.2B 契约、§8 `web_tunnel_unavailable`、§9.2 #12 / #15 / #16 / 新增 #34、附录 A 同步；Connect 排期不再是 T3 的阻塞关键路径。代价（第三方二进制、OpenAI 平台凭据、独立吊销路径）在 §5.7.4 显式记录。**Quick Tunnel 仍只用于开发与对照，不向用户交付。** | 设计基线 §0.2 A10、§0.3、§1.3、§2、§3、§5.7.3、§5.7.4、§5.11、§5.12、§5.13、§7、附录 A；计划 §1、§2、§4.3、§5、§6、§7.2、§8、§9 |
| R48 | **T1 允许删除**（原 A8 要求按 T0–T3 四档交付） | A8 改为「T0 / T2 / T3 三档交付」，A15 的降级目标改为 T2；§5.7.0 删除 T1 行与「再否则 T1」；§5.7.1 由 T1 设计改写为「已删除」说明（保留小节号，避免破坏引用），并把「T0 不返回 `tool_calls`」与「skill 指令仍由 Magi 上下文编译注入」两条事实并入；§5.7.5 改为按档位的两条 skill 工具调用路径；§0.3 新增「恢复 T1」否决项；计划阶段 4 改名 `T2 → T3`、删除 4.1 工单、§5 预估拆出「阶段 4（T2）」并下调、§6 i18n 去掉 `toolTier.t1`；§9.2 #11 去掉 T1 验收；附录 B 通路示意图去掉 T1 边。**T1 不能调用工具，只增加发送内容与额度消耗，收益为零。** | 设计基线 §0.2 A8/A15、§0.3、§1.1 S6、§1.3、§2、§3、§5.7.0、§5.7.1、§5.7.5、§5.11、§5.13、附录 B；计划 §1、§4.3、§5、§6、§9.2、§9.4 |

**R47/R48 对其他记录的影响**：R21 中的 §5 预估（「阶段 0–3 + T1/T2 49–80 人日」等）是第三轮的历史结论，已被本轮 §5 的「阶段 0–3 + T2 / 含 T3」口径取代；第三轮确认段中「Connect 的设备身份与稳定 MCP 入口在仓库中尚不存在，构成 T3 排期风险」仍是事实陈述，但风险处置已由 R47 改为「Connect 未就绪时用 OpenAI Tunnel 正式交付」，不再阻塞 T3 交付。
