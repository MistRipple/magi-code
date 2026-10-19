# Magi 插件作者指南

这是一份可以单独保存、复制给其他开发者或交给 AI 使用的 Magi SDK 1 开发手册。开发不需要 Magi 源码、项目文档或项目脚本。你需要了解基本 JavaScript 和 JSON；增加页面时需要基本 HTML。插件运行者只需安装 Magi，作者打包时可使用本文提供的 Python 3 脚本。

第一次开发，按第 1–3 节完成一个只有两个文件的插件；需要保存数据时读第 4 节，需要设置或页面时再读第 5–6 节。第 7–8 节处理升级和分发，第 9 节是接口速查，第 10 节是进阶合同。

## 1. 先确认你的功能适合哪种扩展

插件把新增能力、数据和可选页面打成一个 ZIP，用户在 Magi 设置里安装。先按需求判断当前能否实现：

| 你想做什么 | 当前支持情况 | 从哪里开始 |
| --- | --- | --- |
| 给模型增加计算、文本转换、格式检查工具 | 支持，使用纯 JavaScript 和 JSON 输入输出 | 第 2 节 |
| 保存插件数据，让工具和页面共同使用 | 支持，数据带版本、可订阅；全局共享 | 第 4 节 |
| 增加用户主动选择的命令 | 支持，执行后返回一段会话回复 | 第 4 节 |
| 增加设置表单、主页面或右侧面板 | 支持，页面通过消息桥读写数据 | 第 5–6 节 |
| 直接读取项目文件、执行 shell、调用远端 API | 通用插件 SDK 暂不支持；后台没有文件、进程或网络接口 | 这类需求不能仅靠申请权限实现 |
| 在插件后台直接调用任意模型或其他工具 | 没有通用 `sdk.call` 接口；工作流可提交受控动作 | 先读第 10 节限制，再决定是否适用 |
| 接入 MCP 服务、安装 Skill | Magi 有各自的配置入口；当前不支持把它们声明进此 ZIP 清单 | 使用对应功能入口 |
| 驱动浏览器、保持后台常驻、监听任意宿主事件 | 没有通用第三方 SDK；内置 GPT Web/MCP 插件的专用能力不对外开放 | 不适合作为本文的入门目标 |

插件的安装、授权、启用和数据都对所有工作区共用一份。切换工作区不会获得一份新的便签或配置；不要用插件数据保存另一套 Magi 会话或任务状态。

## 2. 第一个插件：两个文件完成文字回显

目标：让模型调用一个“回显文字”工具，返回确定的 JSON。此例不需要页面、数据存储或网络连接。

创建 `hello-plugin` 文件夹，保存以下两个文件，使用 UTF-8 编码：

```text
hello-plugin/
  manifest.json
  plugin.mjs
```

### 文件：manifest.json

清单描述插件身份、工具参数和权限。`example.echo` 是学习用 ID，发布自己的插件时换成你的命名空间。

```json
{
  "sdkVersion": 1,
  "id": "example.echo",
  "version": "1.0.0",
  "name": "文字回显",
  "description": "把输入的文字作为结构化结果返回。",
  "backend": "plugin.mjs",
  "permissions": [
    { "kind": "tools", "scope": "application", "targets": ["echo-text"] }
  ],
  "settingsSchema": { "type": "object", "additionalProperties": false, "properties": {} },
  "dataSchemaVersion": 1,
  "contributions": {
    "tools": [{
      "id": "echo-text",
      "title": "回显文字",
      "description": "返回传入的原始文字。验证插件时调用此工具，不要自行推测结果。",
      "inputSchema": {
        "type": "object", "additionalProperties": false, "required": ["text"],
        "properties": { "text": { "type": "string", "maxLength": 2000 } }
      },
      "readOnly": true
    }]
  }
}
```

### 文件：plugin.mjs

这是唯一后台入口。Magi 校验工具参数后，把参数作为 `input` 传入；`sdk` 提供宿主签发的调用身份。

```js
export default function main(input, sdk) {
  const { kind, id } = sdk.invocation.contribution;
  if (kind === 'tool' && id === 'echo-text') {
    return { text: input.text };
  }
  throw new Error('未处理的贡献');
}
```

### 打包成 ZIP

最简单的方法是选中文件夹**里面的两个文件**压缩，不要把 `hello-plugin` 文件夹本身套进 ZIP 根目录。也不要混入 `.DS_Store`、`__MACOSX` 或其他隐藏文件。

需要可重复打包时，将下面内容保存为 `pack.py`，放在 `hello-plugin` **外面**。脚本只使用 Python 3 标准库，以后打包其他插件也能复用：

```python
import json, sys
from pathlib import Path
from zipfile import ZipFile, ZipInfo, ZIP_DEFLATED
root = Path(sys.argv[1]).resolve()
manifest = json.loads((root / 'manifest.json').read_text(encoding='utf-8'))
output = root.parent / (manifest['id'] + '-' + manifest['version'] + '.zip')
for name in ('manifest.json', 'plugin.mjs'):
    if not (root / name).is_file():
        raise ValueError('缺少 ' + name)
with ZipFile(output, 'w') as archive:
    for path in sorted(root.rglob('*')):
        if path.is_symlink():
            raise ValueError('不允许符号链接: ' + str(path))
        if path.is_file():
            info = ZipInfo(path.relative_to(root).as_posix())
            info.compress_type = ZIP_DEFLATED
            info.external_attr = 0o100644 << 16
            archive.writestr(info, path.read_bytes())
print(output)
```

在两个路径所在目录执行：

```bash
# macOS / Linux
python3 pack.py hello-plugin
```

```powershell
# Windows（已安装 Python 3）
py -3 pack.py hello-plugin
```

输出是 `example.echo-1.0.0.zip`。脚本只打包，不替代 Magi 的安装校验。

### 在 Magi 安装并观察结果

以下按钮名按中文界面描述：

