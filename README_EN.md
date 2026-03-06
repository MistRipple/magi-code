<div align="center">

# 🔮 Magi

**Next-Gen Multi-Agent Engineering Orchestration System**

*Intent Analysis · Deep Decomposition · Heterogeneous Collaboration*

[![VSCode Extension](https://img.shields.io/badge/VSCode-Extension-007ACC?style=flat-square&logo=visualstudiocode&logoColor=white)](https://code.visualstudio.com/)
[![Version](https://img.shields.io/badge/version-1.X-blue?style=flat-square)]()
[![License](https://img.shields.io/badge/license-GPL--3.0--or--later-red?style=flat-square)](LICENSE)

<br/>

![Magi Interface](image/home.png)

<br/>

**Magi is not another ChatBot — it's your AI engineering team.**

It runs inside VSCode, transforms complex engineering tasks into executable task contracts,
dispatches multiple heterogeneous Workers for parallel collaboration,
forming a complete closed loop from intent understanding, task decomposition, execution & repair to acceptance & knowledge retention.

[Core Features](#core-features) • [How It Works](#how-it-works) • [Quick Start](#quick-start) • [Contact](#contact)

[中文文档](README.md)

</div>

---

## ![](https://img.shields.io/badge/WHY-Magi-007ACC?style=for-the-badge)

Most AI coding assistants operate in a single-agent mode, which tends to suffer from stability and consistency issues in cross-module development and long-chain debugging. Magi's core goal is to upgrade "conversational capability" into "engineering delivery capability".

| ![](https://img.shields.io/badge/PAIN_POINTS-red?style=flat-square) | ![](https://img.shields.io/badge/LIMITATIONS-gray?style=flat-square) | ![](https://img.shields.io/badge/SOLUTIONS-Magi-green?style=flat-square) |
|:---|:---|:---|
| **Task Drift** | Goals drift and context decays in long conversations, leading to unstable completion. | **Mission Contract-Driven**: Goal, Acceptance, and Context triplet constrains the execution path. |
| **Concurrency Conflicts** | Multiple models writing files concurrently can overwrite each other. | **Zero-Conflict Engine**: File Mutex + real-time forced read + intent-driven editing. |
| **Repeated Failures** | Retries the same failing path without root cause retention. | **Closed-Loop Self-Repair**: Dynamic Todos + multi-round verification + supplementary instruction injection. |
| **Knowledge Gap** | "Amnesia" after each conversation — experience cannot be reused. | **Local Knowledge Retention**: Rolling Summary + WisdomExtractor + PKB. |
| **Governance Imbalance** | Same execution intensity for all tasks — hard to balance efficiency and safety. | **Dual-Mode Governance**: Standard mode for speed, Deep mode for strict constraints. |

### Typical Use Cases

- **Cross-Module Feature Development**: Frontend/backend/config/script coordinated changes, automatically decomposed and executed in parallel.
- **Complex Bug Troubleshooting**: Supports "locate → fix → re-verify" closed loop, reducing repetitive retries.
- **Large-Scale Refactoring**: Deep mode with forced delegation + snapshot rollback to reduce refactoring risk.

### Usage Boundaries & Expectations

- **Deep mode is more stable but slower**: Suited for high-complexity tasks, not for instant responses.
- **Result quality depends on task description**: Clearer Goal and Acceptance yield more stable delivery.
- **External capabilities depend on local configuration**: Misconfigured CLI/MCP will affect availability.

### Privacy & Data

- **Local-first**: Code retrieval, task state, and knowledge retention all happen in the local workspace.
- **Model calls under your control**: Only uses model channels you configure (OpenAI/Anthropic/Gemini etc.) for inference.

---

## ![](https://img.shields.io/badge/VISION-Positioning-6B4E71?style=for-the-badge)

> **Magi's core philosophy: Use the right model for the right job.**

Just describe your goal, and Magi automatically handles:

> [!TIP]
> **Intent Understanding ➔ Complexity Assessment ➔ Task Planning ➔ Expert Dispatch ➔ Parallel Execution ➔ Acceptance Loop ➔ Knowledge Retention**

![Orchestration Flow](image/orchestrator-2.png)
![Running Result](image/orchestrator-1.png)

You're no longer a prompt engineer — you're the tech lead of an AI team.

---

## ![](https://img.shields.io/badge/FEATURES-Core_Features-31A8FF?style=for-the-badge)

### Core Features

### 1. Mission-Driven Orchestration
- Tasks organized by Goal / Acceptance / Context, no more open-ended free-form conversation.
- Assignment-level responsibility division — Worker results are trackable and verifiable.
- Dynamic Todo additions during execution, converging toward real engineering problems.
![Task System](image/todos.png)
### 2. Dynamic Dual-Mode Governance (Standard / Deep)
- **Standard Mode**: Low-latency delivery for lightweight changes and daily development.
- **Deep Mode**: Orchestrator is forbidden from editing code, forced delegation to Workers — suited for cross-module and system-level refactoring.
- Same engine switches governance intensity based on task complexity, balancing speed and stability.

### 3. Zero-Conflict Concurrent Editing Engine
- **File-level Mutex**: Serialized writes to the same file, preventing concurrent overwrites.
- **Real-time Forced Read**: Reads latest disk state before writing, reducing stale context risk.
- **Intent-Driven Editing**: Models express "what to change", the engine handles stable disk writes.
- **Context Freshness Governance**: Automatically handles stale file context during execution, reducing invalid retries.

### 4. Session Isolation & Recovery
- `UnifiedSessionManager` physically isolates state by trace/session, preventing cross-session contamination.
- Snapshot mechanism covers critical write operations, supporting post-failure recovery and rollback.
- Supplementary instruction queue allows mid-execution requirement additions, effective at the next decision point without interrupting the main flow.

### 5. Local Knowledge Extraction & Retention
- `MemoryDocument` handles rolling summaries, compressing noise while preserving key context.
- `WisdomExtractor` extracts learnings / decisions / warnings from Worker reports.
- Results are retained in the Project Knowledge Base (PKB), improving hit rates for similar future tasks.
![Knowledge Base](image/knowledge.png)
### 6. Extensible Toolchain
- Built-in file I/O, code search, task scheduling, process execution and other engineering tools.
- Native MCP (Model Context Protocol) support for external capability integration.
- Skills dynamic loading, extending professional capabilities by scenario.

![Tool Configuration](image/setting-tool.png)

---

## ![](https://img.shields.io/badge/HOW-How_It_Works-E85D04?style=for-the-badge)

### How It Works

### Three-Level Task Model
- **Mission**: The overall task facing the user's goal.
- **Assignment**: Responsibility units dispatched to specific Workers.
- **Todo**: Evolvable steps during execution (can be added, reordered, or closed-loop repaired).

### Execution Loop
- **Phase A — Planning**: Intent analysis, complexity assessment, task decomposition & context injection.
- **Phase B — Execution**: Workers proceed in parallel; dependent tasks relay in topological order.
- **Phase C — Acceptance**: Review against acceptance criteria; failures automatically enter repair cycles.

### Collaboration Safeguards
- **ContractManager**: Manages interface contracts and collaboration boundaries.
- **FileMutex**: Ensures concurrent write safety.
- **SnapshotManager**: Critical steps are rollback-capable.
- **TaskViewService**: Task state visualization for tracking execution chains.


---

## ![](https://img.shields.io/badge/QUICKSTART-Quick_Start-28A745?style=for-the-badge)

### Quick Start

### Requirements

- **VSCode**: `>= 1.93.0`
- **Node.js**: `>= 18` recommended
- **Available CLI**: At least one configured (Claude / Codex / Gemini)
- **Network**: Required for online search or external model calls

### 1. Install the Extension
Search for **Magi** in the VSCode Extension Marketplace and install, or install locally via `.vsix`.

### 2. Configure Model CLI
Search `magi` in VSCode Settings and configure the CLI paths for orchestration and Workers:
- `magi.claude.path`
- `magi.codex.path`
- `magi.gemini.path`

### 3. Choose Governance Mode
- `magi.deepTask = false`: Standard mode (default)
- `magi.deepTask = true`: Deep mode (project-level governance)

### 4. Start Collaborating
- Open panel: `Ctrl+Shift+A` (Mac: `Cmd+Shift+A`)
- Launch task: `Ctrl+Shift+Enter` (Mac: `Cmd+Shift+Enter`)
- New session: `Ctrl+Alt+N` (Mac: `Cmd+Alt+N`)
- Stop task: `Ctrl+Shift+Backspace` (Mac: `Cmd+Shift+Backspace`)

### 5. Minimal Verification (1 minute)

- In a new session, type: `Please read README.md and summarize the current version number`.
- Observe whether a task is triggered, execution status appears, and a final reply is returned.
- If it fails, check CLI paths and API key configuration first.

![Settings Panel](image/setting-board.png)

---

## ![](https://img.shields.io/badge/TECH-Tech_Stack-555555?style=for-the-badge)

| Layer | Technology |
|:---|:---|
| Language | TypeScript |
| Host | VSCode Extension API |
| Frontend UI | Svelte |
| Build | esbuild |
| AI Protocols | OpenAI API, Anthropic API, Gemini API (unified client) |
| Extension Protocol | MCP (Model Context Protocol) |

---

## ![](https://img.shields.io/badge/STARS-Star_History-F59E0B?style=for-the-badge)

[![Star History Chart](https://api.star-history.com/svg?repos=MistRipple/magi&type=Date)](https://star-history.com/#MistRipple/magi&Date)

---

## ![](https://img.shields.io/badge/SPONSORS-Sponsors-E91E63?style=for-the-badge)

**Magi's creation would not have been possible without the support of early backers.**

##### Sponsors

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

##### Sponsor Platform

**Token Support**: [BinCode Relay](https://newapi.stonefancyx.com/)

---

## ![](https://img.shields.io/badge/CONTACT-Contact-9C27B0?style=for-the-badge)

### Contact

Whether it's feature suggestions, bug reports, or business inquiries, feel free to reach out.

<p align="left">
  <img src="image/wechat.png" height="180" />
  &nbsp;&nbsp;
  <img src="image/image.png" height="180" />
</p>

> [!NOTE]
> **Left**: Personal WeChat (business/feedback) | **Right**: Magi test group QR code

---

## ![](https://img.shields.io/badge/LICENSE-License-607D8B?style=for-the-badge)

This project uses **Dual Licensing**:

1. **Open Source License**: The core code is licensed under [GNU GPL v3](LICENSE). You may freely use, modify, and distribute the code; if you develop new software based on this project and distribute it, the project must also remain open source under the GPL license.
2. **Commercial License**: If you do not wish to be bound by the GPL license (e.g., integrating into a closed-source commercial product), you may purchase a commercial license.

**For commercial licensing or any questions, please contact the author:**

* **WeChat**: MistRipple
* **GitHub**: [MistRipple](https://github.com/MistRipple)