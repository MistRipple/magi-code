# Magi 统一 Chromium 与多功能右栏架构

> 状态：当前产品唯一架构基线
>
> 更新日期：2026-09-06

本文定义 Magi Desktop 内置浏览器、右侧多功能面板、Rust daemon、浏览器自动化 Worker 和 Web/手机 Web 降级行为。实现必须收敛到本文件描述的单一路径，不保留旧坐标映射、截图投影或双轨兼容实现。

## 1. 产品边界

Magi Desktop 使用 Electron `BrowserWindow` 作为唯一桌面宿主，Svelte 应用在其直属可信 App Renderer 中负责整个工作台。Rust daemon 负责会话、BrowserAuthority、工具治理、持久化和消息引用；Electron Main 负责真实 Chromium guest 的生命周期、安全策略、下载、权限、popup、CDP 和崩溃恢复。

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
Electron BrowserWindow
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

浏览器只能出现在当前 `browser` Tab 的内容槽中，不能覆盖右栏 Tab 栏、工具栏、弹出菜单或其他内容类型。右栏宽度和拖动只由现有 DOM/CSS 布局负责，浏览器实现不得读取或计算窗口坐标。Renderer 只允许把当前内容槽的瞬时宽高作为 fixed 设备画布的 Chromium 显示比例输入；该宽高不是窗口坐标，不得用于设置原生 bounds 或持久化布局。

## 3. 浏览器显示架构

### 3.1 唯一显示路径

Desktop App Renderer 的 `BrowserTabContent.svelte` 在浏览器内容槽直接渲染一个 Electron `<webview>`：

- `<webview>` 使用 `width: 100%`、`height: 100%`、`min-width: 0`、`min-height: 0`，由父级 flex/grid 自然布局；
- Main 不创建用于显示网页的 `WebContentsView`，不调用网页页面的 `setBounds()`，不接收 `getBoundingClientRect()` 坐标；Renderer 只上报经严格校验的内容槽宽高；
- `<webview>` guest 是真实 Chromium 页面，用户输入和 Agent CDP 输入到达同一个 `WebContents`；
- 右栏菜单、标记选择、备注、标记历史、Tooltip、视口菜单和截图操作全部属于 App Renderer DOM；
- 非活动 Browser Tab 保持自己的 guest 和页面状态，但不进入命中区域、不持有 App 焦点，切换只改变 DOM 路由；
- Browser Tab 内禁止子 Tab。`target=_blank`、`window.open` 和网页 popup 统一进入 Main 的单一决策链；只对可证明不依赖独立窗口的请求复用当前顶级 Tab，其余请求明确阻止，绝不创建第二个 Electron guest、BrowserWindow 或 Magi 子 Tab。

这不是截图、canvas、iframe 投影或原生坐标覆盖方案。旧 `WebContentsView` 浏览器显示路径、浏览器几何 IPC、内容槽租约和原生浮层必须删除。

### 3.2 Main 与 Renderer 的职责

Browser guest 使用两阶段安全边界：

1. 可信 App Renderer 的 `will-attach-webview` 在 guest 附加前只允许 `about:blank` 初始页和 `magi-browser-*` 隔离 partition，删除 preload 和 Renderer 自带的 `webpreferences` 字符串，并强制 `nodeIntegration=false`、`contextIsolation=true`、`sandbox=true`、`webSecurity=true`、`webviewTag=false`。不符合入口规则的 guest 直接销毁，不进入注册或页面导航。
2. guest 创建后，Renderer 只提交浏览器身份、注册信息和瞬时显示尺寸：`windowId`、`tabId`、`browserSessionId`、`navigationRevision`、`webContentsId`、`displaySize`。Main 再校验 guest 类型、宿主 App Renderer、精确 partition、逻辑 session、导航代次和显示尺寸，成功后才绑定对应的逻辑 Surface 并开放受控 CDP。后续 `ResizeObserver` 只在同一物理 guest 身份下更新 `displaySize`，不产生导航、页面刷新或布局写回。

Browser guest 不继承可信 App Renderer 的 preload 或桌面控制桥。注册失败、宿主销毁或身份过期时，Main 不把该 guest 暴露给 BrowserAuthority/Worker；宿主 DOM 生命周期负责销毁未绑定或已释放的 `<webview>`。

