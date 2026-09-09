# Magi 统一 Chromium 与多功能右栏架构

> 状态：当前产品唯一架构基线
>
> 更新日期：2026-09-06

本文定义 Magi Desktop 内置浏览器、右侧多功能面板、Rust daemon、浏览器自动化 Worker 和 Web/手机 Web 降级行为。实现必须收敛到本文件描述的单一路径，不保留旧坐标映射、截图投影或双轨兼容实现。

## 1. 产品边界

Magi Desktop 使用 Electron `BaseWindow` 作为唯一桌面宿主，Svelte 应用在一个可信 App Renderer 中负责整个工作台。Rust daemon 负责会话、BrowserAuthority、工具治理、持久化和消息引用；Electron Main 负责真实 Chromium guest 的生命周期、安全策略、下载、权限、popup、CDP 和崩溃恢复。

Web 和手机 Web 不伪造可接管的浏览器。它们可以展示 URL、只读记录、截图和明确的外部打开入口，但不能声称拥有 Desktop 内置浏览器或把普通网页 iframe 当成等价实现。

## 2. 右栏是唯一公共壳

右栏不是浏览器容器，而是通用多功能面板。`RightPane.svelte` 唯一负责：

- 顶级 Tab 集合、活动 Tab、关闭和新增菜单；
- 右栏工具栏、折叠状态、宽度拖动和通用浮层；
- 内容区域的裁剪、层级、主题和焦点边界。

当前顶级内容类型为：

| 类型 | 内容 |
| --- | --- |
| `code` | 源码、Diff、Markdown、图片和文件状态视图 |
| `terminal` | xterm 终端、输出、连接状态和终端生命周期 |
| `agent` | Agent 任务状态、工具输出和详情 |
| `browser` | 地址栏、导航、网页交互、视口、截图、标记和 DOM 选择 |

图片、Markdown、Diff 是代码视图的内容模式，不因浏览器改造拆成新的右栏外壳。未来新增类型必须只实现自己的内容组件，不能复制右栏壳或改变公共布局。

### 2.1 唯一的内容树

```text
Electron BaseWindow
└── App Renderer
    └── WebWorkbenchShell
        ├── 左侧工作区/会话栏
        ├── 中间对话区
        └── RightPane
            ├── 一个公共 Tab 栏与工具栏
            └── 当前活动 Tab 的内容槽
                ├── code: 文件内容
                ├── terminal: 终端
                ├── agent: Agent 详情
                └── browser: BrowserTabContent + 一个 <webview>
```

浏览器只能出现在当前 `browser` Tab 的内容槽中，不能覆盖右栏 Tab 栏、工具栏、弹出菜单或其他内容类型。右栏宽度和拖动只由现有 DOM/CSS 布局负责，浏览器实现不得读取或计算窗口坐标。

## 3. 浏览器显示架构

### 3.1 唯一显示路径

Desktop App Renderer 的 `BrowserTabContent.svelte` 在浏览器内容槽直接渲染一个 Electron `<webview>`：

- `<webview>` 使用 `width: 100%`、`height: 100%`、`min-width: 0`、`min-height: 0`，由父级 flex/grid 自然布局；
- Main 不创建用于显示网页的 `WebContentsView`，不调用网页页面的 `setBounds()`，不接收 `getBoundingClientRect()` 坐标；
- `<webview>` guest 是真实 Chromium 页面，用户输入和 Agent CDP 输入到达同一个 `WebContents`；
- 右栏菜单、标记选择、备注、标记历史、Tooltip、视口菜单和截图操作全部属于 App Renderer DOM；
- 非活动 Browser Tab 保持自己的 guest 和页面状态，但不进入命中区域、不持有 App 焦点，切换只改变 DOM 路由；
- Browser Tab 内禁止子 Tab。`target=_blank`、`window.open` 和网页 popup 在 Main 的 `setWindowOpenHandler` 中复用当前顶级 Tab 导航，不能创建第二个 Electron guest、BrowserWindow 或 Magi 子 Tab。

这不是截图、canvas、iframe 投影或原生坐标覆盖方案。旧 `WebContentsView` 浏览器显示路径、浏览器几何 IPC、内容槽租约和原生浮层必须删除。

### 3.2 Main 与 Renderer 的职责

Renderer 只提交浏览器身份和 guest 注册信息：`windowId`、`tabId`、`browserSessionId`、`navigationRevision`、`webContentsId`。Main 校验 guest 类型、宿主 App Renderer、partition 和导航代次后，绑定对应的逻辑 Surface。

Main 保留这些职责：

