/**
 * 强制依赖注入测试
 *
 * 验证 CLAUDE.md 规范的核心要求：
 * - 禁止多种实现方式：只有一种依赖注入路径
 * - 禁止回退逻辑：无可选依赖的隐式回退
 * - 禁止打补丁：使用运行时验证替代条件检查
 *
 * 覆盖的组件：
 * 1. AutonomousWorker - SharedContextDependencies 强制注入验证
 */

import * as assert from 'assert';
import { AutonomousWorker, SharedContextDependencies } from '../orchestrator/worker';
import { ProfileLoader } from '../orchestrator/profile/profile-loader';
import { GuidanceInjector } from '../orchestrator/profile/guidance-injector';
import { TodoManager } from '../todo';
import {
  ContextAssembler,
  IFileSummaryCache,
  ISharedContextPool,
  FileSummary,
  SharedContextEntry,
  SharedContextEntryType,
  ContextSource,
  QueryOptions,
} from '../context';

// ============================================================================
// Mock 实现
// ============================================================================

/**
 * Mock FileSummaryCache - 符合 IFileSummaryCache 接口
 */
class MockFileSummaryCache implements IFileSummaryCache {
  private cache = new Map<string, { hash: string; summary: FileSummary }>();

  get(filePath: string, currentHash: string): FileSummary | null {
    const entry = this.cache.get(filePath);
    if (entry && entry.hash === currentHash) {
      return entry.summary;
    }
    return null;
  }

  set(filePath: string, fileHash: string, summary: FileSummary, _source: ContextSource): void {
    this.cache.set(filePath, { hash: fileHash, summary });
  }

  has(filePath: string, fileHash: string): boolean {
    const entry = this.cache.get(filePath);
    return entry !== undefined && entry.hash === fileHash;
  }
}

/**
 * Mock SharedContextPool - 符合 ISharedContextPool 接口
 */
class MockSharedContextPool implements ISharedContextPool {
  private entries = new Map<string, SharedContextEntry>();

  getByMission(missionId: string, _options?: QueryOptions): SharedContextEntry[] {
    return Array.from(this.entries.values()).filter((e) => e.missionId === missionId);
  }

  getByType(
    missionId: string,
    type: SharedContextEntryType,
    _maxTokens?: number
  ): SharedContextEntry[] {
    return Array.from(this.entries.values()).filter(
      (e) => e.missionId === missionId && e.type === type
    );
  }

  add(entry: SharedContextEntry): { action: 'added' | 'merged'; id?: string; existingId?: string } {
    this.entries.set(entry.id, entry);
    return { action: 'added', id: entry.id };
  }
}

// ============================================================================
// 测试框架
// ============================================================================

interface TestResult {
  name: string;
  passed: boolean;
  error?: string;
}

const results: TestResult[] = [];

function runTest(name: string, fn: () => void): void {
  try {
    fn();
    results.push({ name, passed: true });
    console.log(`✅ ${name}`);
  } catch (error) {
    const errorMsg = error instanceof Error ? error.message : String(error);
    results.push({ name, passed: false, error: errorMsg });
    console.log(`❌ ${name}`);
    console.log(`   错误: ${errorMsg}`);
  }
}

// ============================================================================
// 测试用例
// ============================================================================

