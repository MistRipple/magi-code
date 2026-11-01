# GPT Web 配置验收

本文只记录当前验收入口和判定口径。历史轮次与旧固定脚本的结果不再保留（见 git 历史），也不能作为 AI 主助手的 GPT Web 配置任务的完成证据。设计与边界见 [AI 配置改造目标与验收](web-model-setup-agent.md)。

## 验收层级

| 层级 | 入口 | 证明什么 | 不证明什么 |
| --- | --- | --- | --- |
| 单元/协议 | `cargo test -p magi-api web_model_setup --lib`、`npm run test --workspace @magi/browser-automation-worker`、`npm --prefix web run test:web-model`、`npm run protocol:check`、`npm run browser-tool-catalog:check` | 校验规则、检查点闸门、状态机、凭据脱敏、协议一致 | 真实页面与浏览器行为 |
| 受控 Electron | `node scripts/verify-electron-gpt-web-setup.mjs`（先 `npm --prefix web run build` 与 `npm run build`） | 真实 daemon、Electron、Worker 与原生输入链；插件门禁；夹具 Provider 驱动真实主对话 Task 与 browser_* 工具循环；密钥不进入模型；`connector_ready` 需要本次通道窗口的真实槽位 `tools/list`，来源不明时保留待核验；主助手思考期间仍持有页面控制、交接后释放（占用状态以 daemon 的 Tab 事实为准）。真实用户输入触发接管的链路（Main 输入事件 → daemon 撤销租约并保持用户控制 → 配置停止）CDP 注入的按键不会触发 Main 的输入事件入口，受控验收无法模拟，由单测与真实 Electron 手动验证覆盖 | 真实站点 DOM 与真实模型规划质量 |
| 真实 Electron/Chromium | 用开发 Electron 打开真实 ChatGPT 与 OpenAI 平台页 | 通用点击、输入、导航后观察、用户接管与恢复 | 账号侧资源是否真的创建成功 |
| 真实 GPT Web 账号 | 从空配置走完整流程（需要用户登录和生成密钥） | Tunnel、人工密钥交接、通道、连接器与随机文件工具往返 | — |

## 真实账号验收步骤

1. 确认 GPT Web 插件已安装、启用、授权、激活。
2. 设置页点击“让 Magi 帮我配置”。登录、验证码、双重验证由用户完成。
3. Agent 在平台页创建或复用 Magi Tunnel，并准备密钥表单；卡片写明组织和项目。
4. 用户点击 Create secret key，把密钥粘贴到卡片的安全输入框，关闭结果页后保存。
5. 通道就绪后当前主助手核对或创建连接器、刷新工具；daemon 观察到同一通道代际的目录读取后，只能在来源可确认时标记完成。当前槽位传输来源不可确认时显示“通道可达、连接器归属待核验”，不能误报完成。
6. 在工作区会话里让 GPT Web 读取随机文件首行，与文件系统和审计记录核对。
7. 分别验证：重启、页面交接、用户接管、连接器重试后不重复创建或写入。

每次验收记录构建、范围（受控/真实）和失败轮次；失败与人工补点单列，不以夹具或旧连接器启用状态替代。

## 本轮实现记录（2026-10-31）

- 默认受控模式 `node scripts/verify-electron-gpt-web-setup.mjs`：40/40 项通过，包含安全保存后 daemon 自动唤醒一次原继续输入，不依赖 UI 轮询发命令。
- `MAGI_SETUP_HOLD_CHANNEL=1 node scripts/verify-electron-gpt-web-setup.mjs`：受控 Electron 44/44 项通过；覆盖插件门禁、激活不自动弹卡、主对话配置、人工交接、普通讨论、通道等待中普通消息撤销自动继续、停用/重新激活、保存回执幂等和连接器来源不明时保持 `connector_attribution_pending`。用户接管的真实 Main 输入事件仍按上表单独手动验收；旧请求归档由 Rust 回归覆盖。
- `cargo test -p magi-api web_model --lib`：42 项通过；包含导航版本竞争只读重试、scope/run 代际、凭据回执故障注入、重启恢复、停用历史完成保护、跨会话转交、在途撤权取消和槽位目录游标。
- `npm run test:browser-core`、Worker 测试 114 项、`npm --prefix web run check`、`npm --prefix web run test:web-model`、`npm run protocol:check`、`npm run browser-tool-catalog:check` 均通过。
- 受控请求前缀记录位于验收临时产物的 `prefix-benchmark.json`：33 次本地 Provider 请求、32 个相邻对比、30 对工具 schema 相同的相邻请求、2 个 Skill 激活边界；同时记录实际 HTTP 字节前缀哈希和公共字节数，已有工具历史未被逐步改写。`providerCacheUsage` 仍为 `unmeasured`，不能据此声称真实 Provider 缓存命中率；只有 Provider 返回的 `cached_tokens`/等价 usage 才能形成命中率事实。

可通过 `MAGI_SETUP_KEEP_ARTIFACTS=1` 保留产物。等待通道模式的构建基线为 `be10d992cc7d80e65bbf86647b9d1421d8c72966` 加工作区 diff 摘要 `8d60610708f08d9dc55f006c06180064cb6e601f10611b08aa2af6258eb7523a`；普通模式后续覆盖了新的 scope/run 身份检查。产物包含每对请求的序号、公共前缀字节数、历史改写数量及构建摘要，不保存请求正文。HTTP 请求诊断另外沿现有 `MAGI_MODEL_REQUEST_FINGERPRINT_DIR` 输出哈希指纹。

