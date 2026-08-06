# Magi 内置浏览器完整设计

> 状态：目标架构基线
>
> 更新日期：2026-08-04
>
> 适用范围：Magi daemon、Desktop、Web 工作台、Goal、子代理、工具运行时与发布链路

## 1. 决策摘要

Magi 内置浏览器采用以下唯一实现：

1. `magi-daemon` 内的 `BrowserAuthority` 是浏览器状态、控制租约和能力判定的唯一权威来源。
2. 浏览器执行引擎使用受 daemon 管理的 Playwright Browser Host sidecar，运行独立的 Chromium 和独立的 Magi 浏览器 Profile。
3. 右侧栏展示 Chromium 的 CDP Screencast 画面并转发用户输入，不把业务网页加载进 Magi 主 Tauri WebView。
4. 模型通过 Magi 第一方内置工具直接调用 `BrowserAuthority`，不依赖用户配置的 MCP。
5. MCP 只保留为未来接入远程浏览器或第三方浏览器服务的适配边界，不参与内置浏览器的核心状态链路。
6. Browser Session 与 Goal 状态正交；Goal 暂停时撤销代理控制租约，但浏览器页面继续保留并允许用户查看。
7. 同一 Magi Browser Profile 同一时刻最多有一个代理写控制租约；该代理可以在自己会话的多个 Tab 间切换。用户接管优先于代理，并通过统一执行中断链路暂停关联 Goal。
8. 页面标记是后端持久化的一等领域对象，不是前端截图上的临时坐标。
9. Chromium、Node Runtime 和 Browser Host 组成独立签名的 Browser Runtime Component，首次使用时按需安装；Magi 常规升级不重复携带该组件。
10. 内置浏览器完成后删除右侧栏现有 HTML iframe 渲染分支；HTML 源码继续由 Code Tab 展示，HTML 运行预览统一进入 Browser Session。

该方案参考 Codex 的产品和权限分层、OpenAI CUA Sample App 的运行与回放结构、Steel Browser 的 Session/Viewer 设计，以及 Playwright MCP 的工具语义。Magi 不复制其中任意一个项目，而是把它们收敛进现有 Goal、工具、事件和会话架构。

## 2. 为什么必须这样设计

### 2.1 不能把 MCP 当作内置浏览器内核

用户 MCP 连接具有以下不确定性：

- 可以被关闭、删除或配置错误。
- 工具名称和 Schema 可能被不同模型错误调用。
- MCP Server 自己维护页面和 Profile，Magi 无法保证 Goal 暂停、会话切换和 daemon 重启后的状态一致性。
- 前端无法可靠判断 MCP 中的页面是否仍存在、由谁控制、是否已经崩溃。
- 页面权限、敏感操作审批和 Magi 的访问模式会形成两套治理链路。

因此内置浏览器必须是 Magi Runtime Capability。外部 MCP 只能是 `BrowserBackend` 的外部实现来源，不能成为默认权威状态源。

### 2.2 不能复用 Magi 主 WebView

在主 Tauri WebView 内通过 iframe 或子 WebView 打开任意网页，会带来：

- 页面与 Magi 自身前端处在同一宿主安全边界。
- Cookie、下载、弹窗、导航、跨域和 CSP 行为难以统一。
- Desktop、daemon-only Web 和远程访问的行为不一致。
- 模型控制仍需额外接入 CDP，形成“显示一套、控制另一套”。
- Windows WebView2、macOS WKWebView 和 Linux WebKitGTK 的行为差异较大。

独立 Chromium 同时承担渲染和控制，右侧栏只消费画面与权威状态，才能保持跨平台一致。

### 2.3 不能再建立第二套执行状态机

Magi 已经具备：

- `ExecutionOwnership`
- Goal `control_revision`
- Goal 与 Plan 原子暂停/恢复
- canonical turn
- daemon 重启恢复
- 任务树中断
- 工具执行取消

浏览器只新增“执行资源租约”，不重新定义任务是否运行。Goal、Plan、Turn 和 Task 仍决定任务状态，Browser Lease 只决定某个执行所有者是否能对共享 Profile 发起写操作。

### 2.4 与当前代码的直接接点

| 当前能力 | 代码位置 | 浏览器接入方式 |
| --- | --- | --- |
| Goal 状态、continuation、control revision | `crates/magi-session-store/src/models.rs` | Lease 绑定当前 Goal revision，暂停/恢复仍调用现有原子状态转换 |
| 异常中断 Goal 恢复 | `crates/magi-session-store/src/store/sidecar.rs` | daemon 重启后不恢复 Lease，只随新 continuation Turn 获取新 Lease |
| 用户停止与 Goal 暂停 | `crates/magi-api/src/routes/sessions.rs`、`routes/goals.rs` | 统一改由 `ExecutionResourceCoordinator` 撤销 Process 与 Browser 资源 |
| 工具资源注入 | `crates/magi-tool-runtime/src/types.rs` | 按现有 Git/Image/MCP Executor 模式注入 `BrowserToolExecutor` |
| 工具治理与进度 | `crates/magi-tool-runtime/src/registry.rs` | 浏览器工具继续走 ToolRegistry，不建立旁路执行器 |
| EventEnvelope 与 SSE 恢复 | `crates/magi-event-bus/src/events.rs`、`crates/magi-api/src/sse.rs` | 浏览器低频状态进入现有事件信封，高频帧走独立 WS |
| 右侧多 Tab 面板 | `web/src/stores/right-pane.svelte.ts`、`web/src/web/RightPane.svelte` | 新增 `browser` kind，不新建第二个右侧栏容器 |
| 跨平台进程树管理 | `crates/magi-process/src/lib.rs` | Browser Host 与 Chromium 纳入 Managed Child 生命周期 |

这些接点已经存在，说明方案不需要替换 Magi 主运行链，只需要补充浏览器领域能力并收敛当前只面向 Shell 的长期工具取消接口。

## 3. 产品目标

- 用户可以在右侧栏打开本地或公网网页，并与代理共享同一个页面状态。
- 主线代理和子代理可以导航、读取、点击、输入、滚动、截图和验证页面。
- 用户可以随时查看代理操作过程、接管页面并在元素或区域上留下标记。
- Goal 被用户打断、暂停、恢复或异常中断时，浏览器控制权与 Goal 状态保持一致。
- daemon 或 Browser Host 重启后不会显示伪运行状态，也不会让模型无限重试工具。
- 浏览器 Profile 与用户日常浏览器隔离，但在 Magi 内跨会话保留登录状态、Cookie 和历史记录。
- 所有模型使用简单、稳定的 JSON Schema，不依赖某个模型特有的 Computer Use 输出格式。
- Desktop、daemon-only Web 与后续远程客户端读取同一个 Browser Authority。

