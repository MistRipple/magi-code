# 插件执行边界

本 crate 实现[插件系统方案](../../docs/plugin-system-requirements-design.md)第 4.1、5.1 节选定的唯一后台执行基线：QuickJS 和独立受管 Worker。构建与 Desktop 打包已纳入 Worker；插件安装服务、生命周期 API 和右栏视图已接入，资源、设置、工具与工作流桥接沿 daemon 的原生权限和执行入口接入。

## 所有权

- daemon 持有 `PluginHost`、`ManagedProcessGroup` 和来自真实运行的身份。执行宿主不拥有安装状态、权限、会话事实或插件领域数据。
- Worker 启动后、读取调用报文之前，先对自身施加操作系统级限制（macOS `sandbox_init`；Linux `no_new_privs` + seccomp 白名单），随后才创建 QuickJS runtime。Windows 的 Worker 级限制尚未实现，`--sandbox-selftest` 在该平台报告 `enforced=false`。
- 每次调用在产品自己的 `magi-plugin-worker` 进程中创建独立 QuickJS runtime。Worker 只从私有 stdio 接收一个调用，执行后退出；插件不能直接取得文件、网络、环境变量、子进程或任意模块加载能力。
- 模块的默认导出接收 `(input, sdk)`，返回 JSON 值或可结算的 Promise。状态由宿主输入与后续业务 SDK 显式传递，不依赖 JavaScript 全局变量跨进程保活。
- `sdk.call(operation, payload)` 只提交操作与参数。宿主重新绑定 Invocation 身份，业务处理器仍须经过原生权限与状态校验，不能从 payload 中接受自报工作区或任务身份。
- 宿主超时覆盖排队、JavaScript 和等待能力结果的全过程。取消和 future 被丢弃都失效能力调用上下文；正常退出有短暂收口期限，超过期限使用现有受管进程释放入口终止，不建立另一套进程管理。

## 执行基线与平台边界

后台格式固定为 QuickJS + ESM，独立 Worker 复用 daemon 的受管进程生命周期。WASM 组件和插件自带原生程序不属于后台加载合同；历史选型实验通过 Git 追溯，不维护第二套运行时。

macOS 与 Linux 的 OS 沙箱由 `--sandbox-selftest` 检查；Windows 没有 Worker 级 OS 限制，只依靠 JavaScript API 隔离与资源上限，自检如实报告 `enforced=false`（设计文档 §5.1 的已接受取舍）。平台执行、正式安装器和异步 SDK 取消结算仍需分别取得对应源码/构建的运行证据，不能用某次本机 preflight 或目录包通过替代全部交付门禁。

## 产品构建与发行边界

- `scripts/dev-daemon.sh` 与 Desktop Rust 构建入口同步构建 `magi-plugin-worker`，不依赖机器上的 Node.js、Python 或外部 QuickJS。
- Electron Builder 将 Worker 放入 `resources/daemon`；macOS 打包二进制清单包含该二进制，当前安装包不做代码签名和公证。能力清单记录 Worker 哈希，解包校验从最终资源目录启动 Worker，并核对与产品一致的编译版本。
- `magi-plugin-worker --preflight` 使用当前二进制路径建立真实 `PluginHost`，经私有 stdio、QuickJS 和受控 SDK 完成 ESM/Promise/回调探测。正常 Worker 无参数仍走唯一执行入口；不接受未知参数。
- 发行脚本在清空环境的条件下运行 preflight。rquickjs 三个 crate 共用其仓库 MIT 许可，QuickJS 引擎另附 MIT 正文；许可与告知清单进入现有资源哈希和 SBOM 流程。
- 目录包与正式安装器分别验收；安装、授权与激活由 `PluginManager` 决定，Worker preflight 通过不能替代业务准入。验证入口见[脚本与验证入口](../../docs/validation.md)与[发布流程](../../docs/release-process.md)。

## 验证

```sh
cargo test -p magi-plugin-runtime --all-targets --locked
cargo clippy -p magi-plugin-runtime --all-targets --locked -- -D warnings
cargo build -p magi-plugin-runtime --bin magi-plugin-worker --release --locked
./target/release/magi-plugin-worker --preflight
./target/release/magi-plugin-worker --sandbox-selftest
```

核心回归覆盖实际 Worker 进程：模块与 Promise、缺少系统 API/import、宿主身份与拒绝、死循环、内存/输出限制、能力等待超时与取消、并发隔离、调用被丢弃时的释放及复制后的发行探测。复用现有 CI 的三个平台作业，不新增每个插件一套测试脚本。
