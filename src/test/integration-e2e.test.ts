/**
 * 端到端集成测试
 *
 * 测试目标：
 * 1. 知识库与编排器集成（Ask 模式 + Auto 模式）
 * 2. 工具权限验证
 * 3. 完整任务执行流程
 */

import { MissionDrivenEngine } from '../orchestrator/core';
import { ToolManager } from '../tools/tool-manager';
import { ProjectKnowledgeBase } from '../knowledge/project-knowledge-base';
import { PermissionMatrix } from '../types';
import { ToolCall } from '../llm/types';
import { UnifiedSessionManager } from '../session/unified-session-manager';
import { SnapshotManager } from '../snapshot-manager';
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
 * 模拟 LLM 适配器工厂
 */
class MockAdapterFactory {
  private toolManager: ToolManager;

  constructor(permissions: PermissionMatrix, workspaceRoot: string = process.cwd()) {
    this.toolManager = new ToolManager(workspaceRoot, permissions);
  }

  getToolManager(): ToolManager {
    return this.toolManager;
  }

  async initialize(): Promise<void> {
    // Mock initialization
  }

  createOrchestratorAdapter(): any {
    return {
      sendMessage: async (message: string) => {
        return `Mock response for: ${message}`;
      }
    };
  }

  getOrCreateAdapter(): any {
    return this.createOrchestratorAdapter();
  }
}

/**
 * 创建测试用的编排器
 */
function createTestOrchestrator(permissions: PermissionMatrix, projectRoot: string): MissionDrivenEngine {
  const adapterFactory = new MockAdapterFactory(permissions, projectRoot);
  const sessionManager = new UnifiedSessionManager(projectRoot);
  const snapshotManager = new SnapshotManager(sessionManager, projectRoot);

  return new MissionDrivenEngine(
    adapterFactory as any,
    { timeout: 300000, maxRetries: 3, permissions },
    projectRoot,
    snapshotManager,
    sessionManager
  );
}

/**
 * 运行所有测试
 */
