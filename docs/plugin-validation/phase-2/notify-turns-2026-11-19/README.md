# 阶段 2 Notify 轮次通知

在 macOS arm64 Electron 45.0.0 目录包上，从 Magi 设置页在线安装 `@idoall/dsh-notify@0.4.9` 和 `dsh-connect-workbuddy@5.21.2`，使用独立状态目录。归档身份、daemon 和 SDK 摘要见[回执](result.json)，本次联合依赖见[安装锁](package-lock.json)。

| 用户场景 | daemon 已提交事实 | Notify 原包结果 |
| --- | --- | --- |
| WorkBuddy `deepseek-v4.1-flash` 返回指定文本 | 第 1 轮完成，已保存助手文本 | 原包通知路由返回对应会话的第 1 轮完成通知 |
| 模型请求在途时停止 | 第 2 轮取消 | 原包没有产生完成或失败通知 |
| 隔离的 HTTP Provider 返回 401 | 第 3 轮失败，原生诊断为 `model_auth_failed` | 原包返回同一会话的第 3 轮失败通知，正文与原生诊断摘要一致 |

轮次观察来自已写入的会话事件和持久状态变化，由共享 Node 宿主发布。验证使用原包 HTTP 通知缓冲区。原包的 `shell.overlay` 仍未挂载。

同一构建复测 Message History 的实时输入、停用隔离和重启续写。两次验收使用独立状态和安装锁。[历史回归回执](history-regression.json)

复现时将 `MAGI_PLUGIN_TEST_APP` 指向待测目录包，再执行 `node scripts/verify-electron-dsh-notify.mjs`。成功场景需要有效的本机 WorkBuddy 登录态。失败及取消使用脚本内的隔离 Provider。此批未结算完整会话句柄、步骤序列和右栏业务。
