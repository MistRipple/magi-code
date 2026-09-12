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
| Host/Worker/Chromium 版本 | 单一 Desktop Runtime manifest | 由统一版本来源生成，不在多个配置点手工漂移 |

## 4. 历史 G0-G7 阶段账本（已归档）

> 本节只保留早期实施证据，已由第 11 节 R01-R10 账本取代，不得作为当前状态、下一步或完成判定来源。

当前源码分支：`codex/session-turn-latency-state-panel`
当前 HEAD：`533f19cfe10df2a177178b8057d865a62c40736e`；工作树仍包含本轮未提交改动，因此当前包是本地验收候选包，不是可发布包。
当前真实包：`/Users/xie/code/magi-rust-rewrite/target/electron-dist/mac-arm64/Magi.app`
当前包身份：Magi `3.0.51` / Electron `43.4.0` / Chromium `150.0.7871.224` / Browser 协议 `3.5` / buildIdentity `533f19cfe10df2a177178b8057d865a62c40736e` / `gitDirty=true`
当前包组件哈希：`app.asar`=`cd8e86af4951f20019c52982c1b85ff5ebe1987554ae2e1b2f1233831719c024`；Main=`5212e5138484a5462be034dffb6f632ffa19bd326719f80bbc5d6de1e40e8314`；Preload=`8269dda2dc57bcca0f393d919a2afc29a07df32b47093273940ab4008ed46f50`；Worker=`9b1fea1b33520df37f0feb28c33c2cece53c9452497f37d02b8856a287217e8a`；daemon=`c2865d4ebccc53d83433f3cd66bb70c1c9139fe15f103a5b41ca7bad94ccd3ac`；WebWorkbenchShell=`9facbaa38d88cb48fbbd5f200846df91202bce8af3e75b3ff35c19dd10bada8b`；RightPane=`28eaa2d8f10b2483527cd47ff03d5bc0951467f71b8cd9ddb259dc1e502d9ef3`；web=`744215c09d6b29f190859d682d718d90feca35aa3c678d97acda4f1670740df7`
当前 live 运行：daemon `/health` HTTP `200`，runtime epoch `runtime-1789116465948-30600-1`；真实 Electron 为单窗口、单 daemon，当前个人会话 `session-1789093090004-0`。
Desktop 验收入口：http://127.0.0.1:38123/web.html

| 阶段 | 功能点 | 状态 | 完成条件 |
| --- | --- | --- | --- |
| G0 | 目标、架构边界、右栏功能盘点和验收规则 | 已完成 | 本文约束冻结，Browser 与 Code/Image/Terminal/Agent 的职责边界明确。 |
| G1 | Renderer DOM 内真实 webview 首次创建与导航 | 已完成 | 同一 Electron 包创建一级 Browser Tab，注册真实 guest，导航 example.com 并显示 Example Domain。 |
| G2 | 公共右栏、多功能 Tab、生命周期和焦点 | 已完成 | G2.1-G2.7 同一 Electron 构建和真实桌面验收全部通过。 |
| G3 | 导航、输入、刷新、Popup、下载、单页约束 | 已完成 | G3.1-G3.7 在同一 Electron 构建上通过自动化、Rust 回归和真实桌面验收；当前一级 Browser Tab 内完成全链路，不产生子 Tab/额外窗口。 |
| G4 | S4 阶段门 | 已归档，见 R10 | 旧阶段由 R10.4-R10.7 统一收口。 |
| G5 | LLM 浏览器工具、统一协议、DOM/截图/标记 | 已归档，见 R03/R08/R09/R10 | 当前状态只读取第 11 节。 |
| G6 | 视口仿真、故障恢复、升级重启和版本统一 | 已归档，见 R02/R04/R08/R10 | 当前状态只读取第 11 节。 |
| G7 | 清理、全量测试、打包、提交和发布 | 已归档，见 R10 | 当前状态只读取第 11 节；本轮不自动提交或发布。 |

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
| G4.1 | 构建身份与运行时一致性 | 已完成 | G1-G3 的自动化、Rust、daemon、Host、真实 Electron 证据全部绑定同一提交和同一 Electron 包。 |
| G4.2 | 多功能右栏与单页 Browser 约束回归 | 未通过 | 基础同级 Tab 共存已通过，但 Desktop HTML 文件仍同时创建 Code Tab 和 Browser Tab；完成 G4.2.1-G4.2.6 后才可收口。 |
| G4.3 | 会话与任务基础能力回归 | 未开始 | 新建、中文发送、续聊、历史切换、并行、多会话、删除、F5、daemon 重连和 Electron 重启均无消息丢失或会话消失。 |
| G4.4 | G4 阶段门 | 未开始 | G4.1-G4.3 通过同一包真实验收，并形成唯一基线后才进入 G5。 |

#### G4.3 前置恢复阻断

| 功能点 | 状态 | 证据与放行条件 |
| --- | --- | --- |
| accepted event-only 崩溃恢复中的 canonical/sidecar 收敛 | 已完成 | 根因是 accepted event-only 恢复先合并旧 sidecar，随后未按最终 canonical event 重建，导致严格恢复因 item 集合不一致拒绝启动。恢复路径现统一调用 canonical -> sidecar 重建，并对缺少 accepted sidecar 的事件目录直接报错；回归测试覆盖 accepted 后追加 item、完成 Turn、无 session projection 恢复，以及 `SessionStore::from_persisted_parts` 严格校验。`cargo test -p magi-daemon daemon::persistence::tests --lib` 32/32、`cargo test -p magi-session-store --lib` 120/120、`cargo check -p magi-daemon` 通过。下一步必须在修复后新包执行 G4.2/G4.3 真实桌面验收。 |

#### G4.2 子功能账本（2026-09-09）

| 编号 | 功能点 | 状态 | 当前证据与放行条件 |
| --- | --- | --- | --- |
| G4.2.1 | Terminal/Markdown/Code/Image/Diff 同级 Tab | 已完成 | 当前 Electron 包已真实打开并切换 Terminal、`AGENTS.md` Markdown、`Cargo.toml` Code、`docs/images/group.png` Image、`persistence.rs` Diff；均位于唯一 RightPane 顶级 Tab group。 |
| G4.2.2 | Desktop HTML 文件单一路由 | 未验证 | 四个入口已收敛为互斥选择：Electron App Renderer 的 HTML 只请求 Browser，Web/Mobile 和非 HTML 只创建文件视图；`test:browser-navigation`、`test:right-pane`、Web check 和 `git diff --check` 通过，等待新 Electron 包真实复验。 |
| G4.2.3 | 多个 Browser 一级 Tab | 已完成 | 当前包已真实保留并独立切换 HTML 架构页和 `https://example.com/` 两个 Browser 一级 Tab；无 Browser 子 Tab 或额外窗口。 |
| G4.2.4 | Agent 与全部现有内容类型共存 | 未开始 | G4.2.2 通过后，真实创建 Agent 一级 Tab，并与 Browser/Code/Diff/Markdown/Image/Terminal 连续切换。 |
| G4.2.5 | 浮层、焦点、关闭与恢复回归 | 未开始 | G4.2.4 通过后，验证新增菜单、Browser 工具浮层、Composer 焦点、逐类关闭、F5 和 Electron 重启。 |
| G4.2.6 | G4.2 阶段门 | 未开始 | G4.2.1-G4.2.5 使用同一新 Electron 包完成，且自动化、构建身份、真实 UI 和生命周期证据一致。 |

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

### G5 子功能账本（归档，不是当前状态）

| 编号 | 功能点 | 状态 | 完成条件 |
| --- | --- | --- | --- |
| G5.1 | LLM 浏览器工具发现与能力协商 | 未开始 | Desktop Worker 能发现完整 Browser 工具目录，`initialize/initialized` 和协议版本协商成功；Web/Mobile Web 明确不可用而不伪造已连接。 |
| G5.2 | DOM 读取、节点选择与页面操作 | 未开始 | LLM 可读取标题/DOM、选择节点、点击、输入、悬停、滚动；结果进入标准 Item 和消息，不走旁路事件。 |
| G5.3 | 截图、标记与 Artifact 消息链 | 未开始 | 截图、区域标记和 DOM 选择可同时存在；选区 artifact 不是整页，序号/备注/引用持久化，刷新、导航、重启和移动鼠标不丢失。 |
| G5.4 | 控制 Lease、虚拟鼠标与焦点 | 未开始 | AI 接管时虚拟鼠标持续可见；Lease 释放后页面和 Tab 保留且鼠标状态收敛，不抢 Composer 焦点。 |
| G5.5 | 有/无 workspace 能力边界 | 未开始 | 无 workspace 可使用只读和允许的浏览器能力；需要 workspace 的变更操作返回明确状态，不出现无来源的 bridge-runtime 错误。 |
| G5.6 | G5 阶段门 | 未开始 | G5.1-G5.5 通过自动化和真实 Electron LLM 场景，并验证结果进入消息且可恢复。 |

### G6 当前 Browser Tab 视口与恢复（归档设计输入）

- 仅支持宽屏、窄屏、自定义三种用户语义；设备参数只属于当前 Browser Tab。
- 视口调整使用 Chromium 的设备指标能力，不手工缩放页面、不截断页面、不计算外部坐标。
- 拖动调整实时自然生效，不通过整页刷新、不闪烁、不切换其他 Tab 配置。
- 页面响应式内容由浏览器布局自适应；验收检查右侧内容完整、无水平截断。
- Host、Playwright、Chromium 使用统一 manifest/version source。
- 安装、检查更新、卸载、激活、重启提示都返回真实状态；需要重启时明确提示并完成重启恢复。

### G6 子功能账本（归档，不是当前状态）

| 编号 | 功能点 | 状态 | 完成条件 |
| --- | --- | --- | --- |
| G6.1 | 宽屏、窄屏、自定义视口 | 未开始 | 仅支持三种用户语义；设备参数只属于当前 Browser Tab，不进入会话普通布局持久化，不跨 Tab/窗口共享。 |
| G6.2 | Chromium 原生响应式布局 | 未开始 | 通过 Chromium 设备指标改变页面布局，内容自适应、不手工缩放、不截断、不计算外部坐标。 |
| G6.3 | 实时调整与稳定恢复 | 未开始 | 拖动实时生效，不整页刷新、不闪烁、不黑屏；不同大小窗口/Tab 不发生来回变换，刷新和重启后按当前 Tab 规则恢复。 |
| G6.4 | Host/Playwright/Chromium 统一版本 | 未开始 | 从单一 Runtime manifest 生成并校验 Host、Playwright、Chromium 版本，安装、更新、卸载、激活和重启提示状态真实一致。 |
| G6.5 | G6 阶段门 | 未开始 | G6.1-G6.4 在宽屏、窄屏、自定义、刷新、重启和多 Tab 场景通过真实桌面验收。 |

### G7 清理与交付（归档设计输入）

- 删除浏览器 WebContentsView 显示路径、浏览器 setBounds/geometry IPC、截图投影、旧 Overlay 双实现和废弃兼容分支。
- 只保留一套右栏外壳、一套 Browser Tab 内容槽、一套状态协议和一套生命周期状态机。
- 变更检查、Web/Desktop/Rust/Worker 测试、Electron 打包、真实桌面全链路测试全部使用同一提交。
- 通过发布前置校验后才允许提交主分支、打 Tag 或发布 GitHub Release。

### G7 子功能账本（归档，不是当前状态）

| 编号 | 功能点 | 状态 | 完成条件 |
| --- | --- | --- | --- |
| G7.1 | 废弃路径和冗余兼容清理 | 未开始 | 删除 WebContentsView/setBounds/截图投影/旧 Overlay 和双实现；搜索确认无生产引用和孤立协议。 |
| G7.2 | 全量变更检查与测试 | 未开始 | Web/Desktop/Worker/Rust/契约/会话/浏览器/视口/版本测试均使用同一提交通过，`git diff --check` 通过。 |
| G7.3 | Electron 打包与真实桌面全链路 | 未开始 | 使用最终提交构建 Electron 包，完成会话、代码、终端、多代理、浏览器、DOM、标记、视口、升级重启全链路验收。 |
| G7.4 | 发布前置与交付 | 未开始 | release preflight 通过，主分支、Tag、GitHub Release、更新元数据和旧版迁移路径均可验证。 |
| G7.5 | G7 最终阶段门 | 未开始 | G0-G7 全部已完成；未达到前不得宣称项目完成或发布。 |

