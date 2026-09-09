# Magi 内置浏览器与多功能右栏产品级实施计划

> 本文是本项目唯一的执行计划、状态账本和验收入口。实现、测试、打包和交付必须按本文顺序推进。没有对应证据，不得把功能标记为“已完成”；不得因为一次自动化通过或旧包可运行而提前放行。

## 1. 执行规则

### 1.1 状态定义

| 状态 | 含义 |
| --- | --- |
| 未开始 | 尚未进入该功能点。 |
| 进行中 | 正在分析或实现，不代表可交付。 |
| 未验证 | 实现或自动化检查已有结果，但尚未完成同一 Electron 包的真实桌面验收。 |
| 未通过 | 真实场景或回归失败；必须记录新证据、唯一根因和一次结构性修复。 |
| 已完成 | 实现、相关自动化、同一构建身份的真实 Electron 场景和验收记录均齐全。 |

### 1.2 防止重复修改

1. 当前功能点未通过前，不进入下一功能点，不跨层添加兼容修复。
2. 每次失败只追加一条证据链：复现 -> 事件序列 -> 唯一根因 -> 一次修复 -> 回归。
3. 没有新事件证据，不重复修改同一职责边界。
4. 禁止固定等待、盲目轮询、无限重试、双实现、旁路事件和 UI 假状态。
5. 每完成一个功能点，立即更新本文的状态表和更新记录，并写入源码身份、命令结果、Electron 包路径、真实操作证据和下一步。
6. 临时诊断必须在根因确认后删除，并重新执行受影响的回归测试。

## 2. 最终产品目标

### 2.1 Desktop

Magi Desktop 使用同一套 Electron Chromium 运行时，在主 Renderer 的唯一右栏中提供真实可交互的 Browser 一级 Tab。用户看到的是浏览器页面真实渲染，而不是截图、DOM 投影、坐标映射或悬浮窗口遮盖。

右栏必须保持一个公共工作台外壳：

    Electron Window
    └── App Renderer
        └── WebWorkbenchShell
            ├── 左栏
            ├── 中栏对话区
            └── RightPane（唯一右栏外壳）
                ├── 顶级 Tab 栏
                ├── 通用工具栏/新增菜单/浮层层级
                └── 当前 Tab 内容槽
                    ├── Browser：地址栏 + 工具栏 + 唯一 <webview>
                    ├── Code/Diff/Markdown/HTML/Image：文件视图
                    ├── Terminal：终端视图
                    ├── Agent：任务视图
                    └── 后续内容类型

Browser 只是右栏的一个一级内容类型，不拥有右栏外壳、Tab 栏、工具栏、浮层、宽度或焦点。多个 Browser Tab 仍然是右栏顶级 Tab；Browser Tab 内严禁子 Tab、额外窗口和第二套浏览器壳。

### 2.2 Web 与 Mobile Web

Web、手机 Web 不接管本机 Chromium，也不伪造 Desktop 内置浏览器能力。它们可以展示普通网页入口、浏览器状态和明确的能力不可用状态，但不能显示“已连接”或提供实际上无法执行的 Desktop 操作。

### 2.3 明确禁止

- 禁止以 WebContentsView 覆盖右栏内容区。
- 禁止把 getBoundingClientRect、setBounds 或坐标转换作为浏览器显示路径。
- 禁止用截图、Canvas、快照投影替代真实页面显示。
- 禁止将浏览器内容几何、视口、guest 句柄写入普通右栏布局持久化。
- 禁止 Browser Tab 事件修改右栏宽度、折叠状态、其他 Tab 或对话焦点。
- 禁止网页 target=_blank、window.open 创建子 Tab 或额外窗口；统一复用当前一级 Browser Tab。
- 禁止在无真实 guest、无当前 Primary binding 时显示“浏览器已连接”。

## 3. 状态所有权

| 状态 | 唯一负责人 | 持久化规则 |
| --- | --- | --- |
| Browser Session/Tab 身份、URL、标题、生命周期、标记事实 | daemon 的 BrowserAuthority | 持久化；关闭后的迟到事件不得复活 |
| guest WebContents、Chromium 页面、CDP、下载、权限 | Electron Main | 运行时状态；由当前 Renderer webview 注册 |
| 右栏 Tab、宽度、折叠、工具栏、浮层、焦点 | App Renderer 的 RightPane | 使用现有右栏布局规则 |
| Browser Tab 视口模式和设备参数 | 当前 Browser Tab 实例 | 只作用于当前 Tab，不跨 Tab、窗口或会话同步 |
| 标记、选区截图、DOM 节点选择、消息引用 | Artifact/消息持久化链路 | 可恢复，移动鼠标和页面导航不丢失 |
| LLM 占用、控制 Lease、虚拟鼠标 | BrowserAuthority + Desktop 投影 | 随任务获取/释放，任务完成保留页面和 Tab |
| Host/Playwright/Chromium 版本 | 单一 Desktop Runtime manifest | 由统一版本来源生成，不在多个配置点手工漂移 |

## 4. 阶段总账本

当前源码分支：codex/session-turn-latency-state-panel
当前 HEAD：9aecb2cd（工作树包含既有未提交改动，不能将工作树改动误认为该提交内容）
当前真实包：/Users/xie/code/magi-rust-rewrite/target/electron-dist/mac-arm64/Magi.app
当前包身份：Magi 3.0.51 / Electron 43.4.0 / Chromium 150.0.7871.224
Desktop 验收入口：http://127.0.0.1:38123/web.html

| 阶段 | 功能点 | 状态 | 完成条件 |
| --- | --- | --- | --- |
| G0 | 目标、架构边界、右栏功能盘点和验收规则 | 已完成 | 本文约束冻结，Browser 与 Code/Image/Terminal/Agent 的职责边界明确。 |
| G1 | Renderer DOM 内真实 webview 首次创建与导航 | 已完成 | 同一 Electron 包创建一级 Browser Tab，注册真实 guest，导航 example.com 并显示 Example Domain。 |
| G2 | 公共右栏、多功能 Tab、生命周期和焦点 | 已完成 | G2.1-G2.7 同一 Electron 构建和真实桌面验收全部通过。 |
| G3 | 导航、输入、刷新、Popup、下载、单页约束 | 已完成 | G3.1-G3.7 在同一 Electron 构建上通过自动化、Rust 回归和真实桌面验收；当前一级 Browser Tab 内完成全链路，不产生子 Tab/额外窗口。 |
| G4 | S4 阶段门 | 进行中 | G1-G3 具有同一构建的自动化和真实桌面证据。当前执行 G4.1。 |
| G5 | LLM 浏览器工具、统一协议、DOM/截图/标记 | 未开始 | LLM 可以稳定操作真实网页，结果进入消息并可恢复。 |
| G6 | 视口仿真、故障恢复、升级重启和版本统一 | 未开始 | 当前 Tab 级视口自然响应，无截断/闪烁/跨 Tab 串扰；升级和重启状态明确。 |
| G7 | 清理、全量测试、打包、提交和发布 | 未开始 | 废弃实现清理完成，同一最终提交通过全链路验收和发布预检。 |

## 5. 已收口功能点：G2 / S4.3

本轮已按 G2 子功能点完成公共右栏的真实生命周期、布局和焦点证据。G2.1-G2.7 已收口，G3.1-G3.8 也已收口；后续问题必须归入 G4-G7 的新功能点，不回头增加 G2/G3 兼容分支。

| 编号 | 功能点 | 状态 | 唯一完成条件 |
| --- | --- | --- | --- |
| G2.1 | Browser/Code/Image/Terminal/Agent 同级 Tab 共存 | 已完成 | 连续切换不改变其他内容，不新增浏览器外壳，不抢焦点。 |
| G2.2 | 两个 Browser 一级 Tab 的创建、切换、释放、恢复 | 已完成 | 每个 Tab 的 Authority lifecycle、Primary binding、guest WebContents 和 UI 连接状态一致；关闭/隐藏/恢复不高 CPU、不死循环、不复活。 |
| G2.3 | 右栏拖动、窗口缩放、折叠和恢复 | 已完成 | 右栏由同一父容器自然布局，内容不溢出、不重叠、不单独悬浮，拖动范围和边距稳定。 |
| G2.4 | 工具栏菜单、提示、标记面板和浮层层级 | 已完成 | 同一 Electron 包刷新后可框选并保存中文区域标记；标记截图仅覆盖选区；标记历史、编号、对话引用和浮层在真实 webview 上方可见；切换 Code 后浮层收敛，切回 Browser 后标记恢复。 |
| G2.5 | 对话、文件、终端和浏览器焦点隔离 | 已完成 | 对话输入只进入对话；网页输入只进入网页；切换内容后焦点和键盘事件归属正确。 |
| G2.6 | 右栏隐藏/恢复、F5、Renderer 重建和 Electron 重启 | 已完成 | 逻辑 Tab/URL 保留，物理 guest 正确重绑，其他右栏内容不受影响。 |
| G2.7 | G2 阶段门 | 已完成 | G2.1-G2.6 同一新 Electron 构建的自动化和真实桌面证据齐全。 |

