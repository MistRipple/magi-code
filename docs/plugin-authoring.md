# Magi 插件作者指南

这是一份可以单独保存、复制给其他开发者或交给 AI 使用的 Magi 插件开发手册。你需要了解基本 JavaScript 和 JSON；做界面页面时需要基本 HTML。

> **适用范围：** 本指南描述当前的插件 SDK（清单 `sdkVersion` 为 `2`）。能力边界见第 16 节。Windows 上的运行与安装包尚未验收，下文的行为以 macOS 上的验证为准。
>
> **已有的 DSH 插件：** 为 DeepSeek Harness 发布到 npm 的插件不需要按本指南改写。设置 → 插件可搜索上游包，Magi 在安装期检查和封装原包，安装前展示兼容结果与权限。尚未实现的必需契约会阻止安装；没有对应位置的界面槽位会如实披露。页面代码、语言包、样式、图片、链接和页面交互由上游原包直接运行，翻译器只适配宿主能力与业务运行时接口。pi 生态的搜索结果目前只可浏览。

## 1. 先确认你的功能适合哪种形态

| 你想做什么 | 形态 | 在哪一节 |
| --- | --- | --- |
| 给模型增加一段方法说明（Skill） | 声明式 | 第 5 节 |
| 增加设置页、主页面、右侧面板（界面都由你的页面提供） | 声明式 | 第 5 节 |
| 让页面和扩展共享一份带版本的数据 | 声明式 + `storage` 权限 | 第 5、10 节 |
| 观察或拦截会话事件：输入、工具调用、子代理派发、模型请求与响应、用量 | Node 扩展：钩子 | 第 7 节 |
| 为模型提供新的工具 | Node 扩展：工具 | 第 8 节 |
| 为用户提供 `/` 斜杠命令 | Node 扩展：命令 | 第 9 节 |
| 调用模型、读会话、通知或提问用户、派发子任务 | Node 扩展：宿主服务 | 第 10 节 |
| 把第三方反代等模型接进主对话的模型列表 | Node 扩展：模型数据源 | 第 10 节 |
| 按计划定时执行命令 | daemon 调度 + Node 命令 | 第 11 节 |

纯声明式的包不启动任何后台进程；只要用到钩子、工具、命令或宿主服务，就必须声明 `extension`，并让用户授予“完全信任”（第 4 节）。

插件的安装、授权、启用和数据对所有工作区共用一份；切换工作区不会得到另一份数据。

## 2. 快速开始：一个最小的命令插件

目标：用户在输入框输入 `/问候 世界`，得到一条回复。

创建目录 `hello/`，放三个文件（UTF-8）：

`hello/manifest.json`

```json
{
  "sdkVersion": 2,
  "id": "acme.hello",
  "version": "1.0.0",
  "name": "问候",
  "description": "一个最小的命令插件",
  "permissions": [
    { "kind": "extension", "targets": ["full-trust"] }
  ],
  "dataSchemaVersion": 1,
  "settingsSchema": { "type": "object", "additionalProperties": false },
  "extension": { "hooks": [] },
  "contributions": {
    "commands": [{ "id": "hello", "title": "问候", "description": "向某人问好" }]
  }
}
```

`hello/extension/index.mjs`

```js
export default async function activate(api) {
  await api.command('hello', async (input) => `你好，${input.text || '世界'}！`);
}
```

打包（二选一）：

```text
# 在 Magi 仓库里
node scripts/pack-plugin.mjs hello hello.zip

# 没有仓库时：在插件目录里打包，注意排除隐藏文件
cd hello && zip -qr ../hello.zip . -x '.*'
```

> 包根目录就是 `manifest.json` 所在目录，不要多套一层文件夹。包里**只能**有第 3 节列出的文件；macOS 的 `.DS_Store`、`README.md`、`package.json` 之类多余文件都会让安装被拒绝。

安装与使用：

1. 设置 → 插件 → “安装本地 ZIP”，选择 `hello.zip`。预览会列出“完全信任”权限和包摘要，确认安装（安装本身**不会**激活）。
2. 在插件详情里点“授权并激活”，状态变为“已激活”，扩展运行状态显示“运行中”。
3. 回到对话，输入 `/`，选择“问候”，附上文字（如“世界”）发送，会话里出现 `你好，世界！`。

更完整的参考：仓库里的 `examples/plugins/notes` 演示了资源、设置和一个模型工具。

## 3. 包结构与清单

发布包是 ZIP：

```text
manifest.json              必需
extension/index.mjs        声明了 extension 时必需，唯一后台入口
ui/…                       视图页面与静态资源，按需
```

**包里只允许这三类文件**，其他任何文件（`README.md`、`LICENSE`、`package.json`、`.DS_Store`、`node_modules/`、其他目录……）都会让安装失败，报“发布包含未允许的文件类型或入口”：

- `manifest.json`；
- `extension/index.mjs`；
- `ui/` 下扩展名为 `html`、`css`、`js`、`mjs`、`json`、`svg`、`png`、`jpg`、`jpeg`、`webp`、`gif`、`avif`、`bmp`、`ico`、`woff`、`woff2` 的文件。

文本文件必须是 UTF-8。限制：整包不超过 16 MiB，展开后不超过 32 MiB，最多 512 个文件，单文件不超过 4 MiB；不得有符号链接、可执行文件、原生程序或 WASM、路径穿越。预览、安装和升级的 JSON 上传容量按归档的 base64 长度另加 64 KiB 元数据计算；这不增加归档本身的大小上限。

清单字段：

| 字段 | 说明 |
| --- | --- |
| `sdkVersion` | 必须是 `2` |
| `id` | 小写字母、数字、连字符，必须含作者命名空间（`acme.hello`），最长 96 |
| `version` | 语义化版本 |
| `name` / `description` | 展示名称与说明 |
| `permissions` | 权限声明，见第 4 节 |
| `extension` | `{ "hooks": [...], "runtimeContributions": [...] }`，动态贡献种类可省略；声明后包必须含 `extension/index.mjs`；反之不得带该文件 |
| `contributions` | `skills`、`resources`、`views`、`browserSurfaces`、`tools`、`commands`、`modelSources` |
| `settingsSchema` | 设置的 JSON Schema，用于校验设置值（必须是有界的 object schema，禁止外部引用）；它不生成界面 |
| `dataSchemaVersion` | 数据格式版本，正整数 |