1. 打开设置 → 插件 → **安装来源**，在“从其他来源安装”区域点击**选择 ZIP…**，选择刚生成的文件。
2. 预览应显示“文字回显”、`example.echo`、`1.0.0` 和 `tools / echo-text` 权限。点击**确认安装**。
3. 在左侧找到“文字回显”，在右侧点击**授权并激活**。成功状态应为“已激活，所有工作区可用”。仅“已安装”还不能调用。
4. 回到对话，选择已配置、支持工具调用的普通 API 模型，新建会话，发送：`请实际调用“回显文字”工具，text 传入“你好 Magi”，并报告工具结果。`
5. 展开这次工具调用记录，应该看到输入 `{"text":"你好 Magi"}` 和包含相同文字的结果。工具执行记录是验收依据；模型在回复里说“调用成功”不能代替它。

这个示例验证插件执行，不负责配置模型。如果模型没有调用工具，先按下一节排查，不要把模型直接回复的文字当作插件输出。

## 3. 开发、调试和重复安装

### 每次修改后的固定流程

修改本地文件 → 提高清单 `version`（例如 `1.0.0` → `1.0.1`）→ 重新打包 → 设置 → 插件 → 选中插件 → **从文件升级** → 检查预览并**确认升级** → **授权并激活** → 再次测试。

本地文件不是运行中的代码，不会热加载；不必重新编译 Magi，也不必为普通插件改动重启桌面端。同一份 ZIP 会提示“无需安装”，请核对版本、包摘要和你选择的文件。升级清空旧授权，必须重新激活。调试时先保持 `dataSchemaVersion` 不变，数据格式变化再按第 7 节迁移。

### 错误在哪里看

| 阶段 | 去哪里看 | 先检查什么 |
| --- | --- | --- |
| 选择 ZIP 后预览失败 | 设置 → 插件页的错误提示；确认是否出现候选包预览 | ZIP 根目录、JSON 语法、未知字段、贡献 ID 重复、文件类型 |
| 安装成功但功能不出现 | 选中插件，查看右侧版本、权限和激活状态 | 点击“授权并激活”；确认当前已是目标版本 |
| 模型不调用工具 | 本轮对话的工具记录、当前模型配置 | 模型是否支持工具、插件是否激活；新建会话重试明确调用指令 |
| 已调用但执行失败 | 展开本轮插件工具卡片；同时核对输入 | 默认导出、贡献 ID、允许的 SDK、返回 JSON、资源和权限声明 |
| 命令执行失败 | 对应会话回复与插件状态 | 命令返回值必须为非空 `{content: string}`，不能返回普通工具结果 |
| 页面不响应 | 页面自己的状态文字；支持时可在桌面 View 菜单打开开发者工具查看 iframe Console/Network | 等待 init、匹配 instanceId、包内资源路径、消息响应与超时 |
| 页面保存失败 | 页面状态提示 | 插件是否停用、资源是否授权、版本是否冲突、数据是否超限 |
| 桌面后台服务无法启动 | 启动错误窗口中的“打开日志位置” | 这是宿主启动故障，先恢复服务，再测试插件 |

侧栏“异常记录”用于查看系统异常，不能保证包含每一次插件拒绝和完整脚本堆栈。当前没有独立的插件日志控制台；后台也没有 `console.log`，抛出的自定义错误不保证原样回显。

对于需要定位的计算逻辑，可以临时返回不含敏感数据的诊断字段，例如 `{ok:true, stage:'parsed', count:3}`，从工具结果观察；工具参数 schema 拒绝时后台根本不会启动。对 SDK 失败只报告明确失败，不要把异常捕获后返回“成功”。页面使用 `textContent` 展示消息，不把工具内容当 HTML 执行。

### 首次成功后应保留的检查

最小插件至少检查：正确输入、超长/错误类型输入被拒绝、停用后工具消失、升一个补丁版本后行为确实更新。增加存储或页面后再检查其对应场景，无需每次把尚未使用的高级能力全部测试一遍。

## 4. 增加持久数据和显式命令

工具是模型决定何时调用的能力；命令是用户从输入区主动选择的动作；资源是两者和页面共享的 JSON 数据。资源自带版本，避免后保存的旧数据覆盖先保存的新数据。

下面是独立的第二个示例“便签”。新建 `my-note` 文件夹，沿用同一套打包方法；它使用另一个插件 ID，可以和“文字回显”同时安装。本节先只做后台，不需要 `ui/`。

### 文件：manifest.json

```json
{
  "sdkVersion": 1,
  "id": "example.note",
  "version": "1.0.0",
  "name": "便签",
  "description": "页面和模型工具共享一份便签。",
  "backend": "plugin.mjs",
  "permissions": [
    {
      "kind": "storage",
      "scope": "application",
      "targets": [
        "note"
      ]
    },
    {
      "kind": "tools",
      "scope": "application",
      "targets": [
        "read-note",
        "write-note"
      ]
    }
  ],
  "settingsSchema": {
    "type": "object",
    "additionalProperties": false,
    "properties": {}
  },
  "dataSchemaVersion": 1,
  "contributions": {
    "resources": [
      {
        "id": "note",
        "title": "便签内容",
        "description": "页面与工具共享的数据。"
      }
    ],
    "tools": [
      {
        "id": "read-note",
        "title": "读取便签",
        "description": "读取便签文字和版本。",
        "inputSchema": {
          "type": "object",
          "additionalProperties": false,
          "properties": {}
        },
        "readOnly": true
      },
      {
        "id": "write-note",
        "title": "保存便签",
        "description": "替换便签文字。",
        "inputSchema": {
          "type": "object",
          "additionalProperties": false,
          "required": [
            "text"
          ],
          "properties": {
            "text": {
              "type": "string",
              "maxLength": 2000
            }
          }
        },
        "readOnly": false
      }
    ],
    "commands": [
      {
        "id": "clear-note",
        "title": "清空便签",
        "description": "清空已保存的文字。"
      }
    ]
  }
}
```

### 文件：plugin.mjs

```js
export default function main(input, sdk) {
  const { kind, id } = sdk.invocation.contribution;
  if (kind === 'tool' && id === 'read-note') {
    return sdk.call('resource.read', { resourceId: 'note' });
  }
  if ((kind === 'tool' && id === 'write-note') || (kind === 'command' && id === 'clear-note')) {
    const current = sdk.call('resource.read', { resourceId: 'note' });
    const saved = sdk.call('resource.write', {
      resourceId: 'note', expectedVersion: current.version,
      value: { text: kind === 'command' ? '' : input.text },
    });
    return kind === 'command' ? { content: '便签已清空。' } : saved;
  }
  throw new Error('未处理的贡献');
}
```

