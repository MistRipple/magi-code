# GPT Web 配置助手流程验收

## 2026-10-27：连接器详情就绪与导航交接修复

连接器详情页的刷新按钮会等待权限和工具列表加载后才启用。此前只等待 8 秒，且打开详情与刷新在同一页面命令里执行，导航代次变化时可能停在未确认状态。现在列表命令仅打开详情；daemon 对齐新 Surface 后再发详情命令，等待准确的 Magi 标题及可用按钮，最多 30 秒。已提交刷新或关联后结果不确定仍停止，不自动重放；取得控制权之前的只读 Surface 交接可有界重取。

定向站点测试 **27/27**、Worker 类型检查通过。隔离 Electron **68/68** 通过，包含详情按钮延迟 12 秒启用、连续两轮各刷新一次、没有重复创建连接器；日志 `/tmp/magi-connector-fix-electron-20261027.log`。隔离平台与 Tunnel client 为夹具。

加载修复后，在真实 Magi 开发客户端连续执行已有配置检查：前两轮在会话 `session-1793087134529-2` 各 **15 秒**完成；随后从新的主对话 `session-1793095790719-0` 发起「帮我配置 GPT Web」，**21 秒**自动完成。三轮均没有人工补点、显式重试或中途重启，实际进入已有 Magi 详情并回读刷新完成。截图 `/tmp/magi-connector-fix-two-real-rounds-20261027.png`、`/tmp/magi-connector-fix-real-config-20261027.png`；daemon 日志 `/tmp/magi-connector-fix-daemon-20261027.log`。

日志仍包含只读绑定换代 `browser_surface_stale` 和打开详情的 `browser_navigation_superseded`，均由驱动接续；不能表述为底层零告警。这三轮保留已有 Tunnel、运行时密钥和连接器，不增加真实空配置重建的通过次数，也不以刷新回读替代新会话工具往返验证。

随后将预期详情导航交接的 daemon 日志改为 INFO，其他失败与不确定结果仍保留 WARN；诊断变更不改变重试或副作用边界。`cargo check -p magi-api` 和 daemon 构建通过，经开发脚本重启后健康检查正常。新的真实主对话 `session-1793096929792-0` 再次 **18 秒**自动完成已有连接器检查，无人工补点或显式重试，截图 `/tmp/magi-connector-fix-after-restart-20261027.png`。

补充工具验收曾误在 `default` 本地模型下提交，远端返回 `unknown provider for model default`，未进入 GPT Web，不能计作 GPT Web 工具成功或通道失败。本次新增验收结论限于真实配置和连接器刷新；没有新增有效的 GPT Web 文件往返证据。

## 2026-10-27：配置助手的插件生命周期闸门

核对发现，执行端已经校验插件激活与运行租约，但主对话对「帮我配置 GPT Web」的识别、无模型发送放行，以及配置状态读取接口尚未完全受插件约束。本轮把输入识别、发送放行、设置入口和安全卡片统一绑定到 daemon 已激活的 `setup` 命令贡献，并给配置状态接口补上插件检查。配置提交没有发出时保留输入草稿。原生站点适配逻辑仍随 Magi 发布，由第一方插件的安装、授权、激活和排空统一控制。

验证结果：

- `npm --prefix web run check`：0 errors、0 warnings；`test:web-model`、生产构建通过。
- `cargo check -p magi-api` 通过；配置状态机 19 项、配置状态读取闸门 1 项、Web 模型选择器 1 项、未激活时不恢复已保存通道 1 项通过。
- 独立 daemon + 真实 Electron/Renderer/webview/Worker 验收 **62/62** 通过：未安装、仅安装未激活、激活、停用、卸载；涵盖普通输入不被接管、不绕过主模型要求、伪造命令/凭据和旧检查点访问返回 403、激活后的配置流程、停用后的入口/卡片消失、guest 释放和无运行时异常。
- 首次扩展验收时，guest 释放断言早于 UI 投影收敛；改为有界等待。随后卸载测试提交了不存在的会话，先被会话校验拒绝；改用独立新请求以明确验证插件闸门，保留 403 断言后完整重跑通过。

本轮外部平台页面与 Tunnel 客户端使用隔离夹具，没有改动真实账号的登录、Tunnel、连接器或密钥；不将本轮记为真实账号重新配置。

```sh
MAGI_SETUP_TEST_STATIC=1 MAGI_SETUP_TEST_PORT=38971 node scripts/verify-electron-gpt-web-setup.mjs
```

## 2026-10-27 接续真实客户端验证：已有配置连续三轮通过

本轮保留现有 Tunnel、运行时密钥和两个站点的登录态，通过真实开发 Electron 的产品入口继续配置和工具验证，没有删除平台资源或重新生成密钥，因此不增加下文“从空配置重建”的通过次数。

- 原个人配置会话 `session-1793068142878-0` 的未确认检查点，经「检查并重试连接器」后 **28 秒**完成，返回 ChatGPT 主页。独立临时工作区会话 `session-1793069318651-1` 随后两次实际调用 `magi.fs.read`，分别 **10 秒、6 秒**返回不同的 128 位随机首行；第二次由外部更新文件，值均未写入请求。文件系统及 MCP 审计的 `succeeded` 记录一致。截图 `/tmp/magi-gpt-real-tool-readback-20261026.png`。
- 停止并释放该工具会话后，再次从设置发起配置，**5 秒**报告连接器未确认，保留设置列表现场，没有误报完成。显式重试 **12 秒**恢复完成。这次偶发未确认的根因尚未确定，不能把后续成功当作已修复根因。
- 排查发现 Desktop 原有“命令已结算”日志不区分 Worker 的成功和失败协议结果。补充失败/结果不确定时的固定错误码、状态和副作用边界，不输出请求参数、页面文本或凭据。构建并重启 Desktop 加载诊断；daemon 没有重启。
- 重启后在个人配置会话 `session-1793070519861-2` 连续从设置发起三轮检查，分别 **12 秒、12 秒、18 秒**自动进入已有连接器详情、刷新工具并返回主页。三轮均无人工补点、显式重试或中途重启。日志中的详情页导航交接 `browser_navigation_superseded` 由现有驱动正常接续，不能表述为底层零告警，也不能表述为本轮日志改动修复了此前偶发失败。
- 三轮后在新工作区会话 `session-1793071074242-3`、右栏收起时，实际调用文件工具读取 `after-refresh.txt`，**10 秒**返回新随机首行；与文件系统及审计核对一致。验收文件位于专用临时目录，验证后已清理，没有修改用户项目文件；会话、审计和截图保留。截图 `/tmp/magi-gpt-web-final-tool-proof.png`。

本轮定向检查：Worker **133/133**、配置状态机 **18/18**、Desktop Control Server **27/27**，Worker/Desktop 类型检查、Web/Svelte（0 错误、0 警告）、Browser core 和相关 diff 空白检查通过。打包版 `test:electron-gpt-web` 未启动：本地缺少 `target/electron-dist/mac-arm64/Magi.app/Contents/MacOS/Magi`；上述真实验证来自运行中的开发 Electron，不以此替代打包版验收。

结论：当前已有配置的连续三轮检查及新会话工具往返通过；本轮早先仍复现过未确认，从真实空配置连续重建的稳定性标准仍未达成。

## 真实平台从空配置多轮重建（第 5 轮首次无中断通过，尚未连续多轮通过）

用户要求的稳定性标准为真实账号多轮完整配置，前述复用 Tunnel/密钥后的连续刷新不满足此标准。后续每轮分别记录真实资源清理、Magi 自动建 Tunnel 与准备密钥、人工生成并安全保存密钥、自动建连接器、通道回读，以及新会话随机文件读取的独立核对；失败与人工补点单独记录，不以夹具或旧连接器启用状态替代。

第一轮经用户当场确认后完成真实清理：旧 Magi Tunnel（ID 尾号 `356e3db`）删除后列表显示 No tunnels yet；尾号 `gCUA` 的 Magi Tunnel Runtime 密钥回读为 Revoked；旧 Magi 连接器删除后 ChatGPT 显示「你尚未安装任何插件」；通过产品设置「清除配置」清空本地 Tunnel ID 与运行时密钥。ChatGPT/Platform 登录保持，未清除浏览器数据。