### G2.2 历史失败证据

真实验收中创建两个一级 Browser Tab 后出现：

- 两个 Tab 的生命周期显示为“已释放”或 Suspended。
- 同时工具栏显示“浏览器已连接”，内容槽显示 HTML content about:blank。
- 这说明 UI 的 guest/内容槽就绪状态与 Authority lifecycle/Primary binding 不一致。

在没有新的事件序列前，不再修改 Renderer、Main 或 Rust 的同一层。下一步必须同时采集：

1. Renderer getWebContentsId、registerEmbeddedWebview、release ACK。
2. Main materialize、primary_changed、guest destroyed、detachEmbeddedGuest。
3. daemon browser.tab.status_changed、browser.tab.updated、browser.surface.primary_changed。
4. Authority 的 Creating -> Ready/Suspended/Closed 转移以及 Host generation。

必须按时间顺序确认是哪一个事件先写错状态，再做一次结构性修复。不能通过让“浏览器已连接”覆盖生命周期，也不能通过自动重试掩盖事件丢失。

### G2.2 收口证据

此前“已释放”仅表示 AI 控制租约已释放，不是 Browser Tab lifecycle。完成同一 Electron 包重启后的真实桌面复验后，两个逻辑 Tab 均能重新绑定真实 Chromium Surface，Authority 与 UI 状态一致：

- 构建身份：Magi 3.0.51 / Electron 43.4.0 / Chromium 150.0.7871.224 / buildIdentity `9aecb2cd25b99dddcbbcd0dc117707d3bf0877cf`
- 包路径：`/Users/xie/code/magi-rust-rewrite/target/electron-dist/mac-arm64/Magi.app`
- runtime epoch：`runtime-1788874587337-6895-1`
- desktop epoch：`desktop-9760ddab-c247-4e41-b441-10a984843a73`
- session：`session-1788064246534-2`
- browser session：`browser-session-1788869761951-2`
- 真实操作：重启后点击 `example.com` Tab，显示 `Example Domain`；随后在 `about:blank` 与 `example.com` 之间来回切换，两次均保持对应 URL、真实 HTML 内容和选中状态；无子 Tab、无额外窗口。
- Authority 查询：Session `ready`，两个 Tab 均为 `ready`，两个 Surface ID 均存在，`activeTabId` 与 UI 选中 Tab 一致；关闭的 Tab 未复活。
- Host 查询：`hostStatus=ready`、`hostProtocolCompatible=true`、`desktopConnectionGeneration=1`、`lastErrorCode=null`。
- 自动化回归：`npm run test --workspace @magi/desktop`，64/64 通过；包含旧 guest 迟到事件、Primary 切换、关闭 ACK、重连恢复和普通鼠标输入不重复广播 AI 接管事件。

本功能点已收口，不再针对同一生命周期职责增加兼容分支。后续问题必须归入对应阶段的新证据链处理。

### G2.1 收口证据

同一真实 Electron 包上完成右栏多功能 Tab 验收：

- 通过工作区文件树打开 `AGENTS.md` 代码视图和 `docs/images/group.png` 图片视图。
- 在同一右栏新增终端一级 Tab，终端显示“终端已连接”。
- 右栏同时保留 3 个 Browser 一级 Tab、1 个代码 Tab、1 个图片 Tab、1 个 Terminal Tab；所有内容都位于同一个 RightPane Tab group，没有子 Tab、额外浏览器壳或额外窗口。
- 依次切换 Browser -> Code -> Image -> Terminal；每次切换后对应真实内容可见，其他 Tab 身份和关闭按钮仍在，右栏外壳及父容器不变。
- 构建身份与 G2.2 相同：Magi 3.0.51 / Electron 43.4.0 / Chromium 150.0.7871.224 / buildIdentity `9aecb2cd25b99dddcbbcd0dc117707d3bf0877cf`。
- 自动化前置验证：`npm run check --workspace magi-web`（0 error / 0 warning）、`npm run test --workspace magi-web`（全部通过）、`npm run test --workspace @magi/desktop`（64/64 通过）、`git diff --check` 通过。

本功能点已收口；后续布局问题只进入 G2.3，不回头修改 G2.1 的 Tab 所有权模型。

### G2.3 收口证据

同一真实 Electron 包上完成右栏父容器布局验收：

- 构建身份：Magi 3.0.51 / Electron 43.4.0 / Chromium 150.0.7871.224 / buildIdentity `9aecb2cd25b99dddcbbcd0dc117707d3bf0877cf`。
- 包路径：`/Users/xie/code/magi-rust-rewrite/target/electron-dist/mac-arm64/Magi.app`。
- Browser 激活时，从截图可见的真实右栏分隔条拖动：默认右栏从约 `x=780` 拖到 `x=690`，浏览器内容整体跟随父 Grid 轨道变化；地址栏、Tab 栏和页面内容仍在右栏内，没有单独悬浮、遮挡或溢出。随后双击分隔条恢复默认宽度，页面恢复并保持 `Example Domain`。
- Code 激活时，从 `x=780` 拖到 `x=700`，`AGENTS.md` 内容在同一右栏内容槽内自然重排，未改变右栏外壳或中栏边界。
- Terminal 激活时，从 `x=700` 拖到 `x=800`，终端仍位于同一右栏内容槽，输入区域和 Tab 栏没有被覆盖。
- 折叠与恢复：隐藏右栏后中栏扩展到父容器边界；再次展开后 Terminal Tab 和现有 Browser/Code Tab 身份保留，未创建新窗口或新子 Tab。
- 窗口缩放：将同一窗口切换到最大工作区后，三列仍由父级 Grid 管理，右栏内容随窗口轨道缩放，无黑屏或独立坐标层。
- 之前使用 `x=858` 等点位的拖动没有改变宽度，复核截图后确认这些点位落在 Browser 页面内容而非分隔条；这不是产品失败证据。之后改用可视分隔条真实点位完成上述验收，避免把测试坐标误判为实现缺陷。

本功能点已收口，不再针对右栏宽度增加 Browser 专属几何逻辑；后续浮层、焦点和重建问题分别进入 G2.4-G2.6。

### G2.4 当前失败证据（2026-09-08）

真实 Electron 包 `/Users/xie/code/magi-rust-rewrite/target/electron-dist/mac-arm64/Magi.app` 上复现：

1. 打开 `https://example.org/`。
2. 刷新页面。
3. 点击“添加标记”，框选页面区域，输入中文备注并保存。
4. 保存失败，界面显示 `页面标记命中检测失败: binding does not match the current physical Surface (browser_surface_stale)`。

已确认的事件和状态事实：

- Authority Tab 仍为 `ready`，URL 仍为 `https://example.org/`，`navigationRevision` 从 10 推进到 12；不是 Tab 生命周期或页面导航失败。
- Main 在刷新/guest 重建后发布新的 `primary_changed` binding；Renderer 使用当前 Surface，daemon 也按新的 Primary 处理。
- `DesktopControlServer.handleSurfaceEvent` 将 `primary_changed` 转给 daemon 和 Renderer，但 `AutomationWorker.forwardSurfaceEvent` 只转发 `cdp_event`，Worker 的 `PageRuntimeState` 仍保留旧物理 `web_contents_id/target_id`。
- 标记保存进入 `browser_hit_test`，Main 将当前 binding 发送给 Worker；Worker 发现同一 `surface_id` 对应的物理 binding 不一致，按协议拒绝，导致投影和标记创建均失败。

