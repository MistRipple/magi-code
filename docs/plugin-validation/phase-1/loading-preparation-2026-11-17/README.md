# 配置来源接纳与原包图准备

本轮验证的是“真实来源 → 共享 Node 准备 → PluginManager 提交期望修订”。没有激活原包，没有 daemon 正式协调器或 Electron 业务结果，阶段 1 仍未通过。

## 当前实现

- `PluginManager` 在现有 `state.json` 保存来源正文、摘要、来源根及已接纳图，使用 CAS 防止旧编辑者覆盖新修订；提交前重新核对来源文件，不改写被第三方编辑的来源。
- 期望配置与运行回执分开。宿主代次和加载操作序号持久分配，旧修订、旧宿主、旧操作和重复回执不能结算当前操作。重启只恢复期望配置，不恢复“旧进程仍已应用”的投影。
- Node 的内部准备端口使用锁定 Include 的补丁合成规则，处理静态 include、group、稳定匿名行和真实原包入口；模块元数据按引用保存。表达式不求值；未捕获的来源、动态 include 路径和缺失的包绑定不能靠猜测补全。
- 可编辑来源目录与执行依赖项目分别绑定。业务代码未执行，`ctx.loader` 未注册，未调用旧翻译路径。

## 实际原包验证

固定 `dsh-plugin-mgr@0.2.13`，使用上一轮准备项目及原包的真实 bundle 内容，归档和清单摘要在 `result.json`。测试在临时的真实管理器状态根捕获来源，经实际共享 Node 连接完成准备，再由 `PluginManager` 接纳为修订 1。之后实际写入禁用补丁并接纳为修订 2，确认原包 bundle 文件未变；这些是配置意愿，不能解释为插件已经启停。

执行命令（先把当前 `runtime/dsh/*.mjs` 与该目录 `package.json` 放到准备项目的 `runtime/`，复用已锁定依赖）：

```sh
MAGI_LOADING_PREPARED_PROJECT="$PWD/target/dsh-phase-1/projects/mgr-0.2.13" \
  cargo test -p magi-plugin-system --test loading_host \
  fixed_mgr_original_bundle_produces_a_committable_configuration_without_activation \
  --locked -- --ignored --nocapture
```

该专项默认忽略，已按上述命令单独实际运行并通过。测试结束会删除临时状态根，不留下一个真实安装运行的插件实例。它不替代安装、激活、在线更新或 Mgr 页面的业务验收。

## 其他检查与剩余范围

相关 Node 回归 17 项通过；领域回归覆盖 CAS、文件变化、重复通知、旧回执、失败保留与重启；普通跨进程准备回归通过。两 crate 的 `--all-targets --locked` 回归通过（原包专项在默认套件中忽略，已单独运行）；clippy 无告警。

下一条主线是正式 Loader/Include 的编辑合同与已提交图的执行收敛，并接入 daemon 协调器、文件变更及原生设置。动态来源、完整 `flush`、安装事务、取消与恢复、真实页面和各原包业务仍未验收；原有 SDK 2 与翻译实现尚未切换删除。
