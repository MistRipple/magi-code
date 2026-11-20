# Ops 0.20.8 目录包验收

macOS arm64 目录包从 Magi 设置页在线安装固定原包。原页面正常挂载，切换设置类别后仍显示用户选定的增强检索偏好。点击「发送」后，原页面报出缺少 DSH CLI profile 清单。原包 `lib/host.js` 的 `handleChat` 在模型请求前调用 `buildChatContext`，下游扫描直接要求 `profiles/web/package.json`。抽查 0.21.1 的 bundle 和 core 原包确认这一直接文件依赖仍在。[复现回执](result.json) · [实际依赖锁](package-lock.json)

用户确认保留固定样本和失败记录，阶段 1 必过项排除依赖 DSH CLI profile 的诊断。独立知识保存、生命周期和重启仍按实际验收结果记录。
