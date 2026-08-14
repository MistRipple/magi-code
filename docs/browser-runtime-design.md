# Magi 统一 Chromium 桌面与内置浏览器完整架构

> 状态：后续开发唯一产品架构
>
> 更新日期：2026-08-14
>
> 适用范围：Magi Desktop、Web UI、Rust daemon、Browser Automation、右侧面板、浏览器工具、发布、更新与旧实现清理

## 1. 最终决策

Magi Desktop 统一使用 Chromium 桌面宿主。现有 Svelte 业务界面和 Rust daemon 保留；Tauri 系统 WebView、独立 CEF、原生 NSView 覆盖、截图投影和 iframe 均不再属于产品架构。

唯一实现基线：

- Electron `BaseWindow` 作为跨平台 Chromium Desktop Host。
- 现有 Svelte 应用拆分为可信的 `MagiAppView`、`RightPaneChromeView` 和按需创建的 `DesktopOverlayView`。
- 每个桌面窗口中的每个 Browser Tab 由一个独立 `BrowserSurface` 承载，页面使用真实 `WebContentsView`。
- Electron Main Process 是物理页面、窗口布局、输入焦点、导航安全和进程生命周期的唯一所有者。
- Rust daemon 继续拥有 BrowserAuthority、会话、工具治理、Lease、Annotation 和 Artifact。
- 浏览器自动化通过 Electron `webContents.debugger` 的受控 CDP Gateway 执行，不开放 Chromium remote debugging port。
- 生产运行时不再依赖 Playwright 连接已运行 Electron；Playwright 只保留为外部端到端测试工具。
- Chromium、Desktop Host、Rust daemon 和 Browser Automation Worker 随同一个 Magi 桌面发行包签名、更新和回滚。
- 迁移完成后一次性删除 Tauri、CEF、Native Bridge、独立 Browser Runtime 安装器和所有兼容分支。

## 2. 产品目标

### 2.1 用户体验

- 右侧顶级 Tab 一个 Tab 对应一个浏览器页面，Browser Tab 内不存在第二层网页 Tab。
- 右侧面板是多功能面板，不是浏览器专用容器；浏览器、代码、图片、终端、Agent 等内容都通过同一顶级 Tab 框架切换。浏览器只是其中一种内容类型，不能改变右栏的通用 Tab、布局和拖拽契约。
- 浏览器 Surface 只占用右侧面板的内容区，不参与右侧面板的 Tab 栏、工具栏、拖拽边界或其他面板布局；切换到代码、图片或终端时，Browser Surface 仅隐藏，不改变右栏状态。
- 右栏宽度、拖拽手柄、顶级 Tab 和非浏览器内容在没有 Browser Surface、Browser Surface 未就绪或浏览器导航期间行为完全不变。
- 新增面板始终先经过可扩展的面板选择器；当前只提供浏览器，后续增加代码、图片、终端或其他类型时不得修改浏览器 Surface 的所有权边界。
- 新增浏览器 Tab 先提交 BrowserAuthority 的逻辑 Tab，再由 Electron Main 异步创建真实 Surface；创建和网络导航不得阻塞新增菜单、右栏拖拽、已有代码/图片/终端 Tab 或其他面板操作。Surface 创建失败只影响该浏览器 Tab，并返回可操作状态，不得锁死或清空整个右栏。
- 用户可以直接点击、输入、滚动、复制、粘贴、拖动、选择文本、打开右键菜单和使用输入法。
- 当前会话的 Agent 可以操作用户正在查看的同一个页面，不启动隐藏浏览器或第二份 Chromium。
- Agent 接管期间持续显示虚拟鼠标和 Tab 占用状态；用户输入立即接管当前 Surface。
- Agent 任务完成、暂停、失败或取消后只释放控制权，不关闭 Tab，不改变最终页面。
- `target="_blank"`、`window.open()` 和新窗口链接在当前 Browser Tab 中打开，不创建额外窗口或隐藏 Target。
- 右侧面板可以从最小宽度拖到窗口约三分之二，浏览器始终严格位于内容区。
- 右侧面板的宽度始终由 WindowLayoutState 约束，BrowserSurface 使用同一原生父窗口内容树中的内容区 bounds；禁止通过 DOM 测量、额外窗口、悬浮层或坐标补偿改变面板几何。
- 页面刷新、跳转、慢请求和工具执行期间不黑屏、不闪烁、不重建页面、不显示截图投影。
- auto、宽屏、窄屏和自定义 viewport 均由 Chromium 页面真实重排，不裁切旧桌面布局。
- 固定 viewport 只设置 Chromium 的 CSS viewport、设备类型和触控能力，禁止使用 `pageScaleFactor`、截图缩放或按右栏大小拟合模拟设备；页面在原生 1:1 Surface 中重排，超出部分由页面自身滚动，不得被桌面壳裁切或拉伸。
- 页面标记、截图和消息引用在刷新、重启、关闭 Browser Tab 和升级后仍可查看。

多功能右栏的硬约束：

1. `RightPaneChromeView` 是右栏唯一的通用内容壳，负责顶级 Tab、面板选择器、工具栏和非浏览器内容；Browser Surface 不是右栏的父容器，也不能替换它。
2. `BrowserSurfaceView` 只在当前顶级 Tab 为浏览器时显示，且只覆盖内容槽；代码、图片、终端、Agent 和未来面板类型由 Chrome View 独立渲染。图片预览可以作为代码/文件 Tab 的内容模式，但不得因此创建或占用 Browser Surface。
3. 右栏尺寸、分隔条、折叠/展开、顶级 Tab 切换和面板选择器属于 WindowLayout/RightPane 通用能力。浏览器的创建、导航、崩溃、viewport 和 CDP 状态不能写入或重置这些状态。
4. 新建浏览器采用两阶段流程：先完成轻量的逻辑 Tab/占位状态，再异步物化 WebContentsView。等待 daemon、页面导航或 Worker 就绪期间，已有面板仍可切换、右栏仍可拖动；只有当前新建项显示 loading/error。
5. 面板类型必须通过稳定的 `PanelKind`/能力目录扩展，禁止在浏览器组件中硬编码“浏览器是唯一面板”或为每种新面板复制一套右栏布局。

本次目标补充的验收条件：

- 代码、图片、终端、Agent 和后续面板继续使用同一个 `RightPaneChromeView` 的顶级 Tab；即使浏览器正在创建、导航、崩溃恢复或等待自动化 Worker，其他面板仍可立即打开、切换和关闭。
- 浏览器 `WebContentsView` 只能挂载到当前顶级浏览器 Tab 的内容槽，不能覆盖顶级 Tab 栏、浏览器工具栏、拖拽分隔条或把右栏变成浏览器专属窗口；内容槽还必须避开右栏左侧完整的 8px 拖拽命中区。右栏尺寸和拖拽行为由通用 `WindowLayoutState` 维护，切换浏览器不会重置或重新计算其它面板布局。
- 新建浏览器 Tab 必须先完成逻辑 Tab 和 loading 占位，再异步创建真实 Surface；页面网络加载、DOM 快照和 Worker 握手不得阻塞新增面板选择器、右栏拖拽或已有面板交互。
- 验收必须覆盖“浏览器创建中切换代码/图片/终端”“浏览器 Tab 与非浏览器 Tab 反复切换”“右栏拖到最大约三分之二后切换面板”和“浏览器 Surface 创建失败后其它面板仍可用”四类场景。

### 2.2 Agent 能力

