"use strict";
/**
 * Codex Worker
 * Codex CLI 执行器
 */
Object.defineProperty(exports, "__esModule", { value: true });
exports.CodexWorker = void 0;
exports.createCodexWorker = createCodexWorker;
const base_worker_1 = require("./base-worker");
/**
 * Codex CLI Worker
 */
class CodexWorker extends base_worker_1.BaseWorker {
    codexConfig;
    constructor(config) {
        super(config);
        this.codexConfig = config;
    }
    get cliType() {
        return 'codex';
    }
    /** 构建 Codex CLI 命令参数 */
    buildArgs(subTask) {
        const args = [];
        // 使用 exec 模式直接执行
        args.push('exec');
        // 添加提示内容
        args.push(this.buildPrompt(subTask));
        // 自动批准模式
        const approval = this.codexConfig.approval ?? 'full-auto';
        args.push('--approval', approval);
        // 静默模式
        args.push('--quiet');
        return args;
    }
    /** 构建提示词 */
    buildPrompt(subTask) {
        let prompt = subTask.description;
        if (subTask.targetFiles.length > 0) {
            prompt += `\n\n目标文件: ${subTask.targetFiles.join(', ')}`;
        }
        prompt += '\n\n请直接修改文件，完成后简要说明所做的更改。';
        return prompt;
    }
    /** 解析 Codex CLI 输出 */
    parseOutput(output) {
        // Codex 输出解析
        return {};
    }
}
exports.CodexWorker = CodexWorker;
/** 创建 Codex Worker 的工厂函数 */
function createCodexWorker(cliPath = 'codex', workingDirectory, timeout = 300000) {
    return new CodexWorker({
        cliType: 'codex',
        cliPath,
        timeout,
        workingDirectory,
        approval: 'full-auto',
    });
}
//# sourceMappingURL=codex-worker.js.map