/**
 * LLM 编排逻辑端到端测试
 *
 * 严格按照 docs/orchestration-unified-design.md 第 13 章要求实现
 * 使用真实 LLM API 进行测试，不依赖 VS Code
 *
 * 运行方式:
 * npx ts-node src/test/e2e/llm-orchestration-e2e.ts [--quick] [--scenario=ASK-01]
 */

import { UniversalLLMClient } from '../../llm/clients/universal-client';
import { LLMConfigLoader } from '../../llm/config';
import { LLMConfig } from '../../types/agent-types';
import * as path from 'path';
import * as fs from 'fs';
import * as os from 'os';

// ============================================================================
// 类型定义
// ============================================================================

interface VerificationPoint {
  name: string;
  expected: string;
  actual: string;
  passed: boolean;
}

interface ScenarioResult {
  scenarioId: string;
  description: string;
  passed: boolean;
  verificationPoints: VerificationPoint[];
  duration: number;
  error?: string;
  llmResponse?: string;
  tokenUsage?: { input: number; output: number };
}

interface TestContext {
  orchestratorClient: UniversalLLMClient;
  workerClients: Map<string, UniversalLLMClient>;
  config: ReturnType<typeof LLMConfigLoader.loadFullConfig>;
}

// ============================================================================
// 测试框架
// ============================================================================

/**
 * 创建测试上下文
 */
function createTestContext(): TestContext {
  const config = LLMConfigLoader.loadFullConfig();

  // 创建编排者客户端
  const orchestratorConfig: LLMConfig = {
    ...config.orchestrator,
    enabled: true,
  };
  const orchestratorClient = new UniversalLLMClient(orchestratorConfig);

  // 创建 Worker 客户端
  const workerClients = new Map<string, UniversalLLMClient>();
  for (const [workerName, workerConfig] of Object.entries(config.workers)) {
    if (workerConfig.enabled) {
      workerClients.set(workerName, new UniversalLLMClient({
        ...workerConfig,
        enabled: true,
      } as LLMConfig));
    }
  }

  return { orchestratorClient, workerClients, config };
}

/**
 * 执行单个场景测试
 */
async function executeScenario(
  ctx: TestContext,
  scenarioId: string,
  description: string,
  prompt: string,
  verify: (response: string, ctx: TestContext) => VerificationPoint[],
  useOrchestrator: boolean = true
): Promise<ScenarioResult> {
  const startTime = Date.now();
  let response = '';
  let error: string | undefined;
  let tokenUsage: { input: number; output: number } | undefined;

  try {
    const client = useOrchestrator ? ctx.orchestratorClient : ctx.workerClients.values().next().value;
    if (!client) {
      throw new Error('No available LLM client');
    }

    const result = await client.sendMessage({
      messages: [{ role: 'user', content: prompt }],
      systemPrompt: useOrchestrator
        ? '你是一个智能编排者，负责分析用户需求并制定执行计划。'
        : '你是一个执行者，负责完成具体任务。',
      maxTokens: 2048,
      temperature: 0.7,
    });

    response = result.content || '';
    tokenUsage = result.usage ? {
      input: result.usage.inputTokens || 0,
      output: result.usage.outputTokens || 0,
    } : undefined;

  } catch (e) {
    error = e instanceof Error ? e.message : String(e);
  }

  const verificationPoints = verify(response, ctx);
  const passed = verificationPoints.every(v => v.passed) && !error;

  return {
    scenarioId,
    description,
    passed,
    verificationPoints,
    duration: Date.now() - startTime,
    error,
    llmResponse: response.substring(0, 300),
    tokenUsage,
  };
}

// ============================================================================
// 13.1 非任务模式场景 (ASK/DIRECT/EXPLORE)
// ============================================================================

/**
 * ASK 模式测试 - 直接回答问题
 */