唯一根因：运行期物理 Surface 生命周期没有进入 Worker 的重绑协议。不是截图范围、标记持久化、Authority revision 或 UI 浮层问题，不能通过放宽校验、固定等待或重试掩盖。

本轮结构性修复和验收要求：

- `primary_changed` 必须进入 Worker 的运行期重绑通知；Worker 完成 ACK 前，同一 Tab 的命令队列不得使用新 binding 执行。
- 重绑只更新 Worker 的物理页面运行态，不改变右栏布局、Browser Tab 身份或持久化标记事实。
- 增加运行期物理重绑自动化回归，覆盖刷新后 `set_annotations`/`hit_test` 使用新 binding，旧命令被资源队列隔离。
- 修复后必须用同一 Electron 包重复上述真实操作，并记录事件序列、包身份和结果；未通过前 G2.4 保持“未通过”，不得进入 G2.5。

### G2.4 收口证据（2026-09-08）

同一 Electron 包 `/Users/xie/code/magi-rust-rewrite/target/electron-dist/mac-arm64/Magi.app` 完成真实桌面验收：

- 构建身份：Magi `3.0.51` / Electron `43.4.0` / Chromium `150.0.7871.224` / buildIdentity `9aecb2cd25b99dddcbbcd0dc117707d3bf0877cf`；daemon `/health` 返回 `200`。
- 刷新 `https://example.org/` 后页面保持真实 HTML 内容可见，无黑屏；框选页面区域并输入中文备注“刷新后区域标记测试”，保存成功。
- Authority 查询确认标记为 `kind=region`，锚点 `viewport=480x854`，选区 `rect=0.442708x0.234192`，并生成独立截图 artifact；artifact 实测为 `424x400 PNG`，内容只覆盖 Example Domain 标记区域，不是整页截图。
- 页面截图确认标记序号 `1` 和备注常驻在页面；输入区同时出现 `1 刷新后区域标记测试` 引用，切换右栏内容后引用没有丢失。
- 点击标记历史按钮后，AX 树切换到独立 Popover Top Layer，真实截图显示历史入口和提示在浏览器工具栏上方可见，没有被 `<webview>` 遮挡，也没有挤压页面内容；切换到 `AGENTS.md` 代码一级 Tab 后浮层关闭，代码内容仍在同一个 RightPane 内容槽；切回 Browser 后标记区域和历史编号恢复。
- 自动化前置：`npm run test --workspace @magi/desktop`（66/66）、`npm run check --workspace magi-web`（0 error / 0 warning）、`node scripts/verify-browser-contracts.mjs`、`cargo test -p magi-browser-authority -p magi-api browser --lib`（35 passed）、`git diff --check` 全部通过。

本功能点已完成。历史失败的唯一根因是运行期 `primary_changed` 未进入 Worker 重绑协议；修复后同包真实刷新、标记保存、artifact 范围、浮层层级和跨右栏 Tab 恢复均通过。刷新期间工具栏会暂时进入真实 busy 状态，页面内容不消失；响应态和耗时优化留在 G3，不在 G2.4 增加旁路修复。

### G2.5 收口证据（2026-09-09）

同一 Electron 包 `/Users/xie/code/magi-rust-rewrite/target/electron-dist/mac-arm64/Magi.app` 完成真实焦点隔离验收：

- 构建身份：Magi `3.0.51` / Electron `43.4.0` / Chromium `150.0.7871.224` / buildIdentity `9aecb2cd25b99dddcbbcd0dc117707d3bf0877cf`；daemon `/health` 返回 `200`。
- 在个人会话 `session-1788490509881-1` 中保留 Browser 一级 Tab `https://httpbin.org/forms/post`，创建同级 Terminal Tab；没有创建子 Tab 或额外窗口。
- 点击真实 Chromium 页面表单 `Customer name` 后输入 `BrowserFocusCheck`，AX 焦点保持在网页字段，Composer 保持为空；证明网页键盘事件没有进入对话。
- 点击 Composer 后输入 `ComposerFocusCheck`，AX 焦点保持在对话输入区，网页字段仍为 `BrowserFocusCheck`；证明对话键盘事件没有进入网页。
- 切换 Terminal Tab 后输入 `TerminalFocusCheck`，AX 焦点保持在 `Terminal input`，Composer 和网页字段没有新增内容；证明终端键盘事件没有串入其他内容槽。
- 切回 Browser Tab 后重新点击网页字段，焦点恢复到真实网页输入控件，已有字段值保持不变；证明右栏 Tab 切换不丢失页面状态。
- Code/Image/Terminal 与 Browser 同级 Tab 共存、切换及文件内容显示已由同一构建的 G2.1 真实证据覆盖；Code/Image 没有可编辑网页输入面，不会改变上述焦点归属模型。
- 自动化回归：`npm run test --workspace @magi/desktop`（66/66）、`npm run check --workspace magi-web`（0 error / 0 warning）、`node scripts/verify-browser-contracts.mjs`、`cargo test -p magi-browser-authority -p magi-api browser --lib`（35 passed）。

本功能点已完成。根因不是给 Browser 增加专用焦点补丁，而是让 RightPane 的一级 Tab 切换与 Electron App Renderer 的焦点所有权保持单一入口：网页焦点只由真实 webview 内容接收，Composer 和 Terminal 只由各自 Renderer 控件接收。后续焦点问题必须归入新的事件证据链，不回头增加跨面板抢焦点逻辑。

### G2.6 收口证据（2026-09-09）

同一 Electron 包 `/Users/xie/code/magi-rust-rewrite/target/electron-dist/mac-arm64/Magi.app` 完成右栏隐藏、Renderer 刷新和完整 Electron 重启验收：

- 构建身份：Magi `3.0.51` / Electron `43.4.0` / Chromium `150.0.7871.224` / buildIdentity `9aecb2cd25b99dddcbbcd0dc117707d3bf0877cf`。
- 隐藏右栏后，中栏扩展到父容器；恢复右栏后，原有两个 Browser 一级 Tab、URL、选中 Tab 和真实页面内容恢复，未创建子 Tab 或额外窗口。
- 使用真实 Renderer 刷新后，`https://example.com/` 页面仍显示 `Example Domain`；浏览器页面未黑屏，右栏 Tab、地址栏和工具栏仍在同一个内容槽内。
- 完整退出并重新启动 Electron 后，daemon `/health` 返回 `200`，runtime epoch 为 `runtime-1788890695597-34263-1`；逻辑 browser session `browser-session-1788882921221-2` 保留，active Tab `browser-tab-1788882921235-2` 为 `ready`，URL 为 `https://example.com/`，新物理 Surface `surface-5ec3f350-0bd0-4442-b636-57cab8195179` 已重绑；第二个 Tab 保持 `suspended` 且没有 Surface。
- 真实 AX 树确认第一 Browser Tab 仍为选中态，页面标题 `Example Domain` 和 HTML 内容可见；截图确认右栏没有独立悬浮层、黑屏或覆盖中栏。
- 重启前后均只存在一个 Magi Electron 主窗口和一个 daemon，未出现 Tauri 客户端、额外 Electron 窗口或 Browser 子 Tab。

本功能点已完成。唯一状态链为“逻辑 Tab 持久化 -> 当前 Renderer 注册 -> 物理 Surface 重绑 -> Authority ready”；本轮没有增加固定等待、坐标补偿或 Browser 专属几何分支。下一步只执行 G2.7 阶段门。

### G2.7 收口证据（2026-09-09）

阶段门使用与真实验收相同的 Electron 包身份完成：

- `npm run test --workspace @magi/desktop`：69/69 通过。
- `npm run check --workspace magi-web`：0 error / 0 warning。
- `node scripts/verify-browser-contracts.mjs`：协议 3.4、22 个命令、20 个 payload Schema 校验通过。
- `cargo test -p magi-browser-authority -p magi-api browser --lib`：35/35 通过。
- `git diff --check`：通过。
- 真实桌面 G2.1-G2.6 证据均绑定同一 buildIdentity `9aecb2cd25b99dddcbbcd0dc117707d3bf0877cf`；当前 `/health` 返回 `200`，运行包路径为 `/Users/xie/code/magi-rust-rewrite/target/electron-dist/mac-arm64/Magi.app`。

G2 阶段门已完成。后续问题必须按 G3-G7 的新功能点记录，不回头重开 G2 已收口职责。

