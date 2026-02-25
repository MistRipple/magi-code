# Magi 社媒发布模板（抖音 / 小红书）

[![Version](https://img.shields.io/badge/version-0.1.3-blue?style=flat-square)]()
[![License](https://img.shields.io/badge/license-GPL--3.0--or--later-red?style=flat-square)](LICENSE)

适用项目：Magi（VSCode 多智能体编排扩展）

---

## 抖音模板

### 标题备选
- 我做了个 VSCode 插件，让 3 个 AI 一起写代码
- 单 AI 太容易跑偏了，我改成了多 Agent 协作
- Claude + GPT + Gemini，终于能一起干活了

### 口播脚本（40-60 秒）
最近在做一个 VSCode 插件，叫 Magi。

我自己平时用 AI 编程，最大的痛点是：
对话一长容易跑偏，很多任务只能串行排队，改炸了还难回滚。

所以我做了一个多智能体协作方案：
把复杂需求拆成多个子任务，交给不同 Agent 并行处理。
比如一个做架构，一个写实现，一个专门排查问题和补测试。

Magi 支持 Claude、GPT、Gemini 混合编排，
还有任务级文件快照，方向错了可以快速回退。

目前是 v0.1.3，仍在持续迭代。
补充一个已知情况：当前 Codex 接入条件下还存在部分工具循环调用问题，
建议先用其他模型体验，编排侧推荐 Claude。

如果你在使用中遇到问题，欢迎在帖子评论区或 GitHub Issues 反馈，
我会在年后逐一处理。

### 画面建议
1. 首页界面：`image/home.png`
2. 编排流程：`image/orchestrator-1.png`
3. Worker 配置：`image/portrait.png`
4. 工具配置：`image/setting-tool.png`
5. 设置面板：`image/setting-board.png`

### 发布文案
最近在做一个 VSCode 多智能体编排插件 Magi。
核心思路是把复杂任务拆开并行执行，而不是和单个 AI 一问一答。

当前版本 v0.1.3。
已知问题：Codex 接入条件下存在部分工具循环调用，建议先用其他模型体验，编排推荐 Claude。

有问题欢迎评论区或提 Issues，年后会逐一跟进。

GitHub 仓库：https://github.com/MistRipple/magi-docs

---

## 小红书模板

### 标题备选
- 我把 AI 编程从单聊改成了多人协作
- 做了个 VSCode 插件：让 Claude/GPT/Gemini 分工写代码
- AI 编程踩坑后，我做了这个多 Agent 工作流

### 正文模板
最近在做一个 VSCode 插件，叫 Magi。
它不是让你和一个 AI 连续对话，而是把一个复杂需求拆成多个子任务，让不同 Agent 协同和并行处理。

我做它主要是因为三个痛点：
1）对话长了容易上下文漂移；
2）不同模型有不同强项，但很多工具只能单模型；
3）写代码、改 Bug、跑测试经常只能线性排队。

Magi 的做法是：
- 先判断复杂度，简单任务走轻路径；
- 复杂任务自动拆分给不同 Worker；
- 支持 Claude / GPT / Gemini 混合编排；
- 无依赖子任务并行执行；
- 每个节点做文件快照，出问题可回滚。

当前版本是 v0.1.3，还在持续迭代。
补充一个已知问题：目前 Codex 接入条件下存在部分工具循环调用，建议先配置为其他模型体验，编排侧推荐 Claude。

如果你在使用过程中遇到问题，欢迎在帖子里留言，或者到 GitHub Issues 记录，我会在年后逐一处理。

GitHub 仓库：https://github.com/MistRipple/magi-docs

### 配图顺序
1. `image/home.png`
2. `image/orchestrator-1.png`
3. `image/portrait.png`
4. `image/setting-tool.png`
5. `image/setting-board.png`

### 话题建议
#程序员日常 #AI编程 #VSCode插件 #独立开发 #效率工具 #技术分享

