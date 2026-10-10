# 插件领域与包合同

本 crate 属于 daemon 的插件领域，不是第二套 Agent 或界面宿主。执行层位于 [`magi-plugin-runtime`](../magi-plugin-runtime/README.md)，会话/任务/权限继续由原生领域服务持有。

已完成不可变发布包读取与校验、daemon 唯一安装/升级/授权/启用/激活/释放状态服务、版本化资源存储、插件工具目录桥、按作用域的命令/工作流/引擎投影、插件设置 API、类型化工作流动作合同、插件视图资源入口和右栏隔离 iframe 承载。命令和工作流的最终用户选择与 GPT Web 完整迁移仍按方案阶段推进，未接入的贡献不会被伪造为可用能力。

## 唯一合同

插件清单及对外 DTO 使用 `contracts/app-server/app-server.schema.json` 中的 `Plugin*` 定义。Rust 从 `magi-app-server-protocol` 导入生成类型，Web 和未来 SDK 使用同一次生成的 TypeScript 类型，不另外手写同形状 manifest 接口。运行时校验也读取这一份 schema，补充版本语义、包内资源引用、跨贡献身份唯一性和权限作用域检查。

发布包是 ZIP，固定含 `manifest.json` 和 `plugin.mjs`；界面资源位于 `ui/`。后台只接受 UTF-8 ESM，不接纳宿主原生程序、QuickJS 字节码或 WASM 后台组件。包在内存中一次校验后成为不可变对象，安装/执行不能重新读取用户源目录绕过校验。

解包校验覆盖压缩/展开大小、文件数、单文件上限、路径穿越、大小写冲突、Windows 保留名、符号链接、特殊文件、可执行权限、原生载荷、缺失视图资源以及 schema 外部引用。JSON Schema 验证关闭文件和网络解析，插件提供的 schema 不得访问宿主文件或下载远端定义。

## 生命周期与资源

`PluginManager` 以 `state.json` 和不可变版本 ZIP 作为唯一安装事实；API、Web 或未来 CLI 都只调用它。安装来源可以是中心、地址或本地包，但三者在进入管理器前都经过同一 `PluginPackage::from_archive` 校验。升级要求所有作用域排空并切换到单一新版本，不能并发保留旧实现或静默回退。授权、启用和激活是分开的状态，卸载前必须释放所有作用域。

`PluginResourceStore` 是插件资源的单一事实源，写入携带期望版本并返回明确冲突；资源读写还要通过清单声明的 storage 权限。右栏视图只能通过 daemon 资源 URL 读取包内 HTML，响应带受限 CSP，iframe 使用 `sandbox="allow-scripts"` 且不取得父页面 DOM、凭据或 Electron 桥。

## 验证

```sh
npm run protocol:check
cargo test -p magi-plugin-system --all-targets --locked
cargo clippy -p magi-plugin-system --all-targets --locked -- -D warnings
```

包校验是生命周期的前置条件，校验通过不等于已授权或已激活。安装服务、就绪状态与业务能力桥接仍须通过[方案](../../docs/plugin-system-requirements-design.md)的生命周期验收。
