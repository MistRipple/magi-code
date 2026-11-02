# Magi 插件作者指南

> **状态说明（2026-11-02）：** 本指南描述 SDK 2。SDK 1（QuickJS 后台，`backend` / `plugin.mjs`）已移除：安装这类包会被明确拒绝，不提供兼容执行或自动转换。SDK 2 目前提供三类内容：声明式贡献（技能、视图、资源、浏览器 surface）、受信 Node 扩展（对会话、Turn、模型与工具事件挂钩子）和扩展提供的模型工具。用户命令也由 Node 扩展提供。工作流和会话引擎不提供；数据格式迁移也没有，改变 `dataSchemaVersion` 且已有数据时升级会清空该插件的数据。

这是一份可以单独保存、复制给其他开发者或交给 AI 使用的 Magi 插件开发手册。你需要了解基本 JavaScript 和 JSON；增加页面时需要基本 HTML。

## 1. 先确认你的功能适合哪种形态

| 你想做什么 | 形态 | 说明 |
| --- | --- | --- |
| 给模型增加一段方法说明（Skill） | 声明式 | `contributions.skills`，无需脚本 |
| 增加设置表单、主页面、右侧面板 | 声明式 | `contributions.views` 与 `settingsSchema`，页面在隔离 iframe 中运行 |
| 让页面和宿主共享一份带版本的数据 | 声明式 | `contributions.resources` 与 `storage` 权限 |
| 观察或拦截会话事件：工具调用前后、用户输入、子代理派发、模型请求与响应、用量 | Node 扩展 | `extension.hooks`，需要“完全信任”授权 |
| 为模型提供新的工具 | Node 扩展 | `contributions.tools` 声明、`api.tool` 实现，第 5 节 |
| 为用户提供斜杠命令 | Node 扩展 | `contributions.commands` 声明、`api.command` 实现，第 6 节 |
| 工作流、会话引擎 | 不提供 | 已随 QuickJS 移除，不会重建 |

插件的安装、授权、启用和数据都对所有工作区共用一份。

## 2. 包结构与清单

发布包是 ZIP，根目录固定含 `manifest.json`；声明了 `extension` 的包还须含 `extension/index.mjs`；页面放在 `ui/` 下。包内不得有可执行文件、符号链接，也不得含 `plugin.mjs`。

```json
{
  "sdkVersion": 2,
  "id": "acme.audit",
  "version": "1.0.0",
  "name": "审计",
  "description": "记录每次工具调用",
  "permissions": [
    { "kind": "extension", "scope": "application", "targets": ["full-trust"] }
  ],
  "dataSchemaVersion": 1,
  "settingsSchema": { "type": "object", "additionalProperties": false },
  "extension": {
    "hooks": [{ "name": "tool.call", "mode": "observe" }]
  },
  "contributions": {}
}
```

- `sdkVersion` 必须是 `2`。
- `id` 含作者命名空间；`version` 为语义化版本。
- 清单不接受未知字段。贡献 ID 在所有贡献类型间共用一个命名空间。

## 3. 声明式贡献

```json
"contributions": {
  "skills": [{ "id": "review", "title": "评审", "description": "", "instruction": "……" }],
  "resources": [{ "id": "notes", "title": "笔记", "description": "" }],
  "views": [{ "id": "board", "title": "看板", "entry": "ui/index.html", "placements": ["rightPane", "main"] }]
}
```

- 资源读写需要对应 `storage` 权限，目标是资源 ID；资源带版本，写入必须携带期望版本。
- 视图页面在 `sandbox="allow-scripts"` 的隔离 iframe 中，通过宿主消息桥读写资源与设置，不能访问宿主页面。

## 4. Node 扩展

入口文件 `extension/index.mjs` 默认导出 `activate(api)`，用 `api.on(hook, handler)` 绑定钩子。清单里的 `extension.hooks` 必须与代码绑定一致；每个钩子声明模式（`observe` / `advise` / `guard`）和可选的 `deadlineMs`。

```js
export default async function activate(api) {
  await api.on('tool.call', async (input, ctx) => {
    // ctx.signal 在调用被取消或超时时中止
    // 返回 { decision: 'continue' | 'block' | 'replace', reason?, patch? }；不返回等同 continue
    return undefined;
  });
}
```

要点：