- Worker 在 Web 开发任务中自动发现 Magi Browser 能力。
- Worker 可以启动项目服务、打开正确页面、读取 DOM/Accessibility、操作页面并验证结果。
- 工具覆盖导航、快照、点击、输入、键盘、滚动、hover、drag、表单、截图、Console、Network、Performance、Heap、PWA、Lighthouse 和设备仿真。
- 工具名称、schema、执行状态和 UI 卡片使用同一能力目录，不因组件启动顺序随机出现、消失或改名。
- 工具结果包含明确页面、Surface、导航 revision、截图和诊断身份，不能把普通 HTTP path 当成无意义结果展示。

### 2.3 隐私与更新

- 不读取用户 Chrome、Safari、Edge 或其他浏览器 Profile。
- 不访问系统钥匙串、密码、书签、历史、默认浏览器或浏览器同步信息。
- Browser Session 使用 Magi 私有内存 partition，退出 Desktop 后清理浏览数据。
- Chromium 作为 Magi Desktop 的组成部分随应用统一更新，不再单独安装、卸载或激活。
- 设置页展示真实组件版本、状态、更新进度、重启浏览器能力和清理 Magi 浏览器数据。

## 3. 明确非目标

- 不使用 `Page.startScreencast`、Canvas、图片帧或远程桌面作为用户浏览器渲染通道。
- 不使用 iframe 加载任意外部网站，不通过代理移除 CSP 或 `X-Frame-Options`。
- 不把外部网页加载进可信 Magi App Renderer。
- 不使用 Electron `<webview>` 标签作为正式实现。
- 不公开 Electron/Chromium remote debugging TCP 端口。
- 不让 Browser Automation Worker 创建、关闭或淘汰物理 WebContents。
- 不保留 Playwright 和直接 CDP 两套生产自动化实现。
- 不在 Chromium Host 缺失时回退到 CEF、系统 WebView、外部浏览器或用户 MCP。
- 不让普通 daemon Web 客户端提交物理浏览器尺寸或控制 Desktop Surface。
- 首版不自动淘汰后台 Browser Surface；用户关闭 Tab 或窗口才销毁页面。

## 4. 总体架构

```text
Magi Electron Desktop Host
  |
  |-- WindowManager
  |     BaseWindow 1..N
  |       |-- MagiAppView
  |       |     左侧导航、中间会话与全局应用区
  |       |-- RightPaneChromeView
  |       |     多功能右侧面板：顶级 Tab、浏览器工具栏、代码/图片/终端/Agent 内容
  |       |-- BrowserSurfaceView 0..N
  |       |     当前窗口内真实外部 Chromium 页面
  |       `-- DesktopOverlayView 0..N
  |             弹窗、菜单、标记选择等可信覆盖层
  |
  |-- WindowLayoutManager
  |     每窗口唯一布局状态、原子 layout revision、View bounds 与层级
  |
  |-- BrowserSurfaceManager
  |     Surface 创建、挂载、导航、焦点、partition、Target 和崩溃恢复
  |
  |-- BrowserCdpGateway
  |     webContents.debugger、Target allowlist、CDP domain 与命令校验
  |
  |-- BrowserAutomationWorker (Electron utilityProcess)
  |     DOM/AX 快照、元素引用、输入、截图、诊断和设备仿真
  |
  |-- DesktopControlServer
  |     Rust daemon 与 Electron Main 的私有版本化控制协议
  |
  `-- Rust daemon sidecar
        |-- BrowserAuthority
        |-- Browser Tool Runtime
        |-- Session/Goal/Worker Runtime
        |-- Annotation/Artifact Store
        `-- HTTP/SSE API
```

### 4.1 最终代码结构

迁移完成后仓库只保留下列 Browser/Desktop 模块：

```text
apps/desktop/                         Electron 桌面宿主，不再是 Rust/Tauri crate
  package.json
  electron-builder.yml
  src/main/                           Main Process 与唯一物理资源管理
    window-manager.ts
    window-layout.ts                   纯布局 reducer 与 bounds contract
    browser-surface-manager.ts         WebContents 生命周期与受控 CDP gateway
    process-supervisor.ts
    desktop-control-server.ts
    update-manager.ts
  src/preload/                        按 Renderer 拆分的最小 contextBridge
  src/renderer/                       Desktop 专用 Svelte 入口
    app-shell/
    right-pane/
    overlay/

browser-automation-worker/           utilityProcess 打包产物，只实现高层工具与 CDP 适配
  package.json
  src/

contracts/desktop-browser/           唯一协议源
  desktop-ipc.schema.json
  desktop-control.schema.json
  browser-tool.schema.json
  capability-manifest.schema.json

web/                                  共享 Svelte 组件、Web 客户端入口与 Desktop Renderer 构建资产

crates/magi-browser-authority/        Browser Authority、持久 Tab 事实与 Lease 治理
crates/magi-api/                      Browser HTTP/SSE/API 和 tool dispatch
crates/magi-session-store/            BrowserTab/Annotation/Artifact 持久化
crates/magi-tool-runtime/             内置工具目录与 schema 发布
```

目录收敛规则：

- 删除 `browser-host/`，不在原目录内继续保留“Host 创建页面”的旧语义。
- 删除 `apps/desktop` 的 Cargo workspace member 和全部 Tauri Rust 入口，同路径重建为 Electron package。
- Node 项目使用根目录 npm workspaces 和唯一 lockfile，统一 Electron、CDP types、Svelte 和构建工具版本。
- Rust 与 TypeScript 不手写重复 DTO；以 `contracts/desktop-browser` JSON Schema 生成类型并在 CI 校验生成结果无差异。
- `magi-browser-authority` 不包含二进制下载、Chromium 安装、Host 启动或物理页面管理代码。

### 4.2 权威与数据所有权

| 事实 | 唯一所有者 | 是否持久化 | 禁止的第二个写入者 |
| --- | --- | --- | --- |
| Browser Tab URL、标题、顺序、标签 | BrowserAuthority | 是 | Renderer localStorage、Electron Main |
| Annotation、Artifact、消息引用 | BrowserAuthority + canonical session store | 是 | Surface、Worker 本地状态 |
| Window、右栏宽度、active panel、active Surface | Electron Main | 否 | daemon、Renderer store |
| WebContents、Target、partition、focus、visibility | Electron Main | 否 | daemon、Worker |
| viewport、virtual cursor、loading、Surface Lease | Electron Main + BrowserAuthority 授权状态 | 否 | BrowserTab durable record |
| Tool schema、权限和调用审计 | Rust daemon | schema 持久，运行态不持久 | Renderer、Worker |
| CDP session、ElementRef、snapshot revision | Automation Worker | 否 | daemon durable store |
| 桌面版本、组件哈希和更新状态 | Electron Main | 更新日志持久 | Browser Runtime 独立状态机 |

任何实现如果需要两个模块同时修改同一事实，说明边界设计错误，不得通过双向同步、时间戳覆盖或兼容字段解决。

### 4.3 启动与就绪顺序

```text
Electron app ready 前应用隐私启动参数
  -> 校验签名 capability manifest 和 resources 哈希
  -> Main 生成 desktopEpoch 和随机 control token
  -> 启动 Rust daemon，完成协议/版本/父进程握手
  -> 创建 BaseWindow 和可信 Renderer Views
  -> 启动 Automation Worker，完成 workerEpoch 握手
  -> 注册 Browser capability = Ready
  -> 恢复逻辑 BrowserTabRecord，首次激活时才物化 Surface
