# 加载对象与真实配置定位

本批处理 Mgr 根据 `loader.entries()` 定位来源，以及管理动作如何进入已有配置事务。完成的是内部对象层；公开 `ctx.loader` 没有注册，阶段 1 和 Mgr 页面业务未通过。原包、依赖、执行源码摘要和验证结果见 [result.json](result.json)。

## 修复与归属

- 应用的根 Include 声明使用实际来源的文件 URL。执行项目与来源目录分离、目录含空格时仍定位同一份真实文档，无虚构配置文件或工作目录假设。
- 对象读取来自真正的 Cordis 执行树。选项和成员数组不可变，Entry／Group 句柄受修订、实例和实际运行作用域约束。目标 Context 保留调用者的资源归属，生命周期投影不能直接释放目标插件。
- Loader 入口可以继续读取新修订；一次编辑不会使整个服务永久过期。旧 Entry／Group 句柄仍拒绝覆盖新配置。
- 来源动作复用 `ConfigurationAccess` 和原 `loading.edit.submit`／`loading.sources.synchronize`。提交前不修改本地 Entry，没有增加状态文件或配置写入方。无返回值编辑的失败由同步报告；报告后可以继续显式操作，不自动重放，也不永久污染后续刷新。

## 本批验证

真实 Node／Cordis 测试使用分离的来源目录，验证对象读取、来源冲突保留原业务、模块替换、文件树到内存根的移动、删除、旧句柄拒绝和调用实例失效。对象编辑的接纳处理器是明确注入的 Node 测试夹具；原生 CAS、落盘、恢复和双向请求由 Rust／真实 Node 运行测试验证。本批不宣称对象调用已通过发行 daemon 端到端完成。

在独立项目 `target/dsh-phase-1/projects/mgr-loading-objects-2026-11-18` 核对并执行同一 `dsh-plugin-mgr@0.2.13` 的未修改 `dist/internal.js`：`chooseIncludeLayer`、`locateProfile` 找到真正来源，`disableRows`／`enableRows` 写入相邻候选补丁。写文件后实例仍运行，证明保存意愿没有被误报为已接纳或已停用。此项直接调用原包 helper，没有启动 Mgr 的 `apply`、HTTP 路由或页面。

Node 59 项通过，含上述原包 helper；两个 Rust crate 全目标 181 项通过，默认忽略的 5 项原包专项在本次独立运行中全部通过。clippy `-D warnings` 和差异空白检查通过。执行前后原包 9 个文件及依赖锁摘要不变。

```sh
MAGI_LOADING_PREPARED_PROJECT="$PWD/target/dsh-phase-1/projects/mgr-loading-objects-2026-11-18" \
  npm --prefix crates/magi-extension-host/runtime/dsh test

MAGI_LOADING_PREPARED_PROJECT="$PWD/target/dsh-phase-1/projects/mgr-loading-objects-2026-11-18" \
  cargo test -p magi-plugin-system --test loading_host --locked -- --ignored --nocapture
```

## 尚未交付

直接激活／强制刷新、Group／Tree 资源终止等方法还需正式生命周期合同；内部调用明确拒绝，不能据此公开半成品服务。完整 Loader 服务形态、Include 行为、动态来源、安装事务与发行 daemon 协调器仍待完成。Mgr 实际实例仍等待 `loader`、`webServer`，不计业务通过。

Mgr 列表、安装、更新、卸载和自身更新，以及真实 Electron、完整应用重启、Windows/Linux 与性能未验收。本批不计算生态支持率。