Main 保留这些职责：

- guest 创建入口的安全配置和 partition；
- URL allow-list、导航拦截、下载、权限、嵌套 WebView 拒绝和 popup 处理；
- `webContents.debugger` 的受控 CDP lane、Target 生命周期和工具命令；
- 当前 Surface 的 fixed 设备指标与基于 `displaySize` 的 Chromium 显示比例；
- 页面加载、失败、崩溃、标题、下载、控制台和 agent cursor 事件；
- Worker/daemon 重连后的物理页面恢复。

Main 不负责：

- 右栏宽度、内容槽位置、DOM 坐标和浏览器页面的原生 bounds；
- Tab 栏、工具栏、标记编辑器、菜单和提示的 UI；
- 通过截图模拟页面显示或创建浏览器专用窗口。

### 3.3 Popup 单页支持矩阵

Electron `HandlerDetails` 只提供 `url`、`frameName`、`features`、`disposition`、`referrer` 和 `_blank` 表单专用的 `postBody`，不能证明页面未来是否会使用 `window.opener`。Magi 不做 OAuth URL 猜测，而是只依据这些原生事实执行下表：

| 请求类型 | 唯一行为 | 结果事实 |
| --- | --- | --- |
| 普通 HTTP(S) `_blank` 链接 | 当前一级 Browser Tab 导航 | Surface、Target、WebContents 不变，导航代次前进 |
| 带 `postBody` 的 `_blank` 表单 | 当前页按原 body、Content-Type 和 referrer 加载 | 禁止退化为 GET |
| 显式 `noopener` 或 `noreferrer` 的脚本窗口 | 当前页导航 | 不承诺独立窗口句柄 |
| 空 `frameName` 且未断开 opener 的脚本窗口 | 阻止，`opener_required` | 原页面和导航代次保持 |
| 非 `_blank` 命名窗口，包括典型命名 OAuth popup | 阻止，`named_window` | 不伪造跨窗口通信 |
| 包含 `popup`、尺寸或位置等独立窗口特性 | 阻止，`separate_window_features` | 不创建窗口 |
| `about:blank` 脚本窗口 | 阻止，`script_blank_window` | 不允许页面自行创建第二文档容器 |
| 非 HTTP(S) 协议 | 阻止，`unsupported_protocol` | 提示改用外部浏览器 |
| 无法解析或被导航安全规则拒绝的 URL | 阻止，`invalid_url` | 不进入导航链 |

阻止原因是 Desktop Browser 协议 3.5 的必填字段：同一事件一方面进入 daemon 的 `browser.popup.blocked`，供任务和工具结果读取；另一方面由当前 Browser Tab 的 Renderer Top Layer 错误条展示。缺少 reason 的事件必须在协议边界拒绝，不能显示模糊的“无响应”。

## 4. 状态所有权与持久化

| 状态 | 唯一所有者 | 持久化 |
| --- | --- | --- |
| Browser Tab 身份、URL、标题、会话归属 | daemon BrowserAuthority | 是 |
| guest WebContents、CDP、下载、权限 | Electron Main | 否，运行时恢复 |
| 内容槽瞬时宽高、fixed 显示比例 | App Renderer 测量 / Electron Main 计算 | 否，仅当前 Surface |
| 右栏 Tab、活动项、宽度和折叠 | App Renderer/窗口布局 | 按现有窗口规则 |
| 视口模式和尺寸 | 当前 Browser Tab 实例 | 不跨 Tab、窗口或会话同步 |
| 标记、截图、DOM 节点快照、消息引用 | Artifact/消息存储 | 是 |
| Agent 占用、虚拟鼠标和任务状态 | BrowserAuthority + Desktop 投影 | 随任务生命周期 |

Browser 工具、任务和消息最终都进入同一 Session/Turn/Item 主链，因此会话持久化必须先满足以下基础契约：