### 3.1 用户需求覆盖矩阵

| 用户描述的需求 | 方案覆盖 |
| --- | --- |
| 像 Codex Desktop 一样提供内置浏览器 | 独立 Chromium + Browser Host，由 Magi daemon 管理并与聊天共享页面 |
| 不能只能依靠 MCP | 第一方 BrowserToolProvider 直接进入 ToolRegistry；MCP 不参与核心链路 |
| 在右侧栏预览和操作网页 | Right Pane 新增 Browser Tab，双向 WebSocket 承载画面和用户输入 |
| 支持页面元素/区域标记 | 持久 BrowserAnnotation、DOM 锚点、截图证据和 stale/resolved 状态 |
| 与 Goal 暂停、继续和异常恢复一致 | ExecutionResourceCoordinator、Browser Control Lease、revision/fence 与 continuation Turn 联动 |
| 子代理不能出现控制错位 | 真实 ExecutionOwnership 绑定，Profile 级单写者，禁止不同子代理并发修改共享浏览器状态 |
| Magi 安装包不能因 Chromium 大幅膨胀 | Browser Runtime Component 独立按需安装，主安装包不携带 Chromium |
| Magi 常规升级不能重复下载 Chromium | App/Host Protocol 兼容时跨版本复用独立 Runtime Component |
| 可以检测 Chromium 更新并由用户操作安装 | Magi 签名 manifest、手动检查、分级提示、用户确认下载和原子切换 |
| 不能再次出现工具可见性和访问模式错位 | BrowserCapabilitySnapshot 同时驱动模型工具目录和执行校验 |
| 当前 HTML 文件预览如何处理 | 保留源码查看与安全文件服务，删除 iframe 渲染，预览动作统一打开 Browser Session |

除“控制用户已有 Chrome Profile”和“模型自动上传本地文件”外，当前用户提出的内置浏览器、右侧预览、标记、Goal 稳定性、轻量打包和独立更新需求都进入目标架构。前两项明确属于非目标，不影响本次需求闭环。

## 4. 非目标

- 不控制用户现有 Chrome Profile；该能力未来通过 `BrowserUseExternal` 独立提供。
- 不让模型自动读取密码、信用卡、验证码或浏览器保存的凭据。
- 不在第一方工具中开放任意 JavaScript 执行或完整 CDP。
- 不自动处理 CAPTCHA、系统权限弹窗和管理员认证。
- 不让模型自动上传本地文件；文件选择由用户手动完成。
- 不把浏览器画面、Cookie、localStorage 或网页正文上传到额外的 Magi 云服务。
- 不同时保留 Rust CDP 和 Playwright 两套浏览器实现。

## 5. 总体架构

```text
┌──────────────────────────── Magi Web UI ────────────────────────────┐
│ RightPane/BrowserPane                                               │
│ 地址栏 / Tab / 状态 / 标记 Overlay / Screencast Canvas / 用户输入 │
└───────────────┬───────────────────────────────┬─────────────────────┘
                │ REST + SSE                    │ WebSocket 二进制帧
                ▼                               ▼
┌──────────────────────────── magi-daemon ────────────────────────────┐
│ Browser API Routes                                                  │
│                │                                                    │
│                ▼                                                    │
│ BrowserAuthority                                                    │
│  ├─ BrowserSession/Tab 权威状态                                     │
│  ├─ BrowserControlLease + fencing token                             │
│  ├─ 固定导航安全边界 + SensitiveActionPolicy                        │
│  ├─ AnnotationStore                                                 │
│  ├─ HostSupervisor/Recovery                                         │
│  └─ EventBus 发布                                                   │
│                ▲                                                    │
│                │ 进程内调用                                         │
│ BrowserToolProvider ─ ToolRegistry ─ Conversation/Worker Runtime    │
│                │                     │                              │
│                └──── ExecutionResourceCoordinator ─ Goal/Turn/Task  │
└────────────────────────────┬────────────────────────────────────────┘
                             │ 私有 WS 控制协议 + 二进制帧
                             ▼
┌──────────────────── Playwright Browser Host ────────────────────────┐
│ Persistent BrowserContext / Page / CDP / Screencast / Input        │
└────────────────────────────┬────────────────────────────────────────┘
                             ▼
                    独立 Playwright Chromium
```

## 6. 模块边界

### 6.1 新增 `crates/magi-browser-runtime`

职责：

- 浏览器领域模型和状态转换。
- `BrowserAuthority`。
- Tab 控制租约和 fencing token。
- Browser Host 客户端协议。
- 固定导航安全边界与敏感动作策略接口。
- 标记锚点、重新定位与状态。
- Browser Session 的恢复协调。

依赖边界：

- 可以依赖 `magi-core` 和 `magi-event-bus`。
- 不依赖 Web、Tauri、MCP 或具体模型 Provider。
- 不依赖 `magi-tool-runtime`，避免形成工具运行时循环依赖。

### 6.2 新增 `browser-host`

这是 TypeScript/Node.js sidecar，只负责执行，不拥有产品状态：

- 启动 Playwright Chromium。
- 管理唯一持久 BrowserContext 和 Page。
- 执行 Playwright 动作。
- 生成 accessibility snapshot、DOM 命中结果和截图。
- 使用 CDP `Page.startScreencast` 输出实时画面。
- 使用 CDP `Input.*` 接收用户输入。
- 输出 console、network、dialog、download 和 crash 事件。

Host 不直接连接前端，不直接修改 Goal，不保存 Magi 权限决策，也不直接向模型暴露工具。

### 6.3 `magi-tool-runtime`

- `ToolRuntimeResources` 新增 `BrowserToolExecutor`。
- 内置工具只做参数规范化、访问模式判定和结果格式化。
- 真实浏览器操作全部交给 `BrowserAuthority`。
- 浏览器工具沿用现有 ToolRegistry、Governance、进度、canonical tool call 和审计链路。

### 6.4 `magi-api`

- 新增 `routes/browser.rs`。
- REST 提供 Browser Session、Tab、标记和用户控制命令。
- SSE 只传播低频领域状态。
- 双向 WebSocket 单独承载高频 Screencast 帧和接管后的用户输入，禁止将图片 Base64 或鼠标事件放进 SSE/REST。

### 6.5 Web 前端

