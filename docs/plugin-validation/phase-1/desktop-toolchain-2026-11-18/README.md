# 桌面目录包的 DSH 工具链

macOS arm64 上生成 Magi 3.0.62 的 Electron 目录包。归档中的 Node 26.13.0 和 npm 11.20.0 通过空环境执行检查。共享 SDK 由同一个 npm 按仓库锁文件安装，Electron 打包后按发行资源清单逐文件校验。[运行回执](result.json)

- 用独立状态目录启动打包后的 Magi。真实 daemon 报告健康，Chromium 加载了工作台，桌面进程退出后 daemon 停止监听。
- 从目录包读取 Node、npm 和 SDK，复用 daemon 的真实 registry 回归。从官方 registry 取回固定 Mgr `0.2.12` 与 `0.2.13`，准备两个候选项目，接纳修订 1 和 2。在 npm 已连接测试 registry 时取消准备，进程和暂存目录回收，修订 1 保留。重开管理器后仍读到修订 2 的版本。
- 用同一目录包内的准备入口验证五个固定原包的联合依赖图。候选项目的 Cordis 和 DSH 模块绑定到包内共享 SDK，原包文件与 ESM 入口经过核对。

保留了根目录的 DSH 清单。由于该清单没有 Magi npm workspace，顶层构建入口找不到 `magi-web`。本次从 `web/` 启动 Renderer 构建，单独执行仓库的发行构建步骤，再用相同 Electron Builder 配置和解包检查生成目录包。正式发行预检及 Windows、Linux 未执行。

Mgr 的运行投影仍在等待公开的 `loader` 和 `webServer`。上述升级属于 daemon 领域回归。Magi 设置页发起升级、正式 daemon 启动共享宿主以及 Mgr 实际页面业务仍待验收。

```sh
resources="$PWD/target/electron-dist/mac-arm64/Magi.app/Contents/Resources"
MAGI_EXTENSION_NODE="$resources/daemon/node/node" \
MAGI_DSH_NPM_CLI="$resources/daemon/node/npm/bin/npm-cli.js" \
MAGI_DSH_SHARED_SDK_PROJECT="$resources/daemon/dsh" \
  cargo test -p magi-daemon --test plugin_registry --locked -- --ignored --nocapture
```