- `guard` 钩子失败或超时时关闭（拒绝该操作）；`advise` 与 `observe` 失败只影响自己，宿主继续。
- 重复崩溃的扩展会被熔断并停止自动重启；运行状态显示在设置页的插件详情里。
- 扩展只能阻止（`block`）或在允许的钩子上替换（`replace`），不能批准宿主未批准的操作。钩子目录、各钩子允许的模式、输入输出形状与时限见《扩展协议 v1》。清单声明了尚未接线的钩子或模式时，安装会被拒绝，而不是静默忽略。
- 改写已装配历史的钩子（`context.assemble`、`compaction.before`）必须同时声明权限 `contextRewrite: ["history"]`；它们会使提示词缓存前缀失效。

### 信任模型

扩展以用户权限运行在宿主随包提供的 Node 里，没有操作系统级沙箱：它能读写用户有权访问的一切。因此必须声明权限 `extension: ["full-trust"]`，用户在授权时明确确认。会话、Turn、任务与审批的事实仍只归 Magi 内核所有，扩展只能通过钩子的返回值影响它们。

## 5. 模型工具

声明工具需要三处一致：`contributions.tools` 条目、覆盖该工具 ID 的 `tools` 权限、`extension/index.mjs` 里的 `api.tool` 绑定。包必须同时声明 `extension`（只提供工具时 `hooks` 可为空数组）。

```json
"permissions": [
  { "kind": "extension", "scope": "application", "targets": ["full-trust"] },
  { "kind": "tools", "scope": "application", "targets": ["word-count"] }
],
"extension": { "hooks": [] },
"contributions": { "tools": [{
  "id": "word-count", "title": "字数统计", "description": "统计文本的词数",
  "readOnly": true,
  "inputSchema": { "type": "object", "required": ["text"], "properties": { "text": { "type": "string" } }, "additionalProperties": false }
}] }
```

```js
export default async function activate(api) {
  await api.tool('word-count', async (input, ctx) => ({ words: input.text.split(/\s+/).filter(Boolean).length }));
}
```

- 模型看到的工具名由宿主生成（`plugin__…`），不要依赖它。参数先按 `inputSchema` 校验，不合法的调用不会到达你的代码。
- `readOnly` 决定审批：只读工具在只读模式下可用；非只读工具在“逐次询问”下需要用户批准，在“只读”下被拒绝。
- 处理器返回的值（任意 JSON）作为工具输出；抛出的错误会作为工具失败返回给模型，不是协议错误。单次调用上限 60 秒；超时或用户停止所属 Turn/任务时 `ctx.signal` 中止，请尽快返回。停止时若工具不是 `readOnly`，宿主无法确认写入是否已生效，结果记为“未确定”，不会自动重试。
- 工具通过用户授权的 `tools` 权限才对模型可见；停用、撤权或宿主不可用时目录中不再出现或调用失败。

## 6. 用户命令

用户在输入框用 `/` 选择命令，可附带一段文字；命令在独立的一轮里执行，不调用主模型，返回的文本作为这一轮的回复写回会话。

```json
"extension": { "hooks": [] },
"contributions": { "commands": [{ "id": "stats", "title": "统计", "description": "统计这段文字" }] }
```

```js
export default async function activate(api) {
  await api.command('stats', async (input, ctx) => `共 ${input.text.length} 个字符`);
}
```

- 处理器返回非空字符串或 `{ content }`；抛错或返回空文本会让这一轮失败并显示原因。上限 120 秒，停止该轮时 `ctx.signal` 中止。
- 命令不能与技能、目标模式、图片或上下文引用同时提交；授权由 `extension` 的完全信任覆盖，不需要额外权限。

## 7. 打包与分发

把插件目录打成 ZIP（根目录即包根，不要多套一层文件夹）。仓库提供 `scripts/pack-plugin.mjs`，输出可重现的 ZIP：

```text
node scripts/pack-plugin.mjs <插件目录> <输出.zip>
```

用户在设置 → 插件中安装本地包，安装前会预览权限与版本差异，并把确认绑定到包摘要。官方插件中心与自建目录使用签名目录（序号、过期时间、吊销列表），详见插件中心说明。

## 8. 升级

升级会保留各作用域的启用意愿，但清空授权与激活，用户须对新包重新授权。`dataSchemaVersion` 变化且已有持久数据时，升级会清空该插件的资源数据与设置：没有数据转换通道，升级前的预览会明确提示。
