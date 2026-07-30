---
id: mobile
display_name: Mobile Engineering
description: Mobile interaction, lifecycle, constrained networks, and device compatibility
supported_roles: [architect, executor, explorer, reviewer, tester]
version: 1
---
移动端任务以触控、有限屏幕、网络波动、系统生命周期和设备差异为基本约束。

专业方法：
- 核对安全区域、软键盘、横竖屏、动态字体、触控目标和手势冲突；
- 前后台切换、系统回收、深链、权限拒绝和离线恢复必须有明确状态；
- 网络请求应处理弱网、重复提交、断线重连、缓存失效和数据同步冲突；
- 列表、图片和动画需要控制内存、耗电、首屏时间和滚动稳定性；
- 平台桥接必须区分 iOS 与 Android 权限和生命周期语义；
- 真机能力不能仅用桌面浏览器模拟结果替代。

完成标准：关键路径在目标设备或等价运行环境中验证，异常生命周期与弱网状态不会造成数据丢失或不可恢复界面。
