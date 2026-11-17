# Magi 工具页设计

当前选定方向：C 紧凑目录。打开 tools-directory.html 查看细化交互；index.html 保留三版对比。

打开 index.html 比较，或在此目录运行 `python3 -m http.server 38991 --bind 127.0.0.1`，访问 http://127.0.0.1:38991/ 。

- dir-1.html：A 行内展开，保留作初稿对比。服务、工具、参数逐级披露。
- dir-2.html：B 左右主从，适合频繁切换不同服务。
- dir-3.html：C 紧凑目录，服务状态与数量固定列对齐。
- thumbs.html / thumbs.png：六个方向总览，前三个可交互，后三个为轻量候选。

每份 HTML 均自包含，使用真实仓库 Magi 图标与当前界面采样主题。HTML 原型数据均为虚构演示配置；不会连接或修改真实 MCP。
当前原型支持：服务与工具搜索、筛选、工具说明展开、连接异常与重试、刷新失败保留目录、启停、编辑、添加、删除确认、浅深主题。页面底部可切换正常、空目录、连接失败、刷新失败场景。
导航保留产品外观；其他设置分类未在本原型中实现，不冒充完整设置工作台。

验证：使用本机 Chrome 的 Playwright Node API，1440×1000 与 375×812；三版控制台无错误，无窄屏横向溢出。交互检查详见 verification.json。Python 版 design_lint 的浏览器体检因未安装 Python Playwright 而跳过，已用 Node Playwright 执行对应截图与行为检查；未宣称 Python 体检通过。尚未做真实后端/Provider 验收，本原型不要求此类连接。
用户已选择 C。细化后的 tools-directory.html 为当前原型，支持目录搜索自动展开、配置变化使旧目录失效、停用禁刷、刷新失败保留目录。工具项仅展示名称与用途说明，不展示调用参数；长说明按需展开。原型补充验证见 verification-selected.json。

旧三方向 HTML 和早期截图保留为历史初稿，其中的参数设计已被用户明确取消；当前以 tools-directory.html 为准。

方案 C 已实现到正式设置工具页：SettingsToolsTab 组织 MCP、Skills、内置工具与命令环境，SettingsMcpToolsSection 展示 MCP 紧凑目录，目录请求和缓存仍由原 settings store 管理，配置与连接事实来自 daemon。旧 MCP 描述悬浮层、定位计时器及重复列表样式已删除。

正式页面验收使用生产 Web 构建与独立 daemon 状态目录，入口 http://127.0.0.1:38124/web.html。真实 Tap stdio 返回 50 个工具；已核对长说明展开与收起、搜索排序与无结果、搜索不触发连接、清空恢复手动展开、真实 HTTP 连接失败与错误详情、刷新传输失败保留目录及真实重试恢复。375px 触屏视口无横向溢出，服务行管理操作热区至少 44px。截图及完整记录见 production-evidence/verification.json；这是目录展示验收，没有执行 Tap 的桌面操作工具，也没有将原型场景作为真实服务结果。

相关检查：Web check 为 0 错误、0 警告；settings-store 回归覆盖刷新失败保留目录、真实空目录、编辑／停用／删除后的迟到响应及停用禁刷；i18n 检查通过。构建通过，保留现有 WebFolderPicker 同时静态／动态导入的构建提示。当前批次未做 Electron 桌面外壳或所有 MCP 服务的业务执行验收。

折叠交互已统一到 settings.css 的共享触发区和箭头样式，覆盖 MCP、Skill、内置工具、分类及 MCP 说明／错误详情。标题区域点击与键盘 Enter／Space 切换，管理操作独立；内置能力状态预览保持可见，命令环境直接展示。搜索自动展开的 MCP 也支持手动折叠。真实 Chromium 的桌面键盘、RTL、减少动态效果、375px 触屏模拟和管理操作独立性记录见 production-evidence/disclosure-verification.json；截图以 tools-disclosures-*.jpg 为准，未将触屏模拟宣称为手机真机验收。

工具页所有刷新入口现在共用 16px 图标；内置工具和命令环境标题刷新按钮统一为 28px，带文字的 Skill「检查更新」保持相同高度，触屏扩展为 44px。桌面截图见 production-evidence/tools-refresh-alignment.jpg。