在上一级目录运行 `python3 pack.py my-note`（Windows 为 `py -3 pack.py my-note`），得到 `example.note-1.0.0.zip`，按第 2 节安装并激活。

依次验证：

1. 要求模型实际调用“读取便签”，首次应返回 `{version:0,value:null}`。
2. 要求调用“保存便签”，参数为 `{"text":"第一条便签"}`，成功结果应为 `{version:1,value:{text:"第一条便签"}}`；在另一会话或工作区读取，也应得到它。
3. 点击输入区左下角 **＋（添加上下文与能力）**，选择插件命令“清空便签”，确认输入区出现所选命令，再发送。也可输入 `/`，在“插件命令”分组选择它。应收到“便签已清空。”；再次读取时 `value.text` 为空，资源版本继续递增。

`expectedVersion` 必须使用刚读到的 `current.version`。它与包版本 `version`、数据格式版本 `dataSchemaVersion` 是三个不同概念。发生并发冲突时重新读取并重新计算，或让用户确认；不要换成最新版本号后直接覆盖旧内容。写工具声明 `readOnly:false`，仍受会话访问模式检查；命令是用户显式动作，但内部资源操作一样需要存储授权。

## 5. 增加设置：先用自动表单

通常不必专门写设置页面。在便签清单中：

1. 将包版本提高为 `1.1.0`。
2. 在原有 `storage` 权限的 `targets` 数组里追加 `"__settings"`，保留 `"note"`。
3. 用下列对象完整替换 `settingsSchema`：

```json
{
  "type": "object", "additionalProperties": false,
  "properties": {
    "title": { "type": "string", "title": "便签标题", "default": "我的便签", "maxLength": 100 }
  }
}
```

为了验证设置确实影响功能，在打包之前，将后台原来的 `read-note` 分支完整替换为：

```js
if (kind === 'tool' && id === 'read-note') {
  const resource = sdk.call('resource.read', { resourceId: 'note' });
  const settings = sdk.call('settings.read', {});
  return { ...resource, title: settings.value?.title ?? '我的便签' };
}
```

将清单和后台修改一起打包到 `1.1.0`，从文件升级并授权激活。选中便签插件，点击**打开设置**，应出现“便签标题”表单。保存标题后调用“读取便签”，结果的 `title` 应是你保存的值，关闭重开设置页也应保留。只增加可选字段且旧值仍合法时不必提高 `dataSchemaVersion`；如果已有设置不符合新 schema，按第 7 节迁移。Schema 的 `default` 不代表已经存入数据，首次设置快照仍可能是 `{version:0,value:null}`。写设置时先读版本，再调用 `settings.write`，参数为 `{expectedVersion, value:{title:'新标题'}}`；写入的是完整对象，不是局部合并。

不声明自定义 `settings` 视图时，Magi 根据 schema 自动生成表单，字段按声明顺序展示。设置同样是普通插件数据，不能用来存放密码或 API 密钥。

## 6. 增加页面：显示并编辑同一份便签

只有需要交互界面时再加页面。模型工具与命令在没有页面时仍可使用。

以第 4 节便签为基础，将包版本提高（例如已做第 5 节时用 `1.2.0`），保持原有贡献，在 `contributions` 对象里**增加**以下 `views` 字段。以下是要合并的字段片段，不是完整清单：

```json
{
  "views": [
    {
      "id": "note-page",
      "title": "便签",
      "entry": "ui/index.html",
      "placements": [
        "main",
        "home",
        "rightPane"
      ]
    }
  ]
}
```

新增 `ui/index.html` 文件。下面是可直接使用的完整页面，不需要 npm、构建工具或外部页面库。它只读写便签；若做过第 5 节，自动设置表单仍可单独使用，本页不消费标题设置。

### 文件：ui/index.html

```html
<!doctype html>
<html lang="zh-CN">
<meta charset="utf-8">
<title>便签</title>
<style>
  body { font: 14px system-ui; margin: 16px; color: CanvasText; background: Canvas; }
  textarea { display: block; box-sizing: border-box; width: 100%; min-height: 120px; margin-bottom: 8px; }
  button, textarea { font: inherit; }
</style>
<h1>便签</h1>
<textarea id="text" maxlength="2000" aria-label="便签文字" disabled></textarea>
<button id="save" disabled>保存</button>
<button id="reload" disabled>重新读取</button>
<p id="status" role="status"></p>
<script>
  const channel = 'magi-plugin-v1';
  const editor = document.getElementById('text');
  const save = document.getElementById('save');
  const reload = document.getElementById('reload');
  const status = document.getElementById('status');
  const pending = new Map();
  let instanceId, version = 0, nextId = 0, dirty = false;
  function request(operation, fields = {}) {
    return new Promise((resolve, reject) => {
      const requestId = String(++nextId);
      const timer = setTimeout(() => {
        pending.delete(requestId);
        reject(new Error('请求超时，请重新打开页面后读取最新数据。'));
      }, 10000);
      pending.set(requestId, { resolve, reject, timer });
      parent.postMessage({ channel, instanceId, type: 'request', requestId, operation, ...fields }, '*');
    });
  }
  function apply(resource) {
    // 读请求和订阅事件可能交错到达；不使用较旧快照覆盖当前版本。
    if (resource.version < version) return;
    version = resource.version;
    editor.value = resource.value?.text ?? '';
    dirty = false;
  }
  async function read() {
    apply(await request('resource.read', { resourceId: 'note' }));
    status.textContent = '已读取版本 ' + version;
  }
  window.addEventListener('message', async (event) => {
    const m = event.data;
    if (event.source !== parent || !m || m.channel !== channel) return;
    if (m.type === 'init' && !instanceId) {
      instanceId = m.instanceId;
      try {
        await request('resource.subscribe', { resourceId: 'note' });
        await read();
        editor.disabled = save.disabled = reload.disabled = false;
      } catch (error) { status.textContent = error.message; }
      return;
    }
    if (m.instanceId !== instanceId) return;
    if (m.type === 'response') {
      const waiter = pending.get(m.requestId);
      if (!waiter) return;
      clearTimeout(waiter.timer);
      pending.delete(m.requestId);
      m.ok ? waiter.resolve(m.result) : waiter.reject(new Error(m.error));
    } else if (m.type === 'event' && m.resourceId === 'note') {
      if (m.error) status.textContent = '订阅已断开，请重新打开页面。';
      else if (m.resource.version > version) {
        // 编辑中的草稿不被远端更新覆盖；保留旧版本，让保存明确报告冲突。
        if (dirty) status.textContent = '便签已在其他位置更新；请保留草稿并重新读取。';
        else apply(m.resource);
      }
    }
  });
  editor.oninput = () => { dirty = true; };
  reload.onclick = () => read().catch(error => { status.textContent = error.message; });
  save.onclick = async () => {
    const text = editor.value;
    const expectedVersion = version;
    editor.disabled = save.disabled = reload.disabled = true;
    try {
      const saved = await request('resource.write', { resourceId: 'note', expectedVersion, value: { text } });
      apply(saved);
      status.textContent = '已保存版本 ' + version;
    } catch (error) {
      status.textContent = error.message + '。草稿已保留，请复制草稿后重新读取再编辑。';
    } finally { editor.disabled = save.disabled = reload.disabled = false; }
  };
</script>
</html>
```