随后点击「让 Magi 帮我配置」，**14 秒**后自动推进到主对话安全卡片：真实创建新 Tunnel（ID 尾号 `c3d708e`），自动准备 Magi Tunnel Runtime、Default project、30 天、Restricted、Tunnels Read + Use，平台独立汇总为 2 selected permissions。该段没有人工选工作空间、填表或补点。截图：`/tmp/magi-real-rebuild-round1-key-ready.png`。

用户生成并安全保存新密钥（预览尾号 `Z2EA`）后，本轮出现 `step_unconfirmed`；实际停在旧本地通道清理，状态为 `stop_failed`，尚未开始创建连接器。独立只读运行时清单显示旧别名已 stopped；执行同一别名 stop 返回 already_stopped，没有结束其他客户端。核对发现原 daemon 进程映射的可执行文件 inode 与磁盘构建不同（原进程 `365364143`，当时磁盘 `365386666`）。通过规定的 `scripts/dev-daemon.sh` 用当前源码重启后，确认进程与磁盘 inode 一致，新密钥和 Tunnel 配置被正常恢复，清单显示 ready。不能据此断言旧错误已由某一源码变更修复，也不能将该轮记成无中断通过。

关闭平台密钥结果页并点击配置卡片「我已完成，继续」后，Magi 在 **30 秒**内自动创建全新 ChatGPT Magi 连接器并回读完成。随后在新会话 `session-1792994969168-0`、GPT Web 临时模式且右栏收起时，读取新验收文件 `gpt-web-real-rebuild-round1.txt` 的第一行，**21 秒**返回未在提示词提供的随机值，独立文件系统核对一致。该文件位于专用临时工作区。截图：`/tmp/magi-real-rebuild-round1-config-complete.png`、`/tmp/magi-real-rebuild-round1-tool-proof.png`。第 1 轮结论为**完整链路恢复成功，非无中断成功**；仍需用当前运行版本完成后续从空配置重建。

### 第 2 轮：发现并修复真实工作空间多选菜单

经用户本轮确认后，真实删除第 1 轮 Tunnel（尾号 `c3d708e`）及 ChatGPT Magi 连接器，撤销尾号 `Z2EA` 的专用密钥。平台分别回读为空列表、无已安装插件和 Revoked；产品「清除配置」后独立核对 settings 通道段为空、密钥文件不存在、本机 runtime stopped。没有退出账号。

从产品入口重新配置，17 秒后出现 workspace_required。真实页面仅有一个工作空间，已被 Magi 选中，但平台是多选菜单，选择后不自动关闭；原逻辑按单选菜单等待关闭而误判为需要人工选择。修复为仅在工作空间多选场景收起本次打开的菜单，不再次点击已选项；项目/有效期等单选保留原行为。测试补充不自动关闭的菜单及提交前必须关闭的断言，Electron 夹具也改为真实多选行为。

Worker 平台测试 **18/18**、Worker 类型检查与开发 Desktop JavaScript 构建通过；唯一工作空间的隔离 Electron **54/54**、多候选人工选择后接续 **56/56**、Browser core 均通过。重启 Desktop 加载修复后，从原检查点点击继续，Magi 在 **14 秒**内自动创建新 Tunnel（尾号 `d4197ed`），准备完整运行时密钥表单（Default project、30 天、Restricted、Tunnels Read + Use，独立显示 2 selected permissions），未手动填表或选择工作空间。截图：`/tmp/magi-real-rebuild-round2-key-ready.png`。

用户经安全卡片保存新密钥后再次出现 `stop_failed`，不能归因于旧 daemon。定位到后台 `connect` 执行期间，通道准备阶段已清空，而 TunnelManager 仍为初始 Stopped；配置助手因此可能再次 configure，停止刚启动的运行时。修复为后台启动期间持续投影 `runtime_not_ready`，直到 start 返回或前置检查失败，再交还真实 Tunnel 状态。延迟 connect 三秒的真实 Electron 隔离回归在修改前触发重复连接断言失败，修改后两次 **55/55** 通过，且仅调用一次 connect。该证据证明启动状态空档和重复连接已修复，不证明厂商 stop 异常的全部原因已排除。

真实账号从产品入口接续时另一次停在连接器表单，daemon 记录 `browser_page_script_failed`；保留原检查点，没有手动补选风险确认。重启开发 Desktop 后通过「检查并重试连接器」，Magi 在 **27 秒**内自动创建新连接器并回读完成。该次错误的具体底层原因未再次复现，不宣称已定位或修复。

在新会话 `session-1792998070777-0`、GPT Web 临时模式、右栏收起时，读取新建随机文件 `gpt-web-real-rebuild-round2.txt` 成功（**24 秒**）；独立更新文件后再次读取，**9 秒**返回新的 128 位随机值，与文件系统核对一致。随机值均未写入提示词，写入仅限专用临时验收工作区。截图：`/tmp/magi-real-rebuild-round2-tool-proof.png`。第 2 轮结论为**恢复后的连接器与真实工具往返成功，非无中断成功**。后续仍须从空配置再验证，不能用本轮恢复或隔离夹具替代真实多轮稳定性结论。

本次定向检查：Tunnel 生命周期 **17/17**、配置状态机 **17/17**、站点与配置输入 **27/27**、Worker 类型检查、daemon cargo check、Browser core 与开发 Desktop JavaScript 构建通过。临时输入失败诊断代码已移除；没有读取新密钥或退出账号。

### 第 3 轮：已清空后重建，校正真实 daemon 版本核对

经用户本轮确认，删除第 2 轮 Tunnel、Magi 连接器，撤销尾号 `0AUA` 的专用密钥；平台回读 No tunnels yet、未安装任何插件、Revoked。本地经产品「清除配置」后独立核对通道段为空、密钥文件不存在、本机专用 runtime stopped。登录态保留。

从空配置点击助手后 **15 秒**自动创建新 Tunnel 并准备密钥表单。用户随后保存新密钥，流程再次进入步骤未确认。核对发现当前真实 daemon 映射 inode `365420878`，而隔离回归重新构建后的磁盘二进制为 `365437016`；真实 daemon 没有加载本次启动状态空档修复。因此不能把此失败算作修复版本回归，也不能把第 2 轮恢复成功当作该修复的真实从空配置验证。此前只重启 Desktop、不核对 daemon 的做法不充分。现通过规定开发脚本重启 daemon，保留本轮新资源和密钥，后续必须先核对进程与磁盘一致再继续。

重启后独立核对 daemon PID `36445` 的进程映射与当时磁盘 inode `365446353` 一致，健康检查正常；从原配置卡片继续，**31 秒**自动创建新连接器并回读完成，没有本轮手动填表补点。随后新建 GPT Web 临时会话 `session-1792999343159-0`，右栏收起，**17 秒**读取 `gpt-web-real-rebuild-round3.txt` 的随机首行，与文件系统独立核对一致。截图：`/tmp/magi-real-rebuild-round3-config-complete.png`、`/tmp/magi-real-rebuild-round3-tool-proof.png`。第 3 轮结论仍为**恢复成功，非无中断成功**。

### 第 4 轮：通道自动启动，连接器重试恢复完成

经用户本轮确认，旧 Tunnel（尾号 `c30513d`）、ChatGPT Magi 连接器及专用密钥（尾号 `qxUA`）已清理，本地连接配置为空，账号登录保留。加载前一批修复后，从新会话 `session-1793000471801-0` 开始，**17 秒**自动创建新 Tunnel（尾号 `daab22`）并准备密钥表单。真实页面核对：Default project、30 天、Restricted、Tunnels All selected，独立汇总为 **2 selected permissions**。主对话安全卡片已回填 Tunnel ID，密钥框为空、保存按钮禁用，等待用户生成并安全保存。截图 `/tmp/magi-real-rebuild-round4-key-ready.png`。

本轮运行 daemon PID `40975` 映射的可执行文件 inode 与磁盘均为 `365469063`，健康检查正常。不能将这一步记作配置全部完成；连接器创建与新会话随机工具读回仍待密钥交接。