## 7. 历史自动化与 Electron 验收矩阵（已由 R10 取代）

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
| 2026-09-09 | G4.1 / 构建身份与运行时一致性 | 进行中 -> 已完成 | 提交 `533f19cfe10df2a177178b8057d865a62c40736e` 的工作树干净；同一提交重新打包 `/Users/xie/code/magi-rust-rewrite/target/electron-dist/mac-arm64/Magi.app`。包内 Magi 3.0.51、Electron 43.4.0、Chromium 150.0.7871.224、`gitDirty=false`，daemon/Worker 3.0.51，desktop protocol 3.4；daemon `/health` HTTP 200 且 buildIdentity 与包一致。Desktop 74/74、Worker 55/55、Web golden、浏览器契约（协议 3.4 / 23 命令 / 21 payload）、下载生命周期和 `git diff --check` 均通过。已重启同一 Electron 包及其 daemon。下一步进入 G4.2。 |
| 2026-09-09 | G4.3 前置 / event-only 崩溃恢复 | 未通过 -> 已完成 | 真实重启阻断的根因是 accepted event-only 恢复合并旧 sidecar 后未按最终 canonical event 重建，严格恢复因此拒绝启动。修复为恢复 event-only session 后立即从 canonical turns 重建 sidecar，并对缺失 sidecar 的 accepted event 直接失败；没有放宽校验、删除数据或增加重试。新增回归覆盖 accepted 后追加 item、完成 Turn、无 projection 恢复和严格 `SessionStore::from_persisted_parts`；daemon persistence 32/32、session-store 120/120、`cargo check -p magi-daemon` 通过。修复尚未进入 Electron 包，下一步先重新打包，再执行 G4.2/G4.3 真实验收。 |
| 2026-09-09 | G4.2 / 多功能右栏初次回归 | 进行中 -> 未通过 | 同一包真实打开 Terminal/Markdown/Code/Image/Diff 和两个 Browser 一级 Tab；发现点击 `docs/architecture.html` 同时创建 Code Tab 与 Browser Tab。根因锁定为 Shell、EditsPanel、FileChangeCard、KnowledgePanel 的 HTML 双路由；下一步只执行 G4.2.2 单一路由修复和回归。 |
| 2026-09-09 | G4.2.2 / Desktop HTML 文件单一路由 | 未通过 -> 未验证 | 四个入口统一在 Browser 路由成功时立即返回，Shell 文件树也改为 HTML/Code 互斥分支；新增运行时能力测试，防止普通 Web 或伪造查询参数误报 Electron 能力。专项测试、Svelte 检查和 `git diff --check` 通过；下一步重新打包并真实点击 `docs/architecture.html`。 |
| 2026-09-09 | R01 / guest 创建前安全边界 | 进行中 -> 未验证 | 已新增 `browser-webview-security.ts` 作为可信 App Renderer 的唯一创建前策略，并由 `WindowManager` 的 `will-attach-webview` 在 guest 附加前执行；非法初始 URL/partition 被拒绝，合法入口统一移除 preload/webpreferences 并强制安全参数。Desktop 77/77、Desktop check、浏览器产品验收、契约校验和 `git diff --check` 通过。尚未把本改动打入新 Electron 包，下一步只做同包合法/非法 guest 真实验收，不重复实现。 |
| 2026-09-09 | R01 / guest 创建前安全边界 | 未验证 -> 已完成 | 候选包 `/Users/xie/code/magi-rust-rewrite/target/electron-dist/mac-arm64/Magi.app`，Magi 3.0.51 / Electron 43.4.0 / Chromium 150.0.7871.224 / buildIdentity `533f19cfe10df2a177178b8057d865a62c40736e`。合法 guest 真实导航并显示 `Example Domain`；向 App Renderer 注入错误 partition 和非空白初始 URL 后，CDP target 始终只有原 App Renderer 与合法 webview，Main 日志分别记录两个结构化拒绝原因，合法页面仍可操作。R01 收口，后续不得再增加第二套 guest 安全路径。 |
| 2026-09-09 | R02 / 固定视口显示与仿真状态 | 进行中 -> 未通过 | 候选包 `/Users/xie/code/magi-rust-rewrite/target/electron-dist/mac-arm64/Magi.app`、buildIdentity `533f19cfe10df2a177178b8057d865a62c40736e`。真实 `480x854` 内容槽应用 `1280x800` fixed viewport 后 Electron 仍为 `scale=1`，页面右侧被裁切；创建标记后 Main 快照仍报告 fixed，guest 实际回到 `480x854` / DPR 2，重复提交相同 fixed 因缓存命中而不恢复。下一步只定位第一个清除真实设备仿真的生命周期事件。 |
| 2026-09-09 | R03 / 标记与页面坐标契约 | 进行中 -> 未通过 | 同一候选包真实框选 `Example Domain` 标题并保存备注 `R03 宽屏坐标复现`；Renderer 使用槽位 `480x854` 归一化坐标，Worker 使用页面 `1280x800` viewport 反算，两者在裁切和仿真缩放场景不等价。直接调用创建标记 API 可稳定触发实际 viewport 回到 auto；R02 收口前不新增坐标补丁。 |
| 2026-09-10 | R02.2 / clipped screenshot 根因 | 进行中 -> 已完成 | 在同一 Electron guest 上逐项执行 Worker 的初始化、标记投影和截图动作；`Page.enable`、`Runtime.enable`、`Network.enable`、`Page.getFrameTree`、`Page.createIsolatedWorld`、无 clip 截图和 `set_annotations` 后 fixed viewport 均保持。单独执行带 `clip` 的 `Page.captureScreenshot` 后，真实 guest 从 `1280x800` / DPR 1 立即退回 `480x854` / DPR 2，物理 Target 未变化。根因锁定为 clipped CDP screenshot 清除 Electron 设备仿真，下一步只收敛截图与仿真的唯一实现。 |
| 2026-09-10 | R02.3 / 仿真状态唯一事实源 | 进行中 -> 未通过 | 新候选包已证明 Renderer F5 后新 guest 能在约 180 ms 内自动恢复 fixed `1280x800` / DPR 1，原生命周期阻断已消失；但同一 Host 在 auto 下普通截图 64 ms 成功，执行任意 fixed 设备指标覆盖后普通或 clipped `Page.captureScreenshot` 均稳定在 10 秒超时。对照 Chromium DevTools `DeviceModeModel` 后确认 Magi 唯一仿真提交缺少 `dontSetVisibleSize: true`，CDP 因而会改写由 `<webview>` 内容槽拥有的 guest 物理可见尺寸。下一步只修正该唯一仿真参数，重新打包后复验 fixed 截图、标记、重复提交、auto 和 F5 重绑。 |
| 2026-09-10 | R02.3 / 无污染同包复验 | 未通过 | 完整重启 `app.asar` SHA-256 为 `7320536a064892966df5da23b18dc104d0b462cb912dc31163c9a0608e647d70` 的候选包，验收过程只连接 App Renderer。fixed `1280x800` / DPR 1 提交在 2 ms 内完成；随后 daemon 普通截图在 10002 ms 后返回 `browser_cdp_timeout:Page.captureScreenshot`。失败后才读取 guest，实际仍为 `1280x800` / DPR 1、文档 complete 且 visible。外部 guest CDP 污染、视口覆盖丢失和页面未完成均被排除；下一步只定位 Host 持久 CDP session 与截图命令序列的差异。 |
| 2026-09-10 | R02.3 / Electron 合成层最小复现 | 未通过 | Electron 43.4.0、macOS DPR 2、`480x854` `<webview>`、相同 Page/Runtime 与 fixed `1280x800` / DPR 1 参数下，直属 `BrowserWindow` 的普通截图 48 ms 成功；`BaseWindow -> WebContentsView -> <webview>` 的 auto 和 fixed 截图分别在 5003/5004 ms 超时。两者 guest 的 CSS/raw layout metrics 完全一致，证明问题不在 Worker、daemon、参数或页面，而在多余 App WebContentsView 原生合成层。下一步收敛 Desktop 壳为唯一 BrowserWindow Renderer 并删除对应双轨。 |
| 2026-09-10 | R02.3 / 单层 BrowserWindow 源码收敛 | 进行中 | 源码基线 `533f19cfe10df2a177178b8057d865a62c40736e`，工作树含本轮未提交改动。Desktop 壳已收敛为 `BrowserWindow -> App Renderer -> Browser Tab 内容槽 <webview>`；删除 App `WebContentsView` 的创建、布局、焦点、恢复和销毁双轨，以及临时 Electron 对照诊断脚本。`npm run check --workspace @magi/desktop`、Desktop 78/78 和 `git diff --check` 通过，生产 `window-manager.ts`/`index.ts` 已无 `BaseWindow`、`WebContentsView`、`appView`、`addChildView` 引用。尚未生成并验收包含该改动的新 Electron 包，因此本子项不得标记完成。 |
| 2026-09-10 | R02.3 / 单层 BrowserWindow 同包验收 | 进行中 -> 已完成 | 候选包 `/Users/xie/code/magi-rust-rewrite/target/electron-dist/mac-arm64/Magi.app`，Magi 3.0.51 / Electron 43.4.0 / Chromium 150.0.7871.224，buildIdentity `533f19cfe10df2a177178b8057d865a62c40736e`，`app.asar` SHA-256 `7bc805226ed619b45722d1002bad280841499980bba791ac172f3cf4a7fc8dc3`，隔离状态 `/tmp/magi-r02-browser-window.QsFUTO`。个人会话无工作区下，auto 截图 49 ms、`960x1708`；fixed `1280x800` / DPR 1 提交 3 ms、截图 52 ms 且截图后 guest 仍为 `1280x800` / DPR 1；区域标记创建 160 ms，artifact `256x160`。完整重启同一包后执行无外部 guest CDP 的连续序列：fixed 提交 3 ms、截图 76 ms、区域标记 89 ms、artifact `128x80`、重复 fixed 1 ms、重复截图 50 ms；Renderer F5 后 260 ms 内自动恢复同一 fixed 状态并成功截图 `1280x800`；切回 auto 3 ms，截图 73 ms、`960x1708`，最终 guest 为 `480x854` / DPR 2。不存在 10 秒超时、整页标记 artifact、页面刷新或视口丢失。R02.3 收口，下一步只进入 R02.4。 |
| 2026-09-10 | R02.4 / 内容槽到 Chromium scale | 进行中 -> 已完成 | 候选包 `/Users/xie/code/magi-rust-rewrite/target/electron-dist/mac-arm64/Magi.app`，buildIdentity `533f19cfe10df2a177178b8057d865a62c40736e`，`app.asar` SHA-256 `4a3540d79bf8187de0ccca3d221f2693e828bbf7d0faadf8b84f3d13b8d44691`，隔离状态 `/tmp/magi-r02-scale.amNXYu`。Renderer 仅用 `ResizeObserver` 上报当前 Surface 的瞬时 `displaySize`，Main 通过单一纯函数计算 `min(1, 槽宽/逻辑宽, 槽高/逻辑高)` 并写入同一 `Emulation.setDeviceMetricsOverride.scale`；协议严格绑定 tab/session/navigation/webContents，且不含坐标。真实响应页在 fixed `1280x800` 下分别于 `480x854`、`600x854`、`400x854` 内容槽完整显示 LEFT、RIGHT EDGE 和 VIEWPORT RIGHT；拖动过程中 `webContentsId=2`、Surface、URL、标题和 `navigationRevision=2` 均未变化，无刷新、重建、截断或黑屏；截图仍输出逻辑 `1280x800`，31-46 ms 完成。切回 auto 后同一 `400x854` 槽立即按 CSS media query 自然重排。Desktop 81/81、Desktop/Web check、浏览器 core/product 验收、右栏 golden、协议生成检查和 `git diff --check` 通过。R02.4 收口，下一步只进入 R02.5。 |
| 2026-09-10 | R02.5 / 模式切换与实例隔离 | 进行中 -> 已完成 | 继续使用同一候选包与隔离状态。真实执行 auto、fixed 宽屏 `1280x800`、fixed 窄屏 `390x844`，响应页分别按桌面双栏与手机单栏布局；在视口 Popover 内直接输入 `1000x700` 后无需确认，180 ms debounce 后自动提交并完整显示。导航到延迟 1.5 秒的真实页面时，在加载中提交 `1200x900` 仅 1 ms 返回，导航完成后截图为 `1200x900`，证明最新视口最终生效。右栏折叠再展开后 fixed `1200x900` 自动恢复并可截图；整个 BrowserWindow 从 `1440x928` 缩至 `1200x768` 后内容槽变为 `400x694`，同一 webContents/Surface/导航代次保持，截图仍为逻辑 `1200x900`。同一 Browser Session 的两个一级 Tab 分别保持 `1200x900 desktop` 与 `390x844 mobile`，往返切换不串扰；完整重启同一包后两个 Tab 均恢复 `auto`，fixed 和内容槽尺寸未进入会话持久化。R02.5 收口，下一步只进入 R02.6。 |
| 2026-09-10 | R02.6 / fixed 视口阶段门 | 进行中 -> 已完成 | 最终候选包 `/Users/xie/code/magi-rust-rewrite/target/electron-dist/mac-arm64/Magi.app`，buildIdentity `533f19cfe10df2a177178b8057d865a62c40736e`，`app.asar` SHA-256 `d393ff86819589728e97546a00067088fed72db231fa7a3fc7931d75f1ba96dc`，Worker SHA-256 `630c9c67221df6df31a71ec50bd265a6b7abdcebba8a95cb0de471b85d8c639a`，daemon SHA-256 `9558c2ad957554c8bae9e27812a87c57eccebcf9b1e08c122f1e18963916226a`。真实 fixed `1200x900 / DPR 2` 截图为 `2400x1800`；滚动页 `scrollY=1800` 时创建归一化 `0.2x0.15` 标记，Host 先捕获无副作用的可见 viewport，Rust 内存裁剪得到 `480x270` PNG，创建后 `scrollY` 仍为 `1800`、viewport/DPR 不变。根因同时收敛为标记投影 revision 合并和普通命令不再 abort 运行中 CDP，日志不再出现 `browser_command_cancelled` 或 debugger 重连。`npm run check`、Desktop 81/81、Worker 55/55、magi-api 裁剪单测、`cargo check -p magi-daemon`、browser core/product、RightPane/browser-navigation/desktop-right-pane golden、协议生成与 `git diff --check` 全部通过。R02 全部收口；真实锚点仍把 DPR 2 记录为 1000，作为 R03 坐标契约第一项，不回开 R02。 |
| 2026-09-10 | R03 / 标记、截图与页面坐标契约 | 进行中 -> 已完成 | 最终候选包 `/Users/xie/code/magi-rust-rewrite/target/electron-dist/mac-arm64/Magi.app`，buildIdentity `533f19cfe10df2a177178b8057d865a62c40736e`，`app.asar` SHA-256 `057f93503cbf12130d61bd348ca883affe7445c9decfda703929c1158b56078f`，Worker SHA-256 `35f201b3a500708034d342d57a4a976ebfaffc541f9661cc48cfd81f643e964a`，daemon SHA-256 `6e9b760d8a4f0fefe53181f7660f96ccfa3676536dc20cd2c4da7f684ea31dbc`。Main 快照新增当前 Surface 的 `displaySize + Chromium scale`；Renderer 只在 fixed 内容画布内归一化，留白点击不生成选择。真实 UI 在 `1280x800`、槽 `480x854`、scale `0.375` 下从画布 25% 拖到 50%，锚点精确为 `x/y/width/height=0.25`，artifact `320x200`；元素点击生成真实节点锚点和 `187x38` artifact。fixed `1200x900 / DPR 2`、`scrollY=1800` 的标记锚点记录 `deviceScaleFactorMillis=2000`，artifact `480x270`，完成后滚动仍为 1800。Screenshot 协议已要求 `navigation_revision`；reload 从 revision 16 到 17 后提交旧 revision 返回 HTTP 409，标记数保持不变。显式 normalized clip 与标记共用 daemon 内存裁剪，PNG/JPEG/WebP 三格式像素单测通过；Worker 56/56、Desktop 81/81、Rust Host 协议、browser core/product、协议 Schema、全 workspace check 和 `cargo check -p magi-daemon` 通过。R03 收口，下一步只进入 R04。 |
| 2026-09-10 | R04 / 多窗口 Primary、关闭与接管 | 进行中 -> 已完成 | 候选包 `/Users/xie/code/magi-rust-rewrite/target/electron-dist/mac-arm64/Magi.app`，buildIdentity `533f19cfe10df2a177178b8057d865a62c40736e`，`app.asar` SHA-256 `bbb386360db2e291fddd4273d5091547f098bc8bdcc4f051004bcbd62048efe0`，Worker SHA-256 `aff5464e3981e621043e9d3366c7252759c08c87690f47b09cc8bcd8fbf9e04c`，daemon SHA-256 `741c1e2ff37005b05d8700dcf04bc6c98ae34591e30c317b9fc1035549c7b8b6`。活动 Browser Tab 注册/重绑会提升当前窗口 Surface；旧 Primary revision 前进并清除 Agent cursor/Inspect。新增严格 `primary_surface_closed`：窗口最后一个 Primary 无替代者时 Authority 清除精确 binding、撤销 lease/fence 并转 Suspended；有替代者只发布新 Primary；全局 closeTab 仍以 `closeRecord(record,false)` 释放全部 Surface，不重复事件。Authority 50/50、daemon browser_host 8/8、Desktop 84/84、协议校验和 `git diff --check` 通过。真实包中第二个 Tab 从 Suspended 恢复为独立 Surface 并成为活动 Tab；关闭后 Authority 仅保留第一个 `SCROLL` Tab，其 Surface、URL 与 webview 保持不变。产品当前只暴露一个 BrowserWindow；同一逻辑 Tab 的多窗口仲裁由 Registry/Authority 状态机直接验证。R04 收口，下一步只进入 R08。 |
| 2026-09-10 | R08 / Worker 调用链与恢复矩阵 | 进行中 -> 已完成 | 使用 R04 同一候选包。源码核对确认 Authority/BrowserHostClient/Desktop Control/Automation Worker/Main CDP/guest 为唯一工具链；Main 生命周期命令与 Worker 页面算法职责不重叠。Worker 显式重启在 273 ms 内完成，新 workerEpoch ready，Surface `surface-586e...`、URL `http://127.0.0.1:38999/scroll` 和 navigation revision 18 不变，截图 `960x1708` 成功。终止 daemon PID 53240 后 ProcessSupervisor 自动拉起 PID 53580，新 runtimeEpoch `runtime-1789007989551-53580-1`、Host ready；新 daemon 内 connection generation 从 1 重新建立，原 Surface/URL/revision 保留，截图 99 ms 成功。Desktop 84/84、Worker 56/56、Authority 50/50、daemon browser_host 8/8 覆盖重绑 gate、资源队列、取消、超时、旧结果隔离、guest/App Renderer/Desktop 生命周期。最终设计已写入实际链路、五层身份和恢复矩阵。R08 收口，下一步只进入 R05。 |

