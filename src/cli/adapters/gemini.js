"use strict";
/**
 * Gemini CLI 适配器
 *
 * Gemini CLI (Google) 使用独立进程模式，每次调用启动新进程。
 * 支持自动执行文件修改。
 * 支持 --resume 恢复之前的会话。
 *
 * 输出格式：JSONL 格式（使用 --output-format json）
 */
Object.defineProperty(exports, "__esModule", { value: true });
exports.GeminiAdapter = void 0;
const events_1 = require("events");
const child_process_1 = require("child_process");
const types_1 = require("../types");
/**
 * Gemini CLI 适配器
 * 每次 sendMessage 启动新进程
 * 支持会话恢复功能
 */
class GeminiAdapter extends events_1.EventEmitter {
    type = 'gemini';
    config;
    _state = 'idle';
    currentProcess = null;
    sessionId = null;
    /**
     * 检查 Gemini CLI 是否已安装
     */
    static async checkInstalled() {
        return new Promise((resolve) => {
            const proc = (0, child_process_1.spawn)('gemini', ['--version'], { stdio: ['ignore', 'pipe', 'pipe'] });
            proc.on('error', () => resolve(false));
            proc.on('close', (code) => resolve(code === 0));
            setTimeout(() => { proc.kill(); resolve(false); }, 3000);
        });
    }
    constructor(config) {
        super();
        this.config = { ...config, type: 'gemini', timeout: config.timeout || 5 * 60 * 1000 };
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
        return types_1.CLI_CAPABILITIES.gemini;
    }
    setState(state) {
        if (this._state !== state) {
            this._state = state;
            this.emit('stateChange', state);
        }
    }
    /** 连接（Gemini CLI 不需要持久连接） */
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
    /** 发送消息（Gemini CLI 通过 read_file 工具读取图片，使用内置多模态能力分析） */
    async sendMessage(message, imagePaths) {
        if (this.isBusy) {
            throw new Error('Gemini CLI is busy');
        }
        // Gemini CLI 通过 read_file 工具读取图片，然后使用内置多模态能力分析
        // 使用明确的文件路径格式，让 Gemini 识别需要读取本地文件
        let finalMessage = message;
        if (imagePaths && imagePaths.length > 0) {
            const imageRefs = imagePaths.map((p, i) => `图片${i + 1}: ${p}`).join('\n');
            finalMessage = `请先读取并分析以下本地图片文件：\n${imageRefs}\n\n然后回答：${message}`;
            console.log('[GeminiAdapter] 已将图片路径添加到 prompt 中:', imagePaths);
        }
        this.setState('busy');
        console.log('[GeminiAdapter] sendMessage 开始, message:', finalMessage.substring(0, 100));
        return new Promise((resolve, reject) => {
            const args = this.buildArgs(finalMessage);
            let output = '';
            console.log('[GeminiAdapter] 启动进程: gemini', args.join(' '));
            this.currentProcess = (0, child_process_1.spawn)('gemini', args, {
                cwd: this.config.cwd,
                env: { ...process.env, ...this.config.env },
                stdio: ['pipe', 'pipe', 'pipe'],
            });
            // 关闭 stdin
            this.currentProcess.stdin?.end();
            console.log('[GeminiAdapter] 进程已启动, PID:', this.currentProcess.pid);
            // 设置超时
            const timeout = setTimeout(() => {
                console.log('[GeminiAdapter] 超时!');
                this.currentProcess?.kill();
                this.setState('ready');
                reject(new Error('Gemini CLI timeout'));
            }, this.config.timeout);
            this.currentProcess.stdout?.on('data', (data) => {
                const chunk = data.toString();
                console.log('[GeminiAdapter] stdout:', chunk.substring(0, 100));
                output += chunk;
                this.emit('output', chunk);
            });
            this.currentProcess.stderr?.on('data', (data) => {
                const chunk = data.toString();
                console.log('[GeminiAdapter] stderr:', chunk.substring(0, 100));
                output += chunk;
                this.emit('output', chunk);
            });
            this.currentProcess.on('close', (code) => {
                console.log('[GeminiAdapter] 进程关闭, code:', code, 'output length:', output.length);
                clearTimeout(timeout);
                this.currentProcess = null;
                this.setState('ready');
                const response = this.parseOutput(output);
                console.log('[GeminiAdapter] 解析结果:', response.content?.substring(0, 100));
                // 提取 session_id
                this.extractSessionId(output);
                if (code !== 0 && !response.content) {
                    console.log('[GeminiAdapter] 错误退出');
                    reject(new Error(`Gemini CLI exited with code ${code}`));
                }
                else {
                    console.log('[GeminiAdapter] 成功完成');
                    this.emit('response', response);
                    resolve(response);
                }
            });
            this.currentProcess.on('error', (err) => {
                console.log('[GeminiAdapter] 进程错误:', err.message);
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
    /** 构建命令行参数（Gemini CLI 不支持图片） */
    buildArgs(message) {
        console.log('[GeminiAdapter] buildArgs, 当前 sessionId:', this.sessionId);
        // Gemini CLI 参数格式: gemini --yolo --output-format stream-json "message"
        // --yolo: 自动执行，无需确认
        // --output-format stream-json: 实时流式 JSON 输出
        // --resume: 恢复之前的会话
        const args = ['--yolo', '--output-format', 'stream-json'];
        // 如果有之前的 session_id，使用 --resume 恢复会话
        if (this.sessionId) {
            args.push('--resume', this.sessionId);
        }
        args.push(message);
        return args;
    }
    /** 从输出中提取 session_id */
    extractSessionId(output) {
        console.log('[GeminiAdapter] extractSessionId 开始解析...');
        // Gemini JSON 输出可能被系统消息包围，需要提取 JSON 部分
        const jsonMatch = output.match(/\{[\s\S]*"session_id"[\s\S]*\}/);
        if (jsonMatch) {
            try {
                const json = JSON.parse(jsonMatch[0]);
                if (json.session_id) {
                    const oldSessionId = this.sessionId;
                    this.sessionId = json.session_id;
                    console.log('[GeminiAdapter] 提取到 session_id:', this.sessionId, '(之前:', oldSessionId, ')');
                    return;
                }
            }
            catch (e) {
                console.log('[GeminiAdapter] JSON 解析失败:', e);
            }
        }
        console.log('[GeminiAdapter] 警告: 未能从输出中提取 session_id');
    }
    /** 获取当前会话 ID */
    getSessionId() {
        return this.sessionId;
    }
    /** 设置会话 ID（用于恢复之前的会话） */
    setSessionId(sessionId) {
        this.sessionId = sessionId;
        console.log('[GeminiAdapter] 设置 sessionId:', sessionId);
    }
    /** 重置会话（开始新对话） */
    resetSession() {
        this.sessionId = null;
        console.log('[GeminiAdapter] 重置会话');
    }
    /** 解析 Gemini CLI stream-json 输出 */
    parseOutput(output) {
        const fileChanges = [];
        let content = '';
        let error;
        // stream-json 格式：每行一个 JSON 事件
        // {"type":"init","session_id":"..."}
        // {"type":"message","role":"user","content":"..."}
        // {"type":"message","role":"assistant","content":"..."}
        // {"type":"result","response":"..."}
        const lines = output.trim().split('\n');
        for (const line of lines) {
            if (!line.trim())
                continue;
            // 跳过非 JSON 行（如 YOLO mode 提示）
            if (!line.trim().startsWith('{'))
                continue;
            try {
                const json = JSON.parse(line);
                // 提取 session_id
                if (json.session_id && !this.sessionId) {
                    this.sessionId = json.session_id;
                    console.log('[GeminiAdapter] 从 stream-json 提取 session_id:', this.sessionId);
                }
                // 提取响应内容
                if (json.type === 'message' && json.role === 'assistant' && json.content) {
                    content += json.content + '\n';
                }
                else if (json.type === 'result' && json.response) {
                    // 最终结果
                    if (!content)
                        content = json.response;
                }
                else if (json.response) {
                    // 兼容旧格式
                    content = json.response;
                }
                if (json.error) {
                    // 确保 error 是字符串
                    error = typeof json.error === 'string' ? json.error : JSON.stringify(json.error);
                }
            }
            catch (e) {
                // 忽略解析失败的行
            }
        }
        return {
            content: content.trim(),
            done: true,
            fileChanges: fileChanges.length > 0 ? fileChanges : undefined,
            error,
            raw: output,
        };
    }
}
exports.GeminiAdapter = GeminiAdapter;
//# sourceMappingURL=gemini.js.map