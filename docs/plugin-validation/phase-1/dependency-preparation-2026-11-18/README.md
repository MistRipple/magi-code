# 原包的依赖项目准备

测试使用 macOS arm64、Magi 锁定归档中的 Node 26.13.0 和 npm 11.20.0，以及此前验证的共享 SDK 项目。两个固定 Mgr 原包走真实 registry。npm 为每个项目生成锁并安装普通依赖，Cordis 和 DSH 绑定到 SDK 项目的同一物理模块。候选项目比对全部原包文件，随后核对实际 ESM 入口。[运行回执](result.json)

- 固定 `0.2.12` 和 `0.2.13` 依次由真实 Node 宿主应用，daemon 重开管理器后仍读到 `0.2.13` 的已提交修订。
- 在升级准备阶段阻塞本地测试 registry，等收到真实 npm 连接后取消。连接随进程退出，暂存目录删除，已提交修订仍为 `0.2.12`。
- 使用相同准备器验证 Mgr `0.2.13`、WorkBuddy、Ops、Free Search 和 REA 的联合依赖项目。生成的锁包含 198 个条目，原包文件和入口核对通过。

目前正式发行版仍需接入工具链和安装任务。取消用例覆盖受管进程的准备阶段，Magi 设置页取消、公开 Loader 和 Electron 原页面业务尚未验证。两个 Mgr 实例仍报告等待 `loader` 和 `webServer`。

运行时将当前 `runtime/dsh` 脚本放入已锁定共享 SDK 项目的 `runtime` 目录。使用该 Node 归档中的 `node`、`npm-cli.js` 绝对路径赋给 `MAGI_EXTENSION_NODE` 和 `MAGI_DSH_NPM_CLI`，将已准备的项目绝对路径赋给 `MAGI_DSH_SHARED_SDK_PROJECT`。

```sh
cargo test -p magi-daemon --test plugin_registry --locked -- --ignored --nocapture
```
