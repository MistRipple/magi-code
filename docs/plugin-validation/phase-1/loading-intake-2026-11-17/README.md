# 来源通知与共同接纳入口

本轮工作单元是“配置来源变更 → 静态准备 → 领域 CAS → 执行”，解决原包直接写文件后缺少正式接纳入口，以及重复通知可能重试失败启动的问题。**阶段 1、Mgr 页面和业务仍未通过。**

## 实现与事实归属

`LoadingRuntime::reconcile_sources` 复用 `PluginManager` 的来源捕获和唯一 `state.json` 提交，串行完成准备、来源重核、CAS 和执行。调用方只提供受信安装项目、原包绑定与来源组合，不能提交自造的源码快照或前一张图。显式编辑带期望修订；文件通知在取得同一串行入口后读取当前修订，再捕获磁盘候选。

来源与已接纳图没有变化，且该图已经完成收敛时，返回没有新 `application` 的结果，不向 Node 再发图应用。该结果只证明没有新的来源提交，不能解释为插件就绪；失败启动不会因重复文件通知而自动重试。显式图应用请求与来源通知分别表达操作意图，共用原有应用与回执机制。

`LoadingSourceMonitor` 使用工作区已有的 `notify` 依赖，监视来源目录。事件只使输入失效，按最新状态合并，不直接加载代码；不引入固定等待或定时轮询。首次注册后重新捕获，编辑过程中再次变化会触发后续捕获，接纳时仍核对摘要。监视器提供 `Inspecting`、`Reconciled`、`Rejected` 观察结果，来源错误保留原文件，已有可执行修订继续保留。监视后端错误不会被普通文件事件覆盖，需重建监视器；OS 要求重扫时重新检查来源。

监视器由安装协调器持有，依赖修订替换前停止并等待当前接纳结束。直接丢弃会取消工作，未确认的宿主变更仍按已有合同失效。它不保存第二份权威配置，也不调用旧宿主或翻译路径。本轮新增的协调与通知代码分别承担原先缺失的提交顺序和资源生命周期，没有增加另一套安装或配置提交机制。

## 真实原包证据

固定 `dsh-plugin-mgr@0.2.13`，复用锁定的独立准备项目。执行前核对 tarball SHA-256，并核对归档中的 9 个原文件与安装目录字节一致；原包 bundle 原样进入正式来源组合，没有改包或补假服务。

实际共享 Node 与 OS 文件通知完成：

1. 初始来源接纳为修订 1，原包配置实例真实等待 `loader`、`webServer`。
2. 直接写入 `disabled: true`，通知驱动接纳修订 2，撤除该配置实例，旧身份无法读取实例。
3. 原子保存 `disabled: false`，通知驱动接纳修订 3，以更高激活代次重建实例；它仍真实等待上述服务。

这是配置实例生命周期验证，**没有调用 Mgr 自身的页面或 HTTP 操作，也不是其核心业务通过**。临时状态根在测试结束后删除。完整摘要、环境、依赖锁、回执与未验证项见 [result.json](result.json)，专项输出见 [original-run.log](original-run.log)。

```sh
MAGI_LOADING_PREPARED_PROJECT="$PWD/target/dsh-phase-1/projects/mgr-execution-2026-11-17" \
  cargo test -p magi-plugin-system --test loading_host \
  original_mgr_file_changes_are_admitted_by_real_notifications_and_the_shared_owner \
  --locked -- --ignored --nocapture
```

该专项默认忽略，本轮已单独执行。默认套件的另外两个原包专项没有在本轮重新运行，不将历史结果拼接为整批业务验收。

## 验证与剩余工作

- 两 crate 的 `cargo test --all-targets --locked --quiet` 通过。加载运行回归 9 项覆盖真实通知、半写入失败后恢复、原子保存、项目内符号链接、停止通知后的迟到编辑、来源并发 CAS、重复通知不重放失败启动，以及既有执行与崩溃行为。
- 插件代码实际读取已落盘的 `state.json`，分别观察到修订 1 和 3 后才执行，验证提交发生在业务代码之前；不是源码字符串断言。
- 两 crate 的 `cargo clippy --all-targets --locked -- -D warnings`、相关文件格式检查通过。

完整公开 Loader/Include 编辑合同、`flush`、动态来源发现、安装事务和原生设置尚未完成。发行 daemon 尚未持有并运行这套协调器；也未切换删除旧 SDK 2 与翻译路径。本轮没有修改 UI，没有进行 Electron、Provider、Mgr 页面、在线安装／更新／卸载、自身更新或完整产品重启验收；真实文件通知仅在当前 macOS 环境验证，Windows/Linux 仍待验证。下一步应完成公开加载编辑合同并接入同一所有者，不能将这份内部通知证据当作已提供 `ctx.loader`。
