"use strict";
/**
 * Codex CLI 适配器
 *
 * Codex CLI (OpenAI) 使用独立进程模式，每次调用启动新进程。
 * 支持 --full-auto 模式自动执行文件修改。
 * 支持 exec resume 恢复之前的会话。
 *
 * 输出格式：JSONL 格式
 */
Object.defineProperty(exports, "__esModule", { value: true });
exports.CodexAdapter = void 0;
const events_1 = require("events");
const child_process_1 = require("child_process");
const types_1 = require("../types");
/**
 * Codex CLI 适配器
 * 每次 sendMessage 启动新进程
 * 支持会话恢复功能
 */
class CodexAdapter extends events_1.EventEmitter {
    type = 'codex';
    config;
    _state = 'idle';
    currentProcess = null;
    sessionId = null;
    /**
     * 检查 Codex CLI 是否已安装
     */
    static async checkInstalled() {
        return new Promise((resolve) => {
            const proc = (0, child_process_1.spawn)('codex', ['--version'], { stdio: ['ignore', 'pipe', 'pipe'] });
            proc.on('error', () => resolve(false));
            proc.on('close', (code) => resolve(code === 0));
            setTimeout(() => { proc.kill(); resolve(false); }, 3000);
        });
    }
    /**
     * 使用 Codex CLI 描述图片内容
     *
     * ⚠️ 注意：此方法现在仅用于以下场景：
     * - Codex 会话恢复模式（exec resume 不支持 -i 参数）
     *
     * Claude 和 Gemini CLI 已原生支持图片识别：
     * - Claude: 通过 Read 工具 + analyze_image MCP 工具
     * - Gemini: 通过 read_file 工具 + 内置多模态能力
     *
     * @param imagePaths 图片路径数组
     * @param cwd 工作目录
     * @param timeout 超时时间（毫秒）
     * @returns 图片描述文本
     */
    static async describeImages(imagePaths, cwd, timeout = 60000) {
        if (!imagePaths || imagePaths.length === 0) {
            return '';
        }
        console.log('[CodexAdapter] describeImages 开始, 图片数量:', imagePaths.length);
        return new Promise((resolve, reject) => {
            // 构建参数：使用简洁的提示词让 Codex 描述图片
            const args = ['exec', '--full-auto', '--json', '--skip-git-repo-check'];
            for (const imgPath of imagePaths) {
                args.push('-i', imgPath);
            }
            // 使用 -- 分隔符确保 prompt 不被当作 -i 的参数
            args.push('--', '请简洁地描述这些图片的内容，重点关注与编程/开发相关的信息。如果是代码截图，请提取关键代码。如果是UI截图，请描述界面元素和布局。');
            console.log('[CodexAdapter] describeImages 启动进程: codex', args.join(' '));
            const proc = (0, child_process_1.spawn)('codex', args, {
                cwd: cwd || process.cwd(),
                env: process.env,
                stdio: ['pipe', 'pipe', 'pipe'],
            });
            proc.stdin?.end();
            let output = '';
            const timeoutHandle = setTimeout(() => {
                proc.kill();
                console.log('[CodexAdapter] describeImages 超时');
                reject(new Error('图片描述超时'));
            }, timeout);
            proc.stdout?.on('data', (data) => {
                output += data.toString();
            });
            proc.stderr?.on('data', (data) => {
                output += data.toString();
            });
            proc.on('close', (code) => {
                clearTimeout(timeoutHandle);
                console.log('[CodexAdapter] describeImages 进程关闭, code:', code);
                // 解析输出，提取描述文本
                const description = CodexAdapter.extractDescriptionFromOutput(output);
                if (description) {
                    console.log('[CodexAdapter] describeImages 成功:', description.substring(0, 100));
                    resolve(description);
                }
                else if (code !== 0) {
                    console.log('[CodexAdapter] describeImages 失败');
                    reject(new Error('图片描述失败'));
                }
                else {
                    resolve('（无法获取图片描述）');
                }
            });
            proc.on('error', (err) => {
                clearTimeout(timeoutHandle);
                console.log('[CodexAdapter] describeImages 进程错误:', err.message);
                reject(err);
            });
        });
    }
    /**
     * 从 Codex 输出中提取描述文本
     */
    static extractDescriptionFromOutput(output) {
        const lines = output.split('\n');
        const descriptions = [];
        for (const line of lines) {
            if (!line.trim())
                continue;
            try {
                const msg = JSON.parse(line);
                if (msg.type === 'item.completed' && msg.item?.type === 'agent_message' && msg.item?.text) {
                    descriptions.push(msg.item.text);
                }
            }
            catch {
                // 忽略非 JSON 行
            }
        }
        return descriptions.join('\n').trim();
    }
    constructor(config) {
        super();
        this.config = { ...config, type: 'codex', timeout: config.timeout || 5 * 60 * 1000 };
    }
    get state() {
        return this._state;
    }
    get isConnected() {
        return this._state !== 'error';
    }
    get isBusy() {
        return this._state === 'busy';
    }
    /** 获取 CLI 能力 */
    get capabilities() {
        return types_1.CLI_CAPABILITIES.codex;
    }
    setState(state) {
        if (this._state !== state) {
            this._state = state;
            this.emit('stateChange', state);
        }
    }
    /** 连接（Codex CLI 不需要持久连接） */
    async connect() {
        this.setState('ready');
    }
    /** 断开连接 */
    async disconnect() {
        if (this.currentProcess) {
            this.currentProcess.kill();
            this.currentProcess = null;
        }
        this.setState('disconnected');
    }
    /** 发送消息（支持图片） */
    async sendMessage(message, imagePaths) {
        if (this.isBusy) {
            throw new Error('Codex CLI is busy');
        }
        this.setState('busy');
        console.log('[CodexAdapter] sendMessage 开始, message:', message.substring(0, 50));
        console.log('[CodexAdapter] 图片数量:', imagePaths?.length || 0);
        return new Promise((resolve, reject) => {
            const args = this.buildArgs(message, imagePaths);
            let output = '';
            console.log('[CodexAdapter] 启动进程: codex', args.join(' '));
            this.currentProcess = (0, child_process_1.spawn)('codex', args, {
                cwd: this.config.cwd,
                env: { ...process.env, ...this.config.env },
                stdio: ['pipe', 'pipe', 'pipe'],
            });
            // 关闭 stdin
            this.currentProcess.stdin?.end();
            console.log('[CodexAdapter] 进程已启动, PID:', this.currentProcess.pid);
            // 设置超时
            const timeout = setTimeout(() => {
                console.log('[CodexAdapter] 超时!');
                this.currentProcess?.kill();
                this.setState('ready');
                reject(new Error('Codex CLI timeout'));
            }, this.config.timeout);
            this.currentProcess.stdout?.on('data', (data) => {
                const chunk = data.toString();
                console.log('[CodexAdapter] stdout:', chunk.substring(0, 100));
                output += chunk;
                this.emit('output', chunk);
            });
            this.currentProcess.stderr?.on('data', (data) => {
                const chunk = data.toString();
                console.log('[CodexAdapter] stderr:', chunk.substring(0, 100));
                output += chunk;
                this.emit('output', chunk);
            });
            this.currentProcess.on('close', (code) => {
                console.log('[CodexAdapter] 进程关闭, code:', code, 'output length:', output.length);
                clearTimeout(timeout);
                this.currentProcess = null;
                this.setState('ready');
                const response = this.parseOutput(output);
                console.log('[CodexAdapter] 解析结果:', response.content?.substring(0, 100));
                // 提取 session_id
                this.extractSessionId(output);
                if (code !== 0 && !response.content) {
                    console.log('[CodexAdapter] 错误退出');
                    reject(new Error(`Codex CLI exited with code ${code}`));
                }
                else {
                    console.log('[CodexAdapter] 成功完成');
                    this.emit('response', response);
                    resolve(response);
                }
            });
            this.currentProcess.on('error', (err) => {
                console.log('[CodexAdapter] 进程错误:', err.message);
                clearTimeout(timeout);
                this.currentProcess = null;
                this.setState('error');
                this.emit('error', err);
                reject(err);
            });
        });
    }
    /** 中断当前操作 */
    async interrupt() {
        if (this.currentProcess) {
            this.currentProcess.kill('SIGINT');
            this.currentProcess = null;
        }
        this.setState('ready');
    }
    /** 构建命令行参数 */
    buildArgs(message, imagePaths) {
        console.log('[CodexAdapter] buildArgs, 当前 sessionId:', this.sessionId);
        const hasImages = imagePaths && imagePaths.length > 0;
        // 如果有之前的 session_id，使用 exec resume 恢复会话
        // 注意：exec resume 不支持 -i 参数，如果有图片需要开启新会话
        // 注意：exec resume 不支持 --json 和 --skip-git-repo-check 参数
        if (this.sessionId && !hasImages) {
            // 格式: codex exec resume <SESSION_ID> "message"
            console.log('[CodexAdapter] 使用 resume 模式恢复会话');
            return ['exec', 'resume', this.sessionId, message];
        }
        // 新会话或有图片时：codex exec --full-auto --json --skip-git-repo-check [-i image]... -- "message"
        const args = ['exec', '--full-auto', '--json', '--skip-git-repo-check'];
        // 添加图片参数（Codex 使用 -i 参数）
        if (hasImages) {
            console.log('[CodexAdapter] 有图片，开启新会话（exec resume 不支持 -i 参数）');
            // 重置 sessionId，因为带图片需要新会话
            this.sessionId = null;
            for (const imgPath of imagePaths) {
                args.push('-i', imgPath);
            }
        }
        // 使用 -- 分隔符确保 message 不被当作 -i 的参数
        args.push('--', message);
        return args;
    }
    /** 从输出中提取 session_id (thread_id) */
    extractSessionId(output) {
        console.log('[CodexAdapter] extractSessionId 开始解析...');
        const lines = output.trim().split('\n');
        for (const line of lines) {
            try {
                const msg = JSON.parse(line);
                // Codex 使用 thread_id 作为会话标识
                if (msg.type === 'thread.started' && msg.thread_id) {
                    const oldSessionId = this.sessionId;
                    this.sessionId = msg.thread_id;
                    console.log('[CodexAdapter] 提取到 thread_id:', this.sessionId, '(之前:', oldSessionId, ')');
                    return;
                }
            }
            catch {
                // 忽略非 JSON 行
            }
        }
        console.log('[CodexAdapter] 警告: 未能从输出中提取 thread_id');
    }
    /** 获取当前会话 ID */
    getSessionId() {
        return this.sessionId;
    }
    /** 设置会话 ID（用于恢复之前的会话） */
    setSessionId(sessionId) {
        this.sessionId = sessionId;
        console.log('[CodexAdapter] 设置 sessionId:', sessionId);
    }
    /** 重置会话（开始新对话） */
    resetSession() {
        this.sessionId = null;
        console.log('[CodexAdapter] 重置会话');
    }
    /** 解析 Codex CLI 输出（支持 JSONL 和纯文本两种格式） */
    parseOutput(output) {
        const lines = output.split('\n');
        const fileChanges = [];
        const contentParts = [];
        const plainTextParts = [];
        let error;
        let hasJsonOutput = false;
        for (const line of lines) {
            if (!line.trim())
                continue;
            try {
                const msg = JSON.parse(line);
                hasJsonOutput = true;
                // 解析 item.completed 消息
                if (msg.type === 'item.completed' && msg.item) {
                    const item = msg.item;
                    // agent_message 包含助手回复
                    if (item.type === 'agent_message' && item.text) {
                        contentParts.push(item.text);
                    }
                    // reasoning 包含思考过程
                    if (item.type === 'reasoning' && item.text) {
                        contentParts.push(`[思考] ${item.text}`);
                    }
                    // 文件操作
                    if (item.type === 'file_write' || item.type === 'file_edit') {
                        fileChanges.push({
                            filePath: item.path || item.file || 'unknown',
                            type: item.type === 'file_write' ? 'create' : 'modify',
                        });
                    }
                }
                // 错误消息
                if (msg.type === 'error' || msg.error) {
                    error = msg.error || msg.message || 'Unknown error';
                }
            }
            catch {
                // 非 JSON 行，收集为纯文本（用于 exec resume 模式）
                const trimmedLine = line.trim();
                if (trimmedLine) {
                    // 检查是否是错误信息
                    if (trimmedLine.toLowerCase().includes('error:') || trimmedLine.toLowerCase().includes('failed')) {
                        if (!error)
                            error = trimmedLine;
                    }
                    else {
                        // 过滤掉一些无用的输出行
                        if (!trimmedLine.startsWith('Resuming session') &&
                            !trimmedLine.startsWith('Session resumed') &&
                            !trimmedLine.match(/^[─━═]+$/)) { // 过滤分隔线
                            plainTextParts.push(trimmedLine);
                        }
                    }
                }
            }
        }
        // 如果没有 JSON 输出，使用纯文本内容（exec resume 模式）
        const content = hasJsonOutput
            ? contentParts.join('\n').trim()
            : plainTextParts.join('\n').trim();
        return {
            content,
            done: true,
            fileChanges: fileChanges.length > 0 ? fileChanges : undefined,
            error,
            raw: output,
        };
    }
}
exports.CodexAdapter = CodexAdapter;
//# sourceMappingURL=codex.js.map