| 2026-09-10 | R05.2-R05.3 / 浮层锚点与生命周期 | 进行中 -> 已完成 | 同一真实 Electron 包确认 tooltip 不再覆盖 viewport/history anchor；viewport、标记历史、右栏新增菜单、标记选择层和备注面板均吸附到各自触发控件或内容槽，且位于 webview 上方。两个 Browser Tab、Terminal、右栏折叠/展开均能关闭非活动浮层。专项 Web 检查和 `git diff --check` 通过。 |
| 2026-09-10 | R05.4 / guest 与 Composer 焦点 | 进行中 -> 已完成 | 首个样本因调试客户端默认 viewport 将 Renderer 仿真为 800×600，点击点实际被右栏 overlay 覆盖，排除为无效样本。使用 `defaultViewport:null` 恢复真实 1440×896 窗口后，网页→Composer→网页交叉输入严格隔离；800×600 下点击真实可见 Composer 区域也通过。Esc 能关闭备注面板并恢复触发按钮焦点。下一步进入 R05.5 同包阶段门。 |
| 2026-09-10 | R05 / 浮层与焦点协议阶段门 | 进行中 -> 已完成 | 候选包 `/Users/xie/code/magi-rust-rewrite/target/electron-dist/mac-arm64/Magi.app`，buildIdentity `533f19cfe10df2a177178b8057d865a62c40736e`；`app.asar`/Worker/daemon SHA-256 分别为 `bbb386...efe0`、`aff546...e04c`、`741c1e...b8b6`，Web `RightPane-DIPVA2Fz.js` 为 `c655cf...30d7`。真实 tooltip、viewport/标记历史/新增菜单、标记选择与备注、双 Browser Tab、Terminal、折叠/恢复、Esc、网页/Composer 交叉焦点均通过。根级 check、Web 全量、Desktop 84/84、browser core/product、协议检查和 `git diff --check` 全部通过。下一步只进入 R06。 |
| 2026-09-10 | R06.1 / Popup 决策链核对 | 进行中 -> 未通过 | `setWindowOpenHandler` 当前把所有可解析 HTTP(S) 请求都复用到当前页，未区分命名窗口、opener 或独立窗口特性；`popup_blocked` 只有 URL、没有 reason，Renderer 也不展示该事件。Electron 43 的 `HandlerDetails` 只能提供 url/frameName/features/disposition/referrer/postBody，下一步基于这些可证明字段建立唯一决策函数和类型化失败原因。 |
| 2026-09-10 | R06.1 / Popup 唯一决策链 | 未通过 -> 已完成 | 新增纯 `decideBrowserPopup`：普通 `_blank` 链接和 POST 表单允许；脚本窗口仅显式 `noopener/noreferrer` 允许；命名窗口、opener、独立窗口特性、空白页、非 HTTP(S) 与无效地址分别返回稳定 reason。reason 已进入 TS/Rust 协议 3.5、daemon 事件和 Renderer Top Layer 可见错误条。Desktop 90/90、根级 check、Rust host protocol、daemon check、browser core/product 和协议校验通过。下一步构建同包真实验收。 |
| 2026-09-10 | R06.2-R06.4 / Popup 真实矩阵首轮 | 部分通过 | 普通 `_blank`、显式 noopener 和 POST 均在同一 Target/Surface/WebContents 内完成，POST body=`query=magi`；命名、about:blank、mailto 均保留原页并显示类型化中文原因。无名称 `window.open(url)` 被 Electron 报告为可导航 disposition，旧判断误放行至 `/should-not-open`。下一步改用空 `frameName` + 未显式 noopener 的可证明条件阻止，不再依赖 disposition 推断。 |
| 2026-09-10 | R06.4-R06.5 / 阻止原因与单页约束复验 | 未通过 -> 已完成 | 新包中无名称 opener 与独立 features 请求均保留原 URL/revision，分别返回 `opener_required`、`separate_window_features`；连同 `named_window`、`script_blank_window`、`unsupported_protocol` 均显示中文 Top Layer 原因。全部场景保持同一 Surface、Target、`webContentsId=2`，RightPane Browser Tab=2、webview=2、page Target=1，无新增页面容器。 |
| 2026-09-10 | R06 / Popup 支持范围阶段门 | 进行中 -> 已完成 | 候选包 `/Users/xie/code/magi-rust-rewrite/target/electron-dist/mac-arm64/Magi.app`，buildIdentity `533f19cf...d865a62c40736e`；`app.asar`/Worker/daemon/Web RightPane SHA-256 为 `923f38...f934d`、`083b98...88692`、`bfabf4...10493`、`460b70...15ab2`。运行组件协议 3.5 ready/compatible。Web 全量、Desktop 90/90、Worker 56/56、Authority 50/50、daemon browser_host 8/8、browser core/product、协议、rustfmt 和 `git diff --check` 全部通过。最终矩阵已回写架构文档，下一步只进入 R07。 |
| 2026-09-10 | R07.1 / 多类型生命周期核对 | 进行中 -> 已完成 | Browser 非活动 Tab 保留 guest；Terminal 视图切换会断开 WebSocket，但 Rust PTY 由 manager 保活并缓存 2 MiB，显式关 Tab/会话才终止；Code/Image 可重取，Agent 由任务事实恢复。真实长命令切换 Browser 后回 Terminal 能恢复输出；Renderer F5 则丢 Terminal Tab，但同 ID 仍可重连 PTY，确认 UI 身份与资源生命周期分裂。修复为 Desktop 每窗口 sessionStorage 恢复非 Browser Tab，Web 继续 localStorage；终端快照改为历史输出先于终态。 |
| 2026-09-10 | R07.2-R07.4 / 多类型运行态复验 | 进行中 | F5 后同一 Terminal Tab/PTY/活动态恢复并收到长命令前后输出；显式关闭 Terminal 后延迟 marker 未生成，PTY 已终止。两个 Browser guest 在 Terminal 活动时保留且隐藏；关闭第二个 Browser Tab 后 host/Target 精确减少 1。Browser、Image、Terminal 往返切换只有当前内容可见；跨个人会话后切回原会话，同一 Terminal ID 恢复完整长命令输出。下一步验证完整 Electron 重启与最终清理。 |
| 2026-09-10 | R07.2-R07.5 / 窗口关闭与完整重启 | 进行中 -> 已完成 | 真实 `window.close()` 触发 Electron 正常关闭事务后，Terminal 延迟 marker 未生成，Magi/daemon 均退出；重新启动同包后 Terminal Tab 不伪恢复，BrowserAuthority 恢复唯一 Browser URL，新建 Surface `surface-6cd9...5458` 和 Target `91D593...D26A`。强制 kill PID 绕过 before-quit，不作为正常关闭证据。 |
| 2026-09-10 | R07 / Tab、终端与资源生命周期阶段门 | 进行中 -> 已完成 | 候选包 `/Users/xie/code/magi-rust-rewrite/target/electron-dist/mac-arm64/Magi.app`，buildIdentity `533f19cf...d865a62c40736e`；`app.asar`/Worker/daemon/Web RightPane SHA-256 为 `923f38...f934d`、`083b98...88692`、`5c6d5d...3831c`、`725e3a...35611`。Web 全量、Desktop 90/90、Worker 56/56、Terminal 11 项、Authority 50/50、daemon browser_host 8/8、browser core/product、协议、rustfmt 和 `git diff --check` 全部通过。生命周期矩阵已回写架构文档，下一步只进入 R09。 |
| 2026-09-10 | R09.1-R09.2 / 跨端权限核对 | 进行中 -> 未通过 | Browser HTTP 路由优先信任客户端声明的 `clientPlatform=desktop`，Web 可自行提升；App Server 协商了 `desktopBrowserSurface/browserTools` 但浏览器方法未校验连接能力；普通 Web Turn 未携带 server-only 浏览器禁用事实，设置页仍允许 Web 修改浏览器开关和回收资源。下一步统一以服务端观测的非 tunnel Electron 请求授权，并把该事实写入当前 Turn 的拒绝工具集。 |
| 2026-09-10 | R09.1-R09.4 / Desktop token 与跨端降级 | 未通过 -> 已完成 | Main 复用每进程随机 controlToken，只为 `persist:magi-app` 的 daemon HTTP/WebSocket 注入 Header；daemon 与当前 Desktop connection token、Magi/Electron UA、非 tunnel 三项同时匹配。App Renderer 能力=true、App Server 工具=26、截图 200；同 UA curl 和 Browser guest 无 token 时能力=false、App Server `-32600`、写请求 501。guest 内 `window.magiDesktop=null`、webview=0，仅保留记录/外部入口。Web Turn server-only 字段不可由 JSON 伪造并拒绝全部 Browser 工具。 |
| 2026-09-10 | R09 / Web 与 Mobile 权限阶段门 | 进行中 -> 已完成 | 候选包 `/Users/xie/code/magi-rust-rewrite/target/electron-dist/mac-arm64/Magi.app`，buildIdentity `533f19cf...d865a62c40736e`；`app.asar`/Worker/daemon/RightPane/SettingsPanel SHA-256 为 `a4e522...0ba90`、`083b98...88692`、`d4a024...eb3ef`、`86437f...db356`、`39d19a...a10c9`。可信 Desktop 截图 200、App Server 工具 26；无 token Web/guest 降级通过。Web 全量、Desktop 91/91、Worker 56/56、Browser 17 项、App Server 10 项及全部验收检查通过。下一步只进入 R10。 |
| 2026-09-10 | R10.1-R10.3 / 文档、清理与构建身份 | 进行中 -> 已完成 | 最终设计修正为 BrowserWindow/Top Layer/Desktop token，旧 G0-G7 与第 10 节明确归档。生产扫描无 WebContentsView/原生 bounds/截图投影/Tauri/CEF/独立 Browser Runtime/静态全局 anchor 路径；仅测试与发行守卫保留禁止词。Electron 单一发行边界、26 项工具目录、Browser 协议 3.5、App Server 生成类型通过。当前未提交包 manifest 明确 `gitDirty=true`，验收以组件 SHA-256 绑定，不伪称干净提交。 |
| 2026-09-10 | R10.4 / Desktop HTML 单一路由 | 进行中 -> 未通过 | 工作区与会话注册成功，`magi:previewFile` 已 `defaultPrevented=true`，但 12 秒内 Browser/Code Tab 均未创建且无错误。根因是 Shell 的互斥 Browser 请求只携带 filepath，而 RightPane 仍要求当前已有同一 HTML Code Tab；互斥后该条件永远不成立。下一步把完整 workspace/session 绑定放入一次性请求并直接创建 Browser Tab。 |
| 2026-09-10 | R10.4 / HTML 与多功能右栏复验 | 未通过 -> 已完成 | 新请求契约携带完整 workspace/session 绑定，RightPane 不再依赖 Code Tab。临时真实工作区中打开 `docs/architecture.html` 只生成一个 Browser Tab，标题与 daemon 文件站点 URL 正确，`codeArchitecture=false`、webview/Target=1。Browser、Markdown README、Image、Terminal 四类一级 Tab 共存切换，只有当前内容可见。 |
| 2026-09-10 | R10.5 / 真实仓库会话投影启动 | 未开始 -> 未通过 | 使用已有真实仓库工作区重启新包时，daemon 拒绝启动：`session projection 的 canonical event 游标超前: 51 > 0`，来源 `.magi/session-projections/session-1787242768063-0.json`。该状态不能删除或忽略；下一步核对 canonical/sidecar/projection 三者恢复顺序和权威修复规则。 |
| 2026-09-10 | R10.5 / 会话权威持久化与真实应用恢复 | 未通过 -> 已完成 | 根因是 project projection 与 daemon event 分属不同生命周期、加载器未验证 workspace 归属且未知 ID 会回落到全局路径；并发 sidecar flush 还缺少 canonical commit 锁。现已用项目本地稳定 identity、严格路径归属、可恢复 event 导入事务、workspace 先行落盘、canonical/sidecar 同代快照和受管 daemon 退出恢复收敛。真实数据 35 个 session 已安全恢复；同包完成首发、两轮续聊、双会话并行、切换、删除、Renderer F5、SIGKILL daemon 约 1 秒恢复、完整 Electron 重启和真实仓库 13 个历史会话 UI 复验。自动化最终为 workspace 25、session-store 121、API 605、daemon 124、Desktop 92 全通过。下一步只进入 R10.6。 |
| 2026-09-10 | R10.6-R10.7 / 续跑计划冻结 | 进行中 -> 已完成 | 第 11.10 节已补齐逐项执行表；R10.6.1-R10.6.9、R10.7.1-R10.7.5 已按顺序完成。本地候选包与当前源码、协议和测试证据已重新对齐；由于工作树仍有未提交改动，当前仅作为本地验收候选，不宣称可发布。 |
| 2026-09-10 | R10.6.1 / 候选包身份与 Browser 基线 | 进行中 -> 已完成 | 冻结 `app.asar`/daemon/Worker 哈希 `dce870...1bee`、`3d5f9d...eeb4`、`1777d6...4d53`。真实 Magi 仅一个主窗口、一个 App page、一个 webview Target、一个 RightPane Browser 一级 Tab；Host ready、协议 3.5 兼容、generation=1，Authority Session/Tab/Surface ready。下一步只执行 R10.6.2。 |
| 2026-09-10 | R10.6.2 / 导航、响应态、Popup 与下载 | 进行中 -> 已完成 | 同一候选包、同一 Browser 一级 Tab 完成地址导航、302 重定向、后退、前进、刷新、60 秒流式慢加载、Stop、DNS 失败、Retry 与成功恢复；慢加载真实窗口截图显示 partial HTML 和 Stop 控件，无黑屏。普通 `_blank` 复用当前 Target；命名窗口保留原页并显示类型化中文原因，始终只有 1 个 webview Target/1 个 RightPane Browser Tab。dialog 后页面继续可操作；下载 progressing/completed/cancelled/interrupted 全部显示真实状态，取消/中断无残留文件，完成文件为私有目录内 524288 字节。下一步只执行 R10.6.3。 |
| 2026-09-10 | R10.6.3 / 多 Tab 视口隔离首轮 | 进行中 -> 未通过 | auto `480/600/660`、宽屏 `1280×800`、窄屏 `390×844`、自定义 `700×700 -> 760×700` 均由 Chromium 生效，Target、页面内存标记和 navigation revision 不变，右边界无截断；两个 Browser Tab 也分别保存 `760×700` 与 `390×844`。但从第一 Tab 切回第二 Tab 的紧邻切换会被旧活动 Tab 状态覆盖，20 秒内 Main/Authority 仍停留第一 Tab。当前只采集 RightPane store -> activate -> Authority snapshot 的覆盖事件，未确定首个错误事件前不修改视口、Renderer 或 Main。 |
| 2026-09-10 | R10.6.3 / 视口与快速多 Tab 切换 | 未通过 -> 已完成 | 唯一根因是旧激活请求的 Authority 回调无条件用 `revealTabId` 重写 RightPane，使较新的用户选择丢失。现统一在 Authority 读取、Main 激活、Surface 就绪和 Authority 回调各异步边界校验最新目标，过期请求只释放自身单飞槽。专项 golden、Svelte 0 error/0 warning 与 `git diff --check` 通过；新包上快速 A→B→A 最终均由最后意图获胜，两个 Tab 分别保持 `760×700`、`390×844`。同包重验 auto `480/600/660`、宽屏、窄屏、自定义实时输入，Target/页面内存/navigation revision 不变、20 组尺寸采样稳定；Electron 重启后视口恢复 auto，未跨 Tab 或应用持久化。 |
| 2026-09-10 | R10.6.2 / 新包回归 | 已完成 -> 已完成 | WebWorkbenchShell 变更后在新目录包重新执行完整矩阵：地址、重定向、历史、刷新、Popup、命名窗口阻止、dialog、慢加载/Stop、DNS 失败/Retry/恢复，以及四种下载状态全部通过，最终仍为 1 个 Browser Tab/1 个 Target。新包 WebWorkbenchShell/RightPane SHA-256 为 `fc5368...d564`、`b3d480...349`。 |
| 2026-09-10 | R10.6.4 / 截图、区域标记与消息 Artifact | 进行中 -> 已完成 | 同一修复包中，工具栏截图直接进入 Composer，解码为 `960×1708` PNG；新区域标记序号 2，选区 `x=.15,y=.12,w=.47,h=.32`，artifact 为 `451×547` PNG 而非整页。备注从“R10.6.4 区域标记”编辑为“R10.6.4 标记已编辑”，网页覆盖层、Composer chip 同步更新。真实发送后，对话消息同时显示序号 2、备注和截图；点击标记预览从带 `sessionId` 的 artifact URL 成功解码 `451×547`。Renderer F5、完整 Electron 重启后消息、预览、网页标记与历史 Top Layer 均恢复；历史层打开前后内容槽保持 `480×854`，没有挤压页面。个人会话无 workspace 全链路通过。 |
| 2026-09-10 | R10.6.4 / 重启后历史标记复用 | 已完成 -> 未通过 | 完整 Electron 重启后同 URL 的标记、序号、截图 artifact 与历史 Top Layer 都存在，但 Authority 状态变为 `stale`；历史项仍可点击，`InputArea` 却只接受 `active`，事件已发出但 Composer 无引用，形成无响应按钮。唯一根因是历史列表与 Composer 对可引用状态的契约不一致；当前只统一 `active/stale` 历史 artifact 的引用语义并回归，不修改标记坐标或恢复生命周期。 |
| 2026-09-10 | R10.6.4 / 历史标记状态契约复验 | 未通过 -> 已完成 | 新增唯一 `isReferenceableBrowserAnnotation` 契约：active/stale/resolved 可引用，deleted 不可引用；Browser 历史与 Composer 共用。agent-api golden、Svelte 0 error/0 warning、`git diff --check` 通过。新目录包完整重启后序号 2 标记为 stale，点击历史项成功恢复 Composer chip；此前消息中的 `451×547` artifact 继续可预览，网页覆盖层和序号仍在。当前 WebWorkbenchShell/RightPane/web SHA-256 为 `9038be...6c8f`、`fbda32...ce15`、`36cae5...7170`。下一步只进入 R10.6.5。 |
| 2026-09-11 | R10.6.5 / DOM、标记与截图并存首轮 | 进行中 -> 未通过 | Composer 同时存在序号 2 标记、DOM chip 和截图；选中后移动鼠标、进入/取消标记模式均未清除节点，发送及 F5 后三类上下文仍在标准 user_message。持久化事实却显示 DOM 选中了 Magi 内部 `#magi-browser-annotations` 全视口 DIV，而非测试页 `DOM selection target`。唯一根因范围锁定为页面标记覆盖层污染 Chromium Inspect 命中链；当前只建立内部节点排除与真实底层页面命中规则，不修改 Composer 保留逻辑。 |
| 2026-09-11 | R10.6.5 / DOM、标记与截图并存修复 | 未通过 -> 已完成 | 根因是 Main 两条 `DOM.getNodeForLocation` 均设置 `ignorePointerEventsNone:true`，强制命中 Magi 的透明标记宿主。悬停与点击统一改为 `false`，遵循用户真实指针命中语义；Desktop 94/94、Desktop/Web check、agent-api golden 与 `git diff --check` 通过。新包 `app.asar` SHA-256 `756d3e...eef8` 上真实选中 `P[data-testid="dom-target"]`，textExcerpt=`DOM selection target`、outerHTML/attributes 正确。移动鼠标、进入/取消标记模式后节点 chip 保留；序号 2 标记、P 节点、`960×1708` 截图同时进入 Composer 和 canonical user_message，F5 后三类仍在。刷新只精确清除未发送的旧节点草稿，已发送消息保持。 |
| 2026-09-11 | R10.6.6 / 焦点、Top Layer 与浅色主题 | 进行中 -> 已完成 | 同一真实 Electron 包在 guest WebContents 与 App Renderer 的独立 CDP 输入通道交叉输入：焦点 `WEBVIEW/#name -> Composer -> WEBVIEW/#name`，`WEB_CDP_RETURN` 与 `COMPOSER_CDP` 互不串入。Terminal 激活时 App 焦点为 xterm helper textarea，Browser guest 隐藏但保活，切回后恢复且关闭 Terminal 不影响 Browser。10 个工具提示全部从 toolbar bottom=`74` 向下显示并落在 1440×928 窗口；视口、标记历史和新增面板 Top Layer 均在窗口内，打开前后 Browser 内容槽严格保持 `480×854`，切换 Terminal 后全部收敛。通过真实主题入口切到 builtin.light 后，启用的标记保存按钮为 `#2563eb/#fff`、opacity=1、对比度 `5.17:1`，随后恢复 system。系统当时锁屏，未使用 CUA 注入；上述证据来自同一可见 Electron 的真实 App/guest Target，而非单元模拟。 |
| 2026-09-11 | R10.6.7 / LLM 工具、Lease 与页面保留 | 进行中 -> 已完成 | 可信 App Renderer 通过 `/api/app-server` 完成 initialize/initialized，协议 1.0、Host ready，发现 26 项 Browser 工具。Magi LLM 真实产生并完成 `browser_snapshot`、`browser_click_at(x=450,y=750)`、`browser_evaluate(document.title)` canonical Tool Item；click_at 结果 `clicked=true`。写工具期间观测到 `agentOccupied=true` 与页面虚拟鼠标持续显示；停止后 `occupied=false`、controlMode=user、cursor=none。`gpt-5.6-luna/sol` 通过本地代理时连续缺失终止 SSE，工具 Item 本身仍完成；切换独立 `deepseek-v4-flash` 后同一真实流程完整结束，assistant_text 精确为 `Magi Browser Acceptance`。任务结束保持 1 个 Browser Tab/1 个 Target，原 URL、标题和页面均未关闭。 |
| 2026-09-11 | R10.6.8 / Worker 与 daemon 恢复 | 进行中 | 显式 Worker 重启后 workerEpoch 更新，原 Target/Surface/URL/revision/2 个标记保持，截图 `200 image/png`；SIGKILL daemon PID 4670 后自动恢复 PID 7064 与新 runtimeEpoch，App/guest Target 和 Surface 保持，截图正常。下一步执行 Renderer F5。 |
| 2026-09-11 | R10.6.8 / Renderer F5 首轮 | 进行中 -> 未通过 | F5 后 App Target 保持，guest 从 Target `79D2...`/webContents 2 重建为 `5E6B...`/webContents 4，逻辑 Surface `surface-8d85...`、URL、2 个标记和已发送消息恢复；但连续截图返回 `409 browser_cdp_timeout:Page.captureScreenshot`。Authority/UI ready 与真实新 guest 自动化不可用发生分裂。当前只采集 debugger session、viewport commit、primary_changed/Worker rebind 顺序，未确定首错前不增加等待或重试。 |
| 2026-09-11 | R10.6.8 / Renderer 首帧与完整恢复 | 未通过 -> 已完成 | 根因是 F5 后 UI/文档/视口 ready 早于新 guest 首个 compositor frame；过早 `Page.captureScreenshot` 会卡住 Chromium Target。对照实验在同一 ready 状态先等待两次 guest `requestAnimationFrame` 后立即 `200`。结构修复为受管 guest `setBackgroundThrottling(false)`，截图在同一 Surface lane、独立 world 内等待两个真实帧后才 capture；没有固定成功等待或截图旁路。修复包连续 5 次 F5 后立即截图全部 `200`、114-272 ms，其中 cycle 1/4/5 发起时新 Target 尚未出现在外部调试目录；每次保持 URL、2 个标记和消息三类上下文。完整 Electron 重启后新 desktopEpoch、Surface、Target、daemon/worker 全 ready，截图 `200`，页面与标题回复保留。Desktop 95/95、check 和 `git diff --check` 通过。 |
| 2026-09-11 | R10.6.9 / 最终 Browser 同包阶段门 | 进行中 -> 已完成 | 最终包 `app.asar` SHA-256 `995a8b...3dac`。根级 check、Desktop 95/95、Worker 57/57、Web 全量、Authority 50/50、API Browser 37/37、daemon browser_host 8/8、协议 3.5（23 命令/21 payload）、26 项工具目录、browser core/product 和 `git diff --check` 全通过。同包 live 回归：导航/Popup 单 Tab；宽屏 Chromium 截图 `1280×800` 后恢复 auto；2 个标记存在时 Inspect 命中真实 P；Composer 截图 `960×1708`、artifact `200 image/png`；Magi `browser_evaluate(document.title)` canonical Tool Item completed，assistant 回复 `Magi Browser Acceptance`，最终 running/occupied=false，Tab/URL 保留。R10.6 关闭，下一步只进入 R10.7。 |
| 2026-09-11 | R10.7.1 / 全量源码与协议首轮 | 进行中 -> 未通过 | 根级 `npm test`、Rust workspace check、rustfmt、release guard、browser core/download 与 `git diff --check` 已通过；`cargo test --workspace` 在 `magi-skill-runtime` 17 项中失败 3 项：`builtin_requests_succeed_when_allowed_by_skill_policy`、`mixed_builtin_and_bridge_plan_dispatches_correctly`、`builtin_dispatch_uses_runtime_invocation_policy`，实际均为 Rejected。当前只复现并定位 Skill 调度权限事实，不进入构建阶段。 |

