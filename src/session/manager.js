"use strict";
/**
 * 会话管理器
 * 协调 CLI 适配器和会话存储
 */
Object.defineProperty(exports, "__esModule", { value: true });
exports.SessionManager = void 0;
const events_1 = require("events");
const uuid_1 = require("uuid");
const adapter_factory_1 = require("../cli/adapter-factory");
const storage_1 = require("./storage");
/**
 * 会话管理器
 */
class SessionManager extends events_1.EventEmitter {
    factory;
    storage;
    currentSession = null;
    sessions = new Map();
    constructor(context, cwd) {
        super();
        this.factory = new adapter_factory_1.CLIAdapterFactory({ cwd });
        this.storage = new storage_1.SessionStorage(context);
    }
    /** 获取当前会话 */
    get current() {
        return this.currentSession;
    }
    /** 获取当前会话 ID */
    get currentId() {
        return this.currentSession?.id || null;
    }
    /** 初始化，加载会话列表 */
    async initialize() {
        const metas = await this.storage.list();
        // 预加载最近的会话
        if (metas.length > 0) {
            const recent = await this.storage.load(metas[0].id);
            if (recent) {
                this.sessions.set(recent.id, recent);
                this.currentSession = recent;
            }
        }
    }
    /** 创建新会话 */
    async createSession(name) {
        const session = {
            id: (0, uuid_1.v4)(),
            name,
            messages: [],
            createdAt: Date.now(),
            updatedAt: Date.now(),
        };
        this.sessions.set(session.id, session);
        await this.storage.save(session);
        this.currentSession = session;
        this.emit('sessionCreated', session);
        return session;
    }
    /** 切换会话 */
    async switchSession(id) {
        let session = this.sessions.get(id);
        if (!session) {
            session = await this.storage.load(id);
            if (session) {
                this.sessions.set(id, session);
            }
        }
        if (session) {
            this.currentSession = session;
            this.emit('sessionSwitched', session);
        }
        return session || null;
    }
    /** 重命名会话 */
    async renameSession(id, name) {
        const session = this.sessions.get(id) || await this.storage.load(id);
        if (session) {
            session.name = name;
            session.updatedAt = Date.now();
            this.sessions.set(id, session);
            await this.storage.save(session);
            this.emit('sessionUpdated', session);
        }
    }
    /** 删除会话 */
    async deleteSession(id) {
        this.sessions.delete(id);
        await this.storage.delete(id);
        if (this.currentSession?.id === id) {
            this.currentSession = null;
            // 切换到最近的会话
            const metas = await this.storage.list();
            if (metas.length > 0) {
                await this.switchSession(metas[0].id);
            }
        }
        this.emit('sessionDeleted', id);
    }
    /** 获取会话列表 */
    async listSessions() {
        return this.storage.list();
    }
    /** 发送消息到 CLI */
    async sendMessage(content, cli) {
        // 确保有当前会话
        if (!this.currentSession) {
            await this.createSession();
        }
        // 添加用户消息
        const userMessage = {
            id: (0, uuid_1.v4)(),
            role: 'user',
            content,
            cli,
            timestamp: Date.now(),
        };
        this.addMessage(userMessage);
        // 获取或创建适配器
        const targetCli = cli || 'claude'; // 默认使用 Claude
        const adapter = await this.factory.connect(targetCli);
        // 设置输出监听
        const outputHandler = (chunk) => {
            this.emit('cliOutput', targetCli, chunk);
        };
        adapter.on('output', outputHandler);
        try {
            // 发送消息
            const response = await adapter.sendMessage(content);
            // 添加助手消息
            const assistantMessage = {
                id: (0, uuid_1.v4)(),
                role: 'assistant',
                content: response.content,
                cli: targetCli,
                timestamp: Date.now(),
                fileChanges: response.fileChanges,
            };
            this.addMessage(assistantMessage);
            this.emit('cliResponse', targetCli, response);
            return response;
        }
        finally {
            adapter.off('output', outputHandler);
        }
    }
    /** 添加消息到当前会话 */
    addMessage(message) {
        if (!this.currentSession)
            return;
        this.currentSession.messages.push(message);
        this.currentSession.updatedAt = Date.now();
        // 异步保存
        this.storage.save(this.currentSession).catch(err => {
            this.emit('error', err);
        });
        this.emit('messageAdded', message);
    }
    /** 中断当前 CLI 操作 */
    async interrupt(cli) {
        if (cli) {
            const adapter = this.factory.getAdapter(cli);
            if (adapter) {
                await adapter.interrupt();
            }
        }
        else {
            // 中断所有
            const adapters = this.factory.getConnectedAdapters();
            await Promise.all(adapters.map(a => a.interrupt()));
        }
    }
    /** 获取 CLI 适配器 */
    getAdapter(cli) {
        return this.factory.getAdapter(cli);
    }
    /** 获取适配器工厂 */
    getFactory() {
        return this.factory;
    }
    /** 销毁管理器 */
    async dispose() {
        await this.factory.dispose();
    }
    /** 事件监听类型 */
    on(event, listener) {
        return super.on(event, listener);
    }
    off(event, listener) {
        return super.off(event, listener);
    }
    emit(event, ...args) {
        return super.emit(event, ...args);
    }
}
exports.SessionManager = SessionManager;
//# sourceMappingURL=manager.js.map