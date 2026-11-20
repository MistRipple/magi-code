# 阶段 2 收尾验收（2026-11-20）

对象：重新打包后的目录包 `target/electron-dist/mac-arm64/Magi.app`（macOS arm64，Electron 45.0.0；`npm run desktop:package -- --dir`，包内 `execution-seams.mjs` 已继承官方 `McpResourceRuntime`，带锁定包 `@deepseek-ai/dsh-mcp-resources@0.2.2-rc.3`）。插件验收启动真实 Electron 与独立状态根，经原生入口安装固定原包。按用例使用本地受控 Provider 或在线 WorkBuddy Provider。构建身份与依赖摘要见 [Better Sidebar 结果](better-sidebar-result.json)（`daemonSha256`、`sdkLockSha256`、`installLockSha256`）。

## 结果

| 入口 | 结果 | 说明 |
| --- | --- | --- |
| `test:electron-dsh-better-sidebar` | **通过** | 固定 0.32.1：激活、未挂载槽位披露、路由只读本次绑定会话、空会话在真实 Turn 后变为有历史、从右栏打开原文件树、折叠后编辑文档与未保存输入保留、保存落盘、关闭、实例撤销后旧路由 403/409、重启后读回已保存文件与已提交历史。结果文件 [better-sidebar-result.json](better-sidebar-result.json)。 |
| `test:electron-dsh-message-history` | **通过** | 固定原包记录真实完成的会话；停用后不记录新输入且旧页面请求被拒；完整重启后保留历史并记录下一条实时输入。 |
| `test:electron-dsh-notify` | **通过** | 原包收到真实模型完成通知；失败摘要保留；被取消的轮次不通知。 |
| `test:electron-dsh-ops` | **通过** | 固定 0.20.8：设置与知识保存。 |
| `test:electron-free-search` | **通过 22/22** | 固定 0.9.41：设置、联网搜索与抓取、启停、失效页面请求拒绝、重启恢复。 |
| `test:electron-workbuddy` | **通过 16/16** | 固定 5.21.2：在线模型、原设置、在途取消与宿主恢复。 |
| `test:electron-right-pane-links` | **通过 16/16** | 右栏链接展开、真实页面加载、复用。 |
| `test:electron-plugin-interactions` | **通过 23/23** | 预览与确认安装、窄屏、真实 npm 旧版一键更新、搜索失败与重试、连续改词后旧请求不覆盖新结果。 |
| `test:electron-plugins` | **通过 14/14** | 工作区侧栏展开、会话导航与草稿保留。旧 10d、10e 的原生 MCP 验收移至独立入口。 |
| `test:electron-mcp-server` | **通过 81/81** | 独立验收客户端、网络访问和活动页。覆盖服务启停、真实 HTTP 和发行包 stdio、外部会话及审批、在途取消、审计与留存、令牌吊销、项目移除与恢复、网络直连。外部活动段的来源现由会话只读栏统一展示，真机覆盖原始与摘要两种模式及中英文。结果文件 [mcp-server-result.json](mcp-server-result.json)。 |
| `test:electron-dsh-management` | **通过**（14 项） | Mgr 固定原包：设置页安装、错误摘要被拒、npm 阶段取消、自更新提交与激活、原页面列表、旧网关状态、宿主崩溃恢复、非法配置、文件修复、原页面启停/更新/卸载、完整应用重启。原先因上游漂移失败（脚本锁定 `dsh-free-search@0.9.70`，npm latest 已先后变成 0.9.74、0.9.75）。已按产品设计处理：验收 registry gate 新增 `pinLatest`，把该包的 `latest` 固定到锁定版本（`/<包>/latest` 取该版本的官方版本文档，包文档只改 `dist-tags.latest`，tarball 与 integrity 仍是官方字节），上游发版不再改变这条验收的目标。另修一处脚本问题：完整重启后页面可能仍是 `about:blank`，先等页面加载到应用地址再发页面内 `fetch`。结果文件 [management-result.json](management-result.json)。 |
| `test:electron-dsh-health` | 已删除 | 2026-11-20 按用户决定删除脚本 `verify-electron-dsh-health.mjs`、`scripts/lib/dsh-health-acceptance.mjs` 和 `package.json` 入口。原因：`dsh-plugin-health@3.0.5` 依赖 alpha 线的 `@deepseek-ai/dsh-plugin-market-index@0.2.1-alpha.4`，与共享 SDK 锁定的 0.2.2-rc.3 不是同一条线，按版本锁定不引入；它也不在阶段 1、阶段 2 的固定样本清单。 |
| `test:electron-dsh-rea` | **通过 23/23**（非业务模式） | 前置程序 `rea-agents@6.5.0` 装到 `target/plugin-acceptance/rea-prerequisite`（`npm install --ignore-scripts`）。固定 1.2.0：精确包名搜索、原包 SVG 与完整性摘要、**255 个真实 REA 工具突破静态清单 128 项限制并进入 daemon**（即 `mcpResources` 修复在真实 Electron 里生效）、原页面状态卡读取版本与工具数、重新启用不复活旧草稿、完整应用重启后恢复实际工具与已提交设置。结果文件 [rea-result.json](rea-result.json)。 |
| `MAGI_REA_BUSINESS=1 npm run test:electron-dsh-rea` | **通过 30/30**（在线业务，目录验收包） | 固定 REA 1.2.0 与 WorkBuddy 5.21.2。由在线 `deepseek-v4.1-flash` 发起静态分析，经 Magi 原生审批，真实 REA 6.9.0 返回样本 IPC、preload API 和源文件摘要。模型回复给出 IPC 并明确报告结果截断。执行中的分析可以取消，宿主崩溃后单次失败且未重放，恢复后新轮次完成分析，完整重启后工具和设置恢复。结果文件 [rea-business-result.json](rea-business-result.json)。 |