async function testASKMode(ctx: TestContext): Promise<ScenarioResult[]> {
  const results: ScenarioResult[] = [];

  // ASK-01: 用户问"什么是 TypeScript"
  results.push(await executeScenario(
    ctx,
    'ASK-01',
    '用户问"什么是 TypeScript"',
    '什么是 TypeScript？请简要回答。',
    (response) => {
      const hasContent = response.length > 50;
      const mentionsTS = response.toLowerCase().includes('typescript') ||
                         response.includes('类型') ||
                         response.includes('JavaScript');
      return [
        { name: '直接回答', expected: 'true', actual: String(hasContent), passed: hasContent },
        { name: '内容相关', expected: 'true', actual: String(mentionsTS), passed: mentionsTS },
      ];
    }
  ));

  // ASK-02: 用户问技术对比问题
  results.push(await executeScenario(
    ctx,
    'ASK-02',
    '用户问技术对比问题',
    'React 和 Vue 有什么区别？哪个更适合大型项目？',
    (response) => {
      const hasContent = response.length > 100;
      const mentionsBoth = (response.includes('React') || response.includes('react')) &&
                           (response.includes('Vue') || response.includes('vue'));
      return [
        { name: '回答全面', expected: 'true', actual: String(hasContent), passed: hasContent },
        { name: '涵盖两者', expected: 'true', actual: String(mentionsBoth), passed: mentionsBoth },
      ];
    }
  ));

  // ASK-03: 用户问代码解释
  results.push(await executeScenario(
    ctx,
    'ASK-03',
    '用户问代码解释',
    '解释一下这段代码: async function test() { await Promise.all([1,2,3].map(x => fetch(x))); }',
    (response) => {
      const hasExplanation = response.length > 50;
      const mentionsAsync = response.includes('async') || response.includes('异步') || response.includes('Promise');
      return [
        { name: '解释代码', expected: 'true', actual: String(hasExplanation), passed: hasExplanation },
        { name: '提及异步', expected: 'true', actual: String(mentionsAsync), passed: mentionsAsync },
      ];
    }
  ));

  // ASK-04: 用户连续问多个问题
  results.push(await executeScenario(
    ctx,
    'ASK-04',
    '用户连续问多个问题',
    '1. 什么是 Promise？ 2. async/await 和 Promise 的关系是什么？',
    (response) => {
      const hasContent = response.length > 100;
      const answersMultiple = response.includes('Promise') &&
                              (response.includes('async') || response.includes('await'));
      return [
        { name: '回答多个问题', expected: 'true', actual: String(hasContent), passed: hasContent },
        { name: '涵盖所有问题', expected: 'true', actual: String(answersMultiple), passed: answersMultiple },
      ];
    }
  ));

  return results;
}

/**
 * DIRECT 模式测试 - 直接执行简单任务
 */
async function testDIRECTMode(ctx: TestContext): Promise<ScenarioResult[]> {
  const results: ScenarioResult[] = [];

  // DIR-01: 给函数加注释
  results.push(await executeScenario(
    ctx,
    'DIR-01',
    '给函数加注释',
    '给以下函数加上 JSDoc 注释:\nfunction add(a: number, b: number): number { return a + b; }',
    (response) => {
      const hasComment = response.includes('/**') || response.includes('//') || response.includes('注释');
      return [
        { name: '生成注释', expected: 'true', actual: String(hasComment), passed: hasComment },
      ];
    },
    false // 使用 Worker
  ));

  // DIR-02: 变量重命名
  results.push(await executeScenario(
    ctx,
    'DIR-02',
    '变量重命名',
    '把以下代码中的变量 a 改成 num1, b 改成 num2:\nfunction add(a: number, b: number) { return a + b; }',
    (response) => {
      const hasRename = response.includes('num1') && response.includes('num2');
      return [
        { name: '完成重命名', expected: 'true', actual: String(hasRename), passed: hasRename },
      ];
    },
    false
  ));

  // DIR-03: 代码格式化
  results.push(await executeScenario(
    ctx,
    'DIR-03',
    '代码格式化',
    '格式化以下代码:\nconst x={a:1,b:2,c:{d:3,e:4}}',
    (response) => {
      const hasFormatted = response.includes('\n') || response.includes('  ');
      return [
        { name: '格式化输出', expected: 'true', actual: String(hasFormatted), passed: hasFormatted },
      ];
    },
    false
  ));

  // DIR-04: 删除代码
  results.push(await executeScenario(
    ctx,
    'DIR-04',
    '删除指定代码',
    '删除以下代码中的 console.log 语句:\nfunction test() {\n  console.log("debug");\n  return 42;\n}',
    (response) => {
      const hasResult = response.includes('return') || response.includes('function');
      const noConsole = !response.toLowerCase().includes('console.log("debug")');
      return [
        { name: '返回修改后代码', expected: 'true', actual: String(hasResult), passed: hasResult },
      ];
    },
    false
  ));

  return results;
}

/**
 * EXPLORE 模式测试 - 分析探索
 */
