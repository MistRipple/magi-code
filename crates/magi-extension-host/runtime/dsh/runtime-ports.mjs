// Tool pipeline and runtime provider ports for the shared DSH host.
// Wires official `@deepseek-ai/dsh-user-approval`, `@deepseek-ai/dsh-compaction`,
// `@deepseek-ai/dsh-tools` pipeline hooks, `@deepseek-ai/dsh-system-prompt` assembly,
// plus `permissionPresets`, `goals`, `schedule`, `tokenMeter`, `toolResultPruner`,
// and `deepseekLlmApiExtensions` on the shared Cordis context.

import { randomUUID } from 'node:crypto';
import { Service } from '@deepseek-ai/cordis';
import { CompactionEngine } from '@deepseek-ai/dsh-compaction';
import { ToolCallId } from '@deepseek-ai/dsh-llm';
import { scopeTarget } from '@deepseek-ai/dsh-scope';
import { defineDomain, domainTable } from '@deepseek-ai/dsh-storage-domain';
import { renderContextSections, renderPrompt } from '@deepseek-ai/dsh-system-prompt';
import { ApprovalService } from '@deepseek-ai/dsh-user-approval';
import { snapshotJsonValue } from '@deepseek-ai/dsh-util-values';
import { z } from 'zod';

const SCHEDULE_DOMAIN_SPEC = defineDomain({
  name: 'magi_schedules',
  version: 1,
  tables: {
    jobs: domainTable(z.object({
      id: z.string(),
      sessionId: z.string(),
      text: z.string(),
      intervalMs: z.number().int().positive().optional(),
      runAt: z.number().int().positive().optional(),
      paused: z.boolean().default(false),
      ownerPluginId: z.string().nullable().default(null),
      createdAt: z.number().int(),
      lastRunAt: z.number().int().nullable().default(null),
      runCount: z.number().int().nonnegative().default(0),
    })),
  },
});

const PERMISSION_PRESETS = Object.freeze([
  Object.freeze({
    id: 'read-only',
    magiAccessProfile: 'read_only',
    title: 'Read Only',
    description: 'Magi read-only access profile.',
  }),
  Object.freeze({
    id: 'workspace-write',
    magiAccessProfile: 'restricted',
    title: 'Workspace Write (Restricted)',
    description: 'Magi restricted access profile with safety approvals enforced.',
  }),
  Object.freeze({
    id: 'danger-full-access',
    magiAccessProfile: 'full_access',
    title: 'Full Access',
    description: 'Magi full access profile permitting unrestricted workspace operations.',
  }),
]);

function countEventListeners(root, eventName) {
  try {
    const hooks = root?.events?._hooks?.[eventName];
    if (Array.isArray(hooks)) return hooks.length;
    if (hooks && typeof hooks.size === 'number') return hooks.size;
  } catch { /* ignore */ }
  return 0;
}

function normalizeBlocks(raw) {
  if (!Array.isArray(raw)) return [];
  const out = [];
  for (const item of raw) {
    if (typeof item === 'string') out.push({ type: 'text', text: item });
    else if (item && typeof item === 'object' && item.type === 'text' && typeof item.text === 'string') {
      out.push({ type: 'text', text: item.text });
    }
  }
  return out;
}

function renderBlocksText(blocks) {
  return normalizeBlocks(blocks).map(b => b.text).join('\n');
}

