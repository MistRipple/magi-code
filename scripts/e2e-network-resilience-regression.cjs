#!/usr/bin/env node
/**
 * 网络韧性回归脚本
 *
 * 覆盖目标：
 * 1) 统一网络底座：重试/状态码/超时信号合并
 * 2) Web 工具：web_search / web_fetch 抖动下行为
 * 3) Skills HTTP：幂等与非幂等重试边界
 * 4) MCP：streamable-http 的请求头合并与重试边界
 * 5) 模型相关网络调用：连接测试与模型列表拉取
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

async function waitForAbort(signal, timeoutMs, label) {
  if (signal.aborted) {
    return;
  }
  await Promise.race([
    new Promise((resolve) => signal.addEventListener('abort', resolve, { once: true })),
    wait(timeoutMs).then(() => {
      throw new Error(`等待 Abort 超时: ${label}`);
    }),
  ]);
}

function loadCompiledModule(relPath) {
  const abs = path.join(OUT, relPath);
  if (!fs.existsSync(abs)) {
    throw new Error(`缺少编译产物: ${abs}，请先执行 npm run compile`);
  }
  return require(abs);
}

async function withPatchedFetch(fakeFetch, fn) {
  const originalFetch = global.fetch;
  global.fetch = fakeFetch;
  try {
    return await fn();
  } finally {
    global.fetch = originalFetch;
  }
}

async function withPatchedAbortSignalAny(fakeAny, fn) {
  const originalAny = AbortSignal.any;
  Object.defineProperty(AbortSignal, 'any', {
    value: fakeAny,
    configurable: true,
    writable: true,
  });
  try {
    return await fn();
  } finally {
    Object.defineProperty(AbortSignal, 'any', {
      value: originalAny,
      configurable: true,
      writable: true,
    });
  }
}

async function testNetworkUtilsRetry() {
  const { fetchWithRetry } = loadCompiledModule(path.join('tools', 'network-utils.js'));

  await withPatchedFetch(async () => {
    throw new Error('fetch failed');
  }, async () => {
    let calls = 0;
    await withPatchedFetch(async () => {
      calls += 1;
      if (calls < 3) {
        throw new Error('fetch failed');
      }
      return new Response('ok', {
        status: 200,
        headers: { 'content-type': 'text/plain' },
      });
    }, async () => {
      const response = await fetchWithRetry(
        'https://example.com',
        { method: 'GET' },
        { timeoutMs: 200, attempts: 3, baseDelayMs: 1, maxDelayMs: 3 },
      );
      assert(response.status === 200, 'fetchWithRetry 网络抖动后应成功');
      assert(calls === 3, `fetchWithRetry 网络抖动重试次数异常: ${calls}`);
    });
  });
}

async function testNetworkUtilsRetryOnStatus() {
  const { fetchWithRetry } = loadCompiledModule(path.join('tools', 'network-utils.js'));

  let calls = 0;
  await withPatchedFetch(async () => {
    calls += 1;
    if (calls === 1) {
      return new Response('busy', { status: 503, statusText: 'Service Unavailable' });
    }
    return new Response('ok', { status: 200 });
  }, async () => {
    const response = await fetchWithRetry(
      'https://example.com/status',
      { method: 'GET' },
      { timeoutMs: 200, attempts: 2, baseDelayMs: 1, maxDelayMs: 3 },
    );
    assert(response.status === 200, '503 后应重试并成功');
    assert(calls === 2, `503 重试次数异常: ${calls}`);
  });
}

async function testCombineSignalFallback() {
  const { combineSignalWithTimeout } = loadCompiledModule(path.join('tools', 'network-utils.js'));

  await withPatchedAbortSignalAny(undefined, async () => {
    const externalController = new AbortController();
    const mergedSignal = combineSignalWithTimeout(externalController.signal, 20);
    await waitForAbort(mergedSignal, 300, 'combineSignalWithTimeout fallback timeout');
    assert(mergedSignal.aborted, 'fallback 合并信号应在超时后中断');
  });
}

async function testWebExecutorResilience() {
  const { WebExecutor } = loadCompiledModule(path.join('tools', 'web-executor.js'));
  const executor = new WebExecutor();

  let fetchCalls = 0;
  await withPatchedFetch(async () => {
    fetchCalls += 1;
    if (fetchCalls === 1) {
      throw new Error('fetch failed');
    }
    return new Response('<html><title>Example</title><body>Hello</body></html>', {
      status: 200,
      headers: { 'content-type': 'text/html' },
    });
  }, async () => {
    const result = await executor.execute({
      id: 'web-fetch-1',
      name: 'web_fetch',
      arguments: { url: 'https://example.com' },
    });
    assert(result.isError === false, `web_fetch 网络抖动后应成功: ${String(result.content).slice(0, 160)}`);
    assert(fetchCalls === 2, `web_fetch 重试次数异常: ${fetchCalls}`);
  });

  await withPatchedFetch(async () => {
    throw new Error('fetch failed');
  }, async () => {
    const result = await executor.execute({
      id: 'web-search-1',
      name: 'web_search',
      arguments: { query: 'magi network resilience regression query' },
    });
    assert(result.isError === true, '搜索源全部网络失败时应返回错误');
    assert(
      String(result.content).includes('upstream connectivity issues'),
      `搜索失败文案应归因为上游连接问题: ${String(result.content)}`,
    );
  });

  await withPatchedFetch(async (url) => {
    const rawUrl = String(url);
    if (rawUrl.includes('duckduckgo.com')) {
      return new Response('busy', { status: 503, statusText: 'Service Unavailable' });
    }
    if (rawUrl.startsWith('https://help.bcgsoft.com')) {
      return new Response('<html><title>BCG Help</title></html>', {
        status: 200,
        headers: { 'content-type': 'text/html' },
      });
    }
    throw new Error(`unexpected url: ${rawUrl}`);
  }, async () => {
    const result = await executor.execute({
      id: 'web-search-2',
      name: 'web_search',
      arguments: { query: 'help.bcgsoft.com' },
    });
    assert(result.isError === false, '搜索源失败但目标站可达时应直连兜底成功');
    assert(
      String(result.content).includes('direct fallback'),
      `应返回直连兜底结果: ${String(result.content)}`,
    );
  });
}

async function testSkillsManagerHttpRetryBoundary() {
  const { SkillsManager } = loadCompiledModule(path.join('tools', 'skills-manager.js'));

  const manager = new SkillsManager({
    customTools: [
      {
        name: 'network_get_test',
        description: 'GET 网络回归测试',
        input_schema: { type: 'object', properties: {} },
        executor: {
          type: 'http',
          url: 'https://skill.example.com/get',
          method: 'GET',
          timeoutMs: 500,
        },
      },
      {
        name: 'network_post_test',
        description: 'POST 网络回归测试',
        input_schema: { type: 'object', properties: {} },
        executor: {
          type: 'http',
          url: 'https://skill.example.com/post',
          method: 'POST',
          timeoutMs: 500,
        },
      },
    ],
    instructionSkills: [],
  });

  let getCalls = 0;
  await withPatchedFetch(async () => {
    getCalls += 1;
    if (getCalls === 1) {
      throw new Error('fetch failed');
    }
    return new Response('ok', {
      status: 200,
      headers: { 'content-type': 'text/plain' },
    });
  }, async () => {
    const result = await manager.execute({
      id: 'skill-get-1',
      name: 'network_get_test',
      arguments: {},
    });
    assert(result.isError === false, `GET 工具应在重试后成功: ${result.content}`);
    assert(getCalls === 2, `GET 工具重试次数异常: ${getCalls}`);
  });

  let postCalls = 0;
  await withPatchedFetch(async () => {
    postCalls += 1;
    throw new Error('fetch failed');
  }, async () => {
    const result = await manager.execute({
      id: 'skill-post-1',
      name: 'network_post_test',
      arguments: { a: 1 },
    });
    assert(result.isError === true, 'POST 工具失败时应返回错误');
    assert(postCalls === 1, `POST 工具不应自动重试，当前次数: ${postCalls}`);
    assert(
      String(result.content).includes('network upstream unstable'),
      `POST 工具错误文案应提示上游网络抖动: ${result.content}`,
    );
  });
}

async function testUniversalClientModelListRetry() {
  const { UniversalLLMClient } = loadCompiledModule(path.join('llm', 'clients', 'universal-client.js'));

  const client = new UniversalLLMClient({
    provider: 'openai',
    baseUrl: 'https://api.example.com',
    apiKey: 'test-key',
    model: 'gpt-4o-mini',
    enabled: true,
  });

  let calls = 0;
  await withPatchedFetch(async () => {
    calls += 1;
    if (calls === 1) {
      return new Response('busy', { status: 503, statusText: 'Service Unavailable' });
    }
    return new Response(JSON.stringify({
      data: [{ id: 'gpt-4o-mini' }, { id: 'gpt-4.1-mini' }],
    }), {
      status: 200,
      headers: { 'content-type': 'application/json' },
    });
  }, async () => {
    const result = await client.testConnectionFast();
    assert(result.success === true, `模型连接测试应在重试后成功: ${JSON.stringify(result)}`);
    assert(result.modelExists === true, '应识别到目标模型存在');
    assert(calls === 2, `模型连接测试重试次数异常: ${calls}`);
  });
}

async function testConfigHandlerModelListRetry() {
  const { ConfigCommandHandler } = loadCompiledModule(path.join('ui', 'handlers', 'config-handler.js'));

  const handler = new ConfigCommandHandler();
  const sentData = [];
  const toasts = [];
  const fakeCtx = {
    sendData(type, payload) {
      sentData.push({ type, payload });
    },
    sendToast(message, level) {
      toasts.push({ message, level });
    },
    sendStateUpdate() {},
    getAdapterFactory() { return {}; },
    getOrchestratorEngine() { return { reloadCompressionAdapter: async () => undefined }; },
    getProjectKnowledgeBase() { return undefined; },
    getWorkspaceRoot() { return ROOT; },
    getPromptEnhancer() {
      return {
        enhance: async (prompt) => ({ enhancedPrompt: prompt, error: '' }),
      };
    },
    getExtensionUri() { return { fsPath: ROOT, path: ROOT, toString: () => ROOT }; },
  };

  let calls = 0;
  await withPatchedFetch(async () => {
    calls += 1;
    if (calls === 1) {
      throw new Error('fetch failed');
    }
    return new Response(JSON.stringify({
      data: [{ id: 'magi-model-a' }, { id: 'magi-model-b' }],
    }), {
      status: 200,
      headers: { 'content-type': 'application/json' },
    });
  }, async () => {
    await handler.handle({
      type: 'fetchModelList',
      target: 'orchestrator',
      config: {
        baseUrl: 'https://api.example.com',
        apiKey: 'test-key',
      },
    }, fakeCtx);
  });

  assert(calls === 2, `配置页模型列表请求重试次数异常: ${calls}`);
  const result = sentData.find((item) => item.type === 'modelListFetched');
  assert(result, '配置页应发送 modelListFetched 消息');
  assert(result.payload.success === true, `配置页模型列表应成功: ${JSON.stringify(result.payload)}`);
  assert(Array.isArray(result.payload.models) && result.payload.models.length === 2, '配置页模型列表数据异常');
  assert(toasts.some((item) => String(item.message).includes('获取到 2 个模型')), '配置页成功提示缺失');
}

async function testMcpStreamableFetchWrapper() {
  const { MCPManager } = loadCompiledModule(path.join('tools', 'mcp-manager.js'));
  const manager = new MCPManager();

  const transport = manager.buildStreamableTransport({
    id: 'network-test-mcp',
    name: 'network-test-mcp',
    type: 'streamable-http',
    enabled: true,
    url: 'https://mcp.example.com',
    headers: {
      Authorization: 'Bearer token123',
    },
  });

  const wrappedFetch = transport._fetch;
  assert(typeof wrappedFetch === 'function', 'streamable-http 包装 fetch 未注入');

  let getCalls = 0;
  const captured = [];
  await withPatchedFetch(async (_url, init) => {
    getCalls += 1;
    const headers = new Headers(init?.headers);
    captured.push({
      accept: headers.get('Accept'),
      authorization: headers.get('Authorization'),
    });
    if (getCalls === 1) {
      throw new Error('fetch failed');
    }
    return new Response('ok', { status: 200 });
  }, async () => {
    const response = await wrappedFetch('https://mcp.example.com/stream', {
      method: 'GET',
      headers: new Headers({ Accept: 'text/event-stream' }),
    });
    assert(response.status === 200, 'streamable GET 应在重试后成功');
  });

  assert(getCalls === 2, `streamable GET 重试次数异常: ${getCalls}`);
  assert(captured.length === 2, `streamable GET 抓取请求头次数异常: ${captured.length}`);
  assert(captured.every((item) => item.accept === 'text/event-stream'), 'streamable GET 应保留 SDK Accept 头');
  assert(captured.every((item) => item.authorization === 'Bearer token123'), 'streamable GET 应保留配置 Authorization 头');

  let postCalls = 0;
  await withPatchedFetch(async () => {
    postCalls += 1;
    throw new Error('fetch failed');
  }, async () => {
    let failed = false;
    try {
      await wrappedFetch('https://mcp.example.com/message', {
        method: 'POST',
        headers: new Headers({ 'Content-Type': 'application/json' }),
        body: '{"jsonrpc":"2.0"}',
      });
    } catch (error) {
      failed = true;
      assert(String(error.message).includes('[MCP_STREAMABLE_FETCH]'), 'streamable POST 错误前缀异常');
    }
    assert(failed, 'streamable POST 网络失败时应抛错');
  });
  assert(postCalls === 1, `streamable POST 不应自动重试，当前次数: ${postCalls}`);
}

async function main() {
  const checks = [];

  await testNetworkUtilsRetry();
  checks.push('network_utils_retry');

  await testNetworkUtilsRetryOnStatus();
  checks.push('network_utils_status_retry');

  await testCombineSignalFallback();
  checks.push('network_utils_signal_fallback');

  await testWebExecutorResilience();
  checks.push('web_executor_resilience');

  await testSkillsManagerHttpRetryBoundary();
  checks.push('skills_http_retry_boundary');

  await testUniversalClientModelListRetry();
  checks.push('universal_client_models_retry');

  await testConfigHandlerModelListRetry();
  checks.push('config_handler_models_retry');

  await testMcpStreamableFetchWrapper();
  checks.push('mcp_streamable_fetch_retry_and_headers');

  console.log('\n=== 网络韧性回归结果 ===');
  console.log(JSON.stringify({
    pass: true,
    totalChecks: checks.length,
    checks,
  }, null, 2));
}

main().catch((error) => {
  console.error('网络韧性回归失败:', error?.stack || error);
  process.exit(1);
});
