# Magi 桌面图标

正式图标使用已确认的浅色圆角底座与陶橙色扁平 M。设计母版为 source/app.png；source/tray.png 是对应的透明底黑色 M，供 macOS 模板图标使用。两份母版由内置 imagegen 生成，完整提示词保存在同目录。

在 macOS 仓库根目录重新导出：

    node apps/desktop/scripts/generate-icons.mjs

脚本使用系统 sips/iconutil，只转换尺寸与容器格式，不调用图像生成服务。导出物需一并提交，其他系统打包无需运行该脚本。

- 本目录的各尺寸 PNG：Linux、窗口、Dock 和非 macOS 托盘图标。
- icon.icns：macOS 安装包图标，包含标准及 Retina 表示。
- icon.ico：Windows 安装包图标，包含 16–256 像素的多尺寸表示。
- ../resources/tray/magiTemplate.png：18 × 18、72 dpi。
- ../resources/tray/magiTemplate@2x.png：36 × 36、144 dpi。

Electron Main 在开发与打包模式下均选择独立的 macOS 模板图标，显式标记 setTemplateImage(true)，保留 Retina 表示，不在运行时缩放彩色桌面图标。系统根据菜单栏外观渲染黑白颜色。既有常驻、打开窗口和退出菜单行为保持原有生命周期。

Electron Builder 将模板目录按原文件名复制到 resources/tray；after-pack 会检查 macOS 包同时包含 1x 和 2x 模板。参考：https://www.electronjs.org/docs/latest/api/tray

本目录源文件是后续导出的依据；design/ 中的探索稿不参与打包。修改图片后需重新导出，并运行 Desktop 检查和相关原生图像加载验收。