- `RightPaneTabKind` 新增 `browser`。
- 一个 Right Pane Browser Tab 只对应一个 `BrowserTabId`；同一 Browser Session 的多个网页必须表现为多个顶级 Right Pane Tab。
- Browser Pane 内部禁止再嵌套网页 Tab 条；用户通过顶级 Right Pane Tab 栏的 `+` 创建新的网页面板。
- 前端只持久化 `browserSessionId` 与 `tabId` 引用，不把 URL、租约、画面或运行状态写入 localStorage 作为事实源。
- HTML 文件的 Raw/Source 模式继续复用 Code Tab。
- HTML 文件的 Rendered/Preview 动作改为创建或复用 Browser Session，并导航到 daemon 提供的工作区站点 URL。
- `web/src/web/RightPane.svelte` 中 `htmlPreviewRevisions`、`htmlPreviewUrl`、`htmlPreviewMode` 和 iframe 渲染分支在 Browser Preview 验收通过后删除。
- `site-open` 一类后端工作区静态站点路由继续作为 Chromium 的受限资源入口，不再直接嵌入 Magi 主 WebView。
- 迁移必须原子完成：Browser Preview 可用、入口切换和旧 iframe 删除进入同一交付，不长期保留双预览路径。

### 6.6 Desktop 与发布层

- Magi 常规安装包只携带 Browser Runtime 协议客户端和组件管理器，不携带 Chromium、Node Runtime 或 Browser Host 资源包。
- Node Runtime、Browser Host 产物和 Playwright Chromium 组成独立签名、按平台发布的 Browser Runtime Component。
- 首次打开内置浏览器时由用户确认下载体积，组件管理器完成下载、签名校验和原子安装。
- daemon 使用 `magi-process` 的 Managed Child 管理已安装 Host 及其 Chromium 进程树。
- Desktop 只负责展示安装进度和定位运行组件，不拥有 Browser Session。
- daemon-only 开发入口使用同一版本化运行组件，不允许切换成系统 Chrome 形成双实现。
- Magi 升级后只要 Host 协议兼容，就继续复用已安装组件；仅在组件缺失、协议不兼容或浏览器安全更新时下载新版本。

## 7. 领域模型

### 7.1 BrowserProfile

```rust
pub struct BrowserProfile {
    pub profile_id: BrowserProfileId,
    pub kind: BrowserProfileKind, // 首版只有 ManagedDefault
    pub data_path: PathBuf,
    pub created_at: UtcMillis,
    pub updated_at: UtcMillis,
}
```

首版只有一个 Magi 托管 Profile，路径为：

```text
~/.magi/browser/profiles/default
```

它与 Safari、Chrome、Edge 等日常浏览器完全隔离。Cookie 和 localStorage 保存在 Chromium Profile 中，不写入 Magi JSON 状态文件。

### 7.2 BrowserSession

```rust
pub struct BrowserSession {
    pub browser_session_id: BrowserSessionId,
    pub workspace_id: WorkspaceId,
    pub session_id: SessionId,
    pub profile_id: BrowserProfileId,
    pub lifecycle: BrowserSessionLifecycle,
    pub active_tab_id: Option<BrowserTabId>,
    pub tab_ids: Vec<BrowserTabId>,
    pub runtime_epoch: u64,
    pub revision: u64,
    pub created_at: UtcMillis,
    pub updated_at: UtcMillis,
}
```

约束：

- 每个 Magi 会话最多一个未关闭 Browser Session。
- Browser Session 是逻辑页面集合，不等于 Chromium 进程。
- 多个 Browser Session 可以共享同一个 Magi Browser Profile，但页面归属必须隔离。
- 关闭聊天会话时关闭页面，不清除 Profile 登录数据。

### 7.3 BrowserTab

```rust
pub struct BrowserTab {
    pub tab_id: BrowserTabId,
    pub browser_session_id: BrowserSessionId,
    pub lifecycle: BrowserTabLifecycle,
    pub url: String,
    pub origin: Option<String>,
    pub title: String,
    pub viewport: BrowserViewport,
    pub navigation_revision: u64,
    pub snapshot_revision: u64,
    pub frame_sequence: u64,
    pub created_at: UtcMillis,
    pub updated_at: UtcMillis,
}
```

### 7.4 BrowserControlLease

```rust
pub struct BrowserControlLease {
    pub lease_id: BrowserLeaseId,
    pub profile_id: BrowserProfileId,
    pub browser_session_id: BrowserSessionId,
    pub owner: ExecutionOwnership,
    pub turn_id: String,
    pub goal_binding: Option<GoalControlBinding>,
    pub fence: u64,
    pub acquired_at: UtcMillis,
    pub expires_at: UtcMillis,
}
```

核心规则：

- 同一 Browser Profile 同一时刻最多一个有效写 Lease。
- Lease 只能操作其绑定 Browser Session 中的 Tab，不能借共享 Profile 跨会话读取页面。
- snapshot、screenshot 等只读命令仍校验会话归属和 revision，但不取得第二个写 Lease。
- 每个写动作必须携带 `lease_id + fence`。
- Lease 被撤销后永不恢复；Goal 恢复后重新申请新 Lease。
- 用户查看页面不需要 Lease，用户输入会先进入显式接管流程。
- 不允许通过延长旧 Lease 模拟 Goal 恢复。

采用 Profile 级单写者而不是 Tab 级多写者，是因为同一 Persistent BrowserContext 中的 Tab 会共享 Cookie、localStorage、Service Worker 和登录态。即使操作不同 Tab，两个代理也可能修改同一站点状态；首版禁止这种并发，稳定性优先于浏览器写操作吞吐量。

### 7.5 BrowserAnnotation

```rust
pub struct BrowserAnnotation {
    pub annotation_id: BrowserAnnotationId,
    pub browser_session_id: BrowserSessionId,
    pub tab_id: BrowserTabId,
    pub author: BrowserAnnotationAuthor,
    pub kind: BrowserAnnotationKind, // Element | Region
    pub anchor: BrowserAnnotationAnchor,
    pub comment: String,
    pub status: BrowserAnnotationStatus, // Active | Resolved | Stale | Deleted
    pub screenshot_artifact_id: Option<String>,
    pub created_at: UtcMillis,
    pub updated_at: UtcMillis,
}
```

元素锚点包含：

- URL、Origin、frame path、viewport 和 scroll offset。
- `data-testid`、稳定 id、ARIA role/name、标签名和文本摘要。
- CSS 结构路径和祖先指纹。
- DOM 文本/属性指纹。
- 创建时 bounding box 和 snapshot revision。
- 临时 CDP backend node id；该字段不能作为持久化唯一依据。

区域锚点使用相对 viewport 的归一化矩形，同时保存页面滚动位置和截图，不只保存屏幕像素坐标。

## 8. 状态机

### 8.1 Browser Host

```text
NotInstalled → Downloading → Verifying → Installed
                                      │
                                      ▼
Stopped → Starting → Ready
   ▲          │        │
   │          └→ Failed│
   │                   ▼
   └────── Recovering ← Degraded
```