## 10. 历史执行批次（已冻结）

本节记录 2026-09-09 的旧执行批次，已经冻结，不再提供当前状态或下一步。当前唯一入口是第 11 节 R01-R10 账本；原 G4.2.2 HTML 单一路由只在 R10 最终候选包中复验，不恢复旧 G4 执行流。

### 10.0 本轮执行批次（2026-09-09）

本轮目标固定为完成 G4 阶段门，不扩展目标、不回头重写已收口的 G1-G3：

| 顺序 | 功能点 | 当前状态 | 唯一动作 | 放行条件 |
| --- | --- | --- | --- | --- |
| 1 | G4.1 构建身份与运行时一致性 | 已完成 | 已使用提交 `533f19cf` 构建并核对 Electron、daemon、Renderer、Chromium 运行身份 | 同一构建身份的自动化、daemon、Host、Electron 证据齐全，`gitDirty=false` |
| 2 | G4.2 多功能右栏与单页 Browser 约束回归 | 未通过 | 当前只修复 G4.2.2 Desktop HTML 文件双路由；通过后再执行 Agent、浮层、焦点、关闭与恢复 | G4.2.1-G4.2.6 每项均有同一新包证据；Browser 无子 Tab、额外窗口或外壳覆盖 |
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

## 11. R01-R10 设计审阅收敛账本（2026-09-09）