重新打包、升级并激活。从空白会话主页的“便签”入口打开主页面；也可展开右侧面板，点击“新增面板”选择便签。

页面代码稍长，是因为它包含完整消息收发、请求超时、订阅和草稿冲突处理。先直接运行，再根据需要修改。它在 Magi 的隔离 iframe 内执行，用普通浏览器双击 HTML 不会收到初始化消息。

验证下面四件事：

1. 页面保存文字后，要求模型调用读取工具，应读到相同内容；关闭重开页面也应保留。
2. 页面打开时，要求模型调用保存工具，页面应收到订阅并显示新值。
3. 在主页面输入草稿，再在右侧的第二份便签页面保存其他内容。第一份页面应保留草稿、提示更新；保存旧草稿会冲突，不能悄悄覆盖新值。复制草稿后点“重新读取”再编辑。
4. 在插件管理中停用便签，页面入口和工具应失效；重新激活后数据仍保留。

### 页面位置和通信要点

`placements` 可选 `main`（完整页面）、`rightPane`（右侧 Tab）、`settings`（自定义设置）、`home`（主页打开入口，必须同时有 `main`）、`homeCard`（空会话主页卡片，每包最多 2 张，同时有 `main` 时带“打开”按钮）。同一份 HTML 可放多个位置，但每份挂载有独立实例 ID，数据通过资源同步。

页面与后台是不同环境：页面可使用 DOM 和浏览器定时器，后台不能。页面不能直接访问父界面、Electron 或网络，静态依赖必须随 `ui/` 发布，不使用 CDN。等宿主发来 `init` 再请求，把实例 ID 原样回传；协议及可用操作见第 9 节。

## 7. 修改、升级和数据迁移

先区分三个版本：

| 版本 | 示例 | 什么时候改变 |
| --- | --- | --- |
| 清单 `version` | `1.2.0` → `1.2.1` | 发布新的代码、页面或清单 |
| 清单 `dataSchemaVersion` | `1` → `2` | 持久资源或设置格式需要转换 |
| 资源快照 `version` | `3` → `4` | 每次成功写入，由 Magi 递增；提交写入用它做并发校验 |

普通升级按第 3 节操作；同一插件保持 `id` 不变，查看右侧版本确认更新确实生效。不要为了测试更新先卸载：卸载默认保留数据，并不能获得全新的空状态；练习时确需独立数据可换一个测试插件 ID，发布升级时保持原 ID。

### 有数据时改变格式

例如新版给便签增加 `schema` 字段，并决定把数据格式提高到 2。将清单包版本提高，同时设置：

```json
{ "dataSchemaVersion": 2, "dataMigration": { "fromVersions": [1] } }
```

这是要合并到清单顶层的字段片段。`fromVersions` 填旧**数据格式版本整数**，不是 `1.2.0` 这类包版本。它包含 1–8 个互不相同、为正且小于新版本的整数。不存在持久数据时，不要求迁移声明。

在唯一 `plugin.mjs` 的函数内部，取得 `{kind,id}` 之后、工具和命令分支之前加入：

```js
if (kind === 'migration') {
  if (input.resourceId === 'note') {
    return { value: { text: input.value?.text ?? '', schema: input.toVersion } };
  }
  return { value: input.value };
}
```

这个转换同时支持已有设置：遇到 `__settings` 就保持原值，但若设置 schema 也变了，必须在此生成符合新版 schema 的值。再把普通写入分支原来的 `value` 属性替换为下面这一行，使之后保存或清空的便签也保持新格式：

```js
value: { text: kind === 'command' ? '' : input.text, schema: 2 },
```

迁移时 Magi 逐条传入 `{fromVersion,toVersion,scope,resourceId,value}`，贡献为 `{kind:'migration',id:'data'}`。只能读输入、返回 `{value:新值}` 或 `{drop:true}`，不允许任何 `sdk.call`。保留的资源必须还在新清单中声明，设置符合新 schema，结果受数据和运行报文大小限制。

预览应说明将转换已有数据。先用测试 ID 和样本数据验收成功转换，再故意让转换抛错，确认升级被拒绝且旧数据保留。转换、校验或提交前失败保持旧版本；提交后恢复到新版与已转换数据，不混用新代码和旧格式。

### 停用、撤权和卸载

关闭页面只释放这份页面，不删除数据。停用和升级先停止新调用与页面编辑，默认等待在途调用 30 秒，再取消并给 10 秒结算；无法结算会明确失败。撤权立即取消相关调用。卸载前先停用，再禁用，状态满足条件后卸载按钮才可用；卸载保留配置和数据。

## 8. 把插件交给其他用户

### 直接分享 ZIP 或下载地址

先按第 2 节打包，再把 ZIP 交给用户，或放到可直接下载的 HTTP(S) 地址。用户从设置 → 插件 → 安装来源选择本地 ZIP 或地址安装，完成预览、确认安装、授权激活。

