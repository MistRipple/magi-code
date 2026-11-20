# 共享宿主装配和来源恢复

macOS arm64 使用已打包的 Node 26.13.0、npm 11.20.0 和共享 SDK。daemon 装配入口按持久代次创建共享进程和私有网关，实际绑定网关 origin 后才从来源监视器接纳配置。[运行回执](result.json)

- 从官方 npm registry 取回 `dsh-plugin-mgr@0.2.12`、`0.2.13`。共享宿主观察到两版均已解析 `webServer`，必需依赖仅余 `loader`。在准备升级时取消，修订 1 保留。成功接纳后保存修订 2。
- 停止进程后写入非法 YAML，再重新打开插件管理器和启动共享宿主。旧版启动参数无法覆盖已保存的 `0.2.13`，新宿主签发了新激活代次。非法正文完整保留并报告解析错误。修复文件后观察到修订仍为 2，没有重复应用图。
- 同步改造现有多来源编辑回归。使用真实编辑事务和文件内容复现只替换第一份文件就退出，新进程在启动监视时补完保存，首次应用完整的修订 2。另复测私有网关处理流、升级连接和停用撤销。

固定 Mgr 原包仍处于等待，尚未完成页面业务。装配入口由 daemon 集成回归调用，发行 daemon 的正式启动、Magi 设置页安装、取消与升级仍待接入。

```sh
resources="$PWD/target/electron-dist/mac-arm64/Magi.app/Contents/Resources"
MAGI_EXTENSION_NODE="$resources/daemon/node/node" \
MAGI_DSH_NPM_CLI="$resources/daemon/node/npm/bin/npm-cli.js" \
MAGI_DSH_SHARED_SDK_PROJECT="$resources/daemon/dsh" \
  cargo test -p magi-daemon --test plugin_registry --locked -- --ignored --nocapture
```
