<div align="center">

[English](README_EN.md) | 中文文档

# 🔮 Magi

**新一代多智能体工程编排系统**

*意图洞察 · 深度拆解 · 异构协作*

[![VSCode Extension](https://img.shields.io/badge/VSCode-Extension-007ACC?style=flat-square&logo=visualstudiocode&logoColor=white)](https://code.visualstudio.com/)
[![Version](https://img.shields.io/badge/version-1.X-blue?style=flat-square)]()
[![License](https://img.shields.io/badge/license-GPL--3.0--or--later-red?style=flat-square)](LICENSE)

<br/>

![Magi 编排界面](image/home.png)

<br/>

**Magi 不是另一个 ChatBot，而是你的 AI 研发团队。**

它运行在 VSCode 中，将复杂工程任务转化为可执行的任务合同，调度多个异构 Worker 并行协作，
从意图理解、任务拆解、执行修复到验收沉淀形成完整闭环。

[核心特性](#核心特性) • [工作原理](#工作原理) • [快速开始](#快速开始) • [联系](#联系)

[English](README_EN.md)

</div>

---

## ![](https://img.shields.io/badge/WHY-Magi-007ACC?style=for-the-badge)

现有 AI 编程助手大多是单体智能模式，在跨模块开发和长链路排障中容易出现稳定性与一致性问题。Magi 的核心目标是把“对话能力”升级为“工程交付能力”。

| ![](https://img.shields.io/badge/PAIN_POINTS-痛点-red?style=flat-square) | ![](https://img.shields.io/badge/LIMITATIONS-局限-gray?style=flat-square) | ![](https://img.shields.io/badge/SOLUTIONS-Magi_方案-green?style=flat-square) |
|:---|:---|:---|
| **任务迷航** | 长对话后目标漂移、上下文衰减，任务完成度不稳定。 | **Mission 合同驱动**：目标、验收、上下文三元组约束执行路径。 |
| **并发冲突** | 多模型并发写文件易相互覆盖，修改不可控。 | **零冲突引擎**：File Mutex + 实时强读 + 意图驱动编辑。 |
| **故障反复** | 失败后重复同路径重试，缺少根因沉淀。 | **闭环自修复**：动态 Todo + 多轮验证 + 补充指令注入。 |
| **知识断层** | 对话结束即“失忆”，经验无法持续复用。 | **本地知识沉淀**：Rolling Summary + WisdomExtractor + PKB。 |
| **治理失衡** | 所有任务用同一执行强度，效率与安全难兼得。 | **双模式治理**：常规模式提速，深度模式强约束保安全。 |

### 典型使用场景

- **跨模块功能开发**：前后端/配置/脚本联动变更，自动拆分并并行推进。
- **复杂 Bug 排障**：支持“定位-修复-复验”闭环，减少反复重试。
- **大规模重构**：深度模式下强制委派 + 快照回溯，降低改造风险。

### 使用边界与预期

- **深度模式更稳但更慢**：适合高复杂任务，不追求秒级响应。
- **结果质量依赖任务描述**：Goal 与 Acceptance 越清晰，交付越稳定。
- **外部能力依赖本地配置**：CLI/MCP 未正确配置会影响对应能力可用性。

### 隐私与数据说明

- **本地优先**：代码检索、任务状态、知识沉淀在本地工作区完成。
- **模型调用可控**：仅使用你配置的模型通道（OpenAI/Anthropic/Gemini 等）执行推理。

---

## ![](https://img.shields.io/badge/VISION-定位-6B4E71?style=for-the-badge)

> **Magi 的核心理念：** 用正确的模型，做正确的事。

你只需描述目标，Magi 自动完成：

> [!TIP]
> **意图理解 ➔ 复杂度评估 ➔ 任务规划 ➔ 专家分派 ➔ 并行执行 ➔ 验收闭环 ➔ 经验沉淀**

![编排流程](image/orchestrator-2.png)
![运行效果](image/orchestrator-1.png)

你不再是提示词工程师，而是 AI 团队的技术负责人。

---

## ![](https://img.shields.io/badge/FEATURES-特性-31A8FF?style=for-the-badge)

### 核心特性

### 1. 任务驱动编排（Mission-Driven）
- 以 Goal / Acceptance / Context 组织任务，不再依赖发散式自由对话。
- 支持 Assignment 级责任划分，Worker 结果可追踪、可验收。
- 执行中允许动态追加 Todo，面向真实工程问题逐步收敛。
![任务系统](image/todos.png)
### 2. 动态双模式治理（Standard / Deep）
- **常规模式**：低延迟交付，适合轻量修改与日常开发。
- **深度模式**：编排者禁改代码，强制委派 Worker，适配跨模块与系统级改造。
- 同一套引擎下按任务复杂度切换治理强度，平衡速度与稳定性。

### 3. 零冲突并发编辑引擎
- **File-level Mutex**：同文件写入串行化，防并发覆盖。
- **实时强读**：写入前读取磁盘最新状态，降低上下文陈旧风险。
- **意图驱动编辑**：模型表达“改什么”，底层负责稳定落盘。
- **上下文新鲜度治理**：执行中自动处理文件上下文陈旧，减少无效重试。

### 4. 会话隔离与恢复能力
- `UnifiedSessionManager` 按 trace/session 物理隔离状态，避免串会话污染。
- 快照机制覆盖关键写操作，支持故障后恢复与回溯。
- 补充指令队列支持执行中追加需求，在下一决策点生效，不打断主流程。

### 5. 本地知识提取与沉淀
- `MemoryDocument` 负责滚动摘要，压缩噪声并保留关键上下文。
- `WisdomExtractor` 从 Worker 报告提取 learnings / decisions / warnings。
- 结果沉淀到项目知识库（PKB），提升后续同类任务命中率。
![知识库](image/knowledge.png)
### 6. 可扩展工具链
- 内置文件读写、代码检索、任务调度、进程执行等工程工具。
- 原生支持 MCP（Model Context Protocol）接入外部能力。
- 支持 Skills 动态加载，按场景扩展专业能力。

![工具配置](image/setting-tool.png)

---

## ![](https://img.shields.io/badge/HOW-工作原理-E85D04?style=for-the-badge)

### 工作原理

### 三级任务模型
- **Mission**：面向用户目标的总体任务。
- **Assignment**：分派给具体 Worker 的职责单元。
- **Todo**：执行中的可演化步骤（可新增、可重排、可闭环修复）。

### 执行闭环
- **Phase A 规划**：意图分析、复杂度评估、任务拆解与上下文注入。
- **Phase B 执行**：Worker 并行推进，依赖任务按拓扑顺序接力。
- **Phase C 验收**：对照验收标准复核，未达标自动进入修复循环。

### 协作保障
- **ContractManager**：管理接口契约与协作边界。
- **FileMutex**：保障并发写安全。
- **SnapshotManager**：关键步骤可回溯。
- **TaskViewService**：任务态可视化，便于追踪执行链路。

---

## ![](https://img.shields.io/badge/QUICKSTART-快速开始-28A745?style=for-the-badge)

### 快速开始

### 环境要求

- **VSCode**：`>= 1.93.0`
- **Node.js**：建议 `>= 18`
- **可用 CLI**：至少配置一个（Claude / Codex / Gemini）
- **网络**：如需联网检索或外部模型调用，请确保网络可用

### 1. 安装扩展
在 VSCode 扩展市场搜索 **Magi** 并安装，或通过 `.vsix` 本地安装。

### 2. 配置模型 CLI
在 VSCode 设置中搜索 `magi`，配置编排与 Worker 使用的 CLI 路径：
- `magi.claude.path`
- `magi.codex.path`
- `magi.gemini.path`

### 3. 选择治理模式
- `magi.deepTask = false`：常规模式（默认）
- `magi.deepTask = true`：深度模式（项目级治理）

### 4. 开始协作
- 打开面板：`Ctrl+Shift+A`（Mac: `Cmd+Shift+A`）
- 启动任务：`Ctrl+Shift+Enter`（Mac: `Cmd+Shift+Enter`）
- 新建会话：`Ctrl+Alt+N`（Mac: `Cmd+Alt+N`）
- 停止任务：`Ctrl+Shift+Backspace`（Mac: `Cmd+Shift+Backspace`）

### 5. 最小可运行验证（1 分钟）

- 在新会话输入：`请读取 README.md 并总结当前版本号`。
- 观察是否成功触发任务、出现执行状态与最终回复。
- 若失败，优先检查 CLI 路径与 API Key 配置。

![设置面板](image/setting-board.png)

---

## ![](https://img.shields.io/badge/TECH-技术栈-555555?style=for-the-badge)

| 层次 | 技术 |
|:---|:---|
| 语言 | TypeScript |
| 宿主 | VSCode Extension API |
| 前端 UI | Svelte |
| 构建 | esbuild |
| AI 协议 | OpenAI API, Anthropic API, Gemini API（统一客户端） |
| 扩展协议 | MCP (Model Context Protocol) |

---

## ![](https://img.shields.io/badge/STARS-Star_History-F59E0B?style=for-the-badge)

[![Star History Chart](https://api.star-history.com/svg?repos=MistRipple/magi&type=Date)](https://star-history.com/#MistRipple/magi&Date)

---

## ![](https://img.shields.io/badge/SPONSORS-赞助-E91E63?style=for-the-badge)

**Magi 的诞生离不开早期支持者的帮助。**
##### 赞助用户
<table>
  <tr>
    <td align="center">
      <a href="https://github.com/Poonwai">
        <img src="https://images.weserv.nl/?url=https://github.com/Poonwai.png&mask=circle&w=80" width="80" alt="Poonwai"><br>
        <sub><b>Poonwai</b></sub>
      </a>
    </td>
    <td align="center">
      <a href="https://github.com/agassiz">
        <img src="https://images.weserv.nl/?url=https://github.com/agassiz.png&mask=circle&w=80" width="80" alt="agassiz"><br>
        <sub><b>agassiz</b></sub>
      </a>
    </td>
    <td align="center">
      <a href="https://github.com/StoneFancyX">
        <img src="https://images.weserv.nl/?url=https://github.com/StoneFancyX.png&mask=circle&w=80" width="80" alt="StoneFancyX"><br>
        <sub><b>StoneFancyX</b></sub>
      </a>
    </td>
  </tr>
</table>

---

##### 赞助站

**Token 支持**: [BinCode 中转站](https://newapi.stonefancyx.com/)

---

## ![](https://img.shields.io/badge/CONTACT-联系-9C27B0?style=for-the-badge)

### 联系

无论是功能建议、Bug 反馈还是商务合作，欢迎随时交流。

<p align="left">
  <img src="image/wechat.png" height="180" />
  &nbsp;&nbsp;
  <img src="image/image.png" height="180" />
</p>

> [!NOTE]
> **左侧**：个人微信（商务合作/问题反馈） | **右侧**：Magi 测试群二维码

---

## ![](https://img.shields.io/badge/LICENSE-协议-607D8B?style=for-the-badge)

本项目采用 **双重授权协议 (Dual Licensing)**：

1. **开源授权**：本项目核心代码采用 [GNU GPL v3](LICENSE) 协议。你可以免费使用、修改和分发代码；如果你基于此项目开发新软件并分发，项目也必须保持开源并采用 GPL 协议。
2. **商业授权**：如果你不希望受到 GPL 协议的限制（例如集成到闭源商业产品），可联系购买商业授权。

**如有商业授权需求或任何疑问，请联系作者：**

* **WeChat**: MistRipple
* **GitHub**: [MistRipple](https://github.com/MistRipple)
