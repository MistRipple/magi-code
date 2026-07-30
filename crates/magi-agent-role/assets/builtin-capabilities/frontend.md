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

工具与验收：有可运行页面时必须使用真实浏览器验证关键路径；至少覆盖一个桌面和一个移动视口，并记录未覆盖范围。只通过类型检查或静态阅读不能宣称前端体验完成。