补充核对登录、Tunnel/工作空间、项目/权限、密钥交接、通道连接、连接器重试、结束与完成等提示：

- 密钥已准备时明确按顺序生成、复制到安全框、关闭平台结果页 Done、再安全保存；自动处理期间显示等待，不要求用户同时操作。
- 项目选择、平台登录、权限检查与密钥准备未完成时，只显示对应指引，不叠加“粘贴密钥”通用说明；待确认连接器不再声称正在自动配置。
- 通道失败显示处理原因、重试连接及修改连接信息；不继续声称正在自动连接。
- Tunnel 歧义、ID 无法确认及手动兜底明确指出「已有 Tunnel？手动填写」入口，不引用尚未展开的“下方表单”。
- 中英按钮名称与实际入口一致；完成提示仍只声明通道和连接器回读，不把实际工具往返算作已完成。

上述补充提示修改在源码及定向回归中验证，当前真实第 4 轮已展示密钥交接指引；尚未加载的补充提示、异常路径与后半段流程不能记成已完成真实页面验证。

补充提示定向检查：Web/Svelte **0 错误、0 警告**，Web model golden、国际化 **3358 键 / 294 源文件**、配置状态机 **17/17**、Web 构建与相关改动空白检查通过。未重启当前真实流程，避免将第 4 轮从空配置验收改为重启恢复。

用户按指引生成并安全保存新密钥、关闭平台结果页后，通道自动准备完成，未再出现 `stop_failed`。随后连接器首次配置报告 `browser_pointer_unconfirmed`；产品内显式「检查并重试连接器」推进到名称、Tunnel ID 和无需身份验证均已填写，但又报告 `browser_page_script_failed`，风险复选框尚未选中。未手动操作平台表单，未将中间状态算作成功。

Worker 对自身运行时的固定命中/节点失效原因保留明确错误码（仅有限白名单，不记录任意页面异常正文）；新增回归确认保留不确定边界、固定原因不夹带异常正文、不重放点击。通过设置的「重启 Automation Worker」加载该改动，当前网页与登录保留，daemon 没有重启。再次从产品「检查并重试连接器」接续，**28 秒**自动完成风险确认、创建、连接和列表回读，并返回 ChatGPT 主页。该次未复现前述故障；此改动只改善诊断，**不能宣称已修复前述两个故障的根因**。截图 `/tmp/magi-real-rebuild-round4-config-complete.png`。

新工作区会话 `session-1793001719810-1`、GPT Web 临时模式、右栏始终收起，**18 秒**成功读取 `gpt-web-real-rebuild-round4.txt` 的随机首行，与独立文件系统回读一致；随机值未出现在请求中。截图 `/tmp/magi-real-rebuild-round4-tool-proof.png`。第 4 轮结论为**恢复后配置及真实工具往返成功，非无中断通过**。Worker runtime **83/83**、Worker 类型检查与构建、Browser core 通过。

固定错误码诊断改动的隔离 Electron 唯一工作空间 / ID-only 全流程 **54/54** 通过（`/tmp/magi-round4-diagnostics-electron.log`）。该结果仅证明受控平台页回归，无真实账号稳定性含义。第 5 轮已准备到尾号 `cPgA` 的专用密钥撤销确认页，待用户本轮确认后清理；当前 Tunnel 和连接器尚未删除。

### 第 5 轮：真实清空后无中断自动完成配置

经用户本轮确认，撤销尾号 `cPgA` 的运行时密钥（平台回读 Revoked），删除旧 Magi Tunnel（尾号 `daab22`，回读 No tunnels yet）和 ChatGPT Magi 连接器（回读「你尚未安装任何插件」）。通过产品「清除配置」清空本地通道；独立检查 `webModelTunnel` 为空且私有密钥文件不存在。没有清理浏览数据或退出账号。空配置截图 `/tmp/magi-real-rebuild-round5-local-empty.png`。

开始前核对当前 daemon PID `44041` 与磁盘构建 inode 均为 `365488274`，健康正常。新配置会话 `session-1793010308211-0` 通过主对话开始，**15 秒**自动创建新 Tunnel（尾号 `5919f65`），并完整准备运行时密钥表单：Default project、30 天、Restricted、仅 Tunnels Read + Use，平台汇总 **2 selected permissions**。过程中没有人工填表、选择工作空间或点击重试。配置卡片正确显示等待安全输入、自动回填新 Tunnel ID，保存按钮在密钥为空时禁用；操作指引与真实按钮一致。截图 `/tmp/magi-real-rebuild-round5-key-ready.png`。

用户生成并通过安全卡片保存新密钥、关闭结果页后，通道自动启动；Magi 在 **29 秒**内自动填写连接器表单、选择无需身份验证、确认风险、提交创建和连接，并回读为配置完成。真实页面成功后返回主页。期间未手动操作平台配置表单、未点击配置重试、未重启 daemon / Desktop / Worker；页面交接中的只读有限重取由现有流程自行处理。截图 `/tmp/magi-real-rebuild-round5-config-complete.png`。此前各轮恢复不计入连续通过次数。

新 GPT Web 临时工作区会话 `session-1793010506271-1` 使用新通道，**22 秒**成功调用文件工具读取 `gpt-web-real-rebuild-round5.txt` 的随机首行，独立文件系统核对一致，随机内容未写入提示词。截图 `/tmp/magi-real-rebuild-round5-tool-proof.png`。第 5 轮现记为**首次从真实空配置无中断通过**，人工仅清理测试资源和生成/安全保存密钥；这是一次通过，不能宣称连续多轮稳定性已达标。工具验收时用户曾切换右栏视图，未影响请求完成，不将其写成右栏始终收起。

独立更新相同文件为新随机值后，收起右栏再次请求工具读取，**8 秒**正确返回新内容，独立核对一致，排除复述上一轮返回值。截图 `/tmp/magi-real-rebuild-round5-tool-refresh-proof.png`。全程 daemon PID `44041` 与 inode `365488274` 保持一致。

### 第 6 轮：连接器入口未确认，显式重试后完成真实工具往返

经用户本轮确认，核对尾号 `eA8A` 的专用密钥已显示 Revoked，删除旧 Tunnel（尾号 `5919f65`）后回读 No tunnels yet；删除旧 ChatGPT Magi 连接器后回读「你尚未安装任何插件」。通过产品「清除配置」清空本地 Tunnel ID 与运行时密钥，UI 回读尚未配置、两项输入为空；没有清除浏览数据或退出登录。

开始前核对 daemon PID `44041`、进程映射和磁盘构建 inode 均为 `365488274`，健康检查正常，沿用第 5 轮通过的运行进程，没有重启。独立配置会话 `session-1793011493176-2` 从主对话入口开始，产品计时 **17 秒**自动创建新 Tunnel（尾号 `f3e4e2d`）并准备密钥表单。真实页面逐项回读 Magi Tunnel Runtime、Default project、30 天、Restricted、Tunnels All selected，独立汇总 **2 selected permissions**，其余资源均为 None。没有人工填写平台表单或点击重试。

处理中卡片正确提示等待且禁用凭据提交；准备完成后显示具体生成、复制到安全框、关闭 Done、安全保存的顺序，新 Tunnel ID 自动回填，密钥为空时保存禁用。截图 `/tmp/magi-real-rebuild-round6-key-ready.png`。

用户完成密钥安全保存后，通道自动就绪，但连接器配置在目录页入口报告 `browser_pointer_unconfirmed`（2026-10-26 10:50:20 UTC），页面停在 `/plugins`，没有打开创建表单。Desktop 日志显示该命令首个 `Input.dispatchMouseEvent` 耗时 5069ms；现有证据不足以确定未确认的底层根因，不通过延时或自动重复点击掩盖。提示正确保留未确认状态与「检查并重试连接器」，没有误报完成。该轮已中断，不能计为第 5 轮后的连续成功。