- workspace identity 固定在项目本地 `.magi/workspace-identity.json`。全局 `workspaces.json` 只保存该 identity 到本机路径的注册映射，不得因新状态根、重启或重新选择目录生成第二个 ID。
- workspace session projection 只能位于对应项目的 `.magi/session-projections`；personal projection 只能位于 daemon 状态根。加载时逐文件验证 `workspaceId`、扫描根和规范路径，未知 workspace 不得回落到 personal 目录，重复或错位文件不得自动移动、去重或删除。
- 在当前 daemon 状态根内，canonical event log 是 Turn/Item mutation 权威，projection 是完整检查点。项目首次接入新的状态根且 event 根不存在时，只允许由 identity 匹配的完整 project projection 通过可恢复事务初始化 event segment；游标需要变化时与 projection 更新同事务提交，游标已一致时不重写项目文件。
- workspace 注册事实必须先于引用它的 session projection 落盘。完整 projection checkpoint 与 sidecar checkpoint 都必须在 `canonical_commit_lock` 下捕获同一代 canonical/event 快照，禁止后台 flush 形成跨代状态。

重启只恢复逻辑 Tab 的 URL 和资料，然后在当前内容槽重新注册 guest。它不把旧窗口尺寸、DOM 几何或截图投影写回新窗口。不同窗口可以显示相同 URL，但拥有独立页面实例、视口和焦点。

### 4.1 多窗口 Primary 与关闭

- 一个逻辑 Browser Tab 可以在每个 Desktop 窗口各有一个物理 Surface，但全局只能有一个 Primary。当前窗口活动 Browser Tab 的 guest 注册或重绑时原子提升该 Surface；普通 Secondary 页面事实不得写回 Authority。
- Primary 变更按逻辑 Tab 单调推进 `surfaceRevision`。Authority 在接受新 binding 的同一事务中撤销旧 Surface lease 并推进 fence；旧命令只能以 stale/cancelled 收口，禁止自动重放到新 Surface。
- Surface 失去 Primary 时立即清除本地 Agent 控制态、Inspect 和虚拟鼠标；用户在 Secondary 页面真实输入时先提升该 Surface，再按用户接管语义撤销控制。
- 关闭一个窗口只释放该窗口的 Surface。若同一逻辑 Tab 仍有其他 Surface，提升一个替代 Primary；若不存在替代者，发送 `primary_surface_closed`，Authority 只清除精确匹配的 binding、撤销其 lease，并把逻辑 Tab 转为 Suspended。
- 点击右栏 Tab 的关闭按钮是全局关闭逻辑 Tab：Authority 关闭 Tab，Desktop 释放该 Tab 的全部 Surface；该路径不再发送 per-window Primary 关闭事件。任务结束只释放 lease，保留 Tab、URL 和页面。
- stale epoch、旧 `surfaceRevision`、旧导航代次或已经被替代的关闭事件不得覆盖或清除当前 Primary。

### 4.2 右栏内容生命周期矩阵

| 内容类型 | 切换到其他一级 Tab/会话 | Renderer F5 | 完整应用重启 | 显式关闭 Tab |
| --- | --- | --- | --- | --- |
| Browser | 同一右栏内隐藏并保活 guest；跨会话释放当前窗口 guest，但保留 Authority 的逻辑 Tab/URL | Authority 重新投影逻辑 Tab，当前窗口注册新 guest；不持久化视口尺寸 | daemon 按逻辑 URL 恢复，新建 Surface/Target；不承诺旧 DOM、表单和内存历史 | 全局关闭逻辑 Tab，释放全部窗口 Surface/Target 和监听器 |
| Terminal | 销毁当前 xterm/WebSocket，Rust PTY 继续运行并缓存最近 2 MiB 输出；切回同 ID 重连 | Desktop 当前 BrowserWindow 的 `sessionStorage` 恢复 Terminal Tab 身份，重连同一 PTY并先重放输出、后发布生命周期 | 不恢复 Terminal Tab；daemon 关闭时终止 PTY | 先从 UI 移除，再调用 DELETE 终止精确 TerminalBinding；会话关闭终止其全部 PTY |
| Code/Image | 当前视图卸载，保留轻量路径/类型元数据；大文本、diff 和图片 data URL 不写存储 | 从当前窗口 `sessionStorage` 恢复元数据并按权威文件/Artifact 重取 | 不恢复 Desktop 窗口级 Tab；Web 端仍按原 localStorage 规则 | 释放当前视图缓存，不影响其他内容类型 |
| Agent | 当前视图卸载，任务继续由 canonical task/turn/item 事实驱动 | 从当前窗口 `sessionStorage` 恢复 `agentRunId` 并重建投影 | 不恢复 Desktop 窗口级 Tab；任务事实仍属于会话 | 只关闭观察视图，不取消或终止任务 |