本轮失败及修正：夹具按 Buffer 分块解码 UTF-8 导致中文 schema 被误报变化，现改为流式 UTF-8 解码；导航后的 Authority 快照版本竞争此前会误中断，现只对只读观察做有界重试，不重放写入；等待通道夹具曾在 stop 后仍报告 ready，修正为同时核对进程存活标记。这些失败不计入通过轮次。

仍未完成的证据：真实 ChatGPT/OpenAI 账号从空配置到连接器工具往返、目标连接器来源的可信证明，以及 Provider 返回的 `cached_tokens`/等价缓存 usage。完成这些之前，产品状态应保持“受控链路已通过，真实账号和 Provider 缓存待验收”。

当前配置完成闸门在来源不明时主动保持 blocked；这是真实功能限制，并非仅缺一次测试。真实 Main 输入接管、完整入口矩阵和各安全写入故障点的全场景证据仍需补齐；跨会话转交与在途撤权已有 Rust 回归，真实跨进程入口仍需手动验收。Web 的源码/布局 golden 不等同浅深主题、RTL、200% 缩放及触摸设备的完整视觉验收，本轮未声称这些平台全部通过。

### 续推回归补充

- 配置 scope、用户接管和每次 Browser 插件租约共用 `RunCancellation`。撤权可在配置串行锁被占用时立即撤销执行资格；已送达命令等待回执结算，随后停止追加快照和后续重试。关闭 scope 同时使已克隆的旧执行身份失效，旧 generation 的清理不会影响新执行。
- 显式转交验证原会话归属、维护预约迁移、旧控制和交接失效，以及原会话的保存回执仍可查询；`save_pending` 必须先核对保存结果再转交。新会话可以在上一会话完成或取消后开始新运行，无需读取旧会话的私有 revision；旧格式终态记录也覆盖在回归中。
- 本轮新增 5 项 Rust 回归。`cargo test -p magi-api web_model --lib` 42/42、`cargo test -p magi-api web_slot_mcp --lib` 11/11、`cargo check -p magi-api`、Web/Desktop 构建和 `npm run protocol:check` 通过。
- 默认受控 Electron 40/40；等待通道模式 44/44。默认模式的完整 HTTP 公共字节前缀为 19,877–141,526 字节；等待模式为 19,877–139,717 字节。均只代表本机前缀保留情况，真实缓存 usage 仍未测量。
- 等待模式产物目录：`/var/folders/zq/nly47zbs19g601r8k_lqh2n40000gn/T/magi-ai-setup-OXdH3w`；`prefix-benchmark.json` 记录 34 次请求、33 对相邻请求、32 对稳定 schema 和 1 个 Skill 边界。构建基线为 `be10d992cc7d80e65bbf86647b9d1421d8c72966`，tracked diff SHA-256 为 `4428553748f37101fa57c6a7c4321af63949daaadec865b8ae48a72188b8e499`。新增的两个 setup 源码文件单独记录 SHA-256，避免尚未进入 git index 的源码遗漏于构建身份。临时产物不作为仓库内持久附件。

本轮仍未解决连接器可信来源证明：共享本地槽位 principal 无法区分目标 ChatGPT 连接器、健康探针和其他本机客户端。仅凭页面文字、时间相关性或客户端自报身份不足以放开完成闸门，当前继续保持 `connector_attribution_pending`。真实账号从空配置到工具往返、真实 Main 输入接管与 Provider 缓存 usage 仍按上文独立验收。

### 2026-11-01 重构后复验

- 重新构建 Web、Desktop 和 daemon 后，`node scripts/verify-electron-gpt-web-setup.mjs` 通过 40/40；`MAGI_SETUP_HOLD_CHANNEL=1 node scripts/verify-electron-gpt-web-setup.mjs` 通过 44/44。
- `npm test` 通过 Desktop 159 项、Worker 114 项和 Web 全部 golden；`npm run check`、`npm run release:guard`、`npm run protocol:check`、`npm --prefix web run check` 均通过。
- `cargo test --workspace --lib -- --test-threads=8` 首轮发现 Provider 私有上下文变化的失效原因优先级错误（历史改写被误报为 `provider_context_changed`），已修正为历史链优先；随后 `cargo test -p magi-conversation-runtime --lib -- --test-threads=8` 通过 715/715，工作区其它库在同一轮已通过 1031 项 API、278 项 Bridge 及其余 workspace 目标。
- `npm run test:prompt-cache -- --max-calls 48` 通过 21 次 HTTP 调用；`npm run test:prompt-cache -- --tool-turns --max-calls 48` 通过 32 次 HTTP 调用，账本、统计、lineage、wire key 与稳定亲和头均核对一致。usage 为本地合成数据，不证明真实 Provider 命中率。

真实账号、真实 Main 输入接管、连接器来源可信证明和 Provider 缓存 usage 仍保持未验收状态；上述受控通过结果不会替代这些证据。
