# GPT Web 主对话配置运行合同

本轮重构的产品目标见 [会话化配置方案](gpt-web-conversational-setup-product-plan.md)。本文描述源码中的运行边界；真实账号与工具往返的验收另见 [配置验收](gpt-web-setup-validation.md)。旧独立配置模型循环已经删除。

## 执行与所有权

用户自然语言保留为普通输入。设置入口同样提交普通主对话 Task，使用当前会话的模型、推理强度、权限、canonical 历史与用量链。插件贡献 `gpt-web-setup` Skill 和 `configure` 领域工具；Skill 按需加载，已有 active Skill 时只允许 reference 读取补充说明，原限制持续有效。

配置工具负责 inspect、begin、checkpoint、pause、cancel 和显式 transfer。插件激活、权限、来源与 digest 每次执行重新检查。begin 将执行绑定 Session/Turn/Task、配置运行、执行代际及应用级目标。人工交接关闭本轮 scope 并释放物理控制；下一普通 Turn 可以解释或调整计划，用户明确继续时才 begin。已交接的同一 Turn 不可再次 begin。

Browser 页面事实由 Authority 和受限 driver 拥有。配置期间复用 `browser_snapshot/read/click/type/fill_form/hover/press/scroll/navigate/viewport/wait_for`，其他入口默认拒绝。只接受允许 HTTPS origin、最新节点及当前页面；不接受脚本、选择器、其他 Tab、隐式提交或生成密钥的最终按钮。正文读取有预算，等待最多 5 秒，写入后旧引用失效。用户接管不会因停手自动归还控制。

浏览器链保持 Authority/lease → Desktop Control → Worker → Main CDP → guest。普通项目工具不因配置获得新权限。显式转交先结算原控制、撤销旧代际并迁移维护预约，目标会话仍需明确继续。 配置 scope 与每次 Browser 插件租约共用取消信号：用户接管、停止或在途撤权会阻止后续命令、重试和追加快照，已送达操作仍先等待回执结算。关闭 scope 使旧克隆身份失效，旧代际不能关闭新 scope。未结算的凭据保存必须先核对回执才能转交；上一拥有者结束后，新会话不需要旧私有 revision 即可开始新运行。

## 凭据、异步继续与恢复

凭据只通过可信 Desktop 安全接口提交。请求绑定 Session、setupRunId、配置 revision、handoffId 与 requestId；同一请求的非秘密参数改变返回冲突，重放只读原回执，不再次写密钥。新运行或转交替换当前请求时，旧回执与替换记录在同一 Settings 事务提交；历史回执不能重新获得执行权。

私有凭据以不可变版本文件写入，settings 提交版本引用后才算保存。配置记录先登记 pending 版本，随后提交 saved 版本；断线/重启通过引用核对本次保存，不从全局 hasApiKey 推断成功。安全请求 DTO 不实现 Debug。秘密不进入 settings、普通日志、模型或聊天。

“保存并继续”登记一个持久 Session Input，绑定运行身份、配置 revision、插件 digest、通道代际、daemon epoch 和期限。通道完成、失败或期限到达由 daemon 唤醒；UI 轮询只读状态。人工新输入、暂停、取消、停用或重启撤销自动继续。保存与继续分别回执，接续失败不要求重复粘贴密钥。

快照、配置进度和健康变化不递增配置 revision；配置事实变化才递增。执行 generation 和页面 snapshot revision 分别保护控制资格与节点引用。重启保留检查点和保存结果，清空旧交接与执行 scope。

## 完成与面板

连接器核验至少要求最新页面证据、当前通道 ready、相同通道代际以及本次执行期间槽位收到 tools/list。当前 stdio 槽位只提供共享 principal，daemon 无法可靠地将一次目录读取归因到目标 ChatGPT 连接器；因此目录读取只能显示“通道可达、连接器归属待核验”，不能单独把配置标记为 completed。来源证明仍需补齐；工具目录证据本身也不等于工具执行往返，真实往返独立验收。

面板消费 daemon 的允许动作和运行状态。插件未激活隐藏；开始时紧凑展示，新交接展开；收起清空未提交密钥并提前提示。同一交接的轮询不反复展开，不抢焦点。保存响应丢失时按 requestId 查询原回执，不重传秘密。暂停/取消走结构化控制；普通聊天忙碌不冒充配置运行。Web 只显示脱敏摘要和回本机指引。

## 代码与验证

- 插件稳定说明与贡献：`crates/magi-api/src/bundled_plugins.rs`。
- 持久状态、安全请求与继续 Input：`crates/magi-api/src/web_model_setup.rs`。
- 主助手工具与 scope：`crates/magi-api/src/web_model_setup_tools.rs`。
- 动作、证据及脱敏：`crates/magi-api/src/web_model_setup_policy.rs`。
- 原生 Browser adapter：`crates/magi-api/src/web_model_driver.rs`。
- 通道、私有凭据引用与目录观察：`crates/magi-api/src/web_model_channel.rs`。
- 面板：`web/src/components/WebModelSetupPanel.svelte`。
- 受控 Electron：`scripts/verify-electron-gpt-web-setup.mjs`。

定向验证使用 `cargo test -p magi-api web_model --lib`、Skill/主循环相关测试、`npm --prefix web run check`、`npm --prefix web run test:web-model` 与 `npm run protocol:check`。受控模型和离线站点不能证明真实账号规划质量或 Provider 缓存命中率。
