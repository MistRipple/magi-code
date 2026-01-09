"use strict";
/**
 * 快照管理器
 * 文件快照创建、存储、还原
 */
var __createBinding = (this && this.__createBinding) || (Object.create ? (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    var desc = Object.getOwnPropertyDescriptor(m, k);
    if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
      desc = { enumerable: true, get: function() { return m[k]; } };
    }
    Object.defineProperty(o, k2, desc);
}) : (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    o[k2] = m[k];
}));
var __setModuleDefault = (this && this.__setModuleDefault) || (Object.create ? (function(o, v) {
    Object.defineProperty(o, "default", { enumerable: true, value: v });
}) : function(o, v) {
    o["default"] = v;
});
var __importStar = (this && this.__importStar) || (function () {
    var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function (o) {
            var ar = [];
            for (var k in o) if (Object.prototype.hasOwnProperty.call(o, k)) ar[ar.length] = k;
            return ar;
        };
        return ownKeys(o);
    };
    return function (mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        __setModuleDefault(result, mod);
        return result;
    };
})();
Object.defineProperty(exports, "__esModule", { value: true });
exports.SnapshotManager = void 0;
const fs = __importStar(require("fs"));
const path = __importStar(require("path"));
const events_1 = require("./events");
/** 生成唯一 ID */
function generateId() {
    return `${Date.now()}-${Math.random().toString(36).substring(2, 9)}`;
}
/**
 * 快照管理器
 */