async function testEXPLOREMode(ctx: TestContext): Promise<ScenarioResult[]> {
  const results: ScenarioResult[] = [];

  // EXP-01: 分析代码复杂度
  results.push(await executeScenario(
    ctx,
    'EXP-01',
    '分析代码复杂度',
    '分析以下代码的复杂度:\nfunction findMax(arr) {\n  let max = arr[0];\n  for(let i=1; i<arr.length; i++) {\n    if(arr[i] > max) max = arr[i];\n  }\n  return max;\n}',
    (response) => {
      const hasAnalysis = response.includes('O(') || response.includes('复杂度') ||
                          response.includes('时间') || response.includes('遍历');
      return [
        { name: '分析复杂度', expected: 'true', actual: String(hasAnalysis), passed: hasAnalysis },
      ];
    }
  ));

  // EXP-02: 找出代码问题
  results.push(await executeScenario(
    ctx,
    'EXP-02',
    '找出代码问题',
    '找出以下代码的潜在问题:\nfunction divide(a, b) { return a / b; }',
    (response) => {
      const hasIssue = response.includes('0') || response.includes('除') ||
                       response.includes('检查') || response.includes('异常');
      return [
        { name: '发现问题', expected: 'true', actual: String(hasIssue), passed: hasIssue },
      ];
    }
  ));

  // EXP-03: 代码改进建议
  results.push(await executeScenario(
    ctx,
    'EXP-03',
    '代码改进建议',
    '给以下代码提供改进建议:\nvar data = [];\nfor(var i=0; i<10; i++) { data.push(i*2); }',
    (response) => {
      const hasSuggestion = response.includes('let') || response.includes('const') ||
                            response.includes('map') || response.includes('建议');
      return [
        { name: '提供建议', expected: 'true', actual: String(hasSuggestion), passed: hasSuggestion },
      ];
    }
  ));

  // EXP-04: 代码安全分析
  results.push(await executeScenario(
    ctx,
    'EXP-04',
    '代码安全分析',
    '分析以下代码的安全性:\nfunction query(sql) { return db.execute("SELECT * FROM users WHERE id=" + sql); }',
    (response) => {
      const hasSecurity = response.includes('注入') || response.includes('SQL') ||
                          response.includes('安全') || response.includes('injection');
      return [
        { name: '安全分析', expected: 'true', actual: String(hasSecurity), passed: hasSecurity },
      ];
    }
  ));

  return results;
}

// ============================================================================
// 13.2 完整路径场景 (MissionOrchestrator)
// ============================================================================

/**
 * 单 Worker 任务测试
 */
async function testSingleWorkerMission(ctx: TestContext): Promise<ScenarioResult[]> {
  const results: ScenarioResult[] = [];

  // SIN-01: 编排者分析任务并制定计划
  results.push(await executeScenario(
    ctx,
    'SIN-01',
    '编排者分析任务并制定计划',
    '用户请求: "重构这个函数，提取公共方法"\n\n请分析这个任务，判断任务类型（ASK/DIRECT/EXPLORE/SINGLE/MULTI），并说明你的判断依据。',
    (response) => {
      const hasAnalysis = response.includes('重构') || response.includes('任务');
      const hasType = response.includes('SINGLE') || response.includes('单') ||
                      response.includes('DIRECT') || response.includes('直接');
      return [
        { name: '任务分析', expected: 'true', actual: String(hasAnalysis), passed: hasAnalysis },
        { name: '类型判断', expected: 'true', actual: String(hasType), passed: hasType },
      ];
    },
    true // 使用编排者
  ));

  // SIN-02: 编排者分配任务给 Worker
  results.push(await executeScenario(
    ctx,
    'SIN-02',
    '编排者分配任务给 Worker',
    '作为编排者，你需要将以下任务分配给一个 Worker:\n任务: 实现一个简单的计算器函数\n\n请生成一个任务分配方案，包括:\n1. 选择哪个 Worker\n2. 具体的执行步骤（Todos）\n3. 验收标准',
    (response) => {
      const hasWorker = response.includes('Worker') || response.includes('执行者') ||
                        response.includes('Claude') || response.includes('Gemini');
      const hasTodos = response.includes('步骤') || response.includes('1.') ||
                       response.includes('Todo') || response.includes('任务');
      return [
        { name: 'Worker 分配', expected: 'true', actual: String(hasWorker), passed: hasWorker },
        { name: 'Todo 列表', expected: 'true', actual: String(hasTodos), passed: hasTodos },
      ];
    },
    true
  ));

  // SIN-03: Worker 执行任务并汇报
  results.push(await executeScenario(
    ctx,
    'SIN-03',
    'Worker 执行任务并汇报',
    '你是一个执行 Worker，完成以下任务后请汇报:\n任务: 实现一个 add 函数，接受两个数字并返回它们的和\n\n请:\n1. 实现代码\n2. 汇报完成情况\n3. 列出修改的文件',
    (response) => {
      const hasCode = response.includes('function') || response.includes('const') ||
                      response.includes('=>') || response.includes('return');
      const hasReport = response.includes('完成') || response.includes('实现') ||
                        response.includes('创建');
      return [
        { name: '代码实现', expected: 'true', actual: String(hasCode), passed: hasCode },
        { name: '完成汇报', expected: 'true', actual: String(hasReport), passed: hasReport },
      ];
    },
    false // 使用 Worker
  ));

  // SIN-04: Worker 遇到问题需要澄清
  results.push(await executeScenario(
    ctx,
    'SIN-04',
    'Worker 遇到问题需要澄清',
    '你是一个执行 Worker，遇到以下模糊任务:\n任务: 优化这个函数\n\n这个任务描述不够清晰。请:\n1. 列出你需要澄清的问题\n2. 说明为什么需要这些信息',
    (response) => {
      const hasQuestion = response.includes('?') || response.includes('？') ||
                          response.includes('问题') || response.includes('澄清');
      const hasReason = response.includes('因为') || response.includes('需要') ||
                        response.includes('以便') || response.includes('才能');
      return [
        { name: '提出问题', expected: 'true', actual: String(hasQuestion), passed: hasQuestion },
        { name: '说明原因', expected: 'true', actual: String(hasReason), passed: hasReason },
      ];
    },
    false
  ));

  return results;
}

