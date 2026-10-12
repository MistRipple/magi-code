# Magi 官方插件中心

这个文件夹就是 Magi 的官方插件中心，经 GitHub 提供。Magi 默认从
`https://raw.githubusercontent.com/MistRipple/magi-code/main/plugin-center/catalog.json`
读取目录，用户不需要任何配置就能在「设置 → 插件 → 插件中心」看到并安装这里的插件。

- `catalog.json`：目录，条目的包地址是相对路径（`packages/…`），按目录地址解析。
- `packages/`：发布包（ZIP）。
- `plugins.json`：哪些插件目录会被打包进来。

## 上架一个插件

1. 把插件目录放进仓库（例如 `examples/plugins/<名字>`），确保 `manifest.json` 的 `version` 已更新。
2. 在 `plugins.json` 里加一行 `{"dir": "<插件目录>"}`。
3. 运行 `npm run plugin-center:build` 重新生成 `packages/` 与 `catalog.json`，一并提交。
4. 提交 PR。代码审查就是上架审核；CI 会运行 `npm run plugin-center:check`，保证提交的包与源码一致。

来自官方地址的目录按插件中心来源对待；安装后插件不会自动激活，权限仍要用户逐项授权。
插件开发规范见 [docs/plugin-authoring.md](../docs/plugin-authoring.md)。

> 当前目录为空：官方还没有上架第三方插件。`examples/plugins/board` 是给插件作者参考的示例源码（开发规范与自动化验收使用），不会出现在官方插件中心里。
