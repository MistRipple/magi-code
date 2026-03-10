#!/usr/bin/env node
/**
 * Worker 韧性回归脚本
 *
 * 覆盖目标：
 * 1) 请求级硬超时触发后，超时不重试（快速失败）
 * 2) 网络瞬断仍保留轻量重试能力
 * 3) 账号类确定性错误按统一策略重试后止损（非无限重试）
 * 4) 短窗连续失败触发通道熔断，避免持续打满上游
 * 5) Worker 遇到模型侧错误时，进入恢复/提问链路，不因控制面抖动直接中止
 * 6) Dispatch 取消信号可触发 in-flight worker 请求中断
 */

const fs = require('fs');
const path = require('path');

const ROOT = path.resolve(__dirname, '..');
const OUT = path.join(ROOT, 'out');

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function wait(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function loadCompiledModule(relPath) {
  const abs = path.join(OUT, relPath);
  if (!fs.existsSync(abs)) {
    throw new Error(`缺少编译产物: ${abs}，请先执行 npm run compile`);
  }
  return require(abs);
}

function createOpenAIConfig() {
  return {
    baseUrl: 'http://127.0.0.1:1',
    apiKey: 'test-key',
    model: 'gpt-5',
    provider: 'openai',
    enabled: true,
  };
}

function waitForAbort(signal, timeoutMs, label) {
  return new Promise((resolve, reject) => {
    if (!signal) {
      reject(new Error(`${label}: missing signal`));
      return;
    }
    if (signal.aborted) {
      resolve();
      return;
    }
    const timer = setTimeout(() => {
      reject(new Error(`${label}: abort timeout`));
    }, timeoutMs);
    signal.addEventListener('abort', () => {
      clearTimeout(timer);
      resolve();
    }, { once: true });
  });
}

async function testUniversalClientTimeoutFailFast() {
  const { UniversalLLMClient } = loadCompiledModule(path.join('llm', 'clients', 'universal-client.js'));
  const client = new UniversalLLMClient(createOpenAIConfig());
  let attempts = 0;

  client.protocolAdapter = {
    provider: 'openai',
    protocol: 'responses',
    capabilities: {
      supportsStreaming: true,
      supportsSystemPrompt: true,
      supportsTools: true,
      supportsThinking: true,
    },
    async send(params) {
      attempts += 1;
      await waitForAbort(params.signal, 300, 'timeout-fail-fast');
      throw new Error('aborted-by-timeout');
    },
    async stream() {
      throw new Error('stream not used');
    },
  };

  let caught;
  try {
    await client.sendMessage({
      messages: [{ role: 'user', content: 'timeout regression' }],
      timeoutMs: 60,
      retryPolicy: {
        maxRetries: 3,
        baseDelayMs: 1,
        retryOnTimeout: false,
      },
    });
  } catch (error) {
    caught = error;
  }

  assert(caught, '超时场景应抛出异常');
  const message = String(caught?.message || '');
  assert(message.toLowerCase().includes('timed out'), `超时错误文案异常: ${message}`);
  assert(attempts === 1, `超时场景应只尝试 1 次，实际: ${attempts}`);

  return { attempts, error: message };
}

async function testUniversalClientNetworkRetry() {
  const { UniversalLLMClient } = loadCompiledModule(path.join('llm', 'clients', 'universal-client.js'));
  const client = new UniversalLLMClient(createOpenAIConfig());
  let attempts = 0;

  client.protocolAdapter = {
    provider: 'openai',
    protocol: 'responses',
    capabilities: {
      supportsStreaming: true,
      supportsSystemPrompt: true,
      supportsTools: true,
      supportsThinking: true,
    },
    async send() {
      attempts += 1;
      const error = new Error('fetch failed');
      error.code = 'ECONNRESET';
      throw error;
    },
    async stream() {
      throw new Error('stream not used');
    },
  };

  let caught;
  try {
    await client.sendMessage({
      messages: [{ role: 'user', content: 'network retry regression' }],
      retryPolicy: {
        maxRetries: 2,
        baseDelayMs: 1,
        retryOnTimeout: false,
      },
    });
  } catch (error) {
    caught = error;
  }

  assert(caught, '网络错误场景应抛出异常');
  assert(attempts === 2, `网络错误场景应尝试 2 次，实际: ${attempts}`);

  return { attempts, error: String(caught?.message || caught) };
}

async function testUniversalClientAuthDeterministicFastStop() {
  const { UniversalLLMClient } = loadCompiledModule(path.join('llm', 'clients', 'universal-client.js'));
  const client = new UniversalLLMClient(createOpenAIConfig());
  let attempts = 0;

  client.protocolAdapter = {
    provider: 'openai',
    protocol: 'responses',
    capabilities: {
      supportsStreaming: true,
      supportsSystemPrompt: true,
      supportsTools: true,
      supportsThinking: true,
    },
    async send() {
      attempts += 1;
      const error = new Error('Unauthorized');
      error.status = 401;
      throw error;
    },
    async stream() {
      throw new Error('stream not used');
    },
  };

  let caught;
  try {
    await client.sendMessage({
      messages: [{ role: 'user', content: 'auth retry regression' }],
      retryPolicy: {
        maxRetries: 6,
        baseDelayMs: 1,
        retryOnTimeout: true,
        retryOnAllErrors: true,
        deterministicErrorStreakLimit: 3,
      },
    });
  } catch (error) {
    caught = error;
  }

  assert(caught, '账号错误场景应抛出异常');
  assert(attempts === 3, `账号类确定性错误应在 3 连击后止损，实际尝试: ${attempts}`);

  return { attempts, error: String(caught?.message || caught) };
}

async function testUniversalClientCircuitBreakerWindow() {
  const { UniversalLLMClient } = loadCompiledModule(path.join('llm', 'clients', 'universal-client.js'));
  const client = new UniversalLLMClient(createOpenAIConfig());
  let attempts = 0;

  client.protocolAdapter = {
    provider: 'openai',
    protocol: 'responses',
    capabilities: {
      supportsStreaming: true,
      supportsSystemPrompt: true,
      supportsTools: true,
      supportsThinking: true,
    },
    async send() {
      attempts += 1;
      const error = new Error('service unavailable');
      error.status = 503;
      throw error;
    },
    async stream() {
      throw new Error('stream not used');
    },
  };

  const retryPolicy = {
    maxRetries: 1,
    baseDelayMs: 0,
    retryOnTimeout: true,
    retryOnAllErrors: true,
    circuitBreaker: {
      enabled: true,
      windowMs: 60_000,
      failureThreshold: 2,
      cooldownMs: 5_000,
    },
  };

  let firstError;
  let secondError;
  let thirdError;
  try {
    await client.sendMessage({
      messages: [{ role: 'user', content: 'cb-1' }],
      retryPolicy,
    });
  } catch (error) {
    firstError = error;
  }
  try {
    await client.sendMessage({
      messages: [{ role: 'user', content: 'cb-2' }],
      retryPolicy,
    });
  } catch (error) {
    secondError = error;
  }
  try {
    await client.sendMessage({
      messages: [{ role: 'user', content: 'cb-3' }],
      retryPolicy,
    });
  } catch (error) {
    thirdError = error;
  }

  assert(firstError, '首个失败请求应返回错误');
  assert(secondError, '第二个失败请求应返回错误');
  assert(thirdError, '熔断阶段请求应被拒绝');
  assert(attempts === 2, `熔断打开后不应继续访问上游，实际上游调用次数: ${attempts}`);
  assert(
    String(thirdError?.code || '').toUpperCase() === 'EUPSTREAM_CIRCUIT_OPEN'
      || String(thirdError?.message || '').includes('熔断'),
    `熔断错误标识异常: ${String(thirdError?.message || thirdError)}`,
  );

  return {
    attempts,
    firstError: String(firstError?.message || firstError),
    secondError: String(secondError?.message || secondError),
    thirdError: String(thirdError?.message || thirdError),
  };
}

async function testAutonomousWorkerModelResilience() {
  const { AutonomousWorker } = loadCompiledModule(path.join('orchestrator', 'worker', 'autonomous-worker.js'));

  const worker = new AutonomousWorker(
    'codex',
    {},
    {},
    {},
    {
      contextAssembler: {},
      fileSummaryCache: {},
      sharedContextPool: {
        getByMission() {
          return [];
        },
        add() {},
      },
    },
  );

  let recoveryTouched = false;
  let questionTouched = false;

  worker.executeTodo = async (todo) => {
    todo.status = 'failed';
    todo.output = {
      success: false,
      summary: '',
      modifiedFiles: [],
      error: 'LLM 响应为空：流式传输完成但未收到有效内容',
      duration: 1,
    };
    return {
      success: false,
      todo,
      error: todo.output.error,
    };
  };
  worker.planRecovery = async () => {
    recoveryTouched = true;
    return {
      strategy: 'retry_same_worker',
      reason: 'regression-test',
      confidence: 0.9,
      maxRetries: 1,
    };
  };
  worker.executeRecovery = async () => {
    recoveryTouched = true;
    return { success: false };
  };
  worker.reportQuestion = async () => {
    questionTouched = true;
    return { action: 'continue', timestamp: Date.now() };
  };

  const assignment = {
    id: `ff-assignment-${Date.now()}`,
    missionId: 'ff-mission',
    workerId: 'codex',
    shortTitle: 'fail-fast',
    responsibility: 'verify fail-fast',
    delegationBriefing: 'verify fail-fast',
    assignmentReason: {},
    scope: {
      includes: ['fail-fast'],
      excludes: [],
      scopeHints: [],
      targetPaths: [],
      requiresModification: false,
    },
    guidancePrompt: '',
    producerContracts: [],
    consumerContracts: [],
    todos: [
      {
        id: 'todo-1',
        assignmentId: 'ff-assignment',
        missionId: 'ff-mission',
        content: 'trigger model issue',
        reasoning: 'regression',
        expectedOutput: 'fast fail',
        type: 'analysis',
        priority: 2,
        status: 'pending',
        dependsOn: [],
        requiredContracts: [],
      },
    ],
    planningStatus: 'pending',
    status: 'pending',
    progress: 0,
    createdAt: Date.now(),
  };

  const reports = [];
  const result = await worker.executeAssignment(assignment, {
    workingDirectory: ROOT,
    adapterFactory: {
      isDeepTask: () => false,
      getToolManager: () => ({
        updateSnapshotTodoId() {},
      }),
    },
    preAssembledContext: {
      budgetUsage: 0,
      availableBudget: 1000,
      entries: [],
      usageBySource: {},
      truncatedEntries: [],
      totalEntries: 0,
    },
    onReport: async (report) => {
      reports.push(report?.type || 'unknown');
      return { action: 'continue', timestamp: Date.now() };
    },
    reportTimeout: 2000,
  });

  assert(result.success === false, '模型侧错误应导致 assignment 失败');
  assert(result.errors.length > 0, '失败结果应包含错误信息');
  assert(
    result.errors.some((item) => String(item).includes('未返回可执行内容')),
    `错误信息应为归一化模型文案: ${JSON.stringify(result.errors)}`,
  );
  assert(result.errors.length === 1, `模型失败结果不应重复写入 errors: ${JSON.stringify(result.errors)}`);
  assert(recoveryTouched === true, '模型侧错误应进入恢复链路');
  assert(questionTouched === true, '模型侧错误恢复失败后应进入 question 上报链路');
  assert(reports.includes('failed'), `最终应回传 failed 报告，实际: ${reports.join(',')}`);

  return {
    success: result.success,
    errors: result.errors,
    reports,
  };
}

async function testWorkerPipelineCancelInterrupt() {
  const { WorkerPipeline } = loadCompiledModule(path.join('orchestrator', 'core', 'worker-pipeline.js'));
  const { CancellationToken } = loadCompiledModule(path.join('orchestrator', 'core', 'dispatch-batch.js'));

  const pipeline = new WorkerPipeline();
  const cancellationToken = new CancellationToken();
  const interruptCalls = [];

  const assignment = {
    id: 'pipe-assignment',
    missionId: 'pipe-mission',
    workerId: 'codex',
    scope: {
      targetPaths: [],
      requiresModification: false,
    },
    guidancePrompt: '',
    todos: [],
  };

  const adapterFactory = {
    interrupt(worker) {
      interruptCalls.push(worker);
      return Promise.resolve();
    },
    getToolManager() {
      return {
        setSnapshotContext() {},
        clearSnapshotContext() {},
      };
    },
  };

  const workerInstance = {
    async executeAssignment() {
      await wait(80);
      return {
        assignment,
        success: false,
        completedTodos: [],
        failedTodos: [],
        skippedTodos: [],
        dynamicTodos: [],
        recoveredTodos: [],
        totalDuration: 80,
        errors: ['cancelled'],
        recoveryAttempts: 0,
        summary: 'cancelled',
        verification: {
          attempted: false,
          degraded: false,
          warnings: [],
          rounds: 0,
        },
      };
    },
  };

  const running = pipeline.execute({
    assignment,
    workerInstance,
    adapterFactory,
    workspaceRoot: ROOT,
    cancellationToken,
    enableSnapshot: false,
    enableLSP: false,
    enableTargetEnforce: false,
    enableContextUpdate: false,
  });

  setTimeout(() => cancellationToken.cancel('regression-cancel'), 10);
  await running;

  assert(interruptCalls.length >= 1, '取消后应触发 adapterFactory.interrupt');
  assert(interruptCalls.includes('codex'), `interrupt worker 异常: ${interruptCalls.join(',')}`);

  return {
    interruptCalls,
  };
}

function testSourceGuardrails() {
  const adapterFactorySource = fs.readFileSync(
    path.join(ROOT, 'src', 'llm', 'adapter-factory.ts'),
    'utf8',
  );
  const workerSource = fs.readFileSync(
    path.join(ROOT, 'src', 'orchestrator', 'worker', 'autonomous-worker.ts'),
    'utf8',
  );
  const openAiSource = fs.readFileSync(
    path.join(ROOT, 'src', 'llm', 'protocol', 'adapters', 'openai-responses-adapter.ts'),
    'utf8',
  );
  const anthropicSource = fs.readFileSync(
    path.join(ROOT, 'src', 'llm', 'protocol', 'adapters', 'anthropic-messages-adapter.ts'),
    'utf8',
  );

  assert(
    !adapterFactorySource.includes('[10000, 20000, 30000]'),
    'AdapterFactory 不应保留 10/20/30 秒长重试延迟',
  );
  assert(
    workerSource.includes('Worker.Todo.模型侧失败.进入恢复链路'),
    'Worker 模型侧恢复链路守卫缺失',
  );
  assert(
    workerSource.includes('Worker.question上报失败，按降级继续'),
    'question 上报降级继续守卫缺失',
  );
  assert(
    !workerSource.includes('Worker.Todo.模型侧失败.快速终止当前任务'),
    'Worker 仍保留模型侧快速终止旧逻辑',
  );
  assert(
    !workerSource.includes("return { action: 'abort', abortReason: reason, timestamp: Date.now() };"),
    'question 上报失败后仍存在 abort 旧逻辑',
  );
  assert(
    openAiSource.includes('responses.create(requestParams, { signal: request.signal })'),
    'OpenAI 非流式请求未透传 signal',
  );
  assert(
    anthropicSource.includes('signal: request.signal'),
    'Anthropic 非流式请求未透传 signal',
  );
}

async function main() {
  testSourceGuardrails();

  const timeoutResult = await testUniversalClientTimeoutFailFast();
  const networkResult = await testUniversalClientNetworkRetry();
  const authResult = await testUniversalClientAuthDeterministicFastStop();
  const circuitResult = await testUniversalClientCircuitBreakerWindow();
  const workerResult = await testAutonomousWorkerModelResilience();
  const pipelineResult = await testWorkerPipelineCancelInterrupt();

  console.log('\n=== Worker Fail-Fast 回归结果 ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'source_guardrails',
      'timeout_fail_fast',
      'network_retry_budget',
      'auth_deterministic_fast_stop',
      'circuit_breaker_window',
      'worker_model_error_resilience',
      'pipeline_cancel_interrupt',
    ],
    timeoutResult,
    networkResult,
    authResult,
    circuitResult,
    workerResult,
    pipelineResult,
  }, null, 2));
  process.exit(0);
}

main().catch((error) => {
  console.error('Worker Fail-Fast 回归失败:', error?.stack || error);
  process.exit(1);
});
