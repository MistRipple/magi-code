# 阶段 2 实时观察与历史重读

在同一套 macOS arm64 Electron 45.0.0 目录包上分别安装两款固定原包。每次运行使用独立状态目录与本机 SSE Provider。采集原包业务回执后完整退出 Electron，再用 daemon 的历史读取实现对两份状态目录分页读取。

| 固定原包 | 实际业务 | 重读核对 |
| --- | --- | --- |
| Mission Control 0.2.1 | 文件读取成功与缺失均收到活动。等待原生提问时停止，任务回到 `ASSIGNED` | 两次工具调用和实际结算匹配 canonical 序号 13–16。提问保留调用，历史没有工具结果 |
| Message History 1.0.6 | 检查点前完成 10 轮，重启后读取原插件历史并在同一会话追加一轮 | 封存段与增量事务共重读 11 条用户输入。插件重启后收到的消息匹配 canonical 序号 172 |

[Mission Control 回执](mission-control.json) · [Message History 回执](message-history.json) · [daemon 历史回执](daemon-replay.json) · [Mission Control 依赖锁](mission-control-package-lock.json) · [Message History 依赖锁](message-history-package-lock.json)

定向回归验证失败写盘保留旧事务、检查点轮转期间保持已打开的读取、固定水位分页，以及已附着 Input 的消息身份。当前公共 `sessions`、`sessionQuery`、Better Sidebar 右栏及跨平台仍待交付。