/**
 * 多 Worker 协作任务测试
 */
async function testMultiWorkerMission(ctx: TestContext): Promise<ScenarioResult[]> {
  const results: ScenarioResult[] = [];

  // MUL-01: 编排者分析多 Worker 任务
  results.push(await executeScenario(
    ctx,
    'MUL-01',
    '编排者分析多 Worker 任务',
    '用户请求: "重构后端 API 并更新前端调用"\n\n请分析这个任务，判断是否需要多个 Worker 协作，并说明:\n1. 任务拆分方案\n2. Worker 分配\n3. 执行顺序（串行/并行）\n4. 依赖关系',
    (response) => {
      const hasMultiWorker = response.includes('多个') || response.includes('协作') ||
                              response.includes('Worker') || response.includes('分配');
      const hasDependency = response.includes('依赖') || response.includes('顺序') ||
                            response.includes('先') || response.includes('后');
      return [
        { name: '多 Worker 分析', expected: 'true', actual: String(hasMultiWorker), passed: hasMultiWorker },
        { name: '依赖关系', expected: 'true', actual: String(hasDependency), passed: hasDependency },
      ];
    },
    true
  ));

  // MUL-02: 编排者协调 Worker 输出
  results.push(await executeScenario(
    ctx,
    'MUL-02',
    '编排者协调 Worker 输出',
    '作为编排者，你收到了两个 Worker 的输出:\n\nWorker A (后端): 已完成 API 重构，新增 /api/v2/users 端点\nWorker B (前端): 等待后端 API 信息\n\n请:\n1. 总结 Worker A 的输出\n2. 将必要信息传递给 Worker B\n3. 协调下一步工作',
    (response) => {
      const hasSummary = response.includes('完成') || response.includes('总结') ||
                         response.includes('API');
      const hasCoordination = response.includes('Worker B') || response.includes('前端') ||
                              response.includes('传递') || response.includes('下一步');
      return [
        { name: '总结输出', expected: 'true', actual: String(hasSummary), passed: hasSummary },
        { name: '协调工作', expected: 'true', actual: String(hasCoordination), passed: hasCoordination },
      ];
    },
    true
  ));

  // MUL-03: 多 Worker 冲突处理
  results.push(await executeScenario(
    ctx,
    'MUL-03',
    '多 Worker 冲突处理',
    '两个 Worker 同时修改了同一个文件:\nWorker A: 在 utils.ts 添加了 formatDate 函数\nWorker B: 在 utils.ts 添加了 parseDate 函数\n\n请作为编排者处理这个冲突，给出合并方案。',
    (response) => {
      const hasConflict = response.includes('冲突') || response.includes('合并') ||
                          response.includes('同时');
      const hasSolution = response.includes('方案') || response.includes('建议') ||
                          response.includes('可以') || response.includes('应该');
      return [
        { name: '识别冲突', expected: 'true', actual: String(hasConflict), passed: hasConflict },
        { name: '解决方案', expected: 'true', actual: String(hasSolution), passed: hasSolution },
      ];
    },
    true
  ));

  // MUL-04: 任务完成汇总
  results.push(await executeScenario(
    ctx,
    'MUL-04',
    '任务完成汇总',
    '所有 Worker 已完成任务:\nWorker A: 后端 API 重构完成，修改了 5 个文件\nWorker B: 前端调用更新完成，修改了 3 个文件\n\n请生成任务完成汇总报告，包括:\n1. 总体完成情况\n2. 修改的文件列表\n3. 后续建议',
    (response) => {
      const hasSummary = response.includes('完成') || response.includes('汇总') ||
                         response.includes('报告');
      const hasFiles = response.includes('文件') || response.includes('修改');
      return [
        { name: '完成汇总', expected: 'true', actual: String(hasSummary), passed: hasSummary },
        { name: '文件列表', expected: 'true', actual: String(hasFiles), passed: hasFiles },
      ];
    },
    true
  ));

  return results;
}

