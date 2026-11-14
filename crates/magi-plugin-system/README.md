# 插件领域与包合同

本 crate 属于 daemon 的插件领域，不是第二套 Agent 或界面宿主。插件的可执行部分只有受信 Node 扩展，由 [`magi-extension-host`](../magi-extension-host) 承载；会话/任务/权限继续由原生领域服务持有。SDK 1（QuickJS 后台）已移除：`sdkVersion: 1` 或声明 `backend` 的包在校验阶段被明确拒绝。

这里拥有：不可变发布包读取与校验、daemon 唯一的安装/升级/启用/停用/卸载状态服务、版本化资源存储、已签名插件目录与插件设置 API、视图资源入口和右栏隔离 iframe 承载。插件提供的模型工具由 Node 扩展承载（`contributions.tools`，需 `extension` 与覆盖该工具的 `tools` 权限；目录与参数校验在这里，执行在 `magi-extension-host`）。用户命令（`contributions.commands`，需 `extension`）同样由 Node 扩展承载。工作流、会话引擎已随 QuickJS 删除且不重建；数据格式迁移没有转换通道，数据格式版本变化时升级会清空该插件的数据。

## 唯一合同

插件清单及对外 DTO 使用 `contracts/app-server/app-server.schema.json` 中的 `Plugin*` 定义。Rust 从 `magi-app-server-protocol` 导入生成类型，Web 使用同一次生成的 TypeScript 类型，不另外手写同形状 manifest 接口。运行时校验也读取这一份 schema，补充版本语义、包内资源引用、跨贡献身份唯一性和权限作用域检查。

发布包是 ZIP，固定含 `manifest.json`；声明 `extension` 的包还须含 `extension/index.mjs`，界面资源位于 `ui/`。不接纳宿主原生程序或 WASM。包在内存中一次校验后成为不可变对象，安装/执行不能重新读取用户源目录绕过校验。

解包校验覆盖压缩/展开大小、文件数、单文件上限、路径穿越、大小写冲突、Windows 保留名、符号链接、特殊文件、可执行权限、原生载荷、缺失视图资源以及 schema 外部引用。JSON Schema 验证关闭文件和网络解析，插件提供的 schema 不得访问宿主文件或下载远端定义。

## 生命周期与资源

`PluginManager` 以 `state.json` 和不可变版本 ZIP 作为唯一安装事实；API 与 Web 都只调用它。安装来源可以是中心、地址或本地包，但三者在进入管理器前都经过同一 `PluginPackage::from_archive` 校验。升级要求先关闭该插件的准入（同一插件的停用、升级、卸载互斥），再切换到单一新版本，不能并发保留旧实现或静默回退；数据格式版本变化且已有持久数据时没有转换通道，升级会清空该插件的数据（预览用 `dataReset` 标出）。状态提交后才清理未引用旧包。插件只有启用和停用两个状态，安装完成即启用；卸载沿同一排空，任何状态都可直接卸载。`state.json` 的 `STATE_VERSION` 为 3，旧版本首次打开时折叠并备份（见 `lifecycle/migration.rs`）。

扩展宿主进程的启停由 daemon 的 `ExtensionRuntime` 在生命周期事实提交后收敛，不在本 crate 内。启动时单个插件 ZIP 缺失不会阻断整个 daemon：备份原始 `state.json`、保存缺包恢复记录并隔离缺失条目，其他插件和资源数据保持可用。非法路径、存储权限错误、损坏的状态 JSON 不按“缺包”静默丢弃。

`PluginResourceStore` 是插件资源的单一事实源，写入携带期望版本并返回明确冲突；资源读写还要通过清单声明的 storage 权限。右栏视图只能通过 daemon 资源 URL 读取包内 HTML，响应带受限 CSP，iframe 使用 `sandbox="allow-scripts"` 且不取得父页面 DOM、凭据或 Electron 桥。

## 验证

```sh
npm run protocol:check
cargo test -p magi-plugin-system --all-targets --locked
cargo clippy -p magi-plugin-system --all-targets --locked -- -D warnings
```

包校验是生命周期的前置条件，校验通过不等于已安装或已启用。
