# Fiber 来源保存与执行作用域准入

本批把正式配置实例的根 `fiber.update(config)` 接入已有 daemon 来源事务。公开 `ctx.loader`、Mgr 核心业务与阶段 1 仍未完成。本批结果见 [result.json](result.json)；补充基础配置比较前的内部原包运行单独保留在 [before-config-preimage.json](before-config-preimage.json)，不混合两次运行宣称业务通过。

## 正式链路

插件发起 Fiber 保存 → 校验实际作用域并捕获候选 → 共享连接 `loading.edit.submit` → daemon 核对实例、版本、作用域和修订 → Node 只编译来源编辑提案 → daemon 再次准入并沿原来源日志与 `state.json` 提交 → 原监视器／串行图应用更新实际配置。

没有新增配置所有者、安装项目入口、业务执行 worker 或本地保存后回补链路。公开 Loader 尚未提供，源文件和模块仍是原包内容；本批未修改 Magi 主对话 UI。新增的宿主入站处理器只接来源提交并自动观察实例状态，其余服务交给同一连接原处理器；弱引用和一次绑定避免持有宿主的循环资源与跨宿主重绑。

源保存不能只验证激活身份：依赖中断后，同一激活可能重新挂接实际 fiber。新 `configurationEpoch` 表达该 Cordis 执行作用域的短期租约，不是持久化激活代次。Node 撤销旧租约，daemon 从带完整身份的状态通知维护只读投影，并在准备前和提交前复核。旧通知不能使租约复活；投影不拥有配置、安装或启停事实。

保存前不改变运行配置，回执只证明持久接纳。提案还比较 `expectedConfig`，防止新修订已确认而旧 fiber 尚未消费时覆盖另一个配置编辑。同一作用域连续保存可沿自身已确认的意愿继续，失败不重试。显式 `noSave` 保留 Cordis 的实例内状态语义，不提交全局来源、启停或权限。

## 运行证据

- 实际组内插件在启动中调用 `ctx.fiber.update`，经真实 Node／Rust 双向 RPC 保存修订 2；原包业务代码读取已落盘修订后执行更新配置，未发生自身启动等待环。
- 同一插件在启动期间连续等待两次保存，最终接纳修订 3，实际配置为第二个值；插件只启动一次，已提交的来源意愿没有被当成后台重放任务。
- 错误宿主、包、实例、激活、版本、作用域以及调用方提供额外安装目录均拒绝；持久状态与来源原文保持不变。即使实例和修订正确，基础配置不匹配也拒绝。
- 真正停用提供方、再恢复提供方：消费者激活身份不变，旧保存作用域及重复旧 running 通知均不能恢复写入权；新作用域可正式提交修订 4。
- 显式不保存的更新只改变实例运行值，来源、接纳修订和原图仍保持原值；缺少来源所有者时普通保存直接失败，运行配置不变。
- Node 32 项回归及 Include 生成物核对通过；两 crate 的 `cargo test --all-targets --locked --quiet` 共 174 项通过、4 项默认忽略；其中加载运行测试 19 项。两 crate 的 clippy `-D warnings`、相关 Rust 格式和差异空白检查通过。

固定 `dsh-plugin-mgr@0.2.13` 在独立准备项目 `target/dsh-phase-1/projects/mgr-fiber-save-cas-2026-11-18` 复测，执行前核对 tarball 摘要及全部 9 个原包文件的安装字节，运行源码与本批最终代码一致，依赖锁不变。三个内部专项（图应用、文件通知启停、来源所有权编辑）通过，实例仍真实等待 `loader` 与 `webServer`。本轮没有把机制保存测试或这三个内部专项计为 Mgr 业务通过，也没有重跑默认忽略的静态准备专项。

```sh
MAGI_LOADING_PREPARED_PROJECT="$PWD/target/dsh-phase-1/projects/mgr-fiber-save-cas-2026-11-18" \
  cargo test -p magi-plugin-system --test loading_host original_mgr_ \
  --locked -- --ignored --nocapture
```

## 剩余范围

公开 Loader／Entry／Group 的读取与编辑对象、Include `flush` 和动态来源、安装事务及发行 daemon 协调器仍待完成；完成所需合同后才能注册公开 `loader`。真实 Electron、Mgr 页面与安装／升级／卸载业务、完整应用重启及 Windows/Linux 尚未验收。下一步继续这些公共对象合同，不将内部 Fiber 保存接线视为插件平台已交付。
