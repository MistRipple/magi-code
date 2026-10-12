# Magi 插件作者指南

面向想为 Magi 增加工具、命令、页面或数据面板的作者。设计依据与边界见[插件系统方案](https://github.com/MistRipple/magi-code/blob/main/docs/plugin-system-requirements-design.md)；本文只描述作者今天可以依赖的合同，未列出的能力都不可用。

## 最小流程

```bash
node scripts/pack-plugin.mjs examples/plugins/board   # 生成 example.board-1.0.0.zip
```

在 Magi 设置的“插件”页安装 ZIP，确认权限后启用；插件全局生效，启用、授权和数据对所有工作区共用同一份。用户无需安装 Node.js、npm 或任何构建工具来运行插件；打包脚本只是把目录压成 ZIP，包内容与结构由 Magi 安装时统一校验。示例见 [`examples/plugins/board`](https://github.com/MistRipple/magi-code/blob/main/examples/plugins/board)，仓库测试保证它始终是合法的发布包。

## 包结构

| 文件 | 说明 |
| --- | --- |
| `manifest.json` | 清单，字段以 `contracts/app-server/app-server.schema.json` 的 `PluginManifest` 为准 |
| `plugin.mjs` | 唯一后台入口：UTF-8 ESM bundle，无外部 import，≤512KiB，默认导出 `(input, sdk) => JSON \| Promise<JSON>` |
| `ui/**` | 视图静态资源（html/css/js/svg/图片/字体）。浏览器端脚本只属于视图，不是第二个后台入口 |

包内不得有可执行文件、动态库、WASM、符号链接或安装后脚本。贡献 ID 以小写字母开头，只含小写字母、数字和连字符，所有类型的贡献共用一个命名空间。

## 后台执行模型

每次调用都会启动一个新的 QuickJS 实例（`rquickjs`），并在受 OS 沙箱限制的 Worker 进程里运行。没有 Node.js、DOM、文件、网络、环境变量或进程 API；跨调用状态只能通过资源与设置保存，不能依赖全局变量。有内存、栈、时间、报文大小和能力调用次数上限。

`sdk` 是唯一的宿主接口：

| 成员 | 说明 |
| --- | --- |
| `sdk.invocation.contribution` | `{kind, id}`，宿主签发且不可改写。一个入口承接多个工具、命令时据此分派 |
| `sdk.call('resource.read', {resourceId})` | 返回 `{version, value}`；未写入过时 `version` 为 0、`value` 为 `null` |
| `sdk.call('resource.write', {resourceId, expectedVersion, value})` | 版本不匹配时抛错（并发冲突），重读后重试；成功返回新快照 |
| `sdk.call('settings.read', {})` / `settings.write` | 读写按 `settingsSchema` 校验的设置 |

资源必须在 `contributions.resources` 声明，并获得对应 `storage` 权限；设置使用保留目标 `__settings`。宿主重新绑定调用身份，插件无法指定工作区或任务。

## 贡献

- **工具**（`tools`）：模型可调用。输入按 `inputSchema` 校验后才会启动 Worker；返回值作为工具结果。`readOnly: false` 的工具在受限模式下需要用户确认。需要 `tools` 权限，目标为工具 ID。
- **命令**（`commands`）：用户在输入区显式选择，结果必须是 `{content: string}`，作为本轮回复写回对话。
- **资源**（`resources`）：视图与工具共享的唯一数据源，带版本，写入发布订阅事件。
- **视图**（`views`）：`placements` 取 `rightPane`、`main`、`settings`、`home`、`homeCard`；`home` 必须同时声明 `main`（主页入口只是进入页面的触发器）；`homeCard` 把同一份视图作为卡片嵌入空会话主页（固定高度，每个插件最多 2 张，同时声明 `main` 时卡片带「打开」按钮进入完整页面）。没有 `settings` 视图且 `settingsSchema` 有 `properties` 时，设置页自动生成表单。
- **工作流核心**（`workflows`）、**会话引擎**（`engines`）：替换编排决策或模型通道，合同见方案 §7；需要对应的 `models` 权限。
- **浏览器 surface**（`browserSurfaces`）：声明意愿（持久性、是否保持挂载、关闭动作、后台推理），挂载由 BrowserAuthority 执行。宿主目前只支持应用级、持久、保持挂载、关闭只隐藏这一种组合，其余在激活时被拒绝。

## 视图消息协议

视图在 `sandbox="allow-scripts"` 的 iframe 中运行（`null` origin，CSP 禁止网络），只能通过 `postMessage` 与宿主通信，通道名 `magi-plugin-v1`：

1. 宿主 → 视图 `{type:'init', instanceId, pluginId, scope}`，之后每条消息都要回传 `instanceId`。
2. 视图 → 宿主 `{channel, instanceId, type:'request', requestId, operation, ...}`，宿主回 `{type:'response', requestId, ok, result | error}`。
3. `operation`：`resource.read`、`resource.write`、`resource.subscribe`、`settings.read`、`settings.write`、`context.select`。
4. 订阅后宿主推送 `{type:'event', resourceId, resource}`；插件被停用、撤权或升级时订阅结束并收到 `{error:'subscription_failed'}`。
5. `context.select {resourceId, version, label, summary, selection}`：把选中对象交给输入区。引用只含摘要（≤1KiB）与选中对象（≤4KiB），模型需要完整内容时通过工具读取资源；插件身份由宿主绑定，不采信消息里自报的值。

## 生命周期与数据

- 安装不会激活；激活前展示全部权限，授权绑定包摘要。升级后授权清空，需重新授权。
- 停用与升级先关闭新准入并等待在途运行（默认 30 秒），到期取消，再等 10 秒结算；撤销授权立即取消在途调用。
- 卸载保留配置与数据。
- **数据格式变化**：提高 `dataSchemaVersion` 时，如果插件已有数据，新版清单必须声明 `dataMigration: {"fromVersions": [旧版本号…]}`，否则升级被拒绝。升级时宿主在排空之后、切换之前，用**新版自己的后台入口**逐条转换旧数据：每条调用的 `sdk.invocation.contribution` 为 `{kind:"migration", id:"data"}`，输入 `{fromVersion, toVersion, scope, resourceId, value}`；返回 `{value}` 保留（转换后的）条目，返回 `{drop:true}` 丢弃它。转换是纯函数——此时 `sdk.call` 全部被拒绝，不能访问资源、设置、模型、网络或进程。设置（`__settings`）必须符合新版 `settingsSchema`，保留的资源必须在新版 `resources` 中声明，转换结果超过 4MiB 会被拒绝。转换失败或任何校验失败都会取消升级，旧版本与旧数据保持不变；转换结果先写入暂存区，包切换提交后才应用，进程在任意时刻崩溃，重启都会回到“旧版本旧数据”或“新版本新数据”之一。转换后的条目版本号在旧版本之上递增。没有数据的插件改变 `dataSchemaVersion` 不需要声明。
- 工具名对模型呈现为 `plugin__<清洗后的插件ID>__<贡献ID>_<摘要>`，身份由宿主解析，不要依赖该字符串。

## 分发

- **本地包 / 指定地址**：设置页直接安装。指定地址来源在界面上单独标识，不经过插件中心。
- **插件中心**：在设置页「插件中心」填入一个目录地址即可浏览和安装其中的插件，和添加 Skill 一样简单。目录是一个 JSON 文件（`format`、`plugins[]`，条目含 `id/name/description/version/url/sha256/size`；`url` 可以是相对目录自身的路径，`sha256` 是发布 ZIP 的摘要）。下载的包必须与目录里的摘要一致才会安装，安装后仍要用户逐作用域授权。没有经过发布者签名验证的目录按「指定地址」的信任级别对待，界面会标注未签名，安装后的来源也显示为指定地址。需要更强的发布者认证时，目录可以做成 Ed25519 签名信封（`{payload, keyId, signature}`，`payload` 是目录 JSON 的 Base64 字节，签名覆盖这些字节），用户在「高级」里填入公钥后，只接受该密钥签名的目录，来源才标记为插件中心。

### 官方插件中心

Magi 默认使用官方插件中心，它就是本仓库的 [`plugin-center/`](https://github.com/MistRipple/magi-code/tree/main/plugin-center) 文件夹，经 GitHub 提供，用户不需要配置任何地址。想上架插件：把插件目录放进仓库，在 `plugin-center/plugins.json` 里登记，运行 `npm run plugin-center:build` 生成发布包和目录后提交 PR；代码审查就是上架审核，CI 会检查提交的包与源码一致。来自官方地址的目录按插件中心来源对待，安装后仍不会自动激活，权限要用户逐项授权。

### 自己托管插件中心

不需要任何官方服务，也不需要密钥：

```bash
node scripts/pack-plugin.mjs my-plugin my-plugin-1.0.0.zip        # 打包
node scripts/plugin-center.mjs publish --out catalog.json my-plugin-1.0.0.zip   # 生成目录（包地址是相对路径）
```

把 `catalog.json` 和 ZIP 放进同一个文件夹，托管到任意 HTTP(S) 静态地址（对象存储、GitHub Pages/Releases、内网服务器），把 `catalog.json` 的地址告诉用户即可；文件夹整体搬家不用重新发布。发布前可以用 `node scripts/plugin-center.mjs verify --catalog catalog.json --packages .` 检查包摘要。

要让用户能确认目录确实来自你，再加上签名：

```bash
node scripts/plugin-center.mjs keygen center-key.pem --key-id my-center          # 生成私钥，打印要分发的 keyId / publicKey
node scripts/plugin-center.mjs publish --key center-key.pem --key-id my-center --out catalog.json my-plugin-1.0.0.zip
node scripts/plugin-center.mjs verify --catalog catalog.json --key-id my-center --public-key <publicKey> --packages .
```

用户在插件中心「高级」里填入 `keyId` 与 `publicKey`。私钥只用于签名，务必离线保管；更换密钥时用户需要填入新公钥。