- 组件下载、校验和安装是显式状态，必须在设置页和右侧栏展示，不得表现为浏览器无限启动。
- 心跳间隔 2 秒，连续 6 秒未响应进入 `Degraded`。
- Authority 只进行一次有界自动重启。
- 重启失败进入 `Failed`，禁止后台无限拉起。

### 8.2 Browser Session

```text
Creating → Ready → Recovering → Ready
    │         │         └──────→ Failed
    └─────────┴────────────────→ Closed
```

Browser Session 没有 `Paused` 状态。暂停属于 Goal/Turn，浏览器页面作为用户可见资源继续存在。

### 8.3 Browser Control Lease

```text
Available → Held → Released
                ├→ Revoked
                └→ Expired
```

`Released`、`Revoked`、`Expired` 都是终态。新操作必须获取新 Lease。

### 8.4 Browser Command

```text
Queued → Running → Succeeded
                 ├→ Failed
                 ├→ Cancelled
                 └→ Indeterminate
```

Host 在产生外部副作用后、回执前崩溃时，命令进入 `Indeterminate`。这类命令不得自动重试，模型必须重新读取页面状态。

## 9. Goal、Turn 与子代理联动

### 9.1 统一执行资源取消

当前 `ApiState::cancel_active_tool_executions` 实际只取消 Shell Process。浏览器接入时应将其收敛为：

```text
ExecutionResourceCoordinator.cancel(query, reason)
  ├─ ProcessExecutionRegistry.cancel(query)
  ├─ BrowserAuthority.revoke_leases(query, reason)
  └─ 后续其他长期资源
```

所有用户停止、Goal 暂停、会话关闭、任务树终止和 daemon shutdown 都调用这一处。禁止每个 API Route 分别拼接 Shell 与 Browser 清理逻辑。

### 9.2 状态对应关系

| 运行事件 | Goal/Turn 行为 | Browser 行为 |
| --- | --- | --- |
| 用户停止当前对话 | 当前 Turn 终止，关联 Goal/Plan 原子暂停 | 立即撤销执行链的 Browser Control Lease，取消未完成命令 |
| 用户点击 Goal 暂停 | Goal/Plan 原子暂停，Turn 终止 | 撤销 Lease，页面保留 |
| 用户点击 Goal 继续 | 创建新的 continuation Turn | 不恢复旧 Lease；后续工具重新申请，默认复用会话活动 Tab |
| 用户在浏览器接管 | 通过统一中断入口暂停关联 Goal | 先 fence 旧 Lease，再把输入权交给用户 |
| 子代理完成/失败 | Task 按现有状态机收口 | 释放该 Worker 持有的 Lease |
| Goal 完成 | Goal 进入 Complete | 释放 Lease，Browser Session 保留用于复核 |
| Browser Host 不可恢复 | 当前工具确定失败，关联 Goal 进入运行时 Blocked/Waiting | Session 进入 Failed，展示可诊断原因 |
| daemon 重启 | 现有恢复链将非终态 Turn 标记为 interrupted | 所有 Lease 作废，Session 恢复持久页面边界后等待用户继续 Goal |

### 9.3 用户接管顺序

1. `BrowserAuthority` 在进程内增加 Profile fence，旧 Lease 立即失效。
2. `ExecutionResourceCoordinator` 取消该执行所有者的浏览器命令与其他活动工具。
3. 会话运行时使用现有原子方法暂停 Goal 和绑定 Plan，并终止当前 Turn。
4. Authority 把控制模式切换为 `user`。
5. UI 收到权威事件后才启用输入转发。

如果第 3 步因并发 revision 冲突失败，Lease 仍保持撤销。reconciler 根据当前 Goal 真相重新决定是否需要暂停，绝不恢复已经失效的旧 Lease。

### 9.4 子代理并发

- 同一 Browser Profile 内的写浏览器操作串行化，不同子代理不能同时写不同 Tab。
- Lease 冲突返回 `browser_control_lease_conflict`，不排队偷取控制权，也不让主线静默抢占。
- 未持有浏览器 Lease 的子代理仍可并行执行 Shell、文件、搜索和其他工具，不阻塞整个代理图。
- 主线代理不自动抢占子代理；只有用户接管或执行树终止可以强制撤销。
- Lease owner 使用真实 `worker_id/task_id/execution_chain_ref`，不能只用角色名称。

## 10. 模型工具设计

为兼容 DeepSeek 等不同 Provider，工具 Schema 保持扁平，禁止大规模 `oneOf` 和任意 JavaScript。首版第一方工具固定为：

| 工具 | 用途 | 外部副作用 |
| --- | --- | --- |
| `browser_navigate` | 创建/复用会话与 Tab，导航 URL，前进、后退、刷新 | 可能 |
| `browser_snapshot` | 返回紧凑 accessibility/DOM 快照和稳定 ref | 无 |
| `browser_click` | 点击 snapshot ref | 有 |
| `browser_type` | 向可编辑 ref 输入文字 | 有 |
| `browser_press` | 发送按键或组合键 | 有 |
| `browser_scroll` | 页面或元素滚动 | 有 |
| `browser_screenshot` | 截取当前页面或元素，生成 artifact | 无 |
| `browser_tabs` | 列出、切换、新建、关闭 Tab | 本地状态 |
| `browser_viewport` | 读取或设置设备视口；同步控制 CSS 宽高、设备类型、移动端 UA 与触控语义 | 仅改变浏览器页面环境 |

Developer Mode 单独提供 `browser_inspect`，仅支持受限的 console、network、DOM、computed style 和 performance trace 子命令。它不进入默认工具集合。

`browser_viewport` 的状态由 Authority 持有两种尺寸模式：`auto` 跟随右侧面板尺寸，`fixed` 保持用户或模型指定的尺寸；设备仿真收敛为由宽度决定的唯一语义：`320-600` 为 `mobile` 手机窄屏，`601` 以上为 `desktop` 电脑/平板宽屏，禁止出现 `600px + desktop` 这类会把固定宽屏页面直接裁切的矛盾状态。用户连续调整自定义宽高时，前端以防抖方式动态提交并同步切换宽屏/窄屏状态，不需要额外确认。Browser Host 对每次尺寸变化只提交一次 CDP device metrics 更新，同时应用 UA、Client Hints 和触控能力，并等待同一渲染帧完成 `resize` 与布局提交后再确认成功；当前文档只接收一次浏览器原生 `resize`，按 CSS viewport、媒体查询和 Chromium 的移动端布局规则自然重排或等比适配，禁止先写入桌面尺寸再写入手机尺寸造成画面抖动。服务端 UA 分流只在用户或模型之后显式导航、前进、后退或刷新时生效，不能通过隐藏刷新丢失当前页面状态。