本节是当前唯一执行状态源。审阅依据为 `docs/browser-runtime-design-review.md`；最终架构只回写 `docs/browser-runtime-design.md`。风险项必须先真实复现或通过源码事实证实，不能把审阅推断直接当作故障。R02、R04、R06、R09 的产品规则已根据用户此前明确要求定稿，不再等待重复确认，也不允许实现时引入第二套行为。

### 11.1 总账本

| 顺序 | 编号 | 功能点 | 当前状态 | 当前证据与下一步 |
| --- | --- | --- | --- | --- |
| 1 | R02 | 固定视口 API 与显示规则 | 已完成 | R02.1-R02.6 已通过自动化和同一 Electron 包真实验收；固定视口、Chromium scale、模式切换、实例隔离、截图、滚动和标记前后均保持稳定，后续不得回开本阶段。 |
| 2 | R03 | 标记、截图与页面坐标契约 | 已完成 | 槽位局部、页面 CSS viewport、文档 scroll 和图片像素四套坐标已形成单向契约；scale、DPR、滚动、留白、区域/元素、Artifact 和导航代次均通过同一 Electron 包验证。 |
| 3 | R04 | 多窗口 Primary、关闭与接管语义 | 已完成 | 活动窗口提升、旧控制态清理、全局唯一 Primary、lease/fence 撤销、窗口关闭替代提升、最后 Primary 关闭和全局 Tab 关闭已形成唯一状态机并通过自动化与真实包验证。 |
| 4 | R06 | Popup 支持范围 | 已完成 | R06.1-R06.6 已完成；允许的 `_blank`/noopener/POST 保留当前页语义，不支持的 opener/命名/独立窗口/空白页/协议均返回类型化 reason；无子 Tab、额外 Target 或窗口。 |
| 5 | R09 | Web 远程控制权限 | 已完成 | R09.1-R09.5 已完成；随机 Desktop token、App Server 协商、Browser 写路由与每 Turn 工具目录形成同一权限边界，Web/Mobile/guest/tunnel 均无法提升。 |
| 6 | R01 | guest 创建前安全边界 | 已完成 | 新候选包真实创建合法 Browser guest 并导航到 `https://example.com/`，页面、标题和工具栏正常；随后通过 App Renderer CDP 分别注入 `persist:default + about:blank` 与 `magi-browser-* + https://example.org/`，两次 target 总数均保持 2（App Renderer + 原合法 webview），Main 分别记录 `browser_webview_partition_invalid`、`browser_webview_initial_url_invalid`，原页面不受影响。Desktop 77/77、Desktop check、浏览器产品验收和契约校验通过。 |
| 7 | R08 | Worker 与恢复协议职责 | 已完成 | 实际调用链、Main/Worker 职责、五层身份、取消/超时和 daemon/Worker/guest/App Renderer/Desktop 恢复矩阵已同步设计，并通过自动化及真实 daemon/Worker 重启验收。 |
| 8 | R05 | 浮层与焦点协议 | 已完成 | R05.1-R05.5 已使用同一 Electron 包完成锚点所有权、Top Layer 生命周期、切 Tab/折叠清理、Esc 与网页/Composer 焦点验收；自动化和包身份见第 9 节，后续不得回开本阶段。 |
| 9 | R07 | Tab、终端与资源生命周期 | 已完成 | R07.1-R07.6 已完成；Browser、Terminal、Agent、Code/Image 的切换、F5、会话、关闭和重启边界已形成唯一矩阵并通过同包真实验收。 |
| 10 | R10 | 规范、执行账本和验收证据一致性 | 进行中 | 本节已建立唯一状态账本；最终须同步设计、静态约束、运行测试和同一候选 Electron 包证据，并复验既有 G4.2.2 HTML 单一路由。 |