- guest 创建入口的安全配置和 partition；
- URL allow-list、导航拦截、下载、权限、嵌套 WebView 拒绝和 popup 处理；
- `webContents.debugger` 的受控 CDP lane、Target 生命周期和工具命令；
- 页面加载、失败、崩溃、标题、下载、控制台和 agent cursor 事件；
- Worker/daemon 重连后的物理页面恢复。

Main 不负责：

- 右栏宽度、内容槽矩形、DOM 坐标和浏览器页面的原生 bounds；
- Tab 栏、工具栏、标记编辑器、菜单和提示的 UI；
- 通过截图模拟页面显示或创建浏览器专用窗口。

## 4. 状态所有权与持久化

| 状态 | 唯一所有者 | 持久化 |
| --- | --- | --- |
| Browser Tab 身份、URL、标题、会话归属 | daemon BrowserAuthority | 是 |
| guest WebContents、CDP、下载、权限 | Electron Main | 否，运行时恢复 |
| 右栏 Tab、活动项、宽度和折叠 | App Renderer/窗口布局 | 按现有窗口规则 |
| 视口模式和尺寸 | 当前 Browser Tab 实例 | 不跨 Tab、窗口或会话同步 |
| 标记、截图、DOM 节点快照、消息引用 | Artifact/消息存储 | 是 |
| Agent 占用、虚拟鼠标和任务状态 | BrowserAuthority + Desktop 投影 | 随任务生命周期 |

重启只恢复逻辑 Tab 的 URL 和资料，然后在当前内容槽重新注册 guest。它不把旧窗口尺寸、DOM 几何或截图投影写回新窗口。不同窗口可以显示相同 URL，但拥有独立页面实例、视口和焦点。

## 5. 导航、控制与恢复

所有跨进程请求和事件必须携带 `desktopEpoch`、`windowId`、`browserSessionId`、`tabId`、`surfaceId` 和 `navigationRevision` 中适用的作用域。迟到事件若不匹配当前身份，必须丢弃；关闭的 Tab 不得被迟到事件复活。

### 5.1 导航状态

页面连接状态、页面加载状态和 BrowserAuthority 状态是三个独立状态：

- guest 已注册后立即显示真实页面容器，不因网络请求长期显示“正在连接浏览器”；
- 导航开始显示加载态，保持 Chromium compositor 的正常响应，不销毁 guest、不替换成黑屏层；
- `did-fail-load`、崩溃、超时和取消都结束当前等待并返回明确错误阶段；
- 页面刷新或跳转不能抢回对话输入焦点，也不能触发右栏整体刷新；
- 导航后旧 DOM node id、frame id 和 CDP session 必须失效。

### 5.2 LLM 工具

工具通过统一 BrowserAuthority -> Desktop Control -> Electron Main -> guest CDP 链路执行：

- 导航、前进、后退、刷新、停止；
- 点击、悬停、输入、滚动和键盘事件；
- 页面标题、DOM、AX tree、节点详情、截图和选区截图；
- 标记创建/编辑/删除/编号、DOM 节点选择和消息引用；
- 下载、控制台、网络状态、Agent 虚拟鼠标和任务占用状态。

每种操作等待自己的条件。DOM 读取等待文档可用，导航等待主文档提交或失败，点击等待目标可操作，不能让每个命令无条件等待所有网络资源完成。资源级 CDP lane 保证同一 guest 的命令顺序、取消、超时和重连恢复。

### 5.3 标记与 DOM 选择

标记选择层、备注编辑器和标记历史是当前 Browser Tab 内容槽内的 Renderer DOM 浮层；它们不得占用右栏外框空间。标记区域使用当前内容槽本地坐标，提交后保存为独立 Artifact 和消息引用。DOM 选择保存完整节点快照，包括 URL、标题、节点标识、属性、文本、HTML、边界和导航代次。

标记和 DOM 选择可以同时存在。移动鼠标、切换工具或结束 Inspect 不得清除已经保存的选择；导航或切换 Browser Tab 时才按导航代次使活动选择失效。截图工具必须写入消息编辑框或明确的 Artifact 引用，而不是只下载到本地。

## 6. 视口策略

`auto` 是默认模式：不调用 `Emulation.setDeviceMetricsOverride`，真实 `<webview>` DOM 尺寸就是页面 viewport，Chromium 自己触发 resize、media query 和响应式布局。

固定模式只属于当前 Browser Tab，使用 Chromium CDP `Emulation.setDeviceMetricsOverride`。产品提供两种快捷设备模式：手机窄屏、平板/电脑宽屏；自定义宽高直接作用于当前 Tab。输入过程使用轻量 debounce，提交最新值，不刷新页面、不改右栏 bounds、不在宿主层缩放截图。切回 `auto` 必须调用 `Emulation.clearDeviceMetricsOverride`。

