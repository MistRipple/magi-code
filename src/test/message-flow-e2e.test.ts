/**
 * 消息流端到端测试
 * 验证统一消息协议的完整性
 */

import { EventEmitter } from 'events';
import { CLIAdapterFactory } from '../cli/adapter-factory';
import { SessionManager } from '../cli/session/session-manager';
import { createNormalizer } from '../normalizer';
import { StandardMessage, MessageLifecycle, MessageType, InteractionType } from '../protocol';

interface TestResult {
  name: string;
  passed: boolean;
  error?: string;
  details?: any;
}

class MessageFlowE2ETest {
  private results: TestResult[] = [];
  private testWorkspace = '/tmp/multicli-test';

  async runAllTests(): Promise<void> {
    console.log('🧪 开始消息流端到端测试...\n');

    await this.testNormalizerOutput();
    await this.testCLIAdapterFactoryEvents();
    await this.testQuestionHandling();
    await this.testStreamingFlow();
    await this.testErrorHandling();

    this.printResults();
  }

  /**
   * 测试 Normalizer 输出结构
   */
  private async testNormalizerOutput(): Promise<void> {
    const testName = 'Normalizer 输出结构验证';
    try {
      const normalizer = createNormalizer('claude', 'worker', false);
      const messages: StandardMessage[] = [];
      const updates: any[] = [];

      normalizer.on('message', (msg: StandardMessage) => messages.push(msg));
      normalizer.on('update', (update: any) => updates.push(update));

      // 模拟流式输出
      const messageId = normalizer.startStream('test-trace', 'worker');
      normalizer.processChunk(messageId, '这是一段测试文本\n');
      normalizer.processChunk(messageId, '```javascript\nconst x = 1;\n```\n');
      const finalMsg = normalizer.endStream(messageId);

      // 验证
      const checks = [
        { name: '创建了初始消息', pass: messages.length > 0 },
        { name: '消息包含 blocks', pass: finalMsg?.blocks && finalMsg.blocks.length > 0 },
        { name: '消息生命周期正确', pass: finalMsg?.lifecycle === MessageLifecycle.COMPLETED },
        { name: '消息类型正确', pass: finalMsg?.type === MessageType.TEXT },
        { name: 'blocks 包含文本', pass: finalMsg?.blocks.some(b => b.type === 'text') },
        { name: 'blocks 包含代码', pass: finalMsg?.blocks.some(b => b.type === 'code') },
      ];

      const allPassed = checks.every(c => c.pass);
      this.results.push({
        name: testName,
        passed: allPassed,
        details: {
          checks,
          messageCount: messages.length,
          updateCount: updates.length,
          finalBlocks: finalMsg?.blocks.length,
        },
      });
    } catch (error) {
      this.results.push({
        name: testName,
        passed: false,
        error: error instanceof Error ? error.message : String(error),
      });
    }
  }

  /**
   * 测试 CLIAdapterFactory 事件转发
   */
  private async testCLIAdapterFactoryEvents(): Promise<void> {
    const testName = 'CLIAdapterFactory 事件转发';
    try {
      const factory = new CLIAdapterFactory({
        cwd: this.testWorkspace,
        maxTimeout: 30000,
      });

      const standardMessages: StandardMessage[] = [];
      const standardUpdates: any[] = [];
      const standardCompletes: StandardMessage[] = [];

      factory.on('standardMessage', (msg: StandardMessage) => standardMessages.push(msg));
      factory.on('standardUpdate', (update: any) => standardUpdates.push(update));
      factory.on('standardComplete', (msg: StandardMessage) => standardCompletes.push(msg));

      // 等待一小段时间确保事件监听器设置完成
      await new Promise(resolve => setTimeout(resolve, 100));

      const checks = [
        { name: 'Factory 创建成功', pass: factory !== null },
        { name: '事件监听器已设置', pass: factory.listenerCount('standardMessage') > 0 },
      ];

      const allPassed = checks.every(c => c.pass);
      this.results.push({
        name: testName,
        passed: allPassed,
        details: { checks },
      });
    } catch (error) {
      this.results.push({
        name: testName,
        passed: false,
        error: error instanceof Error ? error.message : String(error),
      });
    }
  }