通过产品「检查并重试连接器」显式接续，Magi 先确认已安装列表为空，再于 **28 秒**自动填写新连接器、提交、连接并回读完成，成功后返回 ChatGPT 主页。没有手动补填平台表单、没有重启 daemon / Desktop / Worker。完成提示只声明通道与连接器确认，并指引在工作区新会话验证工具，语义与实际验证范围一致。截图 `/tmp/magi-real-rebuild-round6-config-complete.png`。

新工作区 GPT Web 临时会话 `session-1793012172477-3`、右栏收起，通过真实文件工具 **18 秒**返回 `gpt-web-real-rebuild-round6.txt` 的随机首行；外部独立改为新随机值后再次读取，**11 秒**返回新内容，两次均与独立文件系统回读一致，随机值未放入请求。截图 `/tmp/magi-real-rebuild-round6-tool-proof.png`。daemon PID `44041` 和 inode `365488274` 仍一致。本轮结论为**恢复后全链路成功，非无中断通过**；当前仍只有第 5 轮满足真实空配置无中断通过。

### 第 7 轮：修复点击前表单重排，恢复后完成真实工具往返

用户本轮确认后，撤销尾号 `smEA` 的专用密钥并回读 Revoked；删除旧 Tunnel（尾号 `f3e4e2d`）后回读 No tunnels yet；删除旧 ChatGPT Magi 连接器（应用 ID 尾号 `a4fa144`）后回读「你尚未安装任何插件」。通过产品「清除配置」清空本地连接；独立只读检查 `settings.json` 的 `webModelTunnel` 为空且 `web-model/tunnel-api-key` 不存在。登录保留。空配置截图 `/tmp/magi-real-rebuild-round7-local-empty.png`。

daemon PID `44041` 与磁盘构建 inode 均为 `365488274`，健康检查正常，未重启任何运行组件。新配置会话 `session-1793014334244-4` 从主对话入口开始，产品计时 **17 秒**自动创建新 Tunnel（尾号 `330da2d`），准备好 Magi Tunnel Runtime、Default project、30 天、Restricted、仅 Tunnels Read + Use，平台独立汇总 **2 selected permissions**，其余资源均为 None。没有人工填表、选择工作空间或点击重试。

交接卡片正确显示等待安全输入、新 Tunnel ID 自动回填、密钥为空时保存禁用；生成、复制、关闭 Done、再安全保存的顺序与实际平台按钮一致。截图 `/tmp/magi-real-rebuild-round7-key-ready.png`。

用户保存密钥后，连接器自动配置在原生输入处报告 `browser_target_obscured`，保留检查点且没有重放不确定点击。定向 Chromium 回归复现跨进程鼠标移动等待期间目标节点重排，旧坐标落在另一节点的情况。Worker 现在仅在鼠标按下前、同一节点边界确实变化时重新获取命中点，最多三次；真实遮挡、节点失效和坐标操作不会因此绕过检查，也不会重放按下/释放。另覆盖 listbox option 在可信 pointerup 后卸载时的完成观察，普通按钮仍要求可信 click。加载修复后通过产品重试，**35 秒**自动完成连接器创建与回读。截图 `/tmp/magi-real-rebuild-round7-config-complete.png`。本轮经历失败、加载修复及重试，不能记作无中断通过。

Worker runtime **83/83**、类型检查及真实 Chromium 原生输入回归通过（`/tmp/magi-round7-worker-tests.log`、`/tmp/magi-round7-pointer-final.log`）。重排回归在修改前报告 `browser_target_obscured`，修改后通过；这证明所覆盖输入场景已修复，不证明平台自动化已连续稳定。

在新工作区 GPT Web 临时会话 `session-1793016637652-5`、右栏收起时验证随机文件 `gpt-web-real-rebuild-round7.txt`。第一次使用原生 typeText 输入中文时只发送了文件名与标点，属于本次操作输入不完整；改用粘贴并在发送前核对完整请求。随后模型一次未调用工具便声称找不到文件，不能记为文件读取成功。明确要求实际调用 Magi 连接器后，**15 秒**返回新生成的 128 位随机首行；外部更新文件后再次实际读取，**13 秒**返回新随机值。两次均与独立文件系统回读一致，MCP 审计分别记录 `magi.fs.read` 成功，随机值未写入请求。截图 `/tmp/magi-real-rebuild-round7-tool-proof.png`。第 7 轮结论为**恢复后配置及真实工具往返成功，非无中断通过**；目前仍只有第 5 轮满足真实空配置无中断通过。

顺路提示检查发现设置通道空状态出现「尚未配置: 尚未配置工具通道…」的重复前缀。完成本轮后修复为该空状态只展示本地化状态名称，操作说明继续使用本节既有表单指引；其他失败状态仍保留 daemon 的原因说明。Web/Svelte **0 错误、0 警告**、Web model golden 通过。

Web 生产构建通过；当前已挂载的设置页面仍显示旧空状态文案，未将源码修复记为真实页面通过。

### 第 8 轮：修复未关联误报，恢复后完成真实工具往返

用户本轮确认后，撤销尾号 `GXMA` 的专用密钥并回读 Revoked；删除旧 Tunnel（尾号 `330da2d`）后回读 No tunnels yet；删除 ChatGPT 旧 Magi 连接器（应用 ID 尾号 `9330f5c`）后回读「你尚未安装任何插件」。通过产品「清除配置」清空本地连接；独立核对 settings 通道段为空、密钥文件不存在。没有退出账号。截图 `/tmp/magi-real-rebuild-round8-local-empty.png`。

开始前确认 daemon PID `44041` 映射 inode 与磁盘构建均为 `365488274`，健康正常。新配置会话 `session-1793017581865-6` 从产品「让 Magi 帮我配置」启动，**19 秒**自动创建新 Tunnel（尾号 `b3bc194`），准备好 Default project、30 天、Restricted、仅 Tunnels Read + Use，平台独立汇总为 2 selected permissions。安全卡片正确回填 Tunnel ID，密钥为空时保存禁用，交接提示与实际按钮一致。

观察到安全保存后流程自动继续，通道启动成功，连接器在 **30 秒**后报告未确认，真实页面停在「连接 Magi」确认层。未手动点击该平台确认按钮。检查发现页面适配器会把创建表单消失、连接确认层尚未挂载的空档当成已完成。新增延迟确认层的行为回归，修改前没有点击连接便返回 configured；修改后等待精确应用名的确认标题和按钮，实际连接一次并等确认层消失；空档不再作为成功证据。隔离 Electron 夹具增加 900ms 确认层挂载延迟。

定向 runtime / site 测试 **108/108**、Worker 类型检查、开发 Desktop JavaScript 构建及延迟确认层的隔离 Electron 完整流程 **54/54** 通过（`/tmp/magi-round8-delayed-connect-electron.log`）。通过产品重启 Worker 加载修复，页面与账号保留；从「检查并重试连接器」恢复，**5 秒**回读完成并返回 ChatGPT 主页。该次恢复首先检查到现有连接器，不能把 5 秒回读记为新建过程或修复后真实延迟确认层已被实际点击。截图 `/tmp/magi-real-rebuild-round8-config-complete.png`。本轮已有未确认与 Worker 重启，不计无中断通过。后续需再从空配置验证完整新建。

继续真实工具核验发现上述「完成」存在实质误报：会话 `session-1793018430947-7` 两次回复无法使用文件工具，审计没有文件调用；平台详情显示「关联」按钮而没有「刷新工具」，证明应用已安装但尚未关联。原实现把列表存在且没有禁用字样当成 enabled，因此将未连接应用当作成功。已收紧列表证据：没有开关或明确工具数量时不报告 enabled；显式配置现有应用会进入详情，通过精确应用名的「关联 → 连接 Magi」接续，成功原生确认或实际工具刷新后才完成。普通继续仍不重放写入。新建成功也必须实际连接确认，不能仅靠列表存在。

新增站点回归覆盖已安装无工具证据、现有未关联应用接续、其他应用确认层拒绝；daemon 状态机 **18/18**（包括未关联不完成与显式重试）通过，站点 **25/25**、Worker 类型检查、daemon check、Web/Svelte **0 错误、0 警告**、国际化 **3355 键 / 294 源文件**及 Browser core 通过。隔离 Electron 列表改为真实页面的「Magi / Magi」无工具数量形式，包含延迟连接确认的流程 **54/54** 通过（`/tmp/magi-round8-no-count-electron.log`）。