## 6. 后续功能清单

### G3 浏览器基础能力

- 地址栏、后退、前进、刷新、停止、页面输入、滚动、下载、加载失败。
- target=_blank、window.open、链接弹窗复用当前一级 Browser Tab。
- 加载、请求、刷新和跳转显示正常响应态，不黑屏，不重复导航。
- 任务完成后保留 Tab 及当前页面。

### G3 子功能账本（2026-09-09）

| 编号 | 功能点 | 状态 | 收口条件 |
| --- | --- | --- | --- |
| G3.1 | 地址栏、后退、前进、刷新 | 已完成 | 真实 Chromium 导航事件、地址/title、Authority 和右栏状态一致；同一 Tab 不重复导航。 |
| G3.2 | 停止导航 | 已完成 | Stop 作为协议级 Surface 中断命令，能够绕过当前导航等待立即调用 Chromium stop；导航原请求、`loading_changed`、页面事实和 UI 按同一代次收口，不黑屏、不删除 Tab。 |
| G3.3 | 页面输入、滚动和焦点 | 已完成 | 页面输入、滚动、对话输入的焦点边界稳定，导航期间不会把事件送到错误内容槽。 |
| G3.4 | 加载中、失败和跳转响应态 | 已完成 | 真实 Chromium 状态驱动响应态；加载、失败、停止和重定向不会闪烁或卡死工具栏。 |
| G3.5 | Popup/window.open/target=_blank 单页复用 | 已完成 | 任何页面型 popup 都不产生子 Tab 或额外窗口，统一复用当前一级 Browser Tab。 |
| G3.6 | 下载生命周期与私有目录 | 已完成 | started/progressing/completed/cancelled/interrupted 全链路由真实 Chromium DownloadItem 驱动，文件只写入 Magi 私有下载目录。 |
| G3.7 | 任务完成后保留 Tab | 已完成 | LLM Turn 完成只释放当前 root task 的控制 Lease，不关闭 Browser Tab 或当前页面；并行任务按 root task 隔离。 |
| G3.8 | G3 阶段门 | 已完成 | G3.1-G3.7 在同一 Electron 构建上通过自动化、Rust 回归、真实桌面和 daemon/Host 事件证据。 |

### G3.2 收口证据（2026-09-09）

同一 Electron 包完成真实桌面 Stop 验收：

- 构建身份：Magi `3.0.51` / Electron `43.4.0` / Chromium `150.0.7871.224` / buildIdentity `9aecb2cd25b99dddcbbcd0dc117707d3bf0877cf`；daemon `/health` 返回 `200`；runtime epoch `runtime-1788892293183-36836-1`。
- 在当前选中的 Browser 一级 Tab 中导航到 `https://httpbin.org/delay/10`。导航开始后重新读取最新 AX 树，得到当前 Stop 控件 `button 862 停止加载`；点击该最新控件后，Stop 立即收敛为刷新，地址栏恢复可用，Browser Tab 没有删除或新增，页面保持白色可见状态，没有黑屏。
- 停止后的地址栏、Tab 标题和页面事实来自当前真实 Chromium Surface；随后执行后退到 `about:blank`、前进回 `httpbin.org/delay/10`，页面 JSON 真实恢复。
- 在同一 Tab 执行刷新，工具栏进入真实 `停止加载` 状态；加载完成后恢复刷新按钮，`httpbin.org/delay/10` JSON 页面重新可见；未发生重复 Tab、额外窗口或旧导航覆盖。
- 在同一 Tab 再导航到 `https://example.com/`，完成后 Tab 标题为 `Example Domain`，页面 HTML 标题和正文真实可见，地址栏同步为 `https://example.com/`。
- 本次真实操作没有使用坐标补偿、固定等待驱动产品逻辑、旧快照恢复或第二套浏览器路径；Stop 通过主进程真实 `WebContents.stop()` 执行，页面状态由同一 navigation revision 事件链收口。

本功能点已完成。下一项严格回到账本顺序执行 G3.1（地址栏、后退、前进、刷新），未完成 G3.1 前不进入 G3.3。

### G3.1 收口证据（2026-09-09）

同一 Electron 包完成真实桌面导航控件验收：

- 构建身份：Magi `3.0.51` / Electron `43.4.0` / Chromium `150.0.7871.224` / buildIdentity `9aecb2cd25b99dddcbbcd0dc117707d3bf0877cf`；daemon `/health` 返回 `200`，runtime epoch `runtime-1788892293183-36836-1`。
- 在当前选中的同一个 Browser 一级 Tab 中，通过地址栏输入并提交 `https://example.org/`；加载完成后地址栏为该 URL，Tab 标题为 `Example Domain`，真实页面 HTML 标题和正文可见，没有创建子 Tab 或额外窗口。
- 点击后退后地址栏和真实页面回到 `https://example.com/`；点击前进后恢复 `https://example.org/`。两个操作均在原一级 Tab 内完成，右栏 Tab 数量和外壳不变。
- 点击刷新后工具栏按真实 Chromium 状态进入刷新/加载状态，随后恢复刷新按钮；页面仍为 `Example Domain`，未出现黑屏、旧 URL 覆盖或重复导航。
- G3.2 的同包证据已覆盖停止导航后的后退、前进、刷新和再次地址栏导航；两组证据共同证明地址栏、历史导航、刷新、Stop 的结果都由同一 WebContents/Surface 事件链收口。
- 自动化前置：`npm run test --workspace @magi/desktop`（70/70）、`npm run check --workspace magi-web`（0 error / 0 warning）、`node scripts/verify-browser-contracts.mjs`、`cargo test -p magi-browser-authority -p magi-api browser --lib`（36 passed）、`git diff --check` 全部通过。

本功能点已完成。下一项进入 G3.3（页面输入、滚动和焦点）；不回头修改 G3.1/G3.2 已收口职责。

### G4 阶段门：G1-G3 统一构建收口

| 编号 | 功能点 | 状态 | 完成条件 |
| --- | --- | --- | --- |
| G4.1 | 构建身份与运行时一致性 | 进行中 | G1-G3 的自动化、Rust、daemon、Host、真实 Electron 证据全部绑定同一提交和同一 Electron 包。 |
| G4.2 | 多功能右栏与单页 Browser 约束回归 | 未开始 | Browser/Code/Diff/Markdown/Image/Terminal/Agent 同级 Tab 共存；Browser 不创建子 Tab、额外窗口或外壳覆盖。 |
| G4.3 | 会话与任务基础能力回归 | 未开始 | 新建、中文发送、续聊、历史切换、并行、多会话、删除、F5、daemon 重连和 Electron 重启均无消息丢失或会话消失。 |
| G4.4 | G4 阶段门 | 未开始 | G4.1-G4.3 通过同一包真实验收，并形成唯一基线后才进入 G5。 |

### G5 LLM 与内容采集

- initialize/initialized 能力协商。
- request/response/notification/server-request 类型化协议。
- 资源级串行队列、取消、超时、重连恢复。
- 浏览器工具作为 Session -> Turn -> Item 标准 Item 流程，不走旁路事件。
- DOM 读取、点击、输入、悬停、滚动、截图、节点选择、控制台/网络/标题。
- 截图、标记、DOM 选择同时存在；标记区域截图必须只覆盖标记区域，不得使用整页截图。
- 标记序号、备注、Artifact、消息引用进入对话记录，刷新和重启可恢复。
- 页面移动鼠标、导航或 guest 重绑不得清除已提交的标记/DOM 消息上下文。
- AI 接管状态下虚拟鼠标持续可见，任务释放后明确消失，不能抢对话焦点。
- 有 workspace 与无 workspace 都能使用允许的浏览器能力，不因 workspace-only 变更接口失败。

### G5 子功能账本

