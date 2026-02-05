import 'reflect-metadata'; // Required for DI
import { EventEmitter } from 'events';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';

// --- 1. VS Code Environment Mock ---
const Module = require('module');
const originalRequire = Module.prototype.require;
Module.prototype.require = function(id: string) {
  if (id === 'vscode') {
    const mockDisposable = { dispose: () => {} };
    const mockEventEmitter = () => mockDisposable;
    return {
      window: {
        createOutputChannel: () => ({ append: () => {}, appendLine: () => {}, show: () => {}, dispose: () => {} }),
        showInformationMessage: () => Promise.resolve(),
        showErrorMessage: () => Promise.resolve(),
        showWarningMessage: () => Promise.resolve(),
        showQuickPick: () => Promise.resolve(undefined),
        showInputBox: () => Promise.resolve(undefined),
        createTerminal: () => ({
          show: () => {},
          sendText: () => {},
          dispose: () => {},
          processId: Promise.resolve(12345),
        }),
        onDidCloseTerminal: mockEventEmitter,
        onDidOpenTerminal: mockEventEmitter,
        activeTextEditor: undefined,
        visibleTextEditors: [],
        terminals: [],
      },
      workspace: {
        workspaceFolders: [{ uri: { fsPath: process.cwd() } }],
        getConfiguration: () => ({
            get: (key: string) => {
                if (key === 'multiCli.timeout') return 60000;
                return undefined;
            },
            update: () => Promise.resolve()
        }),
        onDidChangeConfiguration: mockEventEmitter,
        onDidSaveTextDocument: mockEventEmitter,
        onDidOpenTextDocument: mockEventEmitter,
        onDidCloseTextDocument: mockEventEmitter,
        fs: {
          readFile: () => Promise.resolve(Buffer.from('')),
          writeFile: () => Promise.resolve(),
          stat: () => Promise.resolve({ type: 1 }),
        },
      },
      commands: {
        registerCommand: () => mockDisposable,
        executeCommand: () => Promise.resolve(),
      },
      ExtensionContext: class {},
      Uri: {
        file: (p: string) => ({ fsPath: p, scheme: 'file', path: p }),
        parse: (s: string) => ({ fsPath: s, scheme: 'file', path: s }),
      },
      FileType: { File: 1, Directory: 2 },
      EventEmitter: class { event = () => {}; fire = () => {}; dispose = () => {}; },
      Disposable: class { dispose = () => {}; static from = () => ({ dispose: () => {} }); },
      Range: class { constructor(public start: any, public end: any) {} },
      Position: class { constructor(public line: number, public character: number) {} },
      ThemeIcon: class { constructor(public id: string) {} },
    };
  }
  return originalRequire.apply(this, arguments);
};

// --- Imports after Mocking ---
import { MissionDrivenEngine } from '../src/orchestrator/core/mission-driven-engine';
import { LLMAdapterFactory } from '../src/llm/adapter-factory';
import { UnifiedSessionManager } from '../src/session/unified-session-manager';
import { SnapshotManager } from '../src/snapshot-manager';
import { MessageHub } from '../src/orchestrator/core/message-hub';
import { StandardMessage } from '../src/protocol/message-protocol';
import { LogCategory, logger } from '../src/logging';