```

- manifest、daemon 或协议校验失败时，Desktop 显示唯一且可操作的启动错误，不创建半激活 Browser Surface。
- Worker 未就绪不阻止用户浏览；只将 Agent Browser capability 置为 `AutomationStarting/Failed`。
- 启动阶段不恢复 viewport、focus、Lease、滚动位置或旧 Target identity。
- 同一个 `desktopEpoch` 内不允许第二个 Main Supervisor 接管子进程或 Surface。

## 5. Desktop View 树

所有 View 都是同一 `BaseWindow.contentView` 下的原生兄弟视图，不是额外 `BrowserWindow`、无边框子窗口或悬浮窗。`BrowserSurfaceView` 不能成为 Svelte DOM 元素，因此必须由 Electron Main 在同一原生组合树中设置 bounds 和 z-order；但几何只来自 WindowLayoutState，不来自持续 DOM 测量。

稳态层级只有：

```text
DesktopOverlayView        临时需要时存在
BrowserSurfaceView        当前 Browser Tab 的真实页面
RightPaneChromeView       Tab 和工具栏
MagiAppView               左侧和中间应用区
```

不允许在此树之外再建立“浏览器承载窗口”、“透明定位窗口”或“视频/截图层”。

### 5.1 MagiAppView

`MagiAppView` 加载现有 daemon 托管 Web 应用，但不再渲染右侧面板。它负责：

- 工作区和会话导航。
- 中间消息、编辑器和输入区。
- 全局 Header 和不跨越右侧面板的应用 UI。
- 将右侧展开/折叠、窗口和全局 Overlay 意图提交给 Electron Main。

安全配置：

- `nodeIntegration: false`
- `contextIsolation: true`
- `sandbox: true`
- 固定 CSP
- 只允许 daemon canonical origin 和签名本地资源
- preload 仅暴露版本化、allowlisted `contextBridge` API

### 5.2 RightPaneChromeView

`RightPaneChromeView` 是独立可信 WebContentsView，复用现有 Svelte RightPane 业务组件，负责：

- 顶级右侧 Tab 栏和 `+` 面板选择。
- Browser 地址栏、后退、前进、刷新、外部打开、截图、标记和 viewport 控件。
- 代码、终端、Agent 等非 Browser Panel 内容。
- Browser 内容区占位背景和加载/错误状态。

当当前 Tab 是 Browser：

- RightPaneChromeView 只绘制 Tab、工具栏和内容背景。
- BrowserSurfaceView 由 Electron Main 放在内容区上方。
- BrowserSurfaceView 的 bounds 只来自当前 WindowLayoutSnapshot 的 `browserSurfaceBounds`，并且被严格限制在 RightPaneChromeView 的内容区内。

当当前 Tab 不是 Browser：

- 当前窗口的 BrowserSurfaceView 全部设为不可见。
- RightPaneChromeView 自己渲染完整内容。
- 代码、图片、终端和 Agent 内容直接由 RightPaneChromeView 渲染，不经过 BrowserSurfaceView，也不因为浏览器 Surface 的创建、导航或崩溃而丢失。

### 5.3 BrowserSurfaceView

一个 `BrowserSurfaceView` 对应一个 `BrowserSurfaceInstance`，只加载外部网页。安全配置：

- `nodeIntegration: false`
- `contextIsolation: true`
- `sandbox: true`
- 无 Magi preload
- 无 Magi IPC
- 禁止 `file:`、本地资源和内部 Scheme
- 使用 Browser Session 对应的非持久 partition

非活动 Surface 保持 WebContents 存活，但使用 Electron View 的可见性控制隐藏；不得缩放到 `1 x 1`、导航到空白页或在切换期间销毁。
- Surface 创建、导航和加载失败都通过状态事件反馈给右栏；右栏必须保留 Tab 栏和其他内容的可操作状态，不能用全屏加载层覆盖整个 RightPaneChromeView。

### 5.4 DesktopOverlayView

`DesktopOverlayView` 是 Electron Main 管理的临时可信层，层级始终位于 BrowserSurfaceView 上方。它承载：

- 会跨越 Browser 内容区的下拉菜单和上下文菜单。
- 全局设置、确认和错误 Modal。
- 元素/区域标记选择层。
- 需要捕获浏览器区域输入的临时操作层。

Overlay 只覆盖所需矩形；关闭后立即销毁或隐藏。普通浏览状态不保留全窗口透明 Overlay，避免阻断 BrowserSurface 输入。

Agent 虚拟鼠标是非交互装饰，使用 Magi 包内签名的矢量光标资产，通过 CDP isolated world 中的 closed Shadow DOM 绘制，固定 `pointer-events: none`；每次 document 创建后重新注入。元素高亮使用 CDP Overlay domain，区域标记使用 DesktopOverlayView 捕获，不依赖站点 DOM。

### 5.5 多 Renderer 状态协议

`MagiAppView`、`RightPaneChromeView` 和 `DesktopOverlayView` 之间禁止直接访问对方 DOM、Svelte store 或 `window` 对象。三者只通过 Electron Main 的版本化 IPC 通信。

Main 为每个窗口发布不可变 `DesktopWindowSnapshot`：

```text
desktopEpoch
windowId
snapshotRevision
layout
activePanel
activeTabId
surfacePresentation
leasePresentation
capabilityPresentation
```

- Renderer 只提交意图，例如 `requestRightPaneWidth`、`activatePanel`、`navigateBrowser`和 `beginAnnotation`。
- Main 校验 intent 的 epoch、window、revision、权限和参数后 reduce 状态，再广播新 snapshot。
- BrowserTab durable 更新由 daemon 事件进入 Main，Main 合并为展示 snapshot；Renderer 不直接把窗口状态写回 daemon。
- Desktop 专用 active panel、right pane width、viewport 和 Surface 状态不写 `localStorage`、URL query 或 session bootstrap。
- Web 客户端继续使用独立 `web.html` 入口；它可以查看持久 Browser 记录，但不参与 DesktopWindowSnapshot 或物理 Surface 控制。

## 6. 唯一布局模型

### 6.1 WindowLayoutState

Electron Main 为每个窗口维护唯一状态：

```text
WindowLayoutState
  desktopEpoch
  windowId
  layoutRevision
  clientBounds
  displayScaleFactor
  fullscreen
  safeAreaInsets
  rightPaneVisible
  rightPaneMode          side-by-side | overlay
  rightPaneWidth
  rightPaneTabBarHeight
  browserToolbarHeight
  activePanelKind
  activeTabId
  activeSurfaceId
```

布局算法位于独立纯 TypeScript contract 包，由 Electron Main 唯一调用。Renderer 只接收 LayoutSnapshot 和 CSS variables，不独立计算 BrowserSurface 几何。

### 6.2 布局事务

右栏拖动流程：

```text
RightPaneChromeView pointer intent
  -> Desktop IPC { windowId, requestedWidth, clientSequence }
  -> WindowLayoutManager reduce
  -> layoutRevision + 1
  -> 同一 Main 事件循环内：
       set MagiAppView bounds
       set RightPaneChromeView bounds
       set active BrowserSurfaceView bounds
       update View z-order/visibility
  -> 广播同一 LayoutSnapshot 给两个可信 Renderers
