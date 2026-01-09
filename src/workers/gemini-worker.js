"use strict";
/**
 * Gemini Worker
 * Gemini CLI 执行器
 */
Object.defineProperty(exports, "__esModule", { value: true });
exports.GeminiWorker = void 0;
exports.createGeminiWorker = createGeminiWorker;
const base_worker_1 = require("./base-worker");
/**
 * Gemini CLI Worker
 */
class GeminiWorker extends base_worker_1.BaseWorker {
    geminiConfig;
    constructor(config) {
        // 转换为 WorkerConfig，排除 sandbox 布尔值
        const { sandbox: _sandbox, ...baseConfig } = config;
        super({ ...baseConfig, cliType: 'gemini' });
        this.geminiConfig = config;
    }
    get cliType() {
        return 'gemini';
    }
    /** 构建 Gemini CLI 命令参数 */
    buildArgs(subTask) {
        const args = [];
        // 添加提示内容
        args.push('-p', this.buildPrompt(subTask));
        // 非交互模式
        args.push('--non-interactive');
        // 自动运行工具
        args.push('--auto-run-tools');
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
    /** 解析 Gemini CLI 输出 */
    parseOutput(output) {
        // Gemini 输出解析
        return {};
    }
}
exports.GeminiWorker = GeminiWorker;
/** 创建 Gemini Worker 的工厂函数 */
function createGeminiWorker(cliPath = 'gemini', workingDirectory, timeout = 300000) {
    return new GeminiWorker({
        cliType: 'gemini',
        cliPath,
        timeout,
        workingDirectory,
    });
}
//# sourceMappingURL=gemini-worker.js.map