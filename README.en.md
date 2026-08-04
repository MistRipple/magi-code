# Magi

**Your local AI engineering team.**

Magi is a local-first, self-hostable AI engineering workspace. It keeps goals, ownership, context, tools, changes, and verification on one observable workflow, so complex software work is not a one-shot prompt but something that can run, recover, be reviewed, and ship.

![Magi mainline task overview](docs/images/readme/mainline-task-overview.jpg)

> **Turn a single request into a durable, observable, and reviewable engineering workflow.**

[中文](README.md) · [English](README.en.md) · [架构图](docs/architecture.html) · [License](LICENSE) · [GitHub](https://github.com/MistRipple/magi-code)

## English

### Complex work should not depend on repeated prompting

A real change usually involves a chain of decisions: what outcome matters, where to start, who investigates, which model fits, which files change, how to verify, and where to recover after a failure. Magi organizes that chain into a long-running local engineering team:

- The main agent owns the goal, breaks down the work, waits for results, and synthesizes the outcome.
- Executor, explorer, architect, tester, and reviewer agents work in parallel with bounded responsibilities and independent models.
- Files, Shell, search, knowledge, MCP, Skills, and image generation share one permission and governance model.
- Goals, task state, tool calls, changes, and verification results stay visible on the same execution trail.

Magi is not another chat window with extra controls. It turns every request into an engineering pipeline that can run for a long time, recover from failures, and be reviewed end to end.

### From request to delivery

~~~text
Outcome
   ↓
Main agent interprets constraints and decomposes the work
   ↓
Specialized agents execute in parallel with their own models
   ↓
Files, Shell, search, knowledge, MCP, and Skills run under one governance model
   ↓
Streaming output, task state, changes, and agent results stay visible
   ↓
Main agent waits, synthesizes, verifies, and keeps the goal moving
~~~

### Product capabilities

#### One mainline, many specialists

The main agent works like an engineering lead: it understands the request, assigns work, waits for results, and produces the final synthesis. Built-in specialist roles include executor, explorer, architect, tester, and reviewer.

Each role can use its own model, and one role can run multiple agent instances in the same task. Subagents do not spawn deeper subagents; the mainline owns the topology so fan-out remains deliberate, visible, and bounded.

#### Goal mode for long-running work

Goal mode is durable task state, not a one-off planning message. It keeps the outcome, constraints, acceptance criteria, task ledger, progress, pause state, and terminal reason together.

You can steer a running goal, pause it, resume it, edit it, or clear it. The mainline continues the same goal with its existing context instead of reconstructing the task from scratch on every turn.

#### Models assigned by responsibility

Different responsibilities should not be locked into one model. Magi separates them instead of forcing the whole product through one selector:

- Main model for the conversation and orchestration.
- Auxiliary model for titles, knowledge extraction, memory, and context compaction.
- Image model for image generation.
- Role models for executor, explorer, architect, tester, reviewer, and other agents.

It supports the standard OpenAI-compatible API format and the Anthropic Messages API format. Image generation uses the OpenAI-compatible Images API, so model choice serves the workflow rather than limiting it.

#### One governed tool runtime

File operations, patches, search, shell, processes, change previews, knowledge queries, image generation, Skills, and MCP tools share one catalog and execution policy.

Every call is evaluated against workspace and session scope, access profile, tool read/write policy, and execution governance. Streaming results, tool cards, final summaries, and runtime state are written back through the same event path for the mainline and subagents.

#### Context that stays connected

Magi assembles the active task context from the current conversation, workspace code index, project knowledge, goals, task ledger, agent runs, tool records, and user-selected references. The backend owns this assembly, so the desktop app, browser, mobile browser, and public tunnel all see the same state.

#### One service, many clients

The desktop application starts the Magi daemon. The desktop window, local browser, LAN devices, and public tunnel connect to the same runtime. Closing the window hides it in the system tray by default; quitting from the tray stops the service and all access paths.

Magi targets Windows, Linux, and macOS while retaining Web, LAN, and optional public-tunnel access.

#### Visible engineering operations

Magi keeps the important runtime state visible: streaming output from the mainline and each subagent, agent lifecycle, tool cards, file previews, changes, Goal progress, task status, context usage, knowledge access, and runtime diagnostics. You do not have to trust a black box; you can inspect the evidence before deciding what to approve or roll back.

#### Themes, wallpapers, and workspace materials

Appearance is more than a light/dark switch. Magi includes a shared theme library with system, light, dark, Deep Forest, Starry Snow Mountain, Azure Shrine, Quantum Matrix, Coastal Dawn, and Desert Dawn themes. Themes can carry wallpapers and clear or immersive materials, be previewed, customized, imported, and exported. The daemon persists the active appearance so desktop and Web use the same configuration.

### Product evidence

These captures come from the daemon-hosted local entry point using the `magi` and `magi-rust-rewrite` workspaces. The mainline, model, and appearance captures use sanitized demonstration data, while the change review shows real workspace changes as they appeared when the image was captured. They are not concept art or component crops; every image is a complete application-window capture on the same `1913 × 1263` canvas.

![Magi mainline task overview with the Starry Snow Mountain skin](docs/images/readme/mainline-task-overview.jpg)

![Magi multi-agent conversation](docs/images/readme/multi-agent-conversation.jpg)

![Magi model and role configuration with the Starry Snow Mountain skin](docs/images/readme/model-configuration.jpg)

![Magi theme library and skin previews](docs/images/readme/appearance-themes.jpg)

![Magi tools, MCP, and Skills](docs/images/readme/tools-mcp-skills.jpg)

![Magi knowledge overview](docs/images/readme/knowledge-system-complete.jpg)

![Magi change review](docs/images/readme/changes-review.jpg)

![Magi image generation](docs/images/readme/image-generation.jpg)

### Why teams use Magi

Magi is built to turn model capability into a local engineering system that can cooperate, recover, and be reviewed over time:

- **Bring your own model stack** with independent connections for orchestration, support, image generation, and specialist roles.
- **Coordinate bounded specialists** with explicit responsibilities, controlled fan-out, and one mainline responsible for synthesis.
- **Keep the full execution trail visible**, from the original goal to task progress, tool calls, file changes, validation, and final evidence.
- **Turn project knowledge into a working asset** through code indexing, ADRs, FAQs, and continuously accumulated engineering experience.
- **Apply one governance model everywhere** across files, Shell, search, MCP, Skills, image generation, permissions, and access profiles.
- **Own the runtime and its data** with a local daemon, self-hosted deployment, and user-managed workspace and session state.
- **Continue from any client** because desktop, browser, LAN, and tunnel access share the same authoritative runtime.
- **Support real delivery workflows** with cancellation, recovery, failure diagnostics, change review, and release verification.

### When it fits

When you need to move a project forward instead of receiving a one-off answer, Magi fits naturally:

- Architecture analysis and module-level review of large codebases.
- Refactors that benefit from parallel exploration, implementation, testing, and review.
- Team workflows where different models serve different responsibilities.
- Development goals that run for hours or longer.
- Local developers who want control over project context and runtime deployment.
- Environments where the same task must be inspected from desktop, browser, phone, or LAN devices.

### Quick start

Requirements: stable Rust, Node.js 22 or newer, npm, and the Tauri 2 platform dependencies needed for desktop builds.

~~~bash
git clone https://github.com/MistRipple/magi-code.git
cd magi-code
npm --prefix web ci
./scripts/dev-daemon.sh
~~~

Open http://127.0.0.1:38123/web.html.

For development, start only the daemon. It starts or reuses the fixed-port Vite server and serves the UI, API, and SSE through the same 38123 origin.

### Desktop build

~~~bash
npm --prefix web run build
cargo run -p magi-desktop
~~~

The Tauri 2 desktop host targets macOS DMG for both Apple Silicon and Intel, Linux AppImage/Deb, and Windows NSIS. Pushing a version-matching `v*` tag triggers GitHub Actions to build the installers, signed updater archives, and a Release containing `latest.json`. Installed desktop builds check for updates at startup and periodically while running. Updates download and verify in the background, then wait for the user to choose **Restart now** or **Restart later** instead of interrupting active work. Before installation, Magi persists its state and stops the local service gracefully. Runtime data under `~/.magi` stays outside the application bundle and is preserved across updates.

Release builds require the `TAURI_SIGNING_PRIVATE_KEY` GitHub Repository Secret and may use `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`. The private key is used only by GitHub Actions and is never committed or bundled; the public key is stored in `apps/desktop/tauri.conf.json` for client-side verification.

### Configure models and roles

After launch, open **Settings -> Models** to configure the main connection, auxiliary model, OpenAI-compatible image model, and independent role bindings. Choose the read-only, restricted, or full-access profile that matches the task.

Model settings are stored in the local Magi state directory and should never be committed to the repository.

### Repository layout

~~~text
apps/daemon/                         Headless service entry point
apps/desktop/                        Tauri desktop host
crates/magi-api/                     HTTP, SSE, and public APIs
crates/magi-conversation-runtime/   Conversation, context, and task dispatch
crates/magi-agent-role/              Agent role definitions and registry
crates/magi-tool-runtime/            Built-in tools, permissions, and catalog
crates/magi-knowledge-store/         Code index and project knowledge
crates/magi-context-runtime/         Context source selection and assembly
crates/...                           Sessions, goals, tasks, memory, usage, snapshots
web/                                  Svelte Web UI
docs/                                 Architecture documentation and graph
scripts/                              Development and graph-generation scripts
~~~

### Verification

~~~bash
cargo fmt --all -- --check
cargo check -p magi-daemon
cargo test --workspace
npm --prefix web test
npm --prefix web run check
npm --prefix web run build
~~~

### Engineering principles

- The daemon is the single business kernel; the desktop host does not duplicate business logic.
- Backend state and protocol are authoritative for frontend presentation.
- Each capability has one production path, without duplicate implementations or compatibility fallbacks.
- Tool execution must pass workspace, access-profile, path-boundary, permission, and governance checks.
- Subagents cannot create more subagents; the mainline owns the agent topology.
- Model settings, knowledge records, and runtime data belong to the local user environment by default.

### Repository and license

- GitHub: [MistRipple/magi-code](https://github.com/MistRipple/magi-code)
- Issues: [Report an issue](https://github.com/MistRipple/magi-code/issues)
- Releases: [Download releases](https://github.com/MistRipple/magi-code/releases)

The core Magi code is licensed under the [Apache License 2.0](LICENSE).

---

**Magi was made possible by the early supporters who helped it grow.**

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

##### Sponsor service

**Token support**: [BinCode relay](https://stonefancyx.com/)

### Contact

Feature suggestions, bug reports, and business inquiries are welcome.

<p align="left">
  <img src="docs/images/wechat.png" height="180" alt="Personal WeChat QR code">
  &nbsp;&nbsp;
  <img src="docs/images/group.png" height="180" alt="Magi test group QR code">
</p>

> [!NOTE]
> **Left**: personal WeChat for business inquiries and feedback | **Right**: Magi test group QR code

Magi is released under the [Apache License 2.0](LICENSE). You may use, modify, and distribute it as permitted by the license terms.