Screencast 的 `width` / `height` 表示页面实际 CSS 交互坐标空间，不是压缩后的设备表面尺寸；当无 viewport meta 的桌面文档在手机尺寸下由 Chromium 等比缩放时，Host 使用 `deviceWidth / pageScaleFactor` 还原完整布局坐标，前端点击、批注和画面缩放因此共享同一坐标系。图像像素仍按 Chromium 的设备缩放因子输出，以保持原生清晰度。截图统一直接读取当前 CDP 布局指标并按视觉缩放生成，不依赖 Playwright 的旧 viewport 缓存。每个新的页面画面订阅都会重新启动同一条 screencast 以立即产生首帧，静态页面在服务重连或面板重建后不再依赖刷新才能恢复画面。

面板 ResizeObserver 只允许提交带页面控制器身份的 `sync` 更新；同一个 `BrowserTabId` 在服务端同时只接受一个面板控制器写入，其他窗口只能通过获得焦点、操作画面或拖动面板显式接管。服务端在 `fixed` 模式下原子忽略所有迟到同步，切换回 `auto` 必须是用户显式操作。这样既不会出现模型已设置手机视口后被面板尺寸抢回，也不会在同一会话被多个 Magi 窗口打开时形成尺寸争用和刷新循环。

普通用户任务始终保留完整工具面，required tool chain 只用于完成证据，不用于逐轮隐藏其他工具。只有系统发起的确定性证据恢复轮次才收窄到目标工具；因此模型可以在同一回合中多次调用 `browser_viewport`，例如先验证桌面布局，再切换手机布局，最后读取视口复核。

### 10.1 工具目录与 Runtime Readiness 必须同源

浏览器工具是否出现在模型可见目录中，必须由同一个 `BrowserCapabilitySnapshot` 决定：

```rust
pub struct BrowserCapabilitySnapshot {
    pub revision: u64,
    pub in_app_browser_enabled: bool,
    pub browser_use_enabled: bool,
    pub runtime_status: BrowserRuntimeComponentStatus,
    pub host_protocol_compatible: bool,
    pub access_profile: AccessProfile,
}
```

规则：

- Runtime Component 未安装、校验失败、协议不兼容或被安全策略阻断时，不向模型暴露浏览器动作工具。
- `read_only` 只暴露允许的只读工具；click、type、press 等写工具不能先暴露、执行时再伪装成“工具不存在”。
- 工具目录构建和工具执行校验使用同一 snapshot revision，避免模型看到工具后被另一套访问模式拒绝。
- Runtime 安装入口属于用户 UI，不允许模型静默下载和执行新的运行组件。
- 系统上下文向模型说明 Browser Capability 的 `not_installed`、`update_required` 或 `unavailable` 状态，避免模型连续猜测工具名。
- Runtime 在当前 Turn 中途失效时，Authority 只进行一次有界恢复；恢复失败返回 `browser_runtime_unavailable` 并让关联 Goal 进入等待，不要求模型重复调用。

这条约束直接避免重现 Shell 曾出现的“模型看见或猜到工具，但当前运行表面没有该工具”的问题。

### 10.2 Snapshot 输出边界

- accessibility/DOM snapshot 默认最多 400 个节点、32 KiB 文本。
- 优先返回可交互元素、当前焦点、可见文本和发生变化的子树。
- 超出边界时返回 `truncated=true`、可继续读取的 subtree ref 和完整节点统计，禁止直接把整页 DOM 塞入模型上下文。
- Snapshot ref 只在当前 `snapshot_revision` 有效，导航、显著 DOM 更新或 Host 恢复后全部作废。
- 密码、隐藏字段、Token、Cookie 和请求授权头永不进入 snapshot。

工具规则：

- 工具调用不接受模型伪造 `worker_id`、`task_id`、`session_id` 或 Lease。
- 执行身份全部来自 `ToolExecutionContext`。
- `browser_snapshot` 返回 `snapshot_revision`，元素操作必须带相同 revision。
- revision 已变化时返回 `browser_snapshot_stale`，要求重新 snapshot。
- Host 恢复由 Authority 内部完成，模型不负责“再试一次启动浏览器”。
- 写动作只在 Host 明确未执行时允许 Authority 内部重试一次；结果不明时返回 `Indeterminate`。
- 工具输出提供稳定 `error_code`、`recoverable`、`requires_user_action` 和诊断字段，避免模型反复输出非生产说明。

## 11. 权限与安全

### 11.1 能力分层

Magi 设置分别控制：

- `in_app_browser`：是否显示内置浏览器 UI。
- `browser_use`：模型是否可以使用浏览器工具。

两项能力互不改写。完整 CDP 和外部浏览器不属于当前第一方工具面，也不在设置中暴露伪开关。

### 11.2 无浏览器权限弹窗

内置浏览器是 Magi 会话内的第一方能力。能力启用后，用户操作和模型基础浏览器工具均不触发 Origin 申请、系统权限弹窗或工具审批弹窗。浏览器工具目录与执行时校验共同读取同一轮 `BrowserCapabilitySnapshot`，不允许出现“模型看得见、运行时不可用”的错位。

安全边界由固定策略执行，而不是交给用户逐站点批准：

- 只允许 `http:`、`https:` 和 `about:blank`，禁止凭据 URL、`file:`、`javascript:`、`data:` 和浏览器内部 Scheme。
- 云元数据与 link-local 目标始终阻断；重定向同样经过导航校验。
- 工具只可访问当前 Magi 会话绑定的 Browser Session，不能跨会话读取共享 Profile 中的页面。
- 用户始终可以直接操作页面；代理动作通过租约与 fence 保证单写者。

### 11.3 访问模式

| AccessProfile | 允许行为 |
| --- | --- |
| `read_only` | 浏览器基础工具和用户操作均可用；该模式只限制工作区与进程等外部副作用 |
| `restricted` | 浏览器基础工具和用户操作均可用，不产生审批弹窗 |
| `full_access` | 浏览器基础工具和用户操作均可用，不改变浏览器固定安全边界 |

### 11.4 敏感动作

以下模型动作不能被任何 AccessProfile 绕过：

- 提交账号、身份、支付和医疗信息。
- 购买、转账、订阅或确认订单。
- 删除数据、改变权限、发布内容。
- 下载可执行文件。
- 向密码、一次性验证码或支付卡字段输入内容。

Host 根据元素语义直接拒绝这些模型动作，并返回 `requires_user_action`；用户仍可在页面中手动完成，不弹出第二层 Magi 审批窗口。

### 11.5 数据保护