```

约束：

- 只接受当前 `desktopEpoch/windowId` 的请求。
- 只应用单调增加的 `layoutRevision`。
- 同一事务更新所有 View，不存在独立异步 resize 队列。
- resize 不得触发 focus、导航、页面刷新、设备仿真或 WebContents 重建。
- 窗口缩放、全屏、DPI、显示器切换和安全区变化经过同一 reducer。
- Renderer 不调用 `getBoundingClientRect()` 向 Main 提交 BrowserSurface 绝对坐标。
- Web 客户端没有 Desktop layout capability，不得写入该状态。

### 6.3 尺寸规则

- 中间内容区最小宽度保持可用，不要求维持完整桌面布局。
- 右侧面板最大宽度为当前窗口可用宽度的三分之二。
- 右侧面板最小宽度满足浏览器工具栏和 320 CSS px 内容区。
- 小窗口进入 overlay 模式时，RightPaneChromeView 和 BrowserSurfaceView 作为同一布局事务覆盖 MagiAppView。
- 浏览器内容 bounds 始终等于右侧内容槽，不含 Tab 栏、工具栏、分隔条和窗口安全区。

## 7. 逻辑 Tab 与物理 Surface

### 7.1 BrowserTabRecord

BrowserAuthority 持久化逻辑对象：

```text
BrowserTabRecord
  tabId
  browserSessionId
  order
  canonicalUrl
  pageTitle
  displayLabel?
  lifecycle
  navigationRevision
  annotationSequence
  createdAt
  updatedAt
```

`displayLabel` 是用户编辑标题，`pageTitle` 来自网页。显示优先级：

```text
displayLabel ?? pageTitle ?? canonicalUrl ?? 新建浏览器
```

网页标题更新不得覆盖用户编辑标题。

BrowserAuthority 不再持久化：

- `activeTabId`
- WebContents id、Target id、BrowserContext id
- viewport、scale、物理 bounds
- Agent Lease、用户控制模式和光标
- Window、Desktop 或 Surface identity

### 7.2 BrowserSurfaceInstance

每个桌面窗口按需物化：

```text
BrowserSurfaceInstance
  desktopEpoch
  windowId
  surfaceId
  surfaceRevision
  tabId
  webContentsId
  targetId
  browserContextId
  partitionId
  currentUrl
  viewportMode
  viewportMetrics
  loadingState
  focused
  visible
  primary
```

同一逻辑 Tab 可以在不同桌面窗口拥有独立 Surface。每个 Surface 使用自己的物理尺寸、viewport、焦点、光标和页面运行态，绝不互相 resize。

### 7.3 Primary Surface

同一逻辑 Tab 同时只有一个 Primary Surface：

- 用户在另一个 Surface 输入时，Electron Main 原子提升该 Surface 为 Primary。
- 提升会增加 `surfaceRevision`、撤销旧 Surface Lease，并通知 BrowserAuthority。
- 只有 Primary Surface 的顶级导航写入 `canonicalUrl/pageTitle/navigationRevision`。
- Secondary Surface 保留当前页面，不因 Primary 导航被强制刷新或改尺寸。
- Secondary Surface 显示“页面已在另一窗口更新”状态，用户可显式同步到 canonical URL。

普通 Web 客户端只读取 BrowserTabRecord、Annotation 和 Artifact，不创建 BrowserSurface，不提交 viewport 或布局。

### 7.4 Surface 生命周期

```text
absent -> creating -> ready -> hidden -> ready
ready/hidden -> crashed -> recreating -> ready
ready/hidden/crashed -> closed
```

- 激活逻辑 Tab 时按窗口创建或复用 Surface。
- 切换 Tab 只改变 visible/active，不销毁页面。
- Browser Renderer 崩溃只影响对应 Surface。
- 首版不因后台、任务完成或容量自动回收页面。
- 用户关闭窗口时销毁该窗口全部 Surface，但保留逻辑 BrowserTabRecord。
- 用户关闭逻辑 Browser Tab 时关闭全部 Surface；Annotation 和历史 Artifact 不删除。

## 8. 页面所有权与导航

### 8.1 唯一所有者

`BrowserSurfaceManager` 是 WebContents 的唯一所有者。它负责：

- create、activate、hide、focus、navigate、reload、goBack、goForward、close。
- partition、permission、download、dialog、crash 和 page event。
- WebContents 与 Tab/Surface/Target 的绑定。
- URL、标题和 loading 事件写回 DesktopControlServer。

BrowserAutomationWorker 不得调用 `newPage()`、创建 BrowserWindow、关闭 WebContents 或淘汰页面。

### 8.2 新窗口策略

在每个 Browser WebContents 创建时安装 `setWindowOpenHandler()`：

- `http/https` GET 请求：`deny` 新窗口，并在当前 WebContents 导航。
- 可安全转移的 POST：`deny` 新窗口，并在当前 WebContents 重放原请求。
- 无法安全转移的 `about:blank` 动态窗口、脚本写入窗口和不受支持 Scheme：阻止并展示明确错误。
- OAuth、下载和外部协议由产品策略接管，不允许创建隐藏 Target。

必须在 Chromium 创建新 WebContents 之前阻止请求。禁止“先创建 popup，再关闭并导航原页”。

### 8.3 导航状态

- 导航期间保持同一 WebContentsView 可见，不 detach、不隐藏、不替换背景层。
- Surface 设置与主题一致的背景色，首帧到达后再显示新创建 Surface。
- 页面刷新和慢请求使用 Chromium 正常渲染过程，不显示黑色占位。
- 每次顶级导航增加 `navigationRevision`，旧 snapshot、元素 ref 和标记选择明确失效。
- URL 仅允许 `http`、`https` 和 `about:blank`。
- 禁止凭据 URL、`file`、`javascript`、`data`、Chromium 内部 Scheme 和云环境 metadata 目标。
- `localhost`、`127.0.0.1`、`[::1]` 和经 Workspace 开发服务注册的 LAN URL 必须可用，用于 Agent 启动和验收本地 Web 项目。
- 用户手动导航和 Agent 导航使用不同策略：用户保留正常浏览能力，Agent 访问私有网段时必须经 Workspace 开发服务注册或明确授权。

## 9. Browser Session 与数据隔离

### 9.1 Partition

- MagiAppView、RightPaneChromeView、DesktopOverlayView 使用可信应用 partition。
- 每个 Browser Session 使用独立、非持久 Electron partition。
- 同一 Browser Session 在同一 Desktop 进程的多个 Surface 可以共享该内存 partition。
- 不同 Browser Session、不同 Desktop 实例和用户外部浏览器不共享数据。
- Desktop 退出后清理 Cookie、缓存、Service Worker、IndexedDB、localStorage 和临时下载。

### 9.2 隐私启动策略

Electron `app.ready` 前应用：

- `--use-mock-keychain`
- 禁用密码保存、自动填充、同步、默认浏览器检查和后台组件更新。
- 不调用 Electron `safeStorage` 存储浏览器资料。
- 不扫描、导入或迁移系统浏览器 Profile。

系统证书信任、DNS、代理和图形设备属于网页正常运行依赖，不视为读取用户浏览器资料；不得将其结果写入会话或遥测。

### 9.3 权限

每个 Browser Session 同时设置 permission check 和 request handler。麦克风、摄像头、通知、地理位置、蓝牙、USB、串口、MIDI 和屏幕捕获默认拒绝并完成 callback，不弹出第二层系统错误。

## 10. Desktop、daemon 与 Worker 进程

### 10.1 Electron Main 是唯一 Supervisor

Electron Main 管理：

- Rust daemon sidecar。
- BrowserAutomationWorker `utilityProcess`。
- 应用更新和重启。
- 全部子进程退出、超时和孤儿清理。

daemon 和 Worker 不互相启动对方，也不从 `PATH` 搜索可执行文件。

### 10.2 Rust daemon sidecar

daemon 从应用签名 resources 固定路径启动，必须具备：

- parent PID 绑定。
- 随机实例 token。
- 协议版本握手。
- 健康检查和 canonical `38123/web.html` 入口。
- 优雅停止、超时强制退出和端口身份校验。
- 更新前冻结新任务并等待已有写操作结算。
- Desktop 异常退出后的自终止机制。

DesktopControlServer 使用每个 Desktop 实例独立的 Unix domain socket/macOS/Linux 或 named pipe/Windows，并校验随机 token、对端 PID、desktopEpoch 和协议版本。物理 Surface 控制不经 `38123` HTTP API，不暴露给普通 Web 客户端或其他本地进程。

### 10.3 BrowserAutomationWorker

Worker 使用 Electron `utilityProcess.fork()` 运行打包后的 Node 模块，继承 Electron 自带 Node Runtime，不再分发独立 Node。

Worker 与 Electron Main 只通过 MessagePort 通信：

- 接收经过身份校验的高层浏览器命令。
- 通过 BrowserCdpGateway 请求允许的 CDP domain/method。
- 不获得 `webContents` 对象、任意用户文件系统、Desktop Renderer IPC 或调试端口。
- 只可读取签名的 Worker 自身资产，并使用按 workerEpoch 隔离的临时目录；Artifact 必须通过 daemon API 持久化。
- 崩溃后由 Main 重启，并从 Surface Registry 重建绑定。

### 10.4 故障隔离与恢复

| 故障 | 用户页面 | 控制权 | 恢复动作 |
| --- | --- | --- | --- |
| Automation Worker 崩溃 | 原 Surface 保持可见可操作 | 立即释放全部 Worker Lease | Main 重启 Worker，重绑现有 Target |
| Rust daemon 崩溃 | 原 Surface 保持可见可操作 | Agent 能力失效，用户保留控制 | Main 限次重启 daemon 并重做协议握手 |
| Browser Renderer 崩溃 | 仅对应 Surface 显示明确崩溃状态 | 释放该 Surface Lease | 用新 revision 重建 Surface，只导航 canonical URL |
| RightPaneChromeView 崩溃 | BrowserSurface 不销毁 | 暂停新 UI intent | 由 Main 用当前 DesktopWindowSnapshot 重建 ChromeView |
| MagiAppView 崩溃 | BrowserSurface 和 RightPane 保持 | Agent Browser Lease 按任务状态决定 | 重建 AppView，不重建 Browser Surface |
| Electron Main 崩溃 | 整个 Desktop 退出 | 所有 Lease 失效 | 父进程绑定保证子进程退出，下次启动只恢复逻辑 Tab |
| 更新中断 | 继续使用完整旧版本 | 不启动半版本 Worker | 回滚到同一 manifest 的完整发行包 |

恢复约束：

- 不自动重放 click、type、submit、drag、upload 等写操作。
- 不用销毁并重建 WebContents 作为通用“刷新”方案。
- 每次恢复都增加对应 epoch/revision，旧命令、ElementRef、snapshot 和 Lease 必须结构化失效。
- 重启超过预算后进入稳定 `Failed` 状态，禁止无限循环启动和 UI 闪烁。

## 11. 受控 CDP 自动化

### 11.1 不开放调试端口

Electron Main 对指定 BrowserSurface 的 `webContents.debugger` 建立连接。`BrowserCdpGateway`：

- 只允许 BrowserSurface Target，永不暴露 MagiAppView、RightPaneChromeView 或 OverlayView。
- 校验 `desktopEpoch/windowId/surfaceId/surfaceRevision/targetId`。
- 按工具声明 allowlist CDP domain 和 method。
- 拒绝 Browser、Target 和 SystemInfo 等越权全局操作。
- 记录调用 id、耗时、结果类型和错误码，不记录密码、Cookie、页面正文或调试 token。
- 不监听 TCP，不生成可被其他本地进程访问的 DevTools URL。

### 11.2 生产自动化实现

BrowserAutomationWorker 使用直接 CDP，不在生产运行时使用 Playwright `connectOverCDP()`。核心实现：

- `DOMSnapshot`、`DOM`、`Accessibility`：页面结构和稳定 ElementRef。
- `Runtime`：受控查询与 WebMCP；任意 JavaScript evaluate 受独立策略限制。
- `Input`：鼠标、触控、键盘、drag 和滚动。
- `Page`：导航等待、截图、Dialog、生命周期和 document 注入。
- `Network`：请求、响应、失败和 HAR 风格摘要。
- `Emulation`：viewport、device metrics、touch、UA 和 orientation。
- `Performance`、`Profiler`、`HeapProfiler`：诊断工具。
- `Overlay`：元素高亮和 inspect 辅助。

Lighthouse 作为 Worker 内部审计适配器运行，必须复用 BrowserCdpGateway 已授权的当前 Surface 会话；不得启动 Chromium、打开调试端口或创建隐藏 Page。

Playwright 可以继续作为 Desktop E2E 测试依赖，但不得进入生产 Browser capability manifest 或运行链。

### 11.3 Surface 绑定协议

Desktop Surface Binding：

```text
desktopEpoch
windowId
surfaceId
surfaceRevision
tabId
webContentsId
targetId
browserContextId
navigationRevision
```

Worker 启动或重启：

```text
query Surface Registry
  -> 接收当前 ready bindings
  -> 为每个 Surface 建立 PageRuntime
  -> 不创建新页面
  -> 上报 automation-ready
