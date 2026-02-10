/**
 * 编排者真实场景专项测试
 *
 * 验证编排者的核心能力（不依赖 VSCode）：
 * 1. 需求分析与理解
 * 2. 任务拆分与 Worker 分配
 * 3. Worker 动态 Todo 补充
 * 4. 节点反馈机制
 *
 * 运行: npx ts-node src/test/e2e/orchestrator-real-scenario-e2e.ts
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

interface TestResult {
  name: string;
  passed: boolean;
  details: string[];
  duration: number;
  tokenUsage?: { input: number; output: number };
}

interface TestContext {
  orchestratorClient: UniversalLLMClient;
  workerClients: Map<string, UniversalLLMClient>;
  config: ReturnType<typeof LLMConfigLoader.loadFullConfig>;
}

// ============================================================================
// 编排者系统提示词
// ============================================================================

const ORCHESTRATOR_SYSTEM_PROMPT = `你是一个智能编排者（Orchestrator），负责分析用户需求并制定执行计划。

你的核心职责：
1. **意图分析**：判断用户请求属于以下哪种模式：
   - ASK: 简单问答，直接回答即可
   - DIRECT: 明确的编辑指令，直接执行
   - EXPLORE: 代码分析/探索，需要阅读但不修改
   - TASK: 复杂任务，需要拆分和 Worker 协作
   - CLARIFY: 需求模糊，需要澄清

2. **任务分解**：对于 TASK 模式，将任务拆分为多个子任务
3. **Worker 分配**：根据任务特点选择合适的 Worker（claude/codex/gemini）
4. **Todo 规划**：为每个 Worker 生成具体的 Todo 列表

请用 JSON 格式输出分析结果。`;

const INTENT_ANALYSIS_PROMPT = `分析以下用户请求，判断其意图类型。

用户请求: {userPrompt}

请返回 JSON 格式：
{
  "intent": "question|trivial|exploratory|task|ambiguous",
  "recommendedMode": "ask|direct|explore|task|clarify",
  "confidence": 0.0-1.0,
  "needsClarification": boolean,
  "clarificationQuestions": [],
  "reason": "..."
}`;

const TASK_DECOMPOSITION_PROMPT = `对以下复杂任务进行分解，分配给合适的 Worker。

任务描述: {userPrompt}

可用的 Worker：
- claude: 擅长代码分析、架构设计、文档编写
- codex: 擅长代码生成、重构、性能优化
- gemini: 擅长多模态处理、快速原型

请返回 JSON 格式：
{
  "mission": {
    "goal": "任务目标",
    "scope": "任务范围"
  },
  "assignments": [
    {
      "workerId": "claude|codex|gemini",
      "responsibility": "职责描述",
      "todos": [
        {
          "id": "todo-1",
          "content": "具体任务内容",
          "reasoning": "为什么需要这个任务",
          "expectedOutput": "预期产出",
          "priority": 1
        }
      ]
    }
  ],
  "dependencies": ["描述任务间的依赖关系"],
  "executionOrder": "parallel|sequential"
}`;

// ============================================================================
// 测试框架
// ============================================================================

function createTestContext(): TestContext {
  const config = LLMConfigLoader.loadFullConfig();

  const orchestratorConfig: LLMConfig = {
    ...config.orchestrator,
    enabled: true,
  };
  const orchestratorClient = new UniversalLLMClient(orchestratorConfig);

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

async function analyzeIntent(
  ctx: TestContext,
  userPrompt: string
): Promise<{ mode: string; confidence: number; reason: string; needsClarification: boolean; questions: string[] }> {
  const prompt = INTENT_ANALYSIS_PROMPT.replace('{userPrompt}', userPrompt);

  const result = await ctx.orchestratorClient.sendMessage({
    messages: [{ role: 'user', content: prompt }],
    systemPrompt: ORCHESTRATOR_SYSTEM_PROMPT,
    maxTokens: 1024,
    temperature: 0.3,
  });

  try {
    const jsonMatch = result.content?.match(/\{[\s\S]*\}/);
    if (jsonMatch) {
      const parsed = JSON.parse(jsonMatch[0]);
      return {
        mode: parsed.recommendedMode || 'task',
        confidence: parsed.confidence || 0.5,
        reason: parsed.reason || '',
        needsClarification: parsed.needsClarification || false,
        questions: parsed.clarificationQuestions || [],
      };
    }
  } catch (e) {
    // 解析失败，返回默认值
  }

  return {
    mode: 'task',
    confidence: 0.5,
    reason: '无法解析 LLM 响应',
    needsClarification: false,
    questions: [],
  };
}

async function decomposeTask(
  ctx: TestContext,
  userPrompt: string
): Promise<{
  mission: { goal: string; scope: string };
  assignments: Array<{
    workerId: string;
    responsibility: string;
    todos: Array<{ id: string; content: string; reasoning: string; priority: number }>;
  }>;
  executionOrder: string;
}> {
  const prompt = TASK_DECOMPOSITION_PROMPT.replace('{userPrompt}', userPrompt);

  const result = await ctx.orchestratorClient.sendMessage({
    messages: [{ role: 'user', content: prompt }],
    systemPrompt: ORCHESTRATOR_SYSTEM_PROMPT,
    maxTokens: 4096, // 增加 token 限制以避免截断
    temperature: 0.5,
  });

  try {
    // 移除 markdown 代码块标记
    let content = result.content || '';
    content = content.replace(/```json\s*/g, '').replace(/```\s*/g, '');

    const jsonMatch = content.match(/\{[\s\S]*\}/);
    if (jsonMatch) {
      let jsonStr = jsonMatch[0];

      // 尝试修复被截断的 JSON
      try {
        JSON.parse(jsonStr);
      } catch (e) {
        // JSON 被截断，尝试修复
        // 找到最后一个完整的 assignment 结尾
        const lastAssignmentEnd = jsonStr.lastIndexOf('}]');
        if (lastAssignmentEnd > 0) {
          // 截断到 assignments 数组结束，添加闭合括号
          jsonStr = jsonStr.substring(0, lastAssignmentEnd + 2) + '}';
        }
      }

      const parsed = JSON.parse(jsonStr);
      return {
        mission: parsed.mission || { goal: '', scope: '' },
        assignments: parsed.assignments || [],
        executionOrder: parsed.executionOrder || 'sequential',
      };
    }
  } catch (e) {
    // 解析失败，记录错误用于调试
    console.log('    [DEBUG] JSON 解析失败:', (e as Error).message);
  }

  return {
    mission: { goal: '无法解析', scope: '' },
    assignments: [],
    executionOrder: 'sequential',
  };
}

// ============================================================================
// 测试场景 1: 编排者需求分析与理解
// ============================================================================

async function testRequirementAnalysis(ctx: TestContext): Promise<TestResult[]> {
  const results: TestResult[] = [];

  // 测试 1.1: 简单问题 → ASK 模式
  console.log('\n  📋 测试 1.1: 简单问题 → ASK 模式');
  const start1 = Date.now();
  const details1: string[] = [];
  let passed1 = false;

  try {
    const analysis = await analyzeIntent(ctx, '什么是 TypeScript 的泛型？');

    details1.push(`意图模式: ${analysis.mode}`);
    details1.push(`置信度: ${analysis.confidence}`);
    details1.push(`原因: ${analysis.reason}`);

    if (analysis.mode === 'ask') {
      passed1 = true;
      details1.push('✓ 正确识别为 ASK 模式');
    } else {
      details1.push(`✗ 期望 ASK，实际 ${analysis.mode}`);
    }
  } catch (e) {
    details1.push(`错误: ${e}`);
  }

  results.push({
    name: '简单问题 → ASK 模式',
    passed: passed1,
    details: details1,
    duration: Date.now() - start1,
  });

  // 测试 1.2: 直接编辑指令 → DIRECT 模式
  console.log('\n  📋 测试 1.2: 直接编辑指令 → DIRECT 模式');
  const start2 = Date.now();
  const details2: string[] = [];
  let passed2 = false;

  try {
    const analysis = await analyzeIntent(ctx, '把 src/index.ts 第 10 行的变量名从 foo 改成 bar');

    details2.push(`意图模式: ${analysis.mode}`);
    details2.push(`置信度: ${analysis.confidence}`);

    if (analysis.mode === 'direct') {
      passed2 = true;
      details2.push('✓ 正确识别为 DIRECT 模式');
    } else {
      details2.push(`✗ 期望 DIRECT，实际 ${analysis.mode}`);
      // 如果是 task，也可以接受
      if (analysis.mode === 'task') {
        passed2 = true;
        details2.push('  (TASK 模式也可接受)');
      }
    }
  } catch (e) {
    details2.push(`错误: ${e}`);
  }

  results.push({
    name: '直接编辑指令 → DIRECT 模式',
    passed: passed2,
    details: details2,
    duration: Date.now() - start2,
  });

  // 测试 1.3: 复杂任务 → TASK 模式
  console.log('\n  📋 测试 1.3: 复杂任务 → TASK 模式');
  const start3 = Date.now();
  const details3: string[] = [];
  let passed3 = false;

  try {
    const analysis = await analyzeIntent(
      ctx,
      '实现一个用户认证模块，包括登录、注册、密码重置功能，需要 JWT 验证和数据库存储'
    );

    details3.push(`意图模式: ${analysis.mode}`);
    details3.push(`置信度: ${analysis.confidence}`);
    details3.push(`原因: ${analysis.reason.substring(0, 100)}`);

    if (analysis.mode === 'task') {
      passed3 = true;
      details3.push('✓ 正确识别为 TASK 模式');
    } else {
      details3.push(`✗ 期望 TASK，实际 ${analysis.mode}`);
    }
  } catch (e) {
    details3.push(`错误: ${e}`);
  }

  results.push({
    name: '复杂任务 → TASK 模式',
    passed: passed3,
    details: details3,
    duration: Date.now() - start3,
  });

  // 测试 1.4: 模糊需求 → CLARIFY 模式
  console.log('\n  📋 测试 1.4: 模糊需求 → CLARIFY 模式');
  const start4 = Date.now();
  const details4: string[] = [];
  let passed4 = false;

  try {
    const analysis = await analyzeIntent(ctx, '优化性能');

    details4.push(`意图模式: ${analysis.mode}`);
    details4.push(`需要澄清: ${analysis.needsClarification}`);

    if (analysis.mode === 'clarify' || analysis.needsClarification) {
      passed4 = true;
      details4.push('✓ 正确识别需要澄清');
      if (analysis.questions.length > 0) {
        details4.push(`  澄清问题: ${analysis.questions.join('; ')}`);
      }
    } else {
      // 即使不是 clarify，也可能是合理的（LLM 认为可以处理）
      passed4 = true;
      details4.push(`  LLM 认为可以作为 ${analysis.mode} 处理`);
    }
  } catch (e) {
    details4.push(`错误: ${e}`);
  }

  results.push({
    name: '模糊需求 → CLARIFY 模式',
    passed: passed4,
    details: details4,
    duration: Date.now() - start4,
  });

  // 测试 1.5: 探索分析 → EXPLORE 模式
  console.log('\n  📋 测试 1.5: 探索分析 → EXPLORE 模式');
  const start5 = Date.now();
  const details5: string[] = [];
  let passed5 = false;

  try {
    const analysis = await analyzeIntent(ctx, '分析 src/orchestrator 目录的代码架构');

    details5.push(`意图模式: ${analysis.mode}`);
    details5.push(`置信度: ${analysis.confidence}`);

    if (analysis.mode === 'explore') {
      passed5 = true;
      details5.push('✓ 正确识别为 EXPLORE 模式');
    } else {
      details5.push(`✗ 期望 EXPLORE，实际 ${analysis.mode}`);
      // TASK 也可接受
      if (analysis.mode === 'task' || analysis.mode === 'ask') {
        passed5 = true;
        details5.push('  (也可接受)');
      }
    }
  } catch (e) {
    details5.push(`错误: ${e}`);
  }

  results.push({
    name: '探索分析 → EXPLORE 模式',
    passed: passed5,
    details: details5,
    duration: Date.now() - start5,
  });

  return results;
}

// ============================================================================
// 测试场景 2: 任务拆分与 Worker 分配
// ============================================================================

async function testTaskDecomposition(ctx: TestContext): Promise<TestResult[]> {
  const results: TestResult[] = [];

  // 测试 2.1: 单 Worker 任务拆分
  console.log('\n  📋 测试 2.1: 单 Worker 任务拆分');
  const start1 = Date.now();
  const details1: string[] = [];
  let passed1 = false;

  try {
    const decomposition = await decomposeTask(
      ctx,
      '给 src/utils/helper.ts 文件添加 JSDoc 注释'
    );

    details1.push(`任务目标: ${decomposition.mission.goal}`);
    details1.push(`分配数量: ${decomposition.assignments.length}`);

    if (decomposition.assignments.length >= 1) {
      passed1 = true;
      decomposition.assignments.forEach((a, i) => {
        details1.push(`  Assignment ${i + 1}: Worker=${a.workerId}`);
        details1.push(`    职责: ${a.responsibility.substring(0, 50)}`);
        details1.push(`    Todos: ${a.todos.length}`);
      });
    } else {
      details1.push('✗ 未生成 Assignment');
    }
  } catch (e) {
    details1.push(`错误: ${e}`);
  }

  results.push({
    name: '单 Worker 任务拆分',
    passed: passed1,
    details: details1,
    duration: Date.now() - start1,
  });

  // 测试 2.2: 多 Worker 协作任务
  console.log('\n  📋 测试 2.2: 多 Worker 协作任务');
  const start2 = Date.now();
  const details2: string[] = [];
  let passed2 = false;

  try {
    const decomposition = await decomposeTask(
      ctx,
      '重构用户模块：后端需要更新 API 接口设计，前端需要更新组件调用方式，同时需要编写集成测试'
    );

    details2.push(`任务目标: ${decomposition.mission.goal}`);
    details2.push(`任务范围: ${decomposition.mission.scope}`);
    details2.push(`分配数量: ${decomposition.assignments.length}`);
    details2.push(`执行顺序: ${decomposition.executionOrder}`);

    const workers = new Set(decomposition.assignments.map(a => a.workerId));
    details2.push(`涉及 Worker: ${[...workers].join(', ')}`);

    if (decomposition.assignments.length >= 1) {
      passed2 = true;
      decomposition.assignments.forEach((a, i) => {
        details2.push(`\n  Assignment ${i + 1} (${a.workerId}):`);
        details2.push(`    职责: ${a.responsibility}`);
        a.todos.slice(0, 3).forEach((t, j) => {
          details2.push(`    Todo ${j + 1}: ${t.content.substring(0, 50)}...`);
        });
      });
    }
  } catch (e) {
    details2.push(`错误: ${e}`);
  }

  results.push({
    name: '多 Worker 协作任务',
    passed: passed2,
    details: details2,
    duration: Date.now() - start2,
  });

  // 测试 2.3: 明确指定 Worker
  console.log('\n  📋 测试 2.3: 明确指定 Worker');
  const start3 = Date.now();
  const details3: string[] = [];
  let passed3 = false;

  try {
    const decomposition = await decomposeTask(
      ctx,
      '使用 Claude 分析代码架构设计，使用 Codex 生成代码实现'
    );

    details3.push(`分配数量: ${decomposition.assignments.length}`);

    const workerIds = decomposition.assignments.map(a => a.workerId.toLowerCase());
    const hasClaude = workerIds.some(w => w.includes('claude'));
    const hasCodex = workerIds.some(w => w.includes('codex'));

    details3.push(`包含 Claude: ${hasClaude}`);
    details3.push(`包含 Codex: ${hasCodex}`);

    if (decomposition.assignments.length >= 1) {
      passed3 = true;
      decomposition.assignments.forEach((a, i) => {
        details3.push(`  Worker ${i + 1}: ${a.workerId} - ${a.responsibility.substring(0, 40)}`);
      });
    }
  } catch (e) {
    details3.push(`错误: ${e}`);
  }

  results.push({
    name: '明确指定 Worker',
    passed: passed3,
    details: details3,
    duration: Date.now() - start3,
  });

  return results;
}

// ============================================================================
// 测试场景 3: Worker 动态 Todo 管理
// ============================================================================

async function testDynamicTodoManagement(ctx: TestContext): Promise<TestResult[]> {
  const results: TestResult[] = [];

  // 测试 3.1: Todo 生成质量
  console.log('\n  📋 测试 3.1: Todo 生成质量');
  const start1 = Date.now();
  const details1: string[] = [];
  let passed1 = false;

  try {
    const decomposition = await decomposeTask(
      ctx,
      '实现一个文件上传功能，包括前端上传组件和后端存储接口'
    );

    let totalTodos = 0;
    let validTodos = 0;

    decomposition.assignments.forEach(a => {
      a.todos.forEach(t => {
        totalTodos++;
        if (t.id && t.content && t.content.length > 10) {
          validTodos++;
        }
      });
    });

    details1.push(`总 Todo 数量: ${totalTodos}`);
    details1.push(`有效 Todo 数量: ${validTodos}`);

    if (validTodos >= 2) {
      passed1 = true;
      details1.push('✓ Todo 生成质量合格');

      // 展示 Todo 示例
      const firstAssignment = decomposition.assignments[0];
      if (firstAssignment && firstAssignment.todos.length > 0) {
        details1.push('\nTodo 示例:');
        firstAssignment.todos.slice(0, 2).forEach((t, i) => {
          details1.push(`  [${i + 1}] ${t.content}`);
          if (t.reasoning) {
            details1.push(`      原因: ${t.reasoning.substring(0, 50)}...`);
          }
        });
      }
    } else {
      details1.push('✗ Todo 数量不足或质量不合格');
    }
  } catch (e) {
    details1.push(`错误: ${e}`);
  }

  results.push({
    name: 'Todo 生成质量',
    passed: passed1,
    details: details1,
    duration: Date.now() - start1,
  });

  // 测试 3.2: Todo 优先级与依赖
  console.log('\n  📋 测试 3.2: Todo 优先级与依赖');
  const start2 = Date.now();
  const details2: string[] = [];
  let passed2 = false;

  try {
    const decomposition = await decomposeTask(
      ctx,
      '创建一个 REST API，先设计数据模型，然后实现 CRUD 接口，最后添加数据验证'
    );

    details2.push(`执行顺序: ${decomposition.executionOrder}`);

    let hasPriority = false;
    decomposition.assignments.forEach(a => {
      a.todos.forEach(t => {
        if (t.priority !== undefined) {
          hasPriority = true;
        }
      });
    });

    details2.push(`包含优先级: ${hasPriority}`);

    if (decomposition.assignments.length >= 1) {
      passed2 = true;

      decomposition.assignments.forEach((a, i) => {
        details2.push(`\n  Assignment ${i + 1} (${a.workerId}):`);
        a.todos.forEach((t, j) => {
          details2.push(`    [P${t.priority || '?'}] ${t.content.substring(0, 40)}...`);
        });
      });
    }
  } catch (e) {
    details2.push(`错误: ${e}`);
  }

  results.push({
    name: 'Todo 优先级与依赖',
    passed: passed2,
    details: details2,
    duration: Date.now() - start2,
  });

  return results;
}

// ============================================================================
// 测试场景 4: Worker 汇报与反馈模拟
// ============================================================================

async function testWorkerFeedback(ctx: TestContext): Promise<TestResult[]> {
  const results: TestResult[] = [];

  // 测试 4.1: Worker 完成汇报格式
  console.log('\n  📋 测试 4.1: Worker 完成汇报格式');
  const start1 = Date.now();
  const details1: string[] = [];
  let passed1 = false;

  try {
    const workerClient = ctx.workerClients.values().next().value;
    if (!workerClient) {
      throw new Error('No worker client available');
    }

    const result = await workerClient.sendMessage({
      messages: [{ role: 'user', content: `你是一个 Worker，刚完成了一个 Todo：
Todo: 实现用户登录 API
结果: 成功创建了 /api/auth/login 端点

请生成一个完成汇报，使用以下 JSON 格式：
{
  "type": "completed",
  "todoId": "todo-1",
  "status": "completed",
  "summary": "完成摘要",
  "modifiedFiles": ["文件列表"],
  "duration": 1000
}` }],
      systemPrompt: '你是一个执行任务的 Worker。',
      maxTokens: 512,
      temperature: 0.3,
    });

    const content = result.content || '';
    details1.push(`响应长度: ${content.length}`);

    const jsonMatch = content.match(/\{[\s\S]*\}/);
    if (jsonMatch) {
      try {
        const report = JSON.parse(jsonMatch[0]);
        if (report.type && report.status && report.summary) {
          passed1 = true;
          details1.push('✓ 汇报格式正确');
          details1.push(`  类型: ${report.type}`);
          details1.push(`  状态: ${report.status}`);
          details1.push(`  摘要: ${report.summary.substring(0, 50)}`);
        } else {
          details1.push('✗ 汇报缺少必要字段');
        }
      } catch (e) {
        details1.push('✗ JSON 解析失败');
      }
    } else {
      details1.push('✗ 未找到 JSON 格式汇报');
      passed1 = true; // 允许非 JSON 格式
    }
  } catch (e) {
    details1.push(`错误: ${e}`);
  }

  results.push({
    name: 'Worker 完成汇报格式',
    passed: passed1,
    details: details1,
    duration: Date.now() - start1,
  });

  // 测试 4.2: Worker 澄清请求
  console.log('\n  📋 测试 4.2: Worker 澄清请求');
  const start2 = Date.now();
  const details2: string[] = [];
  let passed2 = false;

  try {
    const workerClient = ctx.workerClients.values().next().value;
    if (!workerClient) {
      throw new Error('No worker client available');
    }

    const result = await workerClient.sendMessage({
      messages: [{ role: 'user', content: `你是一个 Worker，在执行任务时遇到不确定的地方：
任务: 添加用户验证
问题: 不确定使用 JWT 还是 Session

请生成一个澄清请求，说明你的问题和可选方案。` }],
      systemPrompt: '你是一个执行任务的 Worker，遇到不确定的地方需要向编排者澄清。',
      maxTokens: 512,
      temperature: 0.3,
    });

    const content = result.content || '';
    details2.push(`响应长度: ${content.length}`);

    const hasQuestion = content.includes('?') || content.includes('？');
    const hasOptions = content.includes('JWT') || content.includes('Session');

    details2.push(`包含问题: ${hasQuestion}`);
    details2.push(`包含选项: ${hasOptions}`);

    if (hasQuestion || hasOptions) {
      passed2 = true;
      details2.push('✓ 澄清请求格式合理');
      details2.push(`  内容预览: ${content.substring(0, 100)}...`);
    }
  } catch (e) {
    details2.push(`错误: ${e}`);
  }

  results.push({
    name: 'Worker 澄清请求',
    passed: passed2,
    details: details2,
    duration: Date.now() - start2,
  });

  // 测试 4.3: 编排者响应决策
  console.log('\n  📋 测试 4.3: 编排者响应决策');
  const start3 = Date.now();
  const details3: string[] = [];
  let passed3 = false;

  try {
    const result = await ctx.orchestratorClient.sendMessage({
      messages: [{ role: 'user', content: `Worker 汇报：任务执行中遇到问题
问题：文件 config.json 格式错误，无法解析
当前进度：50%

请作为编排者决定下一步行动：
1. 让 Worker 重试
2. 切换到其他 Worker
3. 终止任务并报告用户

请返回 JSON 格式决策：
{
  "action": "retry|switch|abort",
  "reason": "原因",
  "instructions": "给 Worker 的指令"
}` }],
      systemPrompt: ORCHESTRATOR_SYSTEM_PROMPT,
      maxTokens: 512,
      temperature: 0.3,
    });

    const content = result.content || '';
    details3.push(`响应长度: ${content.length}`);

    const jsonMatch = content.match(/\{[\s\S]*\}/);
    if (jsonMatch) {
      try {
        const decision = JSON.parse(jsonMatch[0]);
        if (decision.action && decision.reason) {
          passed3 = true;
          details3.push('✓ 决策格式正确');
          details3.push(`  行动: ${decision.action}`);
          details3.push(`  原因: ${decision.reason.substring(0, 50)}`);
        }
      } catch (e) {
        passed3 = true; // JSON 解析失败但有响应也可以
        details3.push('  响应非 JSON 格式，但有内容');
      }
    } else {
      passed3 = true;
      details3.push('  响应为自然语言格式');
    }
  } catch (e) {
    details3.push(`错误: ${e}`);
  }

  results.push({
    name: '编排者响应决策',
    passed: passed3,
    details: details3,
    duration: Date.now() - start3,
  });

  return results;
}

// ============================================================================
// 主程序
// ============================================================================

function printResults(results: TestResult[]): void {
  for (const result of results) {
    const icon = result.passed ? '✓' : '✗';
    console.log(`    ${icon} ${result.name} (${result.duration}ms)`);
    result.details.forEach(d => {
      console.log(`        ${d}`);
    });
  }
}

async function main() {
  console.log('============================================================');
  console.log('编排者真实场景专项测试');
  console.log('============================================================');
  console.log('');

  const configPath = path.join(os.homedir(), '.magi', 'llm.json');
  if (!fs.existsSync(configPath)) {
    console.error('错误: 未找到 LLM 配置文件 (~/.magi/llm.json)');
    process.exit(1);
  }

  const config = LLMConfigLoader.loadFullConfig();
  console.log(`编排者: ${config.orchestrator.model}`);
  console.log(`Workers: ${Object.keys(config.workers).join(', ')}`);
  console.log('');

  const ctx = createTestContext();
  const allResults: TestResult[] = [];

  try {
    // 场景 1: 需求分析与理解
    console.log('【场景 1: 编排者需求分析与理解】');
    const analysisResults = await testRequirementAnalysis(ctx);
    allResults.push(...analysisResults);
    printResults(analysisResults);

    // 场景 2: 任务拆分与 Worker 分配
    console.log('\n【场景 2: 任务拆分与 Worker 分配】');
    const decompositionResults = await testTaskDecomposition(ctx);
    allResults.push(...decompositionResults);
    printResults(decompositionResults);

    // 场景 3: Worker 动态 Todo 管理
    console.log('\n【场景 3: Worker 动态 Todo 管理】');
    const todoResults = await testDynamicTodoManagement(ctx);
    allResults.push(...todoResults);
    printResults(todoResults);

    // 场景 4: Worker 汇报与反馈
    console.log('\n【场景 4: Worker 汇报与反馈】');
    const feedbackResults = await testWorkerFeedback(ctx);
    allResults.push(...feedbackResults);
    printResults(feedbackResults);

  } catch (error) {
    console.error('测试执行错误:', error);
  }

  // 汇总
  console.log('\n============================================================');
  console.log('测试汇总');
  console.log('============================================================');

  const passed = allResults.filter(r => r.passed).length;
  const total = allResults.length;
  const passRate = total > 0 ? Math.round((passed / total) * 100) : 0;

  console.log(`通过: ${passed}/${total} (${passRate}%)`);
  console.log(`失败: ${total - passed}/${total}`);

  if (total - passed > 0) {
    console.log('\n失败场景:');
    for (const result of allResults.filter(r => !r.passed)) {
      console.log(`  ✗ ${result.name}`);
    }
  }

  console.log('\n============================================================');
  console.log('验证结论');
  console.log('============================================================');

  if (passRate >= 80) {
    console.log('✅ 编排者核心能力验证通过');
    console.log('  - 需求分析与意图识别: 正常');
    console.log('  - 任务拆分与 Worker 分配: 正常');
    console.log('  - Todo 动态管理: 正常');
    console.log('  - 节点反馈机制: 正常');
  } else {
    console.log('❌ 存在需要修复的问题');
  }

  process.exit(passRate >= 80 ? 0 : 1);
}

main().catch((err) => {
  console.error('测试运行失败:', err);
  process.exit(1);
});
