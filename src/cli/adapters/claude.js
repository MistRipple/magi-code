"use strict";
/**
 * Claude CLI 适配器
 *
 * Claude CLI 使用 --print --output-format json 模式，每次调用是独立进程。
 * 使用 --continue 或 --resume <session_id> 来继续之前的会话。
 *
 * JSON 输出格式：
 * - {"type":"system","subtype":"init",...} - 初始化信息
 * - {"type":"assistant","message":{...},...} - 助手响应
 * - {"type":"result","subtype":"success",...} - 最终结果
 */
Object.defineProperty(exports, "__esModule", { value: true });
exports.ClaudeAdapter = void 0;
const events_1 = require("events");
const child_process_1 = require("child_process");
const types_1 = require("../types");
/**
 * Claude CLI 适配器
 * 每次 sendMessage 启动新进程，使用 session_id 保持会话连续性
 */
class ClaudeAdapter extends events_1.EventEmitter {
    type = 'claude';
    config;
    _state = 'idle';
    sessionId = null;
    currentProcess = null;
    _installed = null;
    /**
     * 检查 Claude CLI 是否已安装
     */
    static async checkInstalled() {
        return new Promise((resolve) => {
            const proc = (0, child_process_1.spawn)('claude', ['--version'], { stdio: ['ignore', 'pipe', 'pipe'] });
            proc.on('error', () => resolve(false));
            proc.on('close', (code) => resolve(code === 0));
            setTimeout(() => { proc.kill(); resolve(false); }, 3000);
        });
    }
    constructor(config) {
        super();
        this.config = { ...config, type: 'claude', timeout: config.timeout || 5 * 60 * 1000 };
    }
    get state() {
        return this._state;
    }
    get isConnected() {
        // Claude CLI 不需要持久连接，始终"已连接"
        return this._state !== 'error';
    }
    get isBusy() {
        return this._state === 'busy';
    }
    /** 获取 CLI 能力 */
    get capabilities() {
        return types_1.CLI_CAPABILITIES.claude;
    }
    setState(state) {
        if (this._state !== state) {
            this._state = state;
            this.emit('stateChange', state);
        }
    }
    /** 连接（Claude CLI 不需要持久连接） */
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
    /** 发送消息（Claude CLI 通过 Read 工具读取图片，在 prompt 中引用路径） */
    async sendMessage(message, imagePaths) {
        if (this.isBusy) {
            throw new Error('Claude CLI is busy');
        }
        // Claude CLI 通过 Read 工具读取图片，然后使用 analyze_image MCP 工具分析
        // 使用明确的文件路径格式，让 Claude 识别需要读取本地文件
        let finalMessage = message;
        if (imagePaths && imagePaths.length > 0) {
            const imageRefs = imagePaths.map((p, i) => `图片${i + 1}: ${p}`).join('\n');
            finalMessage = `请先读取并分析以下本地图片文件：\n${imageRefs}\n\n然后回答：${message}`;
            console.log('[ClaudeAdapter] 已将图片路径添加到 prompt 中:', imagePaths);
        }
        this.setState('busy');
        console.log('[ClaudeAdapter] ========== sendMessage 开始 ==========');
        console.log('[ClaudeAdapter] 当前 sessionId:', this.sessionId);
        console.log('[ClaudeAdapter] message:', finalMessage.substring(0, 100));
        return new Promise((resolve, reject) => {
            const args = this.buildArgs(finalMessage);
            let output = '';
            console.log('[ClaudeAdapter] 完整命令行参数:', args);
            console.log('[ClaudeAdapter] 启动进程: claude', args.join(' '));
            // 不使用 shell: true，直接传递参数数组
            this.currentProcess = (0, child_process_1.spawn)('claude', args, {
                cwd: this.config.cwd,
                env: { ...process.env, ...this.config.env },
                stdio: ['pipe', 'pipe', 'pipe'],
            });
            // 立即关闭 stdin，告诉 Claude CLI 没有更多输入
            this.currentProcess.stdin?.end();
            console.log('[ClaudeAdapter] 进程已启动, PID:', this.currentProcess.pid);
            // 设置超时
            const timeout = setTimeout(() => {
                console.log('[ClaudeAdapter] 超时!');
                this.currentProcess?.kill();
                this.setState('ready');
                reject(new Error('Claude CLI timeout'));
            }, this.config.timeout);
            this.currentProcess.stdout?.on('data', (data) => {
                const chunk = data.toString();
                console.log('[ClaudeAdapter] stdout:', chunk.substring(0, 100));
                output += chunk;
                this.emit('output', chunk);
            });
            this.currentProcess.stderr?.on('data', (data) => {
                const chunk = data.toString();
                console.log('[ClaudeAdapter] stderr:', chunk.substring(0, 100));
                output += chunk;
                this.emit('output', chunk);
            });
            this.currentProcess.on('close', (code) => {
                console.log('[ClaudeAdapter] 进程关闭, code:', code, 'output length:', output.length);
                clearTimeout(timeout);
                this.currentProcess = null;
                this.setState('ready');
                const response = this.parseOutput(output);
                console.log('[ClaudeAdapter] 解析结果:', response.content?.substring(0, 100));
                // 保存 session_id 用于后续调用
                if (response.raw) {
                    this.extractSessionId(response.raw);
                }
                if (code !== 0 && !response.content) {
                    console.log('[ClaudeAdapter] 错误退出');
                    reject(new Error(`Claude CLI exited with code ${code}`));
                }
                else {
                    console.log('[ClaudeAdapter] 成功完成');
                    this.emit('response', response);
                    resolve(response);
                }
            });
            this.currentProcess.on('error', (err) => {
                console.log('[ClaudeAdapter] 进程错误:', err.message);
                clearTimeout(timeout);
                this.currentProcess = null;
                this.setState('error');
                this.emit('error', err);
                reject(err);
            });
        });
    }
    /** 中断当前操作 - 🆕 增强版：添加超时机制和强制kill */
    async interrupt() {
        if (!this.currentProcess) {
            this.setState('ready');
            return;
        }
        const proc = this.currentProcess;
        const pid = proc.pid;
        console.log('[ClaudeAdapter] 开始中断进程, PID:', pid);
        return new Promise((resolve) => {
            let resolved = false;
            const cleanup = () => {
                if (resolved)
                    return;
                resolved = true;
                this.currentProcess = null;
                this.setState('ready');
                resolve();
            };
            // 监听进程退出
            proc.once('close', () => {
                console.log('[ClaudeAdapter] 进程已正常退出');
                cleanup();
            });
            proc.once('exit', () => {
                console.log('[ClaudeAdapter] 进程已退出');
                cleanup();
            });
            // 第一步：发送 SIGINT
            console.log('[ClaudeAdapter] 发送 SIGINT');
            proc.kill('SIGINT');
            // 第二步：1秒后如果还没退出，发送 SIGTERM
            setTimeout(() => {
                if (resolved)
                    return;
                console.log('[ClaudeAdapter] SIGINT 超时，发送 SIGTERM');
                try {
                    proc.kill('SIGTERM');
                }
                catch (e) { /* ignore */ }
            }, 1000);
            // 第三步：2秒后如果还没退出，强制 SIGKILL
            setTimeout(() => {
                if (resolved)
                    return;
                console.log('[ClaudeAdapter] SIGTERM 超时，发送 SIGKILL');
                try {
                    proc.kill('SIGKILL');
                }
                catch (e) { /* ignore */ }
            }, 2000);
            // 第四步：3秒后无论如何都清理状态
            setTimeout(() => {
                if (resolved)
                    return;
                console.log('[ClaudeAdapter] 强制清理状态');
                cleanup();
            }, 3000);
        });
    }
    /** 构建命令行参数（Claude CLI 不支持图片） */
    buildArgs(message) {
        // 使用 -p (print) 模式进行非交互式调用
        // 使用 --output-format stream-json --verbose 获取实时流式输出
        // 使用 --dangerously-skip-permissions 跳过权限确认（自动执行）
        const args = ['-p', '--output-format', 'stream-json', '--verbose', '--dangerously-skip-permissions'];
        // 如果有之前的 session_id，使用 --resume 继续会话
        if (this.sessionId) {
            args.push('--resume', this.sessionId);
        }
        // 添加消息内容（不使用 shell 模式时直接传递）
        args.push(message);
        return args;
    }
    /** 从输出中提取 session_id */
    extractSessionId(output) {
        console.log('[ClaudeAdapter] extractSessionId 开始解析...');
        const lines = output.trim().split('\n');
        for (const line of lines) {
            try {
                const msg = JSON.parse(line);
                // session_id 可能在 type: "result" 或 type: "system" 消息中
                if (msg.session_id) {
                    const oldSessionId = this.sessionId;
                    this.sessionId = msg.session_id;
                    console.log('[ClaudeAdapter] 提取到 session_id:', this.sessionId, '(之前:', oldSessionId, ')');
                    return; // 找到后立即返回
                }
            }
            catch {
                // 忽略非 JSON 行
            }
        }
        console.log('[ClaudeAdapter] 警告: 未能从输出中提取 session_id');
    }
    /** 解析 Claude CLI 输出 */
    parseOutput(output) {
        console.log('[ClaudeAdapter] parseOutput 开始...');
        const lines = output.trim().split('\n');
        const fileChanges = [];
        let content = '';
        let error;
        for (const line of lines) {
            try {
                const msg = JSON.parse(line);
                if (msg.type === 'system' && msg.subtype === 'init') {
                    const oldSessionId = this.sessionId;
                    this.sessionId = msg.session_id;
                    console.log('[ClaudeAdapter] parseOutput 中提取 session_id:', this.sessionId, '(之前:', oldSessionId, ')');
                }
                else if (msg.type === 'assistant') {
                    if (msg.error) {
                        error = msg.error;
                    }
                    if (msg.message?.content) {
                        for (const block of msg.message.content) {
                            if (block.type === 'text' && block.text) {
                                content += block.text + '\n';
                            }
                        }
                    }
                }
                else if (msg.type === 'result') {
                    if (msg.is_error) {
                        error = msg.result;
                    }
                    else if (!content) {
                        content = msg.result;
                    }
                }
            }
            catch {
                if (line.trim()) {
                    content += line + '\n';
                }
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
    /** 获取当前会话 ID */
    getSessionId() {
        return this.sessionId;
    }
    /** 设置会话 ID（用于恢复之前的会话） */
    setSessionId(sessionId) {
        this.sessionId = sessionId;
        console.log('[ClaudeAdapter] 设置 sessionId:', sessionId);
    }
    /** 重置会话（开始新对话） */
    resetSession() {
        this.sessionId = null;
        console.log('[ClaudeAdapter] 重置会话');
    }
}
exports.ClaudeAdapter = ClaudeAdapter;
//# sourceMappingURL=claude.js.map