export async function openRuntimePorts({
  root,
  lifecycle,
  requestOwner,
  notifyOwner,
  invocationScope,
  resolveAgentHandle,
  mount,
}) {
  const disposers = [];
  let closed = false;

  // 1. Official ApprovalService (`ctx.approval`)
  await mount(ApprovalService);

  // 2. PermissionPresets (`ctx.permissionPresets`) mapped to Magi Restricted / FullAccess
  class MagiPermissionPresetsService extends Service {
    constructor(ctx) {
      super(ctx, 'permissionPresets');
    }

    list() {
      return [...PERMISSION_PRESETS];
    }

    get(id) {
      return PERMISSION_PRESETS.find(item => item.id === id || item.magiAccessProfile === id);
    }

    async current(sessionId) {
      const response = await requestOwner('host.session.get', { sessionId }).catch(() => null);
      const profile = response?.session?.accessProfile ?? 'restricted';
      if (profile === 'full_access') return 'danger-full-access';
      if (profile === 'read_only') return 'read-only';
      return 'workspace-write';
    }

    async select(sessionId, presetId) {
      const preset = this.get(presetId);
      if (!preset) throw new Error(`Unknown permission preset "${String(presetId)}"`);
      await requestOwner('host.session.setAccessProfile', {
        sessionId,
        accessProfile: preset.magiAccessProfile,
      });
      return preset;
    }
  }
  await mount(MagiPermissionPresetsService);

  // 3. CompactionEngine (`ctx.compaction`)
  class MagiCompactionService extends CompactionEngine {
    async compactIfNeeded(agent, trigger, signal) {
      signal?.throwIfAborted?.();
      const decision = await root.waterfall(
        'compaction/before',
        {
          sessionId: agent?.session?.id ?? agent?.id ?? '',
          trigger,
          reason: trigger,
        },
        async () => ({ kind: 'proceed' }),
      );
      if (decision?.kind === 'summary' && typeof decision.summary === 'string') {
        return { compacted: true, summary: decision.summary };
      }
      return null;
    }
  }
  await mount(MagiCompactionService);

  // 4. Goals (`ctx.goals`) -> daemon authoritative goal storage
  class MagiGoalsService extends Service {
    constructor(ctx) {
      super(ctx, 'goals');
    }

    async list(sessionId) {
      const res = await requestOwner('host.goals.list', { sessionId });
      return Array.isArray(res?.goals) ? res.goals : [];
    }

    async get(sessionId, goalId) {
      const res = await requestOwner('host.goals.get', { sessionId, goalId });
      return res?.goal ?? null;
    }

    async create(sessionId, input) {
      const res = await requestOwner('host.goals.create', { sessionId, ...(input ?? {}) });
      return res?.goal ?? res;
    }

    async update(sessionId, goalId, patch) {
      const res = await requestOwner('host.goals.update', { sessionId, goalId, ...(patch ?? {}) });
      return res?.goal ?? res;
    }

    async delete(sessionId, goalId) {
      await requestOwner('host.goals.delete', { sessionId, goalId });
      return { deleted: true };
    }
  }
  await mount(MagiGoalsService);

  // 5. Schedule (`ctx.schedule`) -> durable schedules backed by `storageDomain` delivering prompts to target session
  const timers = new Map();
  let scheduleDomain = null;

  async function ensureScheduleDomain() {
    if (scheduleDomain) return scheduleDomain;
    scheduleDomain = await root.storageDomain.open(SCHEDULE_DOMAIN_SPEC);
    return scheduleDomain;
  }

  function clearJobTimer(id) {
    const existing = timers.get(id);
    if (existing !== undefined) {
      clearTimeout(existing);
      timers.delete(id);
    }
  }

  async function fireScheduledJob(id) {
    clearJobTimer(id);
    if (closed) return;
    const domain = await ensureScheduleDomain();
    const job = domain.table('jobs').get(id);
    if (!job || job.paused) return;
    try {
      await requestOwner('host.session.prompt', {
        sessionId: job.sessionId,
        requestId: `schedule-${id}-${Date.now()}`,
        steer: false,
        text: job.text,
        images: [],
      });
      const updated = {
        ...job,
        lastRunAt: Date.now(),
        runCount: job.runCount + 1,
      };
      if (typeof job.intervalMs === 'number' && job.intervalMs > 0) {
        await domain.table('jobs').put(id, updated);
        armJobTimer(updated);
      } else {
        await domain.table('jobs').delete(id);
      }
    } catch (err) {
      root.logger.warn(`schedule job "${id}" failed: ${String(err?.message ?? err)}`);
      if (typeof job.intervalMs === 'number' && job.intervalMs > 0) {
        armJobTimer(job);
      }
    }
  }

  function armJobTimer(job) {
    clearJobTimer(job.id);
    if (closed || job.paused) return;
    let delayMs = null;
    if (typeof job.intervalMs === 'number' && job.intervalMs > 0) {
      delayMs = job.intervalMs;
    } else if (typeof job.runAt === 'number') {
      delayMs = Math.max(0, job.runAt - Date.now());
    }
    if (delayMs === null) return;
    const timer = setTimeout(() => { void fireScheduledJob(job.id); }, Math.min(delayMs, 2_147_483_647));
    timer.unref?.();
    timers.set(job.id, timer);
  }

  class MagiScheduleService extends Service {
    constructor(ctx) {
      super(ctx, 'schedule');
    }

    async list(sessionId) {
      const domain = await ensureScheduleDomain();
      const all = [...domain.table('jobs').entries()].map(([, job]) => job);
      return sessionId ? all.filter(item => item.sessionId === sessionId) : all;
    }

    async create({ sessionId, text, intervalMs, runAt, delayMs }) {
      if (typeof sessionId !== 'string' || !sessionId) throw new Error('schedule.create requires sessionId');
      if (typeof text !== 'string' || !text.trim()) throw new Error('schedule.create requires non-empty text');
      const caller = root.reflect.trace(this.ctx);
      const owner = lifecycle.ownerIdentity(caller);
      const now = Date.now();
      const resolvedRunAt = typeof runAt === 'number'
        ? runAt
        : typeof delayMs === 'number' && delayMs >= 0
          ? now + delayMs
          : undefined;
      if (intervalMs === undefined && resolvedRunAt === undefined) {
        throw new Error('schedule.create requires intervalMs, runAt, or delayMs');
      }
      const job = {
        id: randomUUID(),
        sessionId,
        text,
        ...(typeof intervalMs === 'number' ? { intervalMs } : {}),
        ...(typeof resolvedRunAt === 'number' ? { runAt: resolvedRunAt } : {}),
        paused: false,
        ownerPluginId: owner?.pluginId ?? null,
        createdAt: now,
        lastRunAt: null,
        runCount: 0,
      };
      const domain = await ensureScheduleDomain();
      await domain.table('jobs').put(job.id, job);
      armJobTimer(job);
      return job;
    }

    async cancel(id) {
      clearJobTimer(id);
      const domain = await ensureScheduleDomain();
      await domain.table('jobs').delete(id);
      return { cancelled: true };
    }

    async pause(id) {
      clearJobTimer(id);
      const domain = await ensureScheduleDomain();
      const job = domain.table('jobs').get(id);
      if (!job) return null;
      const next = { ...job, paused: true };
      await domain.table('jobs').put(id, next);
      return next;
    }

    async resume(id) {
      const domain = await ensureScheduleDomain();
      const job = domain.table('jobs').get(id);
      if (!job) return null;
      const next = { ...job, paused: false };
      await domain.table('jobs').put(id, next);
      armJobTimer(next);
      return next;
    }
  }
  await mount(MagiScheduleService);
  // Restore persisted schedules on startup.
  const initialSchedules = await ensureScheduleDomain();
  for (const [, job] of initialSchedules.table('jobs').entries()) {
    armJobTimer(job);
  }
  disposers.push(async () => {
    for (const id of [...timers.keys()]) clearJobTimer(id);
    if (scheduleDomain) {
      const dom = scheduleDomain;
      scheduleDomain = null;
      await dom.close();
    }
  });

  // 6. TokenMeter (`ctx.tokenMeter`) -> committed usage ledger synced with daemon assistant/message.usage
  const recordedUsage = new Map();
  const seenCommittedUsageSeqs = new Map();
  function addUsageEntry(sessionId, usage) {
    if (typeof sessionId !== 'string' || !sessionId || !usage || typeof usage !== 'object') return;
    const cur = recordedUsage.get(sessionId) ?? {
      inputTokens: 0,
      outputTokens: 0,
      cacheReadTokens: 0,
      cacheWriteTokens: 0,
      requests: 0,
    };
    const input = Number(usage.inputTokens ?? usage.input ?? usage.promptTokens ?? 0) || 0;
    const output = Number(usage.outputTokens ?? usage.output ?? usage.completionTokens ?? 0) || 0;
    const cacheRead = Number(usage.cacheReadTokens ?? usage.cacheRead ?? 0) || 0;
    const cacheWrite = Number(usage.cacheWriteTokens ?? usage.cacheWrite ?? 0) || 0;
    cur.inputTokens += input;
    cur.outputTokens += output;
    cur.cacheReadTokens += cacheRead;
    cur.cacheWriteTokens += cacheWrite;
    cur.requests += 1;
    recordedUsage.set(sessionId, cur);
  }
  function observeCommittedUsage(params) {
    const sessionId = params?.sessionId;
    const obs = params?.observation;
    if (typeof sessionId !== 'string' || !sessionId || !obs || typeof obs !== 'object') return;
    if (obs.kind !== 'assistantMessage' || !obs.usage || typeof obs.usage !== 'object') return;
    const seq = params?.eventSeq;
    if (typeof seq === 'number') {
      let seen = seenCommittedUsageSeqs.get(sessionId);
      if (!seen) {
        seen = new Set();
        seenCommittedUsageSeqs.set(sessionId, seen);
      }
      if (seen.has(seq)) return;
      seen.add(seq);
      if (seen.size > 4096) {
        const first = seen.values().next().value;
        seen.delete(first);
      }
    }
    addUsageEntry(sessionId, obs.usage);
  }
  class MagiTokenMeterService extends Service {
    constructor(ctx) {
      super(ctx, 'tokenMeter');
    }

    record(sessionId, usage) {
      addUsageEntry(sessionId, usage);
    }

    summary(sessionId) {
      const cur = recordedUsage.get(sessionId);
      return cur ? Object.freeze({ ...cur }) : Object.freeze({
        inputTokens: 0,
        outputTokens: 0,
        cacheReadTokens: 0,
        cacheWriteTokens: 0,
        requests: 0,
      });
    }

    all() {
      const out = {};
      for (const [sid, cur] of recordedUsage.entries()) {
        out[sid] = Object.freeze({ ...cur });
      }
      return out;
    }

    total() {
      let inputTokens = 0;
      let outputTokens = 0;
      let cacheReadTokens = 0;
      let cacheWriteTokens = 0;
      let requests = 0;
      for (const cur of recordedUsage.values()) {
        inputTokens += cur.inputTokens;
        outputTokens += cur.outputTokens;
        cacheReadTokens += cur.cacheReadTokens;
        cacheWriteTokens += cur.cacheWriteTokens;
        requests += cur.requests;
      }
      return Object.freeze({ inputTokens, outputTokens, cacheReadTokens, cacheWriteTokens, requests });
    }
  }
  await mount(MagiTokenMeterService);

  // 7. ToolResultPruner (`ctx.toolResultPruner`)
  const pruners = new Set();
  class MagiToolResultPrunerService extends Service {
    constructor(ctx) {
      super(ctx, 'toolResultPruner');
    }

    register(pruner) {
      if (typeof pruner !== 'function') throw new TypeError('toolResultPruner.register requires a function');
      return this.ctx.effect(() => {
        pruners.add(pruner);
        publishCapabilities();
        return () => {
          pruners.delete(pruner);
          publishCapabilities();
        };
      }, 'toolResultPruner.register');
    }

    prune(input) {
      let current = input;
      for (const pruner of [...pruners]) {
        const next = pruner(current);
        if (next !== undefined) current = next;
      }
      return current;
    }
  }
  await mount(MagiToolResultPrunerService);

  // 8. DeepSeekLlmApiExtensions (`ctx.deepseekLlmApiExtensions`)
  const llmExtensions = new Set();
  class MagiDeepSeekLlmApiExtensionsService extends Service {
    constructor(ctx) {
      super(ctx, 'deepseekLlmApiExtensions');
    }

    register(extension) {
      if (!extension || (typeof extension !== 'function' && typeof extension.extendRequest !== 'function')) {
        throw new TypeError('deepseekLlmApiExtensions.register requires a function or { extendRequest }');
      }
      return this.ctx.effect(() => {
        llmExtensions.add(extension);
        publishCapabilities();
        return () => {
          llmExtensions.delete(extension);
          publishCapabilities();
        };
      }, 'deepseekLlmApiExtensions.register');
    }

    async apply(request, context) {
      let current = request;
      for (const ext of [...llmExtensions]) {
        const fn = typeof ext === 'function' ? ext : ext.extendRequest.bind(ext);
        const next = await fn(current, context);
        if (next !== undefined) current = next;
      }
      return current;
    }
  }
  await mount(MagiDeepSeekLlmApiExtensionsService);

  // Track active runtime hook capabilities so daemon can skip IPC in O(1) when unused.
  // Baseline counts exclude listeners registered by built-in host services (e.g. session-controller UI bridges).
  const baselineApprovalListeners = countEventListeners(root, 'approval/request');
  const baselineAgentCreatedListeners = countEventListeners(root, 'agent/created');
  let lastCapabilitiesJson = '';
  function computeCapabilities() {
    const hasPromptSections = (() => {
      try {
        const layers = root.systemPrompt?.layers;
        if (layers?.global?.contexts) {
          for (const [name] of layers.global.contexts.entries()) {
            if (name === 'approval:policy') continue;
            return true;
          }
        }
        if (layers?.global?.sections) {
          for (const [name, sec] of layers.global.sections.entries()) {
            // Built into the official MCP resource runtime; it has text only while a provider is registered.
            if (name === 'mcp-resource-servers') {
              if (root.mcpResources?.hasProviders?.()) return true;
              continue;
            }
            if (name === 'deployment:persona-prefix' || name === 'deployment:persona-suffix') {
              if (typeof sec?.text === 'string' && sec.text.trim()) return true;
              continue;
            }
            return true;
          }
        }
        const liveAgents = root.get('agents')?.list?.() ?? [];
        for (const agent of liveAgents) {
          for (const layer of root.systemPrompt.layers.chainLayers(agent) ?? []) {
            if (layer?.sections && !layer.sections.isEmpty?.()) return true;
            if (layer?.contexts && !layer.contexts.isEmpty?.()) return true;
          }
        }
      } catch { /* ignore */ }
      return false;
    })();
    const hasToolGuards = (() => {
      try {
        if (root.tools?.guards?.global && !root.tools.guards.global.isEmpty()) return true;
        const liveAgents = root.get('agents')?.list?.() ?? [];
        for (const agent of liveAgents) {
          for (const layer of root.tools?.guards?.chainLayers?.(agent) ?? []) {
            if (layer && !layer.isEmpty?.()) return true;
          }
        }
      } catch { /* ignore */ }
      return false;
    })();
    const hasToolRestrictions = (() => {
      try {
        if (root.tools?.layers?.global?.restrictions && !root.tools.layers.global.restrictions.isEmpty?.()) return true;
        const liveAgents = root.get('agents')?.list?.() ?? [];
        for (const agent of liveAgents) {
          for (const layer of root.tools?.layers?.chainLayers?.(agent) ?? []) {
            if (layer?.restrictions && !layer.restrictions.isEmpty?.()) return true;
          }
        }
      } catch { /* ignore */ }
      return false;
    })();
    const hasPluginAgentCreatedListeners = countEventListeners(root, 'agent/created') > baselineAgentCreatedListeners;
    const hasMessageProjections = Boolean(root.get('sessions')?.messageProjections?.length > 0);
    return {
      toolsPreExecute: countEventListeners(root, 'tools/pre-execute') > 0 || hasToolGuards,
      toolsPostExecute:
        countEventListeners(root, 'tools/post-execute') > 0
        || countEventListeners(root, 'tools/result') > 0
        || pruners.size > 0,
      toolsRestrict: hasToolRestrictions || hasPluginAgentCreatedListeners,
      systemPromptAssemble: countEventListeners(root, 'system-prompt/assemble') > 0 || hasPromptSections,
      approval: countEventListeners(root, 'approval/request') > baselineApprovalListeners,
      compactionBefore: countEventListeners(root, 'compaction/before') > 0,
      contextProject: hasMessageProjections || countEventListeners(root, 'agent/pre-step') > 0,
      llmExtend: llmExtensions.size > 0,
      llmStreamIntercept: countEventListeners(root, 'llm/stream') > 0,
    };
  }

  function publishCapabilities() {
    if (closed) return;
    const caps = computeCapabilities();
    const encoded = JSON.stringify(caps);
    if (encoded === lastCapabilitiesJson) return;
    lastCapabilitiesJson = encoded;
    notifyOwner?.('host.hooks.capabilities', { capabilities: caps });
  }

  disposers.push(root.on('system-prompt/change', publishCapabilities));
  disposers.push(root.on('tools/change', publishCapabilities));
  disposers.push(root.on('agent/created', publishCapabilities, { global: true }));
  disposers.push(root.on('internal/listener', publishCapabilities));
  publishCapabilities();

  async function withSessionAgent(sessionId, fn) {
    if (typeof sessionId === 'string' && sessionId) {
      const live = root.get('agents')?.get?.(sessionId) ?? await resolveAgentHandle?.(sessionId);
      if (live) return await fn(live);
      const scope = invocationScope(sessionId);
      try {
        return await fn(scope.agent);
      } finally {
        await scope.dispose();
      }
    }
    return await fn(undefined);
  }

  // Shared Stage 3 Tool Pipeline execution helpers (used by BOTH plugin tools AND Rust native tools!)
  async function runPreExecute({ sessionId, callId, name, arguments: rawArgs }, signal) {
    signal?.throwIfAborted?.();
    const frozenArgs = snapshotJsonValue(rawArgs ?? {}) ?? {};
    const resolvedCallId = ToolCallId(String(callId ?? randomUUID()));
    return await withSessionAgent(sessionId, async agent => {
      const exec = Object.freeze({
        callId: resolvedCallId,
        rootCallId: resolvedCallId,
        name: String(name ?? ''),
        arguments: frozenArgs,
        ...(agent ? { agent } : {}),
        signal: signal ?? new AbortController().signal,
        token: Symbol('magi.tool.pre'),
      });
      const carrier = agent ? scopeTarget(root.tools, agent) : root.tools;
      const decision = await root.waterfall(
        carrier,
        'tools/pre-execute',
        exec,
        async () => ({ kind: 'allow' }),
      );
      if (!decision || typeof decision !== 'object' || decision.kind === 'allow') {
        const effectiveArgs =
          decision?.arguments && typeof decision.arguments === 'object' && !Array.isArray(decision.arguments)
            ? snapshotJsonValue(decision.arguments)
            : undefined;
        const guardExec = effectiveArgs !== undefined ? Object.freeze({ ...exec, arguments: effectiveArgs }) : exec;
        const guardDenial = root.tools.checkGuards?.(guardExec);
        if (guardDenial) {
          const reason = typeof guardDenial === 'string' ? guardDenial : guardDenial.reason;
          const info = typeof guardDenial === 'object' ? guardDenial.info : undefined;
          return { kind: 'deny', reason: String(reason || 'Denied by tool guard'), ...(info ? { info } : {}) };
        }
        return {
          kind: 'allow',
          ...(effectiveArgs !== undefined ? { arguments: effectiveArgs } : {}),
        };
      }
      if (decision.kind === 'deny') {
        return {
          kind: 'deny',
          reason: String(decision.reason || 'Denied by tools/pre-execute'),
          ...(decision.info ? { info: decision.info } : {}),
        };
      }
      if (decision.kind === 'cancel') {
        return { kind: 'cancel' };
      }
      if (decision.kind === 'ask') {
        // Consult composed `ctx.approval` listeners before deferring to the daemon approval gate.
        if (agent && countEventListeners(root, 'approval/request') > baselineApprovalListeners) {
          if (typeof sessionId === 'string' && sessionId) {
            await root.get('sessionQuery')?.readSession?.(sessionId).catch(() => {});
          }
          const outcome = await root.approval.decide({
            agent,
            toolName: exec.name,
            callId: resolvedCallId,
            reason: decision.reason ?? `Approve tool "${exec.name}"`,
            signal: exec.signal,
          }, agent.session);
          if (outcome === 'allowed-once') {
            const guardDenial = root.tools.checkGuards?.(exec);
            if (guardDenial) {
              const reason = typeof guardDenial === 'string' ? guardDenial : guardDenial.reason;
              return { kind: 'deny', reason: String(reason || 'Denied by tool guard') };
            }
            return { kind: 'allow', approvedByPlugin: true };
          }
          if (outcome === 'rejected') {
            return { kind: 'deny', reason: decision.reason ?? 'Rejected by approval provider' };
          }
          if (outcome === 'cancelled') {
            return { kind: 'cancel' };
          }
        }
        return {
          kind: 'ask',
          ...(decision.reason ? { reason: decision.reason } : {}),
          ...(decision.displayReason ? { displayReason: decision.displayReason } : {}),
        };
      }
      return { kind: 'allow' };
    });
  }

  async function runPostExecute({ sessionId, callId, name, arguments: rawArgs, isError, status, content, value }, signal) {
    signal?.throwIfAborted?.();
    const frozenArgs = snapshotJsonValue(rawArgs ?? {}) ?? {};
    const resolvedCallId = ToolCallId(String(callId ?? randomUUID()));
    const baseBlocks = typeof content === 'string'
      ? [{ type: 'text', text: content }]
      : normalizeBlocks(content);
    return await withSessionAgent(sessionId, async agent => {
      const exec = Object.freeze({
        callId: resolvedCallId,
        rootCallId: resolvedCallId,
        name: String(name ?? ''),
        arguments: frozenArgs,
        ...(agent ? { agent } : {}),
        signal: signal ?? new AbortController().signal,
        token: Symbol('magi.tool.post'),
      });
      const carrier = agent ? scopeTarget(root.tools, agent) : root.tools;
      const pruned = root.toolResultPruner.prune({
        name: exec.name,
        arguments: frozenArgs,
        isError: Boolean(isError),
        status: status ?? (isError ? 'failed' : 'succeeded'),
        content: baseBlocks,
        text: renderBlocksText(baseBlocks),
      });
      const resultObj = Object.freeze(isError
        ? {
            isError: true,
            error: { message: pruned?.text ?? renderBlocksText(baseBlocks) },
            content: pruned?.content ?? baseBlocks,
          }
        : {
            isError: false,
            value: value ?? null,
            content: pruned?.content ?? baseBlocks,
          });
      const decision = await root.waterfall(
        carrier,
        'tools/post-execute',
        exec,
        resultObj,
        async () => ({ kind: 'accept' }),
      );
      let finalResult = resultObj;
      let out = { kind: 'accept' };
      if (decision?.kind === 'block') {
        const feedbackBlocks = normalizeBlocks(decision.feedback);
        finalResult = Object.freeze({
          isError: true,
          error: { message: renderBlocksText(feedbackBlocks) || 'Blocked by tools/post-execute', ...(decision.info ? { info: decision.info } : {}) },
          content: feedbackBlocks,
        });
        out = {
          kind: 'block',
          feedback: feedbackBlocks,
          text: renderBlocksText(feedbackBlocks),
          ...(decision.info ? { info: decision.info } : {}),
        };
      } else if (decision?.kind === 'accept' && decision.content !== undefined) {
        const replaced = normalizeBlocks(decision.content);
        finalResult = Object.freeze({ ...resultObj, content: replaced });
        out = { kind: 'accept', content: replaced, text: renderBlocksText(replaced) };
      } else if (pruned?.text !== undefined && pruned.text !== renderBlocksText(baseBlocks)) {
        const replaced = [{ type: 'text', text: String(pruned.text) }];
        finalResult = Object.freeze({ ...resultObj, content: replaced });
        out = { kind: 'accept', content: replaced, text: String(pruned.text) };
      }
      root.emit(carrier, 'tools/result', exec, finalResult);
      return out;
    });
  }

  async function assembleSystemPrompt({ sessionId, basePrompt, phase, round }, signal) {
    signal?.throwIfAborted?.();
    return await withSessionAgent(sessionId, async agent => {
      const assembly = await root.systemPrompt.assemble({
        ...(agent ? { scope: agent } : {}),
        signal,
        basePrompt,
        phase,
        round,
      });
      const text = renderPrompt(assembly).trim();
      const renderedContexts = renderContextSections(assembly);
      let hasComplete = false;
      try {
        const merged = root.systemPrompt.layers.merge(agent, layer => layer.sections);
        for (const sec of merged.values()) {
          if (sec?.complete === true) {
            hasComplete = true;
            break;
          }
        }
      } catch { /* ignore */ }
      return {
        complete: hasComplete,
        ...(text ? { text } : {}),
        sections: assembly.sections.map(s => ({ name: s.name, text: s.text })),
        contexts: renderedContexts.map(c => ({ name: c.name, text: c.text })),
      };
    });
  }

  async function askApproval({ sessionId, callId, toolName, reason, summary }, signal) {
    signal?.throwIfAborted?.();
    return await withSessionAgent(sessionId, async agent => {
      if (!agent || countEventListeners(root, 'approval/request') <= baselineApprovalListeners) {
        return { outcome: 'unavailable' };
      }
      if (typeof sessionId === 'string' && sessionId) {
        await root.get('sessionQuery')?.readSession?.(sessionId).catch(() => {});
      }
      const outcome = await root.approval.decide({
        agent,
        toolName: String(toolName ?? ''),
        ...(callId ? { callId: ToolCallId(String(callId)) } : {}),
        reason: String(reason ?? summary ?? `Approve ${toolName}`),
        signal: signal ?? new AbortController().signal,
      }, agent.session);
      return { outcome };
    });
  }

  async function beforeCompaction(params, signal) {
    signal?.throwIfAborted?.();
    const decision = await root.waterfall(
      'compaction/before',
      params ?? {},
      async () => ({ kind: 'proceed' }),
    );
    if (decision?.kind === 'cancel') {
      return { kind: 'cancel', plugin: String(decision.plugin ?? 'dsh-plugin'), reason: String(decision.reason ?? 'cancelled by plugin') };
    }
    if (decision?.kind === 'summary' && typeof decision.summary === 'string') {
      return { kind: 'summary', plugin: String(decision.plugin ?? 'dsh-plugin'), summary: decision.summary };
    }
    return { kind: 'proceed' };
  }

  async function projectMessages({ sessionId, messages }, signal) {
    signal?.throwIfAborted?.();
    const sessions = root.get('sessions');
    const projectors = Array.isArray(sessions?.messageProjections) ? sessions.messageProjections : [];
    let current = Array.isArray(messages) ? messages : [];
    for (const projector of projectors) {
      const next = await projector(current, { sessionId, signal });
      if (Array.isArray(next)) current = next;
    }
    if (countEventListeners(root, 'agent/pre-step') > 0) {
      await withSessionAgent(sessionId, async agent => {
        const result = await root.waterfall(
          'agent/pre-step',
          { sessionId, agent, messages: current, signal },
          async () => ({ messages: current }),
        );
        if (Array.isArray(result?.messages)) current = result.messages;
      });
    }
    return { messages: current };
  }

  async function extendLlmRequest({ sessionId, request }, signal) {
    signal?.throwIfAborted?.();
    const extended = await root.deepseekLlmApiExtensions.apply(request ?? {}, { sessionId, signal });
    return { request: extended };
  }

  async function interceptLlmStream({
    sessionId,
    provider,
    model,
    attempt = 0,
    content = '',
    reasoningContent,
    toolCallNames = [],
    finishReason,
  }, signal) {
    signal?.throwIfAborted?.();
    if (countEventListeners(root, 'llm/stream') === 0) {
      return { action: 'accept' };
    }
    const baseText = typeof content === 'string' ? content : '';
    const options = {
      sessionId,
      provider: String(provider ?? ''),
      model: typeof model === 'string' ? model : undefined,
      attempt: Number(attempt) || 0,
      toolCallNames: Array.isArray(toolCallNames) ? toolCallNames : [],
      signal,
    };
    let nextCalls = 0;
    let retryHint = null;
    const makeBaseStream = async function* () {
      if (typeof reasoningContent === 'string' && reasoningContent.length > 0) {
        yield { type: 'reasoning-delta', index: 0, text: reasoningContent };
      }
      if (baseText.length > 0) {
        yield { type: 'text-delta', index: 0, text: baseText };
      }
      yield {
        type: 'finish',
        reason: { kind: finishReason === 'tool_calls' ? 'tool-calls' : 'stop' },
      };
    };
    const next = () => {
      nextCalls += 1;
      if (nextCalls > 1) {
        retryHint = {
          reason: 'llm/stream listener invoked next() for retry',
          ...(typeof options.appendUserMessage === 'string' ? { appendUserMessage: options.appendUserMessage } : {}),
        };
        return (async function* () {
          yield { type: 'finish', reason: { kind: 'stop' } };
        })();
      }
      return makeBaseStream();
    };
    const carrier = root.get('llm') ?? root;
    const wrapped = root.waterfall(carrier, 'llm/stream', options, next);
    if (wrapped && typeof wrapped === 'object' && !(Symbol.asyncIterator in wrapped) && !(Symbol.iterator in wrapped)) {
      if (wrapped.action === 'retry' || wrapped.retry === true) {
        return {
          action: 'retry',
          ...(typeof wrapped.reason === 'string' ? { reason: wrapped.reason } : {}),
          ...(typeof wrapped.appendUserMessage === 'string' ? { appendUserMessage: wrapped.appendUserMessage } : {}),
        };
      }
      if (wrapped.action === 'replace' && typeof wrapped.content === 'string') {
        return { action: 'replace', content: wrapped.content };
      }
      return { action: 'accept' };
    }
    const chunks = [];
    if (wrapped && (typeof wrapped[Symbol.asyncIterator] === 'function' || typeof wrapped[Symbol.iterator] === 'function')) {
      for await (const chunk of wrapped) {
        signal?.throwIfAborted?.();
        if (chunk && typeof chunk === 'object' && chunk.type === 'retry') {
          return {
            action: 'retry',
            ...(typeof chunk.reason === 'string' ? { reason: chunk.reason } : {}),
            ...(typeof chunk.appendUserMessage === 'string' ? { appendUserMessage: chunk.appendUserMessage } : {}),
          };
        }
        chunks.push(chunk);
      }
    }
    if (retryHint || options.retry === true) {
      return {
        action: 'retry',
        reason: retryHint?.reason ?? (typeof options.retryReason === 'string' ? options.retryReason : 'llm/stream retry'),
        ...(typeof (retryHint?.appendUserMessage ?? options.appendUserMessage) === 'string'
          ? { appendUserMessage: retryHint?.appendUserMessage ?? options.appendUserMessage }
          : {}),
      };
    }
    if (chunks.length > 0) {
      const assembledText = chunks
        .filter(c => c && typeof c === 'object' && c.type === 'text-delta' && typeof c.text === 'string')
        .map(c => c.text)
        .join('');
      if (assembledText !== baseText) {
        return { action: 'replace', content: assembledText };
      }
    }
    return { action: 'accept' };
  }

  async function filterVisibleTools({ sessionId, names }) {
    const inputNames = Array.isArray(names) ? names.filter(n => typeof n === 'string') : [];
    if (inputNames.length === 0 || !root.tools?.view) return { names: inputNames };
    return await withSessionAgent(sessionId, async (agent) => {
      if (!agent) return { names: inputNames };
      const view = root.tools.view(agent);
      // Restrictions apply to global tools registered in ToolsService; preserve native names unless explicitly denied.
      const restrictedLayers = root.tools.layers?.chainLayers?.(agent) ?? [];
      const allowed = inputNames.filter(name => {
        for (const layer of restrictedLayers) {
          if (layer?.restrictions) {
            for (const filter of layer.restrictions.values()) {
              if (filter.allow && !filter.allow.has(name)) return false;
              if (filter.deny && filter.deny.has(name)) return false;
            }
          }
        }
        return true;
      });
      void view;
      return { names: allowed };
    });
  }

  return {
    runPreExecute,
    runPostExecute,
    assembleSystemPrompt,
    askApproval,
    beforeCompaction,
    projectMessages,
    extendLlmRequest,
    interceptLlmStream,
    observeCommittedUsage,
    filterVisibleTools,
    hookCapabilities: computeCapabilities,
    publishCapabilities,
    async close() {
      closed = true;
      for (const dispose of disposers.splice(0).reverse()) {
        try { await dispose?.(); } catch { /* ignore */ }
      }
    },
  };
}