### 11.2 已定稿产品规则

| 规则 | 唯一产品行为 | 状态 |
| --- | --- | --- |
| R02 固定视口显示 | `auto` 直接使用内容槽物理尺寸；窄屏/宽屏/自定义与截图统一使用当前 Surface 的同一个持久 CDP session。固定视口调用 `Emulation.setDeviceMetricsOverride`，切回 `auto` 调用 `Emulation.clearDeviceMetricsOverride`；Worker 无权写入或清理设备指标。逻辑 viewport 大于槽位时只使用 Chromium 的 `scale` 完整容纳设备画布，禁止 CSS transform、截图缩放或宿主裁切。 | 已确认 |
| R04 多窗口 Primary/关闭/接管 | 一个会话 Browser Tab 是一个全局逻辑 Tab，每个窗口拥有独立 Surface 和 Surface 级视口；当前窗口激活 Tab 或 Agent 显式接管时原子提升 Primary。只有 Primary 发布权威导航；切换 Primary 立即撤销旧 lease 并以 stale binding 结束旧命令。关闭窗口只释放该窗口 Surface；点击 Tab 关闭是全局关闭逻辑 Tab 及全部 Surface。 | 已确认 |
| R06 Popup 范围 | 继续禁止 Browser 子 Tab 和额外窗口。普通 HTTP(S) 新窗口链接与带 `postBody` 的 `_blank` 表单在当前一级 Tab 中保留请求语义复用；依赖 opener/postMessage、命名窗口、OAuth popup、`about:blank` 脚本窗口和非 HTTP(S) 协议明确阻止并显示原因，不伪装成功。 | 已确认 |
| R09 Web 远程控制 | Web/Mobile 只展示 URL、状态、Artifact 和系统浏览器入口，不允许其会话 Agent 静默接管另一台 Desktop Host。Desktop 浏览器工具只有 Electron App Renderer 发起且绑定当前 Desktop Host 时可用；未来远程接管必须另设用户显式授权能力，不作为当前兼容分支。 | 已确认 |

### 11.3 固定执行顺序与阶段门

1. R01 已完成；R02/R03 已取得真实失败证据，当前只处理 R02，不跨阶段修改其他职责。
2. R02 完成后处理 R03；两项必须共用同一套真实 Chromium 仿真和坐标事实，不允许 Renderer、Main、Worker 各自推导不同几何状态。
3. 核对并收口 R04/R08 的身份、租约、epoch、revision、取消和恢复链。
4. 按定稿规则收敛 R05、R06、R07、R09，删除与唯一规则冲突的实现、断言和文档。
5. 完成 R10：回写最终设计、运行相关和全量测试、构建同一候选 Electron 包、逐项真实验收，并复验 G4.2/G4.3。

每完成一个编号，必须立即把 11.1 的状态改为“已完成”，在第 9 节追加源码身份、命令结果、候选包路径和真实操作证据。验证失败改为“未通过”，记录事件序列、唯一根因和下一次结构性修复；没有新证据不得重复改同一职责。

### 11.4 当前锁定执行卡：R02 固定视口

R02 已完成，本节保留为冻结的历史执行卡，不再作为当前入口。

| 子项 | 交付物 | 状态 | 完成证据 |
| --- | --- | --- | --- |
| R02.1 | 真实故障复现与状态采样 | 已完成 | 同一候选 Electron 包中，槽位 `480x854`、fixed viewport `1280x800`、`scale=1` 时右侧被裁切；创建标记后 Main 仍记录 fixed，guest 实际变为 `480x854` / DPR 2。 |
| R02.2 | 标记链路导致仿真失效的唯一根因 | 已完成 | 同一 guest 逐项隔离确认 `Page.enable`、`Runtime.enable`、`Network.enable`、`Page.getFrameTree`、`Page.createIsolatedWorld`、无 clip 截图和 `set_annotations` 均保持 fixed；单独执行带 `clip` 的 `Page.captureScreenshot` 后，guest 立即由 `1280x800` / DPR 1 回到 `480x854` / DPR 2。根因是 clipped CDP screenshot 清除了 Electron 设备仿真。 |
| R02.3 | 仿真状态唯一事实源 | 已完成 | Desktop 壳已收敛为唯一 `BrowserWindow -> App Renderer -> Browser Tab 内容槽 <webview>`，不存在 App 原生子 View 几何双轨；同一新 Electron 包已通过 auto/fixed、普通与区域截图、重复提交、Renderer F5 恢复和切回 auto 的完整序列。详细源码身份、包哈希、耗时和真实 guest 状态见第 9 节。 |
| R02.4 | 内容槽到 Chromium scale 的单一计算 | 已完成 | Renderer 只上报当前 Surface 的瞬时内容槽宽高，Main 用单一纯函数计算 Chromium `scale`；没有窗口坐标、原生 bounds、CSS transform、截图缩放、持久化或页面刷新。真实包在 480/600/400 像素槽宽下均完整显示 1280 像素设备画布，身份和导航代次不变。 |
| R02.5 | 模式切换与实例隔离 | 已完成 | 同一真实 Electron 包已覆盖 auto、宽屏、窄屏、自定义即时提交、加载中修改、隐藏恢复、BrowserWindow 缩放、两个一级 Browser Tab 独立视口和完整重启非持久化；详细证据见第 9 节。 |
| R02.6 | 自动化与真实 Electron 阶段门 | 已完成 | 小槽位/大视口、DPR 1/2、滚动、标记前后、重启恢复、自动化和同包真实操作均通过；源码身份、包哈希、命令结果与运行证据见第 9 节。 |

R02.6 完成后立即将 R02 标为“已完成”并更新第 9 节，再把 R03 设为“进行中”。任何子项失败都改为“未通过”，记录新事件证据和唯一根因；不能通过重试、固定等待、页面刷新或旧缓存回写掩盖失败。

### 11.5 R03 完成卡与 R04 当前锁定执行卡

| 编号 | 功能点 | 状态 | 完成证据或下一步 |
| --- | --- | --- | --- |
| R03.1 | 四套坐标与 scale/DPR 协议 | 已完成 | Main 提供 Surface 级 display metrics；hit test 提供真实 DPR；最终设计已定义槽位、CSS viewport、文档和图片像素转换。 |
| R03.2 | scaled 区域与元素标记 | 已完成 | 同包真实 UI 区域、元素、留白拒绝、滚动和 DPR2 均通过。 |
| R03.3 | 截图裁剪与 Artifact | 已完成 | 标记和显式 normalized clip 使用真实 viewport 捕获加 Rust 内存裁剪；三格式像素单测通过，无 clipped capture 滚动副作用。 |
| R03.4 | 导航代次拒绝 | 已完成 | Screenshot 协议必带 navigation revision；真实旧 revision 提交返回 409 且不生成标记。 |
| R04.1 | Registry 与 Primary 唯一性核对 | 已完成 | 当前活动 guest 注册即提升；同逻辑 Tab 的 Surface revision 全局单调，Secondary 不发布权威页面事实。 |
| R04.2 | Primary 切换与 lease/fence 撤销 | 已完成 | Authority 原子撤销旧 lease/fence；旧 Primary 清除 Agent cursor/Inspect，旧 binding 命令按 revision 失效。 |
| R04.3 | 窗口关闭与全局 Tab 关闭 | 已完成 | 窗口关闭提升替代者或发送精确 Primary 关闭；全局 Tab 关闭释放全部 Surface 且不重复通知。 |
| R04.4 | 自动化与真实阶段门 | 已完成 | Authority 50/50、daemon 8/8、Desktop 84/84 与同包双 Tab 切换/关闭验收通过。 |

### 11.6 当前锁定执行卡：R05 浮层与焦点协议

本卡是当前唯一可修改阶段。Browser 工具栏、右栏外框和 Browser 页面分别属于 App Renderer、RightPane 和 Chromium guest；菜单、提示、标记选择与备注编辑统一使用 Renderer Popover Top Layer。锚点只允许由触发控件声明，tooltip 只能临时追加并恢复自己拥有的锚点名称，禁止覆盖视口菜单或标记历史菜单的锚点。

| 编号 | 功能点 | 状态 | 完成证据或下一步 |
| --- | --- | --- | --- |
| R05.1 | 锚点冲突真实复现 | 已完成 | 同一 Electron 包中视口按钮位于 `x=1250, y=42`，Popover 却回落到 `x=0, y=787`；运行态确认按钮原有 viewport anchor 被 tooltip anchor 覆盖，菜单 `position-anchor` 因此无法解析。 |
| R05.2 | tooltip 与菜单锚点唯一所有权 | 已完成 | tooltip 只追加当前 Tab 唯一 anchor，并在离开、失焦和组件销毁时精确恢复控件原值；视口和标记历史按钮的菜单 anchor 不再被覆盖。`npm run check --workspace magi-web`、`npm run test:right-pane --workspace magi-web`、`npm run test:desktop-right-pane-intent --workspace magi-web`、`git diff --check` 全部通过。 |
| R05.3 | Top Layer 生命周期 | 已完成 | 同一 Electron 包中 viewport 菜单由 `x=0` 收敛为触发按钮下方 `x=977..1277`，标记历史菜单为 `x=1124..1424`；最右侧 tooltip 为 `x=1309..1424`，不再越过 1440px 窗口。右栏新增菜单由 `x=0` 收敛为触发按钮下方 `x=544..784`。标记选择层与内容槽均为 `x=320,y=74,w=480,h=526`；备注面板位于槽底上方 `y=453.5..588`。切换两个 Browser Tab、切换 Terminal、折叠/展开右栏后所有旧 Popover 均关闭，只有当前内容槽可见。 |
| R05.4 | Esc 与焦点恢复 | 已完成 | Esc 关闭标记备注并恢复“添加标记”按钮焦点。真实 Electron 在 1440×896 非 overlay 布局中，网页先写入 `R05_WEB_FOCUS`，Composer 独立写入 `R05_COMPOSER_FOCUS` 且网页值不变；回到网页追加 `_RETURN` 后 Composer 内容保持。800×600 overlay 布局也在真实可见 Composer 区域通过；首次无效样本是调试客户端默认 viewport 把 App Renderer 仿真为 800×600 后点击了被右栏实际覆盖的中心点，不属于焦点故障。 |
| R05.5 | 同包真实阶段门 | 已完成 | 候选包 buildIdentity `533f19cf...d865a62c40736e`；Web 运行资产 `RightPane-DIPVA2Fz.js` SHA-256 `c655cf20...e1730d7`。根级 `npm run check`、Web 全量测试、Desktop 84/84、browser core/product、协议检查和 `git diff --check` 全部通过；真实运行证据覆盖 R05.1-R05.4。 |

### 11.7 当前锁定执行卡：R06 Popup 支持范围

