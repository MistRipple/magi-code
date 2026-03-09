import type { TerminalOperation, TerminalSessionBlock, ToolCall } from '../types/message';
import { normalizeTerminalOperation, parseLeadingJson } from '../lib/terminal-utils';

const MAX_TOOL_SNAPSHOT_TRACKED = 4000;


function computeOverlap(prev: string, next: string): number {
  const maxOverlap = Math.min(prev.length, next.length);
  for (let length = maxOverlap; length > 0; length -= 1) {
    if (prev.slice(prev.length - length) === next.slice(0, length)) {
      return length;
    }
  }
  return 0;
}

function reconcileOutputSnapshot(previous: string, incoming: string): string {
  if (!incoming) return previous;
  if (!previous) return incoming;
  if (incoming === previous) return previous;
  if (incoming.startsWith(previous)) return incoming;
  if (previous.startsWith(incoming)) return previous;
  if (previous.includes(incoming)) return previous;
  if (incoming.includes(previous)) return incoming;
  const overlap = computeOverlap(previous, incoming);
  if (overlap > 0) {
    return previous + incoming.slice(overlap);
  }
  return incoming;
}

function mergeDeltaOutput(previous: TerminalSessionBlock, patch: Partial<TerminalSessionBlock>): string {
  const currentOutput = previous.output || '';
  const chunk = typeof patch.output === 'string' ? patch.output : '';
  if (!chunk) return currentOutput;

  const fromCursor = Number.isInteger(patch.fromCursor) ? patch.fromCursor as number : undefined;
  const previousNextCursor = Number.isInteger(previous.nextCursor)
    ? previous.nextCursor as number
    : (Number.isInteger(previous.outputCursor) ? previous.outputCursor as number : undefined);

  if (fromCursor === undefined || previousNextCursor === undefined) {
    if (currentOutput.endsWith(chunk)) return currentOutput;
    return currentOutput + chunk;
  }

  if (fromCursor === previousNextCursor) {
    return currentOutput + chunk;
  }

  if (fromCursor < previousNextCursor) {
    const drop = previousNextCursor - fromCursor;
    if (drop >= chunk.length) {
      return currentOutput;
    }
    return currentOutput + chunk.slice(drop);
  }

  return currentOutput + chunk;
}

function normalizeStatus(rawStatus?: string, fallback?: string): string {
  if (typeof rawStatus === 'string' && rawStatus.trim()) return rawStatus;
  if (typeof fallback === 'string' && fallback.trim()) return fallback;
  return 'running';
}

function toIntOrUndefined(value: unknown): number | undefined {
  return Number.isInteger(value) ? (value as number) : undefined;
}

class TerminalSessionStore {
  private sessions = $state<Map<number, TerminalSessionBlock>>(new Map());
  private toolSnapshots = new Map<string, string>();
  private toolSnapshotOrder: string[] = [];
  private toolCallToTerminalId = new Map<string, number>();

  getById(terminalId: number): TerminalSessionBlock | undefined {
    return this.sessions.get(terminalId);
  }

  getByToolCallId(toolCallId?: string): TerminalSessionBlock | undefined {
    if (!toolCallId) return undefined;
    const terminalId = this.toolCallToTerminalId.get(toolCallId);
    if (!terminalId) return undefined;
    return this.sessions.get(terminalId);
  }

  clear(): void {
    this.sessions = new Map();
    this.toolSnapshots.clear();
    this.toolSnapshotOrder = [];
    this.toolCallToTerminalId.clear();
  }

  ingestToolCall(toolCall?: ToolCall): void {
    if (!toolCall) {
      return;
    }

    const normalizedOperation = normalizeTerminalOperation(toolCall.name);
    if (!normalizedOperation) {
      return;
    }

    const snapshotKey = `${toolCall.status}|${toolCall.result || ''}|${toolCall.error || ''}`;
    const previousSnapshot = this.toolSnapshots.get(toolCall.id);
    if (previousSnapshot === snapshotKey) {
      return;
    }
    this.rememberToolSnapshot(toolCall.id, snapshotKey);

    const patches = this.buildPatches(toolCall, normalizedOperation);
    for (const patch of patches) {
      this.upsertPatch(patch);
    }
  }

  private rememberToolSnapshot(toolCallId: string, snapshot: string): void {
    if (!this.toolSnapshots.has(toolCallId)) {
      this.toolSnapshotOrder.push(toolCallId);
      if (this.toolSnapshotOrder.length > MAX_TOOL_SNAPSHOT_TRACKED) {
        const oldest = this.toolSnapshotOrder.shift();
        if (oldest) {
          this.toolSnapshots.delete(oldest);
        }
      }
    }
    this.toolSnapshots.set(toolCallId, snapshot);
  }