发布新版本时提高 `version`，使用新文件名，保留插件 ID；不要同版本替换内容。上线前检查包里没有凭据、开发依赖或测试数据，并提供功能说明、所需权限、适用平台和升级说明。使用模型工具的插件还应说明需要支持工具调用的模型。

### 可选：自托管插件中心

有多个插件或希望用户浏览版本时，再提供 `catalog.json`。不需要搭建后台服务。将以下内容保存为插件目录外的 `catalog.py`，在 ZIP 所在位置运行，参数为刚才生成的包：

```python
import hashlib, json, sys
from pathlib import Path
from zipfile import ZipFile
path = Path(sys.argv[1])
with ZipFile(path) as archive:
    manifest = json.loads(archive.read('manifest.json'))
entry = {key: manifest[key] for key in ('id', 'name', 'description', 'version')}
entry.update(url=path.name, sha256=hashlib.sha256(path.read_bytes()).hexdigest(), size=path.stat().st_size)
Path('catalog.json').write_text(json.dumps({'format': 1, 'plugins': [entry]}, ensure_ascii=False, indent=2), encoding='utf-8')
print('已生成 catalog.json')
```

```bash
python3 catalog.py example.echo-1.0.0.zip
```

Windows 使用 `py -3 catalog.py example.echo-1.0.0.zip`。脚本生成只有这个版本的目录；维护多个条目时，把它们合并到 `plugins` 数组，不要运行一次就覆盖旧条目。

把目录和 ZIP 托管在同一 HTTP(S) 静态目录，将目录地址告诉用户：设置 → 插件 → 安装来源，在“插件中心”区域填写目录地址并保存。自托管未签名目录可用，来源按“指定地址”展示，安装仍需授权。普通开发和直接分享不要求上架官方中心，也不要求签名。

### 交付前验收

只测试实际用到的能力：安装与重新激活、正确/错误工具参数、命令返回、设置保存、页面重开与同步、并发冲突、停用后失效，以及带旧数据升级。声明支持的操作系统应分别实际运行。涉及迁移时额外验证失败不破坏旧数据；不要只测试空数据首次安装。

## 9. 接口参考：清单、权限、SDK 和页面消息

发布包为 ZIP，最多 16MiB，展开总量最多 32MiB，最多 512 个条目，单文件最多 4MiB；后台 `plugin.mjs` 最多 512KiB。只能包含根目录的 `manifest.json`、`plugin.mjs` 和 `ui/` 下的静态资源。允许的静态资源扩展名为 `html/css/js/mjs/json/svg/png/jpg/jpeg/webp/gif/woff/woff2`，文本使用 UTF-8。文件不得带可执行权限，也不得携带原生程序、动态库、字节码、WASM、符号链接或安装后脚本。

路径使用 `/`，最长 256 字节，每段只含 ASCII 字母、数字、`-`、`_`、`.`；不能有空段、`.`、`..`、尾随点或 Windows 保留设备名，例如 `CON`、`NUL`、`COM1`。以下上限涉及文字时，建议按 UTF-8 字节控制，避免中文名称超过宿主限制。

清单顶层只接受以下字段，不能自行添加 `author`、`dependencies`、`scripts`、`skills` 或 `mcpServers` 等字段：

| 字段 | 必填 | 类型与含义 |
| --- | --- | --- |
| `sdkVersion` | 是 | 整数，当前只能为 `1` |
| `id` | 是 | 最长 96；至少两个点分段，如 `your-name.note`，每段以小写字母开头，只含小写字母、数字和连字符 |
| `version` | 是 | 最长 64 的 SemVer 字符串，如 `1.0.0`；升级时提高版本 |
| `name` | 是 | 非空显示名称，最长 128 |
| `description` | 是 | 说明，最长 4096 |
| `backend` | 是 | 固定为 `"plugin.mjs"`，即使只有页面也要提供此文件及默认导出函数 |
| `permissions` | 是 | 权限数组，无需权限时为 `[]`，最多 64 项；格式见本节“权限与作用域” |
| `contributions` | 是 | 贡献对象，字段见下表；未使用的贡献数组可省略 |
| `settingsSchema` | 是 | 根 `type` 必须为 `"object"` 的 JSON Schema；无设置时使用示例中的空对象 schema |
| `dataSchemaVersion` | 是 | 正整数，表示数据格式版本，与包的 SemVer 版本分开维护 |
| `dataMigration` | 否 | `{ "fromVersions": [1] }`；升级转换规则见第 7 节 |

贡献 ID 最长 64，以小写字母开头，只含小写字母、数字和连字符。所有贡献类型共用一个命名空间，例如资源和工具不能都叫 `note`。一包最多 128 项贡献，至少声明一项贡献或权限。`title` 必须非空且最长 128，`description` 最长 4096。

| `contributions` 字段 | 每项的必填字段 | 用途 |
| --- | --- | --- |
| `tools` | `id, title, description, inputSchema, readOnly` | 模型工具；`readOnly` 是布尔值，`inputSchema` 校验工具参数 |
| `commands` | `id, title, description` | 用户显式选择的命令 |
| `resources` | `id, title, description` | 声明版本化数据，不需要给出初始值 |
| `views` | `id, title, entry, placements` | 隔离页面；`entry` 最长 256，必须指向实际存在的 `ui/*.html`，可有子目录 |
| `workflows` | `id, title, description` | 工作流决策核心，详见第 10 节 |
| `engines` | `id, title, description` | 会话引擎，详见第 10 节 |
| `browserSurfaces` | `id, title, description, scope, persistence, keepMounted, closeAction, backgroundInference` | 原生浏览器 surface 声明；当前第三方限制见第 10 节 |

`settingsSchema` 和 `inputSchema` 必须是合法 JSON Schema，序列化大小各不超过 32KiB，只能用 `#` 开头的内部 `$ref` / `$dynamicRef`，不能依赖其他文件或网络 schema。建议使用 `additionalProperties: false`、`required` 和字符串/数组长度上限，使错误尽早被发现。

### 权限与作用域