```

Surface 崩溃并重建后 targetId 和 surfaceRevision 改变，旧命令立即失效。

## 12. 工具执行与控制权

### 12.1 工具流水线

```text
Tool call
  -> 参数 schema 校验
  -> 当前 session/workspace/worker 身份注入
  -> 解析逻辑 Tab 与 Primary Surface
  -> 校验 capability/desktop/worker epoch
  -> 获取或校验 Tab+Surface Lease
  -> 每 Surface 串行命令队列
  -> navigation/snapshot/surface fence
  -> CDP 执行
  -> 规范化 canonical result
  -> 持久 tool/result 与 browser event
  -> UI presentation
```

每个命令携带：

```text
callId
sessionId
tabId
surfaceId
desktopEpoch
workerEpoch
surfaceRevision
navigationRevision
snapshotRevision?
leaseId/fence?
cancellationId
```

写操作结果不明时禁止自动重放。取消必须等待命令达到可判定终态。

### 12.2 Lease

Lease 绑定：

```text
tabId + surfaceId + executionOwnership + turnId + fence
```

- 不再使用 Profile 级 `BrowserProfileControlMode`。
- 不再把同一 Browser Session 的全部 Tab 标记为 AI 占用。
- 用户在 Surface 中产生真实输入时，Electron Main 撤销该 Surface Lease 并增加 fence。
- Agent 后续写命令收到结构化 `browser_control_revoked`，不能继续静默输入。
- read 工具可以按策略在无写 Lease 时运行，但必须绑定正确 Surface revision。
- 任务终态、暂停、取消、Worker 崩溃和 Desktop 重启全部释放 Lease，不关闭页面。

### 12.3 工具目录和能力状态

产品内置工具 schema 稳定注册。每个 turn 的能力快照包含：

```text
DesktopStarting
AutomationStarting
Ready
Restarting
Failed
ProtocolIncompatible
```

移除 `NotInstalled/Downloading/Verifying/UpdateAvailable/Uninstalling` 等独立 Runtime 状态。未 ready 时工具返回结构化可操作错误；`browser_status` 始终可用，Worker 可以发现并等待 Browser capability。

所有失败使用稳定错误码，至少覆盖 `browser_not_ready`、`browser_protocol_incompatible`、`browser_surface_not_found`、`browser_surface_stale`、`browser_navigation_changed`、`browser_control_revoked`、`browser_element_ref_stale`、`browser_permission_denied`、`browser_screenshot_failed` 和 `browser_worker_failed`。UI、LLM 与日志使用同一 canonical error payload，禁止展示无意义 HTTP path 或内部堆栈。

## 13. Viewport 与响应式调试

### 13.1 Surface 级状态

viewport 只属于 `BrowserSurfaceInstance`：

- 不写 BrowserAuthority durable state。
- 不跨 Tab、窗口、Desktop 实例或普通 Web 客户端同步。
- Surface 新建或重建后默认 `auto`。
- 同一逻辑 Tab 的两个 Surface 可以使用不同 viewport。

### 13.2 auto

`auto` 清除全部 device metrics override。网页 CSS viewport 由 BrowserSurfaceView 实际内容 bounds 决定，Chromium 自然触发 resize、media query、flex/grid 和 viewport 单位重排。

### 13.3 fixed

预设：

- 宽屏 `1280 x 800`
- 窄屏 `390 x 844`
- 用户自定义 width/height

输入短防抖动态生效，无确认按钮。Worker 使用 `Emulation.setDeviceMetricsOverride` 设置 CSS viewport、device scale factor、screen、touch、orientation、UA 和 Client Hints。

为保证完整适配而非裁剪：

```text
contentScale = min(
  1,
  surfaceContentWidth / cssViewportWidth,
  surfaceContentHeight / cssViewportHeight
)
```

- 使用 Chromium device metrics `scale` 完成浏览器内部缩放。
- 画面在内容槽内居中，剩余区域使用主题背景留白。
- 不使用截图、Canvas、CSS transform 或外层位图缩放。
- 用户输入由 Chromium 命中测试处理，不由 Svelte 转换坐标。
- 工具坐标保持 CSS px；CDP 截图 clip 与 ElementRef 统一使用 CSS px。
- viewport 修改只使当前 Surface 的 snapshot revision 失效，不导航、不刷新、不创建页面。

## 14. 快捷键、焦点和输入

Electron Main 维护每窗口唯一 `focusedSurface`：

- 地址栏、消息输入框、终端和 BrowserSurface 焦点互斥。
- resize、状态刷新和页面事件不得调用 focus。
- 用户点击 Browser 内容时才将焦点交给 BrowserSurface。
- 切换 Browser Tab 时只在用户明确激活时聚焦。
- `Cmd/Ctrl+C/X/V/A/Z/Shift+Z` 根据 focusedSurface 路由。
- Browser 页面使用 Chromium 原生编辑命令；Magi 文本区使用可信 Renderer 编辑命令。
- 应用级快捷键先处理明确保留项，其余交给 focused WebContents。
- 中文、日文、韩文 IME、死键、组合输入和系统文本服务进入真实 focused WebContents，不通过自定义键盘映射。

Agent 的 CDP 输入不得改变 MagiAppView 或 RightPaneChromeView 的焦点。

## 15. 标记、截图与消息

### 15.1 标记

元素标记：

1. DesktopOverlayView 进入标记模式。
2. 指针移动通过 Surface 本地坐标提交 hit test。
3. Worker 使用 CDP 获取元素、frame、AX 和 bounding box。
4. CDP Overlay domain 高亮当前元素。
5. 用户确认并输入备注。
6. Worker 只截取元素 bounding box。

区域标记：

1. DesktopOverlayView 在 Browser 内容 bounds 内捕获拖动。
2. 生成归一化区域和当前 Surface/viewport/navigation 身份。
3. Worker 使用 CDP clip 只截取选择区域。
4. 保存备注、序号、Annotation 和二进制 Artifact。

截图失败必须返回 `browser_screenshot_failed`，禁止退化为整页截图。

### 15.2 Artifact 生命周期

Artifact 授权基于创建时的 Magi `sessionId` 和 canonical message attachment，不依赖：

- Browser Tab 是否仍打开。
- Browser Session 是否 ready。
- Surface、Target 或 Host 是否仍存在。

BrowserAuthority 维护 Annotation 与 Artifact 的不可变关联；canonical session store 维护消息引用。关闭 Tab、清理 Browser 数据和升级 Desktop 不删除已进入消息的 Artifact。

### 15.3 截图

- 页面截图、元素截图、区域截图和全页截图使用当前 Surface 的 CDP Page capture。
- 全页截图明确计算 content size，并允许超出当前 viewport；不得固定 `captureBeyondViewport: false`。
- 输出保存为原始 PNG/JPEG bytes，禁止重复 base64 编码。
- 浏览器截图按钮默认把图片加入消息编辑框，而不是静默下载。
- 用户可以从消息附件菜单显式另存为文件。

### 15.4 消息展示

`browserAnnotationRefs` 写入 canonical user message metadata。消息区显示：

- 标记序号。
- 用户备注。
- 页面 URL 和标题。
- 元素或区域类型。
- 图片预览和打开入口。
- 删除/失效状态。

LLM 读取的标记引用、消息展示和 Artifact API 必须指向同一 canonical 记录。

## 16. 设置、版本与更新

### 16.1 统一发行

正式桌面包包含：

```text
Electron/Chromium Desktop Host
Magi App/RightPane/Overlay Web assets
Rust daemon sidecar
BrowserAutomationWorker
Browser capability manifest
licenses
```

不再包含：

- 独立 CEF Framework 和 Helper。
- 独立 Playwright Chromium。
- 独立 Node Runtime。
- 可下载 Browser Runtime Component。

### 16.2 版本来源

产品版本唯一来源仍为 workspace `Cargo.toml` 的 `[workspace.package] version`。构建生成 Desktop package version。

真实组件版本分别来自锁文件：

- Electron version。
- Chromium version，由 Electron 决定。
- Rust daemon build/version。
- BrowserAutomationWorker version。
- Desktop Control Protocol version。
- CDP compatibility version。

同版本的含义是“来自同一 Git commit、同一签名发行包和同一 capability manifest”，不是强行使用相同 SemVer。

### 16.3 capability manifest

构建生成并签名：

```text
browser-capability-manifest.json
  productVersion
  gitCommit
  electronVersion
  chromiumVersion
  daemonVersion
  automationWorkerVersion
  desktopProtocolVersion
  platform/arch
  file hashes