| 编号 | 功能点 | 状态 | 完成条件 |
| --- | --- | --- | --- |
| G5.1 | LLM 浏览器工具发现与能力协商 | 未开始 | Desktop Worker 能发现完整 Browser 工具目录，`initialize/initialized` 和协议版本协商成功；Web/Mobile Web 明确不可用而不伪造已连接。 |
| G5.2 | DOM 读取、节点选择与页面操作 | 未开始 | LLM 可读取标题/DOM、选择节点、点击、输入、悬停、滚动；结果进入标准 Item 和消息，不走旁路事件。 |
| G5.3 | 截图、标记与 Artifact 消息链 | 未开始 | 截图、区域标记和 DOM 选择可同时存在；选区 artifact 不是整页，序号/备注/引用持久化，刷新、导航、重启和移动鼠标不丢失。 |
| G5.4 | 控制 Lease、虚拟鼠标与焦点 | 未开始 | AI 接管时虚拟鼠标持续可见；Lease 释放后页面和 Tab 保留且鼠标状态收敛，不抢 Composer 焦点。 |
| G5.5 | 有/无 workspace 能力边界 | 未开始 | 无 workspace 可使用只读和允许的浏览器能力；需要 workspace 的变更操作返回明确状态，不出现无来源的 bridge-runtime 错误。 |
| G5.6 | G5 阶段门 | 未开始 | G5.1-G5.5 通过自动化和真实 Electron LLM 场景，并验证结果进入消息且可恢复。 |

### G6 当前 Browser Tab 视口与恢复

- 仅支持宽屏、窄屏、自定义三种用户语义；设备参数只属于当前 Browser Tab。
- 视口调整使用 Chromium 的设备指标能力，不手工缩放页面、不截断页面、不计算外部坐标。
- 拖动调整实时自然生效，不通过整页刷新、不闪烁、不切换其他 Tab 配置。
- 页面响应式内容由浏览器布局自适应；验收检查右侧内容完整、无水平截断。
- Host、Playwright、Chromium 使用统一 manifest/version source。
- 安装、检查更新、卸载、激活、重启提示都返回真实状态；需要重启时明确提示并完成重启恢复。

### G6 子功能账本

| 编号 | 功能点 | 状态 | 完成条件 |
| --- | --- | --- | --- |
| G6.1 | 宽屏、窄屏、自定义视口 | 未开始 | 仅支持三种用户语义；设备参数只属于当前 Browser Tab，不进入会话普通布局持久化，不跨 Tab/窗口共享。 |
| G6.2 | Chromium 原生响应式布局 | 未开始 | 通过 Chromium 设备指标改变页面布局，内容自适应、不手工缩放、不截断、不计算外部坐标。 |
| G6.3 | 实时调整与稳定恢复 | 未开始 | 拖动实时生效，不整页刷新、不闪烁、不黑屏；不同大小窗口/Tab 不发生来回变换，刷新和重启后按当前 Tab 规则恢复。 |
| G6.4 | Host/Playwright/Chromium 统一版本 | 未开始 | 从单一 Runtime manifest 生成并校验 Host、Playwright、Chromium 版本，安装、更新、卸载、激活和重启提示状态真实一致。 |
| G6.5 | G6 阶段门 | 未开始 | G6.1-G6.4 在宽屏、窄屏、自定义、刷新、重启和多 Tab 场景通过真实桌面验收。 |

### G7 清理与交付

- 删除浏览器 WebContentsView 显示路径、浏览器 setBounds/geometry IPC、截图投影、旧 Overlay 双实现和废弃兼容分支。
- 只保留一套右栏外壳、一套 Browser Tab 内容槽、一套状态协议和一套生命周期状态机。
- 变更检查、Web/Desktop/Rust/Worker 测试、Electron 打包、真实桌面全链路测试全部使用同一提交。
- 通过发布前置校验后才允许提交主分支、打 Tag 或发布 GitHub Release。

### G7 子功能账本

| 编号 | 功能点 | 状态 | 完成条件 |
| --- | --- | --- | --- |
| G7.1 | 废弃路径和冗余兼容清理 | 未开始 | 删除 WebContentsView/setBounds/截图投影/旧 Overlay 和双实现；搜索确认无生产引用和孤立协议。 |
| G7.2 | 全量变更检查与测试 | 未开始 | Web/Desktop/Worker/Rust/契约/会话/浏览器/视口/版本测试均使用同一提交通过，`git diff --check` 通过。 |
| G7.3 | Electron 打包与真实桌面全链路 | 未开始 | 使用最终提交构建 Electron 包，完成会话、代码、终端、多代理、浏览器、DOM、标记、视口、升级重启全链路验收。 |
| G7.4 | 发布前置与交付 | 未开始 | release preflight 通过，主分支、Tag、GitHub Release、更新元数据和旧版迁移路径均可验证。 |
| G7.5 | G7 最终阶段门 | 未开始 | G0-G7 全部已完成；未达到前不得宣称项目完成或发布。 |

## 7. 自动化与真实 Electron 验收矩阵

自动化只能作为前置证据；每组都必须在同一 Electron 包上真实操作，并保留 build identity、状态目录、事件/日志摘要和 UI 结果。

| 验收组 | 必测场景 | 结果 |
| --- | --- | --- |
| 会话 | 新建、中文发送、续聊、历史切换、并行、多会话、删除、重命名、F5、daemon 重连、Electron 重启 | 已完成（阶段 S3 证据已登记，后续回归仍需随最终提交复验） |
| 右栏 Tab | Browser/Code/Diff/Markdown/Image/Terminal/Agent 轮换，两个 Browser Tab 切换和关闭 | 未完成 |
| 布局 | 右栏拖动、窗口缩放、最大化、折叠恢复、皮肤切换、浮层层级 | G2.3、G2.4 浮层层级已完成；皮肤切换随 G2.5/G2.6 回归 |
| 浏览器 | 百度/Bing、页面标题、搜索、输入、滚动、刷新、加载失败、下载、Popup | 未开始 |
| LLM | 打开网页、读取标题、搜索项目、点击、DOM、截图、回复结果 | 未开始 |
| 标记/DOM | 同时标记和选中节点，移动鼠标、刷新、导航、发送、重启恢复 | 未开始 |
| 视口 | 宽屏、窄屏、自定义、拖动实时调整、不同 Tab 独立配置、无截断 | 未开始 |
| 故障 | guest 销毁、daemon 重连、工具取消、超时、连接失败、页面崩溃、关闭 Tab | 未开始 |
| 版本 | 安装、检查更新、卸载、激活、重启提示、统一 runtime manifest | 未开始 |

## 8. 运行命令基线

日常开发只启动 daemon：

    ./scripts/dev-daemon.sh

前端/桌面/Rust 前置检查：

    npm run check
    npm test
    cargo check --workspace
    node scripts/verify-browser-contracts.mjs
    git diff --check

真实桌面验收入口：

    http://127.0.0.1:38123/web.html

正式发布前必须使用项目约定的完整预检：

    npm run release:preflight:full -- --tag vX.Y.Z

## 9. 更新记录

