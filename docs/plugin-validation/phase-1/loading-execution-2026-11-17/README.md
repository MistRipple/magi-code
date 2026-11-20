# 已接纳图的真实宿主执行

本轮解决加载模型的执行与失效问题，目标样本仍为 `dsh-plugin-mgr@0.2.13`。**阶段 1 未通过，Mgr 业务未通过。** 本轮没有修改 Magi 界面，没有接入或发布新旧运行时路由。

## 所有权与执行

`PluginManager` 使用原有 `state.json` 接纳来源、配置修订及配置实例身份。一个包可有多个配置行实例，实例激活代次持久分配，包的激活顺序保持稳定。`LoadingRuntime` 将已提交图和身份交给同一共享 Node 宿主；Node 使用真实 Cordis Loader、EntryGroup 与 fiber 生命周期执行，不另存安装或配置事实。内部执行 Loader 隔离于公开服务，原包仍无法注入尚未交付的 `loader`。

普通配置沿原 fiber 更新，模块／执行上下文替换和重新启用使用新激活代次。应用回执须匹配修订、宿主、操作和全部实例身份；`Applied` 只说明图完成一次收敛，不代表所有实例运行或业务成功。取消未确认的图应用会回收宿主并撤销绑定，保留已保存配置；进程在控制调用之外崩溃也会撤销运行绑定。重启恢复配置意愿，重新签发运行身份。

## 实测发现与修复

上层 Include 等待注入服务时，子实例尚无 fiber，之前被误判为加载失败；依赖恢复后，旧实现也无法重新挂接其 fiber。现在从实际祖先依赖投影 `waiting`，服务恢复后由 Cordis 创建与重新挂接 fiber。

Cordis 在最终处置通知前会清空 fiber 的 `uid`，原先只匹配当前 `uid` 会漏掉这次通知，导致调用取消信号没有失效。现在在采用 fiber 时记录其原身份，父级挂起立即撤销旧调用租约，恢复后旧租约不能复活。显式自停用仍是终止，与依赖挂起分别验证。

## 固定原包结果

本轮在独立准备项目 `target/dsh-phase-1/projects/mgr-execution-2026-11-17` 暂存当前宿主代码，复用原锁定依赖。执行前核对 tarball SHA-256，并逐个核对归档中的 9 个原文件与安装目录字节一致。来源使用原包的真实 bundle 层，不修改包、不添加假服务。

实际共享 Node 接收 `PluginManager` 已提交图后，Mgr 原包进入真实 Cordis 的 `waiting`，`missingServices` 为 `loader`、`webServer`；重复收敛保留同一配置实例身份。原包 bundle 未变化。完整回执、环境、依赖锁与相关代码摘要见 [result.json](result.json)，原始专项输出见 [original-run.log](original-run.log)。

```sh
MAGI_LOADING_PREPARED_PROJECT="$PWD/target/dsh-phase-1/projects/mgr-execution-2026-11-17" \
  cargo test -p magi-plugin-system --test loading_host \
  original_mgr_application_waits_for_real_services_without_claiming_business_success \
  --locked -- --ignored --nocapture
```

该专项默认忽略，本轮已显式运行。测试状态根为临时目录，结束后删除；不能据此声称产品内已经安装、激活或运行了 Mgr 的业务。

## 检查与未完成范围

- 21 项 Node 回归通过，覆盖来源准备、共享服务、配置执行、父级挂起与恢复、旧租约及自停用。
- 两 crate 的 `cargo test --all-targets --locked` 通过；新增跨进程回归覆盖同包多实例、配置更新、停用／重启用、导入失败隔离、取消未确认应用、真实进程退出和持久身份恢复。默认忽略的原包专项与普通套件分开记录。
- 两 crate 的 `cargo clippy --all-targets --locked -- -D warnings` 通过。

下一工作单元仍是正式来源编辑接纳与完整公开 Loader/Include 合同，随后接入 daemon 协调器、文件通知和原生设置。`flush`、动态来源、安装事务、页面、Mgr 列表／启停／安装／更新／卸载及自身更新均未完成业务验收；Electron、Provider 和产品重启未在本轮验证。旧 SDK 2 与翻译路径尚未切换清退，不能将这份内部执行证据计为平台交付。