## 本轮发现并修复的问题

- **回归：官方 `dsh-mcp-client` 原包无法激活**（REA 1.2.0 报 `inner.mcpResources.register is not a function`）。`execution-seams.mjs` 里的 `mcpResources` 只有 `list`。已改为继承官方 `@deepseek-ai/dsh-mcp-resources` 的 `McpResourceRuntime`（锁定依赖），保留 Magi 读 daemon MCP 资源的 `list` 作为扩展；官方运行时无条件注册的 `mcp-resource-servers` 提示段按"有提供者才算"排除出"系统提示装配"能力；补回归测试。重新打包后 `cargo test -p magi-daemon --test plugin_phase1_samples` 4 个真实原包样本（REA 1.2.0、Free Search 0.9.41、Plugin Ops 0.20.8、WorkBuddy 5.21.2）全部通过。
- **该集成测试的脚手架缺陷**：源码运行时路径没有把 `package.json` 复制进 `runtime/`；已修。它默认优先使用 `target/electron-dist/.../Magi.app` 里的打包运行时，所以验证的是最近一次打包的产物，源码改动后须重新打包再跑。
- **验收脚本与现行产品规则不一致**（均为脚本修正，页面与平台行为未改）：
  - REA：本机另有全局 `rea-agents 6.9.0`（`/opt/homebrew/bin/rea`），Magi 解析 PATH 后页面如实显示 6.9.0，而脚本把期望写死成前置目录里的 6.5.0；断言改为"页面版本必须是本机可见的某个真实 `rea` 的版本且工具数一致"，并去掉脚本末尾对已删除变量的引用。
  - Better Sidebar：右栏标签标题槽位 `sidebar.right.pane.tab.title` 已由页面宿主挂载，设置页不再把它报为未挂载；断言改为"仍报告 conversation 头部槽位、不再报告已挂载的标题槽位"。
  - `plugins`：侧栏当前会话选择器 `.session-item.enabled` 在页面和 HEAD 中都不存在，改为 `.session-item.active`。
  - `dsh-management`：`latest` 固定到锁定基线（见上表），重启后等页面加载再发请求。
  - `plugin-interactions`：搜索失败注入改为让该时段内所有被拦截的搜索请求都失败（页面按设计丢弃被取代请求的失败）；已搜过的词按缓存规则不显示"搜索中"；已安装插件的结果行身份是已安装插件 id。

## 本地安装包与验证范围

- 通过仓库的本地打包入口生成 Magi 3.0.62 macOS arm64 DMG、ZIP 及对应 blockmap，沿现行发行策略构建未签名安装包。DMG 通过 `hdiutil verify`，ZIP 通过 `unzip -tq`。独立 MCP 与侧栏导航验收在正式打包目录中的 `Magi.app` 上通过。
- 安装包的 REA 复验已通过安装和设置部分，在线业务未完成。目录包与安装包的 daemon 摘要不同。安装包第一次进入业务时，验收脚本仅按模型 ID 选中同名的 `workbuddy-global` 来源，网关返回该来源未配置此模型。脚本已指定与通过回执一致的 `workbuddy` 来源，随后复验在等待工作台时超时。在线业务通过结果以目录包回执为准。
- 保留交接记录所列的 3 个 `magi-api` 基线失败，本轮未修改或重跑这些测试。参见 [已知情况](../../../handoff-dsh-session-gateway.md#已知情况别重复排查)。
