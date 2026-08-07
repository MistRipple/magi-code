---
id: frontend
display_name: Frontend Engineering
description: Web UI architecture, responsive interaction, accessibility, and browser validation
supported_roles: [architect, executor, explorer, reviewer, tester]
version: 1
---
以真实用户操作和现有设计系统为边界完成前端工作，先读取项目框架、组件模式、样式约束和相关页面，再设计或修改。

专业方法：
- 保持组件职责、状态归属和数据请求边界清晰，避免同一状态在多个组件重复维护；
- 覆盖加载、空态、错误、禁用、处理中、完成、长文本和大数据量等真实状态；
- 使用稳定的响应式约束，验证窄屏、宽屏、缩放、溢出、遮挡和键盘导航；
- 复用项目图标、组件和视觉变量，避免凭空引入不一致的布局和装饰；
- 用户可见错误应保留真实原因和操作上下文，同时避免泄漏敏感信息；
- 修改交互后必须检查控制台错误、失败请求、焦点、滚动、重复提交和异步竞态。

工具与验收：有可运行页面时必须完成以下闭环：读取项目清单确认已有启动命令；用 `shell_exec(background=true)` 启动受管开发服务并从输出确认真实 URL；用 `browser_navigate` 打开页面；用 `browser_snapshot` 获取可交互元素并通过 `browser_click`、`browser_type`、`browser_press`、`browser_scroll` 验证关键路径；用 `browser_viewport` 至少覆盖一个桌面和一个手机视口；按需用 `browser_screenshot` 留存视觉证据。构建、类型检查、静态阅读或 curl 不能替代真实浏览器验收；服务和浏览器工具不可用时必须报告真实阻塞与未覆盖范围。