Desktop 使用每个 BrowserWindow 独立的 `sessionStorage`，只解决同一窗口 Renderer 重载；Web 使用 `localStorage`。两端都不保存 Browser 实体，避免与 BrowserAuthority 形成双事实源。正常窗口关闭必须经过 Electron `before-quit` 事务；强制进程终止属于崩溃边界，不得伪装成正常关闭完成。

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

工具通过唯一链路执行：`BrowserAuthority -> BrowserHostClient -> Desktop Control Server -> Automation Worker -> Main cdp_request -> BrowserSurfaceManager -> guest webContents.debugger`。Main 直接负责 create/restore/close、导航、Stop、fixed viewport、控制权、Inspect 和标记投影等物理生命周期命令；Worker 只负责快照、节点引用、自动化交互、诊断和截图算法，不能在 Main 复制同一套页面算法。

- 导航、前进、后退、刷新、停止；
- 点击、悬停、输入、滚动和键盘事件；
- 页面标题、DOM、AX tree、节点详情、截图和选区截图；
- 标记创建/编辑/删除/编号、DOM 节点选择和消息引用；
- 下载、控制台、网络状态、Agent 虚拟鼠标和任务占用状态。

每种操作等待自己的条件。DOM 读取等待文档可用，导航等待主文档提交或失败，点击等待目标可操作，不能让每个命令无条件等待所有网络资源完成。资源级 CDP lane 保证同一 guest 的命令顺序、取消、超时和重连恢复。

身份与失效边界如下：

- `desktopEpoch` 隔离整个 Desktop Host 生命周期；旧 Desktop 的所有 Surface 事件都被 daemon 丢弃。
- `workerEpoch` 隔离 UtilityProcess；Worker 重启后重新握手并重绑当前 Primary，旧节点引用和 pending result 不得进入新 Worker。
- `surfaceRevision` 隔离同一逻辑 Tab 的 Primary 物理身份；Primary 切换后旧命令不得落到新 Surface。
- `navigationRevision` 隔离文档；document navigation、Renderer/Target 重建使旧节点、截图和写操作失效，同文档 URL/history 只按真实导航事件推进，不等同于整个 CDP session 销毁。
- lease fence 隔离写权限；Primary、导航、用户接管、取消或任务终态推进 fence，旧写操作不能在恢复后重放。

取消在底层命令尚未开始时返回 cancelled；底层写操作已开始且 Electron CDP 无取消句柄时，废弃当前 debugger session，迟到结果只能落到旧 lease/epoch，调用结果不得伪装为成功。超时同样失效当前 debugger session；读操作可以由调用方在取得新 binding 后显式重试，输入、提交、上传等写操作不能自动重放。

### 5.4 恢复矩阵

| 故障 | 唯一恢复行为 |
| --- | --- |
| daemon 重启 | ProcessSupervisor 拉起新 runtimeEpoch；持久化重建逻辑 Session/Tab，Desktop 重新注册当前 Primary；旧 lease 全部失效，物理 guest 保留。 |
| Worker 重启 | 新 workerEpoch 完成 initialize/rebind 后才放行命令；物理 guest、URL 和 Surface 保留，旧节点引用、pending call 和 CDP child session 失效。 |
| guest 崩溃 | 当前命令按物理 lifecycle epoch 失败；旧 debugger/Overlay/viewport lane 清理，逻辑 Tab 保留为可恢复状态，新 guest 注册后恢复 URL 和 fixed viewport，不恢复未持久化 DOM 内存。 |
| App Renderer 重载 | 旧 `<webview>` 由 DOM 生命周期释放；同一逻辑 Surface 等待新 guest 注册，旧 WebContents 事件由 lifecycle epoch 丢弃，URL、标记和当前运行期 viewport 重放。 |
| Desktop 重启 | 新 desktopEpoch；只恢复 BrowserAuthority 持久化的 Tab URL、标题和标记，重新创建物理 Surface。视口、焦点、页面历史、表单和 DOM 内存不跨重启。 |

