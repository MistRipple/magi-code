"use strict";
/**
 * 会话存储
 * 处理会话的持久化存储
 */
Object.defineProperty(exports, "__esModule", { value: true });
exports.SessionStorage = void 0;
/** 存储键前缀 */
const STORAGE_PREFIX = 'cliArranger.session.';
const SESSION_LIST_KEY = 'cliArranger.sessionList';
/**
 * 会话存储类
 */
class SessionStorage {
    context;
    constructor(context) {
        this.context = context;
    }
    /**
     * 保存会话
     */
    async save(session) {
        const key = STORAGE_PREFIX + session.id;
        await this.context.globalState.update(key, session);
        // 更新会话列表
        await this.updateSessionList(session);
    }
    /**
     * 加载会话
     */
    async load(id) {
        const key = STORAGE_PREFIX + id;
        return this.context.globalState.get(key);
    }
    /**
     * 删除会话
     */
    async delete(id) {
        const key = STORAGE_PREFIX + id;
        await this.context.globalState.update(key, undefined);
        // 从列表中移除
        const list = await this.list();
        const filtered = list.filter(m => m.id !== id);
        await this.context.globalState.update(SESSION_LIST_KEY, filtered);
    }
    /**
     * 获取会话列表（元数据）
     */
    async list() {
        return this.context.globalState.get(SESSION_LIST_KEY) || [];
    }
    /**
     * 更新会话列表
     */
    async updateSessionList(session) {
        const list = await this.list();
        const meta = {
            id: session.id,
            name: session.name,
            messageCount: session.messages.length,
            createdAt: session.createdAt,
            updatedAt: session.updatedAt,
        };
        const index = list.findIndex(m => m.id === session.id);
        if (index >= 0) {
            list[index] = meta;
        }
        else {
            list.push(meta);
        }
        // 按更新时间排序
        list.sort((a, b) => b.updatedAt - a.updatedAt);
        await this.context.globalState.update(SESSION_LIST_KEY, list);
    }
    /**
     * 清空所有会话
     */
    async clear() {
        const list = await this.list();
        for (const meta of list) {
            const key = STORAGE_PREFIX + meta.id;
            await this.context.globalState.update(key, undefined);
        }
        await this.context.globalState.update(SESSION_LIST_KEY, []);
    }
    /**
     * 获取会话数量
     */
    async count() {
        const list = await this.list();
        return list.length;
    }
}
exports.SessionStorage = SessionStorage;
//# sourceMappingURL=storage.js.map