规则：

- 清单不接受未知字段。
- 所有贡献的 ID（技能、资源、视图、工具、命令……）共用一个命名空间，不能重名；ID 以小写字母开头，只含小写字母、数字、连字符，最长 64。
- 声明了 `extension` 的包必须至少有一个钩子、工具、命令、模型数据源或运行期贡献种类。
- 工具、命令和模型数据源只有声明了 `extension` 才能出现。

## 4. 权限与信任

| 权限 `kind` | `targets` | 用途 |
| --- | --- | --- |
| `extension` | `["full-trust"]` | 声明受信 Node 扩展；包含 `extension` 时必须声明 |
| `storage` | 资源 ID；设置用 `"__settings"` | 读写对应资源或设置 |
| `tools` | 工具 ID | 让对应工具对模型可见 |
| `models` | `"default"` 或角色 ID | 允许 `model.invoke` / `credential.ref` 使用默认连接或指定角色的模型 |
| `contextRewrite` | `["history"]` | 允许改写已装配历史的钩子（第 7 节） |
| `modelSources` | 数据源贡献 ID | 允许向主对话的模型列表提交对应数据源；用户选用后，主对话内容会发送到你提供的地址（第 10 节） |
| `browser` | surface ID | 浏览器 surface，见第 5 节 |
| `pluginManagement` | `install`／`lifecycle`／`authorize`／`compatibility`／`background` | 在线安装／启停和卸载／独立委托授予目标包声明的权限／修改精确版本豁免／页面请求完成后继续后台管理；各目标分别授权，豁免不能绕过 Magi 能力检查 |
| `composer` | `["images"]` | 由插件原生页面把用户选择的图片加入当前会话输入框草稿；只能移除本插件页面自己接纳的图片或聚焦输入框，不会自动发送 |
| `sessionRead` | `["directory"]` | 读取全部用户会话的标题、身份、项目归属与创建/更新时间；不含消息、路径或凭据，不授予任何会话写入能力 |

`permissions` 里列出的每一项，用户都必须在授权时逐项确认；缺少任何一项都不能激活。

### 信任模型

扩展以用户权限运行在 Magi 随包提供的 Node 里，**没有操作系统级沙箱**：它可以读写用户有权访问的一切文件、联网、派生进程，也能读到同一用户的凭据。所以 `extension: ["full-trust"]` 是一次明确的告知，不是隔离保证。`storage`、`models`、`tools` 只限制通过 Magi 服务入口能做什么，不限制 Node 本身。

会话、Turn、任务、审批和用量这些业务事实只归 Magi 内核。扩展只能阻止（`block`）或在允许的钩子上提出改写，不能批准宿主没批准的操作，也不能写 canonical 日志。

## 5. 声明式贡献

```json
"contributions": {
  "skills": [{ "id": "review", "title": "评审", "description": "", "instruction": "按以下步骤评审……" }],
  "resources": [{ "id": "notes", "title": "笔记", "description": "" }],
  "views": [{ "id": "board", "title": "看板", "entry": "ui/index.html", "placements": ["rightPane", "main"] }]
}
```

- **技能**：`instruction` 是给模型的方法说明（不超过 64 KiB），用户通过 `/` 选用。
- **资源**：带版本的 JSON 数据，写入必须携带读到的版本（乐观并发）。页面和扩展读写同一份；读写需要对应 `storage` 权限，目标是资源 ID。
- **视图**：`entry` 必须是包内 `ui/` 下的 HTML 文件。`placements` 可选：`settings`（在设置里为该插件增加一个分类页）、`main`（主内容区页面）、`home`（主页上的入口，必须同时声明 `main`）、`homeCard`（主页上嵌入的卡片，每个插件最多 2 张）、`rightPane`（右侧面板）。
- **图标**：清单可选字段 `icon`，指向包内 `ui/` 下的图片（PNG、JPEG、GIF、WebP、AVIF、BMP、ICO 或 SVG，不超过 256 KiB）。设置的插件列表、详情和设置导航都显示它；没有声明或加载失败时显示通用插件图标。SVG 保留矢量格式，仅通过图片元素显示；脚本、事件和外部引用不属于图片合同，直接导航也受沙箱 CSP 隔离。
- **视图隔离**：页面运行在 `sandbox="allow-scripts"` 的隔离 iframe 中，不能联网、不能访问宿主页面、凭据或 Electron，只能通过下面的消息桥读写资源和设置。
- **设置界面**：Magi 不为插件生成设置表单，所有界面都由插件自己的页面提供。需要用户配置时，声明一个 `placements` 含 `settings` 的视图，页面通过消息桥的 `settings.read` / `settings.write` 读写；值按 `settingsSchema` 校验。设置读写需要 `storage` 权限，目标 `"__settings"`。完整示例见 `examples/plugins/notes/ui/settings.html`。
- **上游页面**：DSH 翻译包沿用上游的页面入口和组件运行库。页面中的外部 HTTP(S)/mailto 链接保留原始地址、目标和可访问名称，并由宿主外部链接入口打开；图片和 SVG 继续由页面按图片元素显示，SVG 不注入可信 DOM。
- **浏览器 surface**：`browserSurfaces` 条目需要同名的 `browser` 权限；`backgroundInference` 为真时必须 `keepMounted`。

### 视图消息桥

页面通过 `postMessage` 与宿主通信，通道名固定为 `magi-plugin-v1`：