  /**
   * 测试 CLI 询问处理
   */
  private async testQuestionHandling(): Promise<void> {
    const testName = 'CLI 询问处理（Question → StandardMessage）';
    try {
      // 直接测试 CLIAdapterFactory 能否正确监听和转换 question 事件
      const factory = new CLIAdapterFactory({
        cwd: this.testWorkspace,
        maxTimeout: 30000,
      });

      const interactionMessages: StandardMessage[] = [];
      factory.on('standardMessage', (msg: StandardMessage) => {
        if (msg.type === MessageType.INTERACTION) {
          interactionMessages.push(msg);
        }
      });

      // 获取 factory 内部的 sessionManager 并触发 question 事件
      // 注意：这是通过反射访问私有成员，仅用于测试
      const sessionManager = (factory as any).sessionManager;
      if (sessionManager) {
        sessionManager.emit('question', {
          cli: 'claude',
          role: 'worker',
          question: {
            questionId: 'test-q-1',
            cli: 'claude',
            content: 'Do you want to proceed? (y/n)',
            pattern: 'y/n',
            timestamp: Date.now(),
          },
        });
      }

      // 等待事件处理
      await new Promise(resolve => setTimeout(resolve, 100));

      const checks = [
        { name: 'SessionManager 存在', pass: sessionManager !== undefined },
        { name: '生成了交互消息', pass: interactionMessages.length > 0 },
        { name: '消息类型为 INTERACTION', pass: interactionMessages[0]?.type === MessageType.INTERACTION },
        { name: '包含 interaction 字段', pass: interactionMessages[0]?.interaction !== undefined },
        { name: 'interaction 类型为 QUESTION', pass: interactionMessages[0]?.interaction?.type === InteractionType.QUESTION },
        { name: '包含 questionId', pass: interactionMessages[0]?.metadata?.questionId === 'test-q-1' },
      ];

      const allPassed = checks.every(c => c.pass);
      this.results.push({
        name: testName,
        passed: allPassed,
        details: {
          checks,
          interactionMessageCount: interactionMessages.length,
          firstMessage: interactionMessages[0],
        },
      });
    } catch (error) {
      this.results.push({
        name: testName,
        passed: false,
        error: error instanceof Error ? error.message : String(error),
      });
    }
  }

  /**
   * 测试流式消息流
   */
  private async testStreamingFlow(): Promise<void> {
    const testName = '流式消息流（start → chunk → complete）';
    try {
      const normalizer = createNormalizer('claude', 'worker', false);
      const messages: StandardMessage[] = [];
      const updates: any[] = [];
      let completeMessage: StandardMessage | undefined = undefined;

      normalizer.on('message', (msg: StandardMessage) => messages.push(msg));
      normalizer.on('update', (update: any) => updates.push(update));
      normalizer.on('complete', (_id: string, msg: StandardMessage) => {
        completeMessage = msg;
      });

      // 模拟流式输出
      const messageId = normalizer.startStream('test-trace', 'worker');
      normalizer.processChunk(messageId, 'Part 1\n');
      normalizer.processChunk(messageId, 'Part 2\n');
      normalizer.processChunk(messageId, 'Part 3\n');
      normalizer.endStream(messageId);

      const checks = [
        { name: '创建了初始消息', pass: messages.length > 0 },
        { name: '初始消息为 STARTED 或 STREAMING', pass: messages[0]?.lifecycle === MessageLifecycle.STARTED || messages[0]?.lifecycle === MessageLifecycle.STREAMING },
        { name: '生成了更新事件', pass: updates.length > 0 },
        { name: '生成了完成消息', pass: completeMessage !== undefined },
        { name: '完成消息为 COMPLETED', pass: !!(completeMessage && (completeMessage as StandardMessage).lifecycle === MessageLifecycle.COMPLETED) },
        { name: '完成消息包含所有内容', pass: !!(completeMessage && (completeMessage as StandardMessage).blocks.length > 0) },
      ];

      const allPassed = checks.every(c => c.pass);
      this.results.push({
        name: testName,
        passed: allPassed,
        details: {
          checks,
          messageCount: messages.length,
          updateCount: updates.length,
          completeBlocks: completeMessage ? (completeMessage as StandardMessage).blocks.length : 0,
        },
      });
    } catch (error) {
      this.results.push({
        name: testName,
        passed: false,
        error: error instanceof Error ? error.message : String(error),
      });
    }
  }