加载最新 daemon / Worker 后，产品助手在 **21 秒**完成现有连接器关联及回读；该步骤由 Magi 操作，没有手动点击平台「关联」「连接 Magi」。此前 5 秒完成的结果因工具验证失败已撤销，不计本轮通过。加载期间发现旧 Renderer 的执行中视图未随 daemon 重启结算，已重新启动开发 Desktop 并保留账号、Tunnel 和密钥；后续新会话验证仍待完成。

随后在新 GPT Web 临时工作区会话 `session-1793020184531-0` 验证 `gpt-web-real-rebuild-round8.txt`：右栏收起时首次实际文件读取 **24 秒**返回随机首行，与独立文件系统内容一致；外部更新相同文件后，再次明确要求实际调用读取工具，**8 秒**返回新的 128 位随机值，与文件系统独立核对一致。两次随机值均未出现在请求中，MCP 审计分别记录 `magi.fs.read / succeeded`，排除复述旧结果。截图 `/tmp/magi-real-rebuild-round8-tool-proof.png`。核验后从产品设置「停止并释放」释放已完成会话，登录态与通道保留。

第 8 轮结论为**修复后恢复配置及真实工具往返成功，非无中断通过**；当前只有第 5 轮满足从真实空配置无中断通过，尚未达到连续多轮稳定性标准。下一轮需重新清空真实平台资源与本地连接配置，不能以恢复后读取成功替代。

### 第 9 轮：连接器两次重试恢复，真实工具往返通过

经用户本轮确认，尾号 `VW0A` 的运行时密钥已撤销并回读 Revoked；旧 Tunnel（尾号 `b3bc194`）已删除并回读 No tunnels yet；旧 ChatGPT Magi 应用（ID 尾号 `aca3fec`）删除后回读「你尚未安装任何插件」。通过产品「清除配置」清空本地连接，独立核对 `webModelTunnel` 为空且私有密钥文件不存在。没有清除浏览数据或退出账号。截图 `/tmp/magi-real-rebuild-round9-local-empty.png`。

开始前核对 daemon PID `65893` 的映射与磁盘二进制 inode 均为 `365555128`，健康检查正常。从新配置会话 `session-1793021221668-1` 的产品入口启动，**26 秒**自动创建新 Tunnel（尾号 `490e30e`）并准备完整密钥表单：Magi Tunnel Runtime、Default project、30 天、Restricted、仅 Tunnels Read + Use，平台独立显示 **2 selected permissions**；没有人工填表、选择工作空间、点击配置重试或重启进程。

安全卡片正确显示等待安全输入、回填新 Tunnel ID，密钥为空时保存按钮禁用；提示按实际顺序要求生成、复制到安全框、关闭结果页 Done、再安全保存。截图 `/tmp/magi-real-rebuild-round9-key-ready.png`。当前等待用户生成并安全保存密钥，后半段自动连接器、真实工具往返与本轮是否无中断尚未验收。

用户保存后通道正常启动；连接器首次配置 **34 秒**报告未确认，日志为 `browser_pointer_unconfirmed`，真实页面停在目录「添加」入口，未手动补点。显式从产品「检查并重试连接器」恢复后 **36 秒**仍未确认，此时平台已显示「连接 Magi」确认层；再次从同一产品入口授权，Magi **14 秒**完成现有应用关联、回读并返回主页。平台名称、Tunnel、授权、风险确认与连接均由 Magi 操作；本轮未重启 daemon / Desktop / Worker。首次点击未确认的根因仍未定位，不把后续恢复写成已修复该问题。截图 `/tmp/magi-real-rebuild-round9-connector-unconfirmed.png`、`/tmp/magi-real-rebuild-round9-config-complete.png`。

新 GPT Web 临时工作区会话 `session-1793023831535-2`、右栏收起时，首次实际读取随机文件 `gpt-web-real-rebuild-round9.txt` 用时 **28 秒**；外部更新文件后再次实际读取用时 **12 秒**。两次返回均与独立文件系统核对一致，随机值没有出现在请求中，MCP 审计均记录 `magi.fs.read / succeeded`。截图 `/tmp/magi-real-rebuild-round9-tool-proof.png`。第 9 轮为**恢复配置与真实工具往返成功，非无中断通过**，尚未达到连续多轮稳定性标准。

针对连接确认层延迟补充 **12 秒**挂载回归：原 10 秒等待下测试失败，将只读等待上限延长至 **30 秒**后通过，同时检查其他应用确认层拒绝、31 秒超时仍不点击。此修改不重放创建/连接、不放宽成功证据，只证明可容忍更慢的确认层；真实第 9 轮没有记录精确挂载时长，不能据此认定该修改解决了全部未确认故障。站点/runtime **108/108**与 Worker 类型检查通过。

隔离 Electron 将确认层延迟改为 12 秒，首次运行因 `browser_user_control_held` 停止，保留失败日志 `/tmp/magi-round9-slow-confirm-electron.log`；独立重跑完整配置 **54/54** 通过（`/tmp/magi-round9-slow-confirm-electron-retry.log`），确认创建及连接各一次。测试使用受控平台与假 Tunnel client，不能代替真实账号新建验证。页面适配器 revision 提升至 `chatgpt-web-16`，以便 Worker 加载修改后替换已有页面脚本。

工具核验完成后经产品「停止并释放」释放会话，通过「重启 Automation Worker」加载上述修改；daemon 未重启，账号和本轮通道保留。平台已准备到尾号 `DVYA` 专用密钥的撤销确认层，待用户确认第 10 轮清理后再执行，当前尚未撤销或删除新资源。第 10 轮拟清理 Tunnel 尾号 `490e30e` 及当前 ChatGPT Magi 连接器。

### 第 10 轮：真实资源已清空，加载当前构建后开始重建

经用户本轮确认，尾号 `DVYA` 的 Magi 专用密钥撤销后回读 Revoked；旧 Tunnel（尾号 `490e30e`）删除后回读 No tunnels yet；ChatGPT Magi 应用（ID 尾号 `a1d43a2`）删除后回读「你尚未安装任何插件」。产品「清除配置」后独立核对 settings 通道段为空、私有密钥文件不存在，未清除浏览数据，账号登录保持。截图 `/tmp/magi-real-rebuild-round10-local-empty.png`。

重建前检查到旧 daemon PID `65893` 的映射 inode `365555128` 与当时磁盘构建不一致，因此先经规定开发入口重启。Desktop 开发启动会再构建 daemon，故完成 Desktop 启动后再次经开发脚本加载最终构建；开始配置前核对 daemon PID `76144` 的进程映射与磁盘 inode 均为 `365596904`，Worker 产物为 `chatgpt-web-16`。以上重启均发生在本轮新建流程开始前，未把旧进程当成当前修复版本。

从新会话 `session-1793026826371-0` 的产品「让 Magi 帮我配置」入口开始自动重建，后续阶段仍待验证。

首次自动处理 **18 秒**后停在 `permission_or_page_required`。真实 Tunnel 页面显示 **Tunnels access required**、Create tunnel 禁用，明确要求组织级 Tunnels Read / Manage；组织 General 页也显示缺少 `organization.read`。只读核对当前项目菜单仅列 Default project，未切换账号、修改角色或扩大权限。经平台主页返回产品「Tunnels」入口后，同一登录态的 Tunnel 页面恢复为 No tunnels yet 且 Create tunnel 可用。证据仅证明重新导航后页面恢复，不能据此认定账号真的缺权或已经定位权限提示根因。

从现有配置卡片点击「我已完成，继续」，Magi **10 秒**自动创建新 Tunnel（尾号 `cb921c3`）并准备运行时密钥表单。独立真实页面核对名称 Magi Tunnel Runtime、Default project、30 天、Restricted，Tunnels 为 All selected，其他资源均 None，汇总 **2 selected permissions**。没有人工填写平台创建表单，也未修改权限。安全卡片回填新 Tunnel ID，密钥为空时保存按钮禁用；交接提示与实际 Create secret key / Done / 安全保存并继续一致。截图 `/tmp/magi-real-rebuild-round10-key-ready.png`。