1. 页面加载后，宿主先发来 `{ channel, type: 'init', instanceId, pluginId, placement, locale, theme }`；`placement` 为 `settings`（嵌入设置页：内容列宽度、左右留白、与页头的间距、圆角和滚动都由宿主的设置框架决定，页面背景透明、不应自带外层留白，高度由页面经 `view.resize` 上报）或 `page`（其他位置，宿主给定固定尺寸，页面自行滚动）。字号、字体、间距和圆角使用 `theme.tokens` 下发的宿主令牌，不要写死数值。**记下 `instanceId`**，之后每个请求都要带上。
2. 页面发请求：`parent.postMessage({ channel, instanceId, type: 'request', requestId, operation, ...字段 }, '*')`。
3. 宿主回复 `{ type: 'response', requestId, ok, result | error }`。

在途 `route.call` 可发送 `{ channel, instanceId, type: 'cancel', requestId }`，停止对应路由的等待并向后台传播取消。身份校验与原请求相同，只作用于本份文档仍在途的请求；已完成或未知 ID 无操作。取消不是业务回滚，已经提交的设置、安装或外部副作用仍以实际结果为准；写入操作不能据停止等待推断未提交。文档关闭统一回收在途路由，旧回执不能更新新文档。DSH 原页面的 `fetch` 在两种输入形式（`init.signal`、`Request.signal`）下使用此公共通道，取消后以 `AbortError`（或 signal 的自定义 reason）结束，其他并发请求继续运行。

| `operation` | 字段 | `result` |
| --- | --- | --- |
| `resource.read` | `resourceId` | `{ version, value }`（从未写过时是 `{ version: 0, value: null }`） |
| `resource.write` | `resourceId`、`expectedVersion`、`value` | 新的 `{ version, value }`；版本过期时 `ok: false` |
| `resource.subscribe` | `resourceId` | 无；之后资源每次变化宿主推送 `{ type: 'event', resourceId, resource }`，订阅失败（停用、撤权、升级）推送 `{ type: 'event', resourceId, error }` |
| `settings.read` / `settings.write` | `expectedVersion`、`value`（写） | `{ version, value }` |
| `route.call` | `method`、`path`、可选 `query`、`headers`、`body` | `{status,contentType?,body?}`；只可调用本插件声明的后端路由 |
| `composer.snapshot` | 无 | `{sessionId,attachmentIds,writable}`；只包含当前文档接纳的图片 ID，之后宿主按状态变化推送 `{type:'composer',snapshot}` |
| `composer.addImages` | `sessionId`、`images:[{dataUrl,name,size}]` | `{accepted,ids,snapshot}`；等待输入框实际接纳，最多 5 张且每张 10 MiB，容量不足时整批返回 `accepted:false`，坏图或失效身份明确失败 |
| `composer.removeImage` | `sessionId`、`id` | `{removed,snapshot}`；只允许本页面文档添加的图片 |
| `composer.focus` / `composer.notify` | `sessionId`；通知另需 `level`、`message` | 无；上述全部操作需要 `composer:["images"]` 授权及当前安装摘要，页面换代后旧 ID 不能操作草稿 |
| `view.resize` | `height`（CSS 像素，0 到 20000） | 无；仅对 `settings` 位置的视图生效：宿主把 iframe 高度设为该值，由设置页整体滚动，页面自身不应再出现滚动条 |

单次写入不超过 4 MiB。一个最小的页面（`ui/index.html`）：

```html
<!doctype html>
<meta charset="utf-8">
<h3>笔记</h3>
<p id="text"></p>
<button id="edit">改写</button>
<script>
  const CHANNEL = 'magi-plugin-v1';
  let instanceId = null, version = 0, next = 0;
  const pending = new Map();

  function request(operation, fields) {
    return new Promise((resolve, reject) => {
      const requestId = 'r' + (++next);
      pending.set(requestId, { resolve, reject });
      parent.postMessage({ channel: CHANNEL, instanceId, type: 'request', requestId, operation, ...fields }, '*');
    });
  }
  function show(resource) {
    version = resource.version;
    document.getElementById('text').textContent = resource.value?.text ?? '（空）';
  }
  window.addEventListener('message', async (event) => {
    const m = event.data;
    if (!m || m.channel !== CHANNEL) return;
    if (m.type === 'init') {
      instanceId = m.instanceId;
      show(await request('resource.read', { resourceId: 'notes' }));
      await request('resource.subscribe', { resourceId: 'notes' });
    } else if (m.type === 'response') {
      const waiter = pending.get(m.requestId);
      pending.delete(m.requestId);
      if (waiter) m.ok ? waiter.resolve(m.result) : waiter.reject(new Error(m.error));
    } else if (m.type === 'event' && m.resource) {
      show(m.resource);
    }
  });
  document.getElementById('edit').onclick = async () => {
    show(await request('resource.write', { resourceId: 'notes', expectedVersion: version, value: { text: '新内容 ' + Date.now() } }));
  };
</script>
```

对应的清单需要 `storage` 权限（目标 `notes`）、资源 `notes` 和视图声明；这个页面不需要 `extension`。

## 6. Node 扩展入口与 `api`

`extension/index.mjs` 默认导出 `activate(api)`；返回（或 resolve）后扩展才算就绪，10 秒内未就绪视为激活失败。

```js
export default async function activate(api) {
  // 在这里注册钩子、工具、命令，读取初始配置
}
```