```

启动时校验实际文件、版本和协议；不允许跨版本半激活。

### 16.4 设置页

只提供：

- Desktop Host、Chromium、daemon、Automation Worker 和协议版本。
- starting、ready、restarting、failed、protocol-incompatible 状态。
- Magi Desktop 更新检查、下载和安装进度。
- 重启浏览器能力。
- 清理当前或全部 Magi Browser Session 数据。
- 诊断信息和可操作错误。

删除 Chromium 安装、独立检查更新、独立激活和卸载按钮。

### 16.5 更新原子性

- macOS 主应用、Chromium Framework/Helper、Rust sidecar 和 Worker 使用同一 Developer ID 签名，随后 notarize 和 staple。
- Windows 主应用、sidecar、安装器和卸载器使用 Authenticode 签名。
- Linux AppImage、deb/rpm 分别执行哈希、签名和包管理策略。
- 先签名最终包，再生成更新元数据和哈希。
- 更新失败保留完整旧版本，不允许只替换某个组件。

### 16.6 构建与发布链

正式发布只有一条 Desktop pipeline：

```text
校验 workspace version 与 lockfiles
  -> 生成 Rust/TypeScript contracts
  -> 构建 Svelte Desktop Renderer assets
  -> 构建 Automation Worker
  -> 构建 Rust daemon sidecar
  -> 构建 Electron Main/preload
  -> 生成 capability manifest 和 SBOM/licenses
  -> electron-builder 组装单一桌面包
  -> 签名/公证
  -> 解包自检文件、版本、哈希和协议
  -> 安装后真实 Desktop smoke test
  -> 生成 GitHub Release 与更新元数据