| 日期 | 功能点 | 状态变化 | 证据与下一步 |
| --- | --- | --- | --- |
| 2026-09-08 | 计划账本重建 | 进行中 | 清理重复、相互矛盾的历史计划，锁定 G2.4 为当前唯一入口；按账本逐项推进，不跨阶段。 |
| 2026-09-08 | G1 / 首次导航 | 未通过 -> 已完成 | 同一 Electron 包创建一级 Browser Tab，真实注册 guest，导航到 example.com，AX 显示 Example Domain；自动化和 git diff --check 通过。 |
| 2026-09-08 | S4.1 / 显示几何边界 | 进行中 -> 已完成 | 生产路径已删除浏览器 WebContentsView、浏览器 setBounds 和内容槽几何 IPC；浏览器显示由 Renderer webview 内容槽承担。 |
| 2026-09-08 | G2.2 / 双 Browser Tab 生命周期 | 未通过 | 真实场景出现 lifecycle 为 Suspended/已释放、工具栏却显示“已连接”且内容槽仍可见的状态分裂；冻结后续阶段，先采集 Renderer/Main/daemon/Authority 时间序列。 |
| 2026-09-08 | G2.2 / 双 Browser Tab 生命周期 | 未通过 -> 已完成 | 同一 Electron 包重启后重新点击 `example.com` Tab，真实 HTML 内容恢复；来回切换两个一级 Tab 均保持 URL、Surface、Authority lifecycle 和 activeTab 一致，关闭 Tab 不复活；Host ready 且无错误。`npm run test --workspace @magi/desktop` 64/64 通过。下一步进入 G2.1。 |
| 2026-09-08 | G2.1 / 多功能一级 Tab 共存 | 未验证 -> 已完成 | 同一 Electron 包真实打开 `AGENTS.md` 代码视图、`group.png` 图片视图和终端，并与 3 个 Browser 一级 Tab 共存；Browser -> Code -> Image -> Terminal 连续切换均显示正确内容，无子 Tab、额外窗口或右栏外壳变化。`npm run check --workspace magi-web` 0 error/0 warning，`npm run test --workspace magi-web` 和 `npm run test --workspace @magi/desktop` 通过。下一步进入 G2.3。 |
| 2026-09-08 | G2.3 / 右栏父容器布局 | 未验证 -> 已完成 | 同一 Electron 包完成 Browser、Code、Terminal 激活态的真实拖动，窗口最大工作区缩放，以及右栏折叠/恢复；正确命中可视分隔条后，内容始终跟随父 Grid 轨道，无覆盖、溢出、额外窗口或子 Tab。下一步进入 G2.4。 |
| 2026-09-08 | 会话阶段 S3 | 进行中 -> 已完成 | 同一构建已有新建、续聊、历史切换、并行、删除、F5、daemon 重连和 Electron 重启证据；最终交付前仍需随最终提交回归。 |
| 2026-09-08 | G2.4 / 刷新后标记投影 | 未验证 -> 未通过 | 同一 Electron 包真实刷新后保存区域标记失败，错误为 `browser_surface_stale`。事件链确认运行期 `primary_changed` 未进入 Worker 重绑；本轮先修复唯一生命周期缺口，再按同一包复验。 |
| 2026-09-08 | G2.4 / 工具栏、标记和浮层 | 未通过 -> 已完成 | 同一 Electron 包真实刷新后保存中文区域标记成功；artifact 为选区 `424x400 PNG`，不是整页；编号和消息引用常驻，标记历史 Popover 位于 webview 之上；切换 Code 后浮层关闭，切回 Browser 后标记恢复。自动化与 Rust browser 测试通过。下一步只处理 G2.5 焦点隔离。 |
| 2026-09-08 | P0.2 / 工作区选择器注册并绑定当前会话 | 进行中 -> 已完成 | 新打包 Electron 目录包真实选择 `/Users/xie/code/magi-rust-rewrite`；注册完成后 URL 收敛为 `scope=workspace&workspaceId=workspace-1784091993188-0`，文件树显示同一路径，输入区工作区按钮显示 `magi-rust-rewrite`，分支状态属于同一工作区。P0.3/P1 进入唯一执行项。 |
| 2026-09-09 | G2.5 / 对话、文件、终端和浏览器焦点隔离 | 进行中 -> 已完成 | 同一 Electron 包真实验证网页表单、Composer、Terminal 输入焦点互不串扰；切回 Browser 后网页输入焦点可恢复；G2.1 已覆盖同包 Code/Image/Terminal 共存与切换。桌面测试 66/66、Svelte 0 error/0 warning、契约校验和 Rust 浏览器测试 35 passed。下一步进入 G2.6。 |
| 2026-09-09 | G2.6 / 右栏隐藏、Renderer 刷新与 Electron 重启 | 进行中 -> 已完成 | 同一 Electron 包完成右栏隐藏/恢复、真实 Renderer 刷新和完整 Electron 重启；逻辑 Tab/URL 保留，当前 Tab 重绑 ready Surface，第二 Tab 保持 suspended，无黑屏、子 Tab 或额外窗口。下一步进入 G2.7。 |
| 2026-09-09 | G2.7 / G2 阶段门 | 进行中 -> 已完成 | Desktop 69/69、Svelte 0 error/0 warning、浏览器契约通过、Rust 浏览器 35/35、git diff --check 通过；与真实桌面证据绑定同一 buildIdentity `9aecb2cd25b99dddcbbcd0dc117707d3bf0877cf`。下一步进入 G3。 |
| 2026-09-09 | G3.2 / 停止导航 | 进行中 -> 已完成 | 同一 Electron 包真实导航 `httpbin.org/delay/10` 后使用最新 AX Stop 控件立即停止；工具栏、地址栏、Tab 和真实页面状态收口，后退/前进/刷新/再次导航均通过，无黑屏、删除、子 Tab 或额外窗口。下一步回到 G3.1。 |
| 2026-09-09 | G3.1 / 地址栏、后退、前进、刷新 | 未开始 -> 已完成 | 同一 Electron 包真实地址栏导航到 `example.org`，后退回 `example.com`，前进恢复 `example.org`，刷新恢复真实 HTML；地址/title/页面/右栏一级 Tab 一致，自动化与契约/Rust 回归通过。下一步进入 G3.3。 |
| 2026-09-09 | G3.3 / 页面输入、滚动和焦点 | 未开始 -> 已完成 | 同一 Electron 包打开 `https://httpbin.org/forms/post`，网页表单输入 `BrowserInputCheck`；Composer 输入 `ComposerFocusCheck`；清空 Composer 后重新点击网页字段，字段值保持；打开 Wikipedia 长页面并通过真实 webview 滚动，截图确认页面位置实际变化。网页输入、对话输入、焦点和滚动没有串扰。自动化 `npm run test --workspace @magi/desktop` 70/70、`npm run check --workspace magi-web` 0 error/0 warning、Rust browser 36 passed、契约校验和 `git diff --check` 均通过。下一步进入 G3.4。 |
| 2026-09-09 | G3.4 / 加载中、失败和跳转响应态 | 进行中 -> 已完成 | 同一新 Electron 包完成主文档失败、Retry、重启失败态恢复、重定向和 Stop 回归。首次失败保留 Chromium 错误页运行态并显示 `ERR_NAME_NOT_RESOLVED`；重启后不再误发布旧成功页，恢复错误页可通过刷新/重试进入稳定失败投影，再导航成功；重定向按最终 URL 收口；Stop 后无黑屏。Desktop 71/71、契约 3.4/23/21 和 `git diff --check` 通过。下一步进入 G3.5。 |
| 2026-09-09 | G3.5 / Popup、window.open、target=_blank 单页复用 | 未开始 -> 已完成 | 同一 Electron 包真实验证链接、`window.open`、POST 表单和 `about:blank` popup。前三类页面型导航均复用当前 webview target 和右栏一级 Browser Tab，POST 参数保留；不可信 popup 不转移原页面；全程无子 Tab、子窗口或额外 Browser Window。Desktop 71/71、契约 3.4/23/21 和 `git diff --check` 通过。下一步进入 G3.6。 |
| 2026-09-09 | G3.6 / 下载生命周期与私有目录 | 未开始 -> 已完成 | 同一 Electron 目录包真实验证私有目录、started/progressing/completed/cancelled/interrupted、标签关闭清理和 Renderer 终态保留；对首段 64 KiB 后断开的 10 MiB 响应，Chromium 最终发布 `interrupted`，`activeBrowserDownloads=[]`，私有目录无残留文件。Main 下载状态改为显式保存 `started/progressing/interrupted`，快照不再把可恢复中断伪装成 `started`；Desktop 73/73、全量 check、下载生命周期、契约和 `git diff --check` 通过。下一步进入 G3.7。 |
| 2026-09-09 | G3.7 / 任务完成后的 Browser Lease 与页面保留 | 进行中 -> 已完成 | 根因是终态清理按 `session_id` 撤销 Browser Lease，误伤同一会话的并行任务，并把控制资源释放与页面生命周期混在一起。结构性修复改为按 `session_id + root_task_id` 精确撤销当前任务 Lease；终态路径不调用 `close_page`、不销毁 WebContents。`cargo test -p magi-api execution_resource_coordinator_revokes_browser_lease_by_task_scope --lib` 通过；同一 Electron 包完成重复激活复用同一 Surface、不同一级 Tab 获取独立 Surface、Host generation 保持 1、页面和 Tab 保留的 live 验收。 |
| 2026-09-09 | G3.7 / 标记和生命周期同包复验 | 已完成 | 同一 Electron 包 `/Users/xie/code/magi-rust-rewrite/target/electron-dist/mac-arm64/Magi.app`，buildIdentity `9aecb2cd25b99dddcbbcd0dc117707d3bf0877cf`，runtime epoch `runtime-1788948037221-16078-1`；真实标记截图为 `192x340`，整页为 `960x1708`，按选区裁剪，备注可持久化。无黑屏、子 Tab、额外窗口或旧 Surface 误绑定。 |
| 2026-09-09 | G3.8 / G3 阶段门 | 未开始 -> 已完成 | G3.1-G3.7 使用同一 Electron 构建通过 Desktop/Worker/Svelte 检查、Rust 资源隔离与浏览器 Authority 回归、浏览器契约、下载生命周期、标记 live 验收、生命周期 live 验收和 `git diff --check`；下一阶段为 G4 阶段门，不回头重开 G3。 |

