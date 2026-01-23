/**
 * 真实 LLM 编排端到端测试
 * - 使用真实 LLM + IntelligentOrchestrator
 * - 记录消息流、重复内容、dispatchId
 */

import { LLMAdapterFactory } from '../llm/adapter-factory';
import { IntelligentOrchestrator } from '../orchestrator/intelligent-orchestrator';
import { SnapshotManager } from '../snapshot-manager';
import { UnifiedSessionManager } from '../session';
import { UnifiedTaskManager } from '../task/unified-task-manager';
import { SessionManagerTaskRepository } from '../task/session-manager-task-repository';
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

  const session = sessionManager.getOrCreateCurrentSession();
  const repository = new SessionManagerTaskRepository(sessionManager, session.id);
  const taskManager = new UnifiedTaskManager(session.id, repository);
  await taskManager.initialize();

  const orchestrator = new IntelligentOrchestrator(
    adapterFactory,
    sessionManager,
    snapshotManager,
    workspaceRoot,
    {
      review: { selfCheck: false, peerReview: 'never', maxRounds: 0 },
      planReview: { enabled: false },
      verification: { compileCheck: false, lintCheck: false, testCheck: false },
      integration: { enabled: false },
      strategy: { enableVerification: false, enableRecovery: false, autoRollbackOnFailure: false },
    }
  );
  orchestrator.setTaskManager(taskManager, session.id);

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

  process.exit(error ? 1 : 0);
}

run().catch((err) => {
  console.error(err);
  process.exit(1);
});
