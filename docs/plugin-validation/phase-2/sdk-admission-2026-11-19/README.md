# 阶段 2 原包依赖验收

同一份 macOS arm64 Electron 45.0.0 目录包，使用独立状态根逐一从 npm 取包、验证原包摘要并安装。修复前有三款在共享 SDK 解析阶段被拒绝。补齐 DSH 0.2.2-rc.3 的正式库依赖后，Notify 和 Better Sidebar 解除了安装阻塞。随包 SDK 锁与四份已接纳的安装锁均有摘要，仓库内的 SDK 锁就是该构建使用的锁。[修复前](before.json) · [本批回执](result.json) · [SDK 锁](../../../../crates/magi-extension-host/runtime/dsh/package-lock.json)

| 原包 | 已验证 | 剩余业务 |
| --- | --- | --- |
| Notify 0.4.9 | 设置页在线安装通过，原包独立设置页正常渲染。配置原包的 `dataDir` 后，在原页切换「页内提示音」，核对插件写入文件，完整重启后原页显示保存值。原包健康路由返回 200 | 正常会话完成与失败通知。原包请求的 `shell.overlay` 在页面槽位报告中列为未挂载 |
| Better Sidebar 0.32.1 | 在线安装通过，重启后 daemon 报告正在等待 `sessions` | 会话句柄、原包右栏标题与编辑器交互 |
| Mission Control 0.2.1 | 在线安装与宿主激活通过 | 由真实会话事件驱动的任务状态 |
| Message History 1.0.6 | 在线安装与宿主激活通过 | 历史检索和新用户消息入库 |
| Eval Control 0.5.0 | 预览核对原包身份，安装仍以 409 拒绝 | 原包把 `@deepseek-ai/dsh` 声明为必需 npm peer。锁定的官方包还包含 CLI、Web App、Agent Loop 和会话持久化后端 |

在独立临时目录解析官方 `@deepseek-ai/dsh@0.2.2-rc.3` 依赖锁，共得到 743 个锁定位置，比当前共享 SDK 多出 535 个位置，其中 339 个属于 DSH 作用域。该审计只解析锁文件。Eval 原包默认配置会加载会话投影分析器，仍依赖待交付的 `sessionProjections`。

用户确认按真实能力标记 Eval 阻塞。固定 0.5.0 归档及 409 回执继续作为阶段证据，样本计入固定分母，退出条件仍有缺口。

使用轮次通知批次的 Electron 构建再次从设置页搜索、预览和确认安装。预览核对版本及原包摘要，确认安装返回 409，设置详情显示缺失的依赖名，安装列表保持原状。[设置页复测回执](eval-settings-blocked.json)

本批验证了依赖准入、实例装载及 Notify 独立设置页和设置持久化。后续分批验证了[消息历史](../live-input-2026-11-19/README.md)和[轮次通知](../notify-turns-2026-11-19/README.md)。

## 依赖等待与安装拒绝复测

在 macOS arm64 Electron 45.0.0 中，从 Magi 设置页在线搜索、核对归档摘要及 npm integrity，再确认安装。两款原包分别使用独立状态目录，修复后的复测使用同一目录包。

- **Better Sidebar 0.32.1**。旧构建将 Cordis 的依赖等待显示为「启动中」。修复后首次安装和完整重启均显示「等待依赖」，详情列出 `sessions`。重启分配了新的运行实例，安装摘要保持一致。修复前后两次在线安装的依赖锁摘要相同。 [修复前](better-sidebar-waiting-before.json) · [修复后](better-sidebar-waiting-after.json)
- **Eval Control 0.5.0**。预览返回 200，安装返回 409，设置详情明确显示缺少必需的 `@deepseek-ai/dsh`，已安装列表保持原状。固定样本继续计为阻塞。 [同构建复测](eval-settings-blocked-current.json)
- **Message History 1.0.6**。同一构建复测真实 WorkBuddy 会话中的实时输入记录、停用隔离和重启续写。原包与联合安装锁摘要同上一批。 [同构建回归](message-history-regression.json)

Better Sidebar 原包的 `src/index.ts` 注入 `sessions`。文件浏览通过 `sessionCwdOf` 读取会话关联的目录，`session.phase` 与变化面板调用 `snapshotEvents()`。`src/client/native/index.ts` 分别登记右栏正文与标题。daemon 当前可以从检查点恢复会话的 canonical Turn、已接纳 Input 和事件水位。检查点会替换已覆盖的事件事务，单凭恢复后的 Turn 无法还原每条旧事件的原始序号。阶段 2 仍需交付基于已提交事实的会话读取和右栏组件承载，并验证实际编辑业务。