本轮已发生权限页面暂停和一次显式继续，因此不能计为无中断通过。当前停在用户生成并安全保存新密钥之前，连接器创建和新通道工具往返尚未验证；账号登录保持。真实核对过程中曾出现 `browser_navigation_revision_stale` 注册提示，关闭提示后平台主页正常显示，未重启任何运行组件；该现象尚未定位，不宣称已修复。

用户安全保存密钥后，通道显示连接正常；连接器首次配置在真实页面再次报告 `browser_pointer_unconfirmed`，未重放不确定点击。通过产品「检查并重试连接器」接续，Magi 在 **32 秒**内完成添加自定义 MCP 服务器、填写本轮 Tunnel ID、选择隧道、确认无需身份验证、风险确认、创建及列表回读，完成卡片显示通道与连接器均已确认。该次仍属于恢复完成，不能计无中断通过。

在新 GPT Web 临时会话 `session-1793057802260-1` 中，首次自然语言请求没有触发工具，返回了“未找到文件”；补充明确要求使用 Magi 文件读取工具后，第一次实际调用 `magi.fs.read` **6 秒**返回 `round10-first-72f08463f08f7b9934760a87bfb44411`。外部将同一文件更新为 `round10-second-2772cf325e0547d542567eb856d8a9da` 后，第二次明确工具请求 **6 秒**返回新值。两次均与文件系统及 `/Users/xie/.magi/mcp-audit.jsonl` 的 `succeeded` 记录一致，随机值没有写入请求。第 10 轮结论为**恢复配置与真实工具往返成功，非无中断通过**；这也说明产品提示应继续明确“请使用 Magi 连接器的文件读取工具”，减少模型误把工作区文件当作 ChatGPT 上传文件的情况。截图：`/tmp/magi-real-rebuild-round10-config-complete.png`、`/tmp/magi-real-rebuild-round10-tool-proof.png`。

## 2026-10-26 多轮重复配置验收

本轮保留真实账号的登录态、Tunnel 和已保存密钥，从产品入口反复执行已有配置的检查与连接器刷新；没有反复删除真实平台资源或生成新密钥。

真实客户端首次重入发现两次未确认：一次只读页面命令交接失败，一次打开连接器详情时，原生点击已导致导航，却被归入普通 `browser_pointer_unconfirmed`，配置驱动无法接续。Worker 现在把已观察到的导航明确报告为 `browser_navigation_superseded`，仍保留 `side_effect_started` 和不确定结算，不重放旧页点击。daemon 沿已有导航交接路径重新绑定后检查详情页，继续刷新。普通未确认点击仍然停止。

重启 Desktop 后，原检查点恢复并在 **21 秒**完成；随后通过「让 Magi 帮我配置」连续三轮均无需补点完成，分别为 **16 秒、16 秒、13 秒**。每轮都实际进入已有 Magi 连接器详情并刷新工具，成功后返回主页。最后在新产品会话 `session-1792992259124-1`、右栏收起时调用搜索与文件读取工具，**16 秒**返回未出现在提示词中的新随机值，独立文件系统核对完全一致。验收文件位于专用临时工作区。截图：`/tmp/magi-gpt-setup-repeat-complete.png`、`/tmp/magi-gpt-setup-repeat-tool-proof.png`。

隔离真实 Electron 从空状态执行三组流程分别 **54/54、56/56、67/67** 通过，涵盖唯一工作空间、多候选人工选择、接管、非法凭据、通道失败、访问拒绝与刷新超时。新增 `MAGI_SETUP_TEST_REPEAT=3`，在首次配置后连续重入三轮，要求本轮刷新计数增加、连接器创建计数不变、安全凭据保持不变，再检查完成卡片，最终 **63/63** 通过。外部平台页面与 Tunnel client 为夹具，这些空状态测试不等同于真实账号删建三次。

重复验收另发现：重配在两次轮询之间快速完成时，旧卡片的收起状态会遮住新的完成结果。前端现在依据服务端记录 `updatedAt` 变化恢复卡片。首次重复验收因此超时，修复后完成三轮验证；随后测试自身误将 Platform 与 ChatGPT 两个 origin 的 localStorage 计数放在一起判断，已改为各自所属页面的计数与独立安全存储核对，未放宽产品完成条件。

定向检查：Worker runtime **82/82**、Worker 类型检查、Web/Svelte **0 错误、0 警告**、Web model golden、Browser core、Web 与开发 Desktop JavaScript 构建均通过。新增导航回归同时证明命令保留副作用标记且仅发送一次点击。未发布安装包，真实平台的新密钥生成/复制仍由用户完成。

重复验收入口：

```bash
MAGI_SETUP_TEST_STATIC=1 MAGI_SETUP_TEST_ID_ONLY=1 MAGI_SETUP_TEST_UNIQUE_WORKSPACE=1 MAGI_SETUP_TEST_REPEAT=3 node scripts/verify-electron-gpt-web-setup.mjs
```

## 2026-10-25 尽可能自动完成配置

在既有唯一 daemon 状态机和 BrowserAuthority → Main → Worker 原生输入链上扩展，不新增凭据写入路径：

- 唯一工作空间自动选择并提交 Tunnel；多个/未知候选才交用户选，继续后保留同地址原表单，Magi 接续提交。组织沿用当前已选范围。
- 平台窄命令新增 `prepare_runtime_key`：自动打开密钥表单、填写名称、选择唯一项目、30 天有效期、Restricted 与仅 Tunnels Read + Use；逐项确认和独立权限汇总回读后才显示已准备。多个项目或权限不明时提示用户处理后再由 Magi 检查。
- Magi 不点击生成密钥、不读取生成结果或剪贴板；用户生成并直接保存到正式安全输入框，密钥仍只通过现有可信 Desktop 凭据请求存储。
- 开始配置已授权专用 Tunnel 与连接器初次创建/刷新，通道就绪后直接配置连接器并回读，不再多一次首次确认。失败/访问拒绝/结果未知仍显式重试；重启不重放。

已通过：唯一工作空间自动流程的隔离 Electron 54/54、多候选选择后接续原表单 56/56；包含接管、非法密钥、通道失败、访问拒绝、重新配置及刷新重试的恢复流程 67/67；Worker 117/117、后端状态机 17/17、Web/Worker/Desktop/Browser contracts 类型检查、国际化与 Web model golden、App Server 协议一致性、Browser contracts 校验和 Browser core。构建完成。截图 `/tmp/magi-auto-key-ready.png` 展示自动准备后的密钥交接卡片。

实际 Chromium 验证发现并处理了页面文本相邻拼接导致权限汇总判断不稳定的问题，改为等待独立汇总节点准确显示两项权限；工作空间夹具的候选菜单也改为位于所属 dialog 内以符合原生命中规则。恢复矩阵首次在刷新重试时报告 browser_pointer_unconfirmed，保留了原检查点；独立重跑 67/67 通过，没有新增对未确认点击的自动补点。受控页面使用真实 Electron guest、原生输入和状态机，平台响应与 Tunnel client 为夹具；没有把这些结果记为用户真实账号全流程完成。

## 2026-10-25 平台配置清空后重新验收（进行中）

### 新配置的真实工具往返（2026-10-26）

用户生成并经正式安全卡片保存新密钥后，初次接续显示「步骤未确认」；关闭平台密钥结果页并点击卡片「我已完成，继续」后，Magi 自动启动新通道、创建 ChatGPT Magi 连接器并回读为配置完成。真实列表此前为未安装任何插件，没有复用旧连接器。本轮证明了可恢复并完成配置，不能记成保存密钥后全程无停顿。

随后在专用临时工作区的真实产品会话 `session-1792989371124-1`、GPT Web 临时模式、右栏始终收起时完成三轮：读取未在提示词提供的 128 位随机值（12 秒）；外部更新文件后重新读取并返回新的随机值（5 秒）；经全局托盘「允许一次」后创建唯一验收文件并再次读取（39 秒，含人工等待）。独立文件系统核对 `gpt-web-reconfigure-write-20261026.txt` 内容与返回值完全一致。截图：`/tmp/magi-real-gpt-config-complete.png`。

