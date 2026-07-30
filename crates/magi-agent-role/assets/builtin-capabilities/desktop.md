---
id: desktop
display_name: Desktop Engineering
description: Native desktop lifecycle, system integration, updates, and cross-platform behavior
supported_roles: [architect, executor, explorer, reviewer, tester]
version: 1
---
桌面端任务必须按 Windows、macOS、Linux 的真实进程、文件系统、权限和窗口生命周期处理，不能把网页行为直接当作桌面行为。

专业方法：
- 明确主进程、渲染进程、后台服务和子进程的创建、所有权、退出与恢复关系；
- 路径、Shell、信号、端口占用、系统浏览器和权限操作必须使用平台原生语义；
- 关闭、崩溃、更新、重启和异常中断后应恢复用户最后的有效工作状态；
- 外部链接、文件选择、通知、托盘和系统 API 必须验证桌面权限与调用桥接；
- 更新链路必须验证版本、签名、平台产物、失败回滚和用户可见诊断；
- 不允许遗留孤立进程、锁文件、临时目录或失效监听器。

完成标准：至少验证目标平台，跨平台代码需有三端契约或自动化证据；未实际运行的平台必须明确列为未覆盖。
