# 内部来源编辑与持久化事务

本轮围绕公开 Loader 的来源编辑依赖，完成内部创建、更新、移除及来源写回事务；**公开 `ctx.loader` 尚未注册，Mgr 页面业务与阶段 1 均未通过**。

## 修复与实现

上游 Include 已有 journal／patch 所有权规则。本轮从锁定版本 `@deepseek-ai/cordis-plugin-include@1.1.0` 的公开源码导出构建纯数据 helper，保留 MIT 许可，未修改上游源码或插件原包。`include-journal.entry.mjs` 是构建入口，`include-journal.generated.mjs` 不手改；`include:check` 在 Node 测试前核对生成结果。构建使用固定 `esbuild@0.28.2`，运行时不需要 TypeScript loader 或构建器，不引入另一个执行内核。

实测还发现，原准备器在补丁之后才给匿名行分配 ID，使针对已公开匿名 ID 的补丁被忽略。已按 Include 顺序先分配原始来源与插入行身份，再应用补丁，并保留该身份用于写回。

`configuration.edit.prepare` 只编译来源提案，不写文件、不修改运行条目。来源编辑遵循实际所有权：文件中的值回到文件，补丁拥有的值回到对应补丁，插入行回到插入来源；子 Include 的补丁变化作为父 Include 的配置变化向上写回。组内创建与跨 Include 来源移动共用这套规则，`!!js` 仍是未求值数据。

`LoadingRuntime::edit_sources` 使用已有 `PluginManager` 作为唯一提交所有者：

1. 核对期望修订，捕获来源，拒绝未接纳的外部文本变更。
2. 共享 Node 编译来源编辑和下一张未执行图。
3. 在原 `state.json` 保存 `pendingEdit` 意图，再逐个原子保存候选文件。
4. 全部正文与计划摘要一致后，沿原 `accept_loading_configuration` CAS 提交，同时清除意图，再应用图。

跨文件编辑需要持久化意图，原因是单个文件原子替换不能使整个来源集合原子生效。部分保存的组合不能被普通文件通知当成另一张图提交。恢复仅接受已记录的修改前／后正文，未知外部文本原样保留并报告冲突；恢复配置不重放插件业务调用。无变更编辑不创建事务、不改持久状态、不重新应用图。

这些新增状态属于同一插件管理所有者，不是另一份可独立生效的配置。原有 SDK 2 与翻译路径尚未硬切换，本轮没有增加新旧互调、回退、包名分支、假服务或 UI 改写。

## 实际验证

- Node 29 项回归和生成物检查通过。来源编辑覆盖实际写回所有权、组、嵌套 Include、跨文件移动、匿名身份、表达式数据及非法操作。
- 两 crate 的 `cargo test --all-targets --locked --quiet` 通过。加载运行回归共 12 项，其中新增验证包含：原包代码读取已落盘修订后才执行；旧修订和外部文本冲突不会覆盖文件；真实编辑意图写入后模拟只保存第一个文件，再重开管理器恢复其余文件；部分图不能提交；未知文本保留到显式恢复。
- 完整套件后补充了无变更编辑的断言，该专项已单独重跑通过。两 crate 的 clippy `-D warnings`、相关 Rust 格式检查通过。

固定 `dsh-plugin-mgr@0.2.13` 使用独立准备项目 `target/dsh-phase-1/projects/mgr-source-edit-2026-11-18`。执行前核对 tarball SHA-256 和归档中 9 个原文件的安装字节。实际内部来源编辑将配置实例从等待状态撤除，再以新激活身份重建，形成配置修订 1、2、3；只改了拥有该行的候选 `bundle.patch.yml`，候选中的其他来源与原包本身均未改。重建后仍实际等待 `loader`、`webServer`。

```sh
MAGI_LOADING_PREPARED_PROJECT="$PWD/target/dsh-phase-1/projects/mgr-source-edit-2026-11-18" \
  cargo test -p magi-plugin-system --test loading_host \
  original_mgr_source_edits_follow_upstream_ownership_and_the_existing_configuration_commit \
  --locked -- --ignored --nocapture
```

该专项默认忽略，本轮已显式运行；默认套件另外三个原包专项未在本轮重跑。结果、环境、代码和依赖摘要见 [result.json](result.json)，实际专项输出见 [original-run.log](original-run.log)。临时管理器状态在测试后删除。这不是 Mgr 自己的页面、路由或核心业务验收，不能计为该样本业务通过。

## 剩余范围

公开 Loader／Entry／Group 对象、Fiber 保存、Include 同步和 `flush` 尚未完整接线，动态来源和安装事务也未完成；不能仅凭内部 CRUD 提供公开 `loader`。待完成这些合同并接入发行 daemon、原生设置后，再运行 Mgr 列表、目标插件启停、安装／更新／卸载及自身更新任务，最后完成阶段清退。

本轮没有验证真实 Electron、Provider、完整应用重启或 Windows/Linux 文件行为；没有将配置实例撤除／重建冒充业务成功。下一步仍须围绕相同原包完成公开加载对象与服务生命周期，不能扩大成独立于插件任务的重构。
