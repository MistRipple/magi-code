# GPT Web AI 配置改造目标与验收

## 目标

Magi 使用用户现有的原生工具模型，从实时页面内容自主规划完整 GPT Web 配置。参考地址和任务约束是模型上下文，不是页面规则引擎。后台只负责真实资源、安全边界及持久事实，不替 AI 选择控件或维护点击序列。

## 交付边界

| 项目 | 实现 |
| --- | --- |
| 唯一入口 | 插件已安装、启用、授权、激活且清单贡献 `setup` 命令（`ApiState::web_model_setup_available`）。动作只有 start / continue / cancel；设置里的“检查并配置连接器”、卡片继续、重试连接都复用 continue |
| 自主操作 | 快照和正文 → AI 原生工具调用 → 通用浏览器原语 → 新快照；导航、顺序、等待和重规划由 AI 决定 |
| 单一浏览器链 | Authority/lease → Desktop Control → Worker → Main CDP → 当前应用级 guest |
| 本地事实 | daemon 保存 Tunnel、密钥、通道健康和维护占用；UI 只展示检查点及实际提示 |
| 人工交接 | 登录、验证码、密钥生成/复制/安全保存、组织/项目歧义；保留当前表单继续观察。用户接管浏览器时 daemon 立即停止本次运行（reason=`user_takeover`），继续前先重新快照。**运行期间 Agent 持有页面控制租约**，跨命令保留（剩余有效期不足时续租），运行结束（交接、完成、取消、失败）一律释放；若每条命令后就释放，Agent 等模型决策的那几秒里没有任何控制，用户此刻碰页面 Main 无法识别为接管，Agent 会拿着旧计划继续操作 |
| 作用域核对 | `tunnel_ready` 须带页面当前显示的组织，`key_prepared` 须带组织和项目，均须逐字出现在最新快照；daemon 把它们写进用户可见说明，供生成密钥前核对 |
| 完成证据 | `connector_ready` 除页面证据和 Tunnel 归属外，daemon 必须观察到本次运行内 ChatGPT 经槽位端点读取了 Magi 工具目录（`tools/list`）；只靠页面文案不会被接受。真实工具往返另行验收 |
| 操作边界 | 点击最终的 Create secret key 按钮和删除/撤销类控件被拒绝；打开表单的入口可点击。诊断日志只记录页面路径，不含查询串、正文或凭据 |
| 结果不确定 | 不重放写入；先观察是否生效。导航交接只重试只读快照，不重复点击 |
| 重复配置 | 核对名称和 Tunnel 归属后复用；禁止盲目新增、删除或撤销 |
| 运行记录 | 模型调用进入 ModelStep/用量记录；浏览器操作进入 canonical 工具项；模型收到标准 assistant/tool 历史 |
| 凭据隔离 | 模型和工具历史脱敏；安全输入沿专用 Desktop 请求保存，不进聊天和命令参数 |
| 旧路径 | 固定站点配置脚本、专用 wire 命令（含连接器只读检查 `web_connector_status`）、兼容分支和对应原因码已删除，仅保留 AI 自主配置入口 |

## 完成条件

1. 单元和协议回归覆盖节点失效、未知创建结果、凭据脱敏、插件门禁、取消、恢复与资源归属。
2. 真实 Electron 中验证通用点击/输入/悬浮、导航后观察、用户接管与恢复；受控平台页标注为受控验收。
3. 真实账号从空配置完成 Tunnel、人工密钥交接、通道和连接器配置，然后用随机文件完成工具往返并独立核对。
4. 复用已有匹配连接器、同名不匹配连接器和中断恢复分别验证；不得把配置成功或旧连接器启用等同于工具往返成功。
5. 每次验收注明构建和真实/受控范围。旧固定脚本的成功轮次不能作为本次 AI 改造完成证据。

## 代码地图

- AI 指令及执行：`crates/magi-api/src/web_model_setup_agent.{md,rs}`。
- daemon 检查点和凭据交接：`crates/magi-api/src/web_model_setup.rs`。
- 通用页面驱动：`crates/magi-api/src/web_model_driver.rs`。
- 工具目录读取事实：`magi-mcp-server` 的 `with_catalog_observer` → `WebModelChannelRuntime::slot_catalog_served_since`。
- 受控验收：`scripts/verify-electron-gpt-web-setup.mjs`（夹具模型只依据快照内容决策，夹具页面刷新时经槽位 socket 真实读取一次工具目录）。
- 原生浏览器原语：`browser-automation-worker/src/runtime.ts`、`page-script.ts`。
- 卡片与设置：`web/src/components/WebModelSetupPanel.svelte`、`SettingsWebModelSection.svelte`。

真实测试结果以当次运行记录为准；未完成真实空配置和工具往返前不得宣称稳定交付。
