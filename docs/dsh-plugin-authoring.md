# Magi 插件作者指南

Magi 使用 DeepSeek Harness 的 DSH 插件合约。用标准 npm 包发布插件，并通过 `package.json` 的 `exports` 声明后端入口和可选的 `./client` 入口。Magi 设置页支持从上游插件列表安装发布版本，也支持上传 `.tgz` 原包。

## 后端与依赖

后端通过 Cordis 暴露入口并声明服务依赖。Magi 将插件装入共享宿主，从同一个运行时提供 DSH SDK 与公共服务。必需服务缺失时，插件详情会显示缺失项，相关实例等待依赖。只在确实允许相关业务缺席时将依赖声明为可选。

通过 `engines.dsh` 声明支持的 DSH 版本。未声明该字段时，Magi 按 DSH 规则核对 SDK 的 `peerDependencies`。启用前核对目标 Magi 版本的服务覆盖情况，并用发布包验证实际业务。

## 界面

为插件设置页和独立业务页注册 DSH 客户端槽位。按目标 Magi 版本核对右栏能力。主对话的输入框、消息和审批由 Magi 承载。插件详情会显示运行时识别到的未承载槽位。

## 生命周期

在 Cordis 生命周期内注册服务、工具和观察订阅，并随实例回收。验证依赖缺失、停用与重新启用、应用重启、取消和在线更新时的业务表现。

旧 Magi SDK 2 插件需重新安装对应的 DSH 原包。安装和升级时，在 Magi 设置页确认实际来源与版本。

## 合约参考

- [DeepSeek Harness 开发仓库](https://github.com/deepseek-ai/deepseek-harness)
- [Magi 的 DSH 插件设计与阶段范围](https://github.com/MistRipple/magi-code/blob/main/docs/dsh-seam-alignment.md)