### 5.3 标记与 DOM 选择

标记选择层、备注编辑器和标记历史是当前 Browser Tab 内容槽内的 Renderer DOM 浮层；它们不得占用右栏外框空间。坐标只允许按以下单向关系转换：

- 内容槽局部坐标属于 Renderer。`auto` 下内容画布等于整个槽；fixed 下内容画布从槽左上角开始，尺寸为 `逻辑 viewport × Main 返回的 Chromium scale`，剩余区域是不可选择留白。
- 页面 viewport CSS 坐标属于当前 Chromium 文档。Renderer 只把内容画布内位置归一化到 `0..1`；Worker 使用同一页面的 `innerWidth/innerHeight` 反算 CSS 坐标。
- 文档坐标等于 viewport CSS 坐标加捕获时的 `scrollX/scrollY`。导航代次变化后，旧坐标、节点和截图请求必须拒绝。
- 图片像素坐标使用真实截图输出宽高换算，天然包含 `devicePixelRatio`；不得用内容槽像素、窗口 DPR 或固定 1000 替代页面报告的 DPR。

标记截图先从同一个 Chromium WebContents 捕获当前完整可见 viewport，再由 daemon 在内存中按归一化区域无损裁剪；不能调用会改变页面滚动的 Chromium clipped capture，也不能缩放或重新渲染网页。显式 `browser_screenshot` clip 使用相同裁剪实现，PNG/JPEG/WebP 重新计算字节数和 SHA-256。标记提交后保存为独立 Artifact 和消息引用。DOM 选择保存 URL、标题、节点标识、属性、文本、受上限约束的 HTML、`outer_html_truncated`、边界和导航代次，不承诺无限完整的页面 HTML。

标记和 DOM 选择可以同时存在。移动鼠标、切换工具或结束 Inspect 不得清除已经保存的选择；导航或切换 Browser Tab 时才按导航代次使活动选择失效。截图命令必须携带当前 `navigationRevision`，选择后立即导航时拒绝旧请求，不能生成新页面截图配旧页面锚点。截图工具必须写入消息编辑框或明确的 Artifact 引用，而不是只下载到本地。

## 6. 视口策略

`auto` 是默认模式：不调用 `Emulation.setDeviceMetricsOverride`，真实 `<webview>` DOM 尺寸就是页面 viewport，Chromium 自己触发 resize、media query 和响应式布局。

固定模式只属于当前 Browser Tab，使用 Chromium CDP `Emulation.setDeviceMetricsOverride`。产品提供两种快捷设备模式：手机窄屏、平板/电脑宽屏；自定义宽高直接作用于当前 Tab。Renderer 用 `ResizeObserver` 读取当前内容槽 CSS 宽高，Main 计算 `scale = min(1, 槽宽 / 逻辑宽, 槽高 / 逻辑高)` 并作为同一次设备指标提交的一部分，使完整设备画布始终容纳在内容槽内。输入和拖动过程合并到每个动画帧的最新尺寸，不刷新页面、不改右栏 bounds、不使用 CSS transform、不在宿主层缩放截图。切回 `auto` 必须调用 `Emulation.clearDeviceMetricsOverride`，内容槽尺寸变化不触发任何设备指标写入。

浏览器页面缩放不另设第二套状态；当前产品只使用设备指标中的 `scale` 完整容纳 fixed 画布，页面 CSS viewport 和 `devicePixelRatio` 仍由 Chromium 返回。任何标记、点击、DOM 命中或截图都必须从当前 Surface 的同一份 viewport/scale/scroll/DPR 事实换算。

## 7. 焦点、层级与主题

