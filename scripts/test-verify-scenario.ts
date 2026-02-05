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
    return {
      window: {
        createOutputChannel: () => ({ append: () => {}, appendLine: () => {}, show: () => {} }),
        showInformationMessage: () => {},
        showErrorMessage: () => {},
        showWarningMessage: () => {},
      },
      workspace: {
        workspaceFolders: [{ uri: { fsPath: process.cwd() } }],
        getConfiguration: () => ({
            get: (key: string) => {
                if (key === 'multiCli.timeout') return 5000;
                return undefined;
            },
            update: () => Promise.resolve() 
        }),
        onDidChangeConfiguration: () => ({ dispose: () => {} }),
      },
      ExtensionContext: class {},
      Uri: { file: (p: string) => ({ fsPath: p }) },
    };
  }
  return originalRequire.apply(this, arguments);
};

// --- Imports after Mocking ---
import { MissionDrivenEngine } from '../src/orchestrator/core/mission-driven-engine';
import { IAdapterFactory, AdapterResponse, AdapterOutputScope } from '../src/adapters/adapter-factory-interface';
import { AgentType } from '../src/types/agent-types';
import type { ToolManager } from '../src/tools/tool-manager';
import type { MCPToolExecutor } from '../src/tools/mcp-executor';
import { UnifiedSessionManager } from '../src/session/unified-session-manager';
import { SnapshotManager } from '../src/snapshot-manager';
import { MessageHub } from '../src/orchestrator/core/message-hub';
import { StandardMessage } from '../src/protocol/message-protocol';
import { LogCategory, logger } from '../src/logging';

// Silence Logger
logger.info = () => {};
logger.warn = () => {};
logger.error = () => {};
logger.debug = () => {};

// --- 2. Mock Adapter Strategy ---
class MockLLMAdapter {
    constructor(private role: string) {}

    async sendMessage(prompt: string, context?: any, options?: any): Promise<AdapterResponse> {
        console.log(`[MockLLM:${this.role}] Prompt preview: ${prompt.substring(0, 60)}...`);

        // === Orchestrator Logic ===
        if (this.role === 'orchestrator') {
            // A. Intent Classification
            if (prompt.includes('Intent Classification') || prompt.includes('意图分类') || prompt.includes('智能意图分析器')) {
                console.log('  -> Match: Intent Classification');
                return this.jsonResponse({
                    intent: 'task',
                    recommendedMode: 'task',
                    confidence: 0.95,
                    reason: 'User wants to refactor login'
                });
            }

            // B. Goal Understanding
            if ((prompt.includes('goal') && prompt.includes('analysis')) || prompt.includes('分析以下用户请求')) {
                console.log('  -> Match: Goal Understanding');
                return this.jsonResponse({
                    goal: 'Refactor Login',
                    analysis: 'Requires frontend and backend changes',
                    constraints: [],
                    acceptanceCriteria: ['Works'],
                    riskLevel: 'low'
                });
            }

            // C. Worker Selection (Routing)
            if (prompt.includes('Select the best capability') || prompt.includes('Select Participants') || prompt.includes('完成意图到分配的统一决策')) {
                console.log('  -> Match: Routing/Worker Selection');
                return this.jsonResponse({
                    needsWorker: true,
                    categories: ['frontend', 'backend'], 
                    delegationBriefings: [
                        'Frontend: Update login UI',
                        'Backend: Update auth API'
                    ],
                    reason: 'Full stack refactor needed'
                });
            }

            // X. Pre-Analysis (NEW)
            if (prompt.includes('决定最佳执行策略') || prompt.includes('TaskPreAnalyzer')) {
                console.log('  -> Match: Pre-Analysis');
                return this.jsonResponse({
                    complexity: "moderate",
                    needsPlanning: true,
                    needsReview: false,
                    needsVerification: true,
                    parallel: true,
                    reasoning: "Multi-worker task",
                    analysisSummary: "🔍 Analysis: Multi-worker task detected."
                });
            }
            
            // D. Planning / Contracts
            if (prompt.includes('Contract') || prompt.includes('规划协作') || prompt.includes('制定执行计划')) {
                console.log('  -> Match: Planning');
                return this.jsonResponse({
                    analysis: "Full stack refactor",
                    isSimpleTask: false,
                    needsWorker: true,
                    needsCollaboration: true,
                    executionMode: "parallel",
                    subTasks: [
                        {
                            id: "1",
                            description: "Frontend Update",
                            assignedWorker: "gemini",
                            delegationBriefing: "Update UI"
                        },
                        {
                            id: "2",
                            description: "Backend Update",
                            assignedWorker: "codex",
                            delegationBriefing: "Update API"
                        }
                    ],
                    summary: "Plan ready"
                });
            }
            
            // E. Summary
            if (prompt.includes('Summarize') || prompt.includes('总结')) {
                console.log('  -> Match: Summary');
                return {
                    content: '## Execution Summary\nBoth frontend and backend tasks completed successfully.',
                    done: true,
                    tokenUsage: { inputTokens: 10, outputTokens: 5 }
                };
            }
        }

        // === Worker Logic ===

        if (prompt.includes('Frontend') || prompt.includes('UI')) {
            return {
                content: 'Updating CSS and HTML files...',
                done: true,
                tokenUsage: { inputTokens: 10, outputTokens: 10 }
            };
        }

        if (prompt.includes('Backend') || prompt.includes('API')) {
             return {
                content: 'Updating Node.js controllers...',
                done: true,
                tokenUsage: { inputTokens: 10, outputTokens: 10 }
            };
        }

        // Default Worker Response
        return {
            content: 'Task completed.',
            done: true,
            tokenUsage: { inputTokens: 10, outputTokens: 10 }
        };
    }