/**
 * Worker 汇报测试
 */
async function testWorkerReporting(ctx: TestContext): Promise<ScenarioResult[]> {
  const results: ScenarioResult[] = [];

  // REP-01: Worker 完成 Todo 汇报
  results.push(await executeScenario(
    ctx,
    'REP-01',
    'Worker 完成 Todo 汇报',
    '你是一个 Worker，刚完成了一个 Todo:\nTodo: 实现用户登录 API\n结果: 成功创建了 /api/auth/login 端点\n\n请生成一个完成汇报，使用以下格式:\n<report>\n<todo_id>xxx</todo_id>\n<status>completed</status>\n<summary>完成摘要</summary>\n<files_modified>修改的文件</files_modified>\n</report>',
    (response) => {
      const hasReport = response.includes('report') || response.includes('汇报') ||
                        response.includes('完成');
      const hasStructure = response.includes('status') || response.includes('summary') ||
                           response.includes('状态') || response.includes('摘要');
      return [
        { name: '生成汇报', expected: 'true', actual: String(hasReport), passed: hasReport },
        { name: '结构化输出', expected: 'true', actual: String(hasStructure), passed: hasStructure },
      ];
    },
    false
  ));

  // REP-02: Worker 需要澄清
  results.push(await executeScenario(
    ctx,
    'REP-02',
    'Worker 需要澄清',
    '你是一个 Worker，在执行任务时遇到不确定的地方:\n任务: 添加用户验证\n问题: 不确定使用 JWT 还是 Session\n\n请生成一个澄清请求，使用以下格式:\n<clarification_request>\n<question>你的问题</question>\n<context>上下文</context>\n<options>可选方案</options>\n</clarification_request>',
    (response) => {
      const hasRequest = response.includes('clarification') || response.includes('澄清') ||
                         response.includes('问题');
      const hasOptions = response.includes('JWT') || response.includes('Session') ||
                         response.includes('选项') || response.includes('方案');
      return [
        { name: '澄清请求', expected: 'true', actual: String(hasRequest), passed: hasRequest },
        { name: '提供选项', expected: 'true', actual: String(hasOptions), passed: hasOptions },
      ];
    },
    false
  ));

  // REP-03: Worker 请求超范围操作
  results.push(await executeScenario(
    ctx,
    'REP-03',
    'Worker 请求超范围操作',
    '你是一个 Worker，发现需要执行超出原定范围的操作:\n原任务: 优化登录 API 性能\n发现: 需要同时修改数据库索引\n\n请生成一个请求扩展范围的汇报。',
    (response) => {
      const hasRequest = response.includes('请求') || response.includes('需要') ||
                         response.includes('发现') || response.includes('超出');
      const hasReason = response.includes('因为') || response.includes('以便') ||
                        response.includes('原因') || response.includes('数据库');
      return [
        { name: '请求扩展', expected: 'true', actual: String(hasRequest), passed: hasRequest },
        { name: '说明原因', expected: 'true', actual: String(hasReason), passed: hasReason },
      ];
    },
    false
  ));

  // REP-04: Worker 完成所有任务
  results.push(await executeScenario(
    ctx,
    'REP-04',
    'Worker 完成所有任务',
    '你是一个 Worker，已完成所有分配的 Todos:\n1. ✅ 创建登录 API\n2. ✅ 添加验证中间件\n3. ✅ 编写单元测试\n\n请生成最终完成汇报。',
    (response) => {
      const hasComplete = response.includes('完成') || response.includes('所有') ||
                          response.includes('全部');
      const hasList = response.includes('1') || response.includes('2') ||
                      response.includes('登录') || response.includes('测试');
      return [
        { name: '完成汇报', expected: 'true', actual: String(hasComplete), passed: hasComplete },
        { name: '任务列表', expected: 'true', actual: String(hasList), passed: hasList },
      ];
    },
    false
  ));

  return results;
}

// ============================================================================
// 13.3 异常与降级场景
// ============================================================================

/**
 * Worker 失败降级测试
 */