真实写入验收另发现：全局托盘审批已执行，但主对话待审批提示残留并锁住输入框。修复为外部管理审批沿 `interaction_service::resolve_approval` 统一结算和发布既有 `tool.approval.resolved` 事件，并标记外部来源，让全局托盘与原会话都重读 daemon 事实；不增加前端推断或第二条写入路径。新增回归覆盖审批列表清空及会话事件。

修复后定向 API / 审批回归 4/4、daemon 构建通过。通过开发脚本重启 daemon 并重启 Desktop 后，登录与新通道配置保留，GPT Web 再次显示可调用项目工具。在真实新会话 `session-1792989966917-0` 创建 `gpt-web-approval-fixed-20261026.txt`，仍从全局托盘「允许一次」批准；主对话授权提示立即消失、输入框恢复，工具读回 `MAGI-APPROVAL-FIXED`，独立文件系统核对一致（30 秒，含人工等待）。随后可正常输入并提交下一轮读取请求，7 秒后返回文件的中文第二行，无须刷新页面解除锁定。所有验收写入均限于专用临时工作区。最终截图：`/tmp/magi-real-gpt-roundtrip-verified.png`。

### 真实运行时密钥表单自动准备（2026-10-25 23:58）

在用户真实账号、正式持久登录分区与产品配置卡片中继续验证，修复三处平台 DOM 差异：等待项目加载占位控件替换且可操作后再选择；权限下拉兼容语义 button / combobox；资源标签与可见权限选择值分别读取。移除临时项目诊断回传及日志，不把任意页面字段加入配置结果。

重启开发客户端后，从新的空白密钥表单点击「继续准备密钥表单」，Magi 在约 9 秒内自动填写名称、唯一 Default project、30 天有效期、Restricted、仅 Tunnels Read + Use，页面独立显示 2 selected permissions，其余资源均为 None；主对话卡片确认已准备。没有自动生成或读取密钥，已交给用户点击 Create secret key 并经安全卡片保存。GPT Web / Platform 登录态保留。截图：`/tmp/magi-real-runtime-key-prepared.png`。

定向站点 / 平台 / 安全输入测试 45/45、Worker 类型检查与构建通过；隔离真实 Electron 全流程 54/54 通过。Electron 夹具补上加载后替换项目控件、项目选择后异步启用有效期、单选菜单退出动画，以及使用资源名作为 accessible name 的语义权限触发器。本轮真实账号的后续新通道、连接器及文件工具往返仍待用户保存新密钥，不能以夹具通过记为完成。

在用户当前真实 Electron 客户端先结束旧配置流程，通过 GPT Web 设置的「清除配置」清空本地 Tunnel ID 和运行时密钥。经用户当场确认，平台旧 Magi Tunnel 已删除，旧「Magi Tunnel Runtime」密钥已撤销，列表回读为 Revoked。

从「让 Magi 帮我配置」重新进入：空 Tunnel 列表可触发新建表单并自动填写名称和描述；选定原 ChatGPT 工作空间后创建替代 Tunnel。随后正式安全卡片手动保存新 ID，进入运行时密钥输入阶段。新密钥表单已准备 Default project、30 天、Restricted，仅选择 Tunnels 的 Read 与 Use，尚待用户生成密钥并通过 Magi 安全输入框保存；本轮新通道、连接器和工具往返尚未完成。

本轮发现并修复：ID-only 平台列表现在通过同一行的 Edit 详情核对名称，再取消退出并绑定完整 ID；同名、多行不明、详情缺失、列表变化或用户接管时不猜测、不保存。超过四行时交给用户核对选择，避免长时间占用页面。安全输入占位提示依据实际密钥保存状态与 Tunnel 归属判断，不再把首次可跳过密钥误报为已有密钥。

当真实表单返回 workspace_required 时，配置卡片逐项展示 Name、Description 的已填内容，以及 Organizations、ChatGPT workspaces 的选择说明；组织和工作空间仍由用户确认，选定并在平台创建后再继续。显式重试创建遇到该阶段时保留工作空间交接原因，不用泛化的“创建未确认”遮盖具体指引。

独立 Electron 的 ID-only 列表完整受控流程 51/51 通过；加入工作空间交接的流程 54/54 通过，覆盖自动填写、四项说明、取消详情不保存、自动回读、密钥安全入口、通道/连接器及停用。站点与平台脚本测试 30/30、后端配置状态机 16/16、Web/Worker 类型检查、Web 模型与国际化回归、Browser core、生产构建通过。新增交接脚本首次运行在夹具 Create 按钮尚未可用时点击，导致超时；补上就绪等待后重跑通过。截图 `/tmp/magi-tunnel-guide.png`。以上是隔离平台夹具的真实 Electron 输入链验收；用户账号仍停在生成并安全保存新密钥之前，没有将其记为账号全流程完成。

2026-10-25。行为基线见 [GPT Web 开发文档 §4.7](./web-model-browser-development.md)。本次检查包括插件入口、登录交接、Tunnel、密钥、通道启动、连接器创建/刷新、异常恢复与完成后的重新配置。

## 已修复问题

- Tunnel 创建调用未确认后无法自动继续：提供显式重试，先检查，确认不存在才创建；加载中和未关闭的表单不作为不存在的证据。
- 手动只填 Tunnel id 后误入连接阶段：没有密钥时保留安全输入步骤。非法密钥在修改配置和停止旧通道之前拒绝。
- 连接器创建未确认后缺少重试路径：卡片提供再次授权；普通继续只检查，已有连接器不重复创建。
- 工具刷新超时误报成功：等待完成并检查错误，失败保留刷新检查点；已有连接器启用不等于本次刷新成功。
- 配置失败后跳回主页，妨碍用户接手：失败保留原设置页或表单，成功才返回主页。
- 取消或转交配置时可能丢失另一会话未确认的操作：取消不能覆盖其他会话的检查点，转交先持久化边界。
- 完成卡片收起后重新配置不显示：新的活动阶段恢复卡片。
- 通道就绪后一次提交被拦截便不再自动推进：会话空闲后按服务端最新阶段重新尝试。
- 同名连接器歧义与工具数量解析：多个同名项拒绝自动选择；修正工具数量正则；详情可见 Tunnel id 不匹配时拒绝刷新。
- 页面错误正文可能透传：连接器只返回固定原因码；带密钥的通道请求不派生 Debug。

## 真实 Electron 验收

使用产品的 daemon、可信 Renderer、唯一 webview、BrowserAuthority 与 Worker 原生输入链，静态构建的工作台不受开发热更新影响。平台页面和 tunnel-client 为隔离夹具，不读取用户登录分区。

| 流程 | 结果 |
| --- | --- |
| 首次配置：未安装/未激活闸门 → 登录 → 自动创建 Tunnel → 安全输入 → 通道启动 → 显式确认连接器 → 回读完成 → 停用 | 38/38 通过 |
| 异常连续流程：创建时用户接管 → 手动绑定 id → 无密钥等待 → 非法密钥 → 通道启动失败并重连 → 连接器拒绝并重试 → 完成后收起重开 → 已有连接器刷新超时并重试 → 停用 | 49/49 通过 |

同时检查右栏自动打开、只保留一个 webview、Agent 光标与交还控制、窄窗口无横向溢出、创建计数为一次、密钥不进入对话或 settings、平台错误正文不进入配置记录。

复现入口（先构建 Web 与开发 Desktop JavaScript）：

```bash
MAGI_SETUP_TEST_STATIC=1 node scripts/verify-electron-gpt-web-setup.mjs
MAGI_SETUP_TEST_STATIC=1 MAGI_SETUP_TEST_TAKEOVER=1 MAGI_SETUP_TEST_MANUAL_TUNNEL=1 MAGI_SETUP_TEST_CHANNEL_RETRY=1 MAGI_SETUP_TEST_CONNECTOR_RETRY=1 MAGI_SETUP_TEST_RECONFIGURE=1 node scripts/verify-electron-gpt-web-setup.mjs
```