| `api` 成员 | 作用 |
| --- | --- |
| `api.on(hook, handler)` | 绑定钩子（第 7 节） |
| `api.tool(id, handler)` | 绑定模型工具（第 8 节） |
| `api.command(id, handler)` | 绑定用户命令，也是作业的执行入口（第 9、11 节） |
| `api.catalog.replace(expectedRevision, catalog)` | 提交已声明种类的动态贡献目录，返回新的 `{revision}` |
| `api.resource.read(id)` / `write(id, expectedVersion, value)` | 读写本插件资源 |
| `api.settings.read()` / `write(expectedVersion, value)` | 读写本插件设置 |
| `api.config` / `api.configRevision` | 设置快照与版本；设置在界面修改后自动更新 |
| `api.onConfig(listener)` / `api.onDispose(disposer)` | 接收配置通知／释放运行资源；返回取消监听函数 |
| `api.settings.describe()` / `api.settings.onValidate(handler)` | 读取有效 schema／注册一个额外设置校验器 |
| `api.model.invoke(input)` | 调用模型 |
| `api.credential.ref({ roleId? })` | 取得模型绑定引用 |
| `api.session.snapshot(input)` | 读取会话的有界快照 |
| `api.plugins.list({includeUpstreamManifest?:true})` | 读取 daemon 当前插件目录；显式请求时在同一快照附带已安装原包 package.json，来源及精确版本由 daemon 校验。默认不附带，不含配置或凭据 |
| `api.plugins.onChanged(listener)` | 协商 `plugin.directory.events` 后监听目录或运行状态失效通知，返回解除订阅函数；先订阅再读取 `plugins.list()`，读取期间收到通知则补读。通知没有目录数据，不替代 daemon 快照；实例退出回收监听器 |
| `api.plugins.preview(input)`／`install(input)` | 上游包预览与在线安装；复用精确版本和摘要确认合同，需 `pluginManagement:["install"]` |
| `api.plugins.reconcile({pluginIds})` | 协商 `plugin.reconciliation`，需 lifecycle 授权；最多 512 个唯一已安装插件 ID。沿现有 daemon 监管器应用已提交的激活意图，等待活动扩展真实就绪并返回当前目录；不启用停用项、不重启未变化实例，不代授权。启动失败、超时、目标版本／激活变化和调用者失效明确拒绝；故障原因由插件管理展示，修复后沿启停或在线更新入口恢复 |
| `api.plugins.setEnabled(pluginId,enabled,expectedDigest?)`／`remove(pluginId,expectedDigest?)` | 启停或卸载其他插件；需 `pluginManagement:["lifecycle"]`。传入目标摘要可拒绝过期页面操作；启用本身不会代授目标插件权限 |
| `api.plugins.authorize(pluginId,expectedDigest,grants)` | 需单独的 `pluginManagement:["authorize"]` 用户委托。仅授予目标当前摘要声明的权限（可包含完全信任），不能改变自身权限；授权和激活沿原生入口执行，异步提交前复核发起方租约与权限 |
| `api.plugins.compatibility()` | 读取当前随包 DSH SDK 版本、锁文件 runtimeId、revision 与精确版本豁免；仍需活动宿主租约 |
| `api.plugins.setVersionExemption(input)` | 需独立 `pluginManagement:["compatibility"]`。输入使用 `PluginDshVersionExemptionRequest`：精确 target、当前 runtimeVersion／runtimeId、revision、enabled、acceptRisk；写入需要风险确认，安装与升级提交前再次核对 |
| `api.plugins.background` | 与 plugins 相同的管理方法，显式使用宿主实例身份，适用于 HTTP 202 后继续执行的管理任务；写操作另需 `pluginManagement:["background"]`，且仍需 install／lifecycle／authorize／compatibility 各动作授权。停用、撤权、升级或实例替换后失效，提交前重验；不延长原会话调用，也不能调用模型、工具或会话服务 |
| `api.session.directory(input?)` | 需 `sessionRead:["directory"]` 和协商特性 `session.directory`；直接读 daemon 当前用户会话元数据，支持 `workspaceId`、`afterSessionId`、`limit`（默认 100，上限 200）。按稳定 sessionId 升序分页，返回 `{sessions,hasMore,nextAfterSessionId}`；不创建 DSH Session 或复制 Magi 消息，不能据此写入会话 |
| `api.tools.call(tool, input)` | 调用公开的原生工具 |
| `api.task.spawn(input)` | 派发子任务 |
| `api.notify.user({ message, title? })` | 向用户发通知 |
| `api.ask.user({ questions })` | 向用户提问并等待答复 |
| `api.blob.put(value, contentType?)` / `get(ref)` | 有界的 JSON 大对象传递 |
| `api.log(message)` | 诊断日志（最多 4096 字符，不进入会话记录） |

> 注意区分：`api.tool(id, handler)` 是**注册**你自己的工具；`api.tools.call(name, input)` 是**调用**原生工具。

依赖：后台入口只有一个文件，依赖要在你自己的构建阶段打包进它（例如用 esbuild 输出单个 `.mjs`）。不支持裸相对导入、运行时从 npm 安装依赖，也不支持原生模块或包内二进制。安装过程不执行任何脚本。

处理器通用上下文 `ctx`（工具、命令、钩子的第二个参数）：

| 字段 | 说明 |
| --- | --- |
| `ctx.signal` | `AbortSignal`；调用超时、被取消或用户停止所属 Turn/任务时中止，请尽快返回 |
| `ctx.invocationId` | 本次调用的身份 |
| `ctx.owner` | daemon 填入的归属：`sessionId`，以及 `taskId` / `turnId` / `workerId`（视调用类型而定） |
| `ctx.binding` | 例如 `workspaceId` |
| `ctx.execution` | 工具与命令的实际执行上下文：`workingDirectory`、`sourceFolders:[{sourceFolderId,workingDirectory}]`、`accessProfile`。目录来自 daemon 已准备的执行资源；项目只列本次获授的实际执行根，隔离前的源路径不会进入此投影；个人会话使用自有目录，sourceFolders 为空。无目录的调用为 null，不猜宿主 cwd |
| `ctx.job` | 仅作业触发的命令有：`jobId`、`scheduleId`、`budget` |

`owner`、`binding`、`execution`、`job` 由 daemon 注入，扩展不能伪造。命令从接纳起进入原生 Task，调用插件前沿同一任务资源入口完成目录验证、隔离与 Git／账本准备；目录缺失或授权失效会拒绝执行，修复后由用户重新发起。目录上下文用于定位本次业务，不是 OS 沙箱；完全信任插件的直接 OS 行为仍以当前用户权限运行。

### 运行期贡献与回收

工具、命令、技能和模型目录在启动后才确定时，在 `extension.runtimeContributions` 中声明对应种类（`tools`、`commands`、`modelSources`、`skills`；动态设置 schema 使用 `settings`）。声明 `tools`／`modelSources` 动态贡献的包可申请对应的 `["*"]` 权限，用户仍须明确授权。

