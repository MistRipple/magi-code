"use strict";
/**
 * Orchestrator - 核心编排器
 * 负责任务分解、Worker 调度、结果收集
 */
Object.defineProperty(exports, "__esModule", { value: true });
exports.Orchestrator = void 0;
const cli_detector_1 = require("./cli-detector");
const claude_worker_1 = require("./workers/claude-worker");
const codex_worker_1 = require("./workers/codex-worker");
const gemini_worker_1 = require("./workers/gemini-worker");
const events_1 = require("./events");
/**
 * Orchestrator 编排器
 */
class Orchestrator {
    options;
    cliDetector;
    workers = new Map();
    isRunning = false;
    constructor(options) {
        this.options = options;
        this.cliDetector = new cli_detector_1.CLIDetector();
        this.initWorkers();
    }
    /** 初始化 Workers */
    initWorkers() {
        const { workspaceRoot, timeout = 300000 } = this.options;
        this.workers.set('claude', (0, claude_worker_1.createClaudeWorker)('claude', workspaceRoot, timeout));
        this.workers.set('codex', (0, codex_worker_1.createCodexWorker)('codex', workspaceRoot, timeout));
        this.workers.set('gemini', (0, gemini_worker_1.createGeminiWorker)('gemini', workspaceRoot, timeout));
    }
    /** 执行任务 */
    async executeTask(taskId) {
        const task = this.options.taskManager.getTask(taskId);
        if (!task)
            throw new Error(`Task 不存在: ${taskId}`);
        this.isRunning = true;
        this.options.taskManager.updateTaskStatus(taskId, 'running');
        try {
            const statuses = await this.cliDetector.checkAllCLIs();
            const availableCLIs = statuses.filter(s => s.available).map(s => s.type);
            if (availableCLIs.length === 0)
                throw new Error('没有可用的 CLI 工具');
            const category = this.categorizeTask(task.prompt);
            const cli = this.selectBestCLI(category, availableCLIs);
            const files = this.extractTargetFiles(task.prompt);
            this.options.taskManager.addSubTask(taskId, task.prompt, category, cli, files);
            const updatedTask = this.options.taskManager.getTask(taskId);
            if (updatedTask)
                await this.executeSubTasks(updatedTask);
            this.options.taskManager.updateTaskStatus(taskId, 'completed');
        }
        catch (error) {
            const msg = error instanceof Error ? error.message : String(error);
            events_1.globalEventBus.emitEvent('task:failed', { taskId, data: { error: msg } });
            this.options.taskManager.updateTaskStatus(taskId, 'failed');
            throw error;
        }
        finally {
            this.isRunning = false;
        }
    }
    categorizeTask(prompt) {
        const p = prompt.toLowerCase();
        if (['重构', '优化', 'refactor', 'optimize'].some(k => p.includes(k)))
            return 'refactor';
        if (['测试', 'test'].some(k => p.includes(k)))
            return 'test';
        if (['文档', '注释', 'doc', 'comment'].some(k => p.includes(k)))
            return 'document';
        if (['调试', 'debug', 'fix', 'bug'].some(k => p.includes(k)))
            return 'debug';
        if (['审查', 'review'].some(k => p.includes(k)))
            return 'review';
        if (['架构', 'architecture', 'design'].some(k => p.includes(k)))
            return 'architecture';
        if (['前端', 'frontend', 'ui', 'css'].some(k => p.includes(k)))
            return 'frontend';
        return 'implement';
    }
    selectBestCLI(category, available) {
        const map = {
            'architecture': ['claude', 'gemini', 'codex'],
            'implement': ['claude', 'codex', 'gemini'],
            'refactor': ['claude', 'codex', 'gemini'],
            'bugfix': ['claude', 'codex', 'gemini'],
            'debug': ['claude', 'codex', 'gemini'],
            'frontend': ['claude', 'gemini', 'codex'],
            'test': ['codex', 'claude', 'gemini'],
            'document': ['claude', 'gemini', 'codex'],
            'review': ['claude', 'gemini', 'codex'],
            'general': ['claude', 'codex', 'gemini'],
        };
        for (const cli of map[category] || []) {
            if (available.includes(cli))
                return cli;
        }
        return available[0] || 'claude';
    }
    extractTargetFiles(prompt) {
        const m = prompt.match(/[\w\-./]+\.(ts|js|tsx|jsx|py|java|go|rs|cpp|c|css|html|json|md)/gi);
        return m ? [...new Set(m)] : [];
    }
    async executeSubTasks(task) {
        const results = [];
        const mode = this.options.mode || 'sequential';
        if (mode === 'parallel') {
            results.push(...await Promise.all(task.subTasks.map(st => this.executeSubTask(st))));
        }
        else {
            for (const st of task.subTasks) {
                const r = await this.executeSubTask(st);
                results.push(r);
                if (!r.success)
                    break;
            }
        }
        return results;
    }
    async executeSubTask(subTask) {
        const worker = this.workers.get(subTask.assignedCli);
        if (!worker) {
            return { workerId: `unknown-${subTask.id}`, cliType: subTask.assignedCli, success: false,
                error: `Worker 不存在: ${subTask.assignedCli}`, duration: 0, timestamp: new Date() };
        }
        for (const f of subTask.targetFiles) {
            this.options.snapshotManager.createSnapshot(f, subTask.assignedCli, subTask.id);
        }
        this.options.taskManager.updateSubTaskStatus(subTask.taskId, subTask.id, 'running');
        const result = await worker.execute({ subTask, workingDirectory: this.options.workspaceRoot });
        this.options.taskManager.updateSubTaskStatus(subTask.taskId, subTask.id, result.success ? 'completed' : 'failed');
        return result;
    }
    interrupt() {
        if (!this.isRunning)
            return;
        for (const w of this.workers.values())
            w.interrupt();
        this.isRunning = false;
    }
    get running() { return this.isRunning; }
    getWorker(cli) { return this.workers.get(cli); }
}
exports.Orchestrator = Orchestrator;
//# sourceMappingURL=orchestrator.js.map