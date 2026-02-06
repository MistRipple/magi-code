/**
 * 真实 LLM 编排端到端测试
 * - 使用真实 LLM + MissionDrivenEngine
 * - 记录消息流、重复内容、dispatchId
 */

import { LLMAdapterFactory } from '../llm/adapter-factory';
import { MissionDrivenEngine } from '../orchestrator/core';
import { SnapshotManager } from '../snapshot-manager';
import { UnifiedSessionManager } from '../session';
import { globalEventBus } from '../events';

type StandardMsg = {
  id?: string;
  source?: string;
  type?: string;
  lifecycle?: string;
  worker?: string;
  blocks?: Array<{ type: string; content?: string }>;
  metadata?: any;
};

type SharedEntryLike = {
  id: string;
  missionId: string;
  type: string;
  source: string;
  sources?: string[];
};

type SharedContextMetrics = {
  missionId: string;
  assignments: number;
  targetTypeCounts: Record<'decision' | 'contract' | 'risk' | 'constraint', number>;
  totalTargetEntries: number;
  writeDensityPerAssignment: number;
  readCoverageRate: number;
  reuseRate: number;
  avgReadTimes: number;
  mergedSourceRate: number;
};

function normalizeText(input: string): string {
  return input
    .replace(/^#{1,6}\s*/gm, '')
    .replace(/\*\*/g, '')
    .replace(/\s+/g, ' ')
    .trim()
    .toLowerCase();
}

function extractTextFromBlocks(blocks?: Array<{ type: string; content?: string }>): string {
  if (!Array.isArray(blocks)) return '';
  return blocks
    .filter(b => b && b.type === 'text' && typeof b.content === 'string')
    .map(b => b.content as string)
    .join('\n');
}

async function run() {
  const prompt = process.argv.slice(2).join(' ') || '能做编排任务吗';
  const workspaceRoot = process.cwd();

  const sessionManager = new UnifiedSessionManager(workspaceRoot);
  const snapshotManager = new SnapshotManager(sessionManager, workspaceRoot);
  const adapterFactory = new LLMAdapterFactory({ cwd: workspaceRoot });

  // 初始化 adapter factory（加载 profile）
  await adapterFactory.initialize();

  // 统一 Todo 系统：不再需要 UnifiedTaskManager

  const orchestrator = new MissionDrivenEngine(
    adapterFactory,
    {
      timeout: 300000,
      maxRetries: 3,
      review: { selfCheck: false, peerReview: 'never', maxRounds: 0 },
      planReview: { enabled: false },
      verification: { compileCheck: false, lintCheck: false, testCheck: false },
      integration: { enabled: false },
      strategy: { enableVerification: false, enableRecovery: false, autoRollbackOnFailure: false },
    },
    workspaceRoot,
    snapshotManager,
    sessionManager
  );

  // 统一 Todo 系统：不再需要 setTaskManager

  // 自动确认/澄清/提问回调（避免卡住）
  orchestrator.setConfirmationCallback(async () => true);
  orchestrator.setQuestionCallback(async (questions) => {
    return questions.map(q => `Q: ${q}\nA: 无`).join('\n\n');
  });
  orchestrator.setClarificationCallback(async (questions, context, score, originalPrompt) => {
    const answers: Record<string, string> = {};
    questions.forEach(q => { answers[q] = '无'; });
    return { answers, additionalInfo: '' };
  });

  await orchestrator.initialize();

  // 共享上下文读写统计（用于回归验收）
  const contextManager = (orchestrator as any).contextManager as any;
  const sharedContextPool = contextManager?.getSharedContextPool?.();
  const missionSnapshots = new Map<string, SharedEntryLike[]>();
  const readCountByEntry = new Map<string, number>();
  if (contextManager && sharedContextPool) {
    const originalPoolGetByMission = sharedContextPool.getByMission.bind(sharedContextPool);
    const originalPoolGetByType = sharedContextPool.getByType.bind(sharedContextPool);
    const originalClearMissionContext = contextManager.clearMissionContext.bind(contextManager);

    sharedContextPool.getByMission = (missionId: string, options: any) => {
      const entries = originalPoolGetByMission(missionId, options);
      for (const entry of entries) {
        readCountByEntry.set(entry.id, (readCountByEntry.get(entry.id) || 0) + 1);
      }
      return entries;
    };

    sharedContextPool.getByType = (missionId: string, type: string, maxTokens?: number) => {
      const entries = originalPoolGetByType(missionId, type, maxTokens);
      for (const entry of entries) {
        readCountByEntry.set(entry.id, (readCountByEntry.get(entry.id) || 0) + 1);
      }
      return entries;
    };

    contextManager.clearMissionContext = (missionId: string) => {
      const entries = originalPoolGetByMission(missionId, {});
      missionSnapshots.set(
        missionId,
        entries.map((entry: SharedEntryLike) => ({
          ...entry,
          sources: Array.isArray(entry.sources) ? [...entry.sources] : undefined,
        }))
      );
      return originalClearMissionContext(missionId);
    };
  }

  // TODO: LLM mode - check adapter connectivity instead of worker availability
  const availability = {
    claude: adapterFactory.isConnected('claude'),
    codex: adapterFactory.isConnected('codex'),
    gemini: adapterFactory.isConnected('gemini'),
  };
  const available = Object.entries(availability).filter(([, ok]) => ok).map(([k]) => k);
  if (available.length === 0) {
    console.log('适配器未连接，将在首次使用时连接。');
  }

  console.log('=== 真实 LLM 编排 E2E ===');
  console.log(`用户输入: ${prompt}`);
  console.log(`可用适配器: ${available.length > 0 ? available.join(', ') : '将自动连接'}`);
  console.log('');

  const standardMessages: StandardMsg[] = [];
  const standardCompletes: StandardMsg[] = [];
  const progressEvents: Array<{ subTaskId?: string; dispatchId?: string; msg?: string }> = [];
  const subtaskStarts: Array<{ subTaskId?: string; dispatchId?: string; worker?: string }> = [];
  const duplicates: Array<{ id?: string; content: string }> = [];
  const seenContent = new Set<string>();

  adapterFactory.on('standardMessage', (msg: StandardMsg) => {
    standardMessages.push(msg);
  });
  adapterFactory.on('standardComplete', (msg: StandardMsg) => {
    standardCompletes.push(msg);
    const content = extractTextFromBlocks(msg.blocks);
    const key = normalizeText(content);
    if (key && seenContent.has(key)) {
      duplicates.push({ id: msg.id, content });
    } else if (key) {
      seenContent.add(key);
    }
  });

  const unsubUi = globalEventBus.on('orchestrator:ui_message', (event) => {
    const data = event.data as any;
    if (data?.type === 'progress_update') {
      progressEvents.push({
        subTaskId: data?.metadata?.subTaskId,
        dispatchId: data?.metadata?.dispatchId,
        msg: data?.content,
      });
    }
  });

  const unsubStart = globalEventBus.on('subtask:started', (event) => {
    const data = event.data as any;
    subtaskStarts.push({
      subTaskId: event.subTaskId,
      dispatchId: data?.dispatchId,
      worker: data?.worker,
    });
  });

  const start = Date.now();
  let error: Error | null = null;
  try {
    await orchestrator.execute(prompt, '');
  } catch (err) {
    error = err instanceof Error ? err : new Error(String(err));
  } finally {
    unsubUi();
    unsubStart();
    await adapterFactory.shutdown().catch(() => {});
  }

  const duration = Date.now() - start;

  console.log('\n=== 执行完成 ===');
  console.log(`耗时: ${duration}ms`);
  if (error) {
    console.log(`错误: ${error.message}`);
  }

  const dispatchMap = new Map<string, Set<string>>();
  subtaskStarts.forEach(s => {
    if (!s.subTaskId) return;
    if (!dispatchMap.has(s.subTaskId)) dispatchMap.set(s.subTaskId, new Set());
    if (s.dispatchId) dispatchMap.get(s.subTaskId)!.add(s.dispatchId);
  });

  console.log('\n=== 子任务分发统计 ===');
  if (dispatchMap.size === 0) {
    console.log('无子任务分发（可能为 ask 模式）');
  } else {
    dispatchMap.forEach((ids, subTaskId) => {
      console.log(`- ${subTaskId}: dispatchId=${Array.from(ids).join(', ') || 'none'}`);
    });
  }

  console.log('\n=== 重复内容检测（standardComplete） ===');
  if (duplicates.length === 0) {
    console.log('未检测到重复内容');
  } else {
    console.log(`重复内容数: ${duplicates.length}`);
    duplicates.slice(0, 3).forEach((d, i) => {
      console.log(`${i + 1}. id=${d.id || 'n/a'} 片段=${d.content.slice(0, 120)}`);
    });
  }

  console.log('\n=== 标准消息统计 ===');
  console.log(`standardMessage: ${standardMessages.length}`);
  console.log(`standardComplete: ${standardCompletes.length}`);

  const bySource = standardCompletes.reduce((acc, m) => {
    const key = m.source || 'unknown';
    acc[key] = (acc[key] || 0) + 1;
    return acc;
  }, {} as Record<string, number>);
  console.log('按来源统计:', bySource);

  const missionId = (orchestrator as any).lastMissionId as string | null;
  if (missionId && contextManager && sharedContextPool) {
    const missionOrchestrator = (orchestrator as any).missionOrchestrator as any;
    const mission = await missionOrchestrator?.getMission?.(missionId);
    const snapshotEntries: SharedEntryLike[] =
      missionSnapshots.get(missionId) || sharedContextPool.getByMission(missionId, {});
    const targetTypes = ['decision', 'contract', 'risk', 'constraint'] as const;
    const targetEntries = snapshotEntries.filter((entry) => targetTypes.includes(entry.type as any));

    const typeCounts: Record<'decision' | 'contract' | 'risk' | 'constraint', number> = {
      decision: 0,
      contract: 0,
      risk: 0,
      constraint: 0,
    };
    for (const entry of targetEntries) {
      const key = entry.type as keyof typeof typeCounts;
      if (key in typeCounts) {
        typeCounts[key] += 1;
      }
    }

    const readCounts = targetEntries.map((entry) => readCountByEntry.get(entry.id) || 0);
    const readCoverageCount = readCounts.filter((count) => count > 0).length;
    const reuseCount = readCounts.filter((count) => count > 1).length;
    const totalReads = readCounts.reduce((sum, count) => sum + count, 0);
    const mergedSourceCount = targetEntries.filter((entry) => (entry.sources?.length || 0) > 1).length;
    const assignments = mission?.assignments?.length || 0;

    const metrics: SharedContextMetrics = {
      missionId,
      assignments,
      targetTypeCounts: typeCounts,
      totalTargetEntries: targetEntries.length,
      writeDensityPerAssignment: assignments > 0 ? targetEntries.length / assignments : targetEntries.length,
      readCoverageRate: targetEntries.length > 0 ? readCoverageCount / targetEntries.length : 0,
      reuseRate: targetEntries.length > 0 ? reuseCount / targetEntries.length : 0,
      avgReadTimes: targetEntries.length > 0 ? totalReads / targetEntries.length : 0,
      mergedSourceRate: targetEntries.length > 0 ? mergedSourceCount / targetEntries.length : 0,
    };

    console.log('\n=== SharedContextPool 统计（decision/contract/risk/constraint） ===');
    console.log(`missionId: ${metrics.missionId}`);
    console.log(`assignments: ${metrics.assignments}`);
    console.log(`typeCounts: ${JSON.stringify(metrics.targetTypeCounts)}`);
    console.log(`totalEntries: ${metrics.totalTargetEntries}`);
    console.log(`writeDensityPerAssignment: ${metrics.writeDensityPerAssignment.toFixed(2)}`);
    console.log(`readCoverageRate: ${(metrics.readCoverageRate * 100).toFixed(1)}%`);
    console.log(`reuseRate(读取>1): ${(metrics.reuseRate * 100).toFixed(1)}%`);
    console.log(`avgReadTimes: ${metrics.avgReadTimes.toFixed(2)}`);
    console.log(`mergedSourceRate: ${(metrics.mergedSourceRate * 100).toFixed(1)}%`);
  } else {
    console.log('\n=== SharedContextPool 统计 ===');
    console.log('未获取到 missionId 或 SharedContextPool，无法统计写入密度与复用率。');
  }

  process.exit(error ? 1 : 0);
}

run().catch((err) => {
  console.error(err);
  process.exit(1);
});