每条权限格式为 `{ "kind": "storage", "scope": "application", "targets": ["note"] }`。`targets` 为精确目标列表，每项非空且最长 1024，最多 64 项；`"*"` 不表示通配授权。完全相同的权限声明不能重复。激活前必须获得清单全部权限的授权；授权绑定本次 ZIP 摘要，升级后需要重新授权。

插件全局安装、授权、启用并存储数据，运行作用域固定为 `application`。权限声明的 `scope` 字段允许 `application`、`workspace`，但 `workspace` 声明不会创建按工作区独立的插件实例或数据；新插件应使用 `application`。视图收到的 `scope` 也为 `application`，不要自行拼接工作区 ID。

| `kind` | `targets` 内容 | 当前可用能力 |
| --- | --- | --- |
| `storage` | 已声明资源 ID；设置用保留目标 `__settings` | 后台 SDK 与页面读写资源、设置 |
| `tools` | 本插件工具贡献 ID | 将对应工具暴露给模型并允许进入工具执行；写工具仍受会话访问模式约束 |
| `models` | 引擎贡献 ID | 会话引擎准入检查；工作流模型动作由宿主按会话配置执行，不是任意模型调用 SDK |
| `browser` | surface 贡献 ID，与 surface 的 `scope` 一致 | 原生 surface 授权检查；没有通用页面驱动 SDK |
| `filesRead`、`filesWrite`、`network` | 清单可声明目标 | 当前通用后台没有对应 SDK 操作；申请这些权限不会获得文件或网络 API |

命令没有独立的 `commands` 权限种类；执行时要求插件已激活，其内部资源操作仍检查 `storage` 权限。插件设置不是秘密存储，不要把 API 密钥、密码放进资源、设置、工具结果或页面消息。

### 后台执行与 SDK

`plugin.mjs` 是无外部 import 的单个 ESM bundle，默认导出 `(input, sdk) => JSON | Promise<JSON>`。可以用 JavaScript 直接编写，也可以在作者机器把 TypeScript 与依赖打包为一个文件。默认导出必须返回可 JSON 序列化的值，不能返回 `undefined`、函数、循环对象或 `BigInt`。

每次调用使用全新的 QuickJS 实例，不提供 Node.js、DOM、`fetch`、`process`、`require`、`console`、文件、网络或进程 API，也没有浏览器定时器。`sdk.call` **同步返回 JSON 或抛出异常**；默认导出的 Promise 只能依靠此环境内可结算的工作，不能等待未提供的网络或定时器。跨调用状态写入资源，不能依赖全局变量保活。

当前默认限制：内存 32MiB、栈 512KiB、单次执行 5 秒、64 次 SDK 调用、IPC 报文 1MiB；SDK 参数和最终结果还需小于半个报文上限，建议明显小于 512KiB。这些由宿主设置，插件不能在清单里提高。资源存储虽然允许单条 4MiB，后台读取和写入仍受更小的 IPC 上限约束。

`sdk.invocation.contribution` 是宿主签发、深度冻结的 `{kind, id}`。`kind` 分别为 `tool`、`command`、`workflow`、`engine`、`migration`；单入口据此分派。其他调用身份由宿主绑定，不能通过参数指定其他插件、工作区或任务。

| SDK 调用 | 参数 | 返回 |
| --- | --- | --- |
| `sdk.call('resource.read', ...)` | `{resourceId}` | `{version, value}` |
| `sdk.call('resource.write', ...)` | `{resourceId, expectedVersion, value}` | 成功后的 `{version, value}` |
| `sdk.call('settings.read', {})` | 空对象 | 设置快照 `{version, value}` |
| `sdk.call('settings.write', ...)` | `{expectedVersion, value}` | 校验并保存后的设置快照 |

以上四个是当前通用后台开放的全部操作；订阅和上下文选择仅在页面消息桥可用。未写过的资源或设置读到 `{version: 0, value: null}`。Schema 的 `default` 不等于已保存的数据，后台应显式处理 `null`，例如 `settings.value?.title ?? '我的便签'`。

所有写入都要求刚读到的 `expectedVersion`，成功后版本递增。版本冲突时保留用户输入，重新读取后让用户重新确认，或基于新值重新计算；不能直接把旧内容套上新版本强行覆盖。后台 SDK 的拒绝目前可能只抛出通用错误，不要依赖错误文本解析版本号或无限重试。后台抛出的自定义异常也不保证原样展示；可恢复的业务结果应返回明确 JSON，如 `{ok: false, reason: '容量已满'}`。

工具的 `input` 就是通过 `inputSchema` 校验的参数对象。工具返回 JSON 作为结果；`readOnly: true` 必须如实表示没有写入，不能用它规避确认。模型可见工具名由宿主生成，不要自行拼接或依赖其中摘要。

命令的 `input` 是 `{commandId, sessionId, turnId, workspaceId, input}`，其中 `input` 为用户输入字符串，`workspaceId` 可以为 `null`。命令只返回 `{content: string}`，不附加其他字段；文本非空，最长 1MiB，并受前述 Worker 报文上限约束。宿主负责把它写回本轮对话。

### 页面消息协议

视图运行在 `sandbox="allow-scripts"` 的 iframe 中，origin 为 `null`。不能访问父页面 DOM、Electron 桥或直接连接网络。CSP 允许包内脚本、样式、图片和字体，以及内联脚本/样式和 data 图片/字体；禁止网络连接、外部依赖 CDN、表单导航和原生对象。所有依赖随 `ui/` 发布，普通请求通过下述消息桥发给宿主。

所有消息必须带 `channel: 'magi-plugin-v1'`。页面先监听 `message`，等待宿主的 `init`，并验证 `event.source === parent`。后续只接受相同 `instanceId` 的消息。由于 iframe 是不透明 origin，向父窗口发送消息使用 `parent.postMessage(message, '*')`；安全绑定依靠窗口身份和宿主签发的实例 ID，不是由插件自报身份。

