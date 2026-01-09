"use strict";
/**
 * CLI 适配器工厂
 * 统一管理和创建 CLI 适配器实例
 */
Object.defineProperty(exports, "__esModule", { value: true });
exports.CLIAdapterFactory = void 0;
const events_1 = require("events");
const types_1 = require("./types");
const claude_1 = require("./adapters/claude");
const codex_1 = require("./adapters/codex");
const gemini_1 = require("./adapters/gemini");
/**
 * CLI 适配器工厂
 * 提供统一的适配器创建、管理和事件转发
 */
class CLIAdapterFactory extends events_1.EventEmitter {
    adapters = new Map();
    config;
    constructor(config) {
        super();
        this.config = config;
    }
    /**
     * 创建或获取适配器实例
     */
    create(type) {
        const existing = this.adapters.get(type);
        if (existing) {
            return existing;
        }
        const adapterConfig = {
            cwd: this.config.cwd,
            timeout: this.config.timeout,
            env: this.config.env,
        };
        let adapter;
        switch (type) {
            case 'claude':
                adapter = new claude_1.ClaudeAdapter(adapterConfig);
                break;
            case 'codex':
                adapter = new codex_1.CodexAdapter(adapterConfig);
                break;
            case 'gemini':
                adapter = new gemini_1.GeminiAdapter(adapterConfig);
                break;
            default:
                throw new Error(`Unknown CLI type: ${type}`);
        }
        // 转发适配器事件
        this.setupAdapterEvents(adapter, type);
        this.adapters.set(type, adapter);
        return adapter;
    }
    /**
     * 设置适配器事件转发
     */
    setupAdapterEvents(adapter, type) {
        adapter.on('output', (chunk) => {
            this.emit('output', { type, chunk });
        });
        adapter.on('response', (response) => {
            this.emit('response', { type, response });
        });
        adapter.on('error', (error) => {
            this.emit('error', { type, error });
        });
        adapter.on('stateChange', (state) => {
            this.emit('stateChange', { type, state });
        });
    }
    /**
     * 获取已创建的适配器
     */
    getAdapter(type) {
        return this.adapters.get(type);
    }
    /**
     * 检查 CLI 是否可用（已创建且已连接）
     */
    isAvailable(type) {
        const adapter = this.adapters.get(type);
        return adapter?.isConnected ?? false;
    }
    /**
     * 获取或创建适配器
     */
    getOrCreate(type) {
        return this.adapters.get(type) || this.create(type);
    }
    /**
     * 获取所有已创建的适配器
     */
    getAllAdapters() {
        return Array.from(this.adapters.values());
    }
    /**
     * 获取所有适配器状态
     */
    getAllStatus() {
        const types = ['claude', 'codex', 'gemini'];
        return types.map(type => {
            const adapter = this.adapters.get(type);
            return {
                type,
                connected: adapter?.isConnected ?? false,
                busy: adapter?.isBusy ?? false,
                state: adapter?.state ?? 'idle',
            };
        });
    }
    /**
     * 获取所有已连接的适配器
     */
    getConnectedAdapters() {
        return this.getAllAdapters().filter(a => a.isConnected);
    }
    /**
     * 获取所有可用（已连接且不忙）的适配器
     */
    getAvailableAdapters() {
        return this.getAllAdapters().filter(a => a.isConnected && !a.isBusy);
    }
    /**
     * 连接指定类型的适配器
     */
    async connect(type) {
        const adapter = this.create(type);
        if (!adapter.isConnected) {
            await adapter.connect();
        }
        return adapter;
    }
    /**
     * 连接所有适配器
     */
    async connectAll() {
        const types = ['claude', 'codex', 'gemini'];
        await Promise.all(types.map(type => this.connect(type).catch(() => { })));
    }
    /**
     * 检查所有 CLI 的安装状态（轻量检测，不启动进程）
     */
    async checkAllAvailability() {
        const [claude, codex, gemini] = await Promise.all([
            claude_1.ClaudeAdapter.checkInstalled(),
            codex_1.CodexAdapter.checkInstalled(),
            gemini_1.GeminiAdapter.checkInstalled(),
        ]);
        return { claude, codex, gemini };
    }
    /**
     * 断开指定类型的适配器
     */
    async disconnect(type) {
        const adapter = this.adapters.get(type);
        if (adapter) {
            await adapter.disconnect();
        }
    }
    /**
     * 断开所有适配器
     */
    async disconnectAll() {
        const promises = this.getAllAdapters().map(a => a.disconnect());
        await Promise.all(promises);
    }
    /**
     * 发送消息到指定 CLI
     * 如果目标 CLI 不支持图片或处于会话恢复模式，会先用 Codex 描述图片
     */
    async sendMessage(type, message, imagePaths) {
        const adapter = this.getOrCreate(type);
        if (!adapter.isConnected) {
            await adapter.connect();
        }
        const hasImages = imagePaths && imagePaths.length > 0;
        console.log(`[CLIAdapterFactory] sendMessage: type=${type}, hasImages=${hasImages}, imagePaths=`, imagePaths);
        // 判断是否需要预处理图片
        if (hasImages) {
            const needsImageDescription = this.shouldDescribeImages(type, adapter);
            console.log(`[CLIAdapterFactory] needsImageDescription=${needsImageDescription}`);
            if (needsImageDescription) {
                console.log(`[CLIAdapterFactory] 目标 CLI ${type} 需要图片描述，使用 Codex 预处理`);
                try {
                    const imageDescription = await codex_1.CodexAdapter.describeImages(imagePaths, this.config.cwd);
                    console.log(`[CLIAdapterFactory] 图片描述结果: "${imageDescription}"`);
                    // 将图片描述附加到消息中
                    const enhancedMessage = `${message}\n\n[图片内容描述]\n${imageDescription}`;
                    console.log(`[CLIAdapterFactory] 图片描述完成，增强后的消息长度: ${enhancedMessage.length}`);
                    return adapter.sendMessage(enhancedMessage);
                }
                catch (error) {
                    console.error('[CLIAdapterFactory] 图片描述失败:', error);
                    // 图片描述失败时，仍然发送原始消息，但附加提示
                    const fallbackMessage = `${message}\n\n[注意: 图片处理失败，请用户重新描述图片内容]`;
                    return adapter.sendMessage(fallbackMessage);
                }
            }
        }
        // 直接发送（支持图片的 CLI 或无图片）
        return adapter.sendMessage(message, imagePaths);
    }
    /**
     * 判断是否需要用 Codex 描述图片
     * @returns true 如果需要描述图片
     */
    shouldDescribeImages(type, _adapter) {
        const capabilities = types_1.CLI_CAPABILITIES[type];
        // 1. 如果目标 CLI 不支持图片，需要描述
        if (!capabilities.supportsImage) {
            console.log(`[CLIAdapterFactory] ${type} 不支持图片`);
            return true;
        }
        // 2. 如果是 Codex 且处于会话恢复模式（有 sessionId），需要描述
        //    因为 exec resume 不支持 -i 参数
        if (type === 'codex') {
            const sessionId = this.getSessionId('codex');
            if (sessionId) {
                console.log(`[CLIAdapterFactory] Codex 处于会话恢复模式，需要描述图片`);
                return true;
            }
        }
        // 3. 其他情况，直接传递图片
        return false;
    }
    /**
     * 中断指定 CLI 的执行
     */
    async interrupt(type) {
        const adapter = this.adapters.get(type);
        if (adapter) {
            await adapter.interrupt();
        }
    }
    /**
     * 中断所有 CLI 的执行
     */
    async interruptAll() {
        const promises = this.getAllAdapters().map(a => a.interrupt());
        await Promise.all(promises);
    }
    /**
     * 获取指定 CLI 的会话 ID
     */
    getSessionId(type) {
        const adapter = this.adapters.get(type);
        if (adapter && 'getSessionId' in adapter && typeof adapter.getSessionId === 'function') {
            return adapter.getSessionId();
        }
        return null;
    }
    /**
     * 设置指定 CLI 的会话 ID
     */
    setSessionId(type, sessionId) {
        const adapter = this.adapters.get(type);
        if (adapter && 'setSessionId' in adapter && typeof adapter.setSessionId === 'function') {
            adapter.setSessionId(sessionId);
        }
    }
    /**
     * 重置指定 CLI 的会话
     */
    resetSession(type) {
        const adapter = this.adapters.get(type);
        if (adapter && 'resetSession' in adapter && typeof adapter.resetSession === 'function') {
            adapter.resetSession();
        }
    }
    /**
     * 重置所有 CLI 的会话
     */
    resetAllSessions() {
        const types = ['claude', 'codex', 'gemini'];
        types.forEach(type => this.resetSession(type));
    }
    /**
     * 获取所有 CLI 的会话 ID
     */
    getAllSessionIds() {
        return {
            claude: this.getSessionId('claude') ?? undefined,
            codex: this.getSessionId('codex') ?? undefined,
            gemini: this.getSessionId('gemini') ?? undefined,
        };
    }
    /**
     * 设置所有 CLI 的会话 ID
     */
    setAllSessionIds(sessionIds) {
        if (sessionIds.claude !== undefined)
            this.setSessionId('claude', sessionIds.claude);
        if (sessionIds.codex !== undefined)
            this.setSessionId('codex', sessionIds.codex);
        if (sessionIds.gemini !== undefined)
            this.setSessionId('gemini', sessionIds.gemini);
    }
    /**
     * 销毁工厂，清理所有资源
     */
    async dispose() {
        await this.disconnectAll();
        this.adapters.clear();
        this.removeAllListeners();
    }
}
exports.CLIAdapterFactory = CLIAdapterFactory;
//# sourceMappingURL=adapter-factory.js.map