```

发布规则：

- Git tag、Desktop package version、daemon product version 和 capability manifest `productVersion` 由根版本命令一次生成，禁止手工在多个文件修改。
- Electron/Chromium 版本由唯一 npm lockfile 锁定，不在 Rust 配置、运行时 manifest 或设置页另外维护。
- CI 不下载另一份 Chromium，不生成 Browser Runtime 独立 release asset。
- 自动更新只识别 Desktop 发行版本；Chromium、Worker 和 daemon 不拥有独立更新 channel。
- 发布 job 只接受签名后的最终产物，不允许本地未验证包直接上传覆盖更新元数据。

## 17. Tauri 到 Electron 升级迁移

### 17.1 身份连续性

- 保持产品名称、应用 identifier、更新 channel 和用户数据根目录兼容。
- Electron 首次启动只迁移 Magi 自己的会话、设置、Workspace 和 Artifact 数据。
- 不迁移旧 CEF Cache、Cookie、Profile、Helper 状态和 Runtime 安装记录。
- 旧 Browser Tab 只恢复逻辑 URL、标题、顺序和 Annotation；首次激活创建 auto viewport Surface。

### 17.2 平台验证

必须用已发布 Tauri 稳定版真实升级到 Electron 候选版：

- macOS：完整 `.app` 替换、签名、公证、quarantine 和回滚。
- Windows：NSIS 安装位置、应用 ID、卸载项、快捷方式和更新连续性。
- Linux：AppImage 原位更新；deb/rpm 由包管理器升级。

单个发行包中不得包含 Tauri 与 Electron 两个桌面宿主，也不得保留 CEF 回退。允许使用一次性迁移安装器，但迁移代码不能进入长期运行路径，迁移完成后从仓库和发布流程删除。

## 18. 开发迁移阶段与闸门

迁移只在独立开发分支进行。每阶段通过真实验收后才能进入下一阶段。

### 阶段 0：契约冻结

- Desktop IPC contract。
- Desktop Control Protocol。
- WindowLayoutState/LayoutSnapshot。
- BrowserTabRecord/BrowserSurfaceInstance。
- Surface Binding 和 Lease。
- capability manifest。

### 阶段 1：Electron Desktop POC

- BaseWindow + MagiAppView + RightPaneChromeView。
- BrowserSurfaceView 真实网页。
- DesktopOverlayView 覆盖菜单和 Modal。
- 右栏连续拖动、窗口 resize、DPI、全屏和多显示器。
- 快捷键、剪贴板、中文 IME 和焦点。

POC 只验证架构，不进入正式发布；失败即修正目标架构，不在旧实现上追加兼容补丁。

### 阶段 2：进程与 IPC

- Rust daemon sidecar supervisor。
- utilityProcess Automation Worker。
- 私有 Desktop Control socket/pipe。
- parent death、健康检查、重启和更新冻结。

### 阶段 3：Surface 与状态

- BrowserSurfaceManager。
- 多窗口 SurfaceInstance。
- Primary Surface。
- Tab/Surface 生命周期、用户接管和 Tab 级 Lease。
- BrowserAuthority durable state 收敛。

### 阶段 4：直接 CDP 工具

- Snapshot/ElementRef。
- 输入和导航。
- viewport/emulation。
- 标记和截图。
- Console/Network/Performance/Heap/PWA/Lighthouse。
- Worker 崩溃后重绑同一 Target。

### 阶段 5：设置、隐私和更新

- 统一设置页。
- 非持久 partition 和权限审计。
- mock keychain 和用户 Profile 访问审计。
- 签名、公证、安装器和跨宿主升级。

### 阶段 6：唯一切换与清理

- Electron 成为唯一 Desktop entrypoint。
- 删除旧 Tauri/CEF/Playwright 生产路径。
- 删除独立 Browser Runtime 发布与状态机。
- 运行废码扫描、依赖扫描、构建和真实验收。
- 完成前不合并 main、不发布。

## 19. 强制废弃代码清理

### 19.1 旧新职责替换表

| 旧实现 | 唯一新实现 | 删除闸门 |
| --- | --- | --- |
| Tauri Window/WebView | Electron BaseWindow + MagiAppView | Electron 主窗口验收通过 |
| CEF NSView/Helper | BrowserSurfaceView/WebContentsView | 真实页面、输入、resize 验收通过 |
| DOM 测量 + native resize bridge | WindowLayoutManager | reducer、DPI、多窗口验收通过 |
| browser-host 创建 Playwright Page | BrowserSurfaceManager | Worker 能绑定现有 Surface 且 Target 数稳定 |
| Playwright connectOverCDP 产品路径 | BrowserCdpGateway + direct CDP Worker | 工具矩阵验收通过 |
| Browser Runtime installer/updater | Desktop 原子发行和 UpdateManager | 安装、更新、回滚验收通过 |
| Profile 级 Lease | Tab + Surface Lease | 多窗口和用户接管验收通过 |
| viewport durable/localStorage 同步 | Surface ephemeral viewport | 多窗口不互相 resize 验收通过 |
| Surface 相关截图引用 | canonical Artifact store | 关闭 Tab、重启和升级后可读验收通过 |
| Renderer 双向 store 同步 | DesktopWindowSnapshot + intent reducer | 多 Renderer 崩溃恢复验收通过 |

清理不得提前破坏当前开发分支验证，也不得在新路径验收后继续保留旧实现。每一行替换在同一阶段内完成“新实现验收 -> 删除旧实现 -> 废码扫描”，不设置长期 feature flag。

### 19.2 必须删除的桌面代码

- `apps/desktop/src/native_browser.rs`
- `apps/desktop/src/browser_helper.rs`
- `apps/desktop/src/cef_policy.rs`
- Tauri Desktop main、capabilities、command ACL 和原生 Browser commands。
- `cef`、`tauri`、Tauri plugins、objc2 Browser 嵌入依赖。
- CEF external message pump、Native Bridge、NSView Overlay 和 Helper Bundle。

### 19.3 必须删除的 Runtime 与发布代码

- `config/native-browser-runtime.json`
- CEF fetch/stage/package/sign/self-test 脚本。
- `.github/workflows/browser-runtime-release.yml`
- release workflow 中 Browser Runtime 独立归档、manifest 和上传步骤。
- Browser Runtime 下载、签名校验、安装、激活、更新、卸载 manager。
- `NotInstalled/Downloading/Verifying/Installed/UpdateAvailable/UpdateRequired` 等独立组件状态。
- 设置页独立安装、检查、刷新、更新和卸载动作及国际化文案。

### 19.4 必须删除的 Web 兼容代码

- `web/src/lib/native-browser.ts`
- `native_browser_create/resize/focus/navigate/close` bridge。
- `getBoundingClientRect()` BrowserSurface 定位。
- `ResizeObserver -> requestAnimationFrame -> native resize` 队列。
- `1 x 1` 隐藏和旧 frame key/generation 逻辑。
- CEF 页面状态、原生光标和原生 Annotation 事件兼容层。
- Browser Runtime 安装状态驱动的 UI 分支。

### 19.5 必须删除的自动化双实现

- 生产 `chromium.connectOverCDP()` Electron 路径。
- Browser Host `newPage()`、Page 创建和物理页面淘汰职责。
- Playwright 生产依赖、运行组件和协议分支。
- popup “先创建再关闭”处理。
- 独立 Chromium fallback 和 headful/headless 产品切换。

Playwright E2E 依赖只能存在于测试 package/devDependencies，不能被打入生产 capability manifest。

### 19.6 必须清理的数据和迁移代码

- 旧 CEF 临时目录、Cache 和 Runtime active 指针。
- 旧独立 Browser Runtime 安装目录。
- 已失效的 right-pane Browser localStorage payload。
- viewport、activeTab、ProfileControlMode 和 Lease 的旧 durable 字段迁移器。
- 一次性迁移完成后删除迁移代码、feature flag 和版本判断。

### 19.7 禁止残留

最终仓库不得存在：

- `cfg`、环境变量或 feature flag 选择 Tauri/CEF 旧实现。
- “Electron 失败时回退 CEF/Playwright Chromium”的代码。
- Desktop 与 daemon 各自拥有 Browser 页面或 Host supervisor。
- 两套 viewport、Annotation、工具目录或 Browser capability 状态机。
- 注释掉的旧实现、未引用模块、死路由、失效文档和废弃测试快照。

CI 增加废码门禁，扫描旧模块、旧命令、CEF/Tauri 依赖、独立 Runtime 状态和生产 Playwright 依赖；命中即失败。

门禁脚本必须作为正式 CI job 运行，至少检查：

- 被禁文件、目录和 workflow 不存在。
- Cargo metadata 不含 Tauri、CEF、objc2 Browser 嵌入依赖。
- 生产 npm dependency tree 不含 Playwright Chromium、Puppeteer Chromium 或旧 browser-host。
- 发行包解包后不含 CEF、独立 Node、独立 Browser Runtime 和 remote-debugging 启动参数。
- 仓库不存在 `native_browser_*`、`BrowserProfileControlMode`、`connectOverCDP`、`NotInstalled` 等旧产品符号。
- 静态分析、TypeScript/Rust 未使用代码检查和 dependency audit 通过。

## 20. 验收矩阵

### 20.1 架构验收

- Electron Main 是 WebContents、布局和子进程唯一所有者。
- RightPaneChromeView 是多功能右栏的唯一通用内容壳；BrowserSurfaceView 不能成为右栏父容器，也不能阻断代码、图片、终端、Agent 或未来面板。
- 新增浏览器的异步创建只占用该浏览器项的 pending 状态；新增菜单、右栏拖拽、已有面板切换和非浏览器面板创建不等待浏览器网络或 Worker 就绪。
- BrowserAuthority 只持久化逻辑 Browser 事实。
- Surface 运行态按 `desktopEpoch/windowId/surfaceId` 隔离。
- Automation Worker 不创建页面，不获得全局 CDP 或文件权限。
- 普通 Web 客户端无法提交 Desktop layout/viewport。
- 仓库不存在双实现和旧回退路径。

### 20.2 真实桌面验收

1. 启动 Magi，右侧展开按钮始终可见。
2. 新建 Browser Tab，打开百度并读取页面标题。
3. 打开 Bing，搜索 GitHub 中的 `magi-code` 项目，Agent 完成输入、搜索、打开和分析。
4. 用户快捷键、复制粘贴、右键、滚轮、拖动、文本选择和中文 IME 正常。
5. `target="_blank"` 和 `window.open()` 全程不产生第二个 Target。
6. 页面刷新、跳转、慢请求和导航失败期间无黑屏、闪烁、重建或投影帧。
7. 连续拖动右栏到最小值和窗口三分之二，页面不覆盖 Tab、工具栏或中间区。
8. 浏览器正在创建或导航时，打开代码、图片、终端和 Agent Tab，面板内容均可用；浏览器失败只显示该 Tab 的错误。
9. 菜单、标记编辑器、图片预览和设置 Modal 始终位于页面上方并可点击。
10. 两个桌面窗口打开同一逻辑 Tab，尺寸、viewport、焦点和光标互不影响。
11. 用户在 Secondary Surface 操作后正确提升 Primary，Agent 不再操作旧 Surface。
12. auto、宽屏、窄屏和自定义 viewport 动态生效，不刷新、不裁切。
13. Agent Lease 期间虚拟鼠标持续显示；用户输入立即释放 Lease。
14. 任务完成后 Tab、当前 URL、滚动、表单和 SPA 状态保留。
15. 杀死 Automation Worker 后页面仍可见可操作；重启后绑定原 Target。
16. 单个 Browser Renderer 崩溃只影响对应 Surface，其他 Tab 正常。
17. 元素和区域标记只生成选择范围截图，序号和备注正确。
18. 关闭 Tab、重启 Desktop 和升级后，消息中的标记图片仍能打开。
19. 页面、元素、区域和全页截图尺寸、编码和消息附件正确。
20. Browser Session 数据不进入系统钥匙串，不读取外部浏览器 Profile。
21. 设置页版本、状态、更新、重启能力和清理数据真实有效。

### 20.3 自动测试

- Desktop IPC schema、权限和未知字段拒绝测试。
- WindowLayout reducer 和 layout revision 竞态测试。
- Surface Registry、多窗口、Primary 转移和崩溃恢复测试。
- CDP method allowlist 和错误规范化测试。
- 每 Surface 串行队列、取消和 fence 测试。
- Popup 创建前阻断测试，CDP Target 数始终不增加。
- viewport scale、输入坐标和截图坐标测试。
- Annotation/Artifact 关闭 Tab 后访问测试。
- daemon/Worker parent death 和孤儿清理测试。
- 打包解包、manifest、哈希、签名和公证测试。
- Tauri 已发布版本到 Electron 候选版本升级测试。
- macOS、Windows 和 Linux 核心流程 E2E。
- `git diff --check`、前端 check、Rust workspace check/test 和废码扫描。

### 20.4 产品性能和稳定性预算

- 连续拖动右栏 30 秒时，Browser Surface 不导航、不重建、不改 viewport mode，且桌面组合帧率 p95 不低于 55 FPS。
- 新增浏览器 Tab 的点击反馈不等待页面首屏或 Worker；逻辑 Tab/占位状态应先呈现，Surface 物化和导航在后台完成。
- 浏览器 Surface 创建期间，右栏新增菜单、已有 Tab 切换和拖拽操作的输入延迟不因浏览器网络请求增长。
- 用户点击、输入、滚动和 IME 直达 Browser WebContents，不经 daemon、Worker 或坐标转换层。
- Browser Tab 切换不创建新 Target；切回时保留当前 DOM、滚动、表单和 SPA 状态。
- 页面导航和刷新期间不 detach View，不展示黑色截图或空白投影层。
- Snapshot 和工具指标记录 queue wait、CDP time、DOM/AX parse time、result size 和 UI present time，用真实站点基线防止性能回归。
- 工具端到端测试使用百度、Bing、GitHub 和本地响应式测试站，不只使用静态 mock 页面。
- 连续 8 小时 Browser soak test 中不允许 Surface 数、CDP session、Renderer 进程、临时 Artifact 和 Lease 无界增长。

## 21. 完成定义

只有以下条件全部满足，统一 Chromium 改造才算完成：

- Electron Desktop Host 是唯一桌面入口。
- 用户和 Agent 操作同一个 BrowserSurface。
- 所有真实桌面验收和自动测试通过。
- macOS、Windows、Linux 打包和核心流程通过。
- 旧 Tauri 稳定版升级到 Electron 版本通过。
- CEF、Native Bridge、独立 Browser Runtime 和生产 Playwright 路径全部删除。
- 旧配置、路由、状态、脚本、工作流、依赖、文档和测试快照全部清理。
- CI 废码门禁证明不存在双实现、兼容回退和未引用旧代码。
- 正式包不开放调试端口，不访问用户浏览器资料或系统钥匙串。

在上述条件未满足前，不得合并 main、提交发布版本或宣称浏览器产品化完成。
