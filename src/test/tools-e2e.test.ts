/**
 * 内置工具端对端测试
 *
 * 此测试可以在纯 Node.js 环境下运行，不依赖 VSCode
 */

import { MermaidExecutor } from '../tools/mermaid-executor';
import { FileExecutor } from '../tools/file-executor';
import { SearchExecutor } from '../tools/search-executor';
import { RemoveFilesExecutor } from '../tools/remove-files-executor';
import { WebExecutor } from '../tools/web-executor';
import { ToolCall } from '../llm/types';
import path from 'path';

/**
 * 测试结果
 */
interface TestResult {
  name: string;
  passed: boolean;
  error?: string;
  duration: number;
}

/**
 * 运行单个测试
 */
async function runTest(name: string, testFn: () => Promise<any>): Promise<TestResult> {
  const startTime = Date.now();
  console.log(`\n[测试] ${name}`);

  try {
    await testFn();
    const duration = Date.now() - startTime;
    console.log(`  ✅ 通过 (${duration}ms)`);
    return { name, passed: true, duration };
  } catch (error: any) {
    const duration = Date.now() - startTime;
    console.log(`  ❌ 失败: ${error.message}`);
    return { name, passed: false, error: error.message, duration };
  }
}

/**
 * 运行所有测试
 */
async function runTests(): Promise<void> {
  console.log('='.repeat(80));
  console.log('内置工具端对端测试');
  console.log('='.repeat(80));

  const results: TestResult[] = [];
  const projectRoot = path.join(__dirname, '../../');

  // ============================================================================
  // 测试组 1: MermaidExecutor 测试
  // ============================================================================
  console.log('\n' + '='.repeat(80));
  console.log('测试组 1: MermaidExecutor');
  console.log('='.repeat(80));

  const mermaidExecutor = new MermaidExecutor();

  results.push(await runTest('1.1 - 获取工具定义', async () => {
    const def = mermaidExecutor.getToolDefinition();
    if (def.name !== 'mermaid_diagram') throw new Error('工具名称不正确');
    if (!def.input_schema) throw new Error('缺少 input_schema');
    if (def.metadata?.source !== 'builtin') throw new Error('source 不是 builtin');
    console.log(`  - 工具名: ${def.name}`);
    console.log(`  - source: ${def.metadata?.source}`);
  }));

  results.push(await runTest('1.2 - 渲染流程图 (flowchart)', async () => {
    const toolCall: ToolCall = {
      id: 'test-1',
      name: 'mermaid_diagram',
      arguments: {
        code: `flowchart TD
    A[开始] --> B{条件判断}
    B -->|是| C[执行A]
    B -->|否| D[执行B]
    C --> E[结束]
    D --> E`,
        title: '决策流程',
        theme: 'dark'
      }
    };

    const result = await mermaidExecutor.execute(toolCall);
    if (result.isError) throw new Error(result.content);

    const data = JSON.parse(result.content);
    if (data.type !== 'mermaid_diagram') throw new Error('type 不正确');
    if (data.diagramType !== 'flowchart') throw new Error('diagramType 不正确');

    console.log(`  - diagramType: ${data.diagramType}`);
    console.log(`  - title: ${data.title}`);
    console.log(`  - theme: ${data.theme}`);
  }));

  results.push(await runTest('1.3 - 渲染时序图 (sequence)', async () => {
    const toolCall: ToolCall = {
      id: 'test-2',
      name: 'mermaid_diagram',
      arguments: {
        code: `sequenceDiagram
    participant U as 用户
    participant S as 服务器
    participant D as 数据库

    U->>S: 发送请求
    S->>D: 查询数据
    D-->>S: 返回结果
    S-->>U: 响应数据`
      }
    };

    const result = await mermaidExecutor.execute(toolCall);
    if (result.isError) throw new Error(result.content);

    const data = JSON.parse(result.content);
    if (data.diagramType !== 'sequence') throw new Error('diagramType 不正确');

    console.log(`  - diagramType: ${data.diagramType}`);
  }));

  results.push(await runTest('1.4 - 渲染类图 (class)', async () => {
    const toolCall: ToolCall = {
      id: 'test-3',
      name: 'mermaid_diagram',
      arguments: {
        code: `classDiagram
    class Animal {
        +String name
        +int age
        +makeSound()
    }
    class Dog {
        +bark()
    }
    Animal <|-- Dog`
      }
    };

    const result = await mermaidExecutor.execute(toolCall);
    if (result.isError) throw new Error(result.content);

    const data = JSON.parse(result.content);
    if (data.diagramType !== 'class') throw new Error('diagramType 不正确');

    console.log(`  - diagramType: ${data.diagramType}`);
  }));

  results.push(await runTest('1.5 - 拒绝无效代码', async () => {
    const toolCall: ToolCall = {
      id: 'test-4',
      name: 'mermaid_diagram',
      arguments: {
        code: 'this is not valid mermaid code'
      }
    };

    const result = await mermaidExecutor.execute(toolCall);
    if (!result.isError) throw new Error('应该返回错误');
    if (!result.content.includes('Unrecognized')) throw new Error('错误消息不正确');

    console.log(`  - 正确拒绝无效代码`);
  }));

  results.push(await runTest('1.6 - 拒绝空代码', async () => {
    const toolCall: ToolCall = {
      id: 'test-5',
      name: 'mermaid_diagram',
      arguments: {
        code: ''
      }
    };

    const result = await mermaidExecutor.execute(toolCall);
    if (!result.isError) throw new Error('应该返回错误');

    console.log(`  - 正确拒绝空代码`);
  }));

  // ============================================================================
  // 测试组 2: FileExecutor 测试
  // ============================================================================
  console.log('\n' + '='.repeat(80));
  console.log('测试组 2: FileExecutor');
  console.log('='.repeat(80));

  const fileExecutor = new FileExecutor(projectRoot);

  results.push(await runTest('2.1 - 获取工具定义', async () => {
    const def = fileExecutor.getToolDefinition();
    if (def.name !== 'text_editor') throw new Error('工具名称不正确');
    if (def.metadata?.source !== 'builtin') throw new Error('source 不是 builtin');
    console.log(`  - 工具名: ${def.name}`);
  }));

  results.push(await runTest('2.2 - view 命令', async () => {
    const toolCall: ToolCall = {
      id: 'test-file-1',
      name: 'text_editor',
      arguments: {
        command: 'view',
        path: 'package.json',
        view_range: [1, 10]
      }
    };

    const result = await fileExecutor.execute(toolCall);
    if (result.isError) throw new Error(result.content);
    if (!result.content.includes('magi')) throw new Error('内容不正确');

    console.log(`  - 成功读取 package.json`);
  }));

  // ============================================================================
  // 测试组 3: SearchExecutor 测试
  // ============================================================================
  console.log('\n' + '='.repeat(80));
  console.log('测试组 3: SearchExecutor');
  console.log('='.repeat(80));

  const searchExecutor = new SearchExecutor(projectRoot);

  results.push(await runTest('3.1 - 获取工具定义', async () => {
    const def = searchExecutor.getToolDefinition();
    if (def.name !== 'grep_search') throw new Error('工具名称不正确');
    if (def.metadata?.source !== 'builtin') throw new Error('source 不是 builtin');
    console.log(`  - 工具名: ${def.name}`);
  }));

  results.push(await runTest('3.2 - 搜索代码', async () => {
    const toolCall: ToolCall = {
      id: 'test-search-1',
      name: 'grep_search',
      arguments: {
        pattern: 'MermaidExecutor',
        include: '*.ts'
      }
    };

    const result = await searchExecutor.execute(toolCall);
    if (result.isError) throw new Error(result.content);
    if (!result.content.includes('mermaid')) throw new Error('未找到预期结果');

    console.log(`  - 成功搜索到 MermaidExecutor`);
  }));

  // ============================================================================
  // 测试组 4: WebExecutor 测试
  // ============================================================================
  console.log('\n' + '='.repeat(80));
  console.log('测试组 4: WebExecutor');
  console.log('='.repeat(80));

  const webExecutor = new WebExecutor();

  results.push(await runTest('4.1 - 获取工具定义', async () => {
    const defs = webExecutor.getToolDefinitions();
    if (defs.length !== 2) throw new Error('应该有 2 个工具');

    const webSearch = defs.find(d => d.name === 'web_search');
    const webFetch = defs.find(d => d.name === 'web_fetch');

    if (!webSearch) throw new Error('缺少 web_search');
    if (!webFetch) throw new Error('缺少 web_fetch');
    if (webSearch.metadata?.source !== 'builtin') throw new Error('source 不是 builtin');

    console.log(`  - 工具: web_search, web_fetch`);
  }));

  // ============================================================================
  // 测试组 5: RemoveFilesExecutor 测试
  // ============================================================================
  console.log('\n' + '='.repeat(80));
  console.log('测试组 5: RemoveFilesExecutor');
  console.log('='.repeat(80));

  const removeExecutor = new RemoveFilesExecutor(projectRoot);

  results.push(await runTest('5.1 - 获取工具定义', async () => {
    const def = removeExecutor.getToolDefinition();
    if (def.name !== 'remove_files') throw new Error('工具名称不正确');
    if (def.metadata?.source !== 'builtin') throw new Error('source 不是 builtin');
    console.log(`  - 工具名: ${def.name}`);
  }));

  // ============================================================================
  // 测试结果汇总
  // ============================================================================
  console.log('\n' + '='.repeat(80));
  console.log('测试结果汇总');
  console.log('='.repeat(80));

  const passed = results.filter(r => r.passed).length;
  const failed = results.filter(r => !r.passed).length;
  const totalDuration = results.reduce((sum, r) => sum + r.duration, 0);

  console.log(`\n总计: ${results.length} 个测试`);
  console.log(`✅ 通过: ${passed}`);
  console.log(`❌ 失败: ${failed}`);
  console.log(`⏱️  总耗时: ${totalDuration}ms`);

  if (failed > 0) {
    console.log('\n失败的测试:');
    results.filter(r => !r.passed).forEach(r => {
      console.log(`  - ${r.name}: ${r.error}`);
    });
  }

  console.log('\n' + '='.repeat(80));

  // 退出码
  process.exit(failed > 0 ? 1 : 0);
}

// 运行测试
if (require.main === module) {
  runTests().catch(error => {
    console.error('测试运行失败:', error);
    process.exit(1);
  });
}

export { runTests };