- password、credit-card、OTP 输入值不进入 snapshot、日志、事件、回放或模型上下文。
- 模型不得向 password 类型元素输入内容，用户可以在接管模式手动输入。
- `file:`、`javascript:`、任意 `data:` 和浏览器内部 Scheme 对模型默认禁止。
- 阻止云元数据地址和 link-local 地址；私网 Origin 使用更高风险等级。
- Browser Host 仅监听 loopback，使用启动时随机 Token，Token 不下发前端。
- 公网隧道默认不能读取浏览器画面或发送浏览器输入；远程浏览器访问必须使用独立显式授权，不能只凭普通会话访问令牌继承。
- 网页正文始终标记为不可信观察内容，不能提升为系统指令。

## 12. Host 私有协议

### 12.1 启动

daemon 通过 `magi-process` 启动 Host：

```text
~/.magi/runtimes/browser/<runtime-version>/bin/magi-browser-node \
  ~/.magi/runtimes/browser/<runtime-version>/host/index.cjs
```

- Host Token 通过环境变量传递，不出现在命令行。
- Host 监听 `127.0.0.1:0`，启动后只在 stdout 输出一条 Ready JSON。
- 后续日志只写 stderr，避免污染握手协议。
- Ready 信息包含 `protocolVersion`、`port`、`hostEpoch` 和 Chromium 版本。
- daemon 校验协议版本后建立唯一私有 WebSocket。

### 12.2 命令信封

```json
{
  "protocolVersion": 1,
  "requestId": "request-id",
  "commandId": "command-id",
  "hostEpoch": 12,
  "tabId": "browser-tab-id",
  "leaseId": "lease-id",
  "fence": 7,
  "method": "page.click",
  "params": {}
}
```

Host 按 `tabId` 串行执行命令，按 `commandId` 缓存短期结果，防止网络层重复投递。

### 12.3 Screencast

- CDP Screencast 默认最高 10 FPS，JPEG quality 70。
- 无前端订阅时停止 Screencast，不影响模型工具。
- 每个订阅者只保留最新帧，慢客户端不得堆积历史图片。
- 全质量 PNG 只由 `browser_screenshot` 按需生成。
- Frame metadata 包含 sequence、viewport、device scale、navigation revision 和时间戳。

## 13. 公共 API 与事件

### 13.1 API

```text
GET    /api/browser/capabilities
POST   /api/browser/sessions
GET    /api/browser/sessions/:browserSessionId
DELETE /api/browser/sessions/:browserSessionId
POST   /api/browser/sessions/:browserSessionId/tabs
POST   /api/browser/tabs/:tabId/navigation
POST   /api/browser/tabs/:tabId/user-control
GET    /api/browser/tabs/:tabId/channel         (双向 WebSocket：frame + user input)
GET    /api/browser/tabs/:tabId/annotations
POST   /api/browser/tabs/:tabId/annotations
POST   /api/browser/annotations/:annotationId
```

Tool Runtime 不通过这些 HTTP API 回调 daemon，而是进程内调用同一个 Authority。

### 13.2 事件

```text
browser.runtime.status_changed
browser.session.created
browser.session.status_changed
browser.session.recovered
browser.tab.created
browser.tab.updated
browser.tab.closed
browser.tab.crashed
browser.lease.granted
browser.lease.released
browser.lease.revoked
browser.annotation.created
browser.annotation.updated
browser.command.indeterminate
```

所有事件使用现有 `EventEnvelope`，必须携带 workspace/session/task 上下文。前端通过 SSE 收到状态后，从 Browser API 拉取最新投影；Screencast 帧和用户输入不进入 EventBus。

## 14. 右侧栏交互设计

### 14.1 布局

Browser Pane 包含：

- 顶级 Right Pane Tab 中独占的单网页视图。
- 后退、前进、刷新和地址栏。
- 视口选择、缩放、截图和标记模式按钮。
- 工具栏末端的连接状态灯；恢复或失败时才显示低调提示。
- 跟随面板或固定调试分辨率的 Screencast Canvas。
- Canvas 上层独立 Overlay，用于鼠标、代理动作提示和标记。

页面不使用卡片包裹主浏览画面。工具按钮使用现有 Icon 系统并提供 tooltip。

### 14.2 自动控制权

- UI 不提供“用户控制 / 代理接管”选择器。
- 用户点击、键盘或滚轮输入会自动取得用户控制 fence，并直接转发到 Chromium。
- 模型写工具执行前自动回收代理 Lease；同一 Profile 仍保持单写者。
- 用户输入与代理写命令通过 Authority 和 fence 串行化，过期命令不能在控制权切换后继续执行。
- Goal 暂停、完成或异常中断会撤销代理 Lease，页面本身继续保留给用户查看和操作。

### 14.3 标记流程

1. 用户开启标记模式。
2. 点击元素或拖拽区域。
3. 前端把坐标、frame sequence 和 viewport 发送给 Authority。
4. Authority 校验 frame/navigation revision；页面已变化时拒绝旧坐标，未变化时才让 Host 执行 hit-test。
5. Authority 生成持久锚点和截图 artifact。
6. 用户填写评论并保存。
7. 发送消息时，canonical user message metadata 写入 `browserAnnotationRefs`。
8. Context Runtime 注入结构化锚点、元素样式摘要和截图引用；模型需要视觉检查时复用 `view_image`。

页面变化后的重新定位顺序：

1. 稳定测试属性或 id。
2. ARIA role/name。
3. CSS 结构和祖先指纹。
4. 文本、属性与几何综合匹配。

置信度不足时标记 `Stale`，禁止静默迁移到错误元素。用户或代理完成修改后可以显式标记 `Resolved`。

## 15. 持久化与恢复

### 15.1 文件边界

```text
~/.magi/browser/state.json
~/.magi/browser/profiles/default/
~/.magi/browser/artifacts/<session-id>/<browser-session-id>/
```

`state.json` 只保存 Session、Tab 最后 URL、标记和 revision，不保存 Cookie、网页正文或密码。

### 15.2 daemon 重启

1. daemon 加载 Browser state，把上次非终态 Session 标记为 `Recovering`。
2. 所有持久化 Lease 一律丢弃；Lease 本身不落盘。
3. 启动 Browser Host 和持久 Profile。
4. 按 Browser Session 恢复 Tab 顺序和最后已提交 URL。
5. `runtime_epoch` 和 Tab navigation/snapshot revision 增长。
6. 旧标记重新定位，无法定位的进入 `Stale`。
7. 发布 `browser.session.recovered`。
8. Goal 继续遵循现有 interrupted/waiting 逻辑，用户通过 Goal 卡片创建新 continuation Turn。

恢复只保证持久边界，不承诺恢复页面的瞬时运行内存：

- 可以恢复：Profile Cookie、localStorage、站点持久数据、Tab URL、Tab 顺序、截图和 Annotation。
- 尽力恢复但不保证：滚动位置、浏览历史位置和可序列化表单状态。
- 不能恢复：页面 JavaScript 堆、未提交表单输入、WebSocket 连接、内存路由临时状态、Canvas 运行态和执行中的下载。

