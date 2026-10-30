# GPT Web 配置验收

本文只记录当前验收入口和判定口径。历史轮次与旧固定脚本的结果不再保留（见 git 历史），也不能作为 AI 配置 Agent 的完成证据。设计与边界见 [AI 配置改造目标与验收](web-model-setup-agent.md)。

## 验收层级

| 层级 | 入口 | 证明什么 | 不证明什么 |
| --- | --- | --- | --- |
| 单元/协议 | `cargo test -p magi-api web_model_setup --lib`、`npm run test --workspace @magi/browser-automation-worker`、`npm --prefix web run test:web-model`、`npm run protocol:check`、`npm run browser-tool-catalog:check` | 校验规则、检查点闸门、状态机、凭据脱敏、协议一致 | 真实页面与浏览器行为 |
| 受控 Electron | `node scripts/verify-electron-gpt-web-setup.mjs`（先 `npm --prefix web run build` 与 `npm run build`） | 真实 daemon、Electron、Worker 与原生输入链；插件门禁；夹具模型按快照内容驱动工具循环；密钥不进入模型；`connector_ready` 依赖 daemon 观察到的真实槽位 `tools/list`；Agent 思考期间仍持有页面控制、交接后释放（占用状态以 daemon 的 Tab 事实为准）。真实用户输入触发接管的链路（Main 输入事件 → daemon 撤销租约并保持用户控制 → 配置停止）CDP 注入的按键不会触发 Main 的输入事件入口，受控验收无法模拟，由单测与真实 Electron 手动验证覆盖 | 真实站点 DOM 与真实模型规划质量 |
| 真实 Electron/Chromium | 用开发 Electron 打开真实 ChatGPT 与 OpenAI 平台页 | 通用点击、输入、导航后观察、用户接管与恢复 | 账号侧资源是否真的创建成功 |
| 真实 GPT Web 账号 | 从空配置走完整流程（需要用户登录和生成密钥） | Tunnel、人工密钥交接、通道、连接器与随机文件工具往返 | — |

## 真实账号验收步骤

1. 确认 GPT Web 插件已安装、启用、授权、激活。
2. 设置页点击“让 Magi 帮我配置”。登录、验证码、双重验证由用户完成。
3. Agent 在平台页创建或复用 Magi Tunnel，并准备密钥表单；卡片写明组织和项目。
4. 用户点击 Create secret key，把密钥粘贴到卡片的安全输入框，关闭结果页后保存。
5. 通道就绪后 Agent 核对或创建连接器、刷新工具；daemon 观察到工具目录读取后才标记完成。
6. 在工作区会话里让 GPT Web 读取随机文件首行，与文件系统和审计记录核对。
7. 分别验证：重启、页面交接、用户接管、连接器重试后不重复创建或写入。

每次验收记录构建、范围（受控/真实）和失败轮次；失败与人工补点单列，不以夹具或旧连接器启用状态替代。