- App Renderer 的对话输入、右栏工具栏和网页输入框分别拥有清晰焦点边界；浏览器导航完成不能调用 `focus()` 抢走对话输入。
- 网页内容槽是普通 DOM 子节点；菜单和 Tooltip 使用同一 Renderer 的 Popover Top Layer，并以当前 Tab 唯一 CSS anchor 定位，不能被 guest 视图遮住或越过非活动 Tab 生命周期。
- App Renderer 绘制左栏、中栏和右栏的主题材质及壁纸；浏览器只消费内容槽，不创建独立背景或模糊层。
- Agent 虚拟鼠标是页面隔离世界中 `pointer-events: none` 的装饰层，document 创建后重注入；任务结束时隐藏，但保留任务占用状态历史。
- Chromium 启动参数禁止默认同步、组件网络更新、首跑提示和无关用户数据采集。浏览器 partition 仅使用 Magi 自己的状态目录，不读取用户系统浏览器资料。

## 8. Rust/TypeScript 协议

`contracts/desktop-browser` 是 Desktop IPC 的唯一 Schema 来源。请求、响应、通知和 server request 使用明确的消息类型；协议初始化必须经过 `initialize/initialized` 能力协商。Rust、TypeScript 和 Worker 类型由同一份 Schema 生成或校验，不在各端私自增加旁路事件。

Desktop Renderer IPC 只保留窗口布局、App Renderer、浏览器 guest 注册、当前 guest 的瞬时 `displaySize`、浏览器工具控制、外观、文件、更新和菜单所需频道。`displaySize` 只驱动 Chromium fixed scale，不得携带坐标或反向修改 Renderer 布局。右栏 DOM 浮层不需要 Overlay IPC；不存在 `open-overlay`、`overlay-action` 或阻塞原生 Overlay 频道。

## 9. Web 与手机 Web 降级

普通 Web/手机 Web 不创建 Electron `<webview>`，也不暴露 Desktop 控制 API。浏览器 Tab 显示 URL、记录和 Artifact，并提供清晰的“在系统浏览器打开”操作。浏览器工具目录必须将 Desktop 专属能力标记为不可用，不能静默失败、伪造接管成功或发起无权限的系统操作。

Desktop 身份不能来自 `clientPlatform=desktop`、query 参数或可伪造的 UA。Electron Main 每次启动生成 Desktop Control 随机 token，并只通过 `persist:magi-app` session 的 `webRequest.onBeforeSendHeaders` 注入 daemon HTTP/WebSocket 请求；token 不进入 preload、Renderer JavaScript、URL、日志或 Browser guest partition。daemon 只有在以下条件同时成立时才认定可信 App Renderer：

1. 请求不是公网 tunnel；
2. UA 同时包含 Magi Desktop 与 Electron 标识；
3. Header token 与当前已注册 Desktop connection 的 control token 完全一致。

客户端平台声明只能主动降级，不能把服务端观测到的 Web/Mobile 提升成 Desktop。Browser 设置、资源回收、Session/Tab 创建关闭、激活、导航、截图和标记写入都复用同一服务端检查；Browser 记录、标记历史和 Artifact 读取保持跨端可见。

App Server 的 `initialize/initialized` 只在可信 Desktop 传输且客户端同时声明 `desktopBrowserSurface`、`browserTools` 时返回 `server.capabilities.browserTools=true`；`browser/tools/list` 与 `browser/tool` 执行前再次校验。HTTP Turn 入口把同一认证结果写入不参与 JSON 反序列化/持久化的 server-only 字段，Web/Mobile Turn 将全部 Browser 工具加入拒绝集，不会让远端会话的 LLM 静默借用本机 Host。Web 设置页仍可读取资源状态，但浏览器能力开关和资源回收控件必须禁用。

## 10. 迁移与清理门槛

迁移完成后源码中不得出现以下旧链路：

- 浏览器专用 `WebContentsView`、额外 `BrowserWindow` 或原生 View 的页面 `setBounds()`；
- `renderer_geometry`、`bindContentSurface`、`browserContentSlot` 和浏览器内容槽坐标上报；允许的 `displaySize` 只能包含宽高；
- 浏览器截图投影、canvas/iframe 显示和独立 Overlay Renderer；
- Browser Tab 内的第二套子 Tab；
- 只为旧显示实现服务的 IPC、类型、配置、测试和文档。

唯一 Desktop `BrowserWindow` 直接承载可信 App Renderer；Browser guest 只能来自其右栏内容槽中的 `<webview>`。

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
