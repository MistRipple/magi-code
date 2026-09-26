# GitHub 新版本发布流程

本文是 Magi 新版本发布的完整门禁。除非任务明确涉及发布或版本 Tag，日常开发无需执行这套完整流程。

## 发布边界

- 发布仓库固定为 `MistRipple/magi-code`。
- 正式发行物只有 Electron；不再发布 Tauri、CEF 或 Browser Runtime 独立发行物。
- Electron 安装包不做 Apple/Windows 代码签名和公证。旧版 Tauri 客户端无感迁移桥仍需生成并保留旧更新器使用的 `.sig`。
- 仓库提交使用 GitHub 账户 `MistRipple <east_xiaodong@126.com>`，不得使用 `Codex <codex@openai.com>`。

## 创建或移动版本 Tag 前

1. 确认产品版本、发布说明和目标 Tag 一致；发布说明放在 `.github/releases/vX.Y.Z.md`。
2. 在本地执行完整预检：

   ```bash
   npm run release:preflight:full -- --tag vX.Y.Z
   ```

   此命令执行仓库完整检查、测试、构建、发行边界校验、Rust 检查与测试及依赖审计，并使用 `--package` 构建当前操作系统的 Electron 安装包。跨平台安装包由 GitHub 发布工作流构建。

3. 预检通过后，提交并推送发布提交到 `main`。
4. 等待同一提交的 GitHub CI 全部通过。
5. 将版本 Tag 创建或移动到该 CI 已通过的 `main` 提交，以触发发布工作流。

## 发布完成后

确认以下内容均已生成并可访问：

- GitHub Latest Release 和跨平台 Electron 安装包；
- Electron 更新元数据；
- `magi-desktop-stable/latest.json`；
- 旧版 Tauri 迁移包、旧更新器所需的 `.sig`，以及 8 个平台迁移入口。

若任一门禁或交付物未通过，不要将发布标记为完成；先定位并修复，再按发布工作流恢复。