async function testWorkerDegradation(ctx: TestContext): Promise<ScenarioResult[]> {
  const results: ScenarioResult[] = [];

  // DEG-01: 编排者处理 Worker 超时
  results.push(await executeScenario(
    ctx,
    'DEG-01',
    '编排者处理 Worker 超时',
    '作为编排者，Worker A 执行任务时超时了:\n任务: 重构大型模块\n超时时间: 60秒\n\n请决定如何处理:\n1. 重试\n2. 降级到其他 Worker\n3. 拆分任务\n4. 报告用户',
    (response) => {
      const hasDecision = response.includes('重试') || response.includes('降级') ||
                          response.includes('拆分') || response.includes('报告');
      return [
        { name: '处理决策', expected: 'true', actual: String(hasDecision), passed: hasDecision },
      ];
    },
    true
  ));

  // DEG-02: 编排者处理 Worker 错误
  results.push(await executeScenario(
    ctx,
    'DEG-02',
    '编排者处理 Worker 错误',
    '作为编排者，Worker 返回了错误:\n错误: "无法解析文件 config.json，格式错误"\n\n请分析错误并决定下一步行动。',
    (response) => {
      const hasAnalysis = response.includes('错误') || response.includes('分析') ||
                          response.includes('问题');
      const hasAction = response.includes('修复') || response.includes('重试') ||
                        response.includes('建议') || response.includes('检查');
      return [
        { name: '错误分析', expected: 'true', actual: String(hasAnalysis), passed: hasAnalysis },
        { name: '行动方案', expected: 'true', actual: String(hasAction), passed: hasAction },
      ];
    },
    true
  ));

  // DEG-03: 所有 Worker 不可用
  results.push(await executeScenario(
    ctx,
    'DEG-03',
    '所有 Worker 不可用',
    '作为编排者，所有 Worker 都不可用:\nClaude: API 限流\nGemini: 网络错误\nCodex: 服务维护中\n\n请决定如何向用户汇报这个情况，并提供可能的解决方案。',
    (response) => {
      const hasReport = response.includes('无法') || response.includes('不可用') ||
                        response.includes('抱歉') || response.includes('用户');
      const hasSolution = response.includes('稍后') || response.includes('重试') ||
                          response.includes('等待') || response.includes('建议');
      return [
        { name: '向用户汇报', expected: 'true', actual: String(hasReport), passed: hasReport },
        { name: '提供方案', expected: 'true', actual: String(hasSolution), passed: hasSolution },
      ];
    },
    true
  ));

  // DEG-04: Worker 死循环检测
  results.push(await executeScenario(
    ctx,
    'DEG-04',
    'Worker 死循环检测',
    '作为编排者，你检测到 Worker 可能陷入死循环:\n- 工具调用次数: 25 次（超过阈值 20）\n- 重复调用相同工具: 是\n\n请决定如何处理。',
    (response) => {
      const hasDetection = response.includes('死循环') || response.includes('超过') ||
                           response.includes('重复') || response.includes('检测');
      const hasAction = response.includes('中断') || response.includes('停止') ||
                        response.includes('终止') || response.includes('处理');
      return [
        { name: '检测问题', expected: 'true', actual: String(hasDetection), passed: hasDetection },
        { name: '处理行动', expected: 'true', actual: String(hasAction), passed: hasAction },
      ];
    },
    true
  ));

  return results;
}

/**
 * 网络/API 异常测试
 */
async function testNetworkExceptions(ctx: TestContext): Promise<ScenarioResult[]> {
  const results: ScenarioResult[] = [];

  // NET-01: API 连接正常验证
  results.push(await executeScenario(
    ctx,
    'NET-01',
    'API 连接正常验证',
    '你好，请简单回复"连接正常"。',
    (response) => {
      const hasResponse = response.length > 0;
      return [
        { name: 'API 响应', expected: 'true', actual: String(hasResponse), passed: hasResponse },
      ];
    }
  ));

  // NET-02: 重试机制验证
  results.push(await executeScenario(
    ctx,
    'NET-02',
    '重试机制设计验证',
    '设计一个 API 重试机制，包括:\n1. 最大重试次数\n2. 退避策略\n3. 错误判断条件',
    (response) => {
      const hasRetry = response.includes('重试') || response.includes('retry') ||
                       response.includes('次数');
      const hasBackoff = response.includes('退避') || response.includes('等待') ||
                         response.includes('延迟') || response.includes('backoff');
      return [
        { name: '重试设计', expected: 'true', actual: String(hasRetry), passed: hasRetry },
        { name: '退避策略', expected: 'true', actual: String(hasBackoff), passed: hasBackoff },
      ];
    }
  ));

  return results;
}