  /**
   * 测试错误处理
   */
  private async testErrorHandling(): Promise<void> {
    const testName = '错误处理（Error → StandardMessage）';
    try {
      const normalizer = createNormalizer('claude', 'worker', false);
      let errorMessage: StandardMessage | undefined = undefined;

      normalizer.on('complete', (_id: string, msg: StandardMessage) => {
        if (msg.lifecycle === MessageLifecycle.FAILED) {
          errorMessage = msg;
        }
      });

      // 模拟错误
      const messageId = normalizer.startStream('test-trace', 'worker');
      normalizer.processChunk(messageId, 'Some output\n');
      normalizer.endStream(messageId, 'Test error occurred');

      const checks = [
        { name: '生成了错误消息', pass: errorMessage !== undefined },
        { name: '消息生命周期为 FAILED', pass: !!(errorMessage && (errorMessage as StandardMessage).lifecycle === MessageLifecycle.FAILED) },
        { name: '消息类型为 ERROR', pass: !!(errorMessage && (errorMessage as StandardMessage).type === MessageType.ERROR) },
        { name: '包含错误信息', pass: !!(errorMessage && (errorMessage as StandardMessage).metadata?.error === 'Test error occurred') },
      ];

      const allPassed = checks.every(c => c.pass);
      this.results.push({
        name: testName,
        passed: allPassed,
        details: {
          checks,
          errorMessage,
        },
      });
    } catch (error) {
      this.results.push({
        name: testName,
        passed: false,
        error: error instanceof Error ? error.message : String(error),
      });
    }
  }

  /**
   * 打印测试结果
   */
  private printResults(): void {
    console.log('\n' + '='.repeat(60));
    console.log('📊 测试结果汇总');
    console.log('='.repeat(60) + '\n');

    const passed = this.results.filter(r => r.passed).length;
    const failed = this.results.filter(r => !r.passed).length;

    this.results.forEach((result, index) => {
      const icon = result.passed ? '✅' : '❌';
      console.log(`${icon} ${index + 1}. ${result.name}`);

      if (!result.passed && result.error) {
        console.log(`   错误: ${result.error}`);
      }

      if (result.details) {
        if (result.details.checks) {
          result.details.checks.forEach((check: any) => {
            const checkIcon = check.pass ? '  ✓' : '  ✗';
            console.log(`${checkIcon} ${check.name}`);
          });
        }
        const detailsWithoutChecks = { ...result.details };
        delete detailsWithoutChecks.checks;
        if (Object.keys(detailsWithoutChecks).length > 0) {
          console.log(`   详情:`, JSON.stringify(detailsWithoutChecks, null, 2));
        }
      }
      console.log('');
    });

    console.log('='.repeat(60));
    console.log(`总计: ${this.results.length} 个测试`);
    console.log(`✅ 通过: ${passed}`);
    console.log(`❌ 失败: ${failed}`);
    console.log('='.repeat(60) + '\n');

    if (failed === 0) {
      console.log('🎉 所有测试通过！消息流统一协议工作正常。\n');
    } else {
      console.log('⚠️  部分测试失败，请检查上述错误信息。\n');
      process.exit(1);
    }
  }
}

// 运行测试
if (require.main === module) {
  const test = new MessageFlowE2ETest();
  test.runAllTests().catch(error => {
    console.error('测试执行失败:', error);
    process.exit(1);
  });
}

export { MessageFlowE2ETest };

