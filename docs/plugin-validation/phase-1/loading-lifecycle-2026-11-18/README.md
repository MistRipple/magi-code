# 配置接纳与异步启动的等待关系

本批修复正式加载链路中的等待环；公开 `ctx.loader` 尚未注册，Mgr 核心业务和阶段 1 均未通过。代码、依赖、固定原包及三项原包实测结果见 [result.json](result.json)。

## 根因与结果

真实 Node 场景中，启动插件写入真实补丁文件，启用提供自己所需服务的另一个插件，然后等待该服务。旧图应用等待全部 fiber 启动结束，来源监视器也就无法处理下一次文件变更，形成死锁。现在图应用只等待模块导入和 Include／Group 的结构建立，启动回调尚未结束时如实返回 `starting`；监视器随后接纳新来源、启动提供方，原插件才进入 `running`。运行调用租约仍要求实际就绪。

配置保存还存在另一个等待方向：图应用可能等待业务更新回调，而该回调需要保存配置。来源接纳不再持有图应用锁，内部 `submit_source_edit` 完成既有来源写回事务后只返回落盘修订；执行沿既有监视器与串行图应用继续。完整原生编辑分别保留本次接纳修订和最终收敛修订。合法旧图回执不结算新修订，也不因被新意愿替换而销毁健康宿主。

仍由 `PluginManager` 和原 `state.json` 拥有唯一配置事实与提交机制。没有第二份权威配置、新执行 worker、重试旧业务或新旧宿主互调；没有改主对话 UI、原包源码或页面。单独的内部提交在没有监视器和在途应用时只保存意愿，需要调用者显式应用；尚未向插件开放这条未完成接线的入口。

## 验证

- Node 31 项回归及 Include 生成物核对通过。新增验证覆盖等待后续提供方的异步启动、回执返回后才发生的启动失败及资源回收，健康相邻插件保持可用。
- `cargo test -p magi-plugin-system -p magi-extension-host --all-targets --locked --quiet`：169 项通过，4 个依赖准备项目的原包专项默认忽略。加载运行测试 14 项，含真实文件通知解锁启动、更新回调期间接纳下一修订、旧回执后收敛到最新修订、非法来源保留、取消、断连、部分保存和恢复。测试不将 `starting` 当成 `running`。
- 取消测试改为取消真实的未完成模块导入；旧测试通过永不返回的业务启动回调保持控制请求挂起，这个前提在异步图回执合同下已不成立。取消仍证明宿主被回收、调用绑定撤销、已保存意愿保留。
- 两 crate 的 clippy `-D warnings`、相关 Rust 格式检查和差异空白检查通过。

固定 `dsh-plugin-mgr@0.2.13` 使用新的独立准备项目 `target/dsh-phase-1/projects/mgr-loading-lifecycle-2026-11-18`，保留前次项目。执行前核对 tarball 摘要及归档中 9 个原文件的安装字节，运行源码与本轮一致；依赖锁未变。显式运行了三个原包专项：图应用、真实来源文件启停、按来源所有权编辑后撤除／重建。三项均通过对应内部合同，重建后仍真实等待 `loader` 和 `webServer`，原包业务为 `not-run`。本轮未重跑原包静态准备专项。

```sh
MAGI_LOADING_PREPARED_PROJECT="$PWD/target/dsh-phase-1/projects/mgr-loading-lifecycle-2026-11-18" \
  cargo test -p magi-plugin-system --test loading_host original_mgr_ \
  --locked -- --ignored --nocapture
```

## 未完成范围

已核对 Loader 的同步变更通知、`remove: void`、Fiber 同步保存通知以及 Include `flush` 的等待边界，不能将通知之后的接线冒充执行前准入。公开 Loader／Entry／Group、Fiber 保存、Include `flush`、动态来源、安装事务和发行 daemon 协调器尚待完成；真实 Electron、Mgr 页面与安装／升级／卸载业务、完整应用重启及 Windows/Linux 未验收。本批结果不能计入原包业务通过率。
