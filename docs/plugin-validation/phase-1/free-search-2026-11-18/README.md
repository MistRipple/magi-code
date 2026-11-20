# Free Search 0.9.41 目录包业务验证

在 macOS arm64 Electron 目录包中，从设置页在线发现并安装固定 npm 原包。实测与同一批次的 Mgr 真机结果使用相同 daemon 和 SDK 锁摘要。该流程另行安装固定 WorkBuddy 5.21.2 为原生会话提供实际模型请求。保留[验证回执](result.json)和[联合依赖锁](package-lock.json)。

- 原设置页切换引擎为 Bing，读取原包服务确认保存，切走再打开仍显示 Bing。
- 外部配置文件提交 DDG 后，原包接口拒绝携带旧修订的保存，返回 `settings-conflict`，文件未被覆盖。重新加载后设置页显示 DDG，用户可以重新提交 Bing。
- 原页面测试引擎获得真实搜索来源。Magi 会话完成 `web_search` 并返回带 URL 的结果。启用时经原包抓取公开 DNS 查询得到 HTML，停用后原生工具从同一地址读取 JSON。
- 停用期间旧页面收到 403，重新启用后原实例请求收到 409。完整退出并启动 Electron 后，设置、全局提供方选择和已完成的会话轮次得以恢复。
- 验收入口为 `node scripts/verify-electron-free-search.mjs`。固定 WorkBuddy 模型目录及当前登录态用于执行真实模型请求。17 项检查通过。

本批未运行 Free Search 在途调用的取消和插件宿主崩溃结算。原包固定版本在线升级未在本批执行。