恢复后的 Tab 必须增加 navigation/snapshot revision，旧元素 ref 全部失效，Annotation 重新定位。Goal continuation 的第一条浏览器操作必须重新 snapshot，不能假设页面仍处于崩溃前的瞬时状态。

恢复持久页面边界不等于恢复执行。任何代理必须通过新 Turn 获取新 Lease。

### 15.3 Host 崩溃

- Authority 立即 fence 全部 Lease。
- 正在执行的只读命令可在恢复完成后重试一次。
- 写命令只有在 Host 证明未开始执行时才可重试。
- 无法确定是否执行的命令返回 `browser_command_indeterminate`。
- 有界恢复失败后工具快速失败，当前 Goal 通过现有运行时失败路径进入 Blocked/Waiting，禁止模型重复尝试启动工具。

## 16. 独立运行组件与更新成本

### 16.1 运行组件结构

每个平台独立发布：

```text
magi-browser-runtime-<runtime-version>-<target>.tar.zst
  manifest.json
  bin/magi-browser-node
  host/index.cjs
  host/resources/*
  chromium/*
  licenses/*
```

`manifest.json` 至少包含：

- Runtime Component 版本。
- Host 协议版本和兼容范围。
- Node、Playwright、Chromium 版本。
- 目标平台和架构。
- 发布通道、单调递增的 `manifestSequence` 和 `expiresAt`。
- 最低兼容 Magi 版本和最低安全 Runtime 版本。
- 压缩包与每个关键文件的 SHA-256。
- 解压后大小。
- 签名与发布时间。
- macOS codesign/notarization、Windows Authenticode 等平台执行签名信息。

### 16.2 安装与升级策略

```text
用户打开内置浏览器
  ↓
检查当前平台 Runtime Component
  ├─ 已安装且协议兼容：直接启动
  ├─ 未安装：展示体积并由用户确认安装
  └─ 不兼容或强制安全更新：展示更新原因并安装新版本
```

组件安装目录：

```text
~/.magi/runtimes/browser/<runtime-version>/
~/.magi/runtimes/browser/active.json
```

安装过程必须：

1. 下载到 staging 目录，并持续展示进度、速度和剩余大小。
2. 校验发布签名、归档 SHA-256 和 manifest 文件哈希。
3. 解压完成后执行离线启动自检。
4. 通过原子 rename 激活 `active.json`。
5. 成功激活后清理不再使用的旧版本；不在运行时静默回退到旧组件。

Magi App 与 Browser Runtime 使用协议兼容范围解耦，例如 App 支持 Host Protocol `1.x`，已安装 Runtime 为 `1.4.2`。只要协议兼容，Magi `3.0.38`、`3.0.39` 等常规升级都复用同一组件，不重新下载 Chromium。

Browser Runtime 独立更新只在以下情况发生：

- Host Protocol 出现不兼容升级。
- Chromium/Playwright 有需要强制发布的安全修复。
- Browser Runtime 本身存在必须替换的缺陷。
- 用户主动清除了浏览器运行组件。

普通 Browser Host 业务代码变更优先保持协议兼容，并随 Runtime Component 独立发布，不强迫 Magi 主应用同步升级。

### 16.3 发布形式

- 默认 Magi 安装包保持轻量，不包含 Browser Runtime Component。
- Release 同时提供独立 Browser Runtime Component 下载。
- 有离线部署需求时可以额外提供“完整离线安装包”，其中包含完全相同的签名组件；它不是第二套运行实现。
- 组件下载源、签名公钥和最低安全版本由 Magi 的签名 manifest 管理，不能执行任意 URL 指向的 Runtime。

### 16.4 更新检测与用户操作

Magi 不直接根据 Chromium 上游版本号安装浏览器。发布系统必须先完成 Playwright 兼容测试、Browser Host 回归和三平台签名，再生成 Magi 自己的 Browser Runtime 更新 manifest。

更新 manifest 至少提供：

- 最新 Runtime、Chromium、Playwright 和 Host Protocol 版本。
- 当前 App 可用的 Host Protocol 兼容范围。
- `minimumSafeRuntimeVersion`。
- 更新级别：`optional`、`recommended` 或 `required_security`。
- 发布说明、下载大小、目标平台 URL、SHA-256 和签名。

检测时机：

- 已安装 Runtime Component 时，Magi 启动后后台检查一次，但不阻塞主界面。
- 尚未安装且用户从未启用内置浏览器时，启动过程不主动请求 Browser Runtime 更新服务。
- 距上次成功检查超过 24 小时时，在打开内置浏览器前检查。
- 设置页提供“检查浏览器更新”操作，用户可以随时主动检查。
- 同一进程内合并重复请求，避免启动、设置页和浏览器同时发起下载。

更新行为：

| 更新级别 | 产品行为 |
| --- | --- |
| `optional` | 设置页和浏览器工具栏显示更新提示，用户点击后下载；旧 Runtime 可以继续使用 |
| `recommended` | 持续展示更新提示，用户确认后下载；不打断正在运行的 Goal |
| `required_security` | 禁止创建新的代理浏览器 Lease；关联浏览器任务进入可恢复等待，用户确认安装后继续 |

任何级别都不静默安装新的可执行 Runtime。Magi 可以后台检查和预取 manifest，但下载、安装和重启 Browser Host 必须有清晰的用户操作与进度反馈。

manifest 校验必须拒绝：

- 签名无效、已过期或目标平台/发布通道不匹配。
- `manifestSequence` 低于本机已接受序号的回放响应。
- Runtime 版本低于已记录的最低安全版本。
- Host Protocol 超出当前 Magi 声明的兼容范围。
- macOS/Windows 可执行文件平台签名不满足发布策略。

安装切换遵循：

1. 下载、签名校验和自检都在 staging 目录完成，当前 Runtime 不受影响。
2. 普通更新在 Browser Host 空闲时由用户确认切换；运行中的 Goal 不被强制中断。
3. 安全强制更新先 fence 新 Lease，通过现有运行时失败链把关联 Goal 设置为 `Blocked/Waiting`，`blocker_key=browser_runtime_update_required`，再由用户确认安装。
4. 新组件自检通过后原子更新 `active.json` 并启动新 Host。
5. Profile、Browser Session、Tab URL 和 Annotation 继续保留；执行 Lease 一律重新申请。
6. 新组件未通过自检时不得激活，也不得破坏当前已安装组件。

预计影响：

