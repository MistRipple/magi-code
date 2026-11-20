# Mission Control 工具活动

在 macOS arm64 Electron 45.0.0 目录包的 Magi 设置页在线安装 `dsh-plugin-mission-control@0.2.1`。两次使用独立状态根，核对原包 SHA-256、npm integrity、SDK 锁和插件安装锁。使用本机 OpenAI SSE 提供方固定发出同一步的两次 `file_read` 调用。工具通过 Magi 原生执行，在个人会话目录读取一个存在的文件和一个不存在的文件。原包将活动发送到本机 HTTP 验收接收器。

修复前两个结果都已落盘并回灌给模型，Mission Control 未收到工具活动。修复后，同一任务收到两条工具调用记录及两条结果记录。结果分别为 `ok` 和 `error (file_read_not_found)`，对应 canonical 提交序号 13 至 16。每个工具条目保留模型 Step、provider 调用 ID 和结果消息 ID。 [修复前](baseline.json) · [修复后](fixed.json)

在修复后的同一运行中，第二轮调用原生 `ask_user_question`。等到 daemon 返回待回答问题后执行停止。原包只收到工具调用，任务转为 `ASSIGNED`。canonical Turn 记录用户取消，工具条目没有提交结果。 [同次回执](fixed.json)

本批验收覆盖主线工具成功、失败和等待用户输入时取消的实时观察。参数读取 daemon 已接纳的模型消息字符串，提供方归一化前的参数字节未纳入本批。历史补读、未知结果、异步工具及子代理归属仍待真机验证。Better Sidebar 的会话读取和右栏编辑仍待交付。
