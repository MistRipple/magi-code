# 国际化覆盖检查（2026-10-25）

当前结论：Web 主工作台的字典覆盖和常用界面已补漏，不能据此声称整个产品已经 100% 国际化。原生宿主、服务端诊断与插件内容仍有边界。

## 检查与修复

| 范围 | 发现 | 本轮结果 |
| --- | --- | --- |
| 中英文字典 | 原有 3159 个键集合一致，但调用方仍可能引用不存在的键 | 修复工具依赖 checking 与浏览器 loading 两处引用；补充 146 对翻译后为 3305 对 |
| 运行记录 | 事件类型、状态和拆分标签硬编码中文 | 接入 runtimeDiagnostics 字典，保留 daemon 的原始 summary/detail |
| 任务与代理详情 | 空标题、任务类型、运行状态、异常提示硬编码；上下文类型在组件里直接判断语言 | 统一接入字典；任务枚举、用户标题与任务内容保持原样 |
| 操作反馈 | 通用成功/失败后缀写死中文；变更列表刷新失败提示硬编码 | 接入共享插值文案 |
| 外观 | 内置皮肤名称与描述来自中文包；前端校验及 HTTP 兜底错误硬编码 | 按稳定主题 ID 翻译展示名称/描述；原包及自定义名称不变；前端错误接入字典 |
| 插件与桌面更新 | 前端请求失败兜底及桌面更新兜底写死中文 | 本地化前端操作文案，服务端返回的错误原因继续原样显示 |
| 时间、数字与语言标记 | MCP/运行记录时间、部分统计数字依赖操作系统默认语言；HTML lang 未跟随语言切换 | 显式传入界面 locale；工作台响应式同步 document.documentElement.lang |

## 自动验证

新增 `npm --prefix web run test:i18n` 并加入 Web 常规测试入口。检查两份字典的重复键、键集合、空值、英文中的中文、插值变量，以及 294 个生产 TS/Svelte 源码文件的静态 `i18n.t` 引用。动态拼接键不在静态引用检查的覆盖承诺内。空闲状态的空文案与完成日期的语言差异字段有显式例外。

SSR 回归验证中英文来回切换、任务标签和时长即时更新、内置主题名称、自定义内容保持原样，以及外观格式校验错误的翻译。此检查不是运行每个条件分支或验证翻译质量的替代品。

已通过：`check`（0 errors / 0 warnings）、`build`、`test:i18n`、`test:appearance`、`test:runtime-state`、`test:notification-runtime`、`test:desktop-updater`、`test:external-link`、`test:settings-store`、`test:mcp-server`、`test:browser-settings`、`test:relative-time`、`test:tool-call-display`、`test:active-agent-center`、`test:conversation-disclosure`。

## 真实界面验证

使用 daemon 38123 的最新静态构建，在已有会话切换英文：HTML lang 为 en-US，侧栏主题为 Magi Dark，完成回执显示 Total time。逐项打开模型、工具、浏览器、代理、规则、安全、统计、外观、项目、插件、GPT Web、MCP server 共 12 个设置分类；检查当前可见内容，未主动触发写入、错误或运行任务。内置皮肤库名称和描述已为英文；模型/引擎的自定义中文名称保留。工具页仍显示一条服务端提供的中文技能描述。

通过设置页恢复中文后，皮肤名称和 HTML lang 即时恢复；返回原会话，未发送新消息。截图：`target/ui-i18n-audit-2026-10-25/appearance-en.png`、`appearance-zh.png`。

未在本轮重新启动或操作 Electron 原生界面，未枚举每个表单的失败分支。

## 尚未覆盖的内容

- Electron Main：`apps/desktop/src/main/index.ts` 的托盘“打开 Magi / 退出 Magi”和后台服务启动失败对话框仍为中文。本轮仅检查源码；后续需沿宿主启动与语言同步链路实现，并用真实 Electron 验证。
- 服务端和协议诊断：Rust/daemon 返回的错误说明、历史运行记录、前端 wire 校验诊断部分仍为中文。界面继续展示真实原因，不通过匹配中文字符串冒充翻译；要完善需要由产生消息的一方提供语言或结构化错误合同。
- 技能/插件内容：daemon 技能描述、第三方插件 iframe/manifest 提供的内容由其所有者决定语言；本轮只处理 Magi 自己的插件操作提示。
- 用户消息、AI 正文、文件名、自定义模型/主题名保留原始内容。验收 fixture、demo、注释和开发日志不计入产品界面翻译率。

本轮所有修改保留在当前 main 工作区，未提交；其他并行工作的已有改动保持原样。