| 方向 | 消息 |
| --- | --- |
| 宿主 → 页面初始化 | `{channel, type:'init', instanceId, pluginId, scope:'application'}` |
| 页面 → 宿主请求 | `{channel, instanceId, type:'request', requestId, operation, ...参数}` |
| 宿主 → 页面成功 | `{channel, instanceId, type:'response', requestId, ok:true, result?}` |
| 宿主 → 页面失败 | `{channel, instanceId, type:'response', requestId, ok:false, error:string}` |
| 宿主 → 页面资源变化 | `{channel, instanceId, type:'event', resourceId, resource:{version,value}}` |
| 宿主 → 页面订阅失效 | `{channel, instanceId, type:'event', resourceId, error:'subscription_failed'}` |

`requestId` 由页面生成且在当前实例内唯一。页面应为请求设超时、清理等待项，并把失败显示给用户。卸载视图时宿主释放它的订阅；停用、撤权和升级也会使订阅失效，重新打开后重新读取并订阅。

| `operation` | 请求参数 | 结果 |
| --- | --- | --- |
| `resource.read` | `resourceId` | `{version,value}` |
| `resource.write` | `resourceId, expectedVersion, value` | 新 `{version,value}` |
| `resource.subscribe` | `resourceId` | 确认后推送事件；重复订阅不创建第二条订阅 |
| `settings.read` | 无 | 设置 `{version,value}` |
| `settings.write` | `expectedVersion, value` | 新设置 `{version,value}` |
| `context.select` | `resourceId, version, label, summary, selection` | 确认，把选中引用放进会话输入区 |

页面读写和订阅仍要求资源已声明且已获 `storage` 授权。写入快照最多 4MiB，建议使用更小数据；按版本处理读请求和订阅事件，丢弃迟到的旧快照。订阅不替代首次读取，也不意味着后台会持续运行。

`context.select` 的 `resourceId` 必须非空且最长 64，`version` 是非负安全整数，`label` 非空且最长 128；`summary` 控制在 1KiB 内，`selection` 为不超过 4KiB 的 JSON 选中信息，例如 `{index: 0}`。它只把摘要与选中信息交给输入区，不会自动发送消息或把整个资源交给模型；需要完整内容时为模型声明读取工具。不要在摘要中放入秘密或伪造宿主指令。

## 10. 进阶：工作流、会话引擎和分发合同

### 工作流核心

只有确实需要决定多步骤运行的下一动作时才使用工作流。将 `{id:"review",title:"检查流程",description:"返回检查结果"}` 加入 `contributions.workflows`，保持其他贡献不变，后台加入下面分支，重新打包升级并激活。工作流引用为 `plugin/<插件ID>/<贡献ID>`，例如 `plugin/example.note/review`。回到新会话，点击输入区 **＋（添加上下文与能力）**，在“工作流”分组选择“检查流程”，确认输入区显示所选流程后发送；也可从 `/` 菜单选择。此选择随本次提交生效，安装插件不会自动替换当前工作流。工作流决定下一步动作，Magi 执行模型/工具请求、授权、任务调度和持久化，插件不能直接提交任务终态。

当 `sdk.invocation.contribution.kind === 'workflow'` 时，输入字段使用 **snake_case**：

| 字段 | 内容 |
| --- | --- |
| `run_id, attempt_id, action_id` | 宿主签发的运行、本次尝试和当前动作身份 |
| `stage` | 当前阶段字符串 |
| `user_input` | 本次用户输入 |
| `model_result, tool_result` | 上一步结果或 `null`；模型结果为 `{content,tool_calls,status}`，工具结果是 `{status,output}` |
| `context_summary` | 宿主提供的 JSON 上下文摘要 |
| `config` | 上次持久化的 JSON 检查点，初次为运行配置 |

返回值只能为 `{checkpoint_version, stage, action}`，`checkpoint_version` 是正整数，`stage` 非空；`action.action_id` 必须原样回传本次 `input.action_id`，不能缓存或复用旧动作 ID。每次只提交一个动作，动作对象只包含以下对应字段：

| `action.kind` | 除 `kind, action_id` 外的字段 | 行为 |
| --- | --- | --- |
| `model_request` | `model: null, system: string, context_refs: string[], tools: string[]` | 使用宿主当前角色模型；引用必须属于当前 Task 的 `input_refs`，以引用文本加入请求，不自动读取资源正文。工具从宿主当前授权目录中选择子集，空列表表示本次不提供工具；空名称或不可用工具拒绝，不能覆盖模型配置 |
| `tool_call` | `tool: string, input: JSON` | 调用当前宿主工具目录中的工具，仍执行正常权限检查 |
| `dispatch_task` | `role: string, instruction: string, delegation: object` | 明确独立工作价值，经宿主原 `agent_spawn` 入口调度子任务 |
| `wait_user` | `question: string` | 等待用户回答 |
| `save_stage` | `stage: string, checkpoint: JSON` | 保存检查点；后续输入的 `config` 取此值 |
| `complete` | `summary: string` | 提交完成意图，由宿主校验并结算 |

`model_request` 只生成模型结果，返回的 `tool_calls` 不会自动执行；工作流须通过后续 `tool_call` 提交工具动作，再经过宿主权限和资源检查。发送前，原任务的待决动作保存请求正文、工具定义、引用和对应 Step 身份；模型失败或结果提交中断时保留证据，恢复不得自动重发。它沿用原生 Step/attempt 日志，不建立另一套模型或工具执行循环。

工作流同样受已选择和运行中激活的 Skill 限制。模型工具子集与后续 `tool_call` 读取同一当前 Skill；`skill_apply` 的限制先经过原持久化入口提交，再交付成功结果。保存失败时停止工作流，不能用初始 Task 绑定或普通工具目录继续调用。同名 Skill 被收紧、停用或重载时重新解析工具面，不使用仅按名称缓存的旧目录。

任务关闭原生工具时，工作流也不能提交 `tool_call`、`dispatch_task` 或携带非空 `tools` 的 `model_request`；宿主在记录新动作意图前拒绝。保存检查点、等待用户、纯文本模型请求及完成仍按原合同处理；关闭工具不妨碍读取已发生动作的原结果。