// --- 3. Test Runner ---
async function runScenario() {
    console.log('🎬 Starting Real LLM Orchestration Test\n');

    const workspaceRoot = process.cwd();
    const sessionManager = new UnifiedSessionManager(workspaceRoot);
    const snapshotManager = new SnapshotManager(sessionManager, workspaceRoot);

    // 使用真实 LLM 适配器工厂
    const adapterFactory = new LLMAdapterFactory({ cwd: workspaceRoot });

    // 创建 MessageHub（真实 LLM 需要）
    const messageHub = new MessageHub();
    adapterFactory.setMessageHub(messageHub);

    // 初始化适配器工厂（加载配置、Skills、MCP）
    console.log('📦 Initializing adapter factory...');
    await adapterFactory.initialize();
    console.log('✅ Adapter factory initialized\n');

    const engine = new MissionDrivenEngine(
        adapterFactory,
        {
            timeout: 120000,  // 2 分钟超时
            maxRetries: 1,
            strategy: { enableVerification: false }
        } as any,
        workspaceRoot,
        snapshotManager,
        sessionManager
    );

    // 初始化引擎
    try {
        console.log('🔧 Initializing engine...');
        await engine.initialize();
        console.log('✅ Engine initialized\n');
    } catch (e: any) {
        console.warn('⚠️ Engine init warning:', e.message);
    }

    const hub = engine.getMessageHub();
    const timeline: string[] = [];

    // 监听消息事件
    hub.on('unified:message', (msg: StandardMessage) => {
        let event = '';
        const getBlockContent = (block: any) => block?.type === 'text' || block?.type === 'thinking' ? block.content : '';

        if (msg.source === 'orchestrator') {
            if (msg.type === 'text') {
                const content = getBlockContent(msg.blocks?.[0]);
                event = `🤖 ORCHESTRATOR: ${content?.substring(0, 80)}...`;
            }
            if (msg.type === 'plan') event = `📋 PLAN GENERATED`;
            if (msg.type === 'thinking') {
                const content = getBlockContent(msg.blocks?.[0]);
                event = `💭 ORCHESTRATOR THINKING: ${content?.substring(0, 60)}...`;
            }
            // 方案 B：使用 MessageType.TASK_CARD 识别
            if (msg.type === 'task_card' && msg.metadata?.subTaskCard) {
                const card = msg.metadata.subTaskCard as any;
                event = `🎫 CARD: [${card.worker}] ${card.title} (${card.status})`;
            }
            // 汇总消息使用 'result' 类型（TASK_CARD 已独立）
            if (msg.type === 'result') {
                const content = getBlockContent(msg.blocks?.[0]);
                event = `🏁 SUMMARY: ${content?.substring(0, 80)}`;
            }
        } else if (msg.source === 'worker') {
            if (msg.type === 'text') {
                const content = getBlockContent(msg.blocks?.[0]);
                event = `👷 WORKER (${msg.agent}): ${content?.substring(0, 60)}...`;
            }
            if (msg.type === 'thinking') {
                const content = getBlockContent(msg.blocks?.[0]);
                event = `💭 WORKER (${msg.agent}) THINKING: ${content?.substring(0, 40)}...`;
            }
            if (msg.type === 'tool_call') {
                const toolBlock = msg.blocks?.find((b: any) => b.type === 'tool_call') as any;
                event = `🔧 WORKER (${msg.agent}) TOOL: ${toolBlock?.toolName || 'unknown'}`;
            }
            if (msg.type === 'result') {
                const content = getBlockContent(msg.blocks?.[0]);
                event = `✅ WORKER DONE (${msg.agent}): ${content?.substring(0, 60)}`;
            }
        }

        if (event) {
            timeline.push(event);
            console.log(event);
        }
    });

    // 获取用户输入的任务
    const task = process.argv[2] || '帮我创建一个简单的 hello world TypeScript 文件';

    console.log(`🚀 Executing Task: "${task}"\n`);
    console.log('─'.repeat(80));

    try {
        await engine.execute(task, `test-task-${Date.now()}`);
    } catch (error: any) {
        console.error('❌ Execution error:', error.message);
    }

    console.log('─'.repeat(80));

    // --- Validation ---
    console.log('\n📊 Execution Summary:');

    const orchestratorMessages = timeline.filter(t => t.includes('🤖 ORCHESTRATOR') || t.includes('💭 ORCHESTRATOR'));
    const cards = timeline.filter(t => t.includes('🎫 CARD'));
    const workerOutputs = timeline.filter(t => t.includes('👷 WORKER') || t.includes('💭 WORKER'));
    const toolCalls = timeline.filter(t => t.includes('🔧 WORKER'));
    const summary = timeline.filter(t => t.includes('🏁 SUMMARY'));

    console.log(`  • Orchestrator messages: ${orchestratorMessages.length}`);
    console.log(`  • SubTask cards: ${cards.length}`);
    console.log(`  • Worker outputs: ${workerOutputs.length}`);
    console.log(`  • Tool calls: ${toolCalls.length}`);
    console.log(`  • Final summary: ${summary.length > 0 ? '✅' : '❌'}`);

    hub.dispose();
    engine.dispose();
}

runScenario().catch(e => {
    console.error('Fatal error:', e);
    process.exit(1);
});
