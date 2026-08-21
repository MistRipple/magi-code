# Magi Rust Rewrite Agent Guide

本文件是给后续 coding agent 使用的项目级操作说明。进入本仓工作前，先阅读并遵守这里的启动方式与边界约定。

## 基本工程约定

- 默认使用中文沟通、中文说明，并遵循 [$cn-engineering-standard](/Users/xie/.codex/skills/cn-engineering-standard/SKILL.md)。
- 不要回退、覆盖或清理他人已有改动；当前仓经常存在多个 agent 并行修改。
- 修改前先确认当前职责边界，优先做最小充分修改，不保留双实现、临时兜底或绕行路径。
- 涉及浏览器真实验收时，优先访问 daemon 托管入口，而不是直接访问 Vite 入口。

## 推荐开发启动方式

开发调试时只启动 daemon，让 daemon 负责拉起或复用前端热加载服务：

```bash
./scripts/dev-daemon.sh
```

该脚本会在启动前检查 `target` 目录大小，默认超过 `8GiB` 时执行 `cargo clean`，
给后续编译增长预留空间，尽量避免开发缓存超过 `10GiB`。如需调整阈值：

```bash
MAGI_TARGET_PRUNE_GIB=6 ./scripts/dev-daemon.sh
```

大批量测试或多轮编译后，也可以只执行缓存清理检查：

```bash
./scripts/prune-target.sh
```

默认访问地址：

```text
http://127.0.0.1:38123/web.html
```

这条路径是当前项目的主开发入口。它的职责分工是：

- daemon 监听 `MAGI_HOST` / `MAGI_PORT`，默认 `0.0.0.0:38123`；本机浏览器仍访问 `127.0.0.1:38123`。
- daemon 在 `MAGI_WEB_DEV=1` 时自动检查 Vite dev server。
- 如果 Vite 已在固定端口就绪，daemon 会复用它。
- 如果 Vite 未启动，daemon 会执行 `npm --prefix web run dev:daemon` 自动拉起。
- 浏览器仍访问 daemon 的 `/web.html`，页面中的前端开发模块从 daemon 同源加载，并由 daemon 代理到固定端口 Vite。
- API、SSE、设置、会话、任务、变更等请求仍回到当前 daemon，不走另一个前端服务。

## 前端热加载端口

默认 Vite 热加载监听地址是：

```text
0.0.0.0:3000
```

如需修改端口，只改 daemon 的开发环境变量：

```bash
MAGI_WEB_DEV_PORT=3000 ./scripts/dev-daemon.sh
```

可用变量：

- `MAGI_WEB_DEV=1`：启用 daemon 托管的前端热加载模式。
- `MAGI_WEB_DEV_HOST`：Vite dev server host，默认 `0.0.0.0`。
- `MAGI_WEB_DEV_PORT`：Vite dev server port，默认 `3000`。
- `MAGI_WEB_DEV_ROOT`：前端目录，默认仓库根目录下的 `web`。
- `MAGI_HOST`：daemon host，默认 `0.0.0.0`。
- `MAGI_PORT`：daemon port，默认 `38123`。
- `MAGI_STATE_ROOT`：daemon 状态目录，默认 `~/.magi`。

## 不要这样启动

除非你只是在孤立调试 Vite 配置，否则不要单独启动：

```bash
npm --prefix web run dev
```

也不要为了绕过端口占用让 Vite 自动漂移到 `3001`、`3002` 等端口。`web/vite.web.config.ts` 已启用 `strictPort`，端口被占用时应该复用已有 daemon/Vite，或明确停止旧进程后再启动。

## 静态构建模式

不设置 `MAGI_WEB_DEV` 时，daemon 使用静态构建产物：

```bash
npm --prefix web run build
cargo run -p magi-daemon-app
```

静态模式会从 `web/dist/web.html` 和 `web/dist/assets` 提供前端资源。该模式适合生产近似验证；日常开发和浏览器验收优先使用 `MAGI_WEB_DEV=1`。

## 验证建议

改动前后至少按需执行：

```bash
npm --prefix web run check
cargo check -p magi-daemon
```

准备 GitHub Desktop 发布时，先在本地执行完整发布前置校验，减少推送后才发现 CI 失败的往返：

```bash
npm run release:preflight:full -- --tag vX.Y.Z
```

其中 `--package` 会构建当前操作系统的 Electron 发行包，跨平台安装包和 GitHub Release 仍由发布工作流负责。

### GitHub 新版本发布规则

- 发布仓库固定为 `MistRipple/magi-code`，发布提交必须先进入 `main`，并等待同一提交的 GitHub CI 全部通过。
- 创建或移动版本 Tag 前，必须在本地执行完整预检：`npm run release:preflight:full -- --tag vX.Y.Z`。预检通过后才提交、推送 `main`，再创建版本 Tag 触发发布工作流。
- 正式发行边界固定为 Electron；不再发布 Tauri、CEF 或 Browser Runtime 独立发行物。
- Electron 安装包保持无 Apple/Windows 代码签名和公证；仅为旧版 Tauri 客户端的无感迁移桥生成并保留旧更新器所需的 `.sig`。
- 发布后必须确认 GitHub Latest Release、Electron 更新元数据、`magi-desktop-stable/latest.json`、旧版迁移包及其 8 个平台入口均已生成并可访问，确保旧版客户端可以无感迁移到 Electron。
- 所有仓库提交使用 GitHub 账户 `MistRipple <east_xiaodong@126.com>`，不得使用 `Codex <codex@openai.com>`。

涉及 daemon 托管前端时，建议额外确认：

```bash
curl -I http://127.0.0.1:38123/web.html
curl http://127.0.0.1:38123/health
```

浏览器验收请打开 `http://127.0.0.1:38123/web.html`，不要把 `http://127.0.0.1:3000/web.html` 当作主验收入口。