脚本会先检查隔离端口，已占用时退出；可通过 `MAGI_SETUP_TEST_PORT` 选择另一组端口。

## 定向回归

- 配置状态机 12 项：插件闸门、登录恢复、未知写入不自动重放、显式重试、非法密钥无配置副作用、跨会话检查点、刷新失败恢复。
- 通道 6 项：密钥私有保存、格式拒绝、插件未启用不恢复通道、配置不含密钥。
- 站点与输入 28 项：平台缺失判定、输入结算/接管、刷新超时/错误、同名歧义、登录页、工具数量、可见 Tunnel id 不匹配。
- 槽位 MCP 9 项：配置前可读目录，工具调用必须绑定当前槽位进行中的 Turn，审批与账本归属、释放后拒绝。
- Web/Svelte、Worker 类型检查、Web 模型回归与 Browser core 检查通过。

## 验证边界

真实账号的套餐、组织权限、验证码、第三方当前页面结构，以及真实 OpenAI Tunnel 网络往返未由隔离夹具证明。本次没有发布新安装包。完成状态表示通道就绪与连接器页面回读确认，实际项目工具执行仍须遵循槽位归属与原有审批。

## 2026-10-25 新账号真实客户端检查

已在开发 Electron 客户端使用用户新登录的 ChatGPT 账号执行配置助手：内置插件从 1.1.0 升级至 1.2.0、重新激活、保留登录、自动打开右栏并复用现有 Tunnel。

真实页面发现并修复：创建入口改为「添加自定义 MCP 服务器」，提交按钮改为「创建为插件」；「添加」按钮加载时禁用，需要等待可用；风险确认框使用 `role=checkbox`。驱动增加页面交接的有限重试，仅接受宿主明确报告尚未输入的失败；已开始输入后的轮询失败保留不确定结果，不能自动重放。Tunnel 工作空间访问被拒绝时返回固定原因并给出对应处理说明。

修复后已真实执行名称、Tunnel id、身份验证填写、风险确认与创建提交。ChatGPT 返回「MCP 服务器拒绝了访问」，要求检查身份验证及 Tunnel 工作空间权限；随后 OpenAI Platform 页面报告登录令牌失效，已打开平台登录页交由用户处理。当前未创建成功，未验证真实工具往返，不能将夹具通过记为该新账号全流程通过。

本次定向验证：Worker 106 项通过；配置状态机 13 项通过；输入前交接重试判定 1 项通过；Web/Svelte 检查与 Web 模型回归通过；Browser core 通过。隔离 Electron 使用新版入口、自定义复选框、初始禁用按钮，并覆盖访问拒绝后重试与重新配置刷新，44/44 项通过。Web 与 daemon 构建成功。本轮未发布安装包。

用户重新登录平台后，确认新账号 Tunnel 列表为空。经用户当场授权，在真实平台创建了 Magi Tunnel 并绑定当前账号列出的 ChatGPT 工作空间。新版表单要求必填描述，API Key 表单要求项目、有效期及 Restricted 权限；已准备仅有 Tunnels Read + Use 的密钥表单，最终生成与复制由用户完成。新 Tunnel 尚待该账号密钥保存并连接。同步补上必填描述、未选择工作空间时暂停，以及仅显示 ID 的列表不能按名称判断缺失的防重复创建检查。

后续通过真实客户端重新打开 Tunnel 编辑表单，确认名称、描述与 ChatGPT 工作空间均已保存，没有重复创建。Magi 当前仍绑定旧 Tunnel，待新密钥就绪后一起更换；当前旧通道的「已就绪」不能证明新账号配置完成。已重新准备 Default project、30 天有效期、Restricted 且仅含 Tunnels Read + Use 的密钥表单，停在用户生成密钥之前。

补充定向检查：Worker 类型检查通过，站点与平台脚本测试 26/26、配置状态机 13/13、Web 模型 golden 回归通过，相关文件 `git diff --check` 通过。首次从仓库根目录运行脚本测试因无法解析 workspace 内的 `tsx` 未启动，改在 Worker 目录执行后通过。新账号通道连接、连接器创建及真实工具往返仍待用户填写新密钥后验证。

### 新账号通道与真实工具往返（后续完成）

用户已通过安全输入框保存新 Tunnel 的运行时密钥。随后完成真实 ChatGPT 连接器配置，并在开发 Electron 使用该账号执行文件工具。验证过程中发现两项实际故障并处理：

- MCP 工具在 Magi 中完成，但 ChatGPT 只收到空对象。成功 JSON 对象现在同时返回同值 `structuredContent` 与原有文本内容。通过不在提示词中提供的随机值验证，ChatGPT 能逐字回报本地文件实际内容。
- 新临时对话的首次后台发送连续两次失败：文本已写入，但离屏 guest 的原生鼠标未使发送按钮生效。推理页改用精确按钮的标准激活语义，并继续以新消息/生成事实确认接纳，不自动补点；首次临时确认保持相同语义。Tunnel/连接器配置仍走原生输入链和控制租约。

最终发送修复后，真实产品会话 `session-1792973531217-3` 在右栏始终收起时连续完成三轮：

1. 读取测试文件，正确返回未出现在提示词里的 128 位随机值（13 秒）。
2. 从客户端外更新该文件为另一个随机值后，要求重新读取，正确返回新值（9 秒），排除仅复述历史的情况。
3. ChatGPT 经 Magi 新建唯一测试文件并再次读取（8 秒）；独立文件系统核对内容完全一致，Magi 同时展示真实创建与读取工具项。

此前还验证了展开右栏读取随机首行、收起右栏读取中文第二行。上述文件均位于专用临时验收工作区，无用户项目文件被修改。

另发现失败卡片的「重试」仅聚焦输入框、「释放占用并重试」仅释放槽位，实际不会重发；已将中英文按钮分别改为「返回输入框」「释放占用」，使操作文案与现有行为一致。

定向检查：MCP 服务端 66/66、Worker runtime 81/81、站点与平台 28/28 通过；Worker 类型检查、Web/Svelte（0 错误、0 警告）、Web model golden、Browser core 通过。曾在仓库根目录直接运行 tsx 测试导致包解析失败，改在 Worker workspace 执行后通过。

本记录证明该新账号的 Tunnel、连接器与真实文件读写往返已打通；不代表已覆盖所有套餐、组织、网络故障、凭据到期、破坏性审批或网页问题卡片。尚未发布新安装包。

### 主对话安全卡片与异常恢复验收

配置请求从主对话发起，人工补充 Tunnel ID 和运行时密钥、连接失败修复、访问拒绝后的修改均使用主对话安全卡片，不打开 GPT Web 设置页。保存后自动续接配置；连接器创建或刷新仍保留显式授权。已保存密钥不回显，更换 Tunnel 必须补充对应密钥，取消修改清空密钥草稿并恢复已保存 ID。

本轮隔离 Electron 正常流程 **49/49**、异常连续流程 **65/65** 通过，包含用户接管、手动绑定、非法密钥、通道连接失败、访问拒绝后就地修改、连接器创建确认、完成后重开、刷新超时后重试、插件停用和秘密不进入对话/settings。使用真实 daemon、Renderer、webview 与 Worker；外部平台页面和 Tunnel 客户端使用隔离夹具，未再次改动真实账号配置。

截图检查发现仅检查「无横向溢出」不足以证明可用：390px 窗口收起右栏后，侧栏仍占 240px，安全卡片被挤窄。已修复为当前可见轨道不足以容纳对话时侧栏进入抽屉；390/720/960/1440px 增加卡片可读宽度、输入框最小宽度及控件边界检查，并核对截图。布局回归同时确认空间充足时恢复侧栏。

复测中一次夹具在导航取消请求后回包产生 `Invalid InterceptionId.`；已将这个确定的拦截失效与产品运行时异常分开处理，其他夹具回包错误仍失败。最终异常流程复测无运行时异常或意外回包失败。

定向验证：配置状态机 **15/15** 通过，Web/Svelte **0 错误、0 警告**，Web model golden、panel layout golden、Web 与 Desktop 构建及相关改动空白检查通过。本轮未发布安装包。
