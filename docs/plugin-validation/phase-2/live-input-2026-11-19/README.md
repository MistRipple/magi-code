# 阶段 2 原包实时消息历史

同一 macOS arm64 Electron 45.0.0 目录包，从 Magi 设置页在线安装 `dsh-plugin-message-history@1.0.6` 和 `dsh-connect-workbuddy@5.21.2`。使用 WorkBuddy 的真实 `deepseek-v4.1-flash` 完成测试会话。两份归档摘要、联合安装锁摘要和运行时构建身份见[回执](result.json)与[联合依赖锁](package-lock.json)。

- 提交普通文本后，daemon 在同一持久事务提交用户消息和 Input 绑定。Message History 收到该会话的 `user/message`，原包路由返回新输入，原包私有记录内的会话 ID 与持久事务一致。
- 停用期间提交的新输入未进入原包历史。停用时旧路由返回 403，重新启用后旧实例请求返回 409。
- 完整重启后，原包读回已有历史，随后收到同一会话的下一条实时输入。原包在隔离验收进程中使用独立私有存储目录。

本批覆盖实时普通文本和原包自身的历史恢复。会话查询及历史回填、采用独立生命周期的代理句柄、附带图片的用户消息、模型步骤与轮次结果、宿主故障期间的历史补读和右栏交互仍待交付。

在包含本批代码的本机目录包上运行 `node scripts/verify-electron-dsh-message-history.mjs`。该脚本需要本机有效的 WorkBuddy 登录态，可通过 `MAGI_PLUGIN_TEST_APP` 选择待验收的目录包。

## 历史补读接口核对

锁定的 `@deepseek-ai/dsh-session-query@0.2.2-rc.3` 原包 SHA-256 为 `84983629e3ba81881ba38a9f0a1596d4c9a768970e6a07c4982223bb8dfe7374`。该版本 `src/corpus.ts` 的 `listSessions()` 为活跃与持久会话返回 `{ header, live, persisted, formatStatus? }`，会话 ID 位于 `header.id`。

固定 Message History 1.0.6 的 `package/lib/importer.js` 第 62–69 行仅读取条目根部的 `id` 或 `sessionId`。两者均缺失时直接跳过，未调用 `readSession()`。随后可能扫描 DSH 原生文件。此发现来自两份官方归档的接口核对，历史补读仍需在支持公共合同的原包上实测。