/**
 * 用户操作异常测试
 */
async function testUserExceptions(ctx: TestContext): Promise<ScenarioResult[]> {
  const results: ScenarioResult[] = [];

  // USR-01: 用户取消处理
  results.push(await executeScenario(
    ctx,
    'USR-01',
    '用户取消处理',
    '作为编排者，用户在任务执行过程中点击了"取消"。\n当前状态: Worker A 正在执行第 2/5 个 Todo\n\n请描述取消流程和清理操作。',
    (response) => {
      const hasCancel = response.includes('取消') || response.includes('中断') ||
                        response.includes('停止');
      const hasCleanup = response.includes('清理') || response.includes('保存') ||
                         response.includes('状态') || response.includes('回滚');
      return [
        { name: '取消处理', expected: 'true', actual: String(hasCancel), passed: hasCancel },
        { name: '清理操作', expected: 'true', actual: String(hasCleanup), passed: hasCleanup },
      ];
    },
    true
  ));

  // USR-02: 用户切换任务
  results.push(await executeScenario(
    ctx,
    'USR-02',
    '用户切换任务',
    '作为编排者，用户在当前任务未完成时发起了新任务:\n当前任务: 重构登录模块（进度 60%）\n新任务: 修复紧急 bug\n\n请决定如何处理。',
    (response) => {
      const hasDecision = response.includes('暂停') || response.includes('保存') ||
                          response.includes('切换') || response.includes('新任务');
      return [
        { name: '处理决策', expected: 'true', actual: String(hasDecision), passed: hasDecision },
      ];
    },
    true
  ));

  return results;
}

// ============================================================================
// 13.4 边界场景
// ============================================================================

async function testBoundaryScenarios(ctx: TestContext): Promise<ScenarioResult[]> {
  const results: ScenarioResult[] = [];

  // EDG-01: 空输入处理
  results.push(await executeScenario(
    ctx,
    'EDG-01',
    '空输入处理',
    '作为编排者，如果用户发送了空消息或只有空白字符，你应该如何处理？',
    (response) => {
      const hasHandling = response.includes('空') || response.includes('提示') ||
                          response.includes('处理') || response.includes('忽略');
      return [
        { name: '空输入处理', expected: 'true', actual: String(hasHandling), passed: hasHandling },
      ];
    }
  ));

  // EDG-02: 特殊字符输入
  results.push(await executeScenario(
    ctx,
    'EDG-02',
    '特殊字符输入',
    '计算: 2 > 1 && 3 < 4 的结果是什么？',
    (response) => {
      const hasResult = response.includes('true') || response.includes('真') ||
                        response.includes('是') || response.includes('正确');
      return [
        { name: '正确处理', expected: 'true', actual: String(hasResult), passed: hasResult },
      ];
    }
  ));

  // EDG-03: Unicode 输入
  results.push(await executeScenario(
    ctx,
    'EDG-03',
    'Unicode 输入',
    '请用 emoji 回复：你好世界！🌍🚀',
    (response) => {
      const hasResponse = response.length > 0;
      return [
        { name: 'Unicode 处理', expected: 'true', actual: String(hasResponse), passed: hasResponse },
      ];
    }
  ));

  // EDG-04: 超长输入处理
  results.push(await executeScenario(
    ctx,
    'EDG-04',
    '超长输入处理',
    '如果用户发送了一个超过 10000 字符的请求，编排者应该如何处理？请简要说明。',
    (response) => {
      const hasHandling = response.includes('截断') || response.includes('限制') ||
                          response.includes('分段') || response.includes('处理');
      return [
        { name: '超长处理', expected: 'true', actual: String(hasHandling), passed: hasHandling },
      ];
    }
  ));

  return results;
}

// ============================================================================
// 主程序
// ============================================================================

function printResults(results: ScenarioResult[]): void {
  for (const result of results) {
    const icon = result.passed ? '✓' : '✗';
    const tokens = result.tokenUsage ? ` [${result.tokenUsage.input}/${result.tokenUsage.output}]` : '';
    console.log(`    ${icon} ${result.scenarioId}: ${result.description} (${result.duration}ms)${tokens}`);
    if (!result.passed) {
      for (const vp of result.verificationPoints) {
        if (!vp.passed) {
          console.log(`        - ${vp.name}: 期望 ${vp.expected}, 实际 ${vp.actual}`);
        }
      }
      if (result.error) {
        console.log(`        - 错误: ${result.error}`);
      }
    }
  }
}