## 10. 当前执行指令

当前唯一入口为 G4：在进入 G5 之前完成 G1-G3 的统一构建阶段门和会话/右栏基础能力回归。未完成 G4.4 前不得进入 G5、G6 或 G7。

### 10.0 本轮执行批次（2026-09-09）

本轮目标固定为完成 G4 阶段门，不扩展目标、不回头重写已收口的 G1-G3：

| 顺序 | 功能点 | 当前状态 | 唯一动作 | 放行条件 |
| --- | --- | --- | --- | --- |
| 1 | G4.1 构建身份与运行时一致性 | 进行中 | 重新构建干净 Electron 包，核对提交、包内资源、daemon、Renderer、Chromium 运行身份一致 | 同一构建身份的自动化、daemon、Host、Electron 证据齐全，`gitDirty=false` |
| 2 | G4.2 多功能右栏与单页 Browser 约束回归 | 未开始 | 在同一包真实切换 Browser/Code/Diff/Markdown/Image/Terminal/Agent，并验证浮层、焦点、单页约束 | 每个一级 Tab 可打开、切换、恢复；Browser 无子 Tab、额外窗口或外壳覆盖 |
| 3 | G4.3 会话与任务基础能力回归 | 未开始 | 在同一包真实执行历史切换、新建、并行、续聊、删除、F5、daemon 重连和 Electron 重启 | 消息、会话、任务和 Browser Tab 不丢失、不串线、不消失 |
| 4 | G4.4 阶段门 | 未开始 | 汇总前三项的同一构建证据并冻结唯一基线 | G4.1-G4.3 全部已完成；否则停留在失败项 |

执行纪律：每个功能点只能有一条当前状态和一条证据记录；验证失败时先记录事件序列和唯一根因，再做一次职责边界内的结构性修复，修复后立即回归并更新本表和第 9 节。没有新证据不得重复修改同一功能点。

### 10.1 历史执行账本（2026-09-09）

本轮已完成 G3，不重新设计已收口的右栏内容槽和 Browser Surface 生命周期。后续只按 G4 账本建立新的证据链。

| 项目 | 状态 | 收口证据 |
| --- | --- | --- |
| P0.1 Electron 包启动与 daemon 健康检查 | 已完成 | 包身份 `Magi 3.0.51 / Electron 43.4.0 / Chromium 150.0.7871.224`；`/health` 返回 `200`，build identity 为 `9aecb2cd25b99dddcbbcd0dc117707d3bf0877cf`。 |
| P0.2 工作区选择器注册并绑定当前会话 | 已完成 | 新 Electron 包真实选择 `/Users/xie/code/magi-rust-rewrite`；权威 workspace identity、URL、文件树和输入区工作区投影一致，`workspaceId=workspace-1784091993188-0`。 |
| P1. G2.4 刷新后标记与浮层 | 已完成 | 同一 Electron 包完成刷新、框选、中文备注保存、选区 artifact 校验、标记编号/消息引用、Popover 层级和 Browser/Code 切换恢复；重启后的会话恢复属于 G2.6，不在本项目点重复跨阶段。 |
| P2. G2.5 焦点隔离 | 已完成 | 同一 Electron 包真实完成网页表单 -> Composer -> Terminal -> Browser 的焦点与键盘边界验证；网页字段值、Composer 文本和终端输入分别留在各自内容槽；G2.1 已覆盖 Code/Image 同级 Tab 切换。 |
| P3. G2.6 隐藏/恢复与 Renderer 重建 | 已完成 | 同一 Electron 包完成右栏隐藏/恢复、真实 Renderer 刷新和完整 Electron 重启；逻辑 Tab/URL 保留，当前 Tab 重新绑定 ready Surface，第二 Tab 保持 suspended，无黑屏、子 Tab 或额外窗口。 |

P0.2 的失败不得通过设置前端 `selectedWorkspaceId`、直接写 URL 或保留个人会话假状态绕过；必须证明注册 API、权威 bootstrap、会话导航提交和 UI 投影使用同一个 workspace identity。完成每个项目后立即更新本节状态、证据和下一项，禁止跳项或重复修改已收口职责。

完成顺序固定为：

1. 记录当前 Electron 包、daemon health、desktop epoch、逻辑 Browser Tab 与右栏状态。
2. 验证隐藏右栏后 Browser/Code/Image/Terminal 逻辑 Tab 身份、URL 和持久化内容仍保留。
3. 在同一包执行 Renderer F5/重建，确认真实 guest 重新绑定当前 Primary，页面不黑屏、不新增子 Tab、不复活已关闭 Tab。
4. 完整退出并重启 Electron，确认 daemon 重连、右栏恢复和 Browser URL/标记/内容状态；若默认作用域或历史 Tab 未恢复，记录唯一根因并只修复 G2.6 状态恢复链路。
5. G2.7 已通过；现在进入 G3，先盘点现有导航、输入和 Popup 实现，再建立单一缺口证据链。

### 10.2 G3 已完成状态（2026-09-09）

| 项目 | 状态 | 收口证据 |
| --- | --- | --- |
| G3.2 停止导航 | 已完成 | 同一 Electron 包真实操作慢导航并点击最新 AX Stop；停止后地址栏恢复、Tab 保留、页面不黑屏，后退/前进/刷新/再次导航回归通过。 |
| G3.1 地址栏、后退、前进、刷新 | 已完成 | 同一 Electron 包真实完成地址栏导航、历史后退/前进和刷新；地址/title/HTML 内容与同一一级 Tab 一致，自动化 70/70、Rust 浏览器 36/36、契约和 Svelte 检查通过。 |
| G3.3 页面输入、滚动和焦点 | 已完成 | 同一 Electron 包真实完成网页表单输入、Composer 输入、焦点切换和长页面滚动；字段值保持，网页输入、对话输入和滚动没有串扰。 |
| 当前 G3.4 加载中、失败和跳转响应态 | 已完成 | 按下表逐项收口；所有项均使用当前 `/Users/xie/code/magi-rust-rewrite/target/electron-dist/mac-arm64/Magi.app` 完成真实桌面验收。 |

#### G3.4 执行账本（2026-09-09）

G3.4 已按当前 Electron 包完成主文档 `Network.loadingFailed`、`chrome-error://` 导航收口、错误码还原和重启重绑失败态验收；以下为已登记的收口证据，不得因后续阶段回归失败而重开同一职责，除非出现新的导航事件序列。

| 编号 | 验收项 | 状态 | 收口证据 |
| --- | --- | --- | --- |
| G3.4.1 主文档失败响应态 | 已完成 | 当前包导航 `https://example.invalid/?g34=1`，快速显示错误层 `ERR_NAME_NOT_RESOLVED`；guest 保持同一 `chrome-error://` 运行态，UI 地址/Tab/重试入口保留，未出现白屏成功态。 |
| G3.4.2 失败后 Retry | 已完成 | 当前包在错误层点击 Retry 后同一目标再次快速失败并保持 `ERR_NAME_NOT_RESOLVED`；随后重新导航本地服务成功，错误层消失，地址和真实页面恢复。 |
| G3.4.3 重启后失败态恢复 | 已完成 | 新包完整重启后不再误发布旧成功页面；重绑 `chrome-error://` guest 后通过刷新/重试显示 `ERR_FAILED <target>` 稳定错误层，再导航本地服务成功恢复真实页面。 |
| G3.4.4 重定向响应态 | 已完成 | 当前包导航 `https://httpbin.org/redirect/2?url=https%3A%2F%2Fexample.com%2F`，实际按 httpbin 最终 `/get` 页面收口；地址/Tab 与 guest URL 一致，无错误层或重复 Tab。 |
| G3.4.5 Stop 响应回归 | 已完成 | 当前包慢导航 `delay/8` 中点击 Stop，控件 1ms 内进入 Main 并恢复为刷新，地址保持当前页，无错误层；随后刷新恢复真实 JSON 内容。 |
| G3.4.6 同包自动化收口 | 已完成 | 新包真实验收后 `npm run check --workspace @magi/desktop`、`npm run test --workspace @magi/desktop`（71/71）、`node scripts/verify-browser-contracts.mjs`（协议 3.4 / 23 命令 / 21 payload）和 `git diff --check` 全部通过；临时诊断脚本和临时解包目录已清理。 |

