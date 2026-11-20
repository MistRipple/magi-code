# 阶段 1 收尾记录（2026-11-18）

本记录区分开发版业务走查、宿主熔断复测和 macOS arm64 目录包验收。它们使用不同构建或初始状态，分别保留结论。固定原包与预期任务见[样本清单](../manifest.json)和[任务清单](../tasks.json)。

## 开发版 Electron

较早批次的本地记录在 `target/dsh-phase-1/electron-live-evidence/`。这轮覆盖了五款原包，其中 REA 的分析轮次包含两个失败的检查调用，分析主任务仍完成。目标插件首次安装通过 Magi API 发起，Mgr 原页面分别验证了启停、更新与卸载。已核对 0.2.13 原包 `dist/index.js` 的公开路由，首次安装由 Magi 提供。Mgr 从 Magi 设置页完成 0.2.12 到 0.2.13 的在线升级，取消覆盖预览。较早批次在共享宿主手动恢复处超时。

| 固定原包 | 本地证据覆盖 |
| --- | --- |
| WorkBuddy 5.21.2 | 模型发现、真实请求和用量、设置写回、取消、启停 |
| Ops 0.20.8 | 原页面诊断、真实答复、知识保存和重启后读取 |
| Mgr 0.2.12 → 0.2.13 | Magi 在线升级，原页面管理目标插件的启停、更新和卸载 |
| Free Search 0.9.41 | 实际搜索与抓取、设置写回、取消、启停 |
| REA 1.2.0 | 原页面设置写回与真实工具分析 |

随后独立复测宿主恢复。五款已装样本进入宿主代次 3，连续终止进程三次后熔断。点击设置页「重启插件宿主」后，五款样本都在代次 6 恢复运行。该轮验证应用级恢复操作，业务在途调用的结算另查 Rust 网关和监管测试。[熔断复测记录](shared-host-recovery.json)

## 目录包

`npm --prefix apps/desktop run package -- --dir` 本地构建 macOS arm64 目录包。页面宿主从锁定依赖和源码现构。解包自检核对发行资源哈希、随包 Node 与 npm 可执行版本，以及废弃运行时的排除。Electron 从独立状态根启动同包 daemon，使用同包 Node 和 SDK 在线安装 Mgr 0.2.13，其原生插件设置 Tab 挂载原包页面。公共作者指南改用 DSH 合约，旧 SDK 2 目录安装入口向用户提示改用 DSH 原包。

页面请求和浏览器资源由同一个 daemon 网关裁决。目录包实测 Mgr 列表通过两个入口均可读取。手动重启宿主后，两条旧页面请求均返回 409，新实例的两个入口均返回 200。[目录包验收记录](packaged-smoke.json)

## 2026-11-19 早期回执

以下结果使用同一份 macOS arm64 目录包和真实在线 npm 原包重新运行，取代上面的历史快照。

| 样本 | 结果 | 证据 |
| --- | --- | --- |
| WorkBuddy 5.21.2 | 26/26，通过真实模型、统计、账号池设置、取消、共享宿主崩溃恢复和完整重启 | target/plugin-acceptance/workbuddy-final/result.json |
| REA 1.2.0 | 27/27，通过 SVG、255 个动态工具、原页面配置、滚动/草稿保留、真实审批分析和完整重启 | target/plugin-acceptance/dsh-rea-final3/result.json |
| Mgr 0.2.12 → 0.2.13 | 通过设置页安装、在途取消、升级、原页面列表、启停、更新、卸载、非法来源恢复 | target/plugin-acceptance/dsh-management-final/results.json |
| Ops 0.20.8 | 安装、设置、知识保存、停用、重启通过；诊断明确排除 | target/plugin-acceptance/dsh-ops-final/result.json |

本批同时通过 web 类型检查、插件黄金回归、插件模型传输 clippy，以及 magi-plugin-system 与 magi-extension-host 全量测试。当时尚未覆盖 Free Search 在途取消、跨平台打包、阶段 2 会话事件和右栏样本。

2026-11-19 的阶段 1 补充回归修正了 Ops 集成测试口径。官方原包读取 DSH CLI `web` profile 时，Rust 端与真机都保留同一失败事实，测试不再注入模拟 profile，独立知识保存仍通过。

旧路径清退另见[后续记录](../retirement-2026-11-18/README.md)。

- 用同一构建和依赖锁跑完固定样本的必过任务。尤其是设置页发起目标插件首次安装、真正进行中的升级取消、非法配置与并发写入拒绝，以及逐包的崩溃期间调用结算。0.2.12 原页面的 DSH CLI 自更新属于已确认缺口，升级通过 Magi 设置页执行。
- 清退 SDK 2 的遗留 API 和测试消费者、`scripts/compat` 与旧投影。核对 `bind_dsh_profile_view` 的 `web`、`magi` 别名与正式配置加载合同，单一生效事实仍由 daemon 接纳的修订持有。
- 当前根 `package.json` 是用户确认保留的 DSH Desktop 清单。它与 Magi 的根 npm 锁文件、CI 所需的 workspaces 和发行脚本入口不一致。目录包用 Desktop 子项目入口在本机已有依赖上验证，干净检出的根 npm 发行门禁仍待恢复。
- 正式发布还须完成阶段 2 的会话事件、会话层标准套件和右栏业务，以及跨平台打包、CI 与发行预检。本轮目录包是本地 macOS 验收产物。

## 2026-11-19 在途调用补充回归

同一份重新构建的 macOS arm64 目录包补齐了此前未运行的逐包在途调用结算。

| 样本 | 新增回归 | 结果 | 证据 |
| --- | --- | --- | --- |
| Free Search 0.9.41 | 选定 WorkBuddy 模型执行真实 `web_fetch`，验证取消、共享宿主 SIGKILL、原代次页面 409、新代次恢复和恢复后的新抓取 | 22/22 | `target/plugin-acceptance/final-cancellation-free-search-recovery/evidence.json` |
| REA 1.2.0 | 真实 MCP 分析执行中取消、共享宿主崩溃、旧调用不重放、新实例恢复和重新分析 | 30/30 | `target/plugin-acceptance/final-cancellation-rea-distinct/result.json` |
| Mgr 0.2.13 | 原包更新查询在 registry 等待期间终止宿主，旧路由 409、新路由 200，随后原页面卸载目标 | 通过 | `target/plugin-acceptance/final-cancellation-mgr-inflight/results.json` |

请求取消由共享宿主向 `AbortSignal` 传递，已用 Node、Rust 共享宿主测试及真实原包回归验证。宿主崩溃不重放未确定调用，恢复后按原会话已选提供方处理新的请求；业务终态仍由 daemon canonical turn 事实提交。
