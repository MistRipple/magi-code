# 插件执行边界

本 crate 实现[插件系统方案](../../docs/plugin-system-requirements-design.md)第 4.1、5.1 节选定的唯一后台执行基线：QuickJS 和独立受管 Worker。构建与 Desktop 打包已纳入 Worker；插件安装服务、生命周期 API 和右栏视图已接入，业务 SDK 与原生工具/设置/工作流桥接仍按阶段计划推进。

## 所有权

- daemon 持有 `PluginHost`、`ManagedProcessGroup` 和来自真实运行的身份。执行宿主不拥有安装状态、权限、会话事实或插件领域数据。
- 每次调用在产品自己的 `magi-plugin-worker` 进程中创建独立 QuickJS runtime。Worker 只从私有 stdio 接收一个调用，执行后退出；插件不能直接取得文件、网络、环境变量、子进程或任意模块加载能力。
- 模块的默认导出接收 `(input, sdk)`，返回 JSON 值或可结算的 Promise。状态由宿主输入与后续业务 SDK 显式传递，不依赖 JavaScript 全局变量跨进程保活。
- `sdk.call(operation, payload)` 只提交操作与参数。宿主重新绑定 Invocation 身份，业务处理器仍须经过原生权限与状态校验，不能从 payload 中接受自报工作区或任务身份。
- 宿主超时覆盖排队、JavaScript 和等待能力结果的全过程。取消和 future 被丢弃都失效能力调用上下文；正常退出有短暂收口期限，超过期限使用现有受管进程释放入口终止，不建立另一套进程管理。

## 候选取舍

| 候选 | 构建与运行特点 | 当前证据 |
| --- | --- | --- |
| QuickJS + 独立受管 Worker | 作者打包 ESM；引擎随 Rust Worker 构建；通过引擎内存/栈限制和中断回调，以及宿主期限和进程释放控制执行 | macOS Apple Silicon 和 Linux aarch64 的 9 项实际子进程回归通过；TypeScript 经 esbuild 构建的 ESM 在 release Worker 返回预期 JSON；Windows CI 尚待执行 |
| TypeScript → JavaScript → WASM 组件 | 作者另需组件化构建和 WIT 合同；组件内嵌 JavaScript 引擎，宿主还需 Wasmtime 及 typed import 桥接 | 用 jco 1.37.0 / ComponentizeJS 0.23.0 将 TypeScript 转为禁用 WASI 功能的组件，在 Wasmtime 49.0.2 执行；基础结果、燃料耗尽、64MiB 线性内存限制均得到预期结果；未验证完整 SDK 异步桥或三平台打包 |

选定 QuickJS + ESM，原因是目前需要跨平台的 JavaScript 决策与受控 SDK 调用，独立 Worker 已能复用现有 daemon 进程管理；WASM 对本方案增加了作者组件化构建、引擎内嵌与宿主组件桥接。比较中的基础 WASM 样例约 11.4MiB，本机 release Worker 约 1.73MiB；两者样例功能与构建优化不同，这只是制品形态记录，不作为速度或安全性排名。未选方案的临时脚本和制品不进入生产代码，也不保留第二套运行时。

阶段 A 尚不能关闭：需要取得 Windows 实际执行、三平台正式安装包及 daemon 业务 SDK 取消结算的证据。Linux 容器使用仓库固定的 Rust 1.97.0 执行上述回归。三平台 CI 已加入执行和包校验回归，以及 Worker 实际 preflight，尚未产生本分支的 CI 结果；JavaScript API 隔离不代表已建立操作系统级沙箱。

## 产品构建与发行边界

- `scripts/dev-daemon.sh` 与 Desktop Rust 构建入口同步构建 `magi-plugin-worker`，不依赖机器上的 Node.js、Python 或外部 QuickJS。
- Electron Builder 将 Worker 放入 `resources/daemon`；macOS 签名清单包含该二进制。能力清单记录 Worker 哈希，解包校验从最终资源目录启动 Worker，并核对与产品一致的编译版本。
- `magi-plugin-worker --preflight` 使用当前二进制路径建立真实 `PluginHost`，经私有 stdio、QuickJS 和受控 SDK 完成 ESM/Promise/回调探测。正常 Worker 无参数仍走唯一执行入口；不接受未知参数。
- 发行脚本在清空环境的条件下运行 preflight。rquickjs 三个 crate 共用其仓库 MIT 许可，QuickJS 引擎另附 MIT 正文；许可与告知清单进入现有资源哈希和 SBOM 流程。
- 本机已验证 release Worker 在含空格的临时资源目录中执行、版本不匹配拒绝及许可收集；`npm run desktop:package -- --dir` 生成 macOS Apple Silicon 目录包，最终 `.app` 内 Worker 的实际 preflight、资源哈希与许可清单通过解包检查。目录包不等于正式安装器验收，其他平台仍待验证；未接通安装与业务授权前不开放插件功能入口。

参考：[rquickjs Runtime 限制接口](https://docs.rs/rquickjs/0.14.0/rquickjs/struct.Runtime.html)、[组件模型 JavaScript 构建链](https://component-model.bytecodealliance.org/language-support/building-a-simple-component/javascript.html)。

## 验证

```sh
cargo test -p magi-plugin-runtime --all-targets --locked
cargo clippy -p magi-plugin-runtime --all-targets --locked -- -D warnings
cargo build -p magi-plugin-runtime --bin magi-plugin-worker --release --locked
./target/release/magi-plugin-worker --preflight
```

核心回归覆盖实际 Worker 进程：模块与 Promise、缺少系统 API/import、宿主身份与拒绝、死循环、内存/输出限制、能力等待超时与取消、并发隔离、调用被丢弃时的释放及复制后的发行探测。复用现有 CI 的三个平台作业，不新增每个插件一套测试脚本。