本阶段继续维持一个 Browser 一级 Tab 对应一个 Chromium guest；网页请求新窗口时只能在当前 guest 内保留可证明的导航语义，或明确阻止并返回原因。严禁创建 Browser 子 Tab、第二个 WebContents、额外 BrowserWindow，严禁把不支持的 opener/OAuth 流程伪装为成功。

| 编号 | 功能点 | 状态 | 完成证据或下一步 |
| --- | --- | --- | --- |
| R06.1 | Main 唯一 Popup 决策链源码核对 | 已完成 | Main 只调用纯 `decideBrowserPopup`；允许结果进入原 `loadPopupInCurrentPage`，阻止结果携带类型化 reason。Desktop Control、Rust Host、daemon 和 Renderer 共用协议 3.5；没有第二套 popup 分支。 |
| R06.2 | 普通 HTTP(S) `_blank` 当前页复用 | 已完成 | 普通 `_blank` 和显式 noopener 脚本请求均保持 Target `3D039...F887`、Surface `surface-c372...ec70`、`webContentsId=2`；revision 分别前进，Browser Tab 与 webview 数量不变。 |
| R06.3 | `_blank` POST 请求语义 | 已完成 | 当前 guest 返回标题 `POST /post-target`，响应体记录 `query=magi`；Surface、Target、WebContents 不变，证明未退化为 GET。 |
| R06.4 | 不支持类型明确阻止 | 已完成 | 无名称 opener、命名 OAuth、独立 window features、about:blank 和 mailto 均保留原页/revision，分别产生稳定 reason 并显示中文 Top Layer 错误；不伪装成功。 |
| R06.5 | 无子 Tab、额外 Target 或窗口 | 已完成 | R06.2-R06.4 全矩阵始终保持同一 Target `234F37...A0095`、Surface `surface-23a3...5595`、`webContentsId=2`；RightPane Browser Tab=2、webview=2、page Target=1，没有新窗口或子 Tab。 |
| R06.6 | 同包真实阶段门 | 已完成 | 当前包协议 3.5 ready/compatible；Web 全量、Desktop 90/90、Worker 56/56、Authority 50/50、daemon browser_host 8/8、browser core/product、协议、rustfmt 和 `git diff --check` 全部通过。包哈希和真实矩阵见第 9 节。 |

### 11.8 当前锁定执行卡：R07 Tab、终端与资源生命周期

右栏只有一个活动显示槽，但各内容类型的资源语义不同：Browser 隐藏保活物理 guest，Terminal 的前端视图可以重建但后端进程和输出事实必须有明确边界，Code/Image 是可重载数据视图，Agent 由任务生命周期管理。不得用一套“切换即卸载”或“全部永久保活”策略覆盖所有类型。

| 编号 | 功能点 | 状态 | 完成证据或下一步 |
| --- | --- | --- | --- |
| R07.1 | 各内容类型所有权与生命周期源码核对 | 已完成 | 已确认 Browser 隐藏保活、Terminal 后端 PTY/2 MiB 缓冲与前端重连、Code/Image 重取、Agent 任务投影及各关闭入口；发现并修复 Desktop F5 不保存 Terminal Tab 身份的唯一缺口。 |
| R07.2 | Browser 隐藏、恢复与关闭 | 已完成 | 类型切换时 guest 保活且不命中；显式关闭单 Tab 精确释放对应 host/Target；窗口关闭后旧 Surface/Target 消失，重启只按 Authority URL 创建新 Surface，不复活已关闭 Tab。 |
| R07.3 | Terminal 进程、连接、缓冲与尺寸 | 已完成 | Tab/会话切换、F5 同 ID 恢复、2 MiB 历史输出重放、ResizeObserver 重算、显式关闭终止均通过；完整应用重启终止 PTY 且不恢复 Terminal Tab。 |
| R07.4 | Agent、Code、Image 与 Terminal 共存 | 已完成 | Browser、Image、Terminal 同时存在并往返切换时只有当前内容可见；关闭 Image/Browser/Terminal 任一类型不影响剩余类型。Agent/Code 使用同一 active-slot 分支和可恢复元数据，专项 golden 已覆盖组件入口。 |
| R07.5 | 资源上限与清理 | 已完成 | Authority 覆盖 Browser 容量回收；Terminal 显式关闭/会话关闭终止 PTY，组件销毁释放 WebSocket、xterm、ResizeObserver、MutationObserver 和订阅；Browser release ACK、Target/Surface 窗口清理均通过真实包。 |
| R07.6 | 同包真实阶段门 | 已完成 | Web 全量、Desktop 90/90、Worker 56/56、Terminal 11 项、Authority 50/50、daemon browser_host 8/8、browser core/product、协议、rustfmt 和 `git diff --check` 全部通过；包哈希与真实证据见第 9 节。 |

### 11.9 当前锁定执行卡：R09 Web 与 Mobile 权限边界

当前产品只允许 Electron Desktop 的可信 App Renderer 绑定本机 Browser Host 并接管真实 Chromium。Web 与 Mobile 只能查看 BrowserAuthority 的 URL、状态、Artifact 和消息引用，并提供系统浏览器入口；不得把“能看到记录”误报为“能操作本机浏览器”，也不得让远程页面静默使用另一台 Desktop Host。

| 编号 | 功能点 | 状态 | 完成证据或下一步 |
| --- | --- | --- | --- |
| R09.1 | 能力协商与工具目录核对 | 已完成 | App Server 只有可信 Desktop token 连接且客户端同时声明 desktopBrowserSurface/browserTools 才广告并执行浏览器方法；Web Turn 的 server-only 标记把全部 Browser 工具加入拒绝集。 |
| R09.2 | 后端本机接管授权 | 已完成 | Browser 写路由统一校验当前 Desktop connection 随机 token、Magi/Electron UA 与非 tunnel 来源；客户端 platform 只能降级，不能提升。设置、回收、创建、激活、关闭、导航、截图和标记写路径均受控。 |
| R09.3 | Web/Mobile 降级体验 | 已完成 | 真实 Browser guest 打开 Magi Web 时 `window.magiDesktop=null`、`desktopBrowserSurface=false`、`webviewCount=0`、写请求 501；记录、标记历史、Artifact 与系统浏览器入口不依赖 Desktop 控制。Web 设置页的开关、选择和回收操作禁用。 |
| R09.4 | 跨端与远程拒绝 | 已完成 | 普通 Web 即使声明 desktop 或伪造 Magi/Electron UA仍不能通过随机 token；App Server 不广告工具并返回 `-32600`。公网 tunnel 在 transport auth 后仍因 `cf-ray` 永久不能成为 Desktop Renderer；Mobile 显式声明只能降级。 |
| R09.5 | 同包阶段门 | 已完成 | Web 全量、Desktop 91/91、Worker 56/56、Browser 路由 17 项、App Server 10 项、Web Turn 权限、daemon check、browser core/product、协议、rustfmt 与 `git diff --check` 全部通过；同包身份和跨端结果见第 9 节。 |

### 11.10 当前锁定执行卡：R10 最终一致性与发版前验收

R10 不再扩展产品功能，只把已经完成的唯一架构与仓库、协议、文档和最终候选包收敛到同一事实。任何真实回归失败只在对应职责内修复，禁止重新打开已冻结阶段或恢复旧实现。

| 编号 | 功能点 | 状态 | 完成证据或下一步 |
| --- | --- | --- | --- |
| R10.1 | 最终设计、计划与源码一致性 | 已完成 | `browser-runtime-design.md` 已同步实际 BrowserWindow、Popup、生命周期和 Desktop token；旧阶段显式归档，第 11 节是唯一状态源。 |
| R10.2 | 废弃代码与双实现清理 | 已完成 | 生产扫描未发现旧显示/几何/投影/独立 Runtime/Tauri/CEF/静态 anchor 路径；保留的命中均为测试、发行拒绝守卫或历史证据。 |
| R10.3 | Schema、类型、版本与构建身份 | 已完成 | Browser 协议 3.5、App Server generated、26 项 Browser 工具和 Electron 单一发行边界通过；当前未提交候选包以 manifest 与组件 SHA-256 精确标识，`gitDirty=true` 不作为发布包。 |
| R10.4 | G4.2 多功能右栏与 HTML 单一路由 | 已完成 | HTML 直接请求只创建 Browser 一级 Tab；Browser、Markdown、Image、Terminal 同槽共存切换，无 Code 双路由、子 Tab 或额外 Target。 |
| R10.5 | G4.3 会话与任务基础能力 | 已完成 | workspace identity、projection/event 恢复事务、canonical/sidecar 锁序和 daemon 监督已收敛；真实数据已恢复，自动化与同一 Electron 包会话全链路通过。详细证据见 R10.5 子账本和第 9 节。 |
| R10.6 | 最终 Browser 全功能同包验收 | 已完成 | R10.6.1-R10.6.9 已按同一源码身份完成；当前本地候选包 `app.asar`=`cd8e86af4951f20019c52982c1b85ff5ebe1987554ae2e1b2f1233831719c024` 重新通过 Browser core live 与生命周期 live。导航、响应态、Popup、下载、视口、截图、标记、DOM、焦点、工具、Lease、Worker/daemon/F5/Electron 恢复证据齐全。 |
| R10.7 | 最终阶段门 | 已完成 | R10.7.1-R10.7.5 全部完成；静态构建、当前候选目录包、真实 Electron、协议/发行边界、冗余扫描和文档一致性均已核对。当前工作树仍有未提交改动，因此只交付本地候选包，不自动提交、推送或发布。 |

#### R10.5 当前唯一执行计划：会话权威持久化与真实应用恢复

R10.5 未完成前不得进入 R10.6。以下子项必须按顺序推进；每完成一个子项立即更新状态和证据，失败时只记录首个错误事件、唯一根因和下一步，不得通过删除用户数据、固定等待、重试或兼容分支绕过。