  private upsertPatch(patch: Partial<TerminalSessionBlock> & { terminalId: number }): void {
    const prev = this.sessions.get(patch.terminalId);
    const now = Date.now();

    if (!prev) {
      const initial: TerminalSessionBlock = {
        terminalId: patch.terminalId,
        operation: patch.operation || 'shell',
        status: normalizeStatus(patch.status, 'running'),
        phase: patch.phase,
        runMode: patch.runMode,
        terminalName: patch.terminalName,
        cwd: patch.cwd,
        command: patch.command,
        output: patch.output || '',
        outputCursor: patch.outputCursor,
        outputStartCursor: patch.outputStartCursor,
        fromCursor: patch.fromCursor,
        nextCursor: patch.nextCursor,
        delta: patch.delta,
        truncated: patch.truncated,
        startupStatus: patch.runMode === 'task' ? undefined : patch.startupStatus,
        startupMessage: patch.runMode === 'task' ? undefined : patch.startupMessage,
        locked: patch.locked,
        returnCode: patch.returnCode,
        accepted: patch.accepted,
        killed: patch.killed,
        releasedLock: patch.releasedLock,
        error: patch.error,
        updatedAt: now,
      };
      this.sessions = new Map(this.sessions).set(initial.terminalId, initial);
      return;
    }

    let mergedOutput = prev.output;
    if (typeof patch.output === 'string') {
      if (patch.delta) {
        mergedOutput = mergeDeltaOutput(prev, patch);
      } else {
        mergedOutput = reconcileOutputSnapshot(prev.output, patch.output);
      }
    }

    const effectiveRunMode = patch.runMode ?? prev.runMode;
    const isTask = effectiveRunMode === 'task';

    const merged: TerminalSessionBlock = {
      ...prev,
      ...patch,
      operation: patch.operation || prev.operation,
      status: normalizeStatus(patch.status, prev.status),
      output: mergedOutput,
      updatedAt: now,
      // 仅在 patch 提供时覆盖，避免被 undefined 清空
      command: patch.command ?? prev.command,
      runMode: patch.runMode ?? prev.runMode,
      phase: patch.phase ?? prev.phase,
      terminalName: patch.terminalName ?? prev.terminalName,
      cwd: patch.cwd ?? prev.cwd,
      // task 模式不存在 service 启动握手语义，强制清空避免残留显示
      startupStatus: isTask ? undefined : (patch.startupStatus ?? prev.startupStatus),
      startupMessage: isTask ? undefined : (patch.startupMessage ?? prev.startupMessage),
      locked: patch.locked ?? prev.locked,
      returnCode: patch.returnCode ?? prev.returnCode,
      accepted: patch.accepted ?? prev.accepted,
      killed: patch.killed ?? prev.killed,
      releasedLock: patch.releasedLock ?? prev.releasedLock,
      outputCursor: patch.outputCursor ?? prev.outputCursor,
      outputStartCursor: patch.outputStartCursor ?? prev.outputStartCursor,
      fromCursor: patch.fromCursor ?? prev.fromCursor,
      nextCursor: patch.nextCursor ?? prev.nextCursor,
      delta: patch.delta ?? prev.delta,
      truncated: patch.truncated ?? prev.truncated,
      error: patch.error ?? prev.error,
    };

    this.sessions = new Map(this.sessions).set(merged.terminalId, merged);
  }

  private buildPatches(
    toolCall: ToolCall,
    operation: TerminalOperation,
  ): Array<Partial<TerminalSessionBlock> & { terminalId: number }> {
    const args = toolCall.arguments || {};
    const status = toolCall.status === 'error' ? 'failed' : toolCall.status;
    const parsed = parseLeadingJson(toolCall.result);
    const patches: Array<Partial<TerminalSessionBlock> & { terminalId: number }> = [];

    let terminalId = toIntOrUndefined((parsed as Record<string, unknown> | null)?.terminal_id);
    if (!terminalId) {
      terminalId = this.toolCallToTerminalId.get(toolCall.id);
    }
    if (!terminalId) {
      terminalId = toIntOrUndefined(args.terminal_id);
    }
    if (!terminalId) {
      return patches;
    }
    this.toolCallToTerminalId.set(toolCall.id, terminalId);

    const json = (!Array.isArray(parsed) && parsed && typeof parsed === 'object')
      ? parsed as Record<string, unknown>
      : undefined;

    const patch: Partial<TerminalSessionBlock> & { terminalId: number } = {
      terminalId,
      operation,
      status: normalizeStatus(typeof json?.status === 'string' ? json.status : status),
      phase: typeof json?.phase === 'string' ? json.phase : undefined,
      runMode: json?.run_mode === 'service' ? 'service' : (json?.run_mode === 'task' ? 'task' : undefined),
      terminalName: typeof json?.terminal_name === 'string' ? json.terminal_name : undefined,
      cwd: typeof json?.cwd === 'string' ? json.cwd : undefined,
      command: typeof args.command === 'string' ? args.command : undefined,
      outputCursor: toIntOrUndefined(json?.output_cursor),
      outputStartCursor: toIntOrUndefined(json?.output_start_cursor),
      fromCursor: toIntOrUndefined(json?.from_cursor),
      nextCursor: toIntOrUndefined(json?.next_cursor),
      delta: typeof json?.delta === 'boolean' ? json.delta : undefined,
      truncated: typeof json?.truncated === 'boolean' ? json.truncated : undefined,
      startupStatus: typeof json?.startup_status === 'string'
        ? json.startup_status as TerminalSessionBlock['startupStatus']
        : undefined,
      startupMessage: typeof json?.startup_message === 'string' ? json.startup_message : undefined,
      locked: typeof json?.locked === 'boolean' ? json.locked : undefined,
      returnCode: typeof json?.return_code === 'number' ? json.return_code : null,
      accepted: typeof json?.accepted === 'boolean' ? json.accepted : undefined,
      killed: typeof json?.killed === 'boolean' ? json.killed : undefined,
      releasedLock: typeof json?.released_lock === 'boolean' ? json.released_lock : undefined,
      error: toolCall.error,
    };

    if (typeof json?.output === 'string') {
      patch.output = json.output;
    }

    patches.push(patch);
    return patches;
  }
}

export const terminalSessions = new TerminalSessionStore();