class SnapshotManager {
    sessionManager;
    workspaceRoot;
    snapshotDir;
    constructor(sessionManager, workspaceRoot) {
        this.sessionManager = sessionManager;
        this.workspaceRoot = workspaceRoot;
        this.snapshotDir = path.join(workspaceRoot, '.cli-arranger', 'snapshots');
        this.ensureSnapshotDir();
    }
    /** 确保快照目录存在 */
    ensureSnapshotDir() {
        if (!fs.existsSync(this.snapshotDir)) {
            fs.mkdirSync(this.snapshotDir, { recursive: true });
        }
    }
    /** 创建文件快照 */
    createSnapshot(filePath, modifiedBy, subTaskId) {
        const session = this.sessionManager.getCurrentSession();
        if (!session)
            return null;
        const absolutePath = path.isAbsolute(filePath)
            ? filePath
            : path.join(this.workspaceRoot, filePath);
        const relativePath = path.relative(this.workspaceRoot, absolutePath);
        // 检查是否已有该文件的快照
        const existingSnapshot = this.sessionManager.getSnapshot(session.id, relativePath);
        if (existingSnapshot) {
            // 更新修改信息，但保留原始内容
            existingSnapshot.lastModifiedBy = modifiedBy;
            existingSnapshot.lastModifiedAt = Date.now();
            existingSnapshot.subTaskId = subTaskId;
            this.sessionManager.addSnapshot(session.id, existingSnapshot);
            return existingSnapshot;
        }
        // 读取原始文件内容
        let originalContent = '';
        if (fs.existsSync(absolutePath)) {
            originalContent = fs.readFileSync(absolutePath, 'utf-8');
        }
        const snapshot = {
            id: generateId(),
            sessionId: session.id,
            filePath: relativePath,
            originalContent,
            lastModifiedBy: modifiedBy,
            lastModifiedAt: Date.now(),
            subTaskId,
        };
        // 保存快照内容到文件
        this.saveSnapshotContent(snapshot);
        // 添加到 Session
        this.sessionManager.addSnapshot(session.id, snapshot);
        events_1.globalEventBus.emitEvent('snapshot:created', {
            sessionId: session.id,
            data: { filePath: relativePath, snapshotId: snapshot.id },
        });
        return snapshot;
    }
    /** 保存快照内容到文件 */
    saveSnapshotContent(snapshot) {
        const sessionDir = path.join(this.snapshotDir, snapshot.sessionId);
        if (!fs.existsSync(sessionDir)) {
            fs.mkdirSync(sessionDir, { recursive: true });
        }
        const snapshotFile = path.join(sessionDir, `${snapshot.id}.snapshot`);
        fs.writeFileSync(snapshotFile, snapshot.originalContent, 'utf-8');
    }
    /** 还原文件到快照状态 */
    revertToSnapshot(filePath) {
        const session = this.sessionManager.getCurrentSession();
        if (!session)
            return false;
        const relativePath = path.isAbsolute(filePath)
            ? path.relative(this.workspaceRoot, filePath)
            : filePath;
        const snapshot = this.sessionManager.getSnapshot(session.id, relativePath);
        if (!snapshot)
            return false;
        const absolutePath = path.join(this.workspaceRoot, relativePath);
        // 读取快照内容
        const snapshotFile = path.join(this.snapshotDir, session.id, `${snapshot.id}.snapshot`);
        let content = snapshot.originalContent;
        if (fs.existsSync(snapshotFile)) {
            content = fs.readFileSync(snapshotFile, 'utf-8');
        }
        // 还原文件
        if (content === '' && fs.existsSync(absolutePath)) {
            // 原本不存在的文件，删除它
            fs.unlinkSync(absolutePath);
        }
        else {
            // 确保目录存在
            const dir = path.dirname(absolutePath);
            if (!fs.existsSync(dir)) {
                fs.mkdirSync(dir, { recursive: true });
            }
            fs.writeFileSync(absolutePath, content, 'utf-8');
        }
        events_1.globalEventBus.emitEvent('snapshot:reverted', {
            sessionId: session.id,
            data: { filePath: relativePath },
        });
        return true;
    }
    /** 获取待处理变更列表 */
    getPendingChanges() {
        const session = this.sessionManager.getCurrentSession();
        if (!session)
            return [];
        const changes = [];
        for (const snapshot of session.snapshots) {
            const absolutePath = path.join(this.workspaceRoot, snapshot.filePath);
            const currentContent = fs.existsSync(absolutePath)
                ? fs.readFileSync(absolutePath, 'utf-8')
                : '';
            // 计算变更行数
            const { additions, deletions } = this.countChanges(snapshot.originalContent, currentContent);
            if (additions > 0 || deletions > 0) {
                changes.push({
                    filePath: snapshot.filePath,
                    snapshotId: snapshot.id,
                    lastModifiedBy: snapshot.lastModifiedBy,
                    additions,
                    deletions,
                    status: 'pending',
                });
            }
        }
        return changes;
    }
    /** 计算变更行数 */
    countChanges(original, current) {
        const originalLines = original.split('\n');
        const currentLines = current.split('\n');
        const additions = Math.max(0, currentLines.length - originalLines.length);
        const deletions = Math.max(0, originalLines.length - currentLines.length);
        if (additions === 0 && deletions === 0 && original !== current) {
            let changedLines = 0;
            const minLen = Math.min(originalLines.length, currentLines.length);
            for (let i = 0; i < minLen; i++) {
                if (originalLines[i] !== currentLines[i])
                    changedLines++;
            }
            return { additions: changedLines, deletions: changedLines };
        }
        return { additions, deletions };
    }
    /** 接受变更（删除快照，保留当前文件） */
    acceptChange(filePath) {
        const session = this.sessionManager.getCurrentSession();
        if (!session)
            return false;
        const relativePath = path.isAbsolute(filePath)
            ? path.relative(this.workspaceRoot, filePath)
            : filePath;
        const snapshot = this.sessionManager.getSnapshot(session.id, relativePath);
        if (!snapshot)
            return false;
        // 删除快照文件
        const snapshotFile = path.join(this.snapshotDir, session.id, `${snapshot.id}.snapshot`);
        if (fs.existsSync(snapshotFile)) {
            fs.unlinkSync(snapshotFile);
        }
        // 从 session 中移除快照
        this.sessionManager.removeSnapshot(session.id, relativePath);
        events_1.globalEventBus.emitEvent('snapshot:accepted', {
            sessionId: session.id,
            data: { filePath: relativePath },
        });
        return true;
    }
    /** 批量接受所有变更 */
    acceptAllChanges() {
        const session = this.sessionManager.getCurrentSession();
        if (!session)
            return 0;
        const snapshots = [...session.snapshots];
        let count = 0;
        for (const snapshot of snapshots) {
            if (this.acceptChange(snapshot.filePath)) {
                count++;
            }
        }
        return count;
    }
    /** 批量还原所有变更 */
    revertAllChanges() {
        const session = this.sessionManager.getCurrentSession();
        if (!session)
            return 0;
        const snapshots = [...session.snapshots];
        let count = 0;
        for (const snapshot of snapshots) {
            if (this.revertToSnapshot(snapshot.filePath)) {
                count++;
            }
        }
        return count;
    }
    /** 为多个文件创建快照 */
    createSnapshots(filePaths, modifiedBy, subTaskId) {
        const snapshots = [];
        for (const filePath of filePaths) {
            const snapshot = this.createSnapshot(filePath, modifiedBy, subTaskId);
            if (snapshot) {
                snapshots.push(snapshot);
            }
        }
        return snapshots;
    }
    /** 清理会话的所有快照 */
    cleanupSession(sessionId) {
        const sessionDir = path.join(this.snapshotDir, sessionId);
        if (fs.existsSync(sessionDir)) {
            fs.rmSync(sessionDir, { recursive: true, force: true });
        }
    }
}
exports.SnapshotManager = SnapshotManager;
//# sourceMappingURL=snapshot-manager.js.map