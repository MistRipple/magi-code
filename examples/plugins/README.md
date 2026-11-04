# 示例笔记插件

这是一个可用的 Magi SDK 2 示例。它演示：

- `storage` 权限和 `api.resource.read`；
- 插件自带的设置页（`ui/settings.html`，`settings` 位置的视图）：宿主不为插件生成设置表单，设置界面由插件自己提供；
- 构建期固定的单一 `extension/index.mjs` 入口；
- 受信 Node 扩展提供一个只读模型工具。
- `api.model.invoke`、`api.notify.user`、`api.ask.user`、`api.session.snapshot`、`api.credential.ref` 与 `api.blob` 只能在 daemon 签发的调用租约内使用；`api.tools.call` 与 `api.task.spawn` 沿原生 Task/工具审批链执行。

在仓库根目录运行 `node scripts/pack-plugin.mjs examples/plugins/notes /tmp/example-notes.zip`，然后在 Magi 插件设置中先预览、确认摘要、授权并激活。Magi MCP 是内置功能，不由本示例提供。
