# MultiCLI Orchestration Refactor Plan

This document defines the target architecture, refactor milestones, and task list
for migrating to a long-lived CLI session model with unified protocol, queueing,
health recovery, and role isolation.

## Goals
- Long-lived CLI sessions per model (claude/codex/gemini) with context continuity.
- Single-writer queue per CLI process to avoid concurrency conflicts.
- Unified message protocol (start/end markers + JSON metadata).
- Reliable cancel/interrupt with soft-stop and hard-restart.
- Health monitoring and auto-recovery with replay of last context.
- Orchestrator and Worker role isolation (separate sessions/processes).
- Token-efficient memory: auto-summarize + memory injection.
- Deterministic UI event stream with full conversation timeline.

## Target Architecture
### 1) Session Manager (per CLI)
- One long-lived process per CLI type (per session scope).
- Queue input, consume output with explicit end markers.
- Resume on crash with context snapshot + memory injection.

### 2) Unified Protocol
- Structured envelope for every CLI call:
  - request_id, task_id, subtask_id, role, intent, timestamp
  - stop_marker (unique)
  - meta: tool_policy, file_lock, memory_refs
- Output parsing:
  - stream to UI
  - detect completion via stop_marker
  - capture tool calls, errors, token usage

### 3) Scheduler + Queueing
- Per-CLI FIFO queue with priority and file locks.
- Global planner splits tasks, then dispatches to CLI queues.
- Parallel where safe; serialize on file-level locks.

### 4) Health + Recovery
- Heartbeat per CLI process.
- Idle timeout detection + auto restart.
- On restart: restore last memory summary + key context.

### 5) Review/Verification Pipeline
- Self-check / peer-review optional and policy-driven.
- Integration review as a separate phase only when needed.
- Avoid review on simple tasks (fast path).

### 6) UI Event Stream
- Full timeline of Orchestrator <-> Worker messages.
- Tool invocation shows both request and result.
- Consistent rendering for special panels (md/code/tool).

## Refactor Milestones
### Milestone A: Protocol + Session Core
- Introduce long-lived CLI session abstraction.
- Implement unified envelope + stop_marker handling.
- Provide session lifecycle APIs: start, send, interrupt, reset.

### Milestone B: Queue + Scheduler
- Per-CLI queue with file locks and priority.
- Dispatch via scheduler; remove per-call process spawn.

### Milestone C: Orchestrator Integration
- Orchestrator uses session API and queue.
- Review policy + integration policy applied centrally.
- Context compression and memory injection integrated.

### Milestone D: UI + Observability
- Emit structured events for every step.
- Fix tool panels ordering and state updates.
- Add stats: per-cli tokens, latency, retries.

## Task List
### A. Protocol + Session Core
1. Define protocol schema (request/response, stop_marker, tool policy). ✅
2. Implement SessionProcess for claude/codex/gemini. ✅
3. Add output parser with stop_marker detection. ✅
4. Add retry + auto-restart on failure. ✅

### B. Queue + Scheduler
5. Per-CLI queue with file-level locks. ✅ (queue + per-CLI single consumer)
6. Scheduler dispatch rules (parallel/serial). ✅ (WorkerPool + file locks)
7. Cancel/interrupt routing to correct session. ✅ (soft interrupt + hard restart)

### C. Orchestrator Integration
8. Route all execution via session API. ✅
9. Move review/self-check policies to central gate. ✅
10. Context compression + memory injection per phase. ✅

### D. UI + Observability
11. Event stream for Orchestrator/Worker messages. ✅
12. Tool panel: request/result pairing and ordering. ✅
13. Stats panel: accurate token + latency, reset button. ✅

## Acceptance Criteria
- Long-lived sessions for each CLI are stable across multiple prompts.
- Orchestrator tasks complete without process churn.
- Tool panel shows request + result in order.
- Cancel/resume works; no orphaned CLI processes.
- Token and request stats update in real time.

## Execution Notes
- Refactor will remove "spawn per message" usage.
- Existing adapters will be wrapped or replaced.
- Tests should include: simple ask, multi-task, cancel, resume.