| 项目 | 估算 |
| --- | --- |
| Magi 常规安装包增量 | 约 2–5 MiB，不包含 Chromium |
| 首次 Browser Runtime 下载 | 约 120–200 MiB，最终以三平台实测为准 |
| Browser Runtime 安装后磁盘 | 约 250–400 MiB |
| 后续 Magi 常规升级 | 不重复下载 Browser Runtime |
| Browser Runtime 安全更新 | 仅下载新的独立组件 |
| 空闲 Browser Host | 约 40–80 MiB RAM |
| Chromium 基础占用 | 约 180–350 MiB RAM |
| 每个复杂 Tab | 约 30–150 MiB RAM |

不使用系统 Chrome 作为体积回退方案。应用更新与浏览器组件更新解耦，同时保留稳定、版本一致和可复现的运行环境。

## 17. 代码落点

```text
crates/magi-core
  browser ids、公共执行资源标识

crates/magi-browser-runtime
  authority、领域模型、host client、lease、capability、annotation、recovery

browser-host
  Playwright、CDP、screencast、input、snapshot、host protocol

crates/magi-tool-runtime
  browser builtins、schema、BrowserToolExecutor 注入

crates/magi-conversation-runtime
  完整 ToolExecutionContext owner、浏览器失败与 Goal 收口

crates/magi-api
  browser routes、DTO、WebSocket、统一执行资源取消

crates/magi-daemon
  BrowserAuthority 装配、persistence、host supervisor、shutdown/recovery

apps/desktop
  Runtime Component 安装/更新展示、资源定位和发布配置，不维护业务状态

web/src/stores
  browser projection store；right-pane 只保留 tab 引用

web/src/components / web/src/web
  BrowserPane、canvas、toolbar、annotation overlay；删除 HTML iframe 预览分支
```

完整生产实现预计新增或修改约 18,000–26,000 行人工维护代码，不包含生成协议、lockfile 和打包清单。其中浏览器运行时与 API 约 6,000–8,000 行，Browser Host 约 4,000–6,000 行，前端与标记约 5,000–7,000 行，测试与发布约 3,000–5,000 行。

## 18. 实施顺序

实施顺序用于降低集成风险，但最终只保留上述一套正式实现：

1. 建立 `magi-browser-runtime` 的纯状态机、Lease 和 Host 协议。
2. 完成 Browser Host、持久 Profile、snapshot、动作、screenshot 和 Screencast。
3. 接入 daemon 生命周期、持久化、恢复、EventBus 和 API。
4. 将 `cancel_active_tool_executions` 收敛为通用 `ExecutionResourceCoordinator`。
5. 接入第一方浏览器工具和完整执行所有权。
6. 接入 Right Pane Browser Tab、Canvas 输入和自动控制权切换。
7. 将 HTML 文件预览入口切换到 Browser Session，并删除 RightPane 的 HTML iframe 渲染状态与分支。
8. 接入标记、canonical message 引用和 Context Runtime。
9. 完成 Runtime Component 下载、签名校验、原子安装和协议兼容复用。
10. 完成 Goal/子代理中断恢复、Host crash 和三平台发布闭环。

每一步都在同一目标架构上向前，不引入临时 MCP 实现、系统 Chrome 回退或第二套前端状态。

## 19. 验收标准

### 19.1 状态一致性

- 用户中断 Goal 后 1 秒内 Goal/Plan 为暂停，Turn 终止，Lease 失效。
- Goal 继续后创建新 Turn 和新 Lease，不复活旧命令。
- 共享 Profile 下同时最多一个代理执行浏览器写操作；其他子代理的非浏览器工具仍可并行。
- Goal 完成后浏览器仍可查看，但代理不能继续操作。
- 当前会话不会展示其他会话的 Browser Session 或状态。

### 19.2 故障恢复

- 强制杀死 Browser Host 后，不出现永久 loading 或伪运行状态。
- 写动作在未知结果时不自动重试。
- daemon 重启后恢复 Profile 与 Tab 持久边界、清空 Lease、废弃旧 snapshot ref，Goal 等待用户继续。
- Screencast WebSocket 断开只影响画面，不能改变 Goal 或工具执行事实。
- SSE lagged 恢复后 Browser UI 通过权威快照收敛。

### 19.3 工具与模型

- OpenAI 兼容模型和 Anthropic Messages 模型都能稳定调用全部基础浏览器工具。
- 使用项目真实配置的 DeepSeek 模型跑通导航、snapshot、点击、输入、截图和完成 Goal。
- Runtime 未安装、更新必需和访问模式受限时，模型可见工具目录与执行时能力完全一致，不出现 `tool_not_available` 错位。
- 模型误用 stale ref、错误 Tab、失效 Lease 时返回稳定错误，不出现重复结束或重复工具循环。
- 所有 AccessProfile 下浏览器基础工具均无权限弹窗；固定导航边界和敏感字段阻断不能被绕过。

### 19.4 UI 与标记

- 桌面和窄窗口下画面、工具栏、地址栏不重叠。
- 画面缩放后点击坐标与真实元素一致。
- 元素标记在普通 DOM 更新后可重新定位；不确定时进入 `Stale`。
- 标记发送到对话后，模型获得结构化锚点与截图证据。
- 代理运行中用户接管能可靠暂停 Goal，并可通过 Goal 卡片恢复。
- HTML 文件的源码仍可直接查看，运行预览只经过 Browser Session，RightPane 不再存在 iframe 第二渲染路径。

### 19.5 发布

- macOS Apple Silicon/Intel、Windows 和 Linux 都能下载并安装匹配平台的签名 Runtime Component。
- Magi 常规升级不会在 Host Protocol 兼容时重复下载 Chromium。
- 首次安装、下载中断、校验失败、磁盘不足和组件不兼容都有明确状态，不表现为浏览器卡死。
- 完整离线安装包中的 Runtime Component 与独立下载组件具有相同哈希和签名。
- 安装组件后基础浏览器可以离线启动，不依赖用户预装 Node 或 Chrome。
- 卸载只移除 Browser Runtime Component，并保留独立 Profile 数据供后续重装复用。
- 第三方许可证和 Chromium notices 进入发布产物。

## 20. 参考依据

- Codex 本地源码功能分层：`/Users/xie/code/codex/codex-rs/features/src/lib.rs`
- [Codex/ChatGPT Browser 官方说明](https://learn.chatgpt.com/docs/browser)
- [OpenAI CUA Sample App](https://github.com/openai/openai-cua-sample-app)
- [Steel Browser](https://github.com/steel-dev/steel-browser)
- [Microsoft Playwright MCP](https://github.com/microsoft/playwright-mcp)
- [Playwright BrowserContext](https://playwright.dev/docs/browser-contexts)
- [Tauri Sidecar](https://v2.tauri.app/develop/sidecar/)

参考项目提供实现经验，Magi 的最终真相源仍是自己的 Browser Authority、Execution Ownership、Goal 和 canonical turn。