`dispatch_task.delegation` 遵循 [子代理决策合同](subagent-delegation-design.md)，`reason` 为 1–600 字符的实际理由。该动作派发后立即等待子任务，因此只接受 `user_requested / context_isolation / independent_review`；即使填写 `parent_work`，`parallel_work` 仍被拒绝。普通步骤直接使用 `model_request / tool_call`；工作流选择或角色存在本身不构成委派收益，也不授予代理权限。插件中的文字不等于用户明确要求代理。宿主不为旧插件缺失的理由补默认值；新动作必须提供有效决策，旧检查点中的已创建子任务继续按原事实结算。通过 `tool_call` 调用 `agent_spawn` 时同样需要完整决策，不能把工作流后续串行步骤写成同时推进的工作。

最小工作流处理分支如下；放在函数内部取得 `{kind,id}` 之后、最终 `throw` 之前。此例只结束运行，不调用模型；发送后应在对话中看到“检查已完成。”：

```js
if (kind === 'workflow' && id === 'review') {
  return {
    checkpoint_version: 1, stage: 'done',
    action: { kind: 'complete', action_id: input.action_id, summary: '检查已完成。' },
  };
}
```

工作流状态必须通过检查点和宿主结果推进，不能依赖全局变量。宿主在输入实际接纳为 Turn 时固定插件包摘要；排队输入在领取时才固定版本。升级不会把旧检查点交给新包，缺少包摘要的历史检查点也会明确拒绝执行，需要核对旧结果后重新发起。插件停用、升级或排空不影响相同请求读取原接纳回执，但不能据此启动新的执行。`complete.summary` 随原任务结果持久化并进入对话终态展示。失败、取消或未知副作用不能伪装成完成，也不能假定宿主会自动重放上一步。

### 会话引擎

在 `contributions.engines` 声明 `{id,title,description}`，并申请 `models` 权限，目标为该引擎贡献 ID。引擎引用同样是 `plugin/<插件ID>/<贡献ID>`，当前可以在设置 → 模型的“插件会话引擎”区域检查其只读注册信息；通用第三方引擎尚无完整的主模型选择器接入流程，不能把“注册成功”当作普通用户已能选用。以下是后台集成合同，不是可照做的完整桌面教程。它负责消息到结果的适配，不拥有会话、工具成功、审批或持久化状态。

`kind === 'engine'` 时，输入为 `{engine_id, session_id, thread_id, invocation_id, prompt, images, tool_context}`：ID 由宿主绑定；`prompt` 为文字，`images` 为 JSON 数组，`tool_context` 为 JSON 上下文。返回 `{events: [...]}`，数组中的事件格式为：

- `{kind:'delta', text:string}`：文字增量。
- `{kind:'completed', content:string, metadata:JSON}`：完整结果。
- `{kind:'failed', code:string, detail:string}`：明确失败。

通用适配器由可信宿主固定声明为文本输入：不支持图片、Magi 原生工具、运行中引导、检查点恢复或供应商输出上限；清单和插件输出不能自行提高能力。包含 `messages` 的调用会把完整 `{prompt,messages}` JSON 编码到 `prompt` 字符串，保留历史角色与内容；没有历史时仍传入原始文字。图片和工具请求在 Worker 启动前拒绝，不再截取最后一条消息或忽略图片。能力随模型 Step 冻结持久化，历史缺失声明时保持未知。

正常调用应以唯一 `completed` 收口，失败以 `failed` 收口；完成后继续发事件会被拒绝，`delta.text` 始终是新增片段。每次调用重新取得绑定包摘要和当前激活状态的运行租约，停用、撤权或升级后的旧适配器不能获得新的执行权。当前通用 Worker 在函数返回后才发布整个事件数组，不能靠返回数组实现实时流式回调；也没有调用远端模型的网络 SDK。需要集成测试时，可用以下本地处理分支验证输入输出合同；同时声明 ID 为 `echo` 的引擎贡献和对应 `models` 权限。它只回显输入，不提供真实模型推理：

```js
if (kind === 'engine' && id === 'echo') {
  return { events: [{ kind: 'completed', content: input.prompt, metadata: {} }] };
}
```

### 当前不开放的通用扩展

- **原生浏览器**：`browserSurfaces` 是声明，不提供第三方页面驱动或任意 URL 挂载 API。字段 `scope` 允许 `application/workspace`、`persistence` 允许 `persistent/ephemeral`、`closeAction` 允许 `hide/destroy`，其余两个字段为布尔值；当前宿主只接受 `scope:'application', persistence:'persistent', keepMounted:true, closeAction:'hide'`，后台推理为真时还必须保持挂载。该组合也不代表任意第三方 surface 都已有挂载实现。
- **第一方宿主能力**：GPT Web 与 Magi MCP 服务由内置插件壳启用，内部站点、浏览器和通道实现不属于通用 SDK。`openai.chatgpt-web`、`magi.mcp-server` 是保留 ID，外部 ZIP 不能使用。
- **其他扩展**：当前没有通用 Skill/MCP 服务清单贡献、生命周期脚本钩子、插件间依赖、插件自带外部程序或后台常驻任务接口。不要把其他产品的插件清单直接套用到 Magi。

### 插件中心目录与可选签名

目录根对象为 `{format:1, plugins:[条目...]}`，可选 `issuedAt` 为毫秒时间戳；每个条目必须含 `id/name/description/version/url/sha256/size`。`url` 可为 HTTP(S) 地址或相对目录自身的路径；`sha256` 是 ZIP 的小写十六进制 SHA-256，`size` 是 ZIP 字节数。最多 512 个条目，目录最多 1MiB；同一 ID 与版本不能重复。下载包的身份、版本、摘要和大小应与条目一致。

需要签名时，目录改为 `{payload, keyId, signature}`：`payload` 是目录 JSON **原始 UTF-8 字节**的标准 Base64，`signature` 是 Ed25519 对这些原始字节的 64 字节签名的 Base64，`keyId` 为发布者自定的密钥标识。用户在高级设置中填写相同 `keyId` 和 Base64 编码的 32 字节 Ed25519 原始公钥；配置后只接受匹配公钥签名的目录。不要签名 Base64 字符串或重新序列化后的 JSON；私钥保留在发布者机器，不打进插件包。签名不替代安装授权。

macOS/Linux 的插件 Worker 有操作系统级限制；Windows 当前只有 JS 接口限制与宿主资源限制，没有 Worker 操作系统级沙箱。发布说明不能把三平台的隔离机制描述成完全相同。