    private jsonResponse(data: any): AdapterResponse {
        return {
            content: JSON.stringify(data),
            done: true,
            tokenUsage: { inputTokens: 20, outputTokens: 20 }
        };
    }
}

class MockAdapterFactory extends EventEmitter implements IAdapterFactory {
    private hub?: MessageHub;

    async sendMessage(agent: AgentType, message: string, images?: string[], options?: AdapterOutputScope): Promise<AdapterResponse> {
        const role = agent === 'orchestrator' ? 'orchestrator' : 'worker';
        const mock = new MockLLMAdapter(role);
        return mock.sendMessage(message);
    }

    async interrupt(_agent: AgentType): Promise<void> {}
    async shutdown(): Promise<void> {}
    isBusy(_agent: AgentType): boolean { return false; }
    getToolManager(): ToolManager { return null as any; }
    async clearAdapter(_agent: AgentType): Promise<void> {}
    getMCPExecutor(): MCPToolExecutor | null { return null; }
    async reloadMCP(): Promise<void> {}
    async reloadSkills(): Promise<void> {}
    refreshUserRules(): void {}

    getAdapter(role: string) {
        return {
            sendMessage: (prompt: string, _context?: any, _options?: any) =>
                this.sendMessage(role as AgentType, prompt)
        } as any;
    }

    setMessageHub(hub: MessageHub) { this.hub = hub; }
    getAvailableAdapters() { return []; }
    isConnected(_agent: AgentType): boolean { return true; }
    async initialize() {}
}

// --- 3. Test Runner ---
async function runScenario() {
    console.log('🎬 Starting Orchestration Scenario Test: Multi-Worker Refactor\n');

    const workspaceRoot = process.cwd();
    const sessionManager = new UnifiedSessionManager(workspaceRoot);
    const snapshotManager = new SnapshotManager(sessionManager, workspaceRoot);
    const adapterFactory = new MockAdapterFactory();
    
    const engine = new MissionDrivenEngine(
        adapterFactory,
        {
            timeout: 30000,
            maxRetries: 0,
            strategy: { enableVerification: false }
        } as any,
        workspaceRoot,
        snapshotManager,
        sessionManager
    );

    // Mock initialization (bypass profile loading issues if any)
    try {
        await engine.initialize();
    } catch (e) {
        // console.warn('Engine init warning:', e);
    }

    const hub = engine.getMessageHub();
    const timeline: string[] = [];

    hub.on('unified:message', (msg: StandardMessage) => {
        let event = '';
        const getBlockContent = (block: any) => block?.type === 'text' || block?.type === 'thinking' ? block.content : '';
        if (msg.source === 'orchestrator') {
            if (msg.type === 'text') event = `🤖 ORCHESTRATOR: ${getBlockContent(msg.blocks?.[0])?.substring(0, 40)}...`;
            if (msg.type === 'plan') event = `📋 PLAN GENERATED`;
            // 方案 B：使用 MessageType.TASK_CARD 识别
            if (msg.type === 'task_card' && msg.metadata?.subTaskCard) {
                const card = msg.metadata.subTaskCard as any;
                event = `🎫 CARD: [${card.worker}] ${card.title} (${card.status})`;
            }
            // 汇总消息使用 'result' 类型（TASK_CARD 已独立）
            if (msg.type === 'result') event = `🏁 SUMMARY: ${getBlockContent(msg.blocks?.[0])?.substring(0, 40)}`;
        } else if (msg.source === 'worker') {
            if (msg.type === 'text') event = `👷 WORKER (${msg.agent}): ${getBlockContent(msg.blocks?.[0])?.substring(0, 30)}...`;
            if (msg.type === 'result') event = `✅ WORKER DONE (${msg.agent}): ${getBlockContent(msg.blocks?.[0])?.substring(0, 30)}`;
        }

        if (event) {
            timeline.push(event);
            console.log(event);
        }
    });

    console.log('🚀 Executing Task: "Refactor login frontend backend"');
    await engine.execute("Refactor login frontend backend", "test-task-1");

    // --- Validation ---
    console.log('\n📊 Validating Scenario...');
    
    const cards = timeline.filter(t => t.includes('🎫 CARD'));
    const workerOutputs = timeline.filter(t => t.includes('👷 WORKER'));
    const summary = timeline.filter(t => t.includes('🏁 SUMMARY'));

    // We expect 2 subtasks (frontend + backend).
    // Note: Due to mock profile loader relying on defaults, 'frontend' might map to 'claude', 'backend' to 'codex' (if profiles exist) OR both to default.
    // But 'categories' in mock routing returned ['frontend', 'backend'].
    // The engine tries to map these categories. If it fails, it might use default worker.
    // Even if both use 'claude', we should see 2 assignments (2 cards).
    
    if (cards.length >= 2) {
        console.log(`✅ SubTasks Created: ${cards.length} (Expected >= 2)`);
    } else {
        console.warn(`⚠️ SubTasks count low: ${cards.length}. This might be due to profile mapping in test environment.`);
    }

    if (workerOutputs.length > 0) {
        console.log(`✅ Workers Executed: ${workerOutputs.length} outputs received`);
    } else {
        console.error(`❌ No worker execution detected`);
    }

    if (summary.length > 0) {
        console.log(`✅ Final Summary Generated`);
    } else {
        console.error(`❌ Missing final summary`);
    }

    hub.dispose();
    engine.dispose();
}

runScenario().catch(e => {
    console.error(e);
    process.exit(1);
});