`api.catalog.replace(0, { tools: [...], commands: [...], modelSources: [...], settingsSchema: {...}, skills: [...] })` 提交首次完整快照。保存返回的 revision，每次修改串行提交；省略的动态数组会被清空。daemon 合并静态声明并验证，失败保留原目录。工具／命令需要在目录获准后再绑定处理器；`await api.tool(...)` 和 `await api.command(...)` 返回释放函数，撤销贡献时先释放，再提交移除后的目录。动态 ID 与静态贡献共用命名空间，不能重复。动态技能沿用第 5 节的字段，须协商 `plugin.skills`，无需绑定处理器；后台就绪后同时进入输入框 `/` 菜单及模型的 `skill_apply` 目录。说明用于发现用途，正文在选用时原样读取；停用或撤权会撤销入口和未提交的选择，旧身份不能继续执行。消息保存接纳时的技能名称，历史展示不随停用或升级重新解释。

动态目录只在内存中生效，每次启动重新建立，不能替代 `api.resource`／`api.settings` 持久化。初始化抛错、崩溃或停用会回收本实例贡献；旧实例的迟到注册不能影响新实例。用 `api.onDispose` 或入口返回的 `dispose` 关闭定时器、连接和文件监听；回收动作应幂等，并且在 daemon 已拒绝新请求时仍能释放本地资源。完整大小与租约合同见[扩展协议第 5 节](extension-protocol-v1.md#5-注册)。

## 7. 钩子

清单声明、代码绑定，两边必须一致：

```json
"extension": { "hooks": [
  { "name": "tool.call", "mode": "guard", "deadlineMs": 3000 }
] }
```

```js
await api.on('tool.call', async (event, ctx) => {
  if (event.tool === 'shell_exec' && /rm\s+-rf/.test(JSON.stringify(event.input))) {
    return { decision: 'block', reason: '禁止递归删除' };
  }
  // 不返回，或返回 { decision: 'continue' }，表示放行
});
```

**三种模式**

- `observe`：只观察，返回值被忽略；处理器形如 `(input, ctx) => {…}`，`ctx.seq` 是递增序号。事件有序投递，但**不保证一定送达**（队列有上限，落后会丢事件）。
- `advise`：可返回 `continue` / `block` / `replace`；自身失败或超时时，Magi 保持原生行为继续。
- `guard`：同样可返回决定，但**失败、超时、熔断或宿主缺失都按阻止处理**，绝不默认放行。

**决定的写法**：返回 `undefined` 等同 `continue`；`{ decision: 'block', reason }` 必须带非空原因，会展示给用户；`{ decision: 'replace', patch }` 的 `patch` 必须是有界对象，形状由钩子决定。`block` 不能被其他插件撤销。

**钩子目录**（清单中声明不在这张表里的钩子或模式，安装会被拒绝，而不是静默忽略）：

| 钩子 | 可用模式 | 输入 | 可返回的 `patch` |
| --- | --- | --- | --- |
| `session.created` / `session.deleted` | observe | 脱敏的领域事件 | — |
| `turn.accepted` / `turn.end` | observe | 接纳或终态事实，不含完整正文 | — |
| `input` | guard | `{ raw: { text, imageCount }, kind, requestId }` | 只能继续或阻止 |
| `prompt.contribute` | advise | `{ target, phase, round, budget }` | `{ fragments: [{ id, stability, priority?, text }] }` |
| `context.assemble` | advise | `{ sessionId, contextWindowTokens, rewritable: { from, to }, messages, omitted }` | `{ rewrites: [{ from, to, replacement: [{ role, text }] }] }` |
| `model.request` / `model.response` / `model.resolved` | observe | 模型与用量的脱敏投影 | — |
| `tool.call` | guard / advise | `{ callId, tool, input }` | `{ input }`，在目录、并发与权限审批之前生效，最终入参会重新校验 |
| `tool.result` | guard / advise | `{ callId, tool, status, projection }` | `{ projection }`，只改模型可见文本，原始回执不变 |
| `compaction.before` | advise | `{ reason, compactedMessages, originalTokens, summaryBudgetTokens, source }` | `block` 取消压缩；`{ summary }` 需授权且通过质量门禁 |
| `subagent.spawn` | guard | `{ role, taskName, goal, toolCallId }` | 只能继续或阻止，不能改角色或模型 |
| `usage.update` | observe | 已提交的用量事实，不含凭据 | — |

补充：

- `tool.call` / `tool.result` / `subagent.spawn` 只在有工具的任务管线里触发，纯对话不会执行工具，所以不会触发。
- `prompt.contribute`：每条片段最多 8K 字符、每阶段合计最多 16K；`start` 阶段只接受 `stable` 并在本轮冻结，`round` 阶段只接受 `volatile`，放在上下文尾部。片段来源由 daemon 标注，不能冒充原生系统段。
- `context.assemble` / `compaction.before` 会改变模型看到的上下文并使提示词缓存前缀失效，必须同时声明权限 `contextRewrite: ["history"]`，并且清单里有且仅当有这类钩子时才声明它。改写提案要经 daemon 校验，被拒绝或扩展失败时保留原生视图；改写**不持久化**，每个新步骤都会重新征询。
- 期限：清单里 `deadlineMs` 取 1–30000 毫秒，省略时默认 5 秒；实际生效的上限 guard 为 30 秒、advise 为 15 秒，超出按上限截断。
- 钩子和 observe 回调里**不能**启动模型、工具、任务、通知或提问（见第 10 节“调用规则”）。

## 8. 模型工具

声明工具需要三处一致：`contributions.tools` 条目、覆盖该工具 ID 的 `tools` 权限、`api.tool` 绑定。

```json
{
  "sdkVersion": 2,
  "id": "acme.words",
  "version": "1.0.0",
  "name": "字数统计",
  "description": "给模型一个统计词数的工具",
  "permissions": [
    { "kind": "extension", "targets": ["full-trust"] },
    { "kind": "tools", "targets": ["word-count"] }
  ],
  "dataSchemaVersion": 1,
  "settingsSchema": { "type": "object", "additionalProperties": false },
  "extension": { "hooks": [] },
  "contributions": {
    "tools": [{
      "id": "word-count",
      "title": "字数统计",
      "description": "统计文本的词数",
      "readOnly": true,
      "inputSchema": {
        "type": "object",
        "required": ["text"],
        "properties": { "text": { "type": "string" } },
        "additionalProperties": false
      }
    }]
  }
}
```

```js
export default async function activate(api) {
  await api.tool('word-count', async (input) => ({
    words: input.text.split(/\s+/).filter(Boolean).length,
  }));
}
```

- 模型看到的工具名由 Magi 生成（`plugin__…`），不要依赖它。
- 参数先按 `inputSchema` 校验（必须是有界的 object schema），不合法的调用不会到达你的代码。
- 返回任意 JSON 作为工具输出；**抛出的错误会作为工具失败返回给模型**，不是协议错误。
- `readOnly` 决定审批：只读工具在“只读”模式下可用；非只读工具在“逐次询问”下需要用户批准，在“只读”下被拒绝。
- 单次调用默认上限 60 秒；可在清单里用 `timeoutMs`（1000–600000）为长任务声明更长期限。超时或用户停止所属 Turn/任务时 `ctx.signal` 中止。停止时如果工具不是 `readOnly`，Magi 无法确认写入是否已生效，结果记为“未确定”，**不会自动重试**。
- 停用、撤权或宿主不可用时，工具从目录中消失或调用失败。

## 9. 用户命令

```json
"extension": { "hooks": [] },
"contributions": { "commands": [{ "id": "stats", "title": "统计", "description": "统计这段文字" }] }
```

```js
await api.command('stats', async (input, ctx) => `共 ${input.text.length} 个字符`);
```

- 用户在输入框用 `/` 选择命令，可附带一段文字（`input.text`）。命令在独立的一轮里执行，**不调用主模型**；返回的文本作为这一轮的回复写回会话。
- 处理器返回非空字符串或 `{ content }`；抛错或返回空文本会让这一轮失败并显示原因。回复最多 1 MiB。
- 上限 120 秒；用户停止该轮时 `ctx.signal` 中止。
- 命令不能与技能、目标模式、图片、上下文引用或会话命令（如 `/compact`）同时提交。
- 授权由 `extension` 的“完全信任”覆盖，命令不需要额外权限。
- 命令携带真实 Task／worker 归属；`model.invoke`、`tools.call` 和 `task.spawn` 等服务仍须各自授权，并沿原生任务策略、审批和账本执行（第 10 节）。命令不触发聊天记忆提取或辅助模型请求。停止、失败或重启中断后不能用通用“继续”重放；用户核对结果后可重新发起命令。

## 10. 宿主服务参考

这些服务让扩展使用 Magi 的业务能力。调用归属、租约和预算由 daemon 注入，扩展不能自报。

### 调用规则

- `resource` 与 `settings` 在 `activate` 期间也可以调用。
- 其余服务（模型、会话快照、通知、提问、工具、子任务）**只能在仍然活动的工具、命令或作业处理器里调用**；在钩子或 observe 回调里调用会立刻失败（`ReentryDenied`）。
- 处理器被取消、超时或所属 Turn 停止后，仍在途的服务调用会被取消。
- 失败是带 `kind` 的异常：

```js
try { await api.model.invoke({ prompt: '…' }); }
catch (error) { console.error(error.kind, error.message); }
```

常见 `kind`：`PermissionDenied`（缺少权限、未激活或包已替换）、`Conflict`（版本冲突）、`ReentryDenied`、`Cancelled`、`DeadlineExceeded`、`InvalidParams`、`Blocked`、`HostGone`、`BlobExpired`、`Internal`。

### 资源与设置

```js
const note = await api.resource.read('notes');          // { version, value }
await api.resource.write('notes', note.version, { text: 'hello' });

const settings = await api.settings.read();             // { version, value }
await api.settings.write(settings.version, { enabled: true });
```

- 写入必须携带读到的版本，过期版本会得到 `Conflict`，请重读后重试。
- 设置按 `settingsSchema` 校验。资源要有对应 `storage` 目标，设置要有 `"__settings"` 目标。
- 这些只覆盖插件自己声明的全局资源，不能写会话、任务或其他插件的数据。

### 调用模型

```js
const reply = await api.model.invoke({ prompt: '用一句话总结……', maxOutputTokens: 512 });
// reply.content 是文本
```

- 需要 `models` 权限；目标 `default` 表示当前默认连接，角色 ID 表示该角色绑定的模型。
- 参数：`prompt`（必填，最大 1 MiB）、`roleId?`、`credentialRef?`、`maxOutputTokens?`（1–32768，默认 4096）。**不接受 `model` 参数**：扩展不能指定或替换模型。
- 调用经 Magi 的模型服务，计入用量账本并归属当前会话；直接用 Node 自己的网络请求不在这个账本里。
- `api.credential.ref({ roleId? })` 返回模型绑定引用（`model-default` 或 `model-role:<roleId>`），**不返回密钥**；它只能交给 `model.invoke` 的 `credentialRef` 使用。

### 提供主对话模型数据源

```json
"permissions": [{ "kind": "modelSources", "targets": ["gateway"] }],
"contributions": { "modelSources": [{ "id": "gateway", "title": "我的反代", "description": "" }] }
```

```js
await api.modelSource.set('gateway', {
  baseUrl: 'http://127.0.0.1:8317/v1',   // OpenAI 兼容或 Anthropic 兼容端点
  apiKey: 'runtime-secret',
  apiProtocol: 'openai_chat',            // openai_responses | openai_chat | anthropic_messages
  models: [{ id: 'my-model', displayName: '我的模型' }],
});
await api.modelSource.clear('gateway');
```

- 数据源 ID 必须先在清单 `contributions.modelSources` 声明，并有同名 `modelSources` 权限；展示名取清单里的 `title`。最多 8 个。
- `set` 是整体替换：新的 `models` 取代旧的。校验与用户自己配置的数据源相同（地址有效、模型 ID 非空且唯一）。
- **只影响主对话，只追加**：数据源以“插件”分组出现在主对话的模型选择器里，身份是 `plugin/<pluginId>/<id>`。它不覆盖用户配置的数据源，不会成为默认选择，子代理、`model.invoke`、辅助、识图和生图模型都不会使用它。
- **只存在于本次运行**：连接信息只在 daemon 内存中，不写盘。插件停用、卸载、崩溃或宿主退出时自动撤销；重启后的实例在 `activate` 里再次提交。已选用该数据源的会话在它不可用期间会明确报错，不会改用其他模型；恢复后可继续使用。
- Magi 设置页不为插件另设数据源列表：用户在对话输入区的模型选择器里看到“插件”分组，插件的启停与授权在插件管理里。授权时会提示：选用后主对话内容（对话历史、工具结果等）会发送到你提供的地址。

### 读取会话

```js
const snap = await api.session.snapshot({ limit: 3 });
```

参数 `sessionId?`、`afterTurnSeq?`、`limit?`（最多 10 个 Turn）。只能读取调用归属的会话；每个 Turn 最多 20 条记录，每条文本截断到 1024 字符。返回 `turns`、`nextAfterTurnSeq`、`hasMore`。

### 通知与提问

```js
await api.notify.user({ title: '完成', message: '已整理 12 条记录' });   // 正文 ≤ 4096 字节，标题 ≤ 256 字节
const answer = await api.ask.user({ questions: [ /* 与原生用户提问相同的结构 */ ] });
```

提问由用户在 Magi 里回答，扩展**不能代答审批**；用户可以选择跳过，结果会体现在返回的答复里；所属轮次被停止时调用以 `Cancelled` 结束。

### 工具与子任务（需要活动任务）

```js
await api.tools.call('file_read', { path: 'README.md' });
await api.task.spawn({ /* 与原生 agent_spawn 相同 */ });
```

- 只能在有活动任务和当前租约的上下文里用（包括模型调用的工具和用户插件命令），并继续检查本插件授权、任务策略和原生工具权限。
- `tools.call` 只接受公开的**原生**工具，不能调用自己或其他扩展的工具；原生工具的 schema、权限、审批和账本照常生效。
- `task.spawn` 沿用原生子代理合同，保留角色、目录与策略，只能指定角色，不能指定模型。

### 大对象

```js
const { ref } = await api.blob.put({ rows: [/* … */] }, 'application/json');
const { value } = await api.blob.get(ref);
```

只存 JSON，单项最多 2 MiB，每个宿主最多 64 项或 16 MiB，5 分钟后过期，只有创建它的插件能读。需要持久保存请用 `resource.write`。

## 11. 持久作业（定时执行命令）

作业由 daemon 保存计划，Node 只提供命令处理器，不要自己写定时器。

创建：向 daemon 提交计划（设置 → 插件详情里的“插件计划”用的是同一入口，只能选当前会话；失败或停止后计划会暂停）：

```text
POST /api/plugins/{pluginId}/jobs
```

```json
{
  "commandId": "stats",
  "sessionId": "<已存在的普通用户会话 ID>",
  "text": "统计今天的记录",
  "runAt": 1767225600000,
  "intervalMs": 3600000,
  "budget": { "maxModelCalls": 2, "maxOutputTokens": 1024, "timeoutMs": 30000 }
}
```

- `commandId` 是清单里声明的命令 ID（不是完整身份）；插件必须处于激活状态。
- `runAt` 是 Unix 毫秒；`intervalMs` 可省略（只执行一次），重复间隔至少 1 分钟。
- 预算：`maxModelCalls` 0–100、`maxOutputTokens` 1–32768、`timeoutMs` 1000–120000，由 daemon 强制；命令里 `ctx.job.budget` 可读到。
- 目标必须是普通用户会话；计划列表与取消分别是 `GET /api/plugins/{pluginId}/jobs` 和 `POST /api/plugins/{pluginId}/jobs/{job}/cancel`。
- 作业沿普通 Session Turn 执行，输出和失败都写入会话记录；到点前 daemon 先持久化执行身份，重启后沿用同一身份，**不会重复执行**。
- 失败、取消或结果不确定时**不会自动重试**；插件停用、包摘要变化（升级）、目标会话不存在或宿主不可用时，计划不会自动换包、改参数或静默重跑。

## 12. 失败、超时与取消

| 情形 | 行为 |
| --- | --- |
| guard 异常、超时、熔断、宿主缺失 | 阻止动作并给出原因，不放行 |
| advise 失败 | 保持 Magi 原生行为，记录诊断 |
| 工具 / 命令超时 | 工具 60 秒、命令 120 秒；`ctx.signal` 中止 |
| 用户停止所属 Turn/任务 | 在途调用被取消；可能写入的工具记为“未确定”，不重试 |
| 熔断 | 连续 5 次失败，或 60 秒内至少 10 次且失败率过半时打开；冷却 30 秒后放行一次探测 |
| 崩溃 / 卡死 | 在途请求以 `HostGone` 结束；1 秒起指数退避重启（上限 60 秒），10 分钟内崩溃超过 5 次则停止自动重启并显示“不可用” |
| 停用 / 升级 / 卸载 | 先关闭准入、等待在途调用结束，再停止宿主并提交状态 |

超时通知不代表你的代码已经停止做所有事：Node 里已发出的文件、网络、进程操作不会被 Magi 撤回，请用 `ctx.signal` 配合自己的清理逻辑。

## 13. 打包、依赖、分发与升级

打包：

```text
node scripts/pack-plugin.mjs <插件目录> <输出.zip>
```

输出可重现的 ZIP（同样内容得到同样摘要）。需要第三方依赖时，先用打包工具把它们并进 `extension/index.mjs`，再打包。

安装：用户在设置 → 插件里安装本地包。安装前会预览权限与版本差异，并把确认绑定到包摘要；官方插件中心和自建目录使用签名目录（序号、过期时间、吊销列表），详见插件中心说明。

升级：

- 升级会保留全局启用意愿，**清空授权与激活**，用户须对新包重新授权。
- 改变 `dataSchemaVersion` 且插件已有持久数据时，**升级会清空该插件的资源数据与设置**——没有数据转换通道。升级前的预览会明确提示。
- 新版读不到旧版本数据格式留下的内容；需要保留的内容请让用户先导出。

## 14. 调试与排错

- **看运行状态**：设置 → 插件，选中你的插件，详情里有扩展的运行状态：运行中、启动中、重启中（含崩溃次数）、不可用及原因。guard 钩子在宿主不可用时会阻止对应动作，详情里会列出被阻止的动作。
- **激活失败**：入口抛错、10 秒内未返回、未声明就注册（`RegistrationDenied`）、清单与代码绑定不一致，都会让激活失败；详情页给出原因。
- **找不到 Node**：Magi 只使用安装包里随附的固定版本 Node，不读用户的 PATH。开发调试时可用环境变量 `MAGI_EXTENSION_NODE` 指向某个 Node。
- **日志**：`api.log(message)` 写入诊断日志，不进入会话记录。
- **模型/服务调用被拒绝**：看异常的 `kind`：`PermissionDenied` 多半是缺少 `models`/`storage` 授权或插件已被撤权；`ReentryDenied` 说明你在钩子里调用了需要活动处理器的服务。
- **本地验证**：仓库的 `npm run test:local-plugins` 会启动独立状态目录的真实 daemon，安装临时 Node 插件并走完安装、授权、调用、停用、升级、卸载；可作为你自己做集成验证的参考。

## 15. 常见错误清单

| 现象 | 原因与办法 |
| --- | --- |
| 安装报“发布包含未允许的文件类型或入口” | 包里有第 3 节之外的文件（`.DS_Store`、`README.md`、`package.json`、多套了一层目录……）；用 `-x '.*'` 重新打包 |
| 激活失败，原因含 `RegistrationDenied` | 代码里 `api.on` / `api.tool` / `api.command` 绑定的名字，没有在清单里声明（或工具缺 `tools` 权限） |
| 激活超时 | `activate` 里有长时间等待或未 `await` 的注册；10 秒内必须返回 |
| 工具、命令不出现 | 插件没有“授权并激活”；工具还需要 `tools` 权限已授权 |
| 清单被拒绝 | 有未知字段；贡献 ID 重名；声明了 `extension` 却没有钩子、工具或命令；缺 `extension: ["full-trust"]` |
| 钩子里调用 `api.model.invoke` 报 `ReentryDenied` | 这些服务只能在工具、命令、作业处理器里用 |
| 命令里 `api.tools.call` / `api.task.spawn` 被拒绝 | 检查当前 Task 租约、插件对应权限和原生任务策略；具有任务归属不代表额外授权 |
| 写资源报 `Conflict` | 版本过期；重新读取再带最新版本写入 |
| 读设置、资源报 `PermissionDenied` | 缺 `storage` 权限（设置的目标是 `"__settings"`） |
| 升级后数据没了 | 改了 `dataSchemaVersion`；这会清空旧数据，见第 13 节 |
| 页面请求没有回应 | 没等到宿主的 `init` 消息就发了请求，或请求里没带 `instanceId` |

## 16. 能力边界

插件系统提供第 1 节列出的形态；以下不在其中：

- 工作流、会话引擎：由 Magi 原生提供，插件不能替换。
- 数据格式迁移：没有转换通道，见第 13 节。
- 运行时安装依赖、包内原生模块或二进制、按通配注册远程工具目录。
- 让扩展指定或替换模型、写 canonical 日志、批准审批。模型数据源只能追加到主对话目录，不能替换用户的数据源或其他角色的模型。


### 常驻浏览器客户端

需要关闭页面后继续订阅事件的插件可声明 `contributions.client: {"entry":"ui/settings.html"}`
并申请 `{"kind":"client","targets":["persistent"]}`；入口必须属于同一清单的 views。
当前窗口在插件激活期间只持有一份隔离客户端，显示位置和视图选择由宿主 context 下发，
无可见页面时继续执行客户端注册的后台逻辑。停用、撤权或升级会销毁旧文档。
会话图片写入和后台持久图片仍分别需要 composer、attachments 授权。
普通 views 不声明 client 时保留原页面生命周期。不要把浏览器文档当作 daemon 会话或持久状态源。

## 会话业务结果

工具或命令可用 `await api.session.message({messageId, text, sourceKind, delivery})` 把结果交给本次调用所属的主会话。清单必须显式请求 `sessionWrite` 的 `context` 或 `wake` 目标；context 不主动请求模型，steer/followup 可产生模型用量，需 wake。不能指定其他会话、在后台回调或子任务中调用；计划作业禁止唤醒。

使用稳定的 messageId 和独立来源（例如 `analysis-result`），不要声称来源是 user、system 或 assistant。等待回执后再报告交付状态：pending 表示待处理，attached 表示已绑定轮次，consumed 表示已进入模型请求；都不表示模型答复成功。取消和未知结果不能自动生成新身份重发。正文、插件身份与名称由 daemon 保存，用户可查看和取消待处理消息。完整限额、身份冲突和生命周期规则见[扩展协议](extension-protocol-v1.md#会话消息交付)。

## 在线安装的请求恢复

管理插件调用 `api.plugins.installBundle({ecosystem:'dsh', package, version, requestId, enabled})` 时，requestId 应标识一次用户操作并在重连／应用重启后保持稳定。同插件、同包摘要、同输入的重发领取原回执；新宿主代次不会使同一操作重新执行。不能修改原 requestId 的输入，也不能用随机新身份自动重试未知结果。

`installation-interrupted` 表示应用退出前任务未完成结算。`installed:true` 仅证明候选摘要已经安装，不能视作授权或运行成功；提示用户到 Magi 插件管理检查状态，确认后再用新 requestId 显式重试。恢复不自动下载、安装或代授权。原回执最多保留 256 项、至少保留至请求创建后七天；容量不足时明确失败。完整提交边界、取消及损坏日志处理见[扩展协议](extension-protocol-v1.md)。