| 顺序 | 编号 | 功能点 | 状态 | 完成证据或下一步 |
| --- | --- | --- | --- | --- |
| 1 | R10.5.1 | 停止写入并建立不可变数据备份 | 已完成 | 已停止 PID `80686` 进程组中的 Electron、Renderer、daemon；`/tmp/magi-r10.5-safety-backup-20260910T1610` 保存仓库 projection/meta、隔离状态 projection/event/migration/workspace 副本，共 59 个文件、34 MiB，`SHA256SUMS` 哈希为 `31fe5e489450679cd00d7a130dc65d909ab5e8edcefd42ae50d0da95300c67a1`，目录已设为只读。 |
| 2 | R10.5.2 | 复盘错误迁移并确定 workspace identity 唯一契约 | 已完成 | 根因已收敛：项目 projection 与 daemon event 分属不同生命周期；加载器未校验 projection 的 `workspaceId` 与扫描根一致，`session_projection_path` 又把未知 workspace 静默回落到全局目录，后续 checkpoint 因而把源文件加入删除事务。唯一契约：项目本地 `.magi/workspace-identity.json` 固化 workspace ID；首次升级可且只能从归属一致的现有 projection 初始化；全局注册表必须与本地 identity 相同；未知或混合归属直接失败且不写盘；project projection 是新状态根导入 canonical event 的完整检查点，导入必须通过持久事务一次提交；普通运行仍以 event log 为 mutation 权威。 |
| 3 | R10.5.3 | 实现单一、原子、幂等的会话恢复事务 | 已完成 | 已新增项目本地稳定 identity；workspace/global projection 严格校验归属与规范路径；未注册 workspace 不再回落；重复/错位 projection 只失败且不移动、不删除；workspace projection 导入 event 使用可恢复双文件事务。daemon 全量 `123/123`、workspace `25/25`、workspace API `18/18` 通过。真实备份副本 `/tmp/magi-r10.5-import-e2e-20260910T1645` 首次注册采用旧 ID，重启后 34 个 projection 保持原位、13 个非空会话和 13 个 event 根恢复；第二次重启无导入日志，projection 哈希 `b1d773...2538`、event 哈希 `dbde66...ae32` 和会话记录均稳定。 |
| 4 | R10.5.4 | 恢复并核验本轮受影响的真实 projection | 已完成 | 恢复计划 `/tmp/magi-r10.5-real-recovery-20260910T1650/recovery-plan.json`，SHA-256 `76daa88...f3ea`。已从只读备份原子补回 34 个历史 projection，逐文件哈希一致；保留并显式归并 `session-1789022389757-0`；仓库现有 35 个唯一 session，全部归属 `workspace-1784091993188-0`，workspace meta 未改，identity manifest 已写入。使用全新状态根 `/tmp/magi-r10.5-real-verify-20260910T1700` 注册真实仓库并连续重启两次：13 个非空历史会话、标题和消息数稳定，13 个 event 根完整，项目 35 个 projection 哈希始终未变化，无待恢复事务或重复导入。 |
| 5 | R10.5.5 | 持久化自动化回归 | 已完成 | workspace `25/25`、session-store `120/120`、API `605/605`、daemon `124/124` 全部通过；覆盖稳定 identity、错位/重复 projection 拒绝、未知 workspace 无回落、event-only sidecar、正常导入、重启幂等和中断导入事务恢复。`cargo check -p magi-daemon`、`cargo fmt --all -- --check`、`git diff --check` 通过。 |
| 6 | R10.5.6 | 同一 Electron 包会话全链路验收 | 已完成 | 候选包 `/Users/xie/code/magi-rust-rewrite/target/electron-dist/mac-arm64/Magi.app`，Magi 3.0.51 / Electron 43.4.0 / Chromium 150.0.7871.224，`app.asar`/daemon/Worker SHA-256 为 `bd2f09...2253`、`3d5f9d...eeb4`、`083b98...8692`。真实个人会话首发立即入列表且 Composer 清空，`hello` 与 `follow-up` 两轮均完成；双会话并行、历史切换和 Renderer 刷新保持消息与运行态；完整 Electron 重启恢复 5 个个人会话。SIGKILL 子 daemon 后约 1 秒产生新 PID/runtimeEpoch，Renderer 不刷新且可立即新建、发送、停止。删除测试同时移除目录记录、projection 和 event。恢复后的真实仓库在同包 Desktop 中展示 13 个非空历史会话，标题、消息数及中文两轮回复完整。真实并发曾捕获 sidecar flush 跨 canonical 代次，已统一 `canonical_commit_lock` 并由新增并发测试验证，修复包复验未再出现该错误。 |
| 7 | R10.5.7 | R10.5 阶段门 | 已完成 | 最终设计已写入稳定 identity、严格归属、可恢复导入事务和 canonical/sidecar 同代快照规则；个人验收状态 5 个 projection/5 个 event 根、真实项目 35 个 projection/13 个有内容 event 根，均无待恢复事务或双归属。workspace `25/25`、session-store `121/121`、API `605/605`、daemon `124/124`、Desktop `92/92`、相关 check/format/diff 全通过。R10.5 关闭，后续不得回开；当前只进入 R10.6。 |

#### R10.6 当前唯一执行计划：最终 Browser 全功能同包验收

R10.6 只验收并修复最终候选实现，不扩展产品范围。以下子项严格顺序执行；当前子项没有形成同一包证据前不得进入下一项。真实失败必须记录首个错误事件与唯一根因，只在其所有权模块内做一次结构性修复。

| 顺序 | 编号 | 功能点 | 状态 | 完成证据或下一步 |
| --- | --- | --- | --- | --- |
| 1 | R10.6.1 | 候选包身份与 Browser 基线 | 已完成 | 当前候选包 `/Users/xie/code/magi-rust-rewrite/target/electron-dist/mac-arm64/Magi.app`：Magi `3.0.51`、Electron `43.4.0`、Chromium `150.0.7871.224`、协议 `3.5`、buildIdentity `533f19c...40736e`、`gitDirty=true`；`app.asar`=`cd8e86...19c024`、Main=`5212e51...e8314`、Preload=`8269dda...e46f50`、Worker=`9b1fea...17e8a`、daemon=`c2865d4...ccd3ac`、WebWorkbenchShell=`9facbaa...ada8b`、RightPane=`28eaa2d...d9ef3`、web=`744215c...40df7`。Host `ready`、协议兼容、generation=1、无错误；单 Tab 基线仅一个 App page/真实 webview Target，Authority Session/Tab/Surface 均 `ready`。 |
| 2 | R10.6.2 | 导航、响应态、失败页、Popup 与下载 | 已完成 | 同包真实矩阵覆盖地址栏、302、后退/前进、刷新、慢加载、Stop、DNS 失败/Retry/恢复和 dialog；真实窗口加载态非黑屏。普通 `_blank` 当前页复用，命名窗口类型化阻止，始终 1 个 Target/1 个一级 Tab。下载 progressing/completed/cancelled/interrupted 全通过，私有完成文件 524288 字节，取消和中断无残留。 |
| 3 | R10.6.3 | 自适应、宽屏、窄屏与自定义视口 | 已完成 | 修复旧 Authority `revealTabId` 覆盖最新 Tab 意图；所有异步边界淘汰过期激活。新包上 auto/宽屏/窄屏/自定义、父宽度动态变化、快速双 Tab 切换和重启不持久化均通过，无刷新、截断、闪烁或跨 Tab 串值。 |
| 4 | R10.6.4 | 截图、区域标记与 Artifact 范围 | 已完成 | 截图、选区 artifact、序号、编辑、历史 Top Layer、消息与预览、F5、完整重启、无 workspace 和 stale 历史复用均通过；只有 deleted 标记不可再次引用。 |
| 5 | R10.6.5 | DOM 选择与标记并存 | 已完成 | Main Inspect 遵循 pointer-events，内部透明层不再可选；真实 P 节点、鼠标移动、标记模式、截图/标记并存、标准消息、F5 和刷新精确失效均通过。 |
| 6 | R10.6.6 | 焦点、Top Layer 与交互层级 | 已完成 | App/guest 真实 Target 交叉焦点、Terminal、10 个提示、三类 Top Layer、内容槽不挤压和浅色保存按钮对比度均通过。 |
| 7 | R10.6.7 | LLM 工具发现、执行、Lease 与页面保留 | 已完成 | 26 项目录、真实 LLM Browser Tool Item、标题回复、写 Lease、虚拟鼠标、释放和任务结束保留页面均通过。 |
| 8 | R10.6.8 | Worker、daemon、Renderer 与 Electron 恢复 | 已完成 | Worker/daemon 保持原 Surface；F5 以首帧事件门修复并连续 5 次立即截图；完整 Electron 重启新建物理身份。各场景 URL、标记、消息和工具均恢复，无永久连接态或黑屏。 |
| 9 | R10.6.9 | R10.6 同包阶段门 | 已完成 | 当前候选包 `cd8e86...19c024` 通过 `verify-browser-core-live.mjs --app-renderer --write-annotation` 与 `--lifecycle-regression`：Host ready；同一 Tab 激活复用 Surface；第二个 Tab 使用独立 Surface；截图 `1280x800`；选区 artifact `256x160` 且小于整页；备注可编辑；临时 Tab 清理后无残留。此前同一源码身份的真实 LLM 标题读取证据仍保留在历史 R10.6.7；本候选包未宣称新增模型调用成功。 |

#### R10.7 当前唯一执行计划：最终阶段门

R10.7 不再接受功能开发。若验收失败，回到失败项的唯一所有权模块修复并重新生成候选包；任何包内容变化都会使旧同包证据失效。

| 顺序 | 编号 | 功能点 | 状态 | 完成证据或下一步 |
| --- | --- | --- | --- | --- |
| 1 | R10.7.1 | 全量源码与协议检查 | 已完成 | `cargo test --workspace` 全部通过；`npm run check`、`npm test`、`cargo check --workspace`、`cargo fmt --all -- --check`、`git diff --check`、`npm run release:guard`、`npm run test:browser-core`、`node scripts/verify-browser-product-acceptance.mjs`、`npm run test:browser-download-lifecycle` 和 `node --check scripts/verify-browser-core-live.mjs` 全部通过。`magi-tool-runtime` 现为 `221 passed / 0 failed / 1 ignored`，Worker intent 测试已在真实 workspace 语义下通过；协议 3.5、23 个命令、21 个 payload Schema、26 项 Browser 工具目录和 Electron 单一发行边界均校验通过。`verify-browser-core-live` 已收敛为显式 App Renderer 同源 transport，真实写接口由 Electron 注入 token，不再用未授权外部 HTTP 请求制造假失败。 |
| 2 | R10.7.2 | 静态 Web 与 Electron 生产构建 | 已完成 | `npm run build --workspace magi-web` 和 `npm run desktop:package -- --dir` 均成功；Electron `after-pack` 自检通过，目录包为 `/Users/xie/code/magi-rust-rewrite/target/electron-dist/mac-arm64/Magi.app`，包内仅保留 Electron 发行边界，统一 manifest、Host、Worker、Playwright、Chromium 和应用版本。 |
| 3 | R10.7.3 | 本地候选包身份冻结 | 已完成 | 唯一本地验收包 `/Users/xie/code/magi-rust-rewrite/target/electron-dist/mac-arm64/Magi.app`。源码 HEAD/buildIdentity=`533f19cfe10df2a177178b8057d865a62c40736e`、`gitDirty=true`；Magi `3.0.51`、Electron `43.4.0`、Chromium `150.0.7871.224`、Browser 协议 `3.5`、CDP `0.0.1551306`。`app.asar`=`cd8e86af4951f20019c52982c1b85ff5ebe1987554ae2e1b2f1233831719c024`；Main=`5212e5138484a5462be034dffb6f632ffa19bd326719f80bbc5d6de1e40e8314`；Preload=`8269dda2dc57bcca0f393d919a2afc29a07df32b47093273940ab4008ed46f50`；Worker=`9b1fea1b33520df37f0feb28c33c2cece53c9452497f37d02b8856a287217e8a`；daemon=`c2865d4ebccc53d83433f3cd66bb70c1c9139fe15f103a5b41ca7bad94ccd3ac`；model bridge=`228950f4f21762a04c7819f1b0768083ff05b13aced18a9238f5322503511afb`；MCP bridge=`205767c2aec89664f64209d3a4057d5d82e96d9a84c302ceef6e0b852464ca4e`；WebWorkbenchShell=`9facbaa38d88cb48fbbd5f200846df91202bce8af3e75b3ff35c19dd10bada8b`；RightPane=`28eaa2d8f10b2483527cd47ff03d5bc0951467f71b8cd9ddb259dc1e502d9ef3`；web=`744215c09d6b29f190859d682d718d90feca35aa3c678d97acda4f1670740df7`。该包仅可作为本地测试候选，不能作为已提交/已发布包。 |
| 4 | R10.7.4 | 最终真实桌面全链路复验 | 已完成 | 只使用当前本地候选包 `/Users/xie/code/magi-rust-rewrite/target/electron-dist/mac-arm64/Magi.app`。daemon `/health` HTTP `200`，runtime epoch=`runtime-1789111790960-21020-1`；真实 Electron 为单窗口、单 daemon，个人会话 `session-1789093090004-0` 的 Browser 页面为 `https://example.com/`。`--write-annotation` live 通过 Host ready、真实 Surface、无持久化 viewport、截图 `1280x800`、选区 artifact `256x160`、备注编辑；`--lifecycle-regression` 通过同一 Tab 复用 Surface、第二 Tab 独立 Surface、generation 保持 `1` 和临时 Tab 清理。此前同一源码身份的完整 UI/LLM/恢复矩阵记录继续作为历史证据，但不能替代当前候选包的实测结果。 |
| 5 | R10.7.5 | 文档一致性与目标关闭 | 已完成 | 当前计划与 `browser-runtime-design.md` 已同步 BrowserWindow、单一 `<webview>` 内容槽、Popup、视口、标记/DOM、会话持久化、Web/Mobile 降级和恢复边界；当前候选包 manifest、源码 buildIdentity 与组件 SHA-256 已核对。生产扫描未发现 WebContentsView、原生浏览器几何覆盖、截图投影、Tauri/CEF/native-browser 等废弃生产路径；保留命中仅为测试、发行守卫或历史账本。当前仅保留本地未提交候选，不提交、推送或发布。 |
