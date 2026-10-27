# 无项目会话与最近对话

`magi-daemon` 拥有所有会话、任务、权限、文件执行目录和持久事实。会话从创建起属于 `personal` 或明确的项目；`workspaceId = None` 是合法的个人归属，不允许借用活动项目、最近项目或第一项项目。UI 导航和任务接纳都服从这一事实。已有会话不通过项目选择器改绑，历史 Turn、审批和文件证据保持原归属；项目选择只作用于新草稿。

## 目录与能力

个人会话使用 `<state_root>/personal-sessions/<sessionId>`。它不是注册项目，不加载项目规则、项目记忆或知识索引。执行计划携带 `execution_root`，子代理沿原执行计划继承；终端和显式继续使用同一会话根目录。项目会话仍按现有隔离副本与 Git 准备链路执行。

内置目录、模型工具面与执行准入共用 `BuiltinToolName` 的两个能力条件：

- `requires_workspace_context`：项目知识、语义索引、符号、项目记忆与代理 Git 分支应用必须关联真实项目。个人会话隐藏 `memory_write`；执行端即使误注入了记忆存储也拒绝无项目写入。
- `requires_execution_directory`：文件、文本搜索、Shell、进程与图片生成需要实际 cwd，个人目录可以满足。cwd 本身不提升权限，写入与外部路径仍走既有 AccessProfile、审批和工具策略。
- `requires_repository`：结构化 `git_*` 需要真实仓库，不要求注册项目。`repositoryPath` 明确选择工作树根目录，省略时只检查当前执行目录；不会搜索最近项目、自动 init 或改变默认 cwd。读写权限覆盖工作树与 linked worktree/submodule 的 Git 元数据目录。

个人 Git 复用 `GitService`、仓库并发协调器和 `SessionCodeContextRegistry`；同一会话的不同仓库拥有独立基线，持久化仍使用 `session-git-contexts.json`。既有合同的空 `workspaceId` 表示没有项目，注册表按 sessionId + 规范工作树根区分仓库，不能被当作主项目上下文。每次调用取得仓库租约后重新观测；写操作受 branch/HEAD/path CAS 与权限保护。外部漂移先由 `git_status` 展示，修改需明确提供匹配当前观测的前置条件。个人子代理只查询获授仓库；主线修改 branch/HEAD 前须等待子代理完成，`agent_apply` 仍要求真实项目与受管理的代理产出。

GPT Web 槽位通过 daemon 的当前 owner session + active turn 解析执行目录与可选项目，MCP 令牌继续只访问令牌授权的注册项目。个人槽位可以调用连接器权限档允许的通用工具与 Git；项目知识和变更账本隐藏并在调用时再次拒绝，Shell 不因支持个人会话而开放。审批、canonical 工具项、取消与结果归原会话；槽位释放、turn 结束或 owner 改变后拒绝旧调用。连接器定义指纹只跟随工具 schema 和权限档变化，不因个人/项目会话切换触发重新配置。

提示词分别描述个人执行目录和项目上下文，不能把个人目录称为“当前项目”。没有项目时，不应宣称所有本地工具都不可用；可用工具以本轮实际定义和执行策略为准。

## 文件与生命周期

文件读取不依赖项目变更账本。`/files/content`、`/files/raw`、`/files/download`、`/files/reveal-target` 和 `/filesystem/list` 共用会话文件作用域解析：个人会话按 sessionId 定位独立根，项目会话按原项目和隔离副本解析。图片预览保留 MIME 白名单；下载使用附件响应。所有路径经 canonicalize 后验证边界，目录穿越、外部符号链接和错误项目绑定必须拒绝。未产生目录的空会话返回空文件列表，读取接口不为预览创建项目或变更快照。

页面预览携带来源会话，不借用侧栏或当前 Tab 的项目。个人会话文件可以在文件树中打开、预览和下载，生成图片同样按来源会话加载。HTML 与相对 CSS/JS 共用文件作用域解析，在 `site.localhost` 隔离来源执行；资源凭据绑定来源会话，删除后不可继续读取。项目会话预览使用其隔离副本，项目文件树与变更审阅仍使用原合同。重连和重启恢复会话与文件引用，不重新执行工具。删除会话先停止执行并持久提交删除，再安全回收会话管理的干净 worktree、Git 基线和私有执行目录，不清理外部仓库或其他会话目录；无法安全回收的 dirty/占用 worktree 保留目录与上下文，禁止强制丢弃成果。

## 最近列表

“最近对话”是 daemon 个人目录与已加载项目目录的活动时间排序视图；项目分组与最近列表引用同一个 sessionId 和投影，不新增归属、会话副本或第二份持久列表。项目行显示项目名称，导航、重命名、删除使用该行真实归属。全局新建进入无项目草稿，项目分组的新增入口明确创建项目草稿；输入框显示“未关联项目”，不再显示伪项目“Magi 空间”。

## 历史与兼容

当前持久模型已经能区分个人和项目。现存 personal 投影、执行目录、canonical 事件无需改格式或搬迁。已有 workspace 归属即使项目名称为 Magi，也必须继续保留：不能根据显示名猜测它是旧默认空间，不能清空 workspaceId、移动文件或改写历史审批。保留原项目身份和项目本地 projection，按普通项目恢复；升级只停止新请求的默认项目兜底。移除注册项目不等于删除历史数据，也不能使项目会话回落成个人会话。

## 验证

`npm --prefix web run test:personal-session` 验证目录选择、最近排序与预览来源；Rust 的 `personal_files_keep_session_boundary_and_do_not_require_project_snapshot` 与 `personal_site_preview_keeps_assets_in_the_owning_session_and_expires_on_delete` 验证路径、错绑和删除边界。`npm run test:personal-session-live` 使用独立状态根、真实 daemon/App Server/Chromium 和本地 Provider，验证个人与项目任务、文件/Shell/执行步骤、图片生成、HTML/CSS/JS 真实渲染与来源隔离、最近导航、全局新建、预览下载、重启刷新不重放和删除隔离，不连接用户模型服务。

2026-10-25 验收：上述真实链路已通过，另通过 Web 类型检查与构建、会话导航/输入框/右栏/文件引用定向回归、`magi-conversation-runtime` 与 `magi-tool-runtime` 库测试、API 个人会话/文件/网页预览定向测试、daemon 编译检查、协议与 Browser 工具目录一致性检查、Browser core 验收。新增回归覆盖删除持久化失败保留成果、转交个人代理不依赖 Git 注册表。工具库保留一项已有 ignored 测试。该验收使用本地模型替身，没有调用外部真实模型，也不等于安装包发布或完整 Desktop guest 验收。

Git 与 GPT Web 边界追加验收覆盖真实 Turn 的 Shell 建仓、查询、切分支与双仓库隔离，MCP socket 的个人目录读写、原会话审批和槽位失效；删除回归使用真实 Git worktree，确认干净自有副本回收、dirty/占用副本及上下文保留、其他会话与外部仓库不受影响。项目记忆误注入拒绝、外部仓库和 linked worktree 元数据授权也有执行前回归。验证命令与外部模型验收限制见[脚本与验证入口](validation.md)。