本节是当前唯一执行入口。G3.1-G3.7 已收口，G3.8 阶段门已通过；下一阶段进入 G4。后续问题必须提供新的事件证据，不得回头对 G3 已收口职责增加兼容分支。每完成一项立即把本节和第 9 节更新为“已完成”，并记录同一构建身份及真实操作证据。

#### G3.5 执行账本（2026-09-09）

当前实现已在 Main 的 Chromium 创建边界上拒绝原生 popup，并把合法页面型 `window.open`、`target=_blank` 和表单提交转换为当前 Browser Tab 导航。完成状态必须以下表同包真实验证为准。

| 编号 | 验收项 | 状态 | 收口证据 |
| --- | --- | --- | --- |
| G3.5.1 target=_blank 当前页复用 | 已完成 | 当前包真实点击 `target="_blank"` 链接后，同一 webview target 从 `index.html` 导航到 `target.html`；右栏仍为 1 个 Browser Tab、1 个 `<webview>`，CDP 仍只有 App Renderer + 当前 webview 两个 target，无额外窗口。 |
| G3.5.2 window.open 当前页复用 | 已完成 | 当前包真实点击 `window.open('./target.html','_blank')` 按钮后，同一 webview target 和右栏一级 Tab 保留，页面变为 `G35 Target`；无子 Tab、子窗口或新 Browser Window。 |
| G3.5.3 表单 popup 当前页复用 | 已完成 | 当前包真实提交 `target="_blank"` POST 表单，响应页在同一 webview target 中显示 `G35 POST Result / posted-value`，证明 POST 数据保留；右栏仍为 1 个 Browser Tab 和 1 个 `<webview>`。 |
| G3.5.4 不可信 popup 阻断 | 已完成 | 当前包真实调用 `window.open('about:blank')` 后，原 `G35 Source` 页面、URL、标题和当前 Tab 保持不变，未创建子 Tab 或额外窗口；Main 边界按不可信 popup 拒绝转移。 |
| G3.5.5 同包自动化收口 | 已完成 | Desktop check/test（71/71）、浏览器契约（协议 3.4 / 23 命令 / 21 payload）和 `git diff --check` 全部通过；临时测试页面、POST 服务和诊断脚本已清理。 |

#### G3.6 执行账本（2026-09-09）

当前下载链路必须只写入 Magi 私有 `browser-downloads` 目录，并以 started/progressing/completed/cancelled/interrupted 的真实 Chromium 状态驱动 UI 与 Authority/消息投影。完成前不得把下载器接回系统默认下载目录。

| 编号 | 验收项 | 状态 | 收口证据 |
| --- | --- | --- | --- |
| G3.6.1 私有目录与文件命名 | 已完成 | 构建身份 `Magi 3.0.51 / Electron 43.4.0 / Chromium 150.0.7871.224` 的真实下载文件只出现在 `/tmp/magi-g36-real-v5/desktop/browser-downloads/browser-session-.../`，完成文件为 `65536` bytes；同一时间系统 `~/Downloads` 没有新增文件。 |
| G3.6.2 started/progressing/completed | 已完成 | 同一构建真实点击下载链接后显示 `正在下载 192 KB / 50 MB`，小文件显示 `下载完成`；完成文件大小与 `Content-Length` 一致，Browser Tab 和当前页面保留。 |
| G3.6.3 cancelled/interrupted | 已完成 | 同一新包真实点击取消显示 `已取消下载`；本地测试服务首段 64 KiB 后断开 10 MiB 响应，Chromium 最终显示 `下载中断`，`getSnapshot().activeBrowserDownloads=[]`，`browser-downloads/browser-session-.../` 无残留文件。 |
| G3.6.4 清理与生命周期 | 已完成 | 同一构建真实启动 50 MB 下载后关闭 Browser Tab，右栏关闭、下载面板消失，私有目录没有 `close-tab.bin` 部分文件；Main 退出和清理路径已有自动化覆盖。 |
| G3.6.5 同包自动化收口 | 已完成 | `npm run check`（Desktop、Worker、Svelte 均通过，Svelte 0 error/0 warning）、`npm test --workspace @magi/desktop`（73/73）、`cargo check -p magi-daemon`、下载生命周期校验、浏览器契约校验和 `git diff --check` 全部通过；临时测试服务与下载测试目录未进入仓库。 |

#### G3.7 执行账本（2026-09-09）

本功能点只收口“任务结束后的资源所有权”：终态 Turn 释放当前 root task 的 Browser 控制 Lease，保留 BrowserAuthority 的 Session、一级 Tab、当前 URL、页面内容和右栏 Tab。任务结束不得调用 `close_page`，不得关闭 WebContents，不得销毁用户仍可查看的页面。资源筛选必须包含 `session_id + root_task_id`，不能释放同一会话中其他并行任务的 Lease。

| 编号 | 验收项 | 状态 | 收口证据 |
| --- | --- | --- | --- |
| G3.7.1 终态释放边界 | 已完成 | 终态清理按 `session_id + root_task_id` 精确撤销当前任务 Lease；`cargo test -p magi-api execution_resource_coordinator_revokes_browser_lease_by_task_scope --lib` 通过。页面生命周期不再由 Lease 释放驱动。 |
| G3.7.2 页面与一级 Tab 保留 | 已完成 | 同一 Electron 包真实验收确认任务控制释放后 URL、标题、页面 DOM、Browser 一级 Tab 和右栏内容槽继续可见；未调用 `close_page`，无子 Tab、额外窗口或页面丢失。 |
| G3.7.3 并行任务隔离 | 已完成 | 同一会话任务 Lease 按 `root_task_id` 隔离；结束一个任务不会撤销其他任务控制资源。生命周期 live 回归确认重复激活复用 `surface-761ed08c-810c-405a-a542-a5c0d8f7aca1`，第二个一级 Tab 使用独立 `surface-946a802d-0fbf-443f-9031-6bb3ba6e3841`，Desktop Host generation 保持 `1`。 |
| G3.7.4 同包自动化收口 | 已完成 | 当前包完成 `npm run check`、`npm test`、`cargo check -p magi-daemon`、Rust 浏览器/资源隔离回归、`node scripts/verify-browser-contracts.mjs`、`node scripts/verify-browser-core-live.mjs --session-id session-1788823897361-1 --lifecycle-regression`、`node scripts/verify-browser-core-live.mjs --session-id session-1788823897361-1 --write-annotation` 和 `git diff --check`；所有命令通过。 |

#### G3.8 阶段门（2026-09-09）

| 项目 | 状态 | 证据 |
| --- | --- | --- |
| 同一 Electron 构建身份 | 已完成 | `/Users/xie/code/magi-rust-rewrite/target/electron-dist/mac-arm64/Magi.app`；Magi 3.0.51 / Electron 43.4.0 / Chromium 150.0.7871.224 / buildIdentity `9aecb2cd25b99dddcbbcd0dc117707d3bf0877cf`。 |
| daemon/Host 事件链 | 已完成 | daemon `/health` 为 HTTP 200，Host `ready`，`hostProtocolCompatible=true`，runtime epoch `runtime-1788948037221-16078-1`；重复激活不增加 Desktop Host generation。 |
| 页面和一级 Tab 约束 | 已完成 | 导航、输入、刷新、失败、重定向、Stop、Popup、下载和任务终态均在当前一级 Browser Tab 内完成；没有子 Tab、额外窗口、WebContentsView 覆盖或页面黑屏。 |
| 标记与消息事实 | 已完成 | 标记截图从 `960x1708` 整页裁剪为 `192x340`，备注可编辑、持久化并可重新读取；标记事实不随鼠标移动或任务 Lease 释放丢失。 |
| 阶段门结论 | 已完成 | G3.1-G3.7 均已完成，后续进入 G4；不得以旧日志、旧包或无新事件证据重新打开本阶段。 |

在 G2.7、G3、G5、G6、G7 全部变为已完成前，不得宣称目标完成，不得发布.