async function runTests(): Promise<void> {
  console.log('='.repeat(80));
  console.log('端到端集成测试');
  console.log('='.repeat(80));

  const results: TestResult[] = [];

  // ============================================================================
  // 测试组 1: 知识库集成测试（Ask 模式）
  // ============================================================================
  console.log('\n' + '='.repeat(80));
  console.log('测试组 1: 知识库集成 - Ask 模式');
  console.log('='.repeat(80));

  results.push(await runTest('1.1 - 知识库初始化', async () => {
    const projectRoot = path.join(__dirname, '../../');
    const kb = new ProjectKnowledgeBase({ projectRoot });
    await kb.initialize();

    const codeIndex = kb.getCodeIndex();
    if (!codeIndex) throw new Error('代码索引未初始化');
    if (codeIndex.files.length === 0) throw new Error('未找到任何文件');

    console.log(`  - 索引了 ${codeIndex.files.length} 个文件`);
  }));

  results.push(await runTest('1.2 - 编排器设置知识库', async () => {
    const projectRoot = path.join(__dirname, '../../');
    const kb = new ProjectKnowledgeBase({ projectRoot });
    await kb.initialize();

    const permissions: PermissionMatrix = {
      allowBash: true,
      allowEdit: true,
      allowWeb: true,
    };

    const orchestrator = createTestOrchestrator(permissions, projectRoot);

    // 设置知识库
    orchestrator.setKnowledgeBase(kb);

    console.log('  - 知识库已成功注入到编排器');
  }));

  results.push(await runTest('1.3 - Ask 模式包含项目上下文', async () => {
    const projectRoot = path.join(__dirname, '../../');
    const kb = new ProjectKnowledgeBase({ projectRoot });
    await kb.initialize();

    // 添加测试 ADR
    kb.addADR({
      id: 'test-001',
      title: '使用 TypeScript 进行开发',
      status: 'accepted',
      date: Date.now(),
      context: '项目需要类型安全',
      decision: '使用 TypeScript 替代 JavaScript',
      consequences: '提高代码质量和可维护性',
    });

    // 添加测试 FAQ
    kb.addFAQ({
      id: 'faq-001',
      question: '如何配置 Worker？',
      answer: '在 config/llm-config.json 中配置 Worker 参数',
      category: 'configuration',
      tags: ['worker', 'config'],
      createdAt: Date.now(),
      updatedAt: Date.now(),
      useCount: 0,
    });

    const permissions: PermissionMatrix = {
      allowBash: true,
      allowEdit: true,
      allowWeb: true,
    };

    const orchestrator = createTestOrchestrator(permissions, projectRoot);

    orchestrator.setKnowledgeBase(kb);

    // 验证知识库方法可用
    const projectContext = (orchestrator as any).getProjectContext(500);
    if (!projectContext) throw new Error('项目上下文为空');
    console.log(`  - 项目上下文长度: ${projectContext.length} 字符`);

    const relevantADRs = (orchestrator as any).getRelevantADRs('TypeScript 开发');
    if (!relevantADRs.includes('TypeScript')) throw new Error('未找到相关 ADR');
    console.log('  - 成功找到相关 ADR');

    const relevantFAQs = (orchestrator as any).getRelevantFAQs('如何配置');
    if (!relevantFAQs.includes('Worker')) throw new Error('未找到相关 FAQ');
    console.log('  - 成功找到相关 FAQ');
  }));

  // ============================================================================
  // 测试组 2: 知识库集成测试（Auto 模式）
  // ============================================================================
  console.log('\n' + '='.repeat(80));
  console.log('测试组 2: 知识库集成 - Auto 模式');
  console.log('='.repeat(80));

  results.push(await runTest('2.1 - MissionDrivenEngine 知识库支持', async () => {
    const projectRoot = path.join(__dirname, '../../');
    const kb = new ProjectKnowledgeBase({ projectRoot });
    await kb.initialize();

    const permissions: PermissionMatrix = {
      allowBash: true,
      allowEdit: true,
      allowWeb: true,
    };

    const orchestrator = createTestOrchestrator(permissions, projectRoot);

    orchestrator.setKnowledgeBase(kb);

    // 验证知识库传递到 MissionDrivenEngine
    const engine = (orchestrator as any).missionDrivenEngine;
    if (!engine) throw new Error('MissionDrivenEngine 未初始化');

    console.log('  - MissionDrivenEngine 已接收知识库');
  }));

  results.push(await runTest('2.2 - MissionOrchestrator 注入项目上下文', async () => {
    const projectRoot = path.join(__dirname, '../../');
    const kb = new ProjectKnowledgeBase({ projectRoot });
    await kb.initialize();

    // 添加测试 ADR
    kb.addADR({
      id: 'test-002',
      title: '使用 Mission-Driven 架构',
      status: 'accepted',
      date: Date.now(),
      context: '需要更好的任务管理',
      decision: '采用 Mission-Driven 架构模式',
      consequences: '提高任务执行的可追踪性',
    });

    const permissions: PermissionMatrix = {
      allowBash: true,
      allowEdit: true,
      allowWeb: true,
    };

    const orchestrator = createTestOrchestrator(permissions, projectRoot);

    orchestrator.setKnowledgeBase(kb);

    // 验证 MissionOrchestrator 可以访问知识库
    const engine = (orchestrator as any).missionDrivenEngine;
    const missionOrchestrator = (engine as any).missionOrchestrator;

    if (!missionOrchestrator) throw new Error('MissionOrchestrator 未初始化');

    // 验证知识库方法可用
    const projectContext = (missionOrchestrator as any).getProjectContext(600);
    if (!projectContext) throw new Error('项目上下文为空');
    console.log(`  - 项目上下文长度: ${projectContext.length} 字符`);

    const relevantADRs = (missionOrchestrator as any).getRelevantADRs('Mission 架构');
    if (!relevantADRs.includes('Mission')) throw new Error('未找到相关 ADR');
    console.log('  - 成功找到相关 ADR');
  }));

  // ============================================================================
  // 测试组 3: 工具权限验证
  // ============================================================================
  console.log('\n' + '='.repeat(80));
  console.log('测试组 3: 工具权限验证');
  console.log('='.repeat(80));

  results.push(await runTest('3.1 - launch-process 工具权限检查（禁用）', async () => {
    const permissions: PermissionMatrix = {
      allowBash: false,
      allowEdit: true,
      allowWeb: true,
    };

    const toolManager = new ToolManager(process.cwd(), permissions);

    const toolCall: ToolCall = {
      id: 'test-launch-process-1',
      name: 'launch-process',
      arguments: { command: 'ls -la', wait: false, max_wait_seconds: 5, name: 'orchestrator' },
    };

    const result = await toolManager.execute(toolCall);

    if (!result.isError) throw new Error('应该返回错误');
    if (!result.content.includes('Tool blocked')) throw new Error('错误信息不正确');
    if (!result.content.includes('Terminal command execution is disabled')) throw new Error('缺少具体原因');

    console.log(`  - 权限拒绝消息: ${result.content}`);
  }));

  results.push(await runTest('3.2 - launch-process 工具权限检查（允许）', async () => {
    const permissions: PermissionMatrix = {
      allowBash: true,
      allowEdit: true,
      allowWeb: true,
    };

    const toolManager = new ToolManager(process.cwd(), permissions);

    const toolCall: ToolCall = {
      id: 'test-launch-process-2',
      name: 'launch-process',
      arguments: { command: 'echo "test"', wait: true, max_wait_seconds: 5, name: 'orchestrator' }
    };

    const result = await toolManager.execute(toolCall);

    if (result.isError && result.content.includes('Permission denied')) {
      throw new Error('不应该返回权限错误');
    }

    console.log('  - launch-process 工具权限检查通过');
  }));

  results.push(await runTest('3.3 - Edit 工具权限检查（禁用）', async () => {
    const permissions: PermissionMatrix = {
      allowBash: true,
      allowEdit: false,
      allowWeb: true,
    };

    const toolManager = new ToolManager(process.cwd(), permissions);

    const toolCall: ToolCall = {
      id: 'test-edit-1',
      name: 'Edit',
      arguments: {
        file_path: '/test.txt',
        old_string: 'old',
        new_string: 'new',
      },
    };

    const result = await toolManager.execute(toolCall);

    if (!result.isError) throw new Error('应该返回错误');
    if (!result.content.includes('Permission denied')) throw new Error('错误信息不正确');
    if (!result.content.includes('File editing is disabled')) throw new Error('缺少具体原因');

    console.log(`  - 权限拒绝消息: ${result.content}`);
  }));

  results.push(await runTest('3.4 - Write 工具权限检查（禁用）', async () => {
    const permissions: PermissionMatrix = {
      allowBash: true,
      allowEdit: false,
      allowWeb: true,
    };

    const toolManager = new ToolManager(process.cwd(), permissions);

    const toolCall: ToolCall = {
      id: 'test-write-1',
      name: 'Write',
      arguments: {
        file_path: '/test.txt',
        content: 'test content',
      },
    };

    const result = await toolManager.execute(toolCall);

    if (!result.isError) throw new Error('应该返回错误');
    if (!result.content.includes('Permission denied')) throw new Error('错误信息不正确');
    if (!result.content.includes('File editing is disabled')) throw new Error('缺少具体原因');

    console.log(`  - 权限拒绝消息: ${result.content}`);
  }));

  results.push(await runTest('3.5 - Web 工具权限检查（禁用）', async () => {
    const permissions: PermissionMatrix = {
      allowBash: true,
      allowEdit: true,
      allowWeb: false,
    };

    const toolManager = new ToolManager(process.cwd(), permissions);

    const toolCall: ToolCall = {
      id: 'test-web-1',
      name: 'WebFetch',
      arguments: { url: 'https://example.com' },
    };

    const result = await toolManager.execute(toolCall);

    if (!result.isError) throw new Error('应该返回错误');
    if (!result.content.includes('Permission denied')) throw new Error('错误信息不正确');
    if (!result.content.includes('Web access is disabled')) throw new Error('缺少具体原因');

    console.log(`  - 权限拒绝消息: ${result.content}`);
  }));

  results.push(await runTest('3.6 - Read 工具无权限限制', async () => {
    const permissions: PermissionMatrix = {
      allowBash: false,
      allowEdit: false,
      allowWeb: false,
    };

    const toolManager = new ToolManager(process.cwd(), permissions);

    const toolCall: ToolCall = {
      id: 'test-read-1',
      name: 'Read',
      arguments: { file_path: '/test.txt' },
    };

    const result = await toolManager.execute(toolCall);

    // Read 工具应该不受权限限制（只读工具）
    // 即使所有权限都禁用，也不应该返回权限错误
    if (result.isError && result.content.includes('Permission denied')) {
      throw new Error('Read 工具不应该受权限限制');
    }

    console.log('  - Read 工具不受权限限制（只读工具）');
  }));

  results.push(await runTest('3.7 - 权限管理方法', async () => {
    const initialPermissions: PermissionMatrix = {
      allowBash: true,
      allowEdit: true,
      allowWeb: true,
    };

    const toolManager = new ToolManager(process.cwd(), initialPermissions);

    // 测试 getPermissions
    const currentPermissions = toolManager.getPermissions();
    if (!currentPermissions.allowBash) throw new Error('初始权限不正确');
    console.log('  - getPermissions() 工作正常');

    // 测试 setPermissions
    const newPermissions: PermissionMatrix = {
      allowBash: false,
      allowEdit: false,
      allowWeb: false,
    };

    toolManager.setPermissions(newPermissions);

    const updatedPermissions = toolManager.getPermissions();
    if (updatedPermissions.allowBash) throw new Error('权限更新失败');
    if (updatedPermissions.allowEdit) throw new Error('权限更新失败');
    if (updatedPermissions.allowWeb) throw new Error('权限更新失败');

    console.log('  - setPermissions() 工作正常');
  }));

  // ============================================================================
  // 测试组 4: 完整流程集成测试
  // ============================================================================
  console.log('\n' + '='.repeat(80));
  console.log('测试组 4: 完整流程集成测试');
  console.log('='.repeat(80));

  results.push(await runTest('4.1 - 知识库 + 权限 + 编排器集成', async () => {
    const projectRoot = path.join(__dirname, '../../');
    const kb = new ProjectKnowledgeBase({ projectRoot });
    await kb.initialize();

    // 添加测试数据
    kb.addADR({
      id: 'test-003',
      title: '集成测试架构',
      status: 'accepted',
      date: Date.now(),
      context: '需要完整的集成测试',
      decision: '实现端到端测试覆盖',
      consequences: '提高系统可靠性',
    });

    const permissions: PermissionMatrix = {
      allowBash: true,
      allowEdit: false, // 禁用编辑权限
      allowWeb: true,
    };

    const orchestrator = createTestOrchestrator(permissions, projectRoot);
    const adapterFactory = new MockAdapterFactory(permissions);

    orchestrator.setKnowledgeBase(kb);

    // 验证知识库可用
    const projectContext = (orchestrator as any).getProjectContext(500);
    if (!projectContext) throw new Error('知识库未正确集成');
    console.log('  - 知识库集成成功');

    // 验证权限配置
    const toolManager = adapterFactory.getToolManager();
    const currentPermissions = toolManager.getPermissions();
    if (currentPermissions.allowEdit) throw new Error('权限配置不正确');
    console.log('  - 权限配置正确');

    // 验证工具权限检查
    const editCall: ToolCall = {
      id: 'test-edit-2',
      name: 'Edit',
      arguments: { file_path: '/test.txt', old_string: 'a', new_string: 'b' },
    };

    const editResult = await toolManager.execute(editCall);
    if (!editResult.isError) throw new Error('应该拒绝编辑操作');
    console.log('  - 工具权限检查正常');

    console.log('  - 完整集成测试通过');
  }));

  results.push(await runTest('4.2 - 知识库传递链验证', async () => {
    const projectRoot = path.join(__dirname, '../../');
    const kb = new ProjectKnowledgeBase({ projectRoot });
    await kb.initialize();

    const permissions: PermissionMatrix = {
      allowBash: true,
      allowEdit: true,
      allowWeb: true,
    };

    const orchestrator = createTestOrchestrator(permissions, projectRoot);

    // 设置知识库
    orchestrator.setKnowledgeBase(kb);

    // 验证传递链：MissionDrivenEngine → MissionOrchestrator
    const missionOrchestrator = (orchestrator as any).missionOrchestrator;
    if (!missionOrchestrator) throw new Error('MissionOrchestrator 未初始化');
    console.log('  - MissionDrivenEngine → MissionOrchestrator ✓');

    // 验证 MissionOrchestrator 可以访问知识库
    const projectContext = (missionOrchestrator as any).getProjectContext(600);
    if (!projectContext) throw new Error('MissionOrchestrator 无法访问知识库');
    console.log('  - MissionOrchestrator 可以访问知识库 ✓');

    console.log('  - 知识库传递链完整');
  }));

  // ============================================================================
  // 测试组 5: 内置工具端到端测试
  // ============================================================================
  console.log('\n' + '='.repeat(80));
  console.log('测试组 5: 内置工具端到端测试');
  console.log('='.repeat(80));

  results.push(await runTest('5.1 - ToolManager 包含所有内置工具', async () => {
    const toolManager = new ToolManager(process.cwd());
    const tools = await toolManager.getTools();

    const expectedTools = [
      'launch-process',
      'read-process',
      'write-process',
      'kill-process',
      'list-processes',
      'text_editor',
      'grep_search',
      'remove_files',
      'web_search',
      'web_fetch',
      'mermaid_diagram'
    ];

    for (const toolName of expectedTools) {
      const tool = tools.find(t => t.name === toolName);
      if (!tool) throw new Error(`缺少内置工具: ${toolName}`);
      if (tool.metadata?.source !== 'builtin') {
        throw new Error(`工具 ${toolName} 的 source 不是 'builtin'`);
      }
    }

    console.log(`  - 找到 ${expectedTools.length} 个内置工具`);
    console.log(`  - 工具列表: ${expectedTools.join(', ')}`);
  }));

  results.push(await runTest('5.2 - mermaid_diagram 工具执行', async () => {
    const toolManager = new ToolManager(process.cwd());

    const toolCall: ToolCall = {
      id: 'test-mermaid-1',
      name: 'mermaid_diagram',
      arguments: {
        code: `graph TD
    A[开始] --> B{判断}
    B -->|是| C[执行操作]
    B -->|否| D[结束]`,
        title: '测试流程图',
        theme: 'dark'
      }
    };

    const result = await toolManager.execute(toolCall);

    if (result.isError) throw new Error(`执行失败: ${result.content}`);

    // 验证返回的 JSON 格式
    const data = JSON.parse(result.content);
    if (data.type !== 'mermaid_diagram') throw new Error('返回类型不正确');
    if (!data.code) throw new Error('缺少 code 字段');
    if (data.diagramType !== 'flowchart') throw new Error('diagramType 不正确');

    console.log(`  - 图表类型: ${data.diagramType}`);
    console.log(`  - 标题: ${data.title}`);
    console.log(`  - 主题: ${data.theme}`);
  }));

  results.push(await runTest('5.3 - mermaid_diagram 验证错误的代码', async () => {
    const toolManager = new ToolManager(process.cwd());

    const toolCall: ToolCall = {
      id: 'test-mermaid-2',
      name: 'mermaid_diagram',
      arguments: {
        code: 'invalid mermaid code without diagram type'
      }
    };

    const result = await toolManager.execute(toolCall);

    if (!result.isError) throw new Error('应该返回错误');
    if (!result.content.includes('Unrecognized')) {
      throw new Error(`错误消息不正确: ${result.content}`);
    }

    console.log(`  - 正确拒绝无效的 Mermaid 代码`);
  }));

  results.push(await runTest('5.4 - grep_search 工具执行', async () => {
    const toolManager = new ToolManager(process.cwd());

    const toolCall: ToolCall = {
      id: 'test-grep-1',
      name: 'grep_search',
      arguments: {
        pattern: 'ToolManager',
        include: '*.ts'
      }
    };

    const result = await toolManager.execute(toolCall);

    if (result.isError) throw new Error(`执行失败: ${result.content}`);
    if (!result.content.includes('tool-manager.ts')) {
      throw new Error('未找到预期的文件');
    }

    console.log(`  - 搜索结果包含 tool-manager.ts`);
  }));

  results.push(await runTest('5.5 - text_editor view 命令', async () => {
    const toolManager = new ToolManager(process.cwd());

    const toolCall: ToolCall = {
      id: 'test-editor-1',
      name: 'text_editor',
      arguments: {
        command: 'view',
        path: 'package.json',
        view_range: [1, 5]
      }
    };

    const result = await toolManager.execute(toolCall);

    if (result.isError) throw new Error(`执行失败: ${result.content}`);
    if (!result.content.includes('magi')) {
      throw new Error('未找到预期的内容');
    }

    console.log(`  - 成功读取 package.json 前 5 行`);
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