function runAllTests(): void {
  console.log('\n========================================');
  console.log('强制依赖注入测试');
  console.log('========================================\n');

  console.log('--- AutonomousWorker 构造函数运行时验证 ---\n');

  const workspaceRoot = process.cwd();

  // 创建有效的依赖项（使用单例模式）
  const profileLoader = ProfileLoader.getInstance();
  const guidanceInjector = new GuidanceInjector();
  const todoManager = new TodoManager(workspaceRoot);
  const mockFileSummaryCache = new MockFileSummaryCache();
  const mockSharedContextPool = new MockSharedContextPool();
  // ContextAssembler 构造函数签名:
  // (projectKnowledgeBase, sharedContextPool, fileSummaryCache, memoryDocument, recentTurnsProvider?)
  const mockContextAssembler = new ContextAssembler(
    null, // projectKnowledgeBase
    mockSharedContextPool,
    mockFileSummaryCache,
    null // memoryDocument
  );

  const validDeps: SharedContextDependencies = {
    contextAssembler: mockContextAssembler,
    fileSummaryCache: mockFileSummaryCache,
    sharedContextPool: mockSharedContextPool,
  };

  // Test 1: 完整依赖 - 应该成功创建
  runTest('AutonomousWorker: 完整依赖应成功创建', () => {
    const worker = new AutonomousWorker(
      'claude',
      profileLoader,
      guidanceInjector,
      todoManager,
      validDeps
    );
    assert.ok(worker, 'Worker 应该被成功创建');
    assert.strictEqual(worker.getWorkerType(), 'claude', 'Worker 类型应该正确');
  });

  // Test 2: 缺少 todoManager - 应该抛出错误
  runTest('AutonomousWorker: 缺少 todoManager 应抛出错误', () => {
    let threw = false;
    let errorMessage = '';
    try {
      new AutonomousWorker(
        'claude',
        profileLoader,
        guidanceInjector,
        null as any, // 故意传入 null
        validDeps
      );
    } catch (e) {
      threw = true;
      errorMessage = e instanceof Error ? e.message : String(e);
    }
    assert.ok(threw, '应该抛出错误');
    assert.ok(
      errorMessage.includes('todoManager 为必需依赖'),
      `错误消息应包含 'todoManager 为必需依赖'，实际: ${errorMessage}`
    );
  });

  // Test 3: 缺少 sharedContextDeps - 应该抛出错误
  runTest('AutonomousWorker: 缺少 sharedContextDeps 应抛出错误', () => {
    let threw = false;
    let errorMessage = '';
    try {
      new AutonomousWorker(
        'gemini',
        profileLoader,
        guidanceInjector,
        todoManager,
        null as any // 故意传入 null
      );
    } catch (e) {
      threw = true;
      errorMessage = e instanceof Error ? e.message : String(e);
    }
    assert.ok(threw, '应该抛出错误');
    assert.ok(
      errorMessage.includes('sharedContextDeps 为必需依赖'),
      `错误消息应包含 'sharedContextDeps 为必需依赖'，实际: ${errorMessage}`
    );
  });

  // Test 4: 缺少 contextAssembler - 应该抛出错误
  runTest('AutonomousWorker: 缺少 contextAssembler 应抛出错误', () => {
    let threw = false;
    let errorMessage = '';
    try {
      new AutonomousWorker('codex', profileLoader, guidanceInjector, todoManager, {
        contextAssembler: null as any,
        fileSummaryCache: mockFileSummaryCache,
        sharedContextPool: mockSharedContextPool,
      });
    } catch (e) {
      threw = true;
      errorMessage = e instanceof Error ? e.message : String(e);
    }
    assert.ok(threw, '应该抛出错误');
    assert.ok(
      errorMessage.includes('缺少 contextAssembler'),
      `错误消息应包含 '缺少 contextAssembler'，实际: ${errorMessage}`
    );
  });

  // Test 5: 缺少 fileSummaryCache - 应该抛出错误
  runTest('AutonomousWorker: 缺少 fileSummaryCache 应抛出错误', () => {
    let threw = false;
    let errorMessage = '';
    try {
      new AutonomousWorker('claude', profileLoader, guidanceInjector, todoManager, {
        contextAssembler: mockContextAssembler,
        fileSummaryCache: null as any,
        sharedContextPool: mockSharedContextPool,
      });
    } catch (e) {
      threw = true;
      errorMessage = e instanceof Error ? e.message : String(e);
    }
    assert.ok(threw, '应该抛出错误');
    assert.ok(
      errorMessage.includes('缺少 fileSummaryCache'),
      `错误消息应包含 '缺少 fileSummaryCache'，实际: ${errorMessage}`
    );
  });

  // Test 6: 缺少 sharedContextPool - 应该抛出错误
  runTest('AutonomousWorker: 缺少 sharedContextPool 应抛出错误', () => {
    let threw = false;
    let errorMessage = '';
    try {
      new AutonomousWorker('gemini', profileLoader, guidanceInjector, todoManager, {
        contextAssembler: mockContextAssembler,
        fileSummaryCache: mockFileSummaryCache,
        sharedContextPool: null as any,
      });
    } catch (e) {
      threw = true;
      errorMessage = e instanceof Error ? e.message : String(e);
    }
    assert.ok(threw, '应该抛出错误');
    assert.ok(
      errorMessage.includes('缺少 sharedContextPool'),
      `错误消息应包含 '缺少 sharedContextPool'，实际: ${errorMessage}`
    );
  });

  // Test 7: 传入 undefined 而非 null - 同样应该抛出错误
  runTest('AutonomousWorker: 传入 undefined 依赖应抛出错误', () => {
    let threw = false;
    let errorMessage = '';
    try {
      new AutonomousWorker('claude', profileLoader, guidanceInjector, todoManager, {
        contextAssembler: undefined as any,
        fileSummaryCache: mockFileSummaryCache,
        sharedContextPool: mockSharedContextPool,
      });
    } catch (e) {
      threw = true;
      errorMessage = e instanceof Error ? e.message : String(e);
    }
    assert.ok(threw, '应该抛出错误');
    assert.ok(
      errorMessage.includes('缺少 contextAssembler'),
      `错误消息应包含 '缺少 contextAssembler'，实际: ${errorMessage}`
    );
  });

  // Test 8: 不同 WorkerSlot 类型验证
  runTest('AutonomousWorker: 不同 WorkerSlot 类型应正确设置', () => {
    const workerTypes: Array<'claude' | 'gemini' | 'codex'> = ['claude', 'gemini', 'codex'];

    for (const workerType of workerTypes) {
      const worker = new AutonomousWorker(
        workerType,
        profileLoader,
        guidanceInjector,
        todoManager,
        validDeps
      );
      assert.strictEqual(
        worker.getWorkerType(),
        workerType,
        `Worker 类型应为 ${workerType}`
      );
    }
  });

  // 输出测试结果摘要
  console.log('\n========================================');
  console.log('测试结果摘要');
  console.log('========================================\n');

  const passed = results.filter((r) => r.passed).length;
  const failed = results.filter((r) => !r.passed).length;

  console.log(`通过: ${passed}/${results.length}`);
  console.log(`失败: ${failed}/${results.length}`);

  if (failed > 0) {
    console.log('\n失败的测试:');
    results
      .filter((r) => !r.passed)
      .forEach((r) => {
        console.log(`  - ${r.name}: ${r.error}`);
      });
    process.exit(1);
  } else {
    console.log('\n✅ 所有测试通过！强制依赖注入规范验证成功。');
    console.log('\n验证内容:');
    console.log('  - AutonomousWorker 构造函数运行时验证');
    console.log('  - SharedContextDependencies 各字段强制性检查');
    console.log('  - 错误消息包含明确的依赖名称');
    process.exit(0);
  }
}

// 执行测试
runAllTests();