## 7. 焦点、层级与主题

- App Renderer 的对话输入、右栏工具栏和网页输入框分别拥有清晰焦点边界；浏览器导航完成不能调用 `focus()` 抢走对话输入。
- 网页内容槽是普通 DOM 子节点；菜单和 Tooltip 是同一 Renderer 中更高的 DOM stacking context，不能被 guest 视图遮住。
- App Renderer 绘制左栏、中栏和右栏的主题材质及壁纸；浏览器只消费内容槽，不创建独立背景或模糊层。
- Agent 虚拟鼠标是页面隔离世界中 `pointer-events: none` 的装饰层，document 创建后重注入；任务结束时隐藏，但保留任务占用状态历史。
- Chromium 启动参数禁止默认同步、组件网络更新、首跑提示和无关用户数据采集。浏览器 partition 仅使用 Magi 自己的状态目录，不读取用户系统浏览器资料。

## 8. Rust/TypeScript 协议

`contracts/desktop-browser` 是 Desktop IPC 的唯一 Schema 来源。请求、响应、通知和 server request 使用明确的消息类型；协议初始化必须经过 `initialize/initialized` 能力协商。Rust、TypeScript 和 Worker 类型由同一份 Schema 生成或校验，不在各端私自增加旁路事件。

Desktop Renderer IPC 只保留窗口布局、App Renderer、浏览器 guest 注册、浏览器工具控制、外观、文件、更新和菜单所需频道。右栏 DOM 浮层不需要 Overlay IPC；不存在 `open-overlay`、`overlay-action` 或阻塞原生 Overlay 频道。

## 9. Web 与手机 Web 降级

普通 Web/手机 Web 不创建 Electron `<webview>`，也不暴露 Desktop 控制 API。浏览器 Tab 显示 URL、记录和 Artifact，并提供清晰的“在系统浏览器打开”操作。浏览器工具目录必须将 Desktop 专属能力标记为不可用，不能静默失败、伪造接管成功或发起无权限的系统操作。

## 10. 迁移与清理门槛

迁移完成后源码中不得出现以下旧链路：

- 浏览器 `WebContentsView`、`BrowserWindow` 或原生 View 的页面 `setBounds()`；
- `renderer_geometry`、`bindContentSurface`、`browserContentSlot` 和浏览器内容槽坐标上报；
- 浏览器截图投影、canvas/iframe 显示和独立 Overlay Renderer；
- Browser Tab 内的第二套子 Tab；
- 只为旧显示实现服务的 IPC、类型、配置、测试和文档。

通用 `WebContentsView` 只能用于承载唯一可信 App Renderer；这不等于浏览器内容显示层。

## 11. 产品级验收

静态检查不是完成条件。必须使用同一提交构建并运行真实 Electron Desktop，按以下顺序验收：

1. 启动、切换浅色/深色主题、无工作区和有工作区，确认无钥匙串提示、无用户资料读取和无第二个 Tauri 客户端。
2. 新建两个 Browser Tab，打开百度/Bing/GitHub，读取标题、搜索、点击、输入、滚动、刷新、前进和后退。
3. 拖动右栏、缩放窗口、最大化和恢复，确认网页始终完整填充内容槽，工具栏和菜单不被遮挡，无黑屏、跳动、溢出或重复刷新。
4. 在浏览器、代码、Markdown、Diff、图片、终端和 Agent 之间反复切换，确认每种视图互不覆盖，终端进程和 Agent 任务不中断。
5. 使用标记和 DOM 选择同时操作，移动鼠标、打开菜单、截图并发送，确认序号、内容、图片和节点资料都进入消息记录，重启后仍可查看。
6. 点击会打开新窗口的链接，确认仍复用当前 Browser Tab，不出现子 Tab 或额外桌面窗口。
7. 切换 `auto`、手机窄屏、宽屏和自定义视口，确认页面由 Chromium 自己响应式布局，不被宿主截断、不因调整而刷新。
8. 运行 LLM 浏览器任务，确认虚拟鼠标、工具调用、标题读取、DOM 选择、截图、取消、超时、完成后保留当前 Tab。
9. 重启 daemon、Worker 和 Desktop，确认逻辑 URL、标记、消息引用和多会话归属恢复，迟到事件不会污染其他 Tab。
10. 检查控制台和日志，不得有 `setBounds` 浏览器调用、Overlay IPC、黑屏、focus 串位、Surface stale 误报或无限连接等待。

前置命令：

```bash
npm run check
npm test
cargo check --workspace
git diff --check
```

只有真实桌面验收、上述命令和发布前检查全部通过，才允许提交 `main`、打包或发布。未通过时必须继续修复根因，不能以“多数场景可用”结案。