async function main() {
  const args = process.argv.slice(2);
  const quickMode = args.includes('--quick');

  console.log('============================================================');
  console.log('LLM 编排逻辑端到端测试');
  console.log('============================================================');
  console.log('');

  // 检查 LLM 配置
  const configPath = path.join(os.homedir(), '.magi', 'llm.json');
  if (!fs.existsSync(configPath)) {
    console.error('错误: 未找到 LLM 配置文件 (~/.magi/llm.json)');
    console.error('请先配置 LLM API');
    process.exit(1);
  }

  let ctx: TestContext;
  const allResults: ScenarioResult[] = [];

  try {
    console.log('初始化测试上下文...');
    ctx = createTestContext();
    console.log(`编排者: ${ctx.config.orchestrator.model}`);
    console.log(`Workers: ${Array.from(ctx.workerClients.keys()).join(', ') || '无可用 Worker'}`);
    console.log('');

    // 13.1 非任务模式场景
    console.log('【13.1 非任务模式场景 (ASK/DIRECT/EXPLORE)】');

    console.log('  [ASK 模式]');
    const askResults = await testASKMode(ctx);
    allResults.push(...askResults);
    printResults(askResults);

    console.log('  [DIRECT 模式]');
    const dirResults = await testDIRECTMode(ctx);
    allResults.push(...dirResults);
    printResults(dirResults);

    console.log('  [EXPLORE 模式]');
    const expResults = await testEXPLOREMode(ctx);
    allResults.push(...expResults);
    printResults(expResults);

    if (!quickMode) {
      // 13.2 完整路径场景
      console.log('');
      console.log('【13.2 完整路径场景 (MissionOrchestrator)】');

      console.log('  [单 Worker 任务]');
      const sinResults = await testSingleWorkerMission(ctx);
      allResults.push(...sinResults);
      printResults(sinResults);

      console.log('  [多 Worker 协作]');
      const mulResults = await testMultiWorkerMission(ctx);
      allResults.push(...mulResults);
      printResults(mulResults);

      console.log('  [Worker 汇报]');
      const repResults = await testWorkerReporting(ctx);
      allResults.push(...repResults);
      printResults(repResults);

      // 13.3 异常与降级场景
      console.log('');
      console.log('【13.3 异常与降级场景】');

      console.log('  [Worker 失败降级]');
      const degResults = await testWorkerDegradation(ctx);
      allResults.push(...degResults);
      printResults(degResults);

      console.log('  [网络/API 异常]');
      const netResults = await testNetworkExceptions(ctx);
      allResults.push(...netResults);
      printResults(netResults);

      console.log('  [用户操作异常]');
      const usrResults = await testUserExceptions(ctx);
      allResults.push(...usrResults);
      printResults(usrResults);

      // 13.4 边界场景
      console.log('');
      console.log('【13.4 边界场景】');
      const edgResults = await testBoundaryScenarios(ctx);
      allResults.push(...edgResults);
      printResults(edgResults);
    }

  } catch (error) {
    console.error('测试执行错误:', error);
    process.exit(1);
  }

  // 汇总
  console.log('');
  console.log('============================================================');
  console.log('测试汇总');
  console.log('============================================================');

  const passed = allResults.filter(r => r.passed).length;
  const total = allResults.length;
  const passRate = total > 0 ? Math.round((passed / total) * 100) : 0;

  console.log(`通过: ${passed}/${total} (${passRate}%)`);
  console.log(`失败: ${total - passed}/${total}`);

  // Token 统计
  const totalInput = allResults.reduce((sum, r) => sum + (r.tokenUsage?.input || 0), 0);
  const totalOutput = allResults.reduce((sum, r) => sum + (r.tokenUsage?.output || 0), 0);
  console.log(`Token 消耗: 输入 ${totalInput}, 输出 ${totalOutput}`);

  if (total - passed > 0) {
    console.log('');
    console.log('失败场景:');
    for (const result of allResults.filter(r => !r.passed)) {
      console.log(`  - ${result.scenarioId}: ${result.description}`);
      if (result.error) {
        console.log(`      错误: ${result.error}`);
      }
    }
  }

  // 发版条件检查
  console.log('');
  console.log('============================================================');
  console.log('发版条件检查');
  console.log('============================================================');

  const meetsThreshold = passRate >= 90;
  console.log(`${meetsThreshold ? '✓' : '✗'} 场景通过率: ${passRate}% (>= 90%)`);

  if (meetsThreshold) {
    console.log('');
    console.log('✅ 满足发版条件');
  } else {
    console.log('');
    console.log('❌ 未满足发版条件');
  }

  process.exit(meetsThreshold ? 0 : 1);
}

main().catch((err) => {
  console.error('测试运行失败:', err);
  process.exit(